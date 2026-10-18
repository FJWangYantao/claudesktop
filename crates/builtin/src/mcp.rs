//! MCP（Model Context Protocol）客户端：让自研内核调用外部工具服务器提供的工具。
//!
//! - 配置：数据目录的 `mcp.json`，格式同 Claude Code 的 `.mcp.json`
//!   （`{"mcpServers": {"名字": {"command", "args", "env", "disabled"}}}`；远程服务写
//!   `{"type": "http" | "sse", "url", "headers"}`）。字符串里的 `${VAR}` / `${VAR:-默认值}` 换成环境变量。
//! - 本地服务一个子进程，stdio 上换行分隔的 JSON-RPC 2.0；远程服务走 HTTP（见 `mcp_http`）。
//!   都是 `initialize` → `notifications/initialized` → `tools/list`；服务器说工具变了
//!   （`notifications/tools/list_changed`）就重新列。
//! - 连上过的服务断了（进程退出、事件流断、会话过期）按 [`RECONNECT_DELAYS`] 退避自动重连；
//!   一开始就没连上的不自动重试，设置页每个服务旁有「重新连接」。
//! - 工具以 `mcp__<服务器>__<工具>` 的名字交给模型（和 Claude Code 一致）；调用走内核的权限流程，
//!   「总是允许」记成 `mcp:<服务器>:<工具>`。服务器标了只读（`readOnlyHint`）的工具 Plan 模式下也能用。
//! - 状态（连接中 / 已连接 N 个工具 / 失败原因）变了就整份经回调上报，界面在设置页显示。

use crate::mcp_http::{self, Http, Sink};
use crate::tools::ToolOutput;
use claudesktop_core::{McpServerView, McpState};
use serde_json::{Value, json};
use std::collections::{BTreeMap, HashMap, VecDeque};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock, Weak, mpsc};
use std::time::{Duration, Instant};

/// 握手要等多久（`npx` 第一次跑要下载包，给宽一点）
const INIT_TIMEOUT: Duration = Duration::from_secs(60);
/// 列工具要等多久
const LIST_TIMEOUT: Duration = Duration::from_secs(30);
/// 一次工具调用最长等多久（用户随时可以停）
const CALL_TIMEOUT: Duration = Duration::from_secs(600);
/// 回给模型的工具结果最多这么多字符
const OUTPUT_MAX_CHARS: usize = 30_000;
/// 失败原因里带上服务器 stderr 的开头和结尾各几行（报错通常在开头，退出原因在结尾）
const STDERR_LINES: usize = 4;
/// 写进请求的协议版本（服务器回它支持的版本，不一致也照常用：只用到最基本的几个方法）
pub(crate) const PROTOCOL_VERSION: &str = "2025-06-18";
/// 连上过的服务断了之后，第几次重连前等多久；都失败就停在「没连上」
pub const RECONNECT_DELAYS: [Duration; 5] = [
    Duration::from_secs(1),
    Duration::from_secs(3),
    Duration::from_secs(10),
    Duration::from_secs(30),
    Duration::from_secs(60),
];

/// 交给模型的工具名前缀
const TOOL_PREFIX: &str = "mcp__";

/// `mcp.json` 不存在时「编辑配置文件」写进去的模板。
pub const CONFIG_TEMPLATE: &str = r#"{
  "mcpServers": {
    "example": {
      "command": "npx",
      "args": ["-y", "@modelcontextprotocol/server-everything"],
      "env": {},
      "disabled": true
    },
    "remote-example": {
      "type": "http",
      "url": "https://example.com/mcp",
      "headers": { "Authorization": "Bearer ${EXAMPLE_TOKEN}" },
      "disabled": true
    }
  }
}
"#;

/// 怎么连服务器。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Transport {
    /// 本地命令，stdio
    #[default]
    Stdio,
    /// Streamable HTTP
    Http,
    /// 旧版 HTTP + SSE
    Sse,
}

/// 一个服务器的配置。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ServerConfig {
    pub name: String,
    pub transport: Transport,
    pub command: String,
    pub args: Vec<String>,
    pub env: BTreeMap<String, String>,
    /// 远程服务的地址和请求头
    pub url: String,
    pub headers: BTreeMap<String, String>,
    pub disabled: bool,
    /// 配置了但用不了的原因（缺 command / url、不认识的 type）
    pub unsupported: Option<String>,
    /// 来自哪个插件；None = mcp.json
    pub plugin: Option<String>,
}

impl ServerConfig {
    /// 插件提供的服务（本地命令或远程）；远程的地址和请求头里的 `${VAR}` 照样换成环境变量。
    pub fn from_plugin(plugin: &str, s: &claudesktop_core::plugin::PluginMcpServer) -> Self {
        let env = |key: &str| std::env::var(key).ok();
        let transport = match (s.url.is_empty(), s.sse) {
            (true, _) => Transport::Stdio,
            (false, false) => Transport::Http,
            (false, true) => Transport::Sse,
        };
        Self {
            name: s.name.clone(),
            transport,
            command: s.command.clone(),
            args: s.args.clone(),
            env: s.env.clone(),
            url: expand(&s.url, &env),
            headers: s.headers.iter().map(|(k, v)| (k.clone(), expand(v, &env))).collect(),
            plugin: Some(plugin.to_string()),
            ..Self::default()
        }
    }
}

/// 读 `mcp.json`：文件不存在 = 没有服务器；格式不对返回原因。
pub fn load_config(path: &Path) -> Result<Vec<ServerConfig>, String> {
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(format!("读不了 {}：{e}", path.display())),
    };
    parse_config(&text)
}

fn parse_config(text: &str) -> Result<Vec<ServerConfig>, String> {
    let root: Value = serde_json::from_str(text).map_err(|e| format!("mcp.json 格式不对：{e}"))?;
    let Some(servers) = root.get("mcpServers").and_then(Value::as_object) else {
        return Err("mcp.json 里缺少 \"mcpServers\" 对象".into());
    };
    let env = |key: &str| std::env::var(key).ok();
    Ok(servers
        .iter()
        .map(|(name, s)| {
            let text =
                |key: &str| expand(s.get(key).and_then(Value::as_str).unwrap_or_default(), &env);
            let map = |key: &str| -> BTreeMap<String, String> {
                s.get(key)
                    .and_then(Value::as_object)
                    .map(|m| {
                        m.iter()
                            .filter_map(|(k, v)| v.as_str().map(|v| (k.clone(), expand(v, &env))))
                            .collect()
                    })
                    .unwrap_or_default()
            };
            let command = text("command");
            let url = text("url");
            // 没写 type 时：有 url 是远程（Streamable HTTP），否则本地命令
            let (transport, unsupported) = match text("type").as_str() {
                "" if !url.is_empty() => (Transport::Http, None),
                "" | "stdio" => (Transport::Stdio, None),
                "http" | "streamable-http" => (Transport::Http, None),
                "sse" => (Transport::Sse, None),
                other => (Transport::Stdio, Some(format!("不认识的 \"type\": {other}"))),
            };
            let unsupported = unsupported.or_else(|| match transport {
                Transport::Stdio if command.trim().is_empty() => Some("缺少 \"command\"".into()),
                Transport::Http | Transport::Sse
                    if !url.starts_with("http://") && !url.starts_with("https://") =>
                {
                    Some("\"url\" 要以 http:// 或 https:// 开头".into())
                }
                _ => None,
            });
            ServerConfig {
                name: name.clone(),
                transport,
                command,
                args: s
                    .get("args")
                    .and_then(Value::as_array)
                    .map(|a| a.iter().filter_map(|v| v.as_str()).map(|v| expand(v, &env)).collect())
                    .unwrap_or_default(),
                env: map("env"),
                url,
                headers: map("headers"),
                disabled: s.get("disabled").and_then(Value::as_bool).unwrap_or(false),
                unsupported,
                plugin: None,
            }
        })
        .collect())
}

/// `${VAR}` / `${VAR:-默认值}` 换成环境变量（没有这个变量时换成默认值或空串），同 Claude Code。
fn expand(text: &str, env: &dyn Fn(&str) -> Option<String>) -> String {
    let mut out = String::new();
    let mut rest = text;
    while let Some(start) = rest.find("${") {
        let Some(len) = rest[start..].find('}') else { break };
        out.push_str(&rest[..start]);
        let inner = &rest[start + 2..start + len];
        let (name, default) = inner.split_once(":-").unwrap_or((inner, ""));
        out.push_str(&env(name).unwrap_or_else(|| default.to_string()));
        rest = &rest[start + len + 1..];
    }
    out.push_str(rest);
    out
}

/// 服务器提供的一个工具。
#[derive(Debug, Clone, PartialEq)]
pub struct McpTool {
    pub name: String,
    pub description: String,
    pub input_schema: Value,
    /// 服务器标了只读（`annotations.readOnlyHint`）：Plan 模式下也给模型
    pub read_only: bool,
}

/// 交给模型的函数名：`mcp__<服务器>__<工具>`，非 `[A-Za-z0-9_-]` 的字符换成 `_`，最长 64（OpenAI 的限制）。
pub fn function_name(server: &str, tool: &str) -> String {
    format!("{TOOL_PREFIX}{server}__{tool}")
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || c == '_' || c == '-' { c } else { '_' })
        .take(64)
        .collect()
}

/// 这个工具名是不是 MCP 工具。
pub fn is_mcp_tool(name: &str) -> bool {
    name.starts_with(TOOL_PREFIX)
}

struct Server {
    config: ServerConfig,
    state: Mutex<McpState>,
    tools: Mutex<Vec<McpTool>>,
    client: Mutex<Option<Arc<Client>>>,
}

type StatusSink = Arc<dyn Fn(Vec<McpServerView>) + Send + Sync>;

/// 全部 MCP 服务器。内核持有一个，所有会话共用。
pub struct McpHub {
    config_path: Option<PathBuf>,
    cwd: PathBuf,
    on_status: StatusSink,
    servers: Mutex<Vec<Arc<Server>>>,
    /// 启用的插件带来的服务，和 mcp.json 里的一起连
    extra: Mutex<Vec<ServerConfig>>,
    /// 每次重新加载 +1；旧一代的连接线程连上了也不再上报
    generation: AtomicU64,
}

impl McpHub {
    pub fn new(
        config_path: Option<PathBuf>,
        cwd: PathBuf,
        on_status: impl Fn(Vec<McpServerView>) + Send + Sync + 'static,
    ) -> Arc<Self> {
        Arc::new(Self {
            config_path,
            cwd,
            on_status: Arc::new(on_status),
            servers: Mutex::new(Vec::new()),
            extra: Mutex::new(Vec::new()),
            generation: AtomicU64::new(0),
        })
    }

    /// 换插件带来的服务；和原来一样返回 false（不用重连）。
    pub fn set_extra(&self, servers: Vec<ServerConfig>) -> bool {
        let mut extra = self.extra.lock().unwrap();
        if *extra == servers {
            return false;
        }
        *extra = servers;
        true
    }

    /// 重新读配置、断开旧连接、在后台逐个连接（不阻塞调用方）。
    pub fn reload(self: &Arc<Self>) {
        let generation = self.generation.fetch_add(1, Ordering::SeqCst) + 1;
        let configs = match &self.config_path {
            Some(path) => load_config(path),
            None => Ok(Vec::new()),
        };
        // 插件的服务接在后面；和 mcp.json 里的重名就不连（工具名会撞）
        let configs = configs.map(|mut configs| {
            for mut server in self.extra.lock().unwrap().iter().cloned() {
                if configs.iter().any(|c| c.name == server.name) {
                    server.unsupported = Some("和已有的 MCP 服务重名，没有连接".into());
                }
                configs.push(server);
            }
            configs
        });
        let servers: Vec<Arc<Server>> = match configs {
            Ok(configs) => configs
                .into_iter()
                .map(|config| {
                    let state = match (&config.unsupported, config.disabled) {
                        (Some(reason), _) => McpState::Failed(reason.clone()),
                        (None, true) => McpState::Disabled,
                        (None, false) => McpState::Connecting,
                    };
                    Arc::new(Server {
                        config,
                        state: Mutex::new(state),
                        tools: Mutex::new(Vec::new()),
                        client: Mutex::new(None),
                    })
                })
                .collect(),
            // 配置文件本身坏了：显示成一个失败的「服务器」，原因写清楚
            Err(reason) => vec![Arc::new(Server {
                config: ServerConfig {
                    name: "mcp.json".into(),
                    unsupported: Some(reason.clone()),
                    ..ServerConfig::default()
                },
                state: Mutex::new(McpState::Failed(reason)),
                tools: Mutex::new(Vec::new()),
                client: Mutex::new(None),
            })],
        };
        // 旧连接在这里随 Server 一起释放（Client 的 Drop 结束子进程）
        let old = std::mem::replace(&mut *self.servers.lock().unwrap(), servers.clone());
        for server in old {
            server.client.lock().unwrap().take();
        }
        self.report();
        for server in servers {
            if *server.state.lock().unwrap() != McpState::Connecting {
                continue;
            }
            let hub = Arc::downgrade(self);
            let _ = std::thread::Builder::new()
                .name(format!("mcp-{}", server.config.name))
                .spawn(move || connect(hub, server, generation));
        }
    }

    /// 断开全部服务器（内核关闭时）。
    pub fn stop(&self) {
        self.generation.fetch_add(1, Ordering::SeqCst);
        for server in std::mem::take(&mut *self.servers.lock().unwrap()) {
            server.client.lock().unwrap().take();
        }
    }

    /// 重连一个没连上的服务（设置页的「重新连接」）；配置本身有问题、已禁用或已连上的不动。
    pub fn reconnect(self: &Arc<Self>, name: &str) {
        let server = self.servers.lock().unwrap().iter().find(|s| s.config.name == name).cloned();
        let Some(server) = server else { return };
        {
            let mut state = server.state.lock().unwrap();
            let stuck = matches!(*state, McpState::Failed(_) | McpState::Reconnecting(_));
            if !stuck || server.config.unsupported.is_some() || server.config.disabled {
                return;
            }
            *state = McpState::Connecting;
        }
        self.report();
        let generation = self.generation.load(Ordering::SeqCst);
        let hub = Arc::downgrade(self);
        let _ = std::thread::Builder::new()
            .name(format!("mcp-{}", server.config.name))
            .spawn(move || connect(hub, server, generation));
    }

    fn report(&self) {
        (self.on_status)(self.status());
    }

    /// 各服务器现在的状态（配置里的顺序）。
    pub fn status(&self) -> Vec<McpServerView> {
        self.servers
            .lock()
            .unwrap()
            .iter()
            .map(|s| {
                let state = s.state.lock().unwrap().clone();
                McpServerView {
                    name: s.config.name.clone(),
                    retry: matches!(state, McpState::Failed(_) | McpState::Reconnecting(_))
                        && s.config.unsupported.is_none(),
                    state,
                    tools: s.tools.lock().unwrap().len(),
                    plugin: s.config.plugin.clone(),
                    url: (s.config.transport != Transport::Stdio).then(|| s.config.url.clone()),
                }
            })
            .collect()
    }

    /// 交给模型的工具定义（OpenAI `tools` 格式）；`writable = false`（Plan 模式）只给只读的。
    pub fn specs(&self, writable: bool) -> Vec<Value> {
        let mut specs = Vec::new();
        for server in self.servers.lock().unwrap().iter() {
            if *server.state.lock().unwrap() != McpState::Ready {
                continue;
            }
            for tool in server.tools.lock().unwrap().iter().filter(|t| writable || t.read_only) {
                let description =
                    format!("[MCP 服务器 {}] {}", server.config.name, tool.description);
                specs.push(json!({
                    "type": "function",
                    "function": {
                        "name": function_name(&server.config.name, &tool.name),
                        "description": description,
                        "parameters": tool.input_schema,
                    },
                }));
            }
        }
        specs
    }

    /// 函数名 → (服务器名, 工具)；没有这个工具（服务器没连上、名字不对）时为 None。
    pub fn lookup(&self, function: &str) -> Option<(String, McpTool)> {
        self.servers.lock().unwrap().iter().find_map(|server| {
            let name = &server.config.name;
            let tools = server.tools.lock().unwrap();
            tools
                .iter()
                .find(|t| function_name(name, &t.name) == function)
                .map(|t| (name.clone(), t.clone()))
        })
    }

    /// 调用一个工具；`stopped` 为真时发取消通知并放弃等待。
    pub fn call(
        &self,
        server: &str,
        tool: &str,
        arguments: Value,
        stopped: &dyn Fn() -> bool,
    ) -> ToolOutput {
        let client = self
            .servers
            .lock()
            .unwrap()
            .iter()
            .find(|s| s.config.name == server)
            .and_then(|s| s.client.lock().unwrap().clone());
        let Some(client) = client else {
            return ToolOutput::fail(
                format!("MCP server `{server}` is not connected."),
                "服务器没连上",
            );
        };
        let params = json!({ "name": tool, "arguments": arguments });
        match client.request("tools/call", params, CALL_TIMEOUT, stopped) {
            Ok(result) => render_result(&result),
            Err(e) if stopped() => ToolOutput::fail(format!("cancelled: {e}"), "已中断"),
            Err(e) => ToolOutput::fail(format!("MCP error: {e}"), short(&e, 40)),
        }
    }
}

impl Drop for McpHub {
    fn drop(&mut self) {
        self.stop();
    }
}

/// 后台连接一个服务器，成功 / 失败都上报（已经换代的不报）。一开始就没连上的不自动重试。
fn connect(hub: Weak<McpHub>, server: Arc<Server>, generation: u64) {
    let result = establish(&hub, &server, generation);
    let Some(hub) = hub.upgrade() else {
        return;
    };
    if hub.generation.load(Ordering::SeqCst) != generation {
        return;
    }
    match result {
        Ok((client, tools)) => server.ready(client, tools),
        Err(reason) => *server.state.lock().unwrap() = McpState::Failed(reason),
    }
    hub.report();
}

/// 连上过的服务断了：按 [`RECONNECT_DELAYS`] 退避重连，期间状态是「重连中」；都失败就停在「没连上」。
fn reconnect(hub: Weak<McpHub>, server: Arc<Server>, generation: u64, mut reason: String) {
    let current = |hub: &McpHub| hub.generation.load(Ordering::SeqCst) == generation;
    for (attempt, delay) in RECONNECT_DELAYS.iter().enumerate() {
        {
            let Some(hub) = hub.upgrade().filter(|h| current(h)) else { return };
            *server.state.lock().unwrap() = McpState::Reconnecting(format!(
                "{reason}\n{} 秒后第 {} 次重连",
                delay.as_secs(),
                attempt + 1
            ));
            hub.report();
        }
        std::thread::sleep(*delay);
        if hub.upgrade().is_none_or(|h| !current(&h)) {
            return;
        }
        // 期间点了「重新连接」或重新加载，已经有人在连
        if *server.state.lock().unwrap() == McpState::Connecting {
            return;
        }
        match establish(&hub, &server, generation) {
            Ok((client, tools)) => {
                let Some(hub) = hub.upgrade().filter(|h| current(h)) else { return };
                server.ready(client, tools);
                hub.report();
                return;
            }
            Err(e) => reason = e,
        }
    }
    if let Some(hub) = hub.upgrade().filter(|h| current(h)) {
        *server.state.lock().unwrap() = McpState::Failed(reason);
        hub.report();
    }
}

/// 连一次：起进程 / 建连接 → 握手 → 列工具。
fn establish(
    hub: &Weak<McpHub>,
    server: &Arc<Server>,
    generation: u64,
) -> Result<(Arc<Client>, Vec<McpTool>), String> {
    let cwd = hub.upgrade().ok_or("内核已关闭")?.cwd.clone();
    // 断了：连上过的转去重连；还在握手时断的由握手那边报错
    let on_exit = {
        let (hub, server) = (hub.clone(), Arc::downgrade(server));
        move |reason: String| {
            let (Some(h), Some(server)) = (hub.upgrade(), server.upgrade()) else { return };
            if h.generation.load(Ordering::SeqCst) != generation
                || *server.state.lock().unwrap() != McpState::Ready
            {
                return;
            }
            server.tools.lock().unwrap().clear();
            server.client.lock().unwrap().take();
            let _ = std::thread::Builder::new()
                .name(format!("mcp-reconnect-{}", server.config.name))
                .spawn(move || reconnect(hub, server, generation, reason));
        }
    };
    let on_tools_changed = {
        let (hub, server) = (hub.clone(), Arc::downgrade(server));
        move || {
            let (hub, server) = (hub.clone(), server.clone());
            let _ = std::thread::Builder::new().name("mcp-relist".into()).spawn(move || {
                let (Some(hub), Some(server)) = (hub.upgrade(), server.upgrade()) else {
                    return;
                };
                let client = server.client.lock().unwrap().clone();
                if let Some(client) = client
                    && let Ok(tools) = list_tools(&client)
                    && hub.generation.load(Ordering::SeqCst) == generation
                {
                    *server.tools.lock().unwrap() = tools;
                    hub.report();
                }
            });
        }
    };
    let client = Client::open(&server.config, &cwd, on_exit, on_tools_changed)?;
    let init = json!({
        "protocolVersion": PROTOCOL_VERSION,
        "capabilities": {},
        "clientInfo": { "name": "claudesktop", "version": env!("CARGO_PKG_VERSION") },
    });
    client
        .request("initialize", init, INIT_TIMEOUT, &|| false)
        .map_err(|e| client.with_stderr(format!("握手失败：{e}")))?;
    client.notify("notifications/initialized", json!({}));
    let tools = list_tools(&client).map_err(|e| client.with_stderr(format!("列工具失败：{e}")))?;
    Ok((client, tools))
}

impl Server {
    /// 连上了：换上新连接和工具。
    fn ready(&self, client: Arc<Client>, tools: Vec<McpTool>) {
        *self.tools.lock().unwrap() = tools;
        *self.client.lock().unwrap() = Some(client);
        *self.state.lock().unwrap() = McpState::Ready;
    }
}

/// `tools/list`（按 `nextCursor` 翻页）。
fn list_tools(client: &Client) -> Result<Vec<McpTool>, String> {
    let mut tools = Vec::new();
    let mut cursor: Option<String> = None;
    loop {
        let params = match &cursor {
            Some(c) => json!({ "cursor": c }),
            None => json!({}),
        };
        let page = client.request("tools/list", params, LIST_TIMEOUT, &|| false)?;
        for t in page.get("tools").and_then(Value::as_array).into_iter().flatten() {
            let Some(name) = t.get("name").and_then(Value::as_str) else {
                continue;
            };
            tools.push(McpTool {
                name: name.to_string(),
                description: t
                    .get("description")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .into(),
                input_schema: t
                    .get("inputSchema")
                    .cloned()
                    .unwrap_or_else(|| json!({ "type": "object", "properties": {} })),
                read_only: t
                    .pointer("/annotations/readOnlyHint")
                    .and_then(Value::as_bool)
                    .unwrap_or(false),
            });
        }
        cursor = page.get("nextCursor").and_then(Value::as_str).map(String::from);
        if cursor.is_none() {
            return Ok(tools);
        }
    }
}

/// `tools/call` 的结果 → 工具输出：文本拼起来，图片 / 资源写一行占位；`isError` 算失败。
fn render_result(result: &Value) -> ToolOutput {
    let mut parts = Vec::new();
    for item in result.get("content").and_then(Value::as_array).into_iter().flatten() {
        let field = |k: &str| item.get(k).and_then(Value::as_str).unwrap_or_default();
        match field("type") {
            "text" => parts.push(field("text").to_string()),
            "image" => parts.push(format!("[image {}]", field("mimeType"))),
            "audio" => parts.push(format!("[audio {}]", field("mimeType"))),
            "resource" => {
                let r = item.get("resource");
                let text = r.and_then(|r| r.get("text")).and_then(Value::as_str);
                let uri = r.and_then(|r| r.get("uri")).and_then(Value::as_str).unwrap_or_default();
                parts.push(text.map_or_else(|| format!("[resource {uri}]"), String::from));
            }
            "resource_link" => parts.push(format!("[resource {}]", field("uri"))),
            _ => {}
        }
    }
    // 只给了结构化结果
    if parts.is_empty()
        && let Some(structured) = result.get("structuredContent")
    {
        parts.push(structured.to_string());
    }
    let mut content = parts.join("\n");
    if content.chars().count() > OUTPUT_MAX_CHARS {
        content =
            content.chars().take(OUTPUT_MAX_CHARS).collect::<String>() + "\n[output truncated]";
    }
    let summary = content
        .lines()
        .find(|l| !l.trim().is_empty())
        .map_or_else(|| "完成".into(), |l| short(l.trim(), 60));
    if result.get("isError").and_then(Value::as_bool).unwrap_or(false) {
        ToolOutput::fail(content, format!("出错：{summary}"))
    } else {
        ToolOutput::ok(content, summary)
    }
}

/// 截到 `n` 个字符，超出加省略号。
fn short(text: &str, n: usize) -> String {
    if text.chars().count() > n {
        text.chars().take(n).collect::<String>() + "…"
    } else {
        text.to_string()
    }
}

type Reply = Result<Value, String>;
/// 断线时的回调（只调一次）
type OnExit = Box<dyn FnOnce(String) + Send>;

/// 和服务器之间怎么收发。
enum Pipe {
    Stdio {
        stdin: Mutex<ChildStdin>,
        child: Mutex<Child>,
    },
    /// 建好 `Client` 之后才连（旧版 SSE 的读线程要拿着它的弱引用）
    Http(OnceLock<Http>),
}

/// 一个服务器上的 JSON-RPC 连接。释放时结束子进程（连同子孙）/ 结束远程会话。
struct Client {
    pipe: Pipe,
    /// 自己的弱引用：远程发消息的线程要拿着连接
    me: Weak<Client>,
    pending: Mutex<HashMap<u64, mpsc::Sender<Reply>>>,
    next_id: AtomicU64,
    alive: AtomicBool,
    /// stderr 的开头和结尾几行（失败时附在原因后面；远程服务没有）
    stderr: Mutex<StderrLog>,
    on_tools_changed: Box<dyn Fn() + Send + Sync>,
    /// 断线时调一次
    on_exit: Mutex<Option<OnExit>>,
}

impl Client {
    fn open(
        config: &ServerConfig,
        cwd: &Path,
        on_exit: impl FnOnce(String) + Send + 'static,
        on_tools_changed: impl Fn() + Send + Sync + 'static,
    ) -> Result<Arc<Self>, String> {
        let new = |pipe: Pipe| {
            Arc::new_cyclic(|me| Self {
                pipe,
                me: me.clone(),
                pending: Mutex::new(HashMap::new()),
                next_id: AtomicU64::new(1),
                alive: AtomicBool::new(true),
                stderr: Mutex::new(StderrLog::default()),
                on_tools_changed: Box::new(on_tools_changed),
                on_exit: Mutex::new(Some(Box::new(on_exit))),
            })
        };
        let kind = match config.transport {
            Transport::Stdio => None,
            Transport::Http => Some(mcp_http::Kind::Streamable),
            Transport::Sse => Some(mcp_http::Kind::Legacy),
        };
        if let Some(kind) = kind {
            let client = new(Pipe::Http(OnceLock::new()));
            let sink: Weak<dyn Sink> = client.me.clone();
            let http = Http::open(kind, &config.url, config.headers.clone(), sink)?;
            if let Pipe::Http(slot) = &client.pipe {
                let _ = slot.set(http);
            }
            return Ok(client);
        }
        let mut child = spawn_process(config, cwd)?;
        let stdin = child.stdin.take().ok_or("拿不到子进程的 stdin")?;
        let stdout = child.stdout.take().ok_or("拿不到子进程的 stdout")?;
        let stderr = child.stderr.take();
        let client = new(Pipe::Stdio { stdin: Mutex::new(stdin), child: Mutex::new(child) });
        if let Some(stderr) = stderr {
            let weak = Arc::downgrade(&client);
            let _ = std::thread::Builder::new().name("mcp-stderr".into()).spawn(move || {
                for line in BufReader::new(stderr).lines().map_while(Result::ok) {
                    let Some(client) = weak.upgrade() else { return };
                    client.stderr.lock().unwrap().push(line);
                }
            });
        }
        // 读线程只拿弱引用：Client 释放（结束子进程）后 stdout 关闭，线程自己退出
        let weak = Arc::downgrade(&client);
        let _ = std::thread::Builder::new().name("mcp-stdout".into()).spawn(move || {
            for line in BufReader::new(stdout).lines().map_while(Result::ok) {
                let Some(client) = weak.upgrade() else { return };
                if let Ok(msg) = serde_json::from_str(&line) {
                    client.dispatch(msg);
                }
            }
            if let Some(client) = weak.upgrade() {
                client.closed("服务器进程已退出".into());
            }
        });
        Ok(client)
    }

    /// 处理服务器发来的一条消息：响应交给等它的请求；服务器的请求（ping 等）就地应答；通知按需处理。
    fn dispatch(&self, msg: Value) {
        let method = msg.get("method").and_then(Value::as_str);
        match (msg.get("id"), method) {
            (Some(id), None) => {
                let Some(id) = id.as_u64() else { return };
                let reply = match msg.get("error") {
                    Some(e) => Err(e
                        .get("message")
                        .and_then(Value::as_str)
                        .unwrap_or("未知错误")
                        .to_string()),
                    None => Ok(msg.get("result").cloned().unwrap_or(Value::Null)),
                };
                if let Some(tx) = self.pending.lock().unwrap().remove(&id) {
                    let _ = tx.send(reply);
                }
            }
            (Some(id), Some(method)) => {
                let reply = if method == "ping" {
                    json!({ "jsonrpc": "2.0", "id": id, "result": {} })
                } else {
                    // 没声明 roots / sampling 等能力，别的请求一律不支持
                    json!({ "jsonrpc": "2.0", "id": id, "error": { "code": -32601, "message": "Method not found" } })
                };
                self.send(&reply);
            }
            (None, Some("notifications/tools/list_changed")) => (self.on_tools_changed)(),
            _ => {}
        }
    }

    /// 发一条消息；本地写不进 stdin 时返回 false（远程的失败经 [`Sink::fail`] 回来）。
    fn send(&self, msg: &Value) -> bool {
        match &self.pipe {
            Pipe::Stdio { stdin, .. } => {
                let mut line = msg.to_string();
                line.push('\n');
                let mut stdin = stdin.lock().unwrap();
                stdin.write_all(line.as_bytes()).and_then(|()| stdin.flush()).is_ok()
            }
            Pipe::Http(http) => {
                let (Some(http), Some(me)) = (http.get(), self.me.upgrade()) else {
                    return false;
                };
                http.send(msg, me);
                true
            }
        }
    }

    fn notify(&self, method: &str, params: Value) {
        self.send(&json!({ "jsonrpc": "2.0", "method": method, "params": params }));
    }

    /// 发一个请求并等响应。超时、被停下或进程退出时返回原因；被停下时顺带告诉服务器取消。
    fn request(
        &self,
        method: &str,
        params: Value,
        timeout: Duration,
        stopped: &dyn Fn() -> bool,
    ) -> Reply {
        if !self.alive.load(Ordering::SeqCst) {
            return Err("连接已断开".into());
        }
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let (tx, rx) = mpsc::channel();
        self.pending.lock().unwrap().insert(id, tx);
        if !self.send(&json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params })) {
            self.pending.lock().unwrap().remove(&id);
            return Err("写不进服务器的 stdin".into());
        }
        let deadline = Instant::now() + timeout;
        loop {
            match rx.recv_timeout(Duration::from_millis(100)) {
                Ok(reply) => return reply,
                Err(mpsc::RecvTimeoutError::Disconnected) => return Err("连接已断开".into()),
                Err(mpsc::RecvTimeoutError::Timeout) => {
                    let gave_up = if stopped() {
                        Some("已取消")
                    } else if Instant::now() >= deadline {
                        Some("超时")
                    } else {
                        None
                    };
                    if let Some(reason) = gave_up {
                        self.pending.lock().unwrap().remove(&id);
                        self.notify(
                            "notifications/cancelled",
                            json!({ "requestId": id, "reason": reason }),
                        );
                        return Err(format!("{method} {reason}"));
                    }
                }
            }
        }
    }

    /// 原因后面附上 stderr 的开头和结尾几行。
    fn with_stderr(&self, reason: String) -> String {
        match self.stderr.lock().unwrap().render() {
            Some(log) => format!("{reason}\n{log}"),
            None => reason,
        }
    }
}

impl Sink for Client {
    fn message(&self, msg: Value) {
        self.dispatch(msg);
    }

    fn fail(&self, id: u64, reason: String) {
        if let Some(tx) = self.pending.lock().unwrap().remove(&id) {
            let _ = tx.send(Err(reason));
        }
    }

    /// 断了：等着的请求都报错，通知外面（只通知一次）。
    fn closed(&self, reason: String) {
        self.alive.store(false, Ordering::SeqCst);
        for (_, tx) in self.pending.lock().unwrap().drain() {
            let _ = tx.send(Err(reason.clone()));
        }
        let on_exit = self.on_exit.lock().unwrap().take();
        if let Some(on_exit) = on_exit {
            on_exit(self.with_stderr(reason));
        }
    }
}

/// stderr 只留开头和结尾各 [`STDERR_LINES`] 行（空行不算），中间的数一下。
#[derive(Default)]
struct StderrLog {
    head: Vec<String>,
    tail: VecDeque<String>,
    dropped: usize,
}

impl StderrLog {
    fn push(&mut self, line: String) {
        if line.trim().is_empty() {
            return;
        }
        if self.head.len() < STDERR_LINES {
            self.head.push(line);
            return;
        }
        self.tail.push_back(line);
        if self.tail.len() > STDERR_LINES {
            self.tail.pop_front();
            self.dropped += 1;
        }
    }

    fn render(&self) -> Option<String> {
        if self.head.is_empty() {
            return None;
        }
        let mut lines = self.head.clone();
        if self.dropped > 0 {
            lines.push(format!("…（省略 {} 行）", self.dropped));
        }
        lines.extend(self.tail.iter().cloned());
        Some(lines.join("\n"))
    }
}

impl Drop for Client {
    fn drop(&mut self) {
        // 远程的会话由 `Http` 的 Drop 结束
        if let Pipe::Stdio { child, .. } = &self.pipe {
            crate::tools::kill_tree(&mut child.lock().unwrap());
        }
    }
}

/// 起服务器进程。Windows 上 `npx` 这类是 `.cmd` 脚本，直接起找不到，就套一层 `cmd /C` 再试。
fn spawn_process(config: &ServerConfig, cwd: &Path) -> Result<Child, String> {
    let build = |program: &str, args: &[String]| {
        let mut cmd = Command::new(program);
        cmd.args(args)
            .envs(&config.env)
            .current_dir(cwd)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt as _;
            cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
        }
        cmd
    };
    match build(&config.command, &config.args).spawn() {
        Ok(child) => Ok(child),
        #[cfg(windows)]
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            let mut args = vec!["/C".to_string(), config.command.clone()];
            args.extend(config.args.iter().cloned());
            build("cmd", &args).spawn().map_err(|e| format!("起不来 `{}`：{e}", config.command))
        }
        Err(e) => Err(format!("起不来 `{}`：{e}", config.command)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_claude_code_format() {
        let servers = parse_config(
            r#"{"mcpServers": {
                "fs": {"command": "npx", "args": ["-y", "server-fs", "C:/x"], "env": {"K": "V"}},
                "off": {"command": "x", "disabled": true},
                "web": {"type": "http", "url": "https://example.com/mcp", "headers": {"X": "1"}},
                "bare": {"url": "http://h/mcp"},
                "old": {"type": "sse", "url": "http://h/sse"},
                "nourl": {"type": "http"},
                "odd": {"type": "ws", "url": "ws://h"},
                "bad": {"args": []}
            }}"#,
        )
        .unwrap();
        let by = |n: &str| servers.iter().find(|s| s.name == n).unwrap();
        assert_eq!(by("fs").args, ["-y", "server-fs", "C:/x"]);
        assert_eq!(by("fs").env.get("K").map(String::as_str), Some("V"));
        assert!(by("fs").unsupported.is_none());
        assert!(by("off").disabled);
        assert_eq!(
            (by("web").transport, by("web").unsupported.as_deref()),
            (Transport::Http, None)
        );
        assert_eq!(by("web").headers.get("X").map(String::as_str), Some("1"));
        // 没写 type、有 url 的是 Streamable HTTP
        assert_eq!(by("bare").transport, Transport::Http);
        assert_eq!(by("old").transport, Transport::Sse);
        assert!(by("nourl").unsupported.as_deref().unwrap().contains("url"));
        assert!(by("odd").unsupported.as_deref().unwrap().contains("ws"));
        assert!(by("bad").unsupported.is_some());
        assert!(parse_config("{}").is_err());
        assert!(parse_config("not json").is_err());
        assert!(parse_config(CONFIG_TEMPLATE).is_ok());
    }

    #[test]
    fn expands_environment_variables() {
        let env = |k: &str| (k == "TOKEN").then(|| "abc".to_string());
        assert_eq!(expand("Bearer ${TOKEN}", &env), "Bearer abc");
        assert_eq!(expand("${NOPE:-dflt}/${TOKEN:-x}", &env), "dflt/abc");
        assert_eq!(expand("${NOPE}-${unclosed", &env), "-${unclosed");
    }

    #[test]
    fn stderr_keeps_head_and_tail() {
        let mut log = StderrLog::default();
        assert_eq!(log.render(), None);
        for i in 0..12 {
            log.push(format!("l{i}"));
            log.push(String::new());
        }
        assert_eq!(log.render().unwrap(), "l0\nl1\nl2\nl3\n…（省略 4 行）\nl8\nl9\nl10\nl11");
    }

    #[test]
    fn function_names_are_safe() {
        assert_eq!(function_name("github", "search_issues"), "mcp__github__search_issues");
        assert_eq!(function_name("my server", "a.b"), "mcp__my_server__a_b");
        assert_eq!(function_name(&"s".repeat(80), "t").len(), 64);
        assert!(is_mcp_tool("mcp__x__y") && !is_mcp_tool("read_file"));
    }

    #[test]
    fn renders_text_images_and_errors() {
        let ok = render_result(&json!({
            "content": [
                {"type": "text", "text": "第一行\n第二行"},
                {"type": "image", "mimeType": "image/png", "data": "..."},
            ]
        }));
        assert!(ok.ok);
        assert_eq!(ok.content, "第一行\n第二行\n[image image/png]");
        assert_eq!(ok.summary, "第一行");
        let err = render_result(
            &json!({ "content": [{"type": "text", "text": "boom"}], "isError": true }),
        );
        assert!(!err.ok);
        assert_eq!(err.summary, "出错：boom");
        let structured = render_result(&json!({ "content": [], "structuredContent": {"n": 1} }));
        assert_eq!(structured.content, r#"{"n":1}"#);
    }
}
