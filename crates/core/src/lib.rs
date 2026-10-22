//! # claudesktop-core
//!
//! 内核边界层：Claude 风格桌面 harness 的「可更换内核」抽象。
//!
//! M1 范围：
//! - [`Kernel`] trait：UI 唯一依赖的内核接口（发送 / 取消 / 关闭）。
//! - [`KernelEvent`]：内核 → UI 的单向事件流，覆盖 ACP `session/update`
//!   全部对 UI 有意义的变体（流式文本 / 思考 / 工具调用 / 计划 / 权限 / fs）。
//! - [`AcpKernel`]：通过 Agent Client Protocol（stdio JSON-RPC）驱动任意
//!   ACP agent 子进程的实现，见 [`acp`] 模块。
//!
//! 设计约束：本 crate 不依赖任何 UI 框架；事件经 [`EventSink`] 回调
//! （内核线程调用），指令经 channel（UI 线程投递）。

mod acp;
pub mod agents;
pub mod attach;
pub mod catalog;
pub mod checkpoint;
mod diff;
pub mod git;
pub mod history_search;
pub mod image_prep;
mod json_error;
pub mod math;
pub mod memory;
pub mod plugin;
pub mod plugin_git;
mod process_guard;
pub mod project;
pub mod review;
mod store;

pub use acp::{AcpKernel, AcpKernelConfig};
pub use agents::{AgentPreset, DetectState, Detected};
pub use diff::{
    DiffHunk, DiffLineView, DiffView, HunkRange, hunk_ranges, hunks_with_context, splice_ranges,
};
pub use json_error::json_error_text;
pub use process_guard::kill_children_on_exit;
pub use store::{
    Record, SessionKind, SessionLog, SessionMeta, SessionStore, WorktreeMeta, default_data_dir,
    effective, read_log,
};

pub use plugin::Plugin;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::sync::Arc;

/// 计划条目的 UI 投影（协议无关）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlanEntryView {
    /// 条目内容（人类可读）
    pub content: String,
    /// 优先级（high / medium / low）
    pub priority: String,
    /// 状态（pending / in_progress / completed）
    pub status: String,
}

/// agent 上报的会话模式（ACP `modes`）。含义由 agent 决定：
/// Claude 适配器是权限模式（default / acceptEdits / plan …），
/// pi 适配器是思考强度（off / low / medium / high …）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModeView {
    pub id: String,
    pub name: String,
    pub description: Option<String>,
}

/// 权限请求里的一个可选项（协议无关的 UI 投影）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PermissionOptionView {
    /// 选项 ID（回传给内核）
    pub id: String,
    /// 展示名（如「允许一次」）
    pub name: String,
    /// 种类：allow_once / allow_always / reject_once / reject_always
    pub kind: String,
}

/// 按块部分同意：这次只放行这些块，其余块不写盘（等用户先看）。
///
/// 只对自研内核的写文件 / 改文件工具开放（其它内核的工具结果不归我们写，做不到）。
/// 写 / 改文件一次只动一个文件，块编号就是那个文件的。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PartialGrant {
    /// 放行的块编号（[`PermissionHunks`] 里的 `index`）
    pub hunks: Vec<u32>,
}

/// 权限请求里「可以按块同意」的那部分：这次要写哪些文件、每个文件的块。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PermissionHunks {
    /// 一个文件一项（写 / 改都只有一个文件，留成列表是为了以后的多文件工具）
    pub files: Vec<HunkFile>,
}

/// 按块同意里的一个文件。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HunkFile {
    /// 显示用路径（和 [`DiffView::path`] 一致，界面上对得上）
    pub path: String,
    /// 这次改动在整个文件里的块（带上下文行，够画出来）
    pub hunks: Vec<crate::DiffHunk>,
    /// 这次要写的内容是不是覆盖整个文件（新建 / 整份重写）；False = 只替换了几处
    pub whole_file: bool,
}

/// 内核 → UI 的事件流。
///
/// 这是「agent UI 事件总线」的第一个版本：类型集合直接对齐
/// ACP v1 `SessionUpdate` 中 UI 真正要渲染的内容，未来内核
/// （自研引擎、其他协议）都映射到这组事件上。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", content = "data", rename_all = "snake_case")]
pub enum KernelEvent {
    /// 生命周期 / 诊断文本（连接建立、协议协商结果等）
    Status(String),
    /// 会话里显示的一行提示（插件钩子出错等）；不进日志
    Notice(String),
    /// 与 agent 的连接已建立（`initialize` 完成），可以开始创建会话
    Connected {
        /// agent 自报的名称（`agentInfo.name`）
        agent_name: String,
    },
    /// 一个会话创建完成（`session/new` 响应到达）；事件的 `session` 即其 ID
    Started {
        /// 创建会话时调用方给的令牌，用来把响应对回发起方
        token: u64,
        /// agent 自报的名称（`agentInfo.name`）
        agent_name: String,
        /// 会话 ID
        session_id: String,
        /// 当前模型（`config_options` 里 category = model 的当前值）
        model: Option<String>,
        /// agent 上报的可用模式；空 = agent 不支持 `session/set_mode`
        modes: Vec<ModeView>,
        /// 当前模式 ID
        current_mode: Option<String>,
        /// 续接了已有会话（agent 保有之前的上下文）；false = 全新会话
        #[serde(default)]
        resumed: bool,
    },
    /// agent 回复的流式文本块（追加渲染）
    AgentMessageChunk { text: String },
    /// agent 思考过程的流式文本块
    AgentThoughtChunk { text: String },
    /// 用户消息回显块（部分 agent 会流式回显用户输入）
    UserMessageChunk { text: String },
    /// 新工具调用开始
    ToolCallStarted {
        /// 工具调用 ID（后续 update 按此关联）
        id: String,
        /// 人类可读标题
        title: String,
        /// 初始状态（pending / in_progress / …）
        status: String,
        /// 工具种类（read / edit / execute / search / fetch / other …）
        kind: String,
        /// 子 agent 的调用：派出它的那个 task 调用的 ID（界面缩进在它下面；不进重建的上下文）
        #[serde(default, skip_serializing_if = "Option::is_none")]
        parent: Option<String>,
    },
    /// 子 agent（自研内核的 `task` 工具）开始干活：派它的 task 调用 ID、角色名、交给它的任务说明
    /// （界面在子任务详情页显示；不进主会话的上下文）
    SubagentStarted { task: String, agent: String, prompt: String },
    /// 子 agent 的回复 / 思考文本块（`thought` 为真是思考）；同上，只给子任务详情页
    SubagentChunk {
        task: String,
        text: String,
        #[serde(default)]
        thought: bool,
    },
    /// 工具调用更新（状态推进、原始输出）
    ToolCallUpdated {
        id: String,
        title: Option<String>,
        status: Option<String>,
        /// 输出摘要：能从结构化输出里取到文本就取文本，否则是 JSON 文本；已截断
        raw_output: Option<String>,
        /// 输出正文预览（界面点开工具行时显示），见 [`output_preview`]
        #[serde(default, skip_serializing_if = "Option::is_none")]
        output: Option<String>,
    },
    /// 运行中的工具到目前为止的输出（预览，见 [`output_tail_preview`]）：只给界面看，不进日志，
    /// 跑完后由带最终输出的 `ToolCallUpdated` 替换
    ToolCallProgress { id: String, output: String },
    /// 工具调用涉及的文件改动（可能在执行前作为预览发出，同一路径再次发出时替换）
    ToolCallDiff { id: String, diff: DiffView },
    /// 计划整体替换（ACP 语义：每次都是全量列表）
    PlanReplaced { entries: Vec<PlanEntryView> },
    /// 权限请求已按策略自动应答（`auto_approve` 开启时：allow_once 优先）
    PermissionAuto {
        title: String,
        chosen: String,
        /// 触发请求的工具调用 ID（界面把结果并进那一行）；旧日志没有
        #[serde(default, skip_serializing_if = "Option::is_none")]
        tool: Option<String>,
    },
    /// agent 请求权限，等待 UI 经 [`Kernel::resolve_permission`] 应答
    PermissionRequest {
        /// 请求 ID（应答时回传）
        id: u64,
        /// 触发请求的工具调用标题
        title: String,
        options: Vec<PermissionOptionView>,
        /// 触发请求的工具调用 ID（应答后界面把结果并进那一行）；旧日志没有
        #[serde(default, skip_serializing_if = "Option::is_none")]
        tool: Option<String>,
        /// 这次改动可以按块部分同意：等这行的块（编号同 [`DiffView`](crate::DiffView) 的
        /// `'~'` 分隔段，和逐块审查共用一份划分）。None = 不支持按块（ACP、命令类工具、旧日志）
        #[serde(default, skip_serializing_if = "Option::is_none")]
        hunks: Option<PermissionHunks>,
    },
    /// 客户端代执行的 fs 读请求结果
    FsRead { path: String, ok: bool },
    /// 客户端代执行的 fs 写请求结果
    FsWrite { path: String, ok: bool },
    /// 一轮对话结束（`session/prompt` 响应到达）
    TurnEnded { stop_reason: String },
    /// 本轮 token 用量（provider 报告时才有）
    UsageUpdated {
        input_tokens: u64,
        output_tokens: u64,
        /// 其中思考消耗的 token（不报告为 0）
        reasoning_tokens: u64,
        /// 本轮最后一次请求结束时上下文里的 token（输入 + 输出），即下一轮大约要带上的量；
        /// 0 = 不知道
        #[serde(default)]
        context_tokens: u64,
        /// 本轮输入里命中提示缓存的部分（包含在 `input_tokens` 里；不报告为 0）
        #[serde(default)]
        cache_read_tokens: u64,
    },
    /// 创建会话失败（`session/new` 报错）；连接仍然可用
    SessionFailed { token: u64, message: String },
    /// 本轮提示失败（如未登录、模型报错）；连接仍然可用
    PromptFailed { message: String },
    /// 端点返回瞬态错误（限流 / 5xx），内核等 `delay_ms` 后自动第 `attempt` 次重试；
    /// 本轮还在进行，界面据此显示倒计时
    Retrying {
        /// HTTP 状态码（429 为限流）
        status: u16,
        attempt: usize,
        delay_ms: u64,
        /// 上一次的错误（「HTTP 429: …」）
        reason: String,
    },
    /// 开始压缩上下文（把较早的对话总结成摘要）：自动压缩发生在一轮中间，手动压缩自成一轮
    Compacting,
    /// 压缩完成：之后的请求带摘要而不是原来的对话
    Compacted {
        /// 模型写的摘要（Markdown）
        summary: String,
        /// 压缩后上下文的估算 token（输入框旁的用量随之回落）；0 = 不知道
        #[serde(default)]
        context_tokens: u64,
    },
    /// 压缩失败；自动压缩时这一轮照常继续
    CompactFailed { message: String },
    /// 内核致命错误（连接断开、初始化失败等，内核不再可用）
    Fatal(String),
    /// 内核改了一个文件：改之前 / 之后的完整内容（None = 文件不存在）。
    /// 给 UI 存检查点用（见 [`checkpoint`]）；内容可能很大，不原样进日志，由 UI 转成 `Record::FileChanged`
    FileChanged { path: String, before: Option<String>, after: Option<String> },
    /// MCP 服务器的连接状态（连接级事件，配置变了 / 某个服务器状态变了就整份重发）
    McpStatus { servers: Vec<McpServerView> },
    /// 内核用模型给会话起了个新标题（第一轮结束后）；界面更新侧栏，失败不发
    TitleGenerated { title: String },
    /// 后台命令的状态变了（启动 / 结束 / 被停掉）：整份任务列表（最近的在前）
    BackgroundTasks { tasks: Vec<BackgroundTaskView> },
    /// 记忆里多了一条 / 更新了一条（界面刷新计数，不进对话流）
    MemoryChanged {
        /// `project` 或 `global`（见 `memory::Scope::name`）
        scope: String,
        /// 这份文件里现在有几条
        count: usize,
    },
    /// 一轮结束后提炼出的「候选」记忆：只进设置页顶部「待确认」，用户点「收下」才落盘。
    /// 不进对话流、不进日志（误记的代价比漏记大：设计说明第四节）
    MemorySuggested { candidates: Vec<MemoryCandidateView> },
}

/// 一条待确认的记忆（`KernelEvent::MemorySuggested`）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MemoryCandidateView {
    /// 结论（一句话）
    pub text: String,
    /// 标签
    #[serde(default)]
    pub tags: Vec<String>,
    /// 命中的键（同一句再来一次时不重复提示）
    #[serde(default)]
    pub key: String,
}

/// 一条后台命令在界面上的样子（见自研内核的 `bgtasks` 模块）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BackgroundTaskView {
    pub id: String,
    /// 起它的会话
    pub session: String,
    pub command: String,
    pub running: bool,
    /// 退出码；还在跑 / 被信号结束时为 None
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exit_code: Option<i32>,
    pub duration_secs: u64,
    /// 输出末尾几行（预览）
    pub output_tail: String,
}

/// 一个 MCP 服务器在界面上的样子。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct McpServerView {
    pub name: String,
    pub state: McpState,
    /// 提供几个工具（连上之后才有）
    pub tools: usize,
    /// 来自哪个插件；None = 数据目录的 mcp.json
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub plugin: Option<String>,
    /// 远程服务的地址；本地命令为 None
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    /// 没连上、可以点「重新连接」（配置本身有问题的不行）
    #[serde(default)]
    pub retry: bool,
    /// 存着这个远程服务的登录令牌（OAuth）：界面据此给「清除登录状态」。
    /// 只看有没有，令牌内容不经过这里（见 `mcp_oauth::has_token`）
    #[serde(default)]
    pub has_auth: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", content = "detail", rename_all = "snake_case")]
pub enum McpState {
    Connecting,
    Ready,
    /// 起不来、握手失败或中途退出：原因（含服务器 stderr 的最后几行）
    Failed(String),
    /// 连上过、断了，正在退避重连：断开的原因和下次重连的说明
    Reconnecting(String),
    /// 配置里写了 `"disabled": true`
    Disabled,
    /// 服务器要求 OAuth 授权：在浏览器里打开 `url` 完成登录。
    /// 流程只等 10 分钟，过期后再「重新连接」会重新起一轮、换一个新地址
    NeedsAuth {
        url: String,
    },
}

impl KernelEvent {
    /// 事件的简短标签（日志 / 冒烟测试输出用）。
    pub fn tag(&self) -> &'static str {
        match self {
            Self::Status(_) => "status",
            Self::Notice(_) => "notice",
            Self::Connected { .. } => "connected",
            Self::Started { .. } => "started",
            Self::AgentMessageChunk { .. } => "agent_message_chunk",
            Self::AgentThoughtChunk { .. } => "agent_thought_chunk",
            Self::UserMessageChunk { .. } => "user_message_chunk",
            Self::ToolCallStarted { .. } => "tool_call_started",
            Self::SubagentStarted { .. } => "subagent_started",
            Self::SubagentChunk { .. } => "subagent_chunk",
            Self::ToolCallUpdated { .. } => "tool_call_updated",
            Self::ToolCallProgress { .. } => "tool_call_progress",
            Self::ToolCallDiff { .. } => "tool_call_diff",
            Self::PlanReplaced { .. } => "plan_replaced",
            Self::PermissionAuto { .. } => "permission_auto",
            Self::PermissionRequest { .. } => "permission_request",
            Self::FsRead { .. } => "fs_read",
            Self::FsWrite { .. } => "fs_write",
            Self::TurnEnded { .. } => "turn_ended",
            Self::UsageUpdated { .. } => "usage_updated",
            Self::SessionFailed { .. } => "session_failed",
            Self::PromptFailed { .. } => "prompt_failed",
            Self::Retrying { .. } => "retrying",
            Self::Compacting => "compacting",
            Self::Compacted { .. } => "compacted",
            Self::CompactFailed { .. } => "compact_failed",
            Self::Fatal(_) => "fatal",
            Self::FileChanged { .. } => "file_changed",
            Self::McpStatus { .. } => "mcp_status",
            Self::TitleGenerated { .. } => "title_generated",
            Self::BackgroundTasks { .. } => "background_tasks",
            Self::MemoryChanged { .. } => "memory_changed",
            Self::MemorySuggested { .. } => "memory_suggested",
        }
    }
}

/// 错误信息看起来是「超出模型的上下文长度」（各家网关说法不一，按关键词认）。
pub fn is_context_overflow(message: &str) -> bool {
    let m = message.to_ascii_lowercase();
    [
        "context length",
        "context_length",
        "context window",
        "maximum context",
        "prompt is too long",
        "input is too long",
        "too many tokens",
        "exceeds the context",
        "上下文长度",
        "超出上下文",
        "超过上下文",
    ]
    .iter()
    .any(|k| m.contains(k))
}

/// 工具输出预览最多保留的行数
pub const OUTPUT_PREVIEW_LINES: usize = 40;
/// 工具输出预览最多保留的字符数（防止少数几行超长）
const OUTPUT_PREVIEW_CHARS: usize = 4000;
/// 预览里单行最多保留的字符数
const OUTPUT_PREVIEW_LINE_CHARS: usize = 240;

/// 工具输出 → 给界面看的预览：去掉首尾空行，制表符按 4 列制表位展开，去掉各行共同的
/// 行首缩进（读文件结果的行号列右对齐补空格，不去掉整段会往右空出一截），最多
/// [`OUTPUT_PREVIEW_LINES`] 行，超长行截断；截掉了内容时末尾注明还剩多少行。
/// 只给人看，不回给模型（模型拿到的是完整输出）。
pub fn output_preview(text: &str) -> String {
    let lines: Vec<&str> = text.trim_matches(['\n', '\r']).lines().collect();
    let mut rows: Vec<String> = Vec::new();
    let mut chars = 0;
    for line in &lines {
        if rows.len() == OUTPUT_PREVIEW_LINES || chars >= OUTPUT_PREVIEW_CHARS {
            break;
        }
        let line = expand_tabs(line.trim_end());
        let line = match line.char_indices().nth(OUTPUT_PREVIEW_LINE_CHARS) {
            Some((cut, _)) => format!("{}…", &line[..cut]),
            None => line,
        };
        chars += line.chars().count();
        rows.push(line);
    }
    let indent = rows
        .iter()
        .filter(|r| !r.is_empty())
        .map(|r| r.len() - r.trim_start_matches(' ').len())
        .min()
        .unwrap_or(0);
    let mut out = rows.iter().map(|r| r.get(indent..).unwrap_or("")).collect::<Vec<_>>().join("\n");
    if rows.len() < lines.len() {
        out.push_str(&format!("\n… 其余 {} 行未显示", lines.len() - rows.len()));
    }
    out
}

/// 运行中的输出预览保留的行数（看最新进展，不铺满屏）
pub const LIVE_PREVIEW_LINES: usize = 12;

/// 还在跑的命令的输出 → 预览：只留最后 [`LIVE_PREVIEW_LINES`] 行（进度条用 `\r` 覆写的行只留最后一截），
/// 前面有省略时开头注明；其余处理同 [`output_preview`]。
pub fn output_tail_preview(text: &str) -> String {
    let lines: Vec<&str> = text
        .trim_matches(['\n', '\r'])
        .lines()
        .map(|l| l.trim_end_matches('\r').rsplit('\r').next().unwrap_or(""))
        .collect();
    let skipped = lines.len().saturating_sub(LIVE_PREVIEW_LINES);
    let body = output_preview(&lines[skipped..].join("\n"));
    if skipped == 0 { body } else { format!("… 前面 {skipped} 行未显示\n{body}") }
}

/// 制表符展开到下一个 4 列制表位（按字符数算列，不管中文宽度）。
fn expand_tabs(line: &str) -> String {
    let mut out = String::with_capacity(line.len());
    let mut col = 0;
    for c in line.chars() {
        if c == '\t' {
            let n = 4 - col % 4;
            out.extend(std::iter::repeat_n(' ', n));
            col += n;
        } else {
            out.push(c);
            col += 1;
        }
    }
    out
}

/// 带会话归属的事件：一条连接上可以同时有多个会话。
///
/// `session` 为 ACP 的 sessionId；连接级事件（`Connected` / `Status` /
/// `Fatal` / `SessionFailed`）为 `None`。
#[derive(Debug, Clone, PartialEq)]
pub struct RoutedEvent {
    pub session: Option<String>,
    pub event: KernelEvent,
}

/// 用户消息里的一张图片附件（base64）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PromptImage {
    /// MIME 类型，如 `image/png`
    pub media_type: String,
    /// 图片字节（base64 编码）
    pub data: String,
}

/// 一条用户提示词：文本 + 可选图片附件 + 聊天的文本附件（内容内联进 `text`）。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct UserPrompt {
    pub text: String,
    pub images: Vec<PromptImage>,
    /// 聊天会话带的文本文件（内容拼进 `text`，见 [`attach::compose`]）；代码会话恒为空
    pub attachments: Vec<attach::TextAttachment>,
}

impl UserPrompt {
    /// 纯文本提示词。
    pub fn text(text: impl Into<String>) -> Self {
        Self { text: text.into(), images: Vec::new(), attachments: Vec::new() }
    }

    pub fn is_empty(&self) -> bool {
        self.text.trim().is_empty() && self.images.is_empty() && self.attachments.is_empty()
    }
}

/// 事件接收端：内核线程内直接调用，实现方负责跨线程投递。
pub type EventSink = Arc<dyn Fn(RoutedEvent) + Send + Sync + 'static>;

/// UI → 内核的指令。
#[derive(Debug, Clone)]
pub enum KernelCommand {
    /// 创建会话；`cwd` 为空用内核的默认工作目录
    NewSession { token: u64, cwd: Option<PathBuf>, kind: SessionKind },
    /// 续接一个已有会话（ACP `session/resume`，其次 `session/load`）；
    /// 都不支持或失败时退回新建，结果经 `Started { resumed }` 报告
    ResumeSession { token: u64, session_id: String, cwd: Option<PathBuf>, kind: SessionKind },
    /// 向某会话发送一轮用户消息
    Prompt { session: String, prompt: UserPrompt },
    /// 请求取消某会话的当前轮
    Cancel { session: String },
    /// 切换某会话的权限模式（ACP `session/set_mode`，如 `default` / `plan`）
    SetMode { session: String, mode_id: String },
    /// 关闭内核（断开连接、终止子进程）
    Shutdown,
}

/// 开会话时交给内核的东西（以后再加会话级选项只往这里添字段）。
#[derive(Debug, Clone, Default)]
pub struct SessionSpec {
    /// None = 内核的默认目录（聊天会话也传 `<数据目录>/chat`，见 [`SessionKind`]）
    pub cwd: Option<PathBuf>,
    /// 会话类型：聊天会话只给联网和全局记忆，不带工作目录相关的东西
    pub kind: SessionKind,
}

impl SessionSpec {
    /// Code 会话（有工作目录那套）。
    pub fn code(cwd: Option<PathBuf>) -> Self {
        Self { cwd, kind: SessionKind::Code }
    }
}

/// 可更换内核的统一抽象。
///
/// 实现方负责把指令转发到自己的运行时；事件一律经
/// [`AcpKernel::spawn`] 注入的 [`EventSink`] 异步回报。
pub trait Kernel: Send + Sync {
    /// 内核种类标识（"acp"、"builtin"…，用于 UI 显示与日志）。
    fn kind(&self) -> &'static str;
    /// 展示名（如 agent 自报名称）。
    fn label(&self) -> String;
    /// 创建一个会话；结果经 `Started` / `SessionFailed` 事件异步回报，
    /// 事件里带回同一个 `token`。
    fn new_session(&self, token: u64, spec: SessionSpec) -> anyhow::Result<()>;
    /// 续接之前的会话（`session_id` 为内核侧 ID）；结果经 `Started` 回报，
    /// 其 `resumed` 表示上下文是否得以延续。`history` 是该会话的本地日志：
    /// 自己保有上下文的内核（ACP agent）忽略它，进程内内核用它重建上下文。
    /// 默认实现：不支持续接，直接新建。
    fn resume_session(
        &self,
        token: u64,
        session_id: &str,
        spec: SessionSpec,
        history: &[Record],
    ) -> anyhow::Result<()> {
        let _ = (session_id, history);
        self.new_session(token, spec)
    }
    /// 能不能开聊天会话（[`SessionKind::Chat`]）。ACP agent 都是写代码的，不能 —— 默认不能。
    fn supports_chat(&self) -> bool {
        false
    }
    /// 向某会话发送一轮用户消息（文本 + 可选图片）；返回仅代表「已投递」。
    fn send_prompt(&self, session: &str, prompt: UserPrompt) -> anyhow::Result<()>;
    /// 请求取消某会话的当前轮（尽力而为）。
    fn cancel(&self, session: &str) -> anyhow::Result<()>;
    /// 应答一个 [`KernelEvent::PermissionRequest`]：`Some(选项 ID)` 选择，
    /// `None` 表示取消（等同拒绝且不再追问）。
    fn resolve_permission(&self, request_id: u64, option_id: Option<String>) -> anyhow::Result<()>;

    /// 同 [`resolve_permission`](Self::resolve_permission)，另外带上「只写这些块」。
    /// 只有自研内核的写文件 / 改文件认它；别的内核忽略 `grant`，按整个动作放行。
    fn resolve_permission_with(
        &self,
        request_id: u64,
        option_id: Option<String>,
        grant: Option<PartialGrant>,
    ) -> anyhow::Result<()> {
        let _ = grant;
        self.resolve_permission(request_id, option_id)
    }
    /// 切换权限模式；结果经 `Status` 事件回报，不支持的 agent 会被忽略。
    fn set_mode(&self, session: &str, mode_id: &str) -> anyhow::Result<()>;
    /// 切换某会话之后各轮使用的模型；默认不支持。
    fn set_model(&self, session: &str, model: &str) -> anyhow::Result<()> {
        let _ = (session, model);
        anyhow::bail!("当前内核不支持切换模型")
    }
    /// 临时覆盖某会话的思考档位（`None` = 回到档案里的那份）；默认不支持。
    /// 只作用于这一轮之后发出的请求，不改设置文件
    fn set_thinking(&self, session: &str, level: Option<&str>) -> anyhow::Result<()> {
        let _ = (session, level);
        anyhow::bail!("当前内核不支持切换思考强度")
    }
    /// 聊天会话的「联网」开关（会话级，下一轮开头生效）；默认不支持。
    fn set_web(&self, session: &str, web: bool) -> anyhow::Result<()> {
        let _ = (session, web);
        anyhow::bail!("当前内核不支持关联网")
    }
    /// 更新各模型的上下文窗口（模型 ID → token；界面取到模型列表后推过来，自动压缩用）。默认忽略。
    fn set_context_windows(&self, windows: std::collections::HashMap<String, u64>) {
        let _ = windows;
    }
    /// 把某会话较早的对话压缩成摘要（手动压缩，会话空闲时）：经 `Compacting` →
    /// `Compacted` / `CompactFailed` → `TurnEnded` 回报。默认不支持。
    fn compact(&self, session: &str) -> anyhow::Result<()> {
        let _ = session;
        anyhow::bail!("当前内核不支持压缩上下文")
    }
    /// 能不能回退对话（[`Kernel::rewind`]）。上下文在 agent 进程里的内核不能。
    fn supports_rewind(&self) -> bool {
        false
    }
    /// 把会话上下文换成 `history`（本地日志）折算出的样子：回退到某条用户消息之前用。
    /// 会话空闲时才调用。默认不支持。
    fn rewind(&self, session: &str, history: &[Record]) -> anyhow::Result<()> {
        let _ = (session, history);
        anyhow::bail!("当前内核不支持回退对话")
    }
    /// 重新读 MCP 配置、重连全部服务器；结果经 `McpStatus` 回报。默认没有 MCP。
    fn reload_mcp(&self) {}
    /// 重连一个没连上的 MCP 服务器（设置页的「重新连接」）。
    fn reconnect_mcp(&self, _server: &str) {}
    /// 清除一个 MCP 服务器存着的登录状态（OAuth 令牌），并重连它：状态回到「没连上 / 需要登录」。
    /// 默认没有 MCP，什么也不做。
    fn clear_mcp_auth(&self, _server: &str) -> anyhow::Result<()> {
        Ok(())
    }

    /// 换一批启用的插件（启用 / 停用之后）：自研内核接它们的 MCP 服务、技能和钩子，
    /// ACP 内核在之后新开 / 续接的会话里把插件的 MCP 服务交给 agent。
    fn set_plugins(&self, _plugins: Vec<plugin::Plugin>) {}
    /// 会话不再使用时调用（切走 / 关掉）：内核可以停掉它起的后台命令、释放上下文。
    fn close_session(&self, _session: &str) {}
    /// 停掉一条后台命令（自研内核的 `shell_background` 起的）。默认没有后台命令。
    fn stop_background_task(&self, _id: &str) {}
    /// 关闭内核。
    fn shutdown(&self);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recognizes_context_overflow() {
        assert!(is_context_overflow(
            "HTTP 400: This model's maximum context length is 128000 tokens. However, your messages resulted in 130000 tokens."
        ));
        assert!(is_context_overflow(
            "HTTP 400: prompt is too long: 210000 tokens > 200000 maximum"
        ));
        assert!(is_context_overflow("HTTP 400: 输入超出上下文长度限制"));
        assert!(!is_context_overflow("HTTP 429: Resource has been exhausted"));
    }

    #[test]
    fn output_preview_keeps_short_output() {
        assert_eq!(output_preview("\n     1\tfn main() {}  \n\n"), "1  fn main() {}");
        assert_eq!(output_preview(""), "");
    }

    #[test]
    fn output_preview_strips_common_indent() {
        // 读文件的行号列：右对齐 + 制表符，去掉共同缩进后行号仍然右对齐
        let read = "     9\tlet a = 1;\n    10\t\tb();\n    11\t";
        assert_eq!(output_preview(read), " 9  let a = 1;\n10      b();\n11");
        // 空行不参与计算共同缩进
        assert_eq!(output_preview("  a\n\n    b"), "a\n\n  b");
    }

    #[test]
    fn output_tail_preview_keeps_the_latest_lines() {
        assert_eq!(output_tail_preview("a\nb"), "a\nb");
        assert_eq!(output_tail_preview("10%\r50%\r100%\ndone\n"), "100%\ndone");
        let text = (1..=20).map(|i| i.to_string()).collect::<Vec<_>>().join("\n");
        let out = output_tail_preview(&text);
        assert!(out.starts_with("… 前面 8 行未显示\n9\n"));
        assert!(out.ends_with("\n20"));
    }

    #[test]
    fn output_preview_clips_lines() {
        let text: String = (1..=100).map(|i| format!("line {i}\n")).collect();
        let out = output_preview(&text);
        let rows: Vec<&str> = out.lines().collect();
        assert_eq!(rows.len(), OUTPUT_PREVIEW_LINES + 1);
        assert_eq!(rows[OUTPUT_PREVIEW_LINES - 1], format!("line {OUTPUT_PREVIEW_LINES}"));
        assert_eq!(
            rows[OUTPUT_PREVIEW_LINES],
            format!("… 其余 {} 行未显示", 100 - OUTPUT_PREVIEW_LINES)
        );
    }

    #[test]
    fn output_preview_clips_long_lines_and_total_size() {
        let out = output_preview(&"中".repeat(1000));
        assert_eq!(out.chars().count(), OUTPUT_PREVIEW_LINE_CHARS + 1);
        // 每行都很长时按总字符数提前收住
        let text = vec!["x".repeat(500); 30].join("\n");
        let out = output_preview(&text);
        assert!(
            out.chars().count() < OUTPUT_PREVIEW_CHARS + 2 * OUTPUT_PREVIEW_LINE_CHARS,
            "{}",
            out.len()
        );
        assert!(out.ends_with("行未显示"), "{out}");
    }
}
