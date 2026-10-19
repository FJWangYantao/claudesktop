//! # claudesktop-builtin
//!
//! 自研内核（进程内）：直接调 LLM provider，产出与 ACP 内核同一套
//! [`KernelEvent`]，UI 无感切换。
//!
//! - OpenAI 兼容 provider：流式文本 / 思考 / 用量 / 工具调用
//! - 工具循环：模型请求工具 → 按模式与规则判断权限（必要时发 `PermissionRequest`
//!   等 UI 应答）→ 执行 → 结果回给模型，直到模型不再调用工具
//! - 模式：逐项确认 / 自动接受编辑 / 计划模式（只读）/ 全部允许；「总是允许」规则按项目落盘
//! - 续接：用本地会话日志重建上下文（见 `history` 模块）
//! - 项目说明：AGENTS.md / CLAUDE.md 读进系统提示词（见 `instructions` 模块）
//! - 上下文压缩：快满时 / 超限报错后把较早的对话总结成摘要（见 `compact` 模块）
//! - MCP：数据目录 `mcp.json` 里配的工具服务器，工具和内置工具一起交给模型（见 `mcp` 模块）
//! - 插件（`claudesktop_core::plugin`）：MCP 服务并进上面那批；技能列进系统提示词；
//!   钩子在会话开始、提交消息、执行工具前后、一轮结束时跑（退出码 2 = 拦下）
//! - 子 agent：`task` 工具开子会话跑子任务，角色内置一个通用的、其余来自插件（见 `subagent` 模块）
//!
//! 线程模型：每轮对话一个线程（阻塞读 SSE 流、执行工具、等权限应答）；
//! 取消 = 置该轮的标志位，各阻塞点轮询检查，取消后不再发出任何事件。

mod anthropic;
mod bgtasks;
mod compact;
mod fetch;
pub mod gateway;
mod history;
mod instructions;
pub mod mcp;
mod mcp_http;
mod mcp_oauth;
mod openai;
mod permissions;
mod provider;
pub mod request_bodies;
pub mod responses;
mod sse;
mod subagent;
mod tools;
pub mod websearch;

pub use anthropic::AnthropicProvider;
pub use openai::OpenAiProvider;
pub use permissions::{DENY_PREFIX, PermissionRules, default_rules_path};
pub use responses::ResponsesProvider;

pub use provider::{
    ChatMessage, ChatProvider, ContentPart, FunctionCall, ModelInfo, Protocol, ProviderConfig,
    RequestParams, StreamEvent, StreamOutcome, ToolCall, Usage,
};

/// 列出端点提供的模型（阻塞，设置页在后台线程调用）。
pub fn fetch_models(config: ProviderConfig) -> anyhow::Result<Vec<ModelInfo>> {
    make_provider(&config).list_models()
}

/// 按配置里的协议构造 provider。
fn make_provider(config: &ProviderConfig) -> std::sync::Arc<dyn ChatProvider> {
    match config.protocol {
        Protocol::OpenAiCompat => std::sync::Arc::new(OpenAiProvider::new(config.clone())),
        Protocol::Anthropic => std::sync::Arc::new(AnthropicProvider::new(config.clone())),
        Protocol::Responses => std::sync::Arc::new(ResponsesProvider::new(config.clone())),
    }
}

use claudesktop_core::plugin::{self, AgentDef, HookEvent, HookOutcome, Plugin, Skill};
use claudesktop_core::{
    EventSink, Kernel, KernelEvent, ModeView, PermissionOptionView, Record, RoutedEvent, checkpoint,
};
use serde_json::{Value, json};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::time::Duration;
use tools::{Access, Action, ToolOutput};

/// 一轮里最多请求模型的次数的默认值（防止工具循环停不下来）。
/// 可经 [`BuiltinConfig::max_steps`] 配置，设置页「内核与模型」里改。
pub const DEFAULT_MAX_STEPS: usize = 200;
/// 可配置步数的下限：再小的话稍长一点的任务都会被拦腰截断。
pub const MIN_MAX_STEPS: usize = 1;
/// 上限：防止手滑填个天文数字，工具循环真的停不下来。
pub const MAX_MAX_STEPS: usize = 5_000;
/// 用量达到窗口的这个比例时，下一次请求前自动压缩上下文（见 `compact` 模块）。
/// 界面上的上下文用量卡据此算「还剩多少到自动压缩」，不要再写一份数字。
pub const AUTO_COMPACT_PERCENT: u64 = compact::AUTO_COMPACT_PERCENT;
/// 同一步里最多同时执行几个只读工具
const PARALLEL_TOOLS: usize = 8;

const MODE_DEFAULT: &str = "default";
const MODE_ACCEPT_EDITS: &str = "acceptEdits";
const MODE_PLAN: &str = "plan";
const MODE_BYPASS: &str = "bypassPermissions";

#[derive(Debug, Clone)]
pub struct BuiltinConfig {
    pub provider: ProviderConfig,
    /// 默认工作目录（写进系统提示词，工具的相对路径以此为准）
    pub cwd: PathBuf,
    /// 「总是允许」规则文件；None = 只在内存里
    pub rules_path: Option<PathBuf>,
    /// 用户级说明文件（默认 `~/.claude/CLAUDE.md`），和项目里的 AGENTS.md / CLAUDE.md
    /// 一起进系统提示词；None = 不读
    pub user_instructions: Option<PathBuf>,
    /// 端点报告的上下文窗口（模型 ID → token）：用量快到窗口时自动压缩；
    /// 查不到窗口的模型只在端点报「超出上下文」后压缩
    pub context_windows: HashMap<String, u64>,
    /// MCP 服务器配置文件（默认数据目录的 `mcp.json`）；None = 不接 MCP
    pub mcp_config: Option<PathBuf>,
    /// 启用的插件（之后经 `Kernel::set_plugins` 更新）
    pub plugins: Vec<Plugin>,
    /// 一轮里最多请求模型的次数（工具循环的上限，默认 [`DEFAULT_MAX_STEPS`]）；
    /// 超界的值读取时夹回 [MIN_MAX_STEPS, MAX_MAX_STEPS]
    pub max_steps: usize,
    /// 第一轮结束后让模型给会话起标题（多一次小请求）；测试里关掉，免得打乱剧本
    pub session_titles: bool,
    /// 全局记忆文件（默认数据目录的 `memory.json`，见 `claudesktop_core::memory`）
    pub memory_path: Option<PathBuf>,
}

impl BuiltinConfig {
    /// 规则文件放在默认数据目录。
    pub fn new(provider: ProviderConfig, cwd: PathBuf) -> Self {
        Self {
            provider,
            cwd,
            rules_path: default_rules_path(),
            user_instructions: instructions::default_user_file(),
            context_windows: HashMap::new(),
            mcp_config: default_mcp_config(),
            plugins: Vec::new(),
            max_steps: DEFAULT_MAX_STEPS,
            session_titles: true,
            memory_path: default_memory_path(),
        }
    }

    /// 从环境变量读取：
    /// - `CLAUDESKTOP_BASE_URL`（必填）：端点，如 `http://127.0.0.1:8045/v1`
    /// - `CLAUDESKTOP_MODEL`（必填）：模型 ID
    /// - `CLAUDESKTOP_API_KEY`（可选，其次 `OPENAI_API_KEY`）
    /// - `CLAUDEDESKTOP_PROTOCOL`（可选，`anthropic` = Anthropic 原生协议）
    pub fn from_env() -> anyhow::Result<Self> {
        let var = |k: &str| std::env::var(k).ok().filter(|v| !v.trim().is_empty());
        let base_url = var("CLAUDESKTOP_BASE_URL")
            .ok_or_else(|| anyhow::anyhow!("未设置 CLAUDESKTOP_BASE_URL（OpenAI 兼容端点）"))?;
        let model =
            var("CLAUDESKTOP_MODEL").ok_or_else(|| anyhow::anyhow!("未设置 CLAUDESKTOP_MODEL"))?;
        let protocol = match var("CLAUDEDESKTOP_PROTOCOL").as_deref() {
            Some("anthropic") => Protocol::Anthropic,
            _ => Protocol::OpenAiCompat,
        };
        Ok(Self::new(
            ProviderConfig {
                protocol,
                base_url,
                api_key: var("CLAUDESKTOP_API_KEY").or_else(|| var("OPENAI_API_KEY")),
                model,
                params: RequestParams::default(),
                gateway: None,
            },
            std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")),
        ))
    }

    /// 写进会话元信息的内核标识（见 [`ProviderConfig::identity`]）。
    pub fn identity(&self) -> String {
        self.provider.identity()
    }

    /// 夹回允许范围的步数上限（设置文件被手改时也守住）。
    pub fn clamped_max_steps(&self) -> usize {
        self.max_steps.clamp(MIN_MAX_STEPS, MAX_MAX_STEPS)
    }
}

fn modes() -> Vec<ModeView> {
    let m = |id: &str, name: &str, desc: &str| ModeView {
        id: id.into(),
        name: name.into(),
        description: Some(desc.into()),
    };
    vec![
        m(MODE_DEFAULT, "逐项确认", "写文件和执行命令前先询问"),
        m(MODE_ACCEPT_EDITS, "自动接受编辑", "自动允许项目内的文件编辑，命令仍询问"),
        m(MODE_PLAN, "计划模式", "只读：不写文件、不执行命令"),
        m(MODE_BYPASS, "全部允许", "全部自动允许（谨慎）"),
    ]
}

struct SessionState {
    cwd: PathBuf,
    /// 本会话使用的模型
    model: String,
    /// 不含系统提示词的对话历史
    history: Vec<ChatMessage>,
    mode: String,
    /// 历史是从本地日志重建的（工具调用只剩摘要）
    restored: bool,
    /// 用户级和项目里的说明（每轮开始时重读）
    instructions: Vec<instructions::Instruction>,
    /// 启用的插件提供的技能（每轮开始时重取）
    skills: Vec<Skill>,
    /// 「会话开始」钩子跑过了（第一轮开始时跑一次）
    started: bool,
    /// 较早对话的摘要（压缩过才有，进系统提示词）
    summary: Option<String>,
    /// 端点最近一次报告的上下文大小：(token, 当时覆盖到的历史条数)；压缩后清空
    measured: Option<(u64, usize)>,
    /// 进行中的一轮的取消标志（子会话用主会话这一轮的）
    turn: Option<Arc<AtomicBool>>,
    /// 第几轮（每发一条消息、每次手动压缩加一；子会话沿用主会话的）：用量网关的日志按它把请求归到轮
    turns: u64,
    /// 子会话的角色（`task` 开的，ID 是 `主会话/调用 ID`）；主会话为 None
    agent: Option<AgentDef>,
    /// 项目级配置停用的 MCP 服务器名（`.claudesktop/settings.json` 的 `mcpDisabled`）
    mcp_disabled: Vec<String>,
    /// 项目级配置指定的插件范围（`pluginsEnabled`）；None = 用全部启用的插件
    plugins_enabled: Option<Vec<String>>,
    /// 记下的项目记忆（每轮重读，见 `memory` 模块）
    memory_project: Vec<claudesktop_core::memory::Entry>,
    /// 记下的全局记忆（每轮重读）
    memory_global: Vec<claudesktop_core::memory::Entry>,
}

impl SessionState {
    /// 重读两份记忆（系统提示词每轮跟着变；文件没改时内容一样，不影响提示缓存）。
    /// 返回值：读不了 / 格式坏了的原因（会话里各提示一行，项目、全局分开报）。
    fn reload_memory(&mut self, global: Option<&Path>) -> Vec<String> {
        use claudesktop_core::memory;
        let mut warnings = Vec::new();
        let (project, project_warning) = memory::load(&memory::project_file_or(&self.cwd));
        self.memory_project = project;
        warnings.extend(project_warning);
        let (global_entries, global_warning) = global.map(memory::load).unwrap_or_default();
        self.memory_global = global_entries;
        warnings.extend(global_warning);
        warnings
    }

    fn system_prompt(&self) -> String {
        let ctx = Context {
            instructions: &self.instructions,
            skills: &self.skills,
            memory_project: &self.memory_project,
            memory_global: &self.memory_global,
        };
        system_prompt(
            &self.cwd,
            &self.mode,
            self.restored,
            self.summary.as_deref(),
            self.agent.as_ref(),
            ctx,
        )
    }

    /// 系统提示词 + 历史的粗估 token（端点还没报告过用量时用）。
    fn estimate_tokens(&self) -> u64 {
        self.system_prompt().len() as u64 / 4 + compact::estimate_tokens(&self.history)
    }
}

struct Inner {
    provider: Arc<dyn ChatProvider>,
    /// 新会话的默认模型（最近一次 `set_model` 选的，初始为配置里的）
    default_model: Mutex<String>,
    sink: EventSink,
    agent_name: String,
    default_cwd: PathBuf,
    user_instructions: Option<PathBuf>,
    /// 一轮里最多请求模型的次数（见 [`BuiltinConfig::max_steps`]）
    max_steps: usize,
    /// 是否让模型给会话起标题（见 [`BuiltinConfig::session_titles`]）
    session_titles: bool,
    /// 见 [`BuiltinConfig::context_windows`]；界面取到模型列表后经 `set_context_windows` 更新
    context_windows: Mutex<HashMap<String, u64>>,
    sessions: Mutex<HashMap<String, SessionState>>,
    next_session: AtomicU64,
    rules: Mutex<PermissionRules>,
    /// MCP 服务器（所有会话共用）
    mcp: Arc<mcp::McpHub>,
    /// 启用的插件（技能、钩子；它们的 MCP 服务在 `mcp` 里）
    plugins: Mutex<Arc<Vec<Plugin>>>,
    /// 等待 UI 应答的权限请求：请求 ID → 应答通道
    pending: Mutex<HashMap<u64, mpsc::Sender<Option<String>>>>,
    next_request: AtomicU64,
    /// 这个内核的编号（后台命令表按它分组：同一进程里几个内核各收自己的）
    kernel_id: u64,
    /// 全局记忆文件（见 [`BuiltinConfig::memory_path`]）
    memory_path: Option<PathBuf>,
}

/// 默认 MCP 配置文件：数据目录下的 `mcp.json`。
pub fn default_mcp_config() -> Option<PathBuf> {
    claudesktop_core::default_data_dir().ok().map(|d| d.join("mcp.json"))
}

/// 默认的全局记忆文件：数据目录下的 `memory.json`。
pub fn default_memory_path() -> Option<PathBuf> {
    claudesktop_core::memory::global_path()
}

/// 一步要发给模型的东西（[`Inner::snapshot`]）。
struct Snapshot {
    messages: Vec<ChatMessage>,
    mode: String,
    cwd: PathBuf,
    model: String,
    /// 子会话的角色
    agent: Option<AgentDef>,
}

/// 工具循环（[`run_steps`]）怎么结束的。
enum Steps {
    /// 模型不再调用工具（或到了步数上限）
    Done {
        stop: String,
        usage: Option<Usage>,
        /// 最后一次请求时的上下文大小
        context_tokens: u64,
        /// 最后一段非空的回复
        reply: String,
    },
    /// 请求失败（超限时已经压缩重试过），已报给界面的只有压缩的结果
    Failed(String),
    /// 被取消，或会话没了
    Stopped,
}

/// 权限判定结果
enum Verdict {
    Allow,
    /// 拒绝；内容回给模型
    Deny(String),
}

impl Inner {
    fn emit(&self, session: Option<&str>, event: KernelEvent) {
        // 子会话的事件转到主会话（转哪些见 subagent::forward）
        if let Some((parent, call)) = session.and_then(subagent::split_id) {
            if let Some(event) = subagent::forward(call, event) {
                self.emit(Some(parent), event);
            }
            return;
        }
        (self.sink)(RoutedEvent { session: session.map(str::to_owned), event });
    }

    /// 启用的插件提供的全部技能。`only` 有值时只算这些插件（项目级 `pluginsEnabled`）。
    fn skills(&self, only: Option<&[String]>) -> Vec<Skill> {
        self.plugins
            .lock()
            .unwrap()
            .iter()
            .filter(|p| only.is_none_or(|names| names.iter().any(|n| n == &p.name)))
            .flat_map(|p| p.skills.iter().cloned())
            .collect()
    }

    /// 可用的子 agent 角色：插件提供的，加上内置的通用角色（插件里没有同名的时候）。
    /// `only` 同 [`Inner::skills`]。
    fn agents(&self, only: Option<&[String]>) -> Vec<AgentDef> {
        let mut agents: Vec<AgentDef> = self
            .plugins
            .lock()
            .unwrap()
            .iter()
            .filter(|p| only.is_none_or(|names| names.iter().any(|n| n == &p.name)))
            .flat_map(|p| p.agents.iter().cloned())
            .collect();
        if !agents.iter().any(|a| a.name == subagent::GENERAL) {
            agents.insert(0, subagent::general());
        }
        agents
    }

    /// 这个会话能不能用这个工具（子会话按角色的工具清单）。
    fn tool_allowed(&self, session: &str, tool: &str) -> bool {
        let sessions = self.sessions.lock().unwrap();
        sessions
            .get(session)
            .and_then(|s| s.agent.as_ref())
            .is_none_or(|agent| subagent::allows(agent, tool))
    }

    /// 这是会话的第一轮吗（给标题起名用）；子会话 / 不存在的会话返回 false。
    fn is_first_turn(&self, session: &str) -> bool {
        let sessions = self.sessions.lock().unwrap();
        sessions.get(session).is_some_and(|s| s.turns <= 1 && subagent::split_id(session).is_none())
    }

    /// 起标题要的素材：模型 ID、第一条用户消息、第一条助手回复。
    fn title_inputs(&self, session: &str) -> Option<(String, String, String)> {
        let sessions = self.sessions.lock().unwrap();
        let state = sessions.get(session)?;
        if state.history.is_empty() {
            return None;
        }
        let first_user = state
            .history
            .iter()
            .find(|m| m.role == "user")
            .and_then(|m| m.text())
            .unwrap_or_default();
        let first_reply = state
            .history
            .iter()
            .find(|m| m.role == "assistant" && m.text().is_some_and(|t| !t.trim().is_empty()))
            .and_then(|m| m.text())
            .unwrap_or_default();
        if first_user.trim().is_empty() {
            return None;
        }
        // 各截一段，控制这个请求的输入
        let clip = |s: &str| -> String {
            s.chars().take(2_000).collect::<String>()
                + if s.chars().count() > 2_000 { "…" } else { "" }
        };
        Some((state.model.clone(), clip(&first_user), clip(&first_reply)))
    }

    /// 依次跑某个时机的钩子（工具类时机按 matcher 只跑管这个工具的），返回
    /// (各钩子的输出, 拦下的原因)。出错的钩子在会话里提示一行、不影响流程；有一个拦下就不再跑后面的。
    fn run_hooks(
        &self,
        session: &str,
        cwd: &Path,
        event: HookEvent,
        tool: Option<&str>,
        mut input: Value,
    ) -> (Vec<String>, Option<String>) {
        let hooks: Vec<plugin::Hook> = self
            .plugins
            .lock()
            .unwrap()
            .iter()
            .flat_map(|p| p.hooks.iter())
            .filter(|h| {
                h.event == event && tool.is_none_or(|t| plugin::hook_matches(&h.matcher, t))
            })
            .cloned()
            .collect();
        let mut outputs = Vec::new();
        if hooks.is_empty() {
            return (outputs, None);
        }
        if let Some(m) = input.as_object_mut() {
            m.insert("session_id".into(), session.into());
            m.insert("hook_event_name".into(), event.key().into());
            m.insert("cwd".into(), cwd.display().to_string().into());
        }
        for hook in hooks {
            match plugin::run_hook(&hook, &input, cwd) {
                HookOutcome::Ok(out) if !out.is_empty() => outputs.push(out),
                HookOutcome::Ok(_) => {}
                HookOutcome::Block(reason) => {
                    let reason = if reason.is_empty() {
                        format!("插件「{}」的钩子没有说明原因", hook.plugin)
                    } else {
                        reason
                    };
                    return (outputs, Some(reason));
                }
                HookOutcome::Error(e) => self.emit(
                    Some(session),
                    KernelEvent::Notice(format!(
                        "插件「{}」的{}钩子出错：{e}",
                        hook.plugin,
                        event.label()
                    )),
                ),
            }
        }
        (outputs, None)
    }

    /// 一轮开始前的钩子：会话第一轮先跑「会话开始」，每轮跑「提交消息」。
    /// 它们的输出当补充上下文接在这条用户消息后面；被拦下时这一轮按失败结束，返回 false。
    fn prompt_hooks(&self, session: &str, flag: &AtomicBool) -> bool {
        let Some((cwd, first, prompt)) = ({
            let mut sessions = self.sessions.lock().unwrap();
            sessions.get_mut(session).map(|state| {
                let first = !std::mem::replace(&mut state.started, true);
                let prompt = state.history.last().map(message_text).unwrap_or_default();
                (state.cwd.clone(), first, prompt)
            })
        }) else {
            return false;
        };
        let mut context = Vec::new();
        if first {
            let (out, _) = self.run_hooks(
                session,
                &cwd,
                HookEvent::SessionStart,
                None,
                json!({"source": "startup"}),
            );
            context.extend(out);
        }
        let (out, blocked) = self.run_hooks(
            session,
            &cwd,
            HookEvent::UserPromptSubmit,
            None,
            json!({"prompt": prompt}),
        );
        context.extend(out);
        if flag.load(Ordering::SeqCst) {
            return false;
        }
        if let Some(reason) = blocked {
            let message = format!("插件钩子拦下了这条消息：{reason}");
            self.commit(
                session,
                flag,
                vec![ChatMessage::new("assistant", format!("[turn failed: {message}]"))],
            );
            self.finish(session, &Arc::new(AtomicBool::new(false)));
            self.emit(Some(session), KernelEvent::PromptFailed { message });
            return false;
        }
        if !context.is_empty()
            && let Some(state) = self.sessions.lock().unwrap().get_mut(session)
            && let Some(last) = state.history.last_mut()
        {
            let extra = format!(
                "\n\n<plugin-hook-context>\n{}\n</plugin-hook-context>",
                context.join("\n\n")
            );
            match last.parts.iter_mut().find_map(|p| match p {
                ContentPart::Text(t) => Some(t),
                _ => None,
            }) {
                Some(text) => text.push_str(&extra),
                None => last.parts.push(ContentPart::Text(extra)),
            }
        }
        true
    }

    /// 本步要发给模型的消息、当前模式、工作目录与模型；会话已不存在返回 None。
    fn snapshot(&self, session: &str) -> Option<Snapshot> {
        let mut sessions = self.sessions.lock().unwrap();
        // 子会话跟着主会话当前的模式（用户中途可能换了）
        if let Some((parent, _)) = subagent::split_id(session)
            && let Some(mode) = sessions.get(parent).map(|p| p.mode.clone())
            && let Some(state) = sessions.get_mut(session)
        {
            state.mode = mode;
        }
        let state = sessions.get(session)?;
        let mut messages = vec![ChatMessage::new("system", state.system_prompt())];
        messages.extend(state.history.iter().cloned());
        Some(Snapshot {
            messages,
            mode: state.mode.clone(),
            cwd: state.cwd.clone(),
            model: state.model.clone(),
            agent: state.agent.clone(),
        })
    }

    /// 模型的上下文窗口；对不上完整 ID 时按最后一段找（`provider/name` → `name`）。
    fn window(&self, model: &str) -> Option<u64> {
        let name = |id: &str| id.rsplit('/').next().unwrap_or(id).to_string();
        let windows = self.context_windows.lock().unwrap();
        windows
            .get(model)
            .copied()
            .or_else(|| windows.iter().find(|(id, _)| name(id) == name(model)).map(|(_, n)| *n))
            .filter(|w| *w > 0)
    }

    /// 下一次请求大约要带的 token：端点上次报告的 + 之后新加的消息（粗估）；没报告过就整个粗估。
    fn context_estimate(&self, session: &str) -> u64 {
        let sessions = self.sessions.lock().unwrap();
        let Some(state) = sessions.get(session) else {
            return 0;
        };
        match state.measured {
            Some((tokens, covered)) if covered <= state.history.len() => {
                tokens + compact::estimate_tokens(&state.history[covered..])
            }
            _ => state.estimate_tokens(),
        }
    }

    /// 记下端点报告的上下文大小：历史里除了最后 `unmeasured` 条，都算在 `tokens` 里。
    fn measure(&self, session: &str, tokens: u64, unmeasured: usize) {
        if tokens == 0 {
            return;
        }
        if let Some(state) = self.sessions.lock().unwrap().get_mut(session) {
            state.measured = Some((tokens, state.history.len().saturating_sub(unmeasured)));
        }
    }
}

/// 进总结请求的既有内容：之前压缩留下的摘要 + `PreCompact` 钩子的补充说明。
struct SummaryInput<'a> {
    previous: Option<&'a str>,
    hook_context: &'a [String],
}

impl Inner {
    /// 压缩上下文：挑切点、请模型总结、换掉历史（规则见 `compact` 模块）。
    /// `idle` = 手动压缩（没有进行中的一轮），全部进摘要；`keep_budget` 是保留尾部的 token 预算。
    /// 开始和结果都报给 UI（`Compacting` → `Compacted` / `CompactFailed`）。
    /// 没有可压缩的内容返回 `Ok(None)`，成功返回总结请求的用量，被取消或失败返回 `Err`。
    fn compact(
        &self,
        session: &str,
        flag: &AtomicBool,
        model: &str,
        idle: bool,
        keep_budget: u64,
        transcript_bytes: usize,
    ) -> anyhow::Result<Option<Usage>> {
        let stopped = || flag.load(Ordering::SeqCst);
        let (history, previous) = {
            let sessions = self.sessions.lock().unwrap();
            let state = sessions.get(session).ok_or_else(|| anyhow::anyhow!("会话已关闭"))?;
            (state.history.clone(), state.summary.clone())
        };
        let Some(split) = compact::split(&history, keep_budget, idle) else {
            return Ok(None);
        };
        // 压缩前跑 PreCompact 钩子：只是通知 + 输出并进总结请求（拦下没有意义，拦了上下文照涨）
        let cwd = self
            .sessions
            .lock()
            .unwrap()
            .get(session)
            .map(|s| s.cwd.clone())
            .unwrap_or_else(|| self.default_cwd.clone());
        let (hook_context, blocked) = self.run_hooks(
            session,
            &cwd,
            HookEvent::PreCompact,
            None,
            json!({ "trigger": if idle { "manual" } else { "auto" } }),
        );
        if let Some(reason) = blocked {
            self.emit(
                Some(session),
                KernelEvent::Notice(format!("插件钩子不支持拦下压缩（{reason}），已照常压缩")),
            );
        }
        self.emit(Some(session), KernelEvent::Compacting);
        let result = self.summarize(
            session,
            model,
            &history[..split.tail_from],
            SummaryInput { previous: previous.as_deref(), hook_context: &hook_context },
            transcript_bytes,
            &stopped,
        );
        if stopped() {
            anyhow::bail!("cancelled");
        }
        let (summary, usage) = match result {
            Ok(r) => r,
            Err(err) => {
                let message = format!("{err:#}");
                self.emit(Some(session), KernelEvent::CompactFailed { message });
                return Err(err);
            }
        };
        // 和 commit() 一样持锁判断取消；一轮之内历史只有本线程在改，切点仍然有效
        let context_tokens = {
            let mut sessions = self.sessions.lock().unwrap();
            let state = match sessions.get_mut(session) {
                Some(state) if !stopped() => state,
                _ => anyhow::bail!("cancelled"),
            };
            state.history = split.apply(&history);
            state.summary = Some(summary.clone());
            state.measured = None;
            state.estimate_tokens()
        };
        self.emit(Some(session), KernelEvent::Compacted { summary, context_tokens });
        Ok(Some(usage.unwrap_or_default()))
    }

    /// 请模型把一段对话写成摘要（不带工具）。记录太长、总结请求本身也超限时，记录减半再试，最多两次。
    /// `summary_input` 汇总了进总结请求的既有内容（旧摘要 + `PreCompact` 钩子的补充说明）。
    fn summarize(
        &self,
        session: &str,
        model: &str,
        head: &[ChatMessage],
        summary_input: SummaryInput<'_>,
        mut max_bytes: usize,
        stopped: &dyn Fn() -> bool,
    ) -> anyhow::Result<(String, Option<Usage>)> {
        let SummaryInput { previous, hook_context } = summary_input;
        let _tag = gateway::tag(session, "compact", self.turn_number(session));
        let mut attempt = 0;
        loop {
            let request = compact::summary_request(
                previous,
                &compact::transcript(head, max_bytes),
                hook_context,
            );
            let mut text = String::new();
            let result =
                self.provider.stream_chat(
                    model,
                    &request,
                    &[],
                    stopped,
                    &mut |event| match event {
                        StreamEvent::Text(t) => text.push_str(&t),
                        StreamEvent::Reasoning(_) => {}
                        StreamEvent::Retrying { status, attempt, delay_ms, reason } => self.emit(
                            Some(session),
                            KernelEvent::Retrying { status, attempt, delay_ms, reason },
                        ),
                    },
                );
            match result {
                Err(err)
                    if attempt < 2
                        && claudesktop_core::is_context_overflow(&format!("{err:#}")) =>
                {
                    attempt += 1;
                    max_bytes /= 2;
                }
                Err(err) => return Err(err),
                Ok(outcome) => {
                    let text = text.trim().to_string();
                    anyhow::ensure!(!text.is_empty(), "模型没有返回摘要");
                    return Ok((text, outcome.usage));
                }
            }
        }
    }

    /// 会话现在是第几轮（见 [`SessionState::turns`]）。
    fn turn_number(&self, session: &str) -> u64 {
        self.sessions.lock().unwrap().get(session).map_or(0, |s| s.turns)
    }

    fn model_of(&self, session: &str) -> Option<String> {
        self.sessions.lock().unwrap().get(session).map(|s| s.model.clone())
    }

    fn mode(&self, session: &str) -> String {
        // 子会话用主会话的模式
        let session = subagent::split_id(session).map_or(session, |(parent, _)| parent);
        self.sessions
            .lock()
            .unwrap()
            .get(session)
            .map_or_else(|| MODE_DEFAULT.into(), |s| s.mode.clone())
    }

    /// 把一步的消息写进历史。和 `cancel()` 共用锁判断标志位：
    /// 取消之后（新一轮可能已经追加了用户消息）不再写，返回 false。
    fn commit(&self, session: &str, flag: &AtomicBool, messages: Vec<ChatMessage>) -> bool {
        let mut sessions = self.sessions.lock().unwrap();
        match sessions.get_mut(session) {
            Some(state) if !flag.load(Ordering::SeqCst) => {
                state.history.extend(messages);
                true
            }
            _ => false,
        }
    }

    /// 本轮结束：清掉自己的标志（取消后可能已经开始了新的一轮）。
    fn finish(&self, session: &str, flag: &Arc<AtomicBool>) {
        if let Some(state) = self.sessions.lock().unwrap().get_mut(session)
            && state.turn.as_ref().is_some_and(|f| Arc::ptr_eq(f, flag))
        {
            state.turn = None;
        }
    }

    /// 执行一次工具调用（含权限判定），并把进度报给 UI。
    fn run_tool(
        &self,
        session: &str,
        cwd: &Path,
        call: &ToolCall,
        stopped: &dyn Fn() -> bool,
    ) -> ToolOutput {
        // 待办清单不碰文件、不用授权，也不占一行工具：直接换掉界面上的计划（子会话没有这个工具）
        if call.function.name == tools::TODO_WRITE && self.tool_allowed(session, tools::TODO_WRITE)
        {
            return match tools::parse_todos(&call.function.arguments) {
                Ok(entries) => {
                    let summary = tools::todo_summary(&entries);
                    self.emit(Some(session), KernelEvent::PlanReplaced { entries });
                    ToolOutput::ok(format!("Todo list updated: {summary}"), summary)
                }
                Err(message) => ToolOutput::fail(message, "参数无效"),
            };
        }
        // 记忆不出网、不用授权：直接写 memory.json（见 memory 模块）。但它是写文件，
        // Plan 模式（只读）不给（工具列表里也没有，这里挡住模型硬调的）
        if call.function.name == tools::MEMORY_WRITE
            && self.tool_allowed(session, tools::MEMORY_WRITE)
        {
            if self.mode(session) == MODE_PLAN {
                return ToolOutput::fail(
                    "Plan mode is active: memory_write is not available. Present a plan instead.",
                    "计划模式不能记",
                );
            }
            return self.remember(session, cwd, &call.function.arguments);
        }
        let (action, title) = self.start_tool(session, cwd, call);
        self.finish_tool(session, cwd, call, action, &title, stopped)
    }

    /// `memory_write`：写进全局或项目的 `memory.json`，报 `MemoryChanged`。
    /// 读写文件失败不让工具失败——把原因回给模型，免得它反复重试。
    fn remember(&self, session: &str, cwd: &Path, arguments: &str) -> ToolOutput {
        use claudesktop_core::memory;
        let (scope, entry) = match tools::parse_memory(arguments) {
            Ok(parsed) => parsed,
            Err(message) => return ToolOutput::fail(message, "参数无效"),
        };
        let (path, scope_name) = match scope {
            memory::Scope::Global => (self.memory_path.clone(), "global"),
            memory::Scope::Project => (Some(memory::project_file_or(cwd)), "project"),
        };
        let Some(path) = path else {
            return ToolOutput::fail(
                "Could not find the data directory, so the note was not saved.",
                "没能保存",
            );
        };
        let mut entry = entry;
        entry.source =
            Some(memory::Source { session: session.to_string(), turn: self.turn_number(session) });
        // 项目记忆在项目目录里（进 git）：和别的写文件工具一样，前后各读一次交给检查点，回退时还原
        let before =
            (scope == memory::Scope::Project).then(|| checkpoint::read_snapshot(&path)).flatten();
        // 读 → 改 → 写全程持锁（几个会话、设置页同时改不会互相覆盖）；读不了就不动文件，
        // 格式坏了挪到旁边再记（不覆盖之前挪开的）
        let result = memory::update(&path, memory::OnBroken::MoveAside, |entries| {
            let updated = memory::upsert(entries, entry.clone());
            memory::enforce_limits(entries, scope.max_entries());
            (updated, entries.len())
        });
        if let Some(before) = before
            && let Some(after) = checkpoint::read_snapshot(&path)
            && before != after
        {
            let file = path.to_string_lossy().into_owned();
            self.emit(Some(session), KernelEvent::FileChanged { path: file, before, after });
        }
        match result {
            Ok(((updated, count), notice)) => {
                if let Some(notice) = notice {
                    self.emit(Some(session), KernelEvent::Notice(notice));
                }
                self.emit(
                    Some(session),
                    KernelEvent::MemoryChanged { scope: scope_name.into(), count },
                );
                let verb = if updated { "Updated" } else { "Saved" };
                let scope = match scope_name {
                    "global" => "every project",
                    _ => "this project",
                };
                ToolOutput::ok(
                    format!("{verb} for {scope}: {}", entry.text),
                    format!("记下了：{}", entry.text),
                )
            }
            Err(text) => {
                self.emit(Some(session), KernelEvent::Notice(text.clone()));
                ToolOutput::fail(text, "没能保存")
            }
        }
    }

    /// 一步里的全部调用：连续的只读调用和子任务（`task`）一起并行跑，其余依次执行；结果按调用顺序返回。
    /// 中途被取消时返回 None。
    fn run_tools(
        &self,
        session: &str,
        cwd: &Path,
        calls: &[ToolCall],
        stopped: &(dyn Fn() -> bool + Sync),
    ) -> Option<Vec<ToolOutput>> {
        let mut outputs = Vec::with_capacity(calls.len());
        let mut rest = calls;
        while !rest.is_empty() {
            let n = rest
                .iter()
                .take_while(|c| {
                    let name = &c.function.name;
                    tools::parallel_safe(name) || name == subagent::TASK
                })
                .count()
                .min(PARALLEL_TOOLS);
            if n < 2 {
                outputs.push(self.run_tool(session, cwd, &rest[0], stopped));
                rest = &rest[1..];
            } else {
                let (group, tail) = rest.split_at(n);
                // 先按顺序报出全部工具行，界面上的顺序和模型的调用顺序一致
                let started: Vec<_> =
                    group.iter().map(|c| self.start_tool(session, cwd, c)).collect();
                outputs.extend(std::thread::scope(|s| {
                    let handles: Vec<_> = group
                        .iter()
                        .zip(started)
                        .map(|(call, (action, title))| {
                            s.spawn(move || {
                                self.finish_tool(session, cwd, call, action, &title, stopped)
                            })
                        })
                        .collect();
                    handles
                        .into_iter()
                        .map(|h| {
                            h.join().unwrap_or_else(|_| ToolOutput::fail("tool crashed", "出错"))
                        })
                        .collect::<Vec<_>>()
                }));
                rest = tail;
            }
            if stopped() {
                return None;
            }
        }
        Some(outputs)
    }

    /// 解析调用并报出工具行（改动类先带上 diff 预览）。
    fn start_tool(
        &self,
        session: &str,
        cwd: &Path,
        call: &ToolCall,
    ) -> (Result<Action, String>, String) {
        let name = &call.function.name;
        let action = if !self.tool_allowed(session, name) {
            Err(format!("Tool `{name}` is not available to this subagent"))
        } else if mcp::is_mcp_tool(name) {
            self.mcp_action(name, &call.function.arguments)
        } else {
            Action::parse(name, &call.function.arguments, cwd)
        };
        let (title, kind) = match &action {
            Ok(a) => (a.title(cwd), a.kind()),
            Err(_) => (call.function.name.clone(), "other"),
        };
        self.emit(
            Some(session),
            KernelEvent::ToolCallStarted {
                id: call.id.clone(),
                title: title.clone(),
                status: "pending".into(),
                kind: kind.into(),
                parent: None,
            },
        );
        // 改动预览：先于权限请求发出，用户能看着 diff 决定是否放行
        if let Some(diff) = action.as_ref().ok().and_then(|a| a.preview(cwd)) {
            self.emit(Some(session), KernelEvent::ToolCallDiff { id: call.id.clone(), diff });
        }
        (action, title)
    }

    /// 执行内置工具；写文件 / 改文件前后各读一次，交给 UI 存检查点（回退时还原）。
    fn run_checkpointed(
        &self,
        session: &str,
        action: &Action,
        stopped: &dyn Fn() -> bool,
    ) -> ToolOutput {
        let target = action.written_path().map(Path::to_path_buf);
        let before = target.as_deref().and_then(checkpoint::read_snapshot);
        // 后台任务记在这个会话名下（事件里按会话分组）
        bgtasks::set_current(self.kernel_id, session);
        let output = action.run(stopped);
        if let (Some(path), Some(before)) = (target, before)
            && let Some(after) = checkpoint::read_snapshot(&path)
            && before != after
        {
            let path = path.to_string_lossy().into_owned();
            self.emit(Some(session), KernelEvent::FileChanged { path, before, after });
        }
        output
    }

    /// 模型调的 MCP 工具 → 动作；服务器没连上或没有这个工具时的错误回给模型。
    fn mcp_action(&self, name: &str, arguments: &str) -> Result<Action, String> {
        let (server, tool) = self.mcp.lookup(name).ok_or_else(|| {
            format!("Unknown tool `{name}` (its MCP server may have disconnected)")
        })?;
        let args = if arguments.trim().is_empty() { "{}" } else { arguments };
        let arguments: serde_json::Value =
            serde_json::from_str(args).map_err(|e| format!("Invalid arguments for {name}: {e}"))?;
        Ok(Action::Mcp { server, tool: tool.name, arguments, read_only: tool.read_only })
    }

    /// 授权、执行，并报告结果。
    fn finish_tool(
        &self,
        session: &str,
        cwd: &Path,
        call: &ToolCall,
        action: Result<Action, String>,
        title: &str,
        stopped: &dyn Fn() -> bool,
    ) -> ToolOutput {
        let tool_input: Value =
            serde_json::from_str(&call.function.arguments).unwrap_or(Value::Null);
        let hook_input = |extra: Option<(&str, Value)>| {
            let mut input = json!({"tool_name": call.function.name, "tool_input": tool_input});
            if let Some((key, value)) = extra {
                input[key] = value;
            }
            input
        };
        // 插件的「执行工具前」钩子在授权之前：被拦下就不问用户了
        let blocked = match &action {
            Ok(_) => {
                self.run_hooks(
                    session,
                    cwd,
                    HookEvent::PreToolUse,
                    Some(&call.function.name),
                    hook_input(None),
                )
                .1
            }
            Err(_) => None,
        };
        let mut output = match action {
            Err(message) => ToolOutput::fail(message, "参数无效"),
            Ok(_) if blocked.is_some() => ToolOutput::fail(
                format!("Blocked by a plugin hook: {}", blocked.unwrap_or_default()),
                "插件钩子拦下",
            ),
            Ok(action) => match self.authorize(session, cwd, &action, title, &call.id, stopped) {
                Verdict::Allow => {
                    if stopped() {
                        return ToolOutput::fail("cancelled", "已中断");
                    }
                    self.emit(
                        Some(session),
                        KernelEvent::ToolCallUpdated {
                            id: call.id.clone(),
                            title: None,
                            status: Some("in_progress".into()),
                            raw_output: None,
                            output: None,
                        },
                    );
                    match &action {
                        Action::Mcp { server, tool, arguments, .. } => {
                            self.mcp.call(server, tool, arguments.clone(), stopped)
                        }
                        Action::Task { prompt, agent, .. } => {
                            self.run_task(session, cwd, &call.id, prompt, agent.as_deref())
                        }
                        _ => self.run_checkpointed(session, &action, stopped),
                    }
                }
                Verdict::Deny(reason) => ToolOutput::fail(reason, "已拒绝"),
            },
        };
        // 「执行工具后」钩子：退出码 2 的原因追加到结果里给模型看
        if output.ok && !stopped() {
            let response = json!({"content": output.content});
            if let (_, Some(feedback)) = self.run_hooks(
                session,
                cwd,
                HookEvent::PostToolUse,
                Some(&call.function.name),
                hook_input(Some(("tool_response", response))),
            ) {
                output.content.push_str(&format!("\n\nPlugin hook feedback: {feedback}"));
            }
        }
        if !stopped() {
            self.emit(
                Some(session),
                KernelEvent::ToolCallUpdated {
                    id: call.id.clone(),
                    title: None,
                    status: Some(if output.ok { "completed" } else { "failed" }.into()),
                    raw_output: Some(output.summary.clone()),
                    output: Some(claudesktop_core::output_preview(&output.content)),
                },
            );
        }
        output
    }

    /// 子任务：开子会话跑完，最后一段回复当结果（见 `subagent` 模块）。在主会话这一轮的线程里阻塞着跑，
    /// 取消标志用主会话这一轮的。
    fn run_task(
        &self,
        session: &str,
        cwd: &Path,
        call_id: &str,
        prompt: &str,
        agent: Option<&str>,
    ) -> ToolOutput {
        let name = agent.unwrap_or(subagent::GENERAL);
        let scope = {
            let sessions = self.sessions.lock().unwrap();
            sessions.get(session).and_then(|s| s.plugins_enabled.clone())
        };
        let agents = self.agents(scope.as_deref());
        let Some(agent) = agents.iter().find(|a| a.name == name).cloned() else {
            let names: Vec<&str> = agents.iter().map(|a| a.name.as_str()).collect();
            return ToolOutput::fail(
                format!("Unknown subagent_type `{name}`. Available: {}", names.join(", ")),
                "没有这个角色",
            );
        };
        let child = format!("{session}/{call_id}");
        let flag = {
            let mut sessions = self.sessions.lock().unwrap();
            let Some((flag, state)) = sessions.get(session).and_then(|parent| {
                let flag = parent.turn.clone()?;
                let state = SessionState {
                    cwd: parent.cwd.clone(),
                    model: agent.model.clone().unwrap_or_else(|| parent.model.clone()),
                    history: vec![ChatMessage::new("user", prompt)],
                    mode: parent.mode.clone(),
                    restored: false,
                    instructions: parent.instructions.clone(),
                    skills: parent.skills.clone(),
                    started: true,
                    summary: None,
                    measured: None,
                    turn: Some(flag.clone()),
                    turns: parent.turns,
                    agent: Some(agent.clone()),
                    // 项目级停用的 MCP 服务器子会话照样停用
                    mcp_disabled: parent.mcp_disabled.clone(),
                    plugins_enabled: parent.plugins_enabled.clone(),
                    // 子会话和主会话看同一份记忆（记不进去：工具不给它）
                    memory_project: parent.memory_project.clone(),
                    memory_global: parent.memory_global.clone(),
                };
                Some((flag, state))
            }) else {
                return ToolOutput::fail("cancelled", "已中断");
            };
            sessions.insert(child.clone(), state);
            flag
        };
        let result = run_steps(self, &child, &flag);
        self.sessions.lock().unwrap().remove(&child);
        match result {
            Steps::Stopped => ToolOutput::fail("cancelled", "已中断"),
            Steps::Failed(message) => {
                ToolOutput::fail(format!("The subagent failed: {message}"), "子任务出错")
            }
            Steps::Done { stop, usage, reply, .. } => {
                // 子会话的用量算进主会话这一轮（上下文大小是主会话自己的，不报）
                if let Some(u) = usage {
                    self.emit(
                        Some(session),
                        KernelEvent::UsageUpdated {
                            input_tokens: u.input_tokens,
                            output_tokens: u.output_tokens,
                            reasoning_tokens: u.reasoning_tokens,
                            context_tokens: 0,
                            cache_read_tokens: u.cache_read_tokens,
                        },
                    );
                }
                self.run_hooks(
                    session,
                    cwd,
                    HookEvent::SubagentStop,
                    None,
                    json!({"subagent_type": agent.name, "stop_reason": stop}),
                );
                if reply.trim().is_empty() {
                    return ToolOutput::fail(
                        format!("The subagent stopped ({stop}) without a final report."),
                        "子任务没有给出结果",
                    );
                }
                let summary = subagent::summary(&reply);
                ToolOutput::ok(reply, summary)
            }
        }
    }

    /// 读工具直接放行；其余按模式 → 已保存规则 → 询问用户。
    /// 抓网页是只读的，Plan 模式下也能用（照样要问）。
    fn authorize(
        &self,
        session: &str,
        cwd: &Path,
        action: &Action,
        title: &str,
        tool: &str,
        stopped: &dyn Fn() -> bool,
    ) -> Verdict {
        let access = action.access();
        if access == Access::Read {
            return Verdict::Allow;
        }
        let mode = self.mode(session);
        match mode.as_str() {
            MODE_PLAN if !matches!(access, Access::Fetch | Access::Mcp { read_only: true }) => {
                return Verdict::Deny(
                    "Plan mode is active: writing files and running commands are not allowed. \
                     Investigate with read-only tools and present a plan instead."
                        .into(),
                );
            }
            MODE_BYPASS => return Verdict::Allow,
            MODE_ACCEPT_EDITS if access == Access::Edit && action.inside(cwd) => {
                return Verdict::Allow;
            }
            _ => {}
        }
        let rule = action.rule(cwd);
        let command = match action {
            Action::Shell { command, .. } => Some(command.as_str()),
            _ => None,
        };
        {
            let mut rules = self.rules.lock().unwrap();
            // 拒绝规则优先：命中就不问，直接按「用户拒绝」回给模型
            if let Some(rule) = &rule
                && rules.denies(cwd, rule, command)
            {
                return Verdict::Deny(
                    "The user has a saved rule that always denies this action. \
                     Do not retry it; stop and ask the user how they would like to proceed \
                     (the rule can be removed in Settings)."
                        .into(),
                );
            }
            if let Some(rule) = &rule
                && rules.allows(cwd, rule, command)
            {
                return Verdict::Allow;
            }
        }

        let option =
            |id: &str, name: String| PermissionOptionView { id: id.into(), name, kind: id.into() };
        // 「总是」两个选项的文案：kind = allow_always / reject_always
        let always_name = |rule: &str, deny: bool| {
            let verb = if deny { "拒绝" } else { "允许" };
            if let Some(prefix) = rule.strip_prefix("shell:") {
                format!("本项目内总是{verb}「{prefix}」开头的命令")
            } else if let Some(host) = rule.strip_prefix("fetch:") {
                format!("本项目内总是{verb}访问 {host}")
            } else if rule == "search" {
                format!("本项目内总是{verb}搜索网页")
            } else if let Some(tool) = rule.strip_prefix("mcp:") {
                format!("本项目内总是{verb} {}", tool.replacen(':', " 的 ", 1))
            } else {
                format!("本项目内总是{verb}编辑")
            }
        };
        let mut options = vec![option("allow_once", "允许一次".into())];
        if let Some(rule) = &rule {
            options.push(option("allow_always", always_name(rule, false)));
        }
        options.push(option("reject_once", "拒绝".into()));
        if let Some(rule) = &rule {
            options.push(option("reject_always", always_name(rule, true)));
        }

        match self.ask(session, title, tool, options, stopped).as_deref() {
            Some("allow_once") => Verdict::Allow,
            Some("allow_always") => {
                if let Some(rule) = rule
                    && let Err(e) = self.rules.lock().unwrap().add(cwd, rule)
                {
                    self.emit(None, KernelEvent::Status(format!("保存权限规则失败: {e}")));
                }
                Verdict::Allow
            }
            Some("reject_always") => {
                if let Some(rule) = rule
                    && let Err(e) = self
                        .rules
                        .lock()
                        .unwrap()
                        .add(cwd, format!("{}{}", permissions::DENY_PREFIX, rule))
                {
                    self.emit(None, KernelEvent::Status(format!("保存权限规则失败: {e}")));
                }
                Verdict::Deny(
                    "The user denied this tool call and saved a rule that always denies it. \
                     Do not retry it in this session; ask the user how they would like to proceed."
                        .into(),
                )
            }
            _ => Verdict::Deny(
                "The user denied this tool call. Do not retry it; \
                 stop and ask the user how they would like to proceed."
                    .into(),
            ),
        }
    }

    /// 发出权限请求并阻塞等待应答；被取消时返回 None。
    fn ask(
        &self,
        session: &str,
        title: &str,
        tool: &str,
        options: Vec<PermissionOptionView>,
        stopped: &dyn Fn() -> bool,
    ) -> Option<String> {
        let id = self.next_request.fetch_add(1, Ordering::Relaxed);
        let (tx, rx) = mpsc::channel();
        self.pending.lock().unwrap().insert(id, tx);
        self.emit(
            Some(session),
            KernelEvent::PermissionRequest {
                id,
                title: title.to_string(),
                options,
                tool: Some(tool.to_string()),
            },
        );
        loop {
            match rx.recv_timeout(Duration::from_millis(100)) {
                Ok(choice) => return choice,
                Err(mpsc::RecvTimeoutError::Timeout) if !stopped() => {}
                Err(_) => {
                    self.pending.lock().unwrap().remove(&id);
                    return None;
                }
            }
        }
    }
}

/// 会话的说明：用户级 + 目录里的 AGENTS.md / CLAUDE.md，再加项目配置指定的说明文件
/// （相对项目根，追加在最后——最具体）。开会话和每轮重读都走这里，两边才一致。
fn session_instructions(
    cwd: &Path,
    user: Option<&Path>,
    project: Option<&claudesktop_core::project::ProjectConfig>,
) -> Vec<instructions::Instruction> {
    let mut list = instructions::load(cwd, user);
    if let Some(file) = project.and_then(|p| p.instructions.as_ref()) {
        let path = claudesktop_core::project::project_root(cwd).unwrap_or(cwd).join(file);
        if let Ok(text) = std::fs::read_to_string(&path)
            && !text.trim().is_empty()
            && !list.iter().any(|i| i.path == path)
        {
            list.push(instructions::Instruction {
                path,
                text: text.trim().chars().take(20_000).collect(),
            });
        }
    }
    list
}

pub struct BuiltinKernel {
    inner: Arc<Inner>,
}

/// 内核编号：后台命令表按它分组（同一进程里几个内核各收自己的任务）
static NEXT_KERNEL_ID: AtomicU64 = AtomicU64::new(1);

impl BuiltinKernel {
    /// 创建内核。进程内无握手，立即报告 `Connected`。
    pub fn spawn(config: BuiltinConfig, sink: EventSink) -> anyhow::Result<Self> {
        let agent_name = format!("builtin · {}", config.provider.model);
        let max_steps = config.clamped_max_steps();
        // 这个内核的编号（后台命令表按它分组）
        let kernel_id = NEXT_KERNEL_ID.fetch_add(1, Ordering::Relaxed);
        let mcp = {
            let sink = sink.clone();
            mcp::McpHub::new(config.mcp_config, config.cwd.clone(), move |servers| {
                sink(RoutedEvent { session: None, event: KernelEvent::McpStatus { servers } })
            })
        };
        mcp.set_extra(plugin_servers(&config.plugins));
        // web_search 的 Brave key 存数据目录（`brave_key` 文件），按规则文件的位置推
        if let Some(dir) = config.rules_path.as_ref().and_then(|p| p.parent()) {
            websearch::set_data_dir(dir.to_path_buf());
        } else if let Ok(dir) = claudesktop_core::default_data_dir() {
            websearch::set_data_dir(dir);
        }
        let inner = Arc::new(Inner {
            provider: make_provider(&config.provider),
            default_model: Mutex::new(config.provider.model.clone()),
            sink,
            agent_name: agent_name.clone(),
            default_cwd: config.cwd,
            user_instructions: config.user_instructions,
            max_steps,
            session_titles: config.session_titles,
            context_windows: Mutex::new(config.context_windows),
            sessions: Mutex::default(),
            next_session: AtomicU64::new(1),
            rules: Mutex::new(PermissionRules::load(config.rules_path)),
            mcp,
            plugins: Mutex::new(Arc::new(config.plugins)),
            pending: Mutex::default(),
            next_request: AtomicU64::new(1),
            kernel_id,
            memory_path: config.memory_path,
        });
        inner.emit(None, KernelEvent::Status(format!("自研内核: {}", config.provider.base_url)));
        inner.emit(None, KernelEvent::Connected { agent_name });
        // 后台命令的状态变化整份发一次（表是进程级的，出口在这里装上）
        // 出口只攥弱引用：表是进程级的，强引用会让关掉的内核一直释放不掉（`shutdown` 里也会摘掉）
        {
            let inner = Arc::downgrade(&inner);
            bgtasks::register(
                kernel_id,
                Box::new(move |tasks| {
                    if let Some(inner) = inner.upgrade() {
                        inner.emit(None, KernelEvent::BackgroundTasks { tasks });
                    }
                }),
            );
        }
        // MCP 服务器在后台连，连上之前的轮次没有它们的工具
        inner.mcp.reload();
        Ok(Self { inner })
    }

    /// 新建会话（`history` 有内容 = 续接），报告 `Started`。
    fn open_session(&self, token: u64, cwd: Option<PathBuf>, history: history::Rebuilt) {
        let n = self.inner.next_session.fetch_add(1, Ordering::Relaxed);
        let session_id = format!("builtin-{}-{n}", std::process::id());
        let cwd = cwd.unwrap_or_else(|| self.inner.default_cwd.clone());
        let resumed = !history.messages.is_empty() || history.summary.is_some();
        // 项目级配置（`.claudesktop/settings.json`）：模型、说明文件、停用的 MCP、权限规则
        let project = claudesktop_core::project::load(&cwd);
        for warning in project.as_ref().map(|p| p.warnings.iter()).into_iter().flatten() {
            self.inner.emit(Some(&session_id), KernelEvent::Notice(warning.clone()));
        }
        if let Some(project) = &project
            && !project.permissions.is_empty()
            && let Some(root) = claudesktop_core::project::project_root(&cwd)
        {
            // 项目里写的权限规则并进 permissions.json（集合语义，去重后落盘）
            if let Some(path) = &self.inner.rules.lock().unwrap().path().map(PathBuf::from) {
                let file = std::fs::read_to_string(path).unwrap_or_else(|_| "{}".into());
                let file: Value = serde_json::from_str(&file).unwrap_or_else(|_| json!({}));
                if let Some(merged) =
                    claudesktop_core::project::merge_permissions(&file, root, project)
                    && serde_json::to_string_pretty(&merged)
                        .map_err(|e| e.to_string())
                        .and_then(|text| {
                            std::fs::create_dir_all(path.parent().unwrap_or(Path::new(".")))
                                .map_err(|e| e.to_string())
                                .and_then(|()| {
                                    let tmp = path.with_extension("json.tmp");
                                    std::fs::write(&tmp, text).map_err(|e| e.to_string())?;
                                    std::fs::rename(&tmp, path).map_err(|e| e.to_string())
                                })
                        })
                        .is_err()
                {
                    self.inner.emit(
                        Some(&session_id),
                        KernelEvent::Notice(
                            "项目配置里的权限规则并进 permissions.json 失败".into(),
                        ),
                    );
                }
            }
        }
        let instructions =
            session_instructions(&cwd, self.inner.user_instructions.as_deref(), project.as_ref());
        let skills = self.inner.skills(project.as_ref().and_then(|p| p.plugins_enabled.as_deref()));
        // 项目指定的默认模型优先（全局默认其次）
        let model = project
            .as_ref()
            .and_then(|p| p.model.clone())
            .unwrap_or_else(|| self.inner.default_model.lock().unwrap().clone());
        let mcp_disabled = project.as_ref().map(|p| p.mcp_disabled.clone()).unwrap_or_default();
        let plugins_enabled = project.as_ref().and_then(|p| p.plugins_enabled.clone());
        let mut state = SessionState {
            cwd,
            model: model.clone(),
            history: history.messages,
            mode: MODE_DEFAULT.into(),
            restored: resumed,
            instructions,
            skills,
            started: false,
            summary: history.summary,
            measured: None,
            turn: None,
            turns: 0,
            agent: None,
            mcp_disabled,
            plugins_enabled,
            memory_project: Vec::new(),
            memory_global: Vec::new(),
        };
        // 记住的东西一开始就带上（之后每轮重读）
        for warning in state.reload_memory(self.inner.memory_path.as_deref()) {
            self.inner.emit(Some(&session_id), KernelEvent::Notice(warning));
        }
        self.inner.sessions.lock().unwrap().insert(session_id.clone(), state);
        self.inner.emit(
            Some(&session_id),
            KernelEvent::Started {
                token,
                agent_name: self.inner.agent_name.clone(),
                session_id: session_id.clone(),
                model: Some(model),
                modes: modes(),
                current_mode: Some(MODE_DEFAULT.into()),
                resumed,
            },
        );
    }
}

/// 跑测试、开浏览器、截图默认不打扰前台（用户在别的窗口干活）
const BACKGROUND: &str = "Work in the background by default: the user keeps using their computer while you work. \
    Run tests, browsers and screenshot tools headless (headless browser modes, `--headless` flags, CI-style \
    non-interactive runs), do not open GUI windows, browser tabs or report viewers, and do not steal focus. \
    To check a GUI or web page, use a headless browser or a virtual display (e.g. Xvfb on Linux) and save \
    screenshots to files you can read. Only bring something to the foreground when the user asks for it.";

/// 进系统提示词的「背景材料」：项目说明文件、技能、记忆。
///
/// 打包传（而不是一个个参数）是为了别让 `system_prompt` 的参数越加越长，
/// 以后再加一类背景材料只往这里添字段。
struct Context<'a> {
    instructions: &'a [instructions::Instruction],
    skills: &'a [Skill],
    memory_project: &'a [claudesktop_core::memory::Entry],
    memory_global: &'a [claudesktop_core::memory::Entry],
}

fn system_prompt(
    cwd: &Path,
    mode: &str,
    restored: bool,
    summary: Option<&str>,
    agent: Option<&AgentDef>,
    ctx: Context<'_>,
) -> String {
    let Context { instructions: project, skills, memory_project, memory_global } = ctx;
    // 子会话没有待办清单（计划是主会话的）
    let todo = if agent.is_none() {
        "For tasks with several steps, track progress with todo_write. "
    } else {
        ""
    };
    let mut prompt = format!(
        "You are a coding agent running inside claudesktop, a desktop agent harness, \
         working on the user's machine.\n\
         Working directory: {}\n\
         Platform: {}\n\n\
         Use the tools to inspect and change the project: find files with glob and search code with grep \
         (rather than shell commands), read files before editing them, \
         request independent reads and searches together in one step (they run in parallel), \
         prefer edit_file for small changes, read URLs the user gives with web_fetch, and verify your work (e.g. build or run tests) \
         when it makes sense. {todo}\
         Tool calls that change things may need the user's approval; \
         if one is denied, do not retry it and ask the user instead.\n\
         {BACKGROUND}\n\
         Keep replies concise. Answer in the user's language. Use Markdown for structure and code.",
        cwd.display(),
        std::env::consts::OS,
    );
    if let Some(agent) = agent {
        prompt.push_str("\n\n");
        prompt.push_str(&subagent::prompt_section(agent));
    }
    if restored {
        prompt.push_str(
            "\n\nThis conversation was restored from a saved log. Earlier tool calls appear as \
             `[tool] <title> → <status>: <summary>` lines; their full inputs and outputs are not \
             available, so re-read files if you need details. Never write such lines yourself.",
        );
    }
    if let Some(section) = instructions::prompt_section(project) {
        prompt.push_str("\n\n");
        prompt.push_str(&section);
    }
    if let Some(section) = claudesktop_core::memory::prompt_section(memory_project, memory_global) {
        prompt.push_str("\n\n");
        prompt.push_str(&section);
    }
    if !skills.is_empty() {
        prompt.push_str(
            "\n\n# Skills\n\
             The user's plugins provide these skills: instructions for specific kinds of tasks. \
             When a task matches a skill's description, read its file with read_file before starting \
             and follow it; paths it mentions are relative to the skill's folder.",
        );
        for skill in skills {
            prompt.push_str(&format!(
                "\n- {}: {} ({})",
                skill.name,
                skill.description,
                skill.path.display()
            ));
        }
    }
    if let Some(summary) = summary {
        prompt.push_str("\n\n");
        prompt.push_str(&compact::prompt_section(summary));
    }
    if mode == MODE_PLAN {
        prompt.push_str(
            "\n\nPlan mode is active: only read-only tools are available. \
             Investigate, then present a concrete step-by-step plan for the user to approve. \
             Do not attempt to modify anything.",
        );
    }
    prompt
}

/// provider 的 finish_reason → ACP 风格的 stop_reason。
fn stop_reason(finish: Option<&str>) -> String {
    match finish {
        None | Some("stop") | Some("tool_calls") => "end_turn".into(),
        Some("length") => "max_tokens".into(),
        Some(other) => other.into(),
    }
}

/// 交给模型总结的记录最多多少字节：窗口的一部分（按 4 字节 / token 估），窗口未知时用固定上限。
fn transcript_budget(window: Option<u64>) -> usize {
    window.map_or(compact::TRANSCRIPT_MAX_BYTES_UNKNOWN, |w| {
        (w * compact::TRANSCRIPT_PERCENT / 100 * 4) as usize
    })
}

/// 一轮对话（在独立线程里运行）：先跑提交消息的钩子，再走工具循环，最后报告用量和结束。
fn run_turn(inner: Arc<Inner>, session: String, flag: Arc<AtomicBool>) {
    if !inner.prompt_hooks(&session, &flag) {
        return;
    }
    let (stop, usage, context_tokens) = match run_steps(&inner, &session, &flag) {
        Steps::Stopped => return, // cancel() 已经报告过 TurnEnded
        Steps::Failed(message) => {
            // 和从日志重建的历史一致（见 history::rebuild）：失败也占一条 assistant 消息，
            // 用户点「重试」重发时不会出现两条连着的用户消息（Anthropic 要求交替）
            inner.commit(
                &session,
                &flag,
                vec![ChatMessage::new("assistant", format!("[turn failed: {message}]"))],
            );
            inner.finish(&session, &flag);
            inner.emit(Some(&session), KernelEvent::PromptFailed { message });
            return;
        }
        Steps::Done { stop, usage, context_tokens, .. } => (stop, usage, context_tokens),
    };

    inner.finish(&session, &flag);
    if let Some(u) = usage {
        inner.emit(
            Some(&session),
            KernelEvent::UsageUpdated {
                input_tokens: u.input_tokens,
                output_tokens: u.output_tokens,
                reasoning_tokens: u.reasoning_tokens,
                context_tokens,
                cache_read_tokens: u.cache_read_tokens,
            },
        );
    }
    let cwd = inner.sessions.lock().unwrap().get(&session).map(|s| s.cwd.clone());
    inner.emit(Some(&session), KernelEvent::TurnEnded { stop_reason: stop.clone() });
    // 「一轮结束」钩子只是通知：这一轮已经结束了，结果不影响什么
    if let Some(cwd) = cwd {
        inner.run_hooks(&session, &cwd, HookEvent::Stop, None, json!({"stop_reason": stop}));
    }
    // 第一轮正常结束后让模型给会话起个标题（失败静默，保留截断版）；子会话不用
    if stop == "end_turn" && inner.session_titles && inner.is_first_turn(&session) {
        name_session(&inner, &session);
    }
}

/// 用模型给会话起标题：单独发一个 max_tokens 很小的请求，只带第一条用户消息和第一条回复。
/// 成功发 `TitleGenerated`；失败 / 取消 / 内容为空都不吭声（界面保留截断版标题）。
fn name_session(inner: &Inner, session: &str) {
    let Some((model, first_user, first_reply)) = inner.title_inputs(session) else {
        return;
    };
    let prompt = format!(
        "为下面的对话起一个简短的标题（不超过 12 个字，概括用户想要什么，用对话的语言，\
         不加引号和句号）。只输出标题本身。\n\n用户：{}\n\n助手：{}",
        first_user, first_reply
    );
    let messages = vec![ChatMessage::new("user", prompt)];
    let no_stop = || false;
    let mut text = String::new();
    let _tag = gateway::tag(session, "title", 1);
    let outcome = inner.provider.stream_chat(&model, &messages, &[], &no_stop, &mut |event| {
        if let StreamEvent::Text(t) = event {
            text.push_str(&t);
        }
    });
    let title = text.trim().trim_matches(['"', '\'', '「', '」', '『', '』']).trim();
    let title: String = title.chars().take(24).collect();
    if outcome.is_ok() && !title.is_empty() {
        inner.emit(Some(session), KernelEvent::TitleGenerated { title });
    }
}

/// 工具循环：请求模型 → 执行工具 → 再请求，直到不再调用工具。主会话的一轮和子任务（子会话）都走这里。
fn run_steps(inner: &Inner, session: &str, flag: &Arc<AtomicBool>) -> Steps {
    let stopped = || flag.load(Ordering::SeqCst);
    // 用量网关的日志按会话、用途归类（子任务的 ID 是 `主会话/调用 ID`）
    let purpose = if subagent::split_id(session).is_some() { "subagent" } else { "turn" };
    let _tag = gateway::tag(session, purpose, inner.turn_number(session));
    let mut usage: Option<Usage> = None;
    // 最后一次请求的输入 + 输出 = 此刻上下文的大小（之前各步都包含在这次的输入里）
    let mut context_tokens = 0;
    let mut stop = "max_turn_requests".to_string();
    let mut reply = String::new();
    // 上一步刚压缩过（不连着压两次）/ 这一轮已经因为超限报错压缩过（只压一次）
    let mut just_compacted = false;
    let mut overflow_compacted = false;

    for _ in 0..inner.max_steps {
        // 用量快到窗口：先压缩再请求
        if !just_compacted {
            if let Some((model, window)) =
                inner.model_of(session).and_then(|m| inner.window(&m).map(|w| (m, w)))
                && inner.context_estimate(session) * 100 >= window * compact::AUTO_COMPACT_PERCENT
            {
                match inner.compact(
                    session,
                    flag,
                    &model,
                    false,
                    window * compact::KEEP_PERCENT / 100,
                    transcript_budget(Some(window)),
                ) {
                    Ok(Some(u)) => usage.get_or_insert_default().add(u),
                    Ok(None) => {}
                    // 压缩失败（已报给界面）就带着原样的上下文接着请求
                    Err(_) if !stopped() => {}
                    Err(_) => return Steps::Stopped,
                }
                just_compacted = true;
            }
        } else {
            just_compacted = false;
        }
        let Some(Snapshot { messages, mode, cwd, model, agent }) = inner.snapshot(session) else {
            return Steps::Stopped;
        };
        let mut specs = tools::specs(mode != MODE_PLAN);
        let (disabled, plugin_scope) = {
            let sessions = inner.sessions.lock().unwrap();
            sessions
                .get(session)
                .map(|s| (s.mcp_disabled.clone(), s.plugins_enabled.clone()))
                .unwrap_or_default()
        };
        specs.extend(inner.mcp.specs(mode != MODE_PLAN, &disabled));
        match &agent {
            // 主会话能派子任务；子会话按角色的工具清单筛（不嵌套）
            None => specs.push(subagent::spec(&inner.agents(plugin_scope.as_deref()))),
            Some(agent) => specs.retain(|s| subagent::allows(agent, subagent::spec_name(s))),
        }
        let mut text = String::new();
        let result =
            inner.provider.stream_chat(&model, &messages, &specs, &stopped, &mut |event| {
                if stopped() {
                    return;
                }
                match event {
                    StreamEvent::Text(t) => {
                        text.push_str(&t);
                        inner.emit(Some(session), KernelEvent::AgentMessageChunk { text: t });
                    }
                    StreamEvent::Reasoning(t) => {
                        inner.emit(Some(session), KernelEvent::AgentThoughtChunk { text: t })
                    }
                    StreamEvent::Retrying { status, attempt, delay_ms, reason } => inner.emit(
                        Some(session),
                        KernelEvent::Retrying { status, attempt, delay_ms, reason },
                    ),
                }
            });
        if stopped() {
            return Steps::Stopped;
        }
        let outcome = match result {
            Ok(outcome) => outcome,
            Err(err) => {
                let mut message = format!("{err:#}");
                // 超出上下文：压缩一次再重发这一步
                if !overflow_compacted && claudesktop_core::is_context_overflow(&message) {
                    overflow_compacted = true;
                    let window = inner.window(&model);
                    let keep = window.unwrap_or_else(|| compact::estimate_tokens(&messages))
                        * compact::KEEP_PERCENT
                        / 100;
                    match inner.compact(
                        session,
                        flag,
                        &model,
                        false,
                        keep,
                        transcript_budget(window),
                    ) {
                        Ok(Some(u)) => {
                            usage.get_or_insert_default().add(u);
                            just_compacted = true;
                            continue;
                        }
                        Ok(None) => {}
                        Err(_) if stopped() => return Steps::Stopped,
                        Err(e) => message.push_str(&format!("\n（自动压缩上下文也失败了：{e:#}）")),
                    }
                }
                return Steps::Failed(message);
            }
        };
        let mut step_tokens = 0;
        if let Some(u) = outcome.usage {
            usage.get_or_insert_default().add(u);
            step_tokens = u.input_tokens + u.output_tokens;
            context_tokens = step_tokens;
        }
        if !text.trim().is_empty() {
            reply = text.clone();
        }

        if outcome.tool_calls.is_empty() {
            if !text.is_empty() {
                inner.commit(session, flag, vec![ChatMessage::new("assistant", text)]);
            }
            inner.measure(session, step_tokens, 0);
            stop = stop_reason(outcome.finish_reason.as_deref());
            break;
        }

        // 同一步的多个调用执行完（连续的只读调用并行）；assistant 消息和全部结果一起写进历史，
        // 保证 tool_calls 后面总跟着对应的 tool 消息
        let Some(outputs) = inner.run_tools(session, &cwd, &outcome.tool_calls, &stopped) else {
            return Steps::Stopped;
        };
        let results: Vec<_> = outcome
            .tool_calls
            .iter()
            .zip(outputs)
            .map(|(call, output)| {
                if output.images.is_empty() {
                    ChatMessage::tool_result(call.id.clone(), output.content)
                } else {
                    ChatMessage::tool_result_with_images(
                        call.id.clone(),
                        output.content,
                        output.images,
                    )
                }
            })
            .collect();
        let mut step = Vec::with_capacity(results.len() + 1);
        let unmeasured = results.len();
        let mut call = ChatMessage::assistant_tool_calls(text, outcome.tool_calls);
        // 思考块跟着工具调用回传（Anthropic 开了思考时缺了它，下一步请求会被拒）
        call.thinking = outcome.thinking;
        step.push(call);
        step.extend(results);
        if !inner.commit(session, flag, step) {
            return Steps::Stopped;
        }
        // 工具结果还没被端点数过
        inner.measure(session, step_tokens, unmeasured);
    }
    Steps::Done { stop, usage, context_tokens, reply }
}

/// 一条消息里的文字部分（给钩子看用户说了什么）。
fn message_text(message: &ChatMessage) -> String {
    message
        .parts
        .iter()
        .filter_map(|p| match p {
            ContentPart::Text(t) => Some(t.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// 插件带来的 MCP 服务。
fn plugin_servers(plugins: &[Plugin]) -> Vec<mcp::ServerConfig> {
    plugins
        .iter()
        .flat_map(|p| p.mcp_servers.iter().map(|s| mcp::ServerConfig::from_plugin(&p.name, s)))
        .collect()
}

impl Kernel for BuiltinKernel {
    fn kind(&self) -> &'static str {
        "builtin"
    }

    fn label(&self) -> String {
        self.inner.agent_name.clone()
    }

    fn new_session(&self, token: u64, cwd: Option<PathBuf>) -> anyhow::Result<()> {
        self.open_session(token, cwd, history::Rebuilt::default());
        Ok(())
    }

    /// 进程内的会话不跨进程存活：`session_id` 无用，上下文从本地日志重建。
    fn resume_session(
        &self,
        token: u64,
        _session_id: &str,
        cwd: Option<PathBuf>,
        history: &[Record],
    ) -> anyhow::Result<()> {
        self.open_session(token, cwd, history::rebuild(history));
        Ok(())
    }

    fn send_prompt(
        &self,
        session: &str,
        prompt: claudesktop_core::UserPrompt,
    ) -> anyhow::Result<()> {
        let (flag, warnings) = {
            let mut sessions = self.inner.sessions.lock().unwrap();
            let state =
                sessions.get_mut(session).ok_or_else(|| anyhow::anyhow!("未知会话 {session}"))?;
            if state.turn.is_some() {
                anyhow::bail!("上一轮还没结束");
            }
            // 说明文件可能改过：每轮重读（内容不变时系统提示词不变，不影响提示缓存）
            let project = claudesktop_core::project::load(&state.cwd);
            state.instructions = session_instructions(
                &state.cwd,
                self.inner.user_instructions.as_deref(),
                project.as_ref(),
            );
            state.skills = self.inner.skills(state.plugins_enabled.as_deref());
            // 记忆文件同理：界面删掉一条，下一轮就不在提示词里了
            let warnings = state.reload_memory(self.inner.memory_path.as_deref());
            // 文本 + 图片附件合成一条多段用户消息
            let mut parts = vec![ContentPart::Text(prompt.text)];
            for image in prompt.images {
                parts.push(ContentPart::Image { media_type: image.media_type, data: image.data });
            }
            state.history.push(ChatMessage::with_parts("user", parts));
            let flag = Arc::new(AtomicBool::new(false));
            state.turn = Some(flag.clone());
            state.turns += 1;
            (flag, warnings)
        };
        for warning in warnings {
            self.inner.emit(Some(session), KernelEvent::Notice(warning));
        }
        let inner = self.inner.clone();
        let session = session.to_string();
        std::thread::Builder::new()
            .name("builtin-turn".into())
            .spawn(move || run_turn(inner, session, flag))
            .map_err(|e| anyhow::anyhow!("spawn turn thread: {e}"))?;
        Ok(())
    }

    fn cancel(&self, session: &str) -> anyhow::Result<()> {
        // 持锁置位：与 commit() 的判断互斥，取消后这一轮的结果不会再写进历史
        let cancelled = {
            let mut sessions = self.inner.sessions.lock().unwrap();
            match sessions.get_mut(session).and_then(|s| s.turn.take()) {
                Some(flag) => {
                    flag.store(true, Ordering::SeqCst);
                    true
                }
                None => false,
            }
        };
        if cancelled {
            self.inner
                .emit(Some(session), KernelEvent::TurnEnded { stop_reason: "cancelled".into() });
        }
        Ok(())
    }

    fn resolve_permission(&self, request_id: u64, option_id: Option<String>) -> anyhow::Result<()> {
        let tx = self.inner.pending.lock().unwrap().remove(&request_id);
        match tx {
            Some(tx) => {
                let _ = tx.send(option_id);
                Ok(())
            }
            None => anyhow::bail!("权限请求 {request_id} 已失效"),
        }
    }

    fn set_mode(&self, session: &str, mode_id: &str) -> anyhow::Result<()> {
        if !modes().iter().any(|m| m.id == mode_id) {
            anyhow::bail!("未知模式 {mode_id}");
        }
        let mut sessions = self.inner.sessions.lock().unwrap();
        let state =
            sessions.get_mut(session).ok_or_else(|| anyhow::anyhow!("未知会话 {session}"))?;
        state.mode = mode_id.to_string();
        Ok(())
    }

    fn set_model(&self, session: &str, model: &str) -> anyhow::Result<()> {
        let model = model.trim();
        if model.is_empty() {
            anyhow::bail!("模型 ID 为空");
        }
        let mut sessions = self.inner.sessions.lock().unwrap();
        let state =
            sessions.get_mut(session).ok_or_else(|| anyhow::anyhow!("未知会话 {session}"))?;
        // 进行中的一轮用完当前模型，下一步请求起生效
        state.model = model.to_string();
        *self.inner.default_model.lock().unwrap() = model.to_string();
        Ok(())
    }

    fn set_context_windows(&self, windows: HashMap<String, u64>) {
        *self.inner.context_windows.lock().unwrap() = windows;
    }

    /// 手动压缩：自成一轮（占着 `turn`，期间不能发消息、可以取消），全部对话进摘要。
    fn supports_rewind(&self) -> bool {
        true
    }

    /// 回退：上下文换成日志折算出的样子（工具调用只剩摘要，和续接历史会话一样）。
    fn rewind(&self, session: &str, history: &[Record]) -> anyhow::Result<()> {
        let rebuilt = history::rebuild(history);
        let mut sessions = self.inner.sessions.lock().unwrap();
        let state =
            sessions.get_mut(session).ok_or_else(|| anyhow::anyhow!("未知会话 {session}"))?;
        if state.turn.is_some() {
            anyhow::bail!("这一轮还没结束");
        }
        state.history = rebuilt.messages;
        state.summary = rebuilt.summary;
        state.restored = true;
        state.measured = None;
        Ok(())
    }

    fn compact(&self, session: &str) -> anyhow::Result<()> {
        let (flag, model) = {
            let mut sessions = self.inner.sessions.lock().unwrap();
            let state =
                sessions.get_mut(session).ok_or_else(|| anyhow::anyhow!("未知会话 {session}"))?;
            if state.turn.is_some() {
                anyhow::bail!("这一轮还没结束");
            }
            if state.history.is_empty() {
                anyhow::bail!("还没有可以压缩的对话");
            }
            let flag = Arc::new(AtomicBool::new(false));
            state.turn = Some(flag.clone());
            state.turns += 1;
            (flag, state.model.clone())
        };
        let inner = self.inner.clone();
        let session = session.to_string();
        std::thread::Builder::new()
            .name("builtin-compact".into())
            .spawn(move || {
                let budget = transcript_budget(inner.window(&model));
                let result = inner.compact(&session, &flag, &model, true, 0, budget);
                if flag.load(Ordering::SeqCst) {
                    return; // cancel() 已经报告过 TurnEnded
                }
                inner.finish(&session, &flag);
                if let Ok(Some(u)) = result {
                    inner.emit(
                        Some(&session),
                        KernelEvent::UsageUpdated {
                            input_tokens: u.input_tokens,
                            output_tokens: u.output_tokens,
                            reasoning_tokens: u.reasoning_tokens,
                            context_tokens: 0,
                            cache_read_tokens: u.cache_read_tokens,
                        },
                    );
                }
                // 失败已经以 CompactFailed 报过，这一轮照常结束
                inner.emit(
                    Some(&session),
                    KernelEvent::TurnEnded { stop_reason: "end_turn".into() },
                );
            })
            .map_err(|e| anyhow::anyhow!("spawn compact thread: {e}"))?;
        Ok(())
    }

    fn reconnect_mcp(&self, server: &str) {
        self.inner.mcp.reconnect(server);
    }

    fn clear_mcp_auth(&self, server: &str) -> anyhow::Result<()> {
        self.inner.mcp.clear_auth(server).map_err(|e| anyhow::anyhow!(e))
    }

    /// 会话结束（切走 / 关掉）时把它起的后台命令停掉：模型再也读不到输出了。
    fn close_session(&self, session: &str) {
        bgtasks::stop_session(session);
        self.inner.sessions.lock().unwrap().remove(session);
    }

    fn stop_background_task(&self, id: &str) {
        let _ = bgtasks::stop(id);
    }

    fn reload_mcp(&self) {
        self.inner.mcp.reload();
    }

    fn set_plugins(&self, plugins: Vec<Plugin>) {
        let servers = plugin_servers(&plugins);
        *self.inner.plugins.lock().unwrap() = Arc::new(plugins);
        // 只在插件的 MCP 服务变了时重连（重连会断掉 mcp.json 里的服务一下）
        if self.inner.mcp.set_extra(servers) {
            self.inner.mcp.reload();
        }
    }

    fn shutdown(&self) {
        for state in self.inner.sessions.lock().unwrap().values_mut() {
            if let Some(flag) = state.turn.take() {
                flag.store(true, Ordering::SeqCst);
            }
        }
        self.inner.mcp.stop();
        bgtasks::unregister(self.inner.kernel_id);
    }
}
