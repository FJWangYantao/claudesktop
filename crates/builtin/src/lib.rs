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
//! - 子 agent：`task` 工具开子会话跑子任务，角色内置通用和只读调研两个、其余来自插件（见 `subagent` 模块）
//!
//! 线程模型：每轮对话一个线程（阻塞读 SSE 流、执行工具、等权限应答）；
//! 取消 = 置该轮的标志位，各阻塞点轮询检查，取消后不再发出任何事件。

mod anthropic;
mod approval;
mod auto;
mod bgtasks;
mod compact;
mod dispatch;
mod fetch;
pub mod gateway;
mod hibernate;
mod history;
mod instructions;
pub mod magpie;
pub mod mcp;
mod mcp_http;
mod mcp_oauth;
mod openai;
mod permissions;
mod prompt;
mod runner;

use runner::{run_compact, run_pending_prompt, run_steps};
mod provider;
pub mod remember;
mod request;
pub mod request_bodies;
#[cfg(all(test, windows))]
mod request_memory;
pub mod responses;
mod sse;
mod subagent;
pub mod suggest;
mod tools;
pub use tools::{
    FAILURE_REMIND_STEPS, PROGRESS_REMIND_STEPS, TODO_REMIND_STEPS, ToolOutput, run_command,
};
pub mod websearch;

pub use anthropic::AnthropicProvider;
pub use openai::OpenAiProvider;
pub use permissions::{DENY_PREFIX, PermissionRules, default_rules_path};
pub use responses::ResponsesProvider;

pub use provider::{
    ChatMessage, ChatProvider, ContentPart, FunctionCall, ModelInfo, Protocol, ProviderConfig,
    RequestParams, StreamEvent, StreamOutcome, ThinkingFormat, ThinkingLevel, ThinkingSpec,
    ToolCall, ToolSpec, Usage, apply_thinking, parse_thinking_level, resolve_level,
    thinking_budget_for, thinking_spec,
};

/// 列出端点提供的模型（阻塞，设置页在后台线程调用）。
pub fn fetch_models(config: ProviderConfig) -> anyhow::Result<Vec<ModelInfo>> {
    make_provider(&config).list_models()
}

/// 只留最近 [`MAX_USER_INPUTS_KEPT`] 条用户输入。
fn trim_user_inputs(inputs: &mut Vec<auto::UserInput>) {
    let extra = inputs.len().saturating_sub(MAX_USER_INPUTS_KEPT);
    inputs.drain(..extra);
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
    EventSink, Kernel, KernelEvent, ModeView, PartialGrant, PermissionHunks, PermissionOptionView,
    PlanEntryView, PromptImage, QuestionAnswer, Record, RoutedEvent, SessionKind, SessionSpec,
    checkpoint,
};
use serde_json::{Value, json};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::time::Duration;
use tools::{Access, Action};

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
/// 自动模式的会话里最多记住多少条用户输入（分类器据此判断授权）
const MAX_USER_INPUTS_KEPT: usize = 50;
/// 自动模式：连着被拦这么多次就停下来问人（Claude Code 的阈值）
const MAX_DENIAL_STREAK: u32 = 3;
/// 自动模式：一个会话累计被拦这么多次也停下来
const MAX_DENIALS: u32 = 20;
/// 一轮里连着这么多次回复被输出上限截断就停下来（模型重发也还是截断，再试没意义）
const MAX_TRUNCATED_STEPS: u32 = 2;
/// 延迟工具每轮最多带多少 token 的工具定义（按 schema JSON 字节 / 4 估算）。
/// 超出的旧工具本轮先不声明；模型真要用再搜一次就回来了（`tool_search` 会把它们排到最前）。
const DEFERRED_TOOL_BUDGET_TOKENS: usize = 2_000;

const MODE_DEFAULT: &str = "default";
const MODE_ACCEPT_EDITS: &str = "acceptEdits";
const MODE_PLAN: &str = "plan";
/// 由分类器（一次独立的模型调用）代替用户审批，见 [`auto`] 模块
const MODE_AUTO: &str = "auto";
const MODE_BYPASS: &str = "bypassPermissions";

/// 「自动」模式怎么判。没配好时这个模式不出现在模式列表里（要显式配好才能用）。
#[derive(Debug, Clone, PartialEq, Default)]
pub struct AutoModeConfig {
    /// 信任边界（自然语言，进分类器的系统提示词）；空 = 只有当前仓库算内部
    pub environment: String,
    /// 分类器用哪个模型 + 端点；None = 复用会话的模型和端点
    pub model: Option<ProviderConfig>,
}

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
    /// 端点回过 400、确认看不了图片的模型（之后经 `set_no_vision_models` 更新）
    pub no_vision_models: Vec<String>,
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
    /// 每轮结束后让模型提炼「候选」记忆（只进设置页待确认，不落盘；多一次小请求）。
    /// 默认关，见 `remember` 模块
    pub memory_suggest: bool,
    /// 「自动」模式（分类器代替用户审批）；None = 没配置，这个模式不出现
    pub auto_mode: Option<AutoModeConfig>,
    /// 聊天会话用的模型；None = 跟随全局默认模型
    pub chat_model: Option<String>,
    /// 子任务（`task`）用的模型（同一端点）；None = 跟主会话。角色自己写了模型的以角色为准
    pub subagent_model: Option<String>,
    /// 新建的聊天会话默认开不开联网（输入框里随时能改）
    pub chat_web: bool,
    /// 用户的「聊天说明」文件（每轮重读）：每次聊天都带上的话
    pub chat_instructions: Option<PathBuf>,
    /// 闲置会话的完整历史快照目录；None = 不休眠
    pub hibernation_dir: Option<PathBuf>,
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
            no_vision_models: Vec::new(),
            mcp_config: default_mcp_config(),
            plugins: Vec::new(),
            max_steps: DEFAULT_MAX_STEPS,
            session_titles: true,
            memory_path: default_memory_path(),
            memory_suggest: false,
            auto_mode: None,
            chat_model: None,
            subagent_model: None,
            chat_web: true,
            chat_instructions: None,
            hibernation_dir: None,
        }
    }

    /// 从环境变量读取：
    /// - `CLAUDESKTOP_BASE_URL`（必填）：端点，如 `http://127.0.0.1:8045/v1`
    /// - `CLAUDESKTOP_MODEL`（必填）：模型 ID
    /// - `CLAUDESKTOP_API_KEY`（可选，其次 `OPENAI_API_KEY`）
    /// - `CLAUDESKTOP_PROTOCOL`（可选，`anthropic` = Anthropic 原生协议；
    ///   旧名 `CLAUDEDESKTOP_PROTOCOL` 仍认）
    pub fn from_env() -> anyhow::Result<Self> {
        let var = |k: &str| std::env::var(k).ok().filter(|v| !v.trim().is_empty());
        Self::from_env_with(var)
    }

    /// 和 [`Self::from_env`] 一样，但环境变量由调用方给：单测里不必 `set_var`
    /// （并行测试会互相污染，Rust 2024 下 `set_var` 还是 unsafe）。
    /// 空白值当没设——注意这个过滤在这里做，`from_env` 的闭包只负责读原始值。
    fn from_env_with(var: impl Fn(&str) -> Option<String>) -> anyhow::Result<Self> {
        let var = |k: &str| var(k).filter(|v| !v.trim().is_empty());
        let base_url = var("CLAUDESKTOP_BASE_URL")
            .ok_or_else(|| anyhow::anyhow!("未设置 CLAUDESKTOP_BASE_URL（OpenAI 兼容端点）"))?;
        let model =
            var("CLAUDESKTOP_MODEL").ok_or_else(|| anyhow::anyhow!("未设置 CLAUDESKTOP_MODEL"))?;
        let protocol = protocol_from_env(&var);
        let mut config = Self::new(
            ProviderConfig {
                protocol,
                base_url,
                api_key: var("CLAUDESKTOP_API_KEY").or_else(|| var("OPENAI_API_KEY")),
                model,
                params: RequestParams::default(),
                gateway: None,
                deepseek_key: None,
            },
            std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")),
        );
        config.hibernation_dir = default_hibernation_dir();
        Ok(config)
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

/// 协议选择：主读 `CLAUDESKTOP_PROTOCOL`，没设时认旧名 `CLAUDEDESKTOP_PROTOCOL`
/// （早先文档里就是拼错的这个名字，用户脚本可能还在用）。
pub(crate) fn protocol_from_env(var: &impl Fn(&str) -> Option<String>) -> Protocol {
    match var("CLAUDESKTOP_PROTOCOL").or_else(|| var("CLAUDEDESKTOP_PROTOCOL")).as_deref() {
        Some("anthropic") => Protocol::Anthropic,
        _ => Protocol::OpenAiCompat,
    }
}

/// 延迟工具按预算挑出本轮要声明的那些（见 `DEFERRED_TOOL_BUDGET_TOKENS`）。
///
/// `loaded` 是会话里加载过的延迟工具，`last_used` 给每个工具最后一次被搜到 / 被调用的步序，
/// `token_cost` 给单个工具的估算 token 数。排序：最近用过的在前。
/// **至少放行一个**（即使单个大 MCP 工具的 schema 就超预算）：模型刚搜到它，下一步却看不见，
/// 会陷入「再搜一次 → 还是看不见」的死循环。
fn select_within_budget(
    loaded: &[String],
    last_used: &std::collections::HashMap<String, u64>,
    token_cost: impl Fn(&str) -> usize,
) -> Vec<String> {
    let mut ranked: Vec<&String> = loaded.iter().collect();
    ranked.sort_by_key(|name| std::cmp::Reverse(last_used.get(*name).copied().unwrap_or(0)));
    let mut used = 0usize;
    let mut active = Vec::new();
    for name in ranked {
        let cost = token_cost(name);
        if active.is_empty() || used + cost <= DEFERRED_TOOL_BUDGET_TOKENS {
            used += cost;
            active.push(name.clone());
        }
    }
    active
}

/// 一个工具定义的估算 token 数：schema JSON 字节数 / 4（够用的粗估，不引 tokenizer）。
fn spec_token_cost(spec: &ToolSpec) -> usize {
    serde_json::to_string(&**spec).map(|json| json.len() / 4).unwrap_or(0)
}

/// 「自动」模式可用的前提下才有它（没配置就不出现，见 [`AutoModeConfig`]）。
pub fn permission_modes(auto_available: bool) -> Vec<ModeView> {
    let m = |id: &str, name: &str, desc: &str| ModeView {
        id: id.into(),
        name: name.into(),
        description: Some(desc.into()),
    };
    let mut list = vec![
        m(MODE_DEFAULT, "逐项确认", "写文件和执行命令前先询问"),
        m(MODE_ACCEPT_EDITS, "自动接受编辑", "自动允许项目内的文件编辑，命令仍询问"),
        m(MODE_PLAN, "计划模式", "只读：不写文件、不执行命令"),
    ];
    if auto_available {
        list.push(m(MODE_AUTO, "自动", "由分类器判断：放行常规操作，拦下越界的和不可逆的"));
    }
    list.push(m(MODE_BYPASS, "全部允许", "全部自动允许（谨慎）"));
    list
}

#[derive(Debug)]
enum HistoryStorage {
    Resident,
    Spilling(u64),
    Dormant(hibernate::SnapshotLocation),
    Restoring(hibernate::SnapshotLocation),
}

struct SessionState {
    cwd: PathBuf,
    /// 会话类型（聊天会话不带工作目录那套，见「设计说明-聊天与Code分离.md」）
    kind: SessionKind,
    /// 聊天会话的「联网」开关（会话级，`Kernel::set_web` 改；Code 会话恒为 false 不用）
    web: bool,
    /// 本会话使用的模型
    model: String,
    /// 本会话临时覆盖的思考档位（会话级 chip 选的）；None = 用档案里的
    thinking: Option<ThinkingLevel>,
    /// 不含系统提示词的对话历史；只有 `HistoryStorage::Resident` 时驻留在这里
    history: Vec<ChatMessage>,
    history_storage: HistoryStorage,
    history_generation: u64,
    /// 休眠、恢复、发送、压缩、回退和关闭共用的每会话 I/O 锁
    history_io: Arc<Mutex<()>>,
    /// `turn` 之外仍读历史的工作（轮末标题、记忆提炼）
    history_activity: usize,
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
    /// 系统提示词里已经带上的条目 id（`<memory-context>` 不再重复附）
    memory_shown: Vec<String>,
    /// 这一轮到目前为止带进过上下文的条目 id（`used_ms` 据此更新，只在写盘时落下去）
    memory_used: Vec<String>,
    /// 用户消息和提交的提问答案（从早到晚，最近 50 条），供自动模式判断授权。
    user_inputs: Vec<auto::UserInput>,
    /// 当前的待办清单（`todo_write` 最后一次写的）：提醒和压缩摘要要用
    todos: Vec<PlanEntryView>,
    /// 上次写清单之后又走了几步（每步一次请求）：到 `TODO_REMIND_STEPS` 提醒一次
    steps_since_todo: u32,
    /// 本会话已经通过 `tool_search` 额外声明给模型的工具（见 `tools::exposure`）。
    /// 骨架阶段恒为空；续接时从日志的 `Record::ToolLoadout` 拿回来。
    loaded_tools: Vec<String>,
    /// 已经走了多少步（每步一次请求）：给 `tool_last_step` 排序用
    step_seq: u64,
    /// 每个延迟工具最后一次被搜到或被调用的步序；预算淘汰时越近越优先
    tool_last_step: std::collections::HashMap<String, u64>,
    /// 运行中收到的「插话」：`run_steps` 在步间安全点把这些话追加成新的用户消息，
    /// 模型下一步就能看见（见 [`Kernel::steer`]）
    pending_steer: std::collections::VecDeque<claudesktop_core::UserPrompt>,
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
        // 系统提示词里会带哪些：这一轮 `<memory-context>` 不重复它们
        self.memory_shown = memory::shown_ids(&self.memory_project, &self.memory_global);
        warnings
    }

    /// 这一轮开始时把两份记忆文件重读一遍（系统提示词跟着变），返回读不了的原因。
    fn reload_memory_for_turn(&mut self, global: Option<&Path>) -> Vec<String> {
        self.memory_used.clear();
        self.reload_memory(global)
    }

    /// 系统提示词 + 历史的粗估 token（端点还没报告过用量时用）。
    fn estimate_tokens(&self) -> u64 {
        self.system_prompt().len() as u64 / 4 + compact::estimate_tokens(&self.history)
    }
}

struct Inner {
    provider: Arc<dyn ChatProvider>,
    /// 构造 provider 用的配置（提炼候选记忆这类旁路请求另起一个 provider 时用）
    provider_config: ProviderConfig,
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
    /// 见 [`BuiltinConfig::no_vision_models`]
    no_vision_models: Mutex<Vec<String>>,
    sessions: Mutex<HashMap<String, SessionState>>,
    next_session: AtomicU64,
    rules: Mutex<PermissionRules>,
    /// MCP 服务器（所有会话共用）
    mcp: Arc<mcp::McpHub>,
    /// 启用的插件（技能、钩子；它们的 MCP 服务在 `mcp` 里）
    plugins: Mutex<Arc<Vec<Plugin>>>,
    /// 等待 UI 应答的请求：请求 ID → 应答通道（权限选项 / 提问答案）
    pending: Mutex<HashMap<u64, mpsc::Sender<Reply>>>,
    next_request: AtomicU64,
    /// 这个内核的编号（后台命令表按它分组：同一进程里几个内核各收自己的）
    kernel_id: u64,
    /// 全局记忆文件（见 [`BuiltinConfig::memory_path`]）
    memory_path: Option<PathBuf>,
    /// 每轮结束后提炼候选记忆（见 [`BuiltinConfig::memory_suggest`]）
    memory_suggest: bool,
    /// 「自动」模式的配置和分类器 provider（None = 没开这个模式）
    auto_mode: Option<AutoMode>,
    /// 聊天会话用的模型（None = 跟随全局默认）
    chat_model: Option<String>,
    /// 子任务用的模型（见 [`BuiltinConfig::subagent_model`]）
    subagent_model: Option<String>,
    /// 新建聊天会话默认的联网开关
    chat_web: bool,
    /// 用户的「聊天说明」文件（每轮重读）
    chat_instructions: Option<PathBuf>,
    /// 闲置历史快照目录（None = 功能关闭）
    hibernation_dir: Option<PathBuf>,
}

/// 「自动」模式的运行时：配置 + 每个会话的拒绝计数。
struct AutoMode {
    config: AutoModeConfig,
    /// 每个会话连续被拒绝几次 / 一共被拒绝几次（连续 3 次或累计 20 次就暂停自动模式）
    denials: Mutex<HashMap<String, Denials>>,
    /// 分类器上次用的 provider 和造它的配置：配置没变就接着用（连接复用，不每次重新握手）
    provider: Mutex<Option<(ProviderConfig, Arc<dyn ChatProvider>)>>,
}

impl AutoMode {
    /// 按配置取分类器的 provider：和上次一样就复用，变了（换了模型 / 端点 / 档位）就重造。
    fn provider_for(&self, config: &ProviderConfig) -> Arc<dyn ChatProvider> {
        let mut cached = self.provider.lock().unwrap();
        if let Some((used, provider)) = cached.as_ref()
            && used == config
        {
            return provider.clone();
        }
        let provider = make_provider(config);
        *cached = Some((config.clone(), provider.clone()));
        provider
    }
}

/// 拒绝计数（见 [`AutoMode::denials`]）。
#[derive(Debug, Default, Clone, Copy)]
struct Denials {
    /// 连着被拒了几次（放行一次就清零）
    streak: u32,
    /// 累计被拒了几次（这个会话里）
    total: u32,
}

/// 默认 MCP 配置文件：数据目录下的 `mcp.json`。
pub fn default_mcp_config() -> Option<PathBuf> {
    claudesktop_core::default_data_dir().ok().map(|d| d.join("mcp.json"))
}

/// 默认的全局记忆文件：数据目录下的 `memory.json`。
pub fn default_memory_path() -> Option<PathBuf> {
    claudesktop_core::memory::global_path()
}

/// 闲置历史默认允许落盘；`CLAUDESKTOP_HIBERNATE=0` 可关闭。
pub fn default_hibernation_dir() -> Option<PathBuf> {
    let enabled = std::env::var("CLAUDESKTOP_HIBERNATE")
        .map_or(true, |value| !value.is_empty() && value != "0");
    enabled
        .then(|| claudesktop_core::default_data_dir().ok())
        .flatten()
        .map(|dir| dir.join("session-hibernation"))
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
    ///
    /// 不带用量：每一次请求的用量在 `run_steps` 里立刻报给界面（`Inner::report_usage`），
    /// 不然跑着的时候上下文用量要等整轮结束才涨。
    Done {
        stop: String,
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
    /// 只放行这些块（写文件 / 改文件按块同意），其余块不写盘
    Partial(tools::KeepHunks),
    /// 拒绝；内容回给模型
    Deny(String),
    /// 自动模式拦下：`reason` 回给模型并进工具展开输出，`summary` 显示在失败的工具行上。
    /// 不另发 `Notice`——失败的工具行本身就是用户要看到的那条记录。
    AutoDeny {
        reason: String,
        summary: &'static str,
    },
}

/// 用户在权限卡上选了什么：选项 ID，加上（选了按块同意时）放行哪些块。
#[derive(Debug, Clone)]
struct Answer {
    option: String,
    hunks: Option<Vec<u32>>,
}

/// 内核等 UI 应答的两种请求共用一个通道（见 `Inner::pending`）。
enum Reply {
    Permission(Option<Answer>),
    Question(QuestionAnswer),
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

    /// 可用的子 agent 角色：内置的（插件里没有同名的时候）排在前面，然后是插件提供的。
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
        let builtin: Vec<AgentDef> = subagent::builtin()
            .into_iter()
            .filter(|b| !agents.iter().any(|a| a.name == b.name))
            .collect();
        agents.splice(0..0, builtin);
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

    /// 这个会话是聊天会话时返回它的联网开关（`Some`），Code 会话返回 `None`。
    /// 界面不知道会话类型，内核按 `SessionState::kind` 判断。
    fn chat_web_of(&self, session: &str) -> Option<bool> {
        let sessions = self.sessions.lock().unwrap();
        sessions.get(session).filter(|s| s.kind == SessionKind::Chat).map(|s| s.web)
    }

    /// 这个会话是不是聊天会话。
    fn is_chat(&self, session: &str) -> bool {
        let sessions = self.sessions.lock().unwrap();
        sessions.get(session).is_some_and(|s| s.kind == SessionKind::Chat)
    }

    /// 这个会话是不是子会话（`task` 开的）。
    fn is_subagent(&self, session: &str) -> bool {
        let sessions = self.sessions.lock().unwrap();
        sessions.get(session).is_some_and(|s| s.agent.is_some())
    }

    /// 本会话已经通过 `tool_search` 加载的工具（交给 `tools::for_model` 决定哪些要声明）。
    fn loaded_tools(&self, session: &str) -> Vec<String> {
        let sessions = self.sessions.lock().unwrap();
        sessions.get(session).map(|s| s.loaded_tools.clone()).unwrap_or_default()
    }

    /// 每个延迟工具最后一次被搜到 / 被调用的步序（预算淘汰排序用）。
    fn tool_last_step(&self, session: &str) -> std::collections::HashMap<String, u64> {
        let sessions = self.sessions.lock().unwrap();
        sessions.get(session).map(|s| s.tool_last_step.clone()).unwrap_or_default()
    }

    /// 收下运行中收到的插话（由 `run_steps` 在步间安全点取走）。
    fn take_steer(&self, session: &str) -> Vec<claudesktop_core::UserPrompt> {
        let mut sessions = self.sessions.lock().unwrap();
        sessions
            .get_mut(session)
            .map(|state| state.pending_steer.drain(..).collect())
            .unwrap_or_default()
    }

    /// 是否还有没送出去的插话（`run_steps` 收尾前用它决定要不要继续跑一步）。
    fn has_steer(&self, session: &str) -> bool {
        self.sessions
            .lock()
            .unwrap()
            .get(session)
            .is_some_and(|state| !state.pending_steer.is_empty())
    }

    /// 把插话追加成一条新的用户消息（和正常发送走同一套 history / user_inputs）。
    fn push_steered_message(
        &self,
        session: &str,
        text: String,
        images: Vec<claudesktop_core::PromptImage>,
    ) {
        let mut parts = vec![ContentPart::Text(text.clone())];
        for image in images {
            parts.push(ContentPart::Image { media_type: image.media_type, data: image.data });
        }
        let mut sessions = self.sessions.lock().unwrap();
        if let Some(state) = sessions.get_mut(session) {
            state.history.push(ChatMessage::with_parts("user", parts));
            state.user_inputs.push(auto::UserInput::Message(text));
            trim_user_inputs(&mut state.user_inputs);
        }
    }

    /// 记下「这一步用到了这个工具」，下一轮预算排序把它排前面。
    fn touch_tool(&self, session: &str, name: &str) {
        let mut sessions = self.sessions.lock().unwrap();
        if let Some(state) = sessions.get_mut(session) {
            state.step_seq += 1;
            let step = state.step_seq;
            state.tool_last_step.insert(name.to_string(), step);
        }
    }

    /// 单个工具定义的估算 token 数（预算淘汰用）；找不到定义时给 0，按「免费」放进预算。
    fn tool_spec_token_cost(
        &self,
        name: &str,
        writable: bool,
        disabled: &[String],
        plugin_scope: Option<&[String]>,
    ) -> usize {
        if let Some(spec) =
            tools::specs(writable).iter().find(|spec| subagent::spec_name(spec) == name)
        {
            return spec_token_cost(spec);
        }
        if let Some(spec) =
            self.mcp.specs_for(writable, disabled, |candidate| candidate == name).into_iter().next()
        {
            return spec_token_cost(&spec);
        }
        if name == subagent::TASK {
            return spec_token_cost(&subagent::spec(&self.agents(plugin_scope)).into());
        }
        0
    }

    /// 这个模型看不了图片吗（能力表说只吃文字，或端点回过 400、界面记进了名单）。
    /// 是的话请求里的图片换成占位文字（见设计说明 7.3）。
    fn model_blind_to_images(&self, model: &str) -> bool {
        if self.no_vision_models.lock().unwrap().iter().any(|m| m.eq_ignore_ascii_case(model)) {
            return true;
        }
        // 能力表（`image_prep::vision_support`，和思考档位一个写法）：认得出才算数
        claudesktop_core::image_prep::vision_support(
            match self.provider_config.protocol {
                Protocol::OpenAiCompat => "openai",
                Protocol::Anthropic => "anthropic",
                Protocol::Responses => "responses",
            },
            model,
        ) == Some(false)
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

    /// 提炼候选记忆要的素材：模型、这一轮的用户消息、最后一段非空回复、已有的记忆正文。
    fn turn_inputs(&self, session: &str) -> Option<(String, String, String, Vec<String>)> {
        let sessions = self.sessions.lock().unwrap();
        let state = sessions.get(session)?;
        // 这一轮：最后一次用户消息之后的内容
        let mut user = String::new();
        let mut reply = String::new();
        for message in state.history.iter().rev() {
            if message.is_user_turn() {
                user = message_text(message);
                break;
            }
            if reply.is_empty()
                && let Some(text) = message.text().filter(|t| !t.trim().is_empty())
            {
                reply = text.to_string();
            }
        }
        if user.trim().is_empty() || reply.trim().is_empty() {
            return None;
        }
        // 已经记下的（免得再提一遍）：这一轮带进过上下文的那些——系统提示词里的，加上
        // 附在用户消息尾部 `<memory-context>` 里的（它们就在上面的 `user` 里，不列出来模型会当成新的再提一遍）
        let known: Vec<String> = state
            .memory_project
            .iter()
            .chain(&state.memory_global)
            .filter(|e| state.memory_used.contains(&e.id) || state.memory_shown.contains(&e.id))
            .map(|e| e.text.clone())
            .collect();
        Some((state.model.clone(), user, reply, known))
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
        // 聊天会话不跑钩子：钩子是插件配的 shell 命令，按工作目录跑，聊天没有工作目录
        if self.is_chat(session) {
            return (Vec::new(), None);
        }
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
            append_to_last_text(last, &extra);
        }
        self.attach_memory(session, flag);
        true
    }

    /// 这一轮的用户消息里提到了哪些记下的事：相关的条目附在这条消息尾部（`<memory-context>`）。
    ///
    /// 放这里（而不是系统提示词）是因为 system 一变整块提示缓存就失效（`anthropic.rs` 的
    /// `cache_control: ephemeral`），钩子上下文也是这么处理的。系统提示词里已经带上的条目不重复，
    /// 带上的记进 `memory_used`（下次写盘时刷新 `used_ms`，淘汰时不会先挑它们）。
    fn attach_memory(&self, session: &str, flag: &AtomicBool) {
        use claudesktop_core::memory;
        let mut sessions = self.sessions.lock().unwrap();
        let Some(state) = sessions.get_mut(session) else {
            return;
        };
        let prompt = match state.history.last() {
            Some(message) if message.role == "user" => message_text(message),
            _ => return,
        };
        if prompt.trim().is_empty() {
            return;
        }
        let shown = state.memory_shown.clone();
        let (block, ids) = {
            let exclude: Vec<&str> = shown.iter().map(String::as_str).collect();
            memory::context(&state.memory_project, &state.memory_global, &prompt, &exclude)
        };
        // 这一轮会用到的条目：拼好的那一节 + 附上的这些
        let mut used = shown;
        if let Some(block) = block {
            if flag.load(Ordering::SeqCst) {
                return;
            }
            used.extend(ids);
            if let Some(last) = state.history.last_mut() {
                append_to_last_text(last, &format!("\n\n{block}"));
            }
        }
        state.memory_used = used;
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
        let mut messages = state.system_messages();
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

    /// 历史恢复完成后追加用户消息；取消、关闭或新一轮已经替换这一轮的占位时不再追加。
    fn prepare_prompt(
        &self,
        session: &str,
        prompt: claudesktop_core::UserPrompt,
        flag: &Arc<AtomicBool>,
    ) -> Option<Vec<String>> {
        let mut sessions = self.sessions.lock().unwrap();
        let state = sessions.get_mut(session)?;
        if flag.load(Ordering::SeqCst)
            || state.turn.as_ref().is_none_or(|current| !Arc::ptr_eq(current, flag))
        {
            return None;
        }
        // 说明文件可能改过：每轮重读（内容不变时系统提示词不变，不影响提示缓存）
        if state.kind == SessionKind::Chat {
            state.instructions = instructions::chat_instructions(self.chat_instructions.as_deref());
        } else {
            let project = claudesktop_core::project::load(&state.cwd);
            state.instructions = session_instructions(
                &state.cwd,
                self.user_instructions.as_deref(),
                project.as_ref(),
            );
            state.skills = self.skills(state.plugins_enabled.as_deref());
        }
        let warnings = state.reload_memory_for_turn(self.memory_path.as_deref());
        let text = prompt.text.clone();
        let full = if prompt.attachments.is_empty() {
            text.clone()
        } else {
            claudesktop_core::attach::compose(&text, &prompt.attachments)
        };
        let mut parts = vec![ContentPart::Text(full)];
        for image in prompt.images {
            parts.push(ContentPart::Image { media_type: image.media_type, data: image.data });
        }
        state.history.push(ChatMessage::with_parts("user", parts));
        state.user_inputs.push(auto::UserInput::Message(text));
        trim_user_inputs(&mut state.user_inputs);
        Some(warnings)
    }

    /// 把一次请求（或一次压缩）的用量报给界面。
    ///
    /// 工具循环每走一步就报一次，输入框旁的用量圆环跑着的时候就跟着涨；
    /// `context` 是这次请求结束时上下文里有多少 token（输入 + 输出），不知道时报 0——
    /// 界面把 0 当成「这次不更新上下文」，只并进本轮累计（压缩、子会话的用量都这样报）。
    fn report_usage(&self, session: &str, u: Usage, context: u64) {
        // 子会话的用量转进主会话的累计里，但它自己的上下文大小和主会话无关
        let context = if subagent::split_id(session).is_some() { 0 } else { context };
        self.emit(
            Some(session),
            KernelEvent::UsageUpdated {
                input_tokens: u.input_tokens,
                output_tokens: u.output_tokens,
                reasoning_tokens: u.reasoning_tokens,
                context_tokens: context,
                cache_read_tokens: u.cache_read_tokens,
            },
        );
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
            // 上次附上的清单不交给模型再总结（压缩完附上最新的）
            let previous =
                state.summary.as_deref().map(|p| tools::summary_without_todos(p).to_string());
            (state.history.clone(), previous)
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
        let (summary, context_tokens) = {
            let mut sessions = self.sessions.lock().unwrap();
            let state = match sessions.get_mut(session) {
                Some(state) if !stopped() => state,
                _ => anyhow::bail!("cancelled"),
            };
            state.history = split.apply(&history);
            // 写清单的调用可能被压进摘要：当前清单原样附在摘要后面，模型压缩后还知道做到哪了
            let summary = tools::summary_with_todos(&summary, &state.todos);
            state.summary = Some(summary.clone());
            state.measured = None;
            (summary, state.estimate_tokens())
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

    /// 一步走完：这一步写了清单就重新计数，否则加一。
    fn count_step(&self, session: &str, wrote_todo: bool) {
        if let Some(state) = self.sessions.lock().unwrap().get_mut(session) {
            state.steps_since_todo = if wrote_todo { 0 } else { state.steps_since_todo + 1 };
        }
    }

    /// 清单还有没做完的项、又好几步没更新：返回挂在这次请求末尾的提醒（并重新计数，
    /// 还不更新就再隔几步提醒一次）。模型干到一半常常忘了勾清单，只靠系统提示词里的一句不够。
    fn todo_nudge(&self, session: &str) -> Option<String> {
        let mut sessions = self.sessions.lock().unwrap();
        let state = sessions.get_mut(session)?;
        if state.agent.is_some()
            || state.steps_since_todo < tools::TODO_REMIND_STEPS
            || !tools::todos_open(&state.todos)
        {
            return None;
        }
        state.steps_since_todo = 0;
        Some(tools::todo_reminder(&state.todos))
    }

    /// 本轮结束：清掉自己的标志（取消后可能已经开始了新的一轮）。
    /// 还没送出去的插话也在这里丢掉——界面的输入框已经清空，只能明确告诉用户重发，
    /// 留着会在下一轮被当成新消息「幽灵注入」。
    fn finish(&self, session: &str, flag: &Arc<AtomicBool>) {
        let dropped = {
            let mut sessions = self.sessions.lock().unwrap();
            match sessions.get_mut(session) {
                Some(state) if state.turn.as_ref().is_some_and(|f| Arc::ptr_eq(f, flag)) => {
                    state.turn = None;
                    state.pending_steer.drain(..).count()
                }
                _ => 0,
            }
        };
        if dropped > 0 {
            self.emit(
                Some(session),
                KernelEvent::Notice(format!(
                    "这一轮已经结束，{dropped} 条插话没能送到模型，请重新发送。"
                )),
            );
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
        // web_search 的 DeepSeek 后端复用这个档案的 key（界面按端点挑好填进来）
        websearch::set_key_hint(config.provider.deepseek_key.clone());
        // 这个内核的编号（后台命令表按它分组）
        let kernel_id = NEXT_KERNEL_ID.fetch_add(1, Ordering::Relaxed);
        let mcp = {
            let sink = sink.clone();
            mcp::McpHub::new(config.mcp_config, config.cwd.clone(), move |servers| {
                sink(RoutedEvent { session: None, event: KernelEvent::McpStatus { servers } })
            })
        };
        mcp.set_extra(plugin_servers(&config.plugins));
        // web_search 的 key 文件和搜索后端的选择都存数据目录，按规则文件的位置推
        if let Some(dir) = config.rules_path.as_ref().and_then(|p| p.parent()) {
            websearch::set_data_dir(dir.to_path_buf());
        } else if let Ok(dir) = claudesktop_core::default_data_dir() {
            websearch::set_data_dir(dir);
        }
        let inner = Arc::new(Inner {
            provider: make_provider(&config.provider),
            provider_config: config.provider.clone(),
            default_model: Mutex::new(config.provider.model.clone()),
            sink,
            agent_name: agent_name.clone(),
            default_cwd: config.cwd,
            user_instructions: config.user_instructions,
            max_steps,
            session_titles: config.session_titles,
            context_windows: Mutex::new(config.context_windows),
            no_vision_models: Mutex::new(config.no_vision_models),
            sessions: Mutex::default(),
            next_session: AtomicU64::new(1),
            rules: Mutex::new(PermissionRules::load(config.rules_path)),
            mcp,
            plugins: Mutex::new(Arc::new(config.plugins)),
            pending: Mutex::default(),
            next_request: AtomicU64::new(1),
            kernel_id,
            memory_path: config.memory_path,
            memory_suggest: config.memory_suggest,
            auto_mode: config.auto_mode.map(|config| AutoMode {
                config,
                denials: Mutex::default(),
                provider: Mutex::default(),
            }),
            chat_model: config.chat_model,
            subagent_model: config.subagent_model,
            chat_web: config.chat_web,
            chat_instructions: config.chat_instructions,
            hibernation_dir: config.hibernation_dir,
        });
        inner.emit(None, KernelEvent::Status(format!("自研内核: {}", config.provider.base_url)));
        inner.emit(None, KernelEvent::Connected { agent_name, auth_methods: Vec::new() });
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
        // 首次连接在后台列工具并缓存；已有有效缓存的服务器等首次调用再连。
        inner.mcp.reload_lazy();
        Ok(Self { inner })
    }

    /// 新建会话（`history` 有内容 = 续接），报告 `Started`。
    fn open_session(&self, token: u64, spec: SessionSpec, history: history::Rebuilt) {
        let n = self.inner.next_session.fetch_add(1, Ordering::Relaxed);
        let session_id = format!("builtin-{}-{n}", std::process::id());
        let kind = spec.kind;
        let cwd = spec.cwd.unwrap_or_else(|| self.inner.default_cwd.clone());
        let resumed = !history.messages.is_empty() || history.summary.is_some();
        // 聊天会话：不读项目配置（模型、权限规则、MCP、插件范围都不读），
        // 说明换成用户的「聊天说明」，不放技能
        let chat = kind == SessionKind::Chat;
        // 项目级配置（`.claudesktop/settings.json`）：模型、说明文件、停用的 MCP、权限规则
        let project = if chat { None } else { claudesktop_core::project::load(&cwd) };
        for warning in project.as_ref().map(|p| p.warnings.iter()).into_iter().flatten() {
            self.inner.emit(Some(&session_id), KernelEvent::Notice(warning.clone()));
        }
        if let Some(project) = &project
            && !project.permissions.is_empty()
            && let Some(root) = claudesktop_core::project::shared_root(&cwd)
        {
            // 项目里写的权限规则并进 permissions.json（集合语义，去重后落盘）；
            // 按共享根记（worktree 里就是主仓库根），主目录和 worktree 共用一份。
            // 键必须和判断时查的键一致（`permissions.rs` 的 `matching` 会查共享根），
            // 否则 worktree 里并进来的规则判断时查不到。
            if let Some(path) = &self.inner.rules.lock().unwrap().path().map(PathBuf::from) {
                let file = std::fs::read_to_string(path).unwrap_or_else(|_| "{}".into());
                let file: Value = serde_json::from_str(&file).unwrap_or_else(|_| json!({}));
                if let Some(merged) =
                    claudesktop_core::project::merge_permissions(&file, &root, project)
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
        let instructions = if chat {
            // 聊天不带项目说明，换成设置里写的「聊天说明」；每轮重读
            instructions::chat_instructions(self.inner.chat_instructions.as_deref())
        } else {
            session_instructions(&cwd, self.inner.user_instructions.as_deref(), project.as_ref())
        };
        // 技能要 read_file 读文件，聊天没有
        let skills = if chat {
            Vec::new()
        } else {
            self.inner.skills(project.as_ref().and_then(|p| p.plugins_enabled.as_deref()))
        };
        // 项目指定的默认模型优先（全局默认其次）；聊天用设置里的聊天模型（空 = 全局默认）
        let model = if chat {
            self.inner
                .chat_model
                .clone()
                .unwrap_or_else(|| self.inner.default_model.lock().unwrap().clone())
        } else {
            project
                .as_ref()
                .and_then(|p| p.model.clone())
                .unwrap_or_else(|| self.inner.default_model.lock().unwrap().clone())
        };
        let mcp_disabled = project.as_ref().map(|p| p.mcp_disabled.clone()).unwrap_or_default();
        let plugins_enabled = project.as_ref().and_then(|p| p.plugins_enabled.clone());
        let mut state = SessionState {
            cwd,
            kind,
            web: !chat || self.inner.chat_web,
            model: model.clone(),
            // 会话级档位默认跟随档案，用户在下拉里选了才覆盖
            thinking: None,
            history: history.messages,
            history_storage: HistoryStorage::Resident,
            history_generation: 0,
            history_io: Arc::new(Mutex::new(())),
            history_activity: 0,
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
            memory_shown: Vec::new(),
            memory_used: Vec::new(),
            user_inputs: history.user_inputs,
            // 续接的会话接着用日志里最后那份清单
            todos: history.todos,
            steps_since_todo: 0,
            // 续接的会话要把加载过的工具还给模型，否则模型手里的工具和日志里的调用对不上
            loaded_tools: history.loaded_tools,
            // 步序从头算：加载过的工具先按「都没用过」排在后面，第一步用完就正常了
            step_seq: 0,
            tool_last_step: std::collections::HashMap::new(),
            pending_steer: Default::default(),
        };
        // 记住的东西一开始就带上（之后每轮重读）
        for warning in state.reload_memory_for_turn(self.inner.memory_path.as_deref()) {
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
                // 聊天没有权限模式：模式表为空，界面据此不画模式 chip
                modes: if chat {
                    Vec::new()
                } else {
                    permission_modes(self.inner.auto_mode.is_some())
                },
                current_mode: (!chat).then(|| MODE_DEFAULT.to_string()),
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

/// 被输出上限截断的工具调用：回给模型的错误（说清楚原因、要求重发完整参数）。
fn truncated_tool_message(name: &str) -> String {
    format!(
        "Tool call `{name}` was not executed: the reply hit the output token limit, so its \
         arguments may be truncated. Re-issue the call with complete arguments."
    )
}

/// 工具结果里的图片怎么送：模型 / 协议带不了就降级，能把图挪到一条 user 消息里就挪。
///
/// 返回要接在工具结果后面的 user 消息（0 或 1 条，不想要图就是空的）。
/// - 模型看不了图：图省掉，正文里补一句说明（界面已经靠 `ToolCallImages` 看过了）；
/// - OpenAI 兼容协议：`role: tool` 的 content 只能是字符串，图改挂一条 user 消息
///   （它是完全合法的多模态 user 消息，也不打乱 tool_calls 后面紧跟 tool 消息的顺序）；
/// - Anthropic / Responses：原生支持，原样留着（Responses 自己会拆成 input_image）。
fn images_as_user_message(
    results: &mut [ChatMessage],
    protocol: Protocol,
    model: &str,
    blind: bool,
) -> Vec<ChatMessage> {
    if !results.iter().any(|message| message.has_image_any()) {
        return Vec::new();
    }
    let mut images: Vec<ContentPart> = Vec::new();
    for message in results.iter_mut() {
        if !message.has_image_any() {
            continue;
        }
        if blind {
            message.replace_images_with_placeholder();
            message.append_text(
                "\n\n(The image was not sent: this model cannot read images. Say so if it matters, \
                 and ask the user to describe it.)",
            );
        } else if protocol == Protocol::OpenAiCompat {
            images.extend(message.take_images());
            message.append_text(
                "\n\n(The image itself follows in the next message so you can look at it.)",
            );
        }
    }
    if images.is_empty() {
        return Vec::new();
    }
    let mut parts = vec![ContentPart::Text(format!(
        "Image(s) returned by the tool calls above ({model} can read images):"
    ))];
    parts.extend(images);
    let mut message = ChatMessage::with_parts("user", parts);
    message.tool_images = true;
    vec![message]
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
        .flat_map(|p| {
            p.mcp_servers.iter().map(|s| {
                let mut config = mcp::ServerConfig::from_plugin(&p.name, s);
                config.cache_salt = p.version.clone().unwrap_or_default();
                config
            })
        })
        .collect()
}

/// 把一段补充上下文接到消息的文字部分末尾（没有文字部分时新起一段）。
/// 钩子上下文、`<memory-context>` 都这么接在用户消息尾部。
fn append_to_last_text(message: &mut ChatMessage, extra: &str) {
    match message.parts.iter_mut().find_map(|p| match p {
        ContentPart::Text(t) => Some(t),
        _ => None,
    }) {
        Some(text) => text.push_str(extra),
        None => message.parts.push(ContentPart::Text(extra.to_string())),
    }
}

impl Kernel for BuiltinKernel {
    fn kind(&self) -> &'static str {
        "builtin"
    }

    fn label(&self) -> String {
        self.inner.agent_name.clone()
    }

    fn new_session(&self, token: u64, spec: SessionSpec) -> anyhow::Result<()> {
        self.open_session(token, spec, history::Rebuilt::default());
        Ok(())
    }

    /// 进程内的会话不跨进程存活：`session_id` 无用，上下文从本地日志重建。
    fn resume_session(
        &self,
        token: u64,
        _session_id: &str,
        spec: SessionSpec,
        history: &[Record],
    ) -> anyhow::Result<()> {
        self.open_session(token, spec, history::rebuild(history));
        Ok(())
    }

    fn supports_chat(&self) -> bool {
        true
    }

    fn set_web(&self, session: &str, web: bool) -> anyhow::Result<()> {
        let mut sessions = self.inner.sessions.lock().unwrap();
        let state =
            sessions.get_mut(session).ok_or_else(|| anyhow::anyhow!("未知会话 {session}"))?;
        if state.kind != SessionKind::Chat {
            anyhow::bail!("只有聊天会话能关联网");
        }
        // 下一轮开头生效（进行中的一轮工具定义中途变了，有的端点会报错）
        state.web = web;
        Ok(())
    }

    fn send_prompt(
        &self,
        session: &str,
        prompt: claudesktop_core::UserPrompt,
    ) -> anyhow::Result<()> {
        let submitted_at = std::time::Instant::now();
        let flag = {
            let mut sessions = self.inner.sessions.lock().unwrap();
            let state =
                sessions.get_mut(session).ok_or_else(|| anyhow::anyhow!("未知会话 {session}"))?;
            if state.turn.is_some() {
                anyhow::bail!("上一轮还没结束");
            }
            let flag = Arc::new(AtomicBool::new(false));
            state.turn = Some(flag.clone());
            state.turns += 1;
            flag
        };
        let inner = self.inner.clone();
        let session = session.to_string();
        let worker_session = session.clone();
        let spawned = std::thread::Builder::new()
            .name("builtin-turn".into())
            .spawn(move || run_pending_prompt(inner, worker_session, prompt, flag, submitted_at));
        if let Err(err) = spawned {
            let mut sessions = self.inner.sessions.lock().unwrap();
            if let Some(state) = sessions.get_mut(&session) {
                state.turn = None;
                state.turns = state.turns.saturating_sub(1);
            }
            return Err(anyhow::anyhow!("spawn turn thread: {err}"));
        }
        Ok(())
    }

    fn cancel(&self, session: &str) -> anyhow::Result<()> {
        // 持锁置位：与 commit() 的判断互斥，取消后这一轮的结果不会再写进历史；
        // 没送出去的插话（输入框已清空）一并丢掉并提示，免得下一轮被幽灵注入
        let (cancelled, dropped) = {
            let mut sessions = self.inner.sessions.lock().unwrap();
            match sessions.get_mut(session) {
                Some(state) => match state.turn.take() {
                    Some(flag) => {
                        flag.store(true, Ordering::SeqCst);
                        (true, state.pending_steer.drain(..).count())
                    }
                    None => (false, 0),
                },
                None => (false, 0),
            }
        };
        if cancelled {
            self.inner
                .emit(Some(session), KernelEvent::TurnEnded { stop_reason: "cancelled".into() });
        }
        if dropped > 0 {
            self.inner.emit(
                Some(session),
                KernelEvent::Notice(format!(
                    "这一轮已停止，{dropped} 条插话没能送到模型，请重新发送。"
                )),
            );
        }
        Ok(())
    }

    fn resolve_permission(&self, request_id: u64, option_id: Option<String>) -> anyhow::Result<()> {
        self.resolve_permission_with(request_id, option_id, None)
    }

    /// 应答一次提问（`ask_user`）：`Reply::Question` 交给阻塞在 `Inner::ask_user` 里的那一轮。
    fn resolve_question(&self, request_id: u64, answer: QuestionAnswer) -> anyhow::Result<()> {
        let tx = self.inner.pending.lock().unwrap().remove(&request_id);
        match tx {
            Some(tx) => tx
                .send(Reply::Question(answer))
                .map_err(|_| anyhow::anyhow!("提问 {request_id} 已失效")),
            None => anyhow::bail!("提问 {request_id} 已失效"),
        }
    }

    /// 同 `resolve_permission`，另外带上「只写这些块」（自研内核的写文件 / 改文件认它）。
    fn resolve_permission_with(
        &self,
        request_id: u64,
        option_id: Option<String>,
        grant: Option<PartialGrant>,
    ) -> anyhow::Result<()> {
        let tx = self.inner.pending.lock().unwrap().remove(&request_id);
        match tx {
            Some(tx) => {
                let answer =
                    option_id.map(|option| Answer { option, hunks: grant.map(|g| g.hunks) });
                let _ = tx.send(Reply::Permission(answer));
                Ok(())
            }
            None => anyhow::bail!("权限请求 {request_id} 已失效"),
        }
    }

    /// 运行中插话：排进 `pending_steer`，由 `run_steps` 在步间安全点取走。
    /// 没有在跑的一轮时直接报错——那条路径该用 `send_prompt`。
    fn steer(&self, session: &str, prompt: claudesktop_core::UserPrompt) -> anyhow::Result<()> {
        let mut sessions = self.inner.sessions.lock().unwrap();
        let state =
            sessions.get_mut(session).ok_or_else(|| anyhow::anyhow!("未知会话 {session}"))?;
        if state.turn.is_none() {
            anyhow::bail!("这一轮已经结束了，用普通发送");
        }
        state.pending_steer.push_back(prompt);
        Ok(())
    }

    fn set_mode(&self, session: &str, mode_id: &str) -> anyhow::Result<()> {
        if !permission_modes(self.inner.auto_mode.is_some()).iter().any(|m| m.id == mode_id) {
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

    fn set_no_vision_models(&self, models: Vec<String>) {
        *self.inner.no_vision_models.lock().unwrap() = models;
    }

    /// 会话级思考档位：存进会话状态，之后每轮请求现取（见 `run_steps`）。
    /// `None` = 回到档案里的那份。会话还没建立时由界面记着，`Started` 之后再补一次。
    fn set_thinking(&self, session: &str, level: Option<&str>) -> anyhow::Result<()> {
        let level = level.map(parse_thinking_level).transpose()?;
        let mut sessions = self.inner.sessions.lock().unwrap();
        let state =
            sessions.get_mut(session).ok_or_else(|| anyhow::anyhow!("未知会话 {session}"))?;
        state.thinking = level;
        Ok(())
    }

    /// 手动压缩：自成一轮（占着 `turn`，期间不能发消息、可以取消），全部对话进摘要。
    fn supports_rewind(&self) -> bool {
        true
    }

    /// 回退：上下文换成日志折算出的样子（工具调用只剩摘要，和续接历史会话一样）。
    fn rewind(&self, session: &str, history: &[Record]) -> anyhow::Result<()> {
        let rebuilt = history::rebuild(history);
        let io = self.inner.history_io(session)?;
        let _guard = io.lock().unwrap();
        self.inner.wake_history_locked(session, Duration::ZERO)?;
        let mut sessions = self.inner.sessions.lock().unwrap();
        let state =
            sessions.get_mut(session).ok_or_else(|| anyhow::anyhow!("未知会话 {session}"))?;
        if state.turn.is_some() {
            anyhow::bail!("这一轮还没结束");
        }
        state.history = rebuilt.messages;
        state.summary = rebuilt.summary;
        // 撤回的消息和答案里的授权跟着作废
        state.user_inputs = rebuilt.user_inputs;
        // 清单回到回退点那时的样子
        state.todos = rebuilt.todos;
        // 回退掉的轮次里 `tool_search` 加载的工具也撤掉，和重启后续接看到的一致
        state.loaded_tools = rebuilt.loaded_tools;
        state.steps_since_todo = 0;
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
            if matches!(state.history_storage, HistoryStorage::Resident) && state.history.is_empty()
            {
                anyhow::bail!("还没有可以压缩的对话");
            }
            let flag = Arc::new(AtomicBool::new(false));
            state.turn = Some(flag.clone());
            state.turns += 1;
            (flag, state.model.clone())
        };
        let inner = self.inner.clone();
        let session = session.to_string();
        let worker_session = session.clone();
        let spawned = std::thread::Builder::new()
            .name("builtin-compact".into())
            .spawn(move || run_compact(inner, worker_session, model, flag));
        if let Err(err) = spawned {
            let mut sessions = self.inner.sessions.lock().unwrap();
            if let Some(state) = sessions.get_mut(&session) {
                state.turn = None;
                state.turns = state.turns.saturating_sub(1);
            }
            return Err(anyhow::anyhow!("spawn compact thread: {err}"));
        }
        Ok(())
    }

    fn reconnect_mcp(&self, server: &str) {
        self.inner.mcp.reconnect(server);
    }

    fn clear_mcp_auth(&self, server: &str) -> anyhow::Result<()> {
        self.inner.mcp.clear_auth(server).map_err(|e| anyhow::anyhow!(e))
    }

    fn hibernate_session(&self, session: &str) -> anyhow::Result<()> {
        self.inner.hibernate_history(session)
    }

    fn wake_session(&self, session: &str) -> anyhow::Result<()> {
        let result = self.inner.wake_history(session);
        if let Err(err) = &result {
            self.inner.emit(Some(session), KernelEvent::Notice(format!("{err:#}")));
        }
        result
    }

    fn history_stats(&self) -> claudesktop_core::KernelHistoryStats {
        self.inner.history_stats()
    }

    /// 会话结束（切走 / 关掉）时把它起的后台命令停掉：模型再也读不到输出了。
    fn close_session(&self, session: &str) {
        bgtasks::stop_session(session);
        self.inner.close_history(session);
    }

    fn stop_background_task(&self, id: &str) {
        let _ = bgtasks::stop(id);
    }

    fn reload_mcp(&self) {
        self.inner.mcp.reload();
    }

    fn reclaim_idle_mcp(&self) {
        self.inner.mcp.reclaim_idle();
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
        self.inner.shutdown_history();
    }
}

#[cfg(test)]
mod tests {
    use super::prompt::{Context, prompt_messages, prompt_sections, system_prompt};
    use super::*;

    /// 环境变量单测一律走闭包注入：并行测试里 `set_var` 会互相污染（且 Rust 2024 下是 unsafe）。
    fn fake_env(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> {
        let map: std::collections::HashMap<String, String> =
            pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect();
        move |key| map.get(key).cloned()
    }

    #[test]
    fn protocol_env_prefers_the_new_name() {
        let env = fake_env(&[("CLAUDESKTOP_PROTOCOL", "anthropic")]);
        assert_eq!(protocol_from_env(&env), Protocol::Anthropic);
    }

    #[test]
    fn protocol_env_still_accepts_the_legacy_name() {
        let env = fake_env(&[("CLAUDEDESKTOP_PROTOCOL", "anthropic")]);
        assert_eq!(protocol_from_env(&env), Protocol::Anthropic);
    }

    #[test]
    fn protocol_env_falls_back_to_openai_compat() {
        assert_eq!(protocol_from_env(&fake_env(&[])), Protocol::OpenAiCompat);
        let other = fake_env(&[("CLAUDESKTOP_PROTOCOL", "whatever")]);
        assert_eq!(protocol_from_env(&other), Protocol::OpenAiCompat);
    }

    #[test]
    fn from_env_reads_the_provider_config() {
        let config = BuiltinConfig::from_env_with(fake_env(&[
            ("CLAUDESKTOP_BASE_URL", "http://127.0.0.1:8045/v1"),
            ("CLAUDESKTOP_MODEL", "demo-model"),
            ("CLAUDESKTOP_API_KEY", "sk-test"),
            ("CLAUDESKTOP_PROTOCOL", "anthropic"),
        ]))
        .unwrap();
        assert_eq!(config.provider.protocol, Protocol::Anthropic);
        assert_eq!(config.provider.base_url, "http://127.0.0.1:8045/v1");
        assert_eq!(config.provider.model, "demo-model");
        assert_eq!(config.provider.api_key.as_deref(), Some("sk-test"));
    }

    #[test]
    fn from_env_requires_base_url_and_model() {
        let only_model = fake_env(&[("CLAUDESKTOP_MODEL", "m")]);
        assert!(BuiltinConfig::from_env_with(only_model).is_err());
        let only_url = fake_env(&[("CLAUDESKTOP_BASE_URL", "http://x/v1")]);
        assert!(BuiltinConfig::from_env_with(only_url).is_err());
    }

    #[test]
    fn from_env_treats_a_blank_value_as_unset() {
        let blank =
            fake_env(&[("CLAUDESKTOP_BASE_URL", "http://x/v1"), ("CLAUDESKTOP_MODEL", "   ")]);
        assert!(BuiltinConfig::from_env_with(blank).is_err());
    }

    #[derive(Default)]
    struct AuthorizationProbe(Mutex<Vec<String>>);

    impl ChatProvider for AuthorizationProbe {
        fn stream_chat(
            &self,
            _: &str,
            messages: &[ChatMessage],
            _: &[ToolSpec],
            _: &dyn Fn() -> bool,
            on_event: &mut dyn FnMut(StreamEvent),
        ) -> anyhow::Result<provider::StreamOutcome> {
            let input = messages[1].text().unwrap();
            let verdict = if input.contains("Only run echo confirmed") {
                r#"{"decision":"allow","reason":"confirmed action"}"#
            } else {
                r#"{"decision":"deny","reason":"[Scope] missing confirmation"}"#
            };
            self.0.lock().unwrap().push(input);
            on_event(StreamEvent::Text(verdict.into()));
            Ok(provider::StreamOutcome { completed: true, ..Default::default() })
        }

        fn list_models(&self) -> anyhow::Result<Vec<ModelInfo>> {
            Ok(Vec::new())
        }
    }

    #[test]
    fn auto_mode_classifies_with_question_answers_live_and_after_resume() {
        let cwd = std::env::temp_dir();
        let mut provider = ProviderConfig { model: "test-classifier".into(), ..Default::default() };
        provider.params.thinking = suggest::short_thinking(provider.protocol, &provider.model);
        let mut config = BuiltinConfig::new(provider.clone(), cwd.clone());
        config.auto_mode =
            Some(AutoModeConfig { model: Some(provider.clone()), ..Default::default() });
        config.rules_path = None;
        config.user_instructions = None;
        config.mcp_config = None;
        config.memory_path = None;
        config.hibernation_dir = None;
        let (tx, rx) = mpsc::channel();
        let kernel = BuiltinKernel::spawn(
            config,
            Arc::new(move |event| {
                let _ = tx.send(event);
            }),
        )
        .unwrap();
        let next = |pred: fn(&KernelEvent) -> bool| loop {
            let record = rx.recv_timeout(Duration::from_secs(2)).unwrap();
            if pred(&record.event) {
                break record;
            }
        };
        let inner = &kernel.inner;
        let probe = Arc::new(AuthorizationProbe::default());
        *inner.auto_mode.as_ref().unwrap().provider.lock().unwrap() =
            Some((provider, probe.clone()));

        let mut records = vec![Record::user("What could we run?")];
        let resume = |records: &[Record]| {
            kernel.resume_session(1, "old", SessionSpec::code(Some(cwd.clone())), records).unwrap();
            let sid = next(|e| matches!(e, KernelEvent::Started { .. })).session.unwrap();
            kernel.set_mode(&sid, MODE_AUTO).unwrap();
            sid
        };
        let sid = resume(&records);
        let action = Action::Shell {
            command: "echo confirmed".into(),
            timeout: Duration::from_secs(1),
            cwd: cwd.clone(),
        };
        let authorize = |sid: &str| inner.authorize(sid, &cwd, &action, "Shell", "s1", &|| false);
        for _ in 0..MAX_DENIAL_STREAK {
            assert!(matches!(authorize(&sid), Verdict::AutoDeny { .. }));
        }
        assert!(inner.denials_give_up(&sid).is_some());
        assert!(probe.0.lock().unwrap().iter().all(|input| !input.contains("ask_user answer")));

        let answer = QuestionAnswer::Choices { choices: vec![0] };
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        let asked = std::thread::scope(|scope| {
            let pending = scope.spawn(|| {
                inner.ask_user(
                    &sid,
                    r#"{"questions":[{
                "question":"Which command?",
                "options":[{"label":"Run it","description":"Only run echo confirmed"},
                           {"label":"Unselected command"}]
            }]}"#,
                    &|| std::time::Instant::now() >= deadline,
                )
            });
            let asked = next(|e| matches!(e, KernelEvent::UserQuestion { .. })).event;
            let KernelEvent::UserQuestion { id, .. } = asked else { unreachable!() };
            kernel.resolve_question(id, answer.clone()).unwrap();
            assert!(pending.join().unwrap().ok);
            asked
        });
        assert!(matches!(authorize(&sid), Verdict::Allow));
        let live = probe.0.lock().unwrap().last().unwrap().clone();
        assert!(live.contains("[ask_user answer 2]") && live.contains("Which command?"), "{live}");
        assert!(
            !live.contains("Unselected command") && !live.contains("missing confirmation"),
            "{live}"
        );

        let KernelEvent::UserQuestion { id, .. } = asked else { unreachable!() };
        records.extend([Record::Event { event: asked }, Record::QuestionAnswered { id, answer }]);
        let resumed = resume(&records);
        assert!(matches!(authorize(&resumed), Verdict::Allow));
        assert_eq!(probe.0.lock().unwrap().last().unwrap(), &live);

        records.push(Record::Rewind {
            turn: 0,
            conversation: true,
            files: 0,
            prompt: String::new(),
        });
        kernel.rewind(&resumed, &records).unwrap();
        assert!(matches!(authorize(&resumed), Verdict::AutoDeny { .. }));
        assert!(!probe.0.lock().unwrap().last().unwrap().contains("ask_user answer"));
    }

    /// 骨架阶段的分节重构必须逐字节等于原来的拼法：节之间用空行连起来，每节自身不带
    /// 前导 / 尾随空行，所以拼回去不会多出或吃掉空行（见 `prompt_sections`）。
    #[test]
    fn prompt_sections_join_back_into_the_prompt() {
        let cwd = Path::new("/tmp/proj");
        let empty =
            || Context { instructions: &[], skills: &[], memory_project: &[], memory_global: &[] };
        let sections = prompt_sections(cwd, MODE_DEFAULT, false, None, None, empty());
        assert_eq!(sections.len(), 1, "没有背景材料时只有 preamble");
        assert_eq!(sections[0].0, "preamble");
        assert!(sections[0].1.contains("Working directory: /tmp/proj"));
        // 节名唯一（阶段 2 按名字算差异，重名会互相覆盖）
        let names: std::collections::HashSet<_> = sections.iter().map(|(name, _)| *name).collect();
        assert_eq!(names.len(), sections.len());
        assert_eq!(
            system_prompt(cwd, MODE_DEFAULT, false, None, None, empty()),
            sections.into_iter().map(|(_, text)| text).collect::<Vec<_>>().join("\n\n")
        );
    }

    /// 模式、续接、技能、摘要各自多出一节，顺序固定（越稳定的越靠前）。
    #[test]
    fn prompt_sections_grow_in_a_stable_order() {
        let cwd = Path::new("/tmp/proj");
        let instructions = vec![instructions::Instruction {
            path: PathBuf::from("/tmp/proj/AGENTS.md"),
            text: "用中文".into(),
        }];
        let skills = vec![Skill {
            name: "demo".into(),
            description: "说明".into(),
            path: PathBuf::from("/tmp/proj/skills/demo"),
        }];
        let ctx = Context {
            instructions: &instructions,
            skills: &skills,
            memory_project: &[],
            memory_global: &[],
        };
        let sections = prompt_sections(cwd, MODE_PLAN, true, Some("## 摘要"), None, ctx);
        let names: Vec<_> = sections.iter().map(|(name, _)| *name).collect();
        assert_eq!(
            names,
            ["preamble", "restored", "project", "skills", "plan", "summary"],
            "节的顺序就是缓存前缀的顺序，改顺序等于让所有会话的缓存失效一次；易变节（摘要、记忆）必须在末尾"
        );
        // 每节自身不带空行，拼起来才不会出现三个空行
        for (name, text) in &sections {
            assert!(!text.starts_with('\n'), "{name} 以空行开头");
            assert!(!text.ends_with('\n'), "{name} 以空行结尾");
        }
    }

    /// 稳定前缀单独一条 system 消息：拼回去等于整段提示词，切模式 / 压缩只动后一条。
    #[test]
    fn prompt_messages_keep_the_stable_prefix_bytes() {
        let cwd = Path::new("/tmp/proj");
        let ctx =
            || Context { instructions: &[], skills: &[], memory_project: &[], memory_global: &[] };
        // 没有易变节：一条，和整段提示词相同
        let plain = prompt_sections(cwd, MODE_DEFAULT, false, None, None, ctx());
        let one = prompt_messages(&plain);
        assert_eq!(one, [system_prompt(cwd, MODE_DEFAULT, false, None, None, ctx())]);
        // 有计划 / 摘要：拆成两条，第一条仍是那条稳定前缀，拼回去逐字节不变
        let volatile = prompt_sections(cwd, MODE_PLAN, false, Some("## 摘要"), None, ctx());
        let two = prompt_messages(&volatile);
        assert_eq!(two.len(), 2);
        assert_eq!(two[0], one[0], "前面的节不变，缓存断点才命中");
        assert!(two[1].contains("Plan mode"));
        assert_eq!(
            two.join("\n\n"),
            system_prompt(cwd, MODE_PLAN, false, Some("## 摘要"), None, ctx())
        );
    }

    /// 延迟工具按预算淘汰：最近用过的优先，超预算的旧工具本轮不声明；
    /// 但至少放行一个，免得模型刚搜到的工具下一步就看不见、陷入反复重搜。
    #[test]
    fn deferred_tools_fit_the_budget_and_keep_the_newest() {
        let loaded: Vec<String> = ["a", "b", "c"].iter().map(|s| s.to_string()).collect();
        let mut last = std::collections::HashMap::new();
        last.insert("a".to_string(), 1u64);
        last.insert("b".to_string(), 5u64);
        last.insert("c".to_string(), 9u64);
        // 每个 900 token：两个以内放得进 2000
        let active = select_within_budget(&loaded, &last, |_| 900);
        assert_eq!(active, ["c", "b"], "最近用过的排前，超预算的淘汰");

        // 都很小：全放
        let all = select_within_budget(&loaded, &last, |_| 10);
        assert_eq!(all, ["c", "b", "a"]);

        // 单个大工具就超预算：仍要放行，否则模型搜到了也看不见
        let huge = select_within_budget(&loaded, &last, |_| 5_000);
        assert_eq!(huge, ["c"]);
    }

    /// 没记录过使用步序的工具（续接后第一步）排在后面，但不至于被全砍掉。
    #[test]
    fn deferred_tools_without_history_rank_last() {
        let loaded: Vec<String> = ["fresh", "stale"].iter().map(|s| s.to_string()).collect();
        let mut last = std::collections::HashMap::new();
        last.insert("stale".to_string(), 3u64);
        let active = select_within_budget(&loaded, &last, |_| 100);
        assert_eq!(active, ["stale", "fresh"]);
    }

    #[test]
    fn hibernation_keeps_session_settings_and_auxiliary_state() {
        let root = std::env::temp_dir()
            .join(format!("claudesktop-hibernate-state-{}", std::process::id()));
        let cwd = root.join("cwd");
        std::fs::create_dir_all(&cwd).unwrap();
        let events = Arc::new(Mutex::new(Vec::new()));
        let captured = events.clone();
        let sink: EventSink = Arc::new(move |event| captured.lock().unwrap().push(event));
        let mut config = BuiltinConfig::new(
            ProviderConfig { model: "session-model".into(), ..Default::default() },
            cwd,
        );
        config.rules_path = None;
        config.user_instructions = None;
        config.mcp_config = None;
        config.memory_path = None;
        config.hibernation_dir = Some(root.join("snapshots"));
        let kernel = BuiltinKernel::spawn(config, sink).unwrap();
        kernel.new_session(1, SessionSpec::default()).unwrap();
        let sid = events
            .lock()
            .unwrap()
            .iter()
            .find_map(|event| match &event.event {
                KernelEvent::Started { session_id, .. } => Some(session_id.clone()),
                _ => None,
            })
            .unwrap();
        let history =
            vec![ChatMessage::new("user", "完整历史"), ChatMessage::new("assistant", "回答")];
        let todos = vec![PlanEntryView {
            content: "保留待办".into(),
            priority: "high".into(),
            status: "in_progress".into(),
        }];
        {
            let mut sessions = kernel.inner.sessions.lock().unwrap();
            let state = sessions.get_mut(&sid).unwrap();
            state.history = history.clone();
            state.mode = MODE_PLAN.into();
            state.model = "session-model-override".into();
            state.thinking = Some(ThinkingLevel::High);
            state.web = false;
            state.summary = Some("已有摘要".into());
            state.measured = Some((123, 1));
            state.todos = todos.clone();
            state.loaded_tools = vec!["mcp__demo__tool".into()];
        }

        kernel.inner.hibernate_history(&sid).unwrap();
        assert!(kernel.inner.sessions.lock().unwrap().get(&sid).unwrap().history.is_empty());
        kernel.inner.wake_history(&sid).unwrap();
        let sessions = kernel.inner.sessions.lock().unwrap();
        let state = sessions.get(&sid).unwrap();
        assert_eq!(state.history, history);
        assert_eq!(state.mode, MODE_PLAN);
        assert_eq!(state.model, "session-model-override");
        assert_eq!(state.thinking, Some(ThinkingLevel::High));
        assert!(!state.web);
        assert_eq!(state.summary.as_deref(), Some("已有摘要"));
        assert_eq!(state.measured, Some((123, 1)));
        assert_eq!(state.todos, todos);
        assert_eq!(state.loaded_tools, ["mcp__demo__tool"]);
        assert!(!state.restored);
        assert_eq!(
            events
                .lock()
                .unwrap()
                .iter()
                .filter(|event| matches!(event.event, KernelEvent::Started { .. }))
                .count(),
            1,
            "恢复不重新开会话"
        );
        drop(sessions);
        drop(events);
        let _ = std::fs::remove_dir_all(root);
    }
}
