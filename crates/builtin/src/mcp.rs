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
    /// OAuth 配置（`mcp.json` 里的 `"oauth"`，可省）
    pub oauth: crate::mcp_oauth::OAuthConfig,
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

/// 往 `mcp.json` 里加一个服务器（设置页「推荐」一键添加）：文件不存在就新建；同名的已经有了不覆盖。
pub fn add_server(path: &Path, name: &str, entry: Value) -> Result<(), String> {
    edit_config(path, |servers| {
        if servers.contains_key(name) {
            return Err(format!("mcp.json 里已经有 {name} 了"));
        }
        servers.insert(name.to_string(), entry);
        Ok(())
    })
}

/// 从 `mcp.json` 删掉一个服务器。
pub fn remove_server(path: &Path, name: &str) -> Result<(), String> {
    edit_config(path, |servers| {
        servers.remove(name).map(drop).ok_or_else(|| format!("mcp.json 里没有 {name}"))
    })
}

/// 打开 / 关掉一个服务器（`"disabled": true`）。
pub fn set_server_disabled(path: &Path, name: &str, disabled: bool) -> Result<(), String> {
    edit_config(path, |servers| {
        let entry = servers
            .get_mut(name)
            .and_then(Value::as_object_mut)
            .ok_or_else(|| format!("mcp.json 里没有 {name}"))?;
        if disabled {
            entry.insert("disabled".into(), Value::Bool(true));
        } else {
            entry.remove("disabled");
        }
        Ok(())
    })
}

/// 读出 `mcpServers` 改一下再写回（键的顺序不变）；文件格式不对就不动它。
fn edit_config(
    path: &Path,
    change: impl FnOnce(&mut serde_json::Map<String, Value>) -> Result<(), String>,
) -> Result<(), String> {
    let mut root: Value = match std::fs::read_to_string(path) {
        Ok(text) => serde_json::from_str(&text)
            .map_err(|e| format!("mcp.json 格式不对：{}", claudesktop_core::json_error_text(&e)))?,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => serde_json::json!({}),
        Err(e) => return Err(format!("读不了 {}：{e}", path.display())),
    };
    let Some(object) = root.as_object_mut() else {
        return Err("mcp.json 应该是一个 JSON 对象".into());
    };
    let servers = object.entry("mcpServers").or_insert_with(|| serde_json::json!({}));
    let Some(servers) = servers.as_object_mut() else {
        return Err("mcp.json 里的 \"mcpServers\" 应该是对象".into());
    };
    change(servers)?;
    let text = serde_json::to_string_pretty(&root).unwrap_or_default() + "\n";
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| format!("建不了 {}：{e}", dir.display()))?;
    }
    std::fs::write(path, text).map_err(|e| format!("写不了 {}：{e}", path.display()))
}

fn parse_config(text: &str) -> Result<Vec<ServerConfig>, String> {
    let root: Value = serde_json::from_str(text)
        .map_err(|e| format!("mcp.json 格式不对：{}", claudesktop_core::json_error_text(&e)))?;
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
                oauth: oauth_config(s.get("oauth"), &env),
            }
        })
        .collect())
}

/// 读 `"oauth"` 那一段：`client_id` / `callback_port` / `scope` 都可省。
fn oauth_config(
    value: Option<&Value>,
    env: &dyn Fn(&str) -> Option<String>,
) -> crate::mcp_oauth::OAuthConfig {
    let Some(v) = value.filter(|v| v.is_object()) else {
        return crate::mcp_oauth::OAuthConfig::default();
    };
    let text = |key: &str| {
        v.get(key).and_then(Value::as_str).map(|s| expand(s, env)).filter(|s| !s.trim().is_empty())
    };
    crate::mcp_oauth::OAuthConfig {
        client_id: text("client_id"),
        // 端口写 0 等于不固定：交给系统挑
        callback_port: v
            .get("callback_port")
            .and_then(Value::as_u64)
            .map(|p| p as u16)
            .filter(|p| *p > 0),
        scope: text("scope"),
    }
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
            let stuck = matches!(
                *state,
                McpState::Failed(_) | McpState::Reconnecting(_) | McpState::NeedsAuth { .. }
            );
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
                    retry: matches!(
                        state,
                        McpState::Failed(_)
                            | McpState::Reconnecting(_)
                            | McpState::NeedsAuth { .. }
                    ) && s.config.unsupported.is_none(),
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

/// 建连接时先带上的令牌：手填了 Authorization 就完全不碰 OAuth；否则用存着的那份。
fn initial_auth(config: &ServerConfig) -> Option<Arc<crate::mcp_oauth::Auth>> {
    let manual = config.headers.keys().any(|k| k.eq_ignore_ascii_case("authorization"));
    if manual {
        return None;
    }
    crate::mcp_oauth::load(&config.url).map(crate::mcp_oauth::Auth::new)
}

/// 走一遍授权：发现 → 注册 → 等浏览器回调 → 换 token。期间把授权地址报给界面（「登录」按钮）。
fn authorize(
    config: &ServerConfig,
    challenge: &str,
    on_auth: &mut dyn FnMut(&str),
) -> Result<Arc<crate::mcp_oauth::Auth>, String> {
    let pending = crate::mcp_oauth::prepare(&config.url, challenge, &config.oauth)
        .map_err(|e| format!("这个服务器要求 OAuth 授权，但{e}"))?;
    on_auth(&pending.url);
    let token = pending.wait()?;
    Ok(crate::mcp_oauth::Auth::new(token))
}

/// 连一次：起进程 / 建连接 → 握手 → 列工具；远程服务要授权时先走一遍 OAuth 再重来。
fn establish(
    hub: &Weak<McpHub>,
    server: &Arc<Server>,
    generation: u64,
) -> Result<(Arc<Client>, Vec<McpTool>), String> {
    let cwd = hub.upgrade().ok_or("内核已关闭")?.cwd.clone();
    // 断了：连上过的转去重连；还在握手时断的由握手那边报错
    let on_exit: Arc<dyn Fn(String) + Send + Sync> = {
        let (hub, server) = (hub.clone(), Arc::downgrade(server));
        Arc::new(move |reason: String| {
            let (Some(h), Some(server)) = (hub.upgrade(), server.upgrade()) else { return };
            if h.generation.load(Ordering::SeqCst) != generation
                || *server.state.lock().unwrap() != McpState::Ready
            {
                return;
            }
            server.tools.lock().unwrap().clear();
            server.client.lock().unwrap().take();
            let who = server.config.name.clone();
            let (hub, server) = (hub.clone(), server.clone());
            let _ = std::thread::Builder::new()
                .name(format!("mcp-reconnect-{who}"))
                .spawn(move || reconnect(hub, server, generation, reason));
        })
    };
    let on_tools_changed: Arc<dyn Fn() + Send + Sync> = {
        let (hub, server) = (hub.clone(), Arc::downgrade(server));
        Arc::new(move || {
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
        })
    };
    let mut on_auth = {
        let (hub, server) = (hub.clone(), server.clone());
        move |url: &str| {
            let Some(hub) = hub.upgrade() else { return };
            if hub.generation.load(Ordering::SeqCst) != generation {
                return;
            }
            *server.state.lock().unwrap() = McpState::NeedsAuth { url: url.to_string() };
            hub.report();
        }
    };
    let challenge = Arc::new(Mutex::new(None));
    let mut auth = initial_auth(&server.config);
    // 这一轮刚登录过：再回 401 就别再要登录了（服务器不认它自己发的令牌，登多少次都一样）
    let mut signed_in = false;
    let client = loop {
        // 只看这一次握手的 401，上一轮留下的挑战不算
        *challenge.lock().unwrap() = None;
        let client = Client::open(
            &server.config,
            &cwd,
            on_exit.clone(),
            on_tools_changed.clone(),
            auth.clone(),
            challenge.clone(),
        )?;
        let init = json!({
            "protocolVersion": PROTOCOL_VERSION,
            "capabilities": {},
            "clientInfo": { "name": "claudesktop", "version": env!("CARGO_PKG_VERSION") },
        });
        let init = client
            .request("initialize", init, INIT_TIMEOUT, &|| false)
            .map_err(|e| client.with_stderr(format!("握手失败：{e}")));
        match init {
            Ok(_) => break client,
            // 握手时回 401：带 Bearer 挑战就走 OAuth，授权完重连一次
            Err(reason) => {
                let Some(challenge) = challenge.lock().unwrap().clone() else {
                    return Err(reason);
                };
                if !challenge.to_ascii_lowercase().contains("bearer") {
                    return Err(reason);
                }
                if signed_in {
                    return Err(format!("登录完成了，但服务器还是拒绝访问：{reason}"));
                }
                if let Some(stale) = auth.take() {
                    drop(stale);
                    crate::mcp_oauth::forget(&server.config.url);
                }
                auth = Some(authorize(&server.config, &challenge, &mut on_auth)?);
                signed_in = true;
            }
        }
    };
    // 拿到令牌、握手过了：状态回到「连接中」，别把「登录」按钮留着
    if *server.state.lock().unwrap() != McpState::Connecting
        && let Some(hub) = hub.upgrade()
        && hub.generation.load(Ordering::SeqCst) == generation
    {
        *server.state.lock().unwrap() = McpState::Connecting;
        hub.report();
    }
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
        on_exit: Arc<dyn Fn(String) + Send + Sync>,
        on_tools_changed: Arc<dyn Fn() + Send + Sync>,
        auth: Option<Arc<crate::mcp_oauth::Auth>>,
        challenge: Arc<Mutex<Option<String>>>,
    ) -> Result<Arc<Self>, String> {
        let new = |pipe: Pipe| {
            Arc::new_cyclic(|me| Self {
                pipe,
                me: me.clone(),
                pending: Mutex::new(HashMap::new()),
                next_id: AtomicU64::new(1),
                alive: AtomicBool::new(true),
                stderr: Mutex::new(StderrLog::default()),
                on_tools_changed: Box::new(move || on_tools_changed()),
                on_exit: Mutex::new(Some(Box::new(move |reason| on_exit(reason)))),
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
            let http = Http::open(kind, &config.url, config.headers.clone(), auth, sink, challenge)
                .map_err(|e| match e {
                    mcp_http::OpenError::NeedsAuth(_) => "服务器要求授权".to_string(),
                    mcp_http::OpenError::Failed(reason) => reason,
                })?;
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
            build("cmd", &args).spawn().map_err(|e| spawn_error(&config.command, &e))
        }
        Err(e) => Err(spawn_error(&config.command, &e)),
    }
}

/// 起进程失败的原因：最常见的是命令没装 / 不在 PATH 里。
fn spawn_error(command: &str, e: &std::io::Error) -> String {
    match e.kind() {
        // 推荐列表里的服务大多靠这两个跑：直接说该装什么
        std::io::ErrorKind::NotFound if command == "npx" => {
            "找不到命令 `npx`：先安装 Node.js（nodejs.org），装好后重启本应用".into()
        }
        std::io::ErrorKind::NotFound if command == "uvx" => {
            "找不到命令 `uvx`：先安装 uv（docs.astral.sh/uv），装好后重启本应用".into()
        }
        std::io::ErrorKind::NotFound => {
            format!("找不到命令 `{command}`：确认它已经装好、在 PATH 里，或者写完整路径")
        }
        std::io::ErrorKind::PermissionDenied => format!("没有权限运行 `{command}`"),
        _ => format!("起不来 `{command}`：{e}"),
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
    fn edits_servers_in_place() {
        let dir = std::env::temp_dir().join(format!("cd-mcp-edit-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let path = dir.join("mcp.json");
        // 文件还没有：新建
        add_server(&path, "b", serde_json::json!({"command": "x"})).unwrap();
        add_server(&path, "a", serde_json::json!({"type": "http", "url": "https://a"})).unwrap();
        assert!(add_server(&path, "a", serde_json::json!({})).is_err());
        let names: Vec<String> = load_config(&path).unwrap().into_iter().map(|c| c.name).collect();
        assert_eq!(names, ["b", "a"]);
        set_server_disabled(&path, "b", true).unwrap();
        assert!(load_config(&path).unwrap()[0].disabled);
        set_server_disabled(&path, "b", false).unwrap();
        assert!(!load_config(&path).unwrap()[0].disabled);
        remove_server(&path, "b").unwrap();
        assert_eq!(load_config(&path).unwrap().len(), 1);
        // 文件坏了：不动它
        std::fs::write(&path, "{ oops").unwrap();
        assert!(add_server(&path, "c", serde_json::json!({})).is_err());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "{ oops");
        let _ = std::fs::remove_dir_all(&dir);
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
