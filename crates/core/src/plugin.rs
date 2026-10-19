//! 插件：数据目录 `plugins/` 下的一个个文件夹，打包给 agent 用的能力。格式兼容 Claude Code 的插件：
//!
//! ```text
//! plugins/<名字>/
//!   .claude-plugin/plugin.json   清单：name / version / description（可以没有，名字取文件夹名）
//!   commands/*.md                斜杠命令：正文是提示词模板（$ARGUMENTS / $1 $2 …），frontmatter 里写说明
//!   skills/<技能>/SKILL.md       技能：frontmatter 写 name / description，模型用到时再读全文
//!   agents/*.md                  子 agent：frontmatter 写 name / description / tools / model，正文是它的系统提示词
//!   .mcp.json                    MCP 服务（格式同数据目录的 mcp.json）
//!   hooks/hooks.json             钩子：在提交消息、执行工具前后等时机跑的命令
//! ```
//!
//! 清单里的 `mcpServers` / `hooks` 也可以直接写对象或指向别的文件。命令、参数、环境变量里的
//! `${CLAUDE_PLUGIN_ROOT}` 换成插件目录。
//!
//! `plugins/` 下的一个文件夹也可以是 Claude Code 的插件市场仓库（`.claude-plugin/marketplace.json`
//! 列出仓库里的几个插件），这时它带来的是清单里的那几个插件。从 git 地址装、更新、卸载见 [`crate::plugin_git`]。
//!
//! 哪些插件启用由界面决定（设置里记着），这里只负责读：[`discover`] 列出全部插件，
//! 读坏了的也列出来、带上原因。斜杠命令的展开（[`expand_command`]）在界面发消息时做，
//! 所有内核都能用；技能、子 agent、钩子目前只有自研内核接，MCP 服务自研内核和 ACP agent 都接。

use serde_json::{Map, Value};
use std::collections::BTreeMap;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// 插件目录里代表插件根目录的占位符
const ROOT_VAR: &str = "${CLAUDE_PLUGIN_ROOT}";
/// 钩子没写超时的时候等多久
const HOOK_TIMEOUT: Duration = Duration::from_secs(60);
/// 钩子输出最多留这么多字符（回给模型 / 显示在界面上）
const HOOK_OUTPUT_MAX: usize = 10_000;

/// 一个插件。
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Plugin {
    /// 清单里的名字（没有清单时是文件夹名）；启用状态按它记
    pub name: String,
    pub version: Option<String>,
    pub description: String,
    /// 插件目录
    pub root: PathBuf,
    /// 从 git 克隆来的：仓库目录（`plugins/` 下那一层，市场仓库里的几个插件共用一个）；
    /// 手动放进来的为 None。更新、卸载按它来
    pub repo: Option<PathBuf>,
    pub commands: Vec<PluginCommand>,
    pub skills: Vec<Skill>,
    pub agents: Vec<AgentDef>,
    pub mcp_servers: Vec<PluginMcpServer>,
    pub hooks: Vec<Hook>,
    /// 读的时候出的问题（清单 / 配置文件格式不对等）；有问题的部分跳过，其余照常可用
    pub problems: Vec<String>,
}

/// 斜杠命令：用户输入 `/名字 参数`，发出去的是展开后的模板。
#[derive(Debug, Clone, PartialEq)]
pub struct PluginCommand {
    /// 命令名（文件名去掉 `.md`；`commands/` 下一层子目录里的是 `目录:名字`）
    pub name: String,
    pub description: String,
    /// 参数提示（frontmatter 的 `argument-hint`），补全列表里显示
    pub argument_hint: Option<String>,
    /// 去掉 frontmatter 的正文
    pub body: String,
}

/// 技能：模型只先看到名字和说明，用到时自己读 `path`。
#[derive(Debug, Clone, PartialEq)]
pub struct Skill {
    pub name: String,
    pub description: String,
    /// `SKILL.md` 的绝对路径
    pub path: PathBuf,
}

/// 子 agent：主模型用 task 工具把子任务交给它，它在独立的上下文里跑完、把结果交回来。
#[derive(Debug, Clone, PartialEq)]
pub struct AgentDef {
    pub name: String,
    /// 什么时候该用它（主模型据此挑选）
    pub description: String,
    /// 能用的工具（frontmatter 的 `tools`，逗号分开；Claude Code 的叫法和内置工具名都认）；None = 全部
    pub tools: Option<Vec<String>>,
    /// 用哪个模型；None = 跟主会话（`inherit` 和 Claude Code 的 sonnet / opus / haiku 别名也按跟主会话）
    pub model: Option<String>,
    /// 正文：它的系统提示词
    pub prompt: String,
    /// 来自哪个插件（内置的为空）
    pub plugin: String,
}

/// 插件提供的 MCP 服务（本地命令或远程 HTTP / SSE，占位符已替换）。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PluginMcpServer {
    pub name: String,
    pub command: String,
    pub args: Vec<String>,
    pub env: BTreeMap<String, String>,
    /// 远程服务的地址；空 = 本地命令
    pub url: String,
    /// 远程服务走旧版 HTTP + SSE（`"type": "sse"`）；否则 Streamable HTTP
    pub sse: bool,
    pub headers: BTreeMap<String, String>,
}

/// 钩子触发的时机（名字同 Claude Code）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum HookEvent {
    /// 会话的第一轮开始前；输出作为补充上下文交给模型
    SessionStart,
    /// 用户消息发给模型前；退出码 2 = 拦下这条消息，输出作为补充上下文
    UserPromptSubmit,
    /// 执行工具前（授权之前）；退出码 2 = 不执行，原因回给模型
    PreToolUse,
    /// 工具执行完；退出码 2 = 把原因追加到工具结果里给模型看
    PostToolUse,
    /// 一轮结束后；只是通知，结果不影响这一轮
    Stop,
    /// 子 agent 跑完、结果交回主会话前；只是通知
    SubagentStop,
}

impl HookEvent {
    pub const ALL: [Self; 6] = [
        Self::SessionStart,
        Self::UserPromptSubmit,
        Self::PreToolUse,
        Self::PostToolUse,
        Self::Stop,
        Self::SubagentStop,
    ];

    /// hooks.json 里的键名
    pub fn key(self) -> &'static str {
        match self {
            Self::SessionStart => "SessionStart",
            Self::UserPromptSubmit => "UserPromptSubmit",
            Self::PreToolUse => "PreToolUse",
            Self::PostToolUse => "PostToolUse",
            Self::Stop => "Stop",
            Self::SubagentStop => "SubagentStop",
        }
    }

    /// 界面上的叫法
    pub fn label(self) -> &'static str {
        match self {
            Self::SessionStart => "会话开始",
            Self::UserPromptSubmit => "提交消息",
            Self::PreToolUse => "执行工具前",
            Self::PostToolUse => "执行工具后",
            Self::Stop => "一轮结束",
            Self::SubagentStop => "子任务结束",
        }
    }
}

/// 一个钩子命令。
#[derive(Debug, Clone, PartialEq)]
pub struct Hook {
    pub event: HookEvent,
    /// 只对哪些工具生效：`Write|Edit` 这样用 `|` 分隔的工具名，空 / `*` = 全部（别的正则写法不认）。
    /// 只对工具类时机有意义
    pub matcher: String,
    /// 交给 shell 跑的命令（占位符已替换）
    pub command: String,
    pub timeout: Duration,
    /// 来自哪个插件、插件目录（跑的时候设成 `CLAUDE_PLUGIN_ROOT`）
    pub plugin: String,
    pub root: PathBuf,
}

/// 默认插件目录：数据目录下的 `plugins/`。
pub fn default_plugins_dir() -> Option<PathBuf> {
    crate::default_data_dir().ok().map(|d| d.join("plugins"))
}

/// 列出 `dir` 下的全部插件（按名字排序）。目录不存在 = 没有插件。
pub fn discover(dir: &Path) -> Vec<Plugin> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut plugins: Vec<Plugin> = entries
        .flatten()
        .filter(|e| e.path().is_dir())
        .filter(|e| !e.file_name().to_string_lossy().starts_with('.'))
        .flat_map(|e| {
            let dir = e.path();
            let repo = dir.join(".git").exists().then(|| dir.clone());
            // 只装了市场里一个插件的仓库：插件在记下的子目录里
            let sub = repo.as_deref().and_then(crate::plugin_git::subdir);
            let mut found = match sub {
                Some(sub) => vec![load(&dir.join(sub))],
                None => match read_json(&dir.join(".claude-plugin").join("marketplace.json")) {
                    Ok(Some(value)) => load_marketplace(&dir, &value),
                    Ok(None) => vec![load(&dir)],
                    Err(e) => {
                        let mut p = load(&dir);
                        p.problems.push(e);
                        vec![p]
                    }
                },
            };
            for p in &mut found {
                p.repo = repo.clone();
            }
            found
        })
        .collect();
    plugins.sort_by(|a, b| a.name.cmp(&b.name));
    plugins
}

/// 插件市场仓库：按 `marketplace.json` 的 `plugins` 逐个读（`source` 是仓库里的相对路径）。
/// 来源是别的仓库 / 网址的条目不去取，列出来、标上原因。
fn load_marketplace(dir: &Path, value: &Value) -> Vec<Plugin> {
    let Some(entries) = value.get("plugins").and_then(Value::as_array) else {
        let mut p = load(dir);
        p.problems.push("marketplace.json 里没有 plugins 列表".into());
        return vec![p];
    };
    entries
        .iter()
        .filter_map(|entry| {
            let name = entry.get("name").and_then(Value::as_str)?.to_string();
            let text = |key: &str| entry.get(key).and_then(Value::as_str).map(str::to_string);
            let mut plugin = match entry.get("source").and_then(Value::as_str) {
                Some(source) => {
                    let mut p = load(&dir.join(source.trim_start_matches("./")));
                    // 插件自己没写清单时，名字、说明用市场里的
                    if !p.root.join(".claude-plugin").join("plugin.json").exists() {
                        p.name = name;
                    }
                    p
                }
                None => Plugin {
                    name,
                    root: dir.to_path_buf(),
                    problems: vec![
                        "这个插件在别的仓库里，暂不支持从市场跟着装，请单独用它的地址安装".into(),
                    ],
                    ..Plugin::default()
                },
            };
            if plugin.description.is_empty() {
                plugin.description = text("description").unwrap_or_default();
            }
            if plugin.version.is_none() {
                plugin.version = text("version");
            }
            Some(plugin)
        })
        .collect()
}

/// 读一个插件目录。
pub fn load(root: &Path) -> Plugin {
    let mut plugin = Plugin {
        name: root.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default(),
        root: root.to_path_buf(),
        ..Plugin::default()
    };
    let manifest_path = root.join(".claude-plugin").join("plugin.json");
    let manifest = match read_json(&manifest_path) {
        Ok(Some(Value::Object(m))) => m,
        Ok(Some(_)) => {
            plugin.problems.push("plugin.json 应该是一个 JSON 对象".into());
            Map::new()
        }
        Ok(None) => Map::new(),
        Err(e) => {
            plugin.problems.push(e);
            Map::new()
        }
    };
    let text = |key: &str| manifest.get(key).and_then(Value::as_str).map(str::to_string);
    if let Some(name) = text("name").filter(|n| !n.trim().is_empty()) {
        plugin.name = name;
    }
    plugin.version = text("version");
    plugin.description = text("description").unwrap_or_default();

    plugin.commands = load_commands(&root.join("commands"));
    plugin.skills = load_skills(&root.join("skills"));
    plugin.agents = load_agents(&root.join("agents"), &plugin.name);

    // MCP：清单里的 mcpServers（对象或文件路径），否则根目录的 .mcp.json
    let mcp = match manifest.get("mcpServers") {
        Some(Value::String(path)) => read_json(&root.join(path)),
        Some(value) => Ok(Some(json_with_key("mcpServers", value.clone()))),
        None => read_json(&root.join(".mcp.json")),
    };
    match mcp {
        Ok(Some(value)) => {
            let (servers, problems) = parse_mcp(&value, root);
            plugin.mcp_servers = servers;
            plugin.problems.extend(problems);
        }
        Ok(None) => {}
        Err(e) => plugin.problems.push(e),
    }

    // 钩子：清单里的 hooks（对象或文件路径），否则 hooks/hooks.json
    let hooks = match manifest.get("hooks") {
        Some(Value::String(path)) => read_json(&root.join(path)),
        Some(value) => Ok(Some(json_with_key("hooks", value.clone()))),
        None => read_json(&root.join("hooks").join("hooks.json")),
    };
    match hooks {
        Ok(Some(value)) => {
            let (hooks, problems) = parse_hooks(&value, &plugin.name, root);
            plugin.hooks = hooks;
            plugin.problems.extend(problems);
        }
        Ok(None) => {}
        Err(e) => plugin.problems.push(e),
    }
    plugin
}

/// 读 JSON 文件：不存在 = `Ok(None)`。
fn read_json(path: &Path) -> Result<Option<Value>, String> {
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(format!("读不了 {}：{e}", file_label(path))),
    };
    serde_json::from_str(&text)
        .map(Some)
        .map_err(|e| format!("{} 格式不对：{}", file_label(path), crate::json_error_text(&e)))
}

/// 报错里的文件名：只留最后两段（`hooks/hooks.json`），整条路径太长
fn file_label(path: &Path) -> String {
    let parts: Vec<_> = path.components().rev().take(2).collect();
    parts.iter().rev().map(|c| c.as_os_str().to_string_lossy()).collect::<Vec<_>>().join("/")
}

/// 清单里直接写的对象：可能已经带着外层键（`{"mcpServers": {...}}`），也可能直接是里面那层
fn json_with_key(key: &str, value: Value) -> Value {
    if value.get(key).is_some() {
        value
    } else {
        let mut m = Map::new();
        m.insert(key.into(), value);
        Value::Object(m)
    }
}

/// 把 `${CLAUDE_PLUGIN_ROOT}` 换成插件目录（正斜杠，shell 里也好用）。
fn substitute(text: &str, root: &Path) -> String {
    text.replace(ROOT_VAR, &root.to_string_lossy().replace('\\', "/"))
}

fn load_commands(dir: &Path) -> Vec<PluginCommand> {
    let mut out = Vec::new();
    let Ok(entries) = std::fs::read_dir(dir) else {
        return out;
    };
    let mut entries: Vec<_> = entries.flatten().map(|e| e.path()).collect();
    entries.sort();
    for path in entries {
        if path.is_dir() {
            // 一层子目录：`目录:名字`
            let prefix = path.file_name().map(|n| n.to_string_lossy().into_owned());
            for mut cmd in load_commands(&path) {
                if let Some(prefix) = &prefix
                    && !cmd.name.contains(':')
                {
                    cmd.name = format!("{prefix}:{}", cmd.name);
                    out.push(cmd);
                }
            }
            continue;
        }
        if path.extension().is_none_or(|e| e != "md") {
            continue;
        }
        let Ok(text) = std::fs::read_to_string(&path) else {
            continue;
        };
        let (front, body) = split_frontmatter(&text);
        let name = path.file_stem().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        out.push(PluginCommand {
            name,
            description: front.get("description").cloned().unwrap_or_default(),
            argument_hint: front.get("argument-hint").cloned().filter(|h| !h.is_empty()),
            body: body.trim().to_string(),
        });
    }
    out
}

fn load_skills(dir: &Path) -> Vec<Skill> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut skills: Vec<Skill> = entries
        .flatten()
        .filter_map(|e| {
            let path = e.path().join("SKILL.md");
            let text = std::fs::read_to_string(&path).ok()?;
            let (front, _) = split_frontmatter(&text);
            let name = front
                .get("name")
                .cloned()
                .filter(|n| !n.is_empty())
                .unwrap_or_else(|| e.file_name().to_string_lossy().into_owned());
            Some(Skill {
                name,
                description: front.get("description").cloned().unwrap_or_default(),
                path,
            })
        })
        .collect();
    skills.sort_by(|a, b| a.name.cmp(&b.name));
    skills
}

fn load_agents(dir: &Path, plugin: &str) -> Vec<AgentDef> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut agents: Vec<AgentDef> = entries
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|e| e == "md"))
        .filter_map(|path| {
            let text = std::fs::read_to_string(&path).ok()?;
            let (front, body) = split_frontmatter(&text);
            let name = front.get("name").cloned().filter(|n| !n.is_empty()).unwrap_or_else(|| {
                path.file_stem().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()
            });
            let tools = front.get("tools").map(|t| {
                t.split(',').map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).collect()
            });
            let model = front
                .get("model")
                .map(|m| m.trim().to_string())
                .filter(|m| !matches!(m.as_str(), "" | "inherit" | "sonnet" | "opus" | "haiku"));
            Some(AgentDef {
                name,
                description: front.get("description").cloned().unwrap_or_default(),
                tools,
                model,
                prompt: body.trim().to_string(),
                plugin: plugin.to_string(),
            })
        })
        .collect();
    agents.sort_by(|a, b| a.name.cmp(&b.name));
    agents
}

/// 拆出开头的 `---` frontmatter（只认单行的 `键: 值`，值的引号去掉），返回 (键值, 正文)。
fn split_frontmatter(text: &str) -> (BTreeMap<String, String>, &str) {
    let mut map = BTreeMap::new();
    let text = text.trim_start_matches('\u{feff}');
    let Some(rest) = text.strip_prefix("---").filter(|r| r.starts_with(['\n', '\r'])) else {
        return (map, text);
    };
    let mut offset = text.len() - rest.len();
    for line in rest.split_inclusive('\n') {
        offset += line.len();
        let line = line.trim_end();
        if line == "---" {
            return (map, &text[offset..]);
        }
        if let Some((key, value)) = line.split_once(':') {
            let value = value.trim();
            let value = value
                .strip_prefix('"')
                .and_then(|v| v.strip_suffix('"'))
                .or_else(|| value.strip_prefix('\'').and_then(|v| v.strip_suffix('\'')))
                .unwrap_or(value);
            map.insert(key.trim().to_string(), value.to_string());
        }
    }
    // 没有收尾的 `---`：当成没有 frontmatter
    (BTreeMap::new(), text)
}

/// `{"mcpServers": {...}}` → 本地命令或远程（HTTP / SSE）的服务；既没命令也没地址的记成问题跳过。
fn parse_mcp(value: &Value, root: &Path) -> (Vec<PluginMcpServer>, Vec<String>) {
    let mut servers = Vec::new();
    let mut problems = Vec::new();
    let Some(map) = value.get("mcpServers").and_then(Value::as_object) else {
        problems.push("MCP 配置里缺少 \"mcpServers\" 对象".into());
        return (servers, problems);
    };
    for (name, s) in map {
        if s.get("disabled").and_then(Value::as_bool) == Some(true) {
            continue;
        }
        let command = s.get("command").and_then(Value::as_str).unwrap_or_default();
        let url = s.get("url").and_then(Value::as_str).unwrap_or_default();
        if url.is_empty() && command.trim().is_empty() {
            problems.push(format!("MCP 服务 {name}：缺少 \"command\" 或 \"url\"，已跳过"));
            continue;
        }
        let strings = |key: &str| -> Vec<String> {
            s.get(key)
                .and_then(Value::as_array)
                .map(|a| a.iter().filter_map(Value::as_str).map(|v| substitute(v, root)).collect())
                .unwrap_or_default()
        };
        let map = |key: &str| -> BTreeMap<String, String> {
            s.get(key)
                .and_then(Value::as_object)
                .map(|m| {
                    m.iter()
                        .filter_map(|(k, v)| v.as_str().map(|v| (k.clone(), substitute(v, root))))
                        .collect()
                })
                .unwrap_or_default()
        };
        servers.push(PluginMcpServer {
            name: name.clone(),
            command: substitute(command, root),
            args: strings("args"),
            env: map("env"),
            url: substitute(url, root),
            sse: s.get("type").and_then(Value::as_str) == Some("sse"),
            headers: map("headers"),
        });
    }
    (servers, problems)
}

/// `{"hooks": {"PreToolUse": [{"matcher": "...", "hooks": [{"type": "command", "command": "..."}]}]}}`
fn parse_hooks(value: &Value, plugin: &str, root: &Path) -> (Vec<Hook>, Vec<String>) {
    let mut hooks = Vec::new();
    let mut problems = Vec::new();
    let Some(map) = value.get("hooks").and_then(Value::as_object) else {
        problems.push("钩子配置里缺少 \"hooks\" 对象".into());
        return (hooks, problems);
    };
    for (key, groups) in map {
        let Some(event) = HookEvent::ALL.into_iter().find(|e| e.key() == key) else {
            problems.push(format!("不支持的钩子时机 {key}，已跳过"));
            continue;
        };
        for group in groups.as_array().into_iter().flatten() {
            let matcher = group.get("matcher").and_then(Value::as_str).unwrap_or_default();
            for h in group.get("hooks").and_then(Value::as_array).into_iter().flatten() {
                let kind = h.get("type").and_then(Value::as_str).unwrap_or("command");
                let command = h.get("command").and_then(Value::as_str).unwrap_or_default();
                if kind != "command" || command.trim().is_empty() {
                    problems.push(format!("{key} 钩子：只支持 type 为 command 的命令，已跳过"));
                    continue;
                }
                let timeout = h
                    .get("timeout")
                    .and_then(Value::as_u64)
                    .map_or(HOOK_TIMEOUT, Duration::from_secs);
                hooks.push(Hook {
                    event,
                    matcher: matcher.to_string(),
                    command: substitute(command, root),
                    timeout,
                    plugin: plugin.to_string(),
                    root: root.to_path_buf(),
                });
            }
        }
    }
    (hooks, problems)
}

/// 在启用的插件里找 `/名字 参数` 对应的命令；不是插件命令返回 None。
/// 也认带插件名的写法 `/插件:名字`（重名时用）。
pub fn find_command<'a>(
    plugins: &'a [Plugin],
    text: &str,
) -> Option<(&'a Plugin, &'a PluginCommand, String)> {
    let rest = text.strip_prefix('/')?;
    let (head, args) = match rest.find(char::is_whitespace) {
        Some(i) => (&rest[..i], rest[i..].trim()),
        None => (rest, ""),
    };
    plugins.iter().find_map(|p| {
        p.commands
            .iter()
            .find(|c| c.name == head || format!("{}:{}", p.name, c.name) == head)
            .map(|c| (p, c, args.to_string()))
    })
}

/// 展开命令模板：`$ARGUMENTS` 换成全部参数，`$1` `$2` … 换成按空白分开的第几个参数。
/// 模板里没有占位符而用户给了参数时，参数接在模板后面。
pub fn expand_command(body: &str, args: &str) -> String {
    let positional: Vec<&str> = args.split_whitespace().collect();
    let has_placeholder =
        body.contains("$ARGUMENTS") || (1..=9).any(|i| body.contains(&format!("${i}")));
    let mut out = body.replace("$ARGUMENTS", args);
    // 倒着换，免得 `$1` 先把 `$10` 的开头换掉
    for i in (1..=9).rev() {
        out = out.replace(&format!("${i}"), positional.get(i - 1).copied().unwrap_or(""));
    }
    if !has_placeholder && !args.is_empty() {
        out = format!("{}\n\n{args}", out.trim_end());
    }
    out
}

/// 内置工具名在 Claude Code 里的叫法（插件钩子的 matcher 按 Claude Code 的工具名写）。
fn claude_tool_name(tool: &str) -> Option<&'static str> {
    Some(match tool {
        "read_file" => "Read",
        "write_file" => "Write",
        "edit_file" => "Edit",
        "shell" => "Bash",
        "list_dir" => "LS",
        "glob" => "Glob",
        "grep" => "Grep",
        "web_fetch" => "WebFetch",
        "todo_write" => "TodoWrite",
        "task" => "Task",
        _ => return None,
    })
}

/// 钩子的 matcher 管不管这个工具：空或 `*` 全管；否则按正则整体匹配，
/// 内置工具名和它在 Claude Code 里的叫法（`shell` ↔ `Bash`）哪个对上都算。
pub fn hook_matches(matcher: &str, tool: &str) -> bool {
    let matcher = matcher.trim();
    if matcher.is_empty() || matcher == "*" {
        return true;
    }
    let names = std::iter::once(tool).chain(claude_tool_name(tool));
    match regex_lite(matcher) {
        Some(alternatives) => names.into_iter().any(|n| alternatives.iter().any(|a| a == n)),
        None => false,
    }
}

/// 子 agent 的工具清单里有没有这个工具（内置工具名和 Claude Code 的叫法都算）。
pub fn tool_listed(tools: &[String], tool: &str) -> bool {
    let alias = claude_tool_name(tool);
    tools.iter().any(|t| t == tool || Some(t.as_str()) == alias)
}

/// 只认 `A|B|C` 这种整词的多选一（Claude Code 插件里最常见的写法），带别的正则符号的不认。
fn regex_lite(matcher: &str) -> Option<Vec<String>> {
    let plain = |s: &str| s.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-');
    let parts: Vec<String> = matcher.split('|').map(|s| s.trim().to_string()).collect();
    parts.iter().all(|p| plain(p) && !p.is_empty()).then_some(parts)
}

/// 钩子跑完的结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HookOutcome {
    /// 退出码 0；stdout（可能是空的）
    Ok(String),
    /// 退出码 2：拦下；stderr 是原因
    Block(String),
    /// 其它退出码、超时、起不来；不影响流程，只提示一下
    Error(String),
}

/// 跑一个钩子：`input` 作为 JSON 写进 stdin，`cwd` 是会话的工作目录。
/// 环境变量带上 `CLAUDE_PLUGIN_ROOT` 和 `CLAUDE_PROJECT_DIR`（插件脚本常用）。
pub fn run_hook(hook: &Hook, input: &Value, cwd: &Path) -> HookOutcome {
    let mut command = shell_command(&hook.command);
    command
        .current_dir(cwd)
        .env("CLAUDE_PLUGIN_ROOT", &hook.root)
        .env("CLAUDE_PROJECT_DIR", cwd)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        // CREATE_NO_WINDOW：不弹黑框
        command.creation_flags(0x0800_0000);
    }
    let mut child = match command.spawn() {
        Ok(child) => child,
        Err(e) => return HookOutcome::Error(format!("起不来：{e}")),
    };
    // stdin 写完就关；输出在别的线程读，免得管道写满卡住
    if let Some(mut stdin) = child.stdin.take() {
        let payload = input.to_string();
        std::thread::spawn(move || {
            let _ = stdin.write_all(payload.as_bytes());
        });
    }
    let read = |pipe: Option<Box<dyn Read + Send>>| {
        std::thread::spawn(move || {
            let mut buf = Vec::new();
            if let Some(mut pipe) = pipe {
                let _ = pipe.read_to_end(&mut buf);
            }
            clip(String::from_utf8_lossy(&buf).trim())
        })
    };
    let stdout = read(child.stdout.take().map(|p| Box::new(p) as Box<dyn Read + Send>));
    let stderr = read(child.stderr.take().map(|p| Box::new(p) as Box<dyn Read + Send>));
    let started = Instant::now();
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if started.elapsed() < hook.timeout => {
                std::thread::sleep(Duration::from_millis(20))
            }
            Ok(None) => {
                let _ = child.kill();
                let _ = child.wait();
                return HookOutcome::Error(format!(
                    "超过 {} 秒没结束，已停掉",
                    hook.timeout.as_secs()
                ));
            }
            Err(e) => return HookOutcome::Error(e.to_string()),
        }
    };
    let stdout = stdout.join().unwrap_or_default();
    let stderr = stderr.join().unwrap_or_default();
    match status.code() {
        Some(0) => HookOutcome::Ok(stdout),
        Some(2) => HookOutcome::Block(if stderr.is_empty() { stdout } else { stderr }),
        code => {
            let detail = if stderr.is_empty() { stdout } else { stderr };
            let code = code.map_or("被终止".to_string(), |c| format!("退出码 {c}"));
            HookOutcome::Error(if detail.is_empty() { code } else { format!("{code}：{detail}") })
        }
    }
}

fn clip(text: &str) -> String {
    match text.char_indices().nth(HOOK_OUTPUT_MAX) {
        Some((cut, _)) => format!("{}…", &text[..cut]),
        None => text.to_string(),
    }
}

/// 跑钩子命令用的 shell：插件多是给 Claude Code 写的（bash 语法），Windows 上有 Git Bash 就用它，
/// 没有再退回 cmd；其它平台用 sh。
fn shell_command(line: &str) -> Command {
    #[cfg(windows)]
    {
        if let Some(bash) = git_bash() {
            let mut c = Command::new(bash);
            c.arg("-c").arg(line);
            return c;
        }
        let mut c = Command::new("cmd");
        c.arg("/C").arg(line);
        c
    }
    #[cfg(not(windows))]
    {
        let mut c = Command::new("sh");
        c.arg("-c").arg(line);
        c
    }
}

/// Git for Windows 带的 bash（`CLAUDE_CODE_GIT_BASH_PATH` 优先，同 Claude Code）。
/// 不用 PATH 上的 `bash`：System32 里那个是 WSL 的，路径和环境都对不上。
#[cfg(windows)]
fn git_bash() -> Option<PathBuf> {
    if let Some(p) = std::env::var_os("CLAUDE_CODE_GIT_BASH_PATH").map(PathBuf::from)
        && p.is_file()
    {
        return Some(p);
    }
    // PATH 上的 git.exe 在 `<Git>\cmd\` 或 `<Git>\mingw64\bin\`，往上找 `<Git>\bin\bash.exe`
    // （装在别的盘、别的目录也找得到）；再试默认安装位置
    let from_path = std::env::var_os("PATH")
        .map(|p| std::env::split_paths(&p).collect::<Vec<_>>())
        .unwrap_or_default()
        .into_iter()
        .filter(|dir| dir.join("git.exe").is_file())
        .flat_map(|dir| {
            dir.ancestors()
                .skip(1)
                .take(2)
                .map(|r| r.join("bin").join("bash.exe"))
                .collect::<Vec<_>>()
        });
    let defaults = ["ProgramFiles", "ProgramW6432", "LocalAppData"]
        .iter()
        .filter_map(|v| std::env::var_os(v).map(PathBuf::from))
        .flat_map(|r| {
            [
                r.join("Git").join("bin").join("bash.exe"),
                r.join("Programs").join("Git").join("bin").join("bash.exe"),
            ]
        });
    from_path.chain(defaults).find(|p| p.is_file())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("cd-plugin-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn write(path: &Path, text: &str) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    }

    #[test]
    fn reads_a_claude_code_style_plugin() {
        let dir = temp("full");
        let root = dir.join("review-kit");
        write(
            &root.join(".claude-plugin/plugin.json"),
            r#"{"name": "review", "version": "1.2.0", "description": "代码审查"}"#,
        );
        write(
            &root.join("commands/review.md"),
            "---\ndescription: \"审查改动\"\nargument-hint: <文件>\n---\n\n请审查 $ARGUMENTS\n",
        );
        write(&root.join("commands/git/log.md"), "看提交记录");
        write(
            &root.join("skills/pdf/SKILL.md"),
            "---\nname: pdf-tools\ndescription: 处理 PDF\n---\n全文",
        );
        write(
            &root.join("agents/reviewer.md"),
            "---\nname: code-reviewer\ndescription: 审查代码\ntools: Read, Grep, glob\nmodel: sonnet\n---\n你是审查员。\n",
        );
        write(&root.join("agents/scout.md"), "---\nmodel: fast-model\n---\n侦察");
        write(
            &root.join(".mcp.json"),
            r#"{"mcpServers": {"db": {"command": "${CLAUDE_PLUGIN_ROOT}/bin/db", "args": ["--root", "${CLAUDE_PLUGIN_ROOT}"]},
                "remote": {"type": "sse", "url": "https://x/sse", "headers": {"X-Root": "${CLAUDE_PLUGIN_ROOT}"}},
                "bad": {"args": []}}}"#,
        );
        write(
            &root.join("hooks/hooks.json"),
            r#"{"hooks": {"PreToolUse": [{"matcher": "Write|Edit", "hooks": [{"type": "command", "command": "check.sh", "timeout": 5}]}],
                "Bogus": []}}"#,
        );

        let plugins = discover(&dir);
        assert_eq!(plugins.len(), 1);
        let p = &plugins[0];
        assert_eq!((p.name.as_str(), p.version.as_deref()), ("review", Some("1.2.0")));
        let names: Vec<_> = p.commands.iter().map(|c| c.name.as_str()).collect();
        assert_eq!(names, ["git:log", "review"]);
        let review = &p.commands[1];
        assert_eq!(review.description, "审查改动");
        assert_eq!(review.argument_hint.as_deref(), Some("<文件>"));
        assert_eq!(review.body, "请审查 $ARGUMENTS");
        assert_eq!(p.skills[0].name, "pdf-tools");
        let reviewer = &p.agents[0];
        assert_eq!(reviewer.name, "code-reviewer");
        let tools = reviewer.tools.clone().unwrap();
        assert_eq!(tools, ["Read", "Grep", "glob"]);
        // Claude Code 的模型别名按跟主会话；正文是提示词
        assert_eq!((reviewer.model.as_deref(), reviewer.prompt.as_str()), (None, "你是审查员。"));
        assert!(tool_listed(&tools, "read_file"));
        assert!(tool_listed(&tools, "glob"));
        assert!(!tool_listed(&tools, "shell"));
        // 没写名字取文件名；没写工具 = 全部
        let scout = &p.agents[1];
        assert_eq!((scout.name.as_str(), scout.tools.is_none()), ("scout", true));
        assert_eq!((scout.model.as_deref(), scout.plugin.as_str()), (Some("fast-model"), "review"));
        assert_eq!(p.mcp_servers.len(), 2);
        let root_text = root.to_string_lossy().replace('\\', "/");
        assert_eq!(p.mcp_servers[0].command, format!("{root_text}/bin/db"));
        assert_eq!(p.mcp_servers[0].args, ["--root", root_text.as_str()]);
        // 远程服务：地址、旧版 SSE、请求头里的占位符也替换
        let remote = &p.mcp_servers[1];
        assert_eq!((remote.url.as_str(), remote.sse), ("https://x/sse", true));
        assert_eq!(remote.headers.get("X-Root"), Some(&root_text));
        assert_eq!(p.hooks.len(), 1);
        assert_eq!(p.hooks[0].event, HookEvent::PreToolUse);
        assert_eq!(p.hooks[0].timeout, Duration::from_secs(5));
        // 缺命令的 MCP、不认识的时机记成问题，其余照常
        assert_eq!(p.problems.len(), 2, "{:?}", p.problems);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn folder_without_manifest_is_still_a_plugin_and_bad_json_is_reported() {
        let dir = temp("bare");
        write(&dir.join("notes/commands/todo.md"), "列出待办");
        write(&dir.join("broken/.claude-plugin/plugin.json"), "{ not json");
        let plugins = discover(&dir);
        let names: Vec<_> = plugins.iter().map(|p| p.name.as_str()).collect();
        assert_eq!(names, ["broken", "notes"]);
        assert!(plugins[0].problems[0].contains("plugin.json"), "{:?}", plugins[0].problems);
        assert_eq!(plugins[1].commands[0].body, "列出待办");
        assert!(discover(&dir.join("missing")).is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn commands_expand_arguments() {
        assert_eq!(expand_command("审查 $ARGUMENTS", "a.rs b.rs"), "审查 a.rs b.rs");
        assert_eq!(expand_command("从 $1 改到 $2", "v1 v2"), "从 v1 改到 v2");
        // 没有占位符：参数接在后面
        assert_eq!(expand_command("总结改动", "只看 core"), "总结改动\n\n只看 core");
        assert_eq!(expand_command("总结改动", ""), "总结改动");

        let plugin = Plugin {
            name: "kit".into(),
            commands: vec![PluginCommand {
                name: "review".into(),
                description: String::new(),
                argument_hint: None,
                body: "x".into(),
            }],
            ..Plugin::default()
        };
        let plugins = [plugin];
        let (_, cmd, args) = find_command(&plugins, "/review  src/main.rs ").unwrap();
        assert_eq!((cmd.name.as_str(), args.as_str()), ("review", "src/main.rs"));
        assert!(find_command(&plugins, "/kit:review").is_some());
        assert!(find_command(&plugins, "/reviewer").is_none());
        assert!(find_command(&plugins, "review").is_none());
    }

    #[test]
    fn matchers_accept_claude_code_tool_names() {
        assert!(hook_matches("", "shell"));
        assert!(hook_matches("*", "anything"));
        assert!(hook_matches("Write|Edit", "edit_file"));
        assert!(hook_matches("Bash", "shell"));
        assert!(hook_matches("mcp__db__query", "mcp__db__query"));
        assert!(!hook_matches("Write|Edit", "shell"));
        // 复杂正则不认，宁可不跑也别跑错
        assert!(!hook_matches("mcp__.*", "mcp__db__query"));
    }

    #[test]
    fn hooks_report_exit_codes() {
        let dir = temp("run");
        let hook = |command: &str| Hook {
            event: HookEvent::PreToolUse,
            matcher: String::new(),
            command: command.into(),
            timeout: Duration::from_secs(10),
            plugin: "t".into(),
            root: dir.clone(),
        };
        let input = serde_json::json!({"tool_name": "shell"});
        assert_eq!(run_hook(&hook("echo ok"), &input, &dir), HookOutcome::Ok("ok".into()));
        match run_hook(&hook("echo nope 1>&2 && exit 2"), &input, &dir) {
            HookOutcome::Block(reason) => assert_eq!(reason, "nope"),
            other => panic!("{other:?}"),
        }
        assert!(matches!(run_hook(&hook("exit 3"), &input, &dir), HookOutcome::Error(_)));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
