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
mod process_guard;
mod store;

pub use acp::{AcpKernel, AcpKernelConfig};
pub use process_guard::kill_children_on_exit;
pub use store::{Record, SessionLog, SessionMeta, SessionStore};

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
    },
    /// 工具调用更新（状态推进、原始输出）
    ToolCallUpdated {
        id: String,
        title: Option<String>,
        status: Option<String>,
        /// 输出摘要：能从结构化输出里取到文本就取文本，否则是 JSON 文本；已截断
        raw_output: Option<String>,
    },
    /// 计划整体替换（ACP 语义：每次都是全量列表）
    PlanReplaced { entries: Vec<PlanEntryView> },
    /// 权限请求已按策略自动应答（`auto_approve` 开启时：allow_once 优先）
    PermissionAuto { title: String, chosen: String },
    /// agent 请求权限，等待 UI 经 [`Kernel::resolve_permission`] 应答
    PermissionRequest {
        /// 请求 ID（应答时回传）
        id: u64,
        /// 触发请求的工具调用标题
        title: String,
        options: Vec<PermissionOptionView>,
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
    },
    /// 创建会话失败（`session/new` 报错）；连接仍然可用
    SessionFailed { token: u64, message: String },
    /// 本轮提示失败（如未登录、模型报错）；连接仍然可用
    PromptFailed { message: String },
    /// 内核致命错误（连接断开、初始化失败等，内核不再可用）
    Fatal(String),
}

impl KernelEvent {
    /// 事件的简短标签（日志 / 冒烟测试输出用）。
    pub fn tag(&self) -> &'static str {
        match self {
            Self::Status(_) => "status",
            Self::Connected { .. } => "connected",
            Self::Started { .. } => "started",
            Self::AgentMessageChunk { .. } => "agent_message_chunk",
            Self::AgentThoughtChunk { .. } => "agent_thought_chunk",
            Self::UserMessageChunk { .. } => "user_message_chunk",
            Self::ToolCallStarted { .. } => "tool_call_started",
            Self::ToolCallUpdated { .. } => "tool_call_updated",
            Self::PlanReplaced { .. } => "plan_replaced",
            Self::PermissionAuto { .. } => "permission_auto",
            Self::PermissionRequest { .. } => "permission_request",
            Self::FsRead { .. } => "fs_read",
            Self::FsWrite { .. } => "fs_write",
            Self::TurnEnded { .. } => "turn_ended",
            Self::UsageUpdated { .. } => "usage_updated",
            Self::SessionFailed { .. } => "session_failed",
            Self::PromptFailed { .. } => "prompt_failed",
            Self::Fatal(_) => "fatal",
        }
    }
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

/// 事件接收端：内核线程内直接调用，实现方负责跨线程投递。
pub type EventSink = Arc<dyn Fn(RoutedEvent) + Send + Sync + 'static>;

/// UI → 内核的指令。
#[derive(Debug, Clone)]
pub enum KernelCommand {
    /// 创建会话；`cwd` 为空用内核的默认工作目录
    NewSession { token: u64, cwd: Option<PathBuf> },
    /// 续接一个已有会话（ACP `session/resume`，其次 `session/load`）；
    /// 都不支持或失败时退回新建，结果经 `Started { resumed }` 报告
    ResumeSession {
        token: u64,
        session_id: String,
        cwd: Option<PathBuf>,
    },
    /// 向某会话发送一轮用户消息
    Prompt { session: String, text: String },
    /// 请求取消某会话的当前轮
    Cancel { session: String },
    /// 切换某会话的权限模式（ACP `session/set_mode`，如 `default` / `plan`）
    SetMode { session: String, mode_id: String },
    /// 关闭内核（断开连接、终止子进程）
    Shutdown,
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
    fn new_session(&self, token: u64, cwd: Option<PathBuf>) -> anyhow::Result<()>;
    /// 续接之前的会话（`session_id` 为内核侧 ID）；结果经 `Started` 回报，
    /// 其 `resumed` 表示上下文是否得以延续。默认实现：不支持续接，直接新建。
    fn resume_session(
        &self,
        token: u64,
        session_id: &str,
        cwd: Option<PathBuf>,
    ) -> anyhow::Result<()> {
        let _ = session_id;
        self.new_session(token, cwd)
    }
    /// 向某会话发送一轮用户消息；返回仅代表「已投递」。
    fn send_prompt(&self, session: &str, text: String) -> anyhow::Result<()>;
    /// 请求取消某会话的当前轮（尽力而为）。
    fn cancel(&self, session: &str) -> anyhow::Result<()>;
    /// 应答一个 [`KernelEvent::PermissionRequest`]：`Some(选项 ID)` 选择，
    /// `None` 表示取消（等同拒绝且不再追问）。
    fn resolve_permission(&self, request_id: u64, option_id: Option<String>) -> anyhow::Result<()>;
    /// 切换权限模式；结果经 `Status` 事件回报，不支持的 agent 会被忽略。
    fn set_mode(&self, session: &str, mode_id: &str) -> anyhow::Result<()>;
    /// 关闭内核。
    fn shutdown(&self);
}
