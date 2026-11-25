//! 单个对话会话的状态，以及把内核事件 / 演示步骤应用到对话记录上。

use crate::demo::*;
use crate::model::*;
use crate::settings::KernelChoice;
use crate::usage::Streamed;
use claudesktop_core::{
    AvailableCommandView, CompactionView, ConfigOptionView, ContextUsageView, CostView, DiffView,
    Kernel, KernelEvent, ModeView, Record, SessionKind, SessionLog, SessionMeta,
};
use gpui_kit::*;
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

pub(crate) mod blocks;
pub(crate) mod cache;
pub(crate) mod layout;
pub(crate) mod replay;
pub(crate) mod wheel;

use layout::new_list_state;
pub use layout::{
    Activity, LIVE_CMD_PREVIEW_LINES, LIVE_PARALLEL_MAX, LIVE_THINK_LINES, ListMetrics,
    LivePreview, Row, RowLayout, StepGroup, TurnSummary, TurnUsage,
};

pub use wheel::{DEFAULT_FRAME_DT, WheelInput, WheelScroll};

/// 内核没上报模型时显示的模型名（统计时当作「未知模型」）
pub const UNKNOWN_MODEL: &str = "默认模型";

/// 一个对话会话：自己的对话记录、忙闲、模式、模型。一条内核连接上可以同时有多个。
pub struct Session {
    pub title: SharedString,
    /// 这个会话跑在哪种内核上（空会话可以换，有对话后固定）
    pub kernel: KernelChoice,
    /// 会话绑定的内核槽 key（[`KernelChoice::key`]）：事件按它找槽
    pub kernel_key: String,
    /// 创建时的令牌，用来把内核回的 Started / SessionFailed 对回这个会话
    pub token: u64,
    /// ACP sessionId（Started 之后才有；演示回放模式为 None）
    pub acp_id: Option<String>,
    /// 已向内核发出 session/new
    pub requested: bool,
    /// 可以发提示词：Started 已到，或演示回放模式
    pub ready: bool,
    pub transcript: Transcript,
    pub(crate) diff_revision: u64,
    pub(crate) diff_epoch: u64,
    pub busy: bool,
    /// 这一轮是手动压缩上下文（没有用户消息；结束时不记用时用量，不钉上一轮的计划）
    pub compact_turn: bool,
    /// 演示回放用：递增即取消正在播放的脚本
    pub cancel_token: u64,
    /// 对话记录的虚拟列表状态（只渲染可见的行）；默认跟随到底，用户往上翻就停，翻回底部再跟
    pub list: ListState,
    /// 每行上次测量时的指纹（[`Session::row_key`]）
    pub measured: Vec<u64>,
    /// 列表里的每一行画哪条记录（[`Row`]）；很大的已完成回复拆成多行后，行数和记录数不一样。
    /// 和 `measured`、`layout` 的下标口径：`rows` / `measured` 用行下标，`layout` / `transcript`
    /// 用记录下标（用 [`Session::row_of`] 换算）
    pub rows: Vec<Row>,
    /// 大回复的拆块结果（记录下标 → 各块文本）
    agent_parts: HashMap<usize, std::rc::Rc<[SharedString]>>,
    /// 每条记录的第一行在 `rows` 里的下标（换算用，和 `transcript` 等长）
    row_start: Vec<usize>,
    /// 上次拆块时的记录形态（条数、是否在跑、末尾回复多长）：没变就不重拆
    rows_sig: Option<(usize, bool, usize)>,
    /// 行划分变了、列表重建之后要恢复的阅读位置：(视口顶那一行, 行内偏移, 是否贴底)
    rows_restore: Option<(Option<Row>, Pixels, bool)>,
    /// 列表末尾的运行状态行上次测量时的指纹（未测量过为 None）
    measured_status: Option<u64>,
    /// 底部思考实时预览的 4 行整块翻页状态
    pub(crate) live_think: Option<layout::LiveThinkPager>,
    /// 本帧的排版度量（列宽、rem）：变了就按新字号 / 列宽重估所有行高并重测。
    /// 由内容区每帧交进来（见 [`Session::set_list_metrics`]）。
    metrics: Option<ListMetrics>,
    /// 上次已按它重估过的度量
    applied_metrics: Option<ListMetrics>,
    layout_rev: u64,
    last_sync: Option<SyncSig>,
    pub(crate) body_cache: crate::app::body_cache::BodyViewCache,
    /// 当前可选模式：自研内核按设置补齐，ACP 在 Started 后换成 agent 上报的。
    pub modes: Vec<ModeEntry>,
    pub permission_mode: usize, // modes 下标
    pub model_label: SharedString,
    /// 正在等待用户选择的登录方式；只在内存里，不进入会话日志
    pub auth: Option<AuthRequestView>,
    /// ACP 上报的完整配置（模型、思考档位及其他选项）
    pub config_options: Vec<ConfigOptionView>,
    /// ACP 上报的斜杠命令 / Skills
    pub available_commands: Vec<AvailableCommandView>,
    /// ACP 上报的上下文窗口大小，和本轮 token 分开
    pub context_size: Option<u64>,
    /// ACP 上报的会话累计费用，和本轮 token 分开
    pub context_cost: Option<CostView>,
    /// 自动演示：会话就绪后发送的提示词
    pub autoprompt: Option<claudesktop_core::UserPrompt>,
    /// 会话工作目录（侧栏按它分组；session/new 用它）
    pub cwd: PathBuf,
    /// 会话类型：聊天（没有工作目录那套）还是 Code。建好就定，发过第一条消息后不再改
    pub kind: SessionKind,
    /// 聊天会话的「联网」开关（会话级，下一轮开头生效）
    pub web: bool,
    /// 内核重连 / 会话还没 Started 时先记着，`Started` 时补交（和 `pending_thinking` 一个套路）
    pub pending_web: Option<bool>,
    /// 任务：建立时没为它选过文件夹（跑在默认目录里）。侧栏归到「任务」区，
    /// 在输入框上方的目录菜单里选个文件夹就成了项目会话
    pub task: bool,
    /// 会话跑在自己的 git worktree 里（`cwd` 就是 worktree 目录）：侧栏仍归到主项目组，
    /// 顶栏的分支 chip 用它（见 `app/worktree.rs`）
    pub worktree: Option<claudesktop_core::WorktreeMeta>,
    /// 正在后台建 worktree（建好了 `worktree` 才有值）：目录 chip 显示转圈，消息先排队
    pub worktree_busy: Option<String>,
    /// worktree 的最新状态（脏了几个文件、领先 / 落后）：`None` 内的 `None` = 还没读到，
    /// 外层 `None` = 没刷过。点开分支菜单、一轮结束时刷新，不在 render 里跑
    pub worktree_state: Option<Option<claudesktop_core::git::WorktreeStatus>>,
    /// 现在不能一键合并回主目录的原因（主目录脏了 / 不在 base 上），和 `worktree_state` 一起刷新
    pub worktree_blocker: Option<String>,
    /// 项目配置里的 `worktree` 一栏（`refresh_worktree` 里读好，菜单只看这份，不在 render 里跑 git）
    pub worktree_config: Option<claudesktop_core::project::WorktreeConfig>,
    /// 这次运行里试过重新检出丢了的 worktree 没有（只试一次，见 `ChatApp::repair_worktree`）
    pub worktree_repair_tried: bool,
    /// 持久化元信息：首条消息发出后才建档（空会话不落盘）
    pub meta: Option<SessionMeta>,
    /// 追加式日志；恢复的会话在第一次写入时才打开
    pub log: Option<SessionLog>,
    /// 从磁盘恢复、还没接上内核会话的历史会话（不自动 session/new）
    pub restored: bool,
    /// 界面显示数据是否已加载；启动历史从日志读取，连接中的休眠会话从显示快照恢复
    pub history_loaded: bool,
    pub history_loading: bool,
    pub history_error: Option<String>,
    pub(crate) display_snapshot: Option<Arc<cache::DisplaySnapshot>>,
    pub(crate) blocks: blocks::BlockCache,
    pub blocks_error: Option<String>,
    pub(crate) display_scroll: Option<(ListOffset, bool)>,
    cached_user_count: usize,
    pub(crate) deferred_events: Vec<(String, claudesktop_core::RoutedEvent)>,
    /// 从磁盘搜索跳过来时，等日志加载完再定位到第几轮
    pub pending_reveal_turn: Option<usize>,
    /// 历史会话正在续接：Started 到达时告诉用户上下文是否延续
    pub resuming: bool,
    /// 正在后台恢复完整历史并回退内核上下文；期间新消息先排队
    pub kernel_rewinding: bool,
    /// 内核会话建立前选的模型：Started 之后再告诉内核
    pub pending_model: Option<String>,
    /// agent 档案记的上次模型（`AcpSettings::last_model` 的快照）：Started 后 agent 没报模型就切过去
    pub last_model: Option<String>,
    /// 待同步给内核的权限模式：新会话默认值、保存的会话选择或连接前新选的模式。
    /// `Started` 时交给内核后清掉。
    pub pending_mode: Option<String>,
    /// 会话级思考档位（输入框那个下拉选的）；`None` = 跟随设置里的档案，
    /// 「默认」这一项也是 `None`（不发字段）
    pub thinking: Option<claudesktop_builtin::ThinkingLevel>,
    /// 会话建立前选的档位：`Started` 时再交给内核（和 `pending_model` 一个套路）
    pub pending_thinking: Option<Option<claudesktop_builtin::ThinkingLevel>>,
    /// 本轮累计的 token，一轮结束时写进日志后清零
    pub turn_usage: TurnUsage,
    /// 本轮收到、端点还没数过的输出（正文 + 思考）：流式期间状态行据此估算 token，
    /// 每次 `UsageUpdated` 到来时清掉（那之前的输出已经被端点数过了）
    turn_streamed: Streamed,
    /// 流式 chunk 的合批缓冲：先攒在这里，每 `STREAM_FLUSH` 才并进正文一次。
    /// chunk 来多快就画多快的话，长回复期间「补齐半截标记」会让整篇 markdown 每个 chunk
    /// 重解析一次（见 chat.rs 的 `reading_markdown`），合批把解析和重画压到每秒约 20 次
    stream_pending: String,
    /// 缓冲喂给哪一行：true = 思考，false = 正文
    stream_pending_thought: bool,
    /// 上一次并进正文的时刻（初始化为「很久以前」，第一个 chunk 立即上屏）
    stream_flushed_at: Instant,
    /// 上次调用方取走进标志（[`Session::take_stream_dirty`]）之后是否又并进过：
    /// 没并进过的 chunk 不改对话内容，不用重画
    stream_dirty: bool,
    /// 本轮开始的时刻（状态条计时用；只在 busy 时有意义）
    pub turn_started: Option<Instant>,
    /// 用户手动改过标题（首条消息不再覆盖）
    pub renamed: bool,
    /// 标题已是模型起的（`TitleGenerated`；之后不再用首条消息覆盖）
    pub title_generated: bool,
    /// 每行在当前视图下怎么画（[`Session::sync_list`] 每帧重算，渲染与测量指纹共用）
    pub layout: Vec<RowLayout>,
    /// 用户点开的过程组（按组第一行的下标）
    pub open_groups: HashSet<usize>,
    /// 用户主动收起的「活跃且只有几步」的过程组（按组第一行的下标）：
    /// 这类组默认展开，点一下收起来（见 `Session::compute_layout`）
    pub folded_live_groups: HashSet<usize>,
    /// 最近一次用户手动点了组头的那一组：只有它播展开 / 收起的弹簧过渡。
    /// 新行涌入、正文到达导致的自动收起不播（否则每出一行列表都抖一下）
    pub animated_group: Option<usize>,
    /// 每轮的用时和用量（按这一轮用户消息的下标），挂在这一轮回复末尾
    pub turn_summaries: HashMap<usize, TurnSummary>,
    /// 输入框上方的计划收成一行
    pub plan_folded: bool,
    /// 上下文里的 token（最近一次请求结束时内核报告的；0 = 不知道，输入框旁不显示用量）。
    /// 自研内核每走完一步工具循环就更新一次，跑着的时候也跟着涨
    pub context_tokens: u64,
    /// 正在从日志重放：新的思考块不计时（用时从日志里的 `Record::Thought` 取）
    replaying: bool,
    /// 刚收尾的那段思考的用时，等调用方取走写进日志（[`Session::take_thought_done`]）
    thought_done: Option<u64>,
    /// 端点限流 / 出错，内核正等着自动重试（下一个实质事件到达时清掉）
    pub retry: Option<Retry>,
    /// 不在眼前时这一轮结束了（回复完 / 出错），用户还没切过来看；侧栏标点、可按它筛选。只在内存里
    pub unread: bool,
    /// 这一轮跑着时用户又发的消息（先进先出）：这一轮正常结束就发出第一条，见 `ChatApp::dispatch_queued`。只在内存里
    pub queued: Vec<QueuedPrompt>,
    /// 这一轮跑着时点了 `/compact`：排在这轮后面，轮子正常结束后、发排队的消息之前压（见
    /// `ChatApp::queued_compaction`）。用户停下 / 这一轮出错时撤掉并说明。只在内存里
    pub compact_queued: bool,
    /// 用户按了停止、内核还没回这一轮的结束：这期间发的消息先排队，等结束到了再发
    /// （不然迟到的「已取消」会落在新的一轮头上，把它提前收掉）
    pub stopping: bool,
    /// 下次同步列表时滚到这一行（从搜索结果跳过来）
    pub reveal: Option<usize>,
    /// 滚轮平滑滚动进行中（见 [`WheelInput`]）；跳转、回到底部时清掉，免得两边抢
    pub wheel: Option<WheelInput>,
    /// 跟随状态机（滞回 + 方向 + 强制跟随窗口，见 [`ScrollFollow`]）
    pub follow: ScrollFollow,
    /// 展开 / 收起卡片时把被点的那一行钉在屏幕原位（GPUI 版的 `pinNode`）
    pub pin: Option<RowPin>,
    /// 刚在权限卡上「拒绝并说明」或提问面板「自己说」：输入框的占位文字提示，用户输入或发送时清掉
    pub steering: Option<&'static str>,
    /// 最近一轮输入命中提示缓存的比例（%）；端点没报告时为 None
    pub cache_hit: Option<u32>,
    /// 收到过的内容事件个数，和 `seen_seq` 比较判断往上翻的时候底下有没有来新内容
    content_seq: u64,
    /// 列表最后一次跟在底部时的 `content_seq`
    seen_seq: u64,
    /// 子任务的过程，按 task 调用 ID（见 [`SubagentRun`]）
    pub subagents: HashMap<String, SubagentRun>,
    /// 正在看哪个子任务的详情页（task 调用 ID）；None = 看对话。只在内存里
    pub viewing_task: Option<String>,
    /// 最新一轮在审查页里撤回过块：下一条消息发出时附上说明（见 `claudesktop_core::review::review_note`）。
    /// 只是「要不要去读日志」的提示，说明本身从日志现算
    pub review_pending: bool,
    /// 展开了全部文件的改动卡片（按回复末尾那一行的下标）。只在内存里
    pub(crate) changes_open: HashSet<usize>,
    /// 最新一轮的改动撤回到了什么程度（审查页或改动卡片上的「撤回」写盘后更新）。
    /// 只在内存里：重启后卡片照常显示「撤回」，再点一次结果不变
    pub undo: Option<TurnUndo>,
    /// 最近一次「看过或有事发生」的时刻（LRU 冷会话回收按它排，见
    /// `ChatApp::reclaim_idle_sessions`）；每秒的计时器在更新，始终有效
    last_touch: Instant,
}

/// Includes in-place changes (streaming, tool output and card expansion) in layout invalidation.
#[derive(Default, serde::Serialize)]
#[serde(transparent)]
pub struct Transcript {
    items: Vec<ChatItem>,
    #[serde(skip)]
    revision: u64,
}

impl std::ops::Deref for Transcript {
    type Target = Vec<ChatItem>;

    fn deref(&self) -> &Self::Target {
        &self.items
    }
}

impl std::ops::DerefMut for Transcript {
    fn deref_mut(&mut self) -> &mut Self::Target {
        self.revision = self.revision.wrapping_add(1);
        &mut self.items
    }
}

#[derive(Clone, Copy, PartialEq)]
struct SyncSig {
    layout_rev: u64,
    transcript_rev: u64,
    content_seq: u64,
    rows: usize,
    view: ViewMode,
    metrics: Option<ListMetrics>,
    images: u64,
    status_key: u64,
    busy: bool,
    compact_turn: bool,
    undo: Option<TurnUndo>,
}

/// 某一轮的改动撤回到了什么程度，见 [`Session::undo`]。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct TurnUndo {
    /// 第几条用户消息那一轮（从 0 数）
    pub turn: usize,
    /// 能撤回的块全撤回了（否则是撤回了一部分）
    pub all: bool,
    /// 对着 diff 数过；重开的会话从日志只看得出「撤回过」，切过来时再数（`ChatApp::settle_undo`）
    pub exact: bool,
}

/// 一个子任务（自研内核的 `task` 调用）的过程：子 agent 的思考、回复和工具调用。
/// 主会话的对话记录里只有 task 那一张卡片，这些在子任务详情页里看。
#[derive(Default, serde::Serialize, serde::Deserialize)]
pub struct SubagentRun {
    /// 角色名（旧日志里没有 `SubagentStarted`，为空）
    pub agent: SharedString,
    /// 主 agent 交给它的任务说明（同上，旧日志为空）
    pub prompt: SharedString,
    /// 按发生顺序：`Thinking` / `Agent` / `Tool`
    pub items: Vec<ChatItem>,
}

impl SubagentRun {
    fn push_text(&mut self, text: &str, thought: bool) {
        match (self.items.last_mut(), thought) {
            (Some(ChatItem::Thinking { text: buf, streaming: true, .. }), true) => {
                buf.push_str(text)
            }
            (Some(ChatItem::Agent(buf)), false) => buf.push_str(text),
            _ => {
                self.finish_thinking();
                self.items.push(if thought {
                    ChatItem::thinking(text, None)
                } else {
                    ChatItem::Agent(text.to_string())
                });
            }
        }
    }

    fn push_tool(&mut self, card: ToolCard) {
        self.finish_thinking();
        self.items.push(ChatItem::Tool(card));
    }

    fn finish_thinking(&mut self) {
        if let Some(ChatItem::Thinking { streaming, .. }) = self.items.last_mut() {
            *streaming = false;
        }
    }

    /// 调用了几次工具。
    pub fn steps(&self) -> usize {
        self.items.iter().filter(|i| matches!(i, ChatItem::Tool(_))).count()
    }

    /// 交回主会话的报告：最后一段回复（后面没再调工具）。
    pub fn report(&self) -> Option<&str> {
        match self.items.last() {
            Some(ChatItem::Agent(text)) if !text.trim().is_empty() => Some(text),
            _ => None,
        }
    }
}

/// 内核的一次自动重试：状态行显示「网关限流，3 秒后第 1 次重试」。
#[derive(Debug, Clone, PartialEq)]
pub struct Retry {
    /// 上一次失败的 HTTP 状态码
    pub status: u16,
    pub attempt: usize,
    /// 预计重发的时刻
    pub at: Instant,
}

/// 进行中的过程组露出最近几步，其余收在组头里
pub const LIVE_TAIL: usize = 3;

/// 流式文本两次并进正文的最小间隔：界面按这个节奏出字（每秒约 20 次），
/// 而不是 chunk 来多快就画多快（见 `Session::stream_pending`）
const STREAM_FLUSH: std::time::Duration = std::time::Duration::from_millis(50);

/// 合批间隔的上限：消息越长每次并进越贵（整篇重解析，见 `Session::stream_pending`），
/// 刷新率砍到 4 次/秒手感也无差——人眼对文字的流畅感知上限远低于此。
const STREAM_FLUSH_MAX: std::time::Duration = std::time::Duration::from_millis(250);
/// 间隔从这条消息的长度起爬坡（以下恒为 `STREAM_FLUSH`），到 `STREAM_FLUSH_LONG` 封顶。
const STREAM_FLUSH_RAMP: usize = 8 * 1024;
const STREAM_FLUSH_LONG: usize = 64 * 1024;

impl Session {
    #[cfg(test)]
    pub fn new(token: u64, kernel: KernelChoice, cwd: PathBuf) -> Self {
        Self::of_kind(token, kernel, cwd, SessionKind::Code)
    }

    /// 新会话，按类型：[`SessionKind::Chat`] 的会话没有工作目录那套（`task` 恒为 false），
    /// `cwd` 是数据目录下的 `chat/` 占位（见「设计说明-聊天与Code分离.md」第三章）。
    pub fn of_kind(token: u64, kernel: KernelChoice, cwd: PathBuf, kind: SessionKind) -> Self {
        let kernel_key = kernel.key();
        let is_demo = kernel == KernelChoice::Demo;
        Self {
            title: "新会话".into(),
            kernel,
            kernel_key,
            token,
            acp_id: None,
            requested: false,
            ready: is_demo,
            transcript: Transcript::default(),
            diff_revision: 0,
            diff_epoch: 0,
            busy: false,
            compact_turn: false,
            cancel_token: 0,
            list: new_list_state(),
            measured: Vec::new(),
            rows: Vec::new(),
            agent_parts: HashMap::new(),
            row_start: Vec::new(),
            rows_sig: None,
            rows_restore: None,
            measured_status: None,
            live_think: None,
            metrics: None,
            applied_metrics: None,
            layout_rev: 0,
            last_sync: None,
            body_cache: Default::default(),
            modes: if kind == SessionKind::Chat {
                // 聊天没有工具要批，也就没有权限模式（Started 报的也是空表）
                Vec::new()
            } else {
                default_modes()
            },
            permission_mode: 0,
            // 演示回放用假模型名；真内核等 Started 上报，没上报就一直显示「默认模型」
            model_label: if is_demo { "GLM-5.3" } else { UNKNOWN_MODEL }.into(),
            auth: None,
            config_options: Vec::new(),
            available_commands: Vec::new(),
            context_size: None,
            context_cost: None,
            autoprompt: None,
            cwd,
            kind,
            web: true,
            pending_web: None,
            task: false,
            worktree: None,
            worktree_busy: None,
            worktree_state: None,
            worktree_blocker: None,
            worktree_config: None,
            worktree_repair_tried: false,
            meta: None,
            log: None,
            restored: false,
            history_loaded: true,
            history_loading: false,
            history_error: None,
            display_snapshot: None,
            blocks: blocks::BlockCache::new(token),
            blocks_error: None,
            display_scroll: None,
            cached_user_count: 0,
            deferred_events: Vec::new(),
            pending_reveal_turn: None,
            resuming: false,
            kernel_rewinding: false,
            pending_model: None,
            last_model: None,
            pending_mode: None,
            thinking: None,
            pending_thinking: None,
            turn_usage: TurnUsage::default(),
            turn_streamed: Streamed::default(),
            stream_pending: String::new(),
            stream_pending_thought: false,
            stream_flushed_at: Instant::now()
                .checked_sub(STREAM_FLUSH)
                .unwrap_or_else(Instant::now),
            stream_dirty: false,
            turn_started: None,
            renamed: false,
            title_generated: false,
            layout: Vec::new(),
            open_groups: HashSet::new(),
            folded_live_groups: HashSet::new(),
            animated_group: None,
            turn_summaries: HashMap::new(),
            plan_folded: false,
            context_tokens: 0,
            replaying: false,
            thought_done: None,
            retry: None,
            unread: false,
            queued: Vec::new(),
            compact_queued: false,
            reveal: None,
            wheel: None,
            follow: ScrollFollow::default(),
            pin: None,
            stopping: false,
            steering: None,
            cache_hit: None,
            content_seq: 0,
            seen_seq: 0,
            subagents: HashMap::new(),
            viewing_task: None,
            review_pending: false,
            changes_open: HashSet::new(),
            undo: None,
            last_touch: Instant::now(),
        }
    }

    /// 启动时给侧栏用的历史会话占位：不读日志，也不构造对话行。
    pub fn placeholder(
        token: u64,
        meta: SessionMeta,
        settings: &crate::settings::Settings,
    ) -> Self {
        let mut s = Self::from_records(token, meta, Vec::new(), settings);
        s.history_loaded = false;
        s
    }
}

/// 滚动区内容滚到了哪一档：上边缘 / 下边缘的渐隐条按它收放（贴顶时顶上那条收成 0）。
///
/// 只用来决定覆盖层的高度，**不要**并进 `Session::row_key`——那是布局指纹，变了会整表重测；
/// 滚动时内容区本来就在重画，这里零额外成本。侧栏是缓存 view，档位要并进 `sync_sidebar` 的指纹。
#[derive(Clone, Copy, PartialEq, Eq, Debug, Hash)]
pub enum ScrollEdge {
    /// 内容不满一屏（滚不动）：两头都到头了，两条都不画
    Fits,
    Top,
    Middle,
    Bottom,
}

/// 顶上那条渐隐的高度（约一行的上边距）
pub const FADE_TOP: Rems = rems(1.);
/// 底下那条渐隐的高度（状态行下边距）
pub const FADE_BOTTOM: Rems = rems(1.5);

impl ScrollEdge {
    /// `top` = 离顶部多远，`remaining` = 离底部还有多远（都按像素）。
    /// 差 1px 以内就算到头（浮点累加的滚动位置不会正好是 0）。
    pub fn of(top: Pixels, remaining: Pixels) -> Self {
        match (top <= px(1.), remaining <= px(1.)) {
            (true, true) => Self::Fits,
            (true, false) => Self::Top,
            (false, true) => Self::Bottom,
            (false, false) => Self::Middle,
        }
    }

    /// 上下两条渐隐的高度。
    pub fn heights(self) -> (Rems, Rems) {
        match self {
            Self::Fits => (Rems::ZERO, Rems::ZERO),
            Self::Top => (Rems::ZERO, FADE_BOTTOM),
            Self::Middle => (FADE_TOP, FADE_BOTTOM),
            Self::Bottom => (FADE_TOP, Rems::ZERO),
        }
    }
}

/// 连续渐隐高度：贴边时对应那条收成 0，离开 `ramp` 距离后到满格（`FADE_TOP` / `FADE_BOTTOM`）。
/// 比 [`ScrollEdge`] 的四档细，滚动时渐隐跟着位置连续长 / 消，不再一眼看出跳档。
pub fn edge_fades(top: Pixels, remaining: Pixels, rem: Pixels) -> (Pixels, Pixels) {
    // 内容不满一屏（两头都到头）：两条都不画
    if top <= px(1.) && remaining <= px(1.) {
        return (px(0.), px(0.));
    }
    let ramp = (rem * 2.5).max(px(1.));
    let up = (top / ramp).clamp(0., 1.);
    let down = (remaining / ramp).clamp(0., 1.);
    (FADE_TOP.to_pixels(rem) * up, FADE_BOTTOM.to_pixels(rem) * down)
}

impl Session {
    /// 上下两条渐隐的高度（连续，见 [`edge_fades`]）。
    pub fn scroll_fades(&self, rem: Pixels) -> (Pixels, Pixels) {
        let offset = self.list.scroll_px_offset_for_scrollbar().y;
        let max = self.list.max_offset_for_scrollbar().y.max(px(0.));
        edge_fades(-offset, max + offset, rem)
    }
}

/// 跟随判定的滞回带：距底这么多以内、并且是往下滚回来的，就重新吸附。
const FOLLOW_ATTACH: Pixels = px(80.);
/// 距底超过视口的这个比例（至少 240px）才认为「翻走了」。
const FOLLOW_DETACH_VIEWPORT: f32 = 0.5;
const FOLLOW_DETACH_MIN: Pixels = px(240.);
/// 程序化滚动 / 跳转之后这段时间不改跟随状态（自己造成的位移不算用户操作）。
const FOLLOW_QUIET: Duration = Duration::from_millis(700);
/// 发送 / 应答之后这段强跟随，抵御输入框骤缩、行高补量引起的瞬时抖动。
const FOLLOW_FORCE: Duration = Duration::from_millis(800);

/// 对话列表的跟随状态机（几何 + 方向驱动，见 [`Session::sync_follow`]）。
#[derive(Clone, Copy, Debug, Default)]
pub struct ScrollFollow {
    /// 「回到底部」按钮显不显示：进入脱离带才置位，回到吸附带才清掉，中间带保持
    pub away: bool,
    /// 用户往下滚进了距底 80px 的吸附带、但还没到 1px 处：**不立刻把视口吸到底**，
    /// 等真正有新内容到来（`content_seq > armed_seq`）时再平滑接回跟底。
    armed: bool,
    /// 进入待命态那一刻的内容序号
    armed_seq: u64,
    quiet_until: Option<Instant>,
    force_until: Option<Instant>,
    /// 最近一次用户输入的方向（Some(true) = 向下）；只有向下滚回底部才自动吸附
    last_input_down: Option<bool>,
}

/// 展开 / 收起卡片时钉住标题行：把被点行的屏幕 Y 记住，之后几帧按偏差反向补偿。
/// 跟底态点开一张卡片，内容在下面长高，不钉住的话视口会被拽到末尾、卡片飞出屏幕。
#[derive(Clone, Copy, Debug)]
pub struct RowPin {
    pub ix: usize,
    /// 点下时这一行的窗口坐标顶部
    pub screen_top: Pixels,
    pub until: Instant,
}

/// 弹簧展开 / 收起的时长上限，覆盖整个过渡
const PIN_WINDOW: Duration = Duration::from_millis(300);

impl Session {
    /// 用户点开 / 收起某一行之前调用：停止跟底并记住这一行的屏幕位置。
    pub fn pin_row(&mut self, ix: usize) {
        self.pin_at_row(self.row_of(ix));
    }

    /// 钉住记录 `ix` 的**最后一行**：改动卡片挂在拆块回复的最后一块上，钉第一块会钉到屏幕外。
    pub fn pin_last_row(&mut self, ix: usize) {
        let row = self.row_of(ix) + self.part_count(ix).map_or(0, |count| count - 1);
        self.pin_at_row(row);
    }

    fn pin_at_row(&mut self, row: usize) {
        self.list.pause_following_tail();
        // 用户主动点开卡片：强跟随窗口作废，否则松手后几帧里还会被拽回底部
        self.follow.force_until = None;
        self.follow.armed = false;
        self.follow.away = self.follow.away || self.distance_to_end() > FOLLOW_ATTACH;
        if let Some(bounds) = self.list.bounds_for_item(row) {
            self.pin = Some(RowPin {
                ix: row,
                screen_top: bounds.top(),
                until: Instant::now() + PIN_WINDOW,
            });
        }
    }

    /// 每帧在列表定位之后调用：把被钉住的行拉回原位。
    /// 返回 true 表示这一帧之后还要再排一帧——点击后的第一帧量到的还是展开前的位置，
    /// 必须等 `prepaint` 把新高度排出来，下一帧才补得上（没有弹簧动画时也一样）。
    pub fn step_pin(&mut self, now: Instant) -> bool {
        let Some(pin) = self.pin else { return false };
        if now >= pin.until || self.list.is_scrollbar_dragging() || self.wheel.is_some() {
            self.pin = None;
            return false;
        }
        let Some(bounds) = self.list.bounds_for_item(pin.ix) else {
            self.pin = None;
            return false;
        };
        let dy = bounds.top() - pin.screen_top;
        if dy.abs() > px(0.5) {
            self.list.scroll_by(dy);
        }
        // 钉住窗口内一直排帧：偏差这一帧可能还是 0（布局还没量到新高度）
        true
    }

    /// 用户自己开始滚动 / 换会话：钉住状态作废。
    pub fn release_pin(&mut self) {
        self.pin = None;
    }
}

/// `sync_follow` 拿到几何量之后要做的动作（纯函数，方便单测）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum FollowStep {
    /// 什么都不做（钉在中间带，或已经在待命）
    Hold,
    /// 几何上已经贴底：直接交回跟底，没有视觉跳变
    Attach,
    /// 停在吸附带里等新内容
    Arm,
    /// 待命期间真来了新内容：平滑滑回底部
    GlideToTail,
    /// 翻走了：脱离跟底
    Detach,
}

/// 跟随判定的核心规则（见 [`Session::sync_follow`]）。
fn follow_step(
    remaining: Pixels,
    detach: Pixels,
    tail_known: bool,
    last_input_down: Option<bool>,
    armed: bool,
    new_content: bool,
) -> FollowStep {
    // 已经贴到 1px 以内：直接交回跟底，没有视觉跳变
    if remaining <= px(1.) {
        return FollowStep::Attach;
    }
    if armed {
        // 待在吸附带里等新内容：只有真来了新内容才滑回底部，松手那一刻绝不瞬移。
        // 期间不动视口，也不重新记 `armed_seq`（否则新内容永远算「没变化」）。
        if new_content {
            return FollowStep::GlideToTail;
        }
        return if remaining > detach { FollowStep::Detach } else { FollowStep::Hold };
    }
    if remaining <= FOLLOW_ATTACH && last_input_down == Some(true) && tail_known {
        return FollowStep::Arm;
    }
    if remaining > detach {
        return FollowStep::Detach;
    }
    FollowStep::Hold
}

impl Session {
    pub fn note_input(&mut self, down: bool) {
        self.follow.last_input_down = Some(down);
        if !down {
            // 用户自己往上翻了：发消息后的强跟随窗口当场作废，别跟用户抢
            self.follow.force_until = None;
            self.follow.armed = false;
        }
    }

    /// 用户自己开始滚动 / 跳转 / 点开卡片：作废强跟随窗口。
    pub fn drop_force_follow(&mut self) {
        self.follow.force_until = None;
        self.follow.armed = false;
    }

    /// 程序化定位（搜索跳转、显示恢复）之后的一小段安静期，别被自动吸附拉走。
    pub fn quiet_follow(&mut self, duration: Duration) {
        self.follow.quiet_until = Some(Instant::now() + duration);
    }

    /// 每帧在列表定位推进之后调用：按距底距离和输入方向决定吸附 / 脱离，中间带保持原状。
    /// 返回 `true` 表示「待命态下刚来了新内容，请外层平滑滑回底部」。
    ///
    /// 按钮显隐（`away`）和自动吸附是两件事：前者只按几何滞回更新，滚轮还在缓出、正拖
    /// 滚动条时也要立刻变（否则连续翻页要等 200 多毫秒才冒出按钮）；吸附才要求方向向下、
    /// 静止、尾部已实测。
    pub fn sync_follow(&mut self, now: Instant, moving: bool) -> bool {
        let dragging = self.list.is_scrollbar_dragging();
        // 用户正拖着滚动条：强跟随作废，跟随判定让位，别把人拽回去
        if dragging {
            self.follow.force_until = None;
            self.follow.armed = false;
        } else if let Some(until) = self.follow.force_until
            && now < until
        {
            if !self.list.is_following_tail() {
                self.list.set_follow_mode(FollowMode::Tail);
            }
            self.follow.away = false;
            self.follow.armed = false;
            return false;
        }
        if self.list.is_following_tail() {
            self.follow.away = false;
            self.follow.armed = false;
            return false;
        }
        let remaining = self.distance_to_end();
        let viewport = self.list.viewport_bounds().size.height;
        let detach = (viewport * FOLLOW_DETACH_VIEWPORT).max(FOLLOW_DETACH_MIN);
        // 先按几何滞回定按钮显隐：不要求静止，也不要求方向
        if remaining <= FOLLOW_ATTACH {
            self.follow.away = false;
        } else if remaining > detach {
            self.follow.away = true;
        }
        let settling = moving
            || self.wheel.is_some()
            || self.follow.quiet_until.is_some_and(|until| now < until);
        if dragging || settling {
            return false;
        }
        let tail_known = self.list.is_tail_measured();
        let step = follow_step(
            remaining,
            detach,
            tail_known,
            self.follow.last_input_down,
            self.follow.armed,
            self.content_seq > self.follow.armed_seq,
        );
        match step {
            FollowStep::GlideToTail => {
                self.follow.armed = false;
                self.follow.away = false;
                return true;
            }
            // 真滚到 1px 处 / 待命期间新内容已经贴底：没有视觉跳变，直接交回跟底
            FollowStep::Attach => {
                self.list.set_follow_mode(FollowMode::Tail);
                self.follow.armed = false;
                self.follow.away = false;
                self.follow.quiet_until = Some(now + FOLLOW_QUIET);
            }
            // 停在 1..=80px 之间：视口原地不动，只进入待命态
            FollowStep::Arm => {
                if !self.follow.armed {
                    self.follow.armed = true;
                    self.follow.armed_seq = self.content_seq;
                }
                self.follow.away = false;
            }
            FollowStep::Detach => {
                self.follow.armed = false;
                self.list.pause_following_tail();
            }
            FollowStep::Hold => {}
        }
        false
    }

    /// 距列表末尾还有多少像素（按当前已量 + 估算的高度算）。
    pub fn distance_to_end(&self) -> Pixels {
        let offset = self.list.scroll_px_offset_for_scrollbar().y;
        let max = self.list.max_offset_for_scrollbar().y.max(px(0.));
        (max + offset).max(px(0.))
    }
}

/// 某个 `ScrollHandle` 滚到哪一档（首页、侧栏这类普通滚动区用它）。
pub fn scroll_handle_edge(handle: &ScrollHandle) -> ScrollEdge {
    let offset = handle.offset().y;
    let max = handle.max_offset().y.max(px(0.));
    ScrollEdge::of(-offset, max + offset)
}

/// 某个 `ScrollHandle` 的连续渐隐高度（首页这类普通滚动区用它，见 [`edge_fades`]）。
pub fn scroll_handle_fades(handle: &ScrollHandle, rem: Pixels) -> (Pixels, Pixels) {
    let offset = handle.offset().y;
    let max = handle.max_offset().y.max(px(0.));
    edge_fades(-offset, max + offset, rem)
}

impl Session {
    /// 展开 / 收起从 `start` 开始的过程组。活跃的小组（`live_small`）默认展开，
    /// 收起状态另存一份，免得和用户点开的旧组混在一起。
    pub fn toggle_group(&mut self, start: usize) {
        self.layout_rev = self.layout_rev.wrapping_add(1);
        self.animated_group = Some(start);
        let live_small = self.layout.get(start).and_then(|l| l.head).is_some_and(|h| h.live_small);
        if live_small {
            if !self.folded_live_groups.remove(&start) {
                self.folded_live_groups.insert(start);
            }
        } else if !self.open_groups.remove(&start) {
            self.open_groups.insert(start);
        }
    }

    pub fn toggle_changes(&mut self, idx: usize) {
        if !self.changes_open.remove(&idx) {
            self.changes_open.insert(idx);
        }
        self.layout_rev = self.layout_rev.wrapping_add(1);
    }

    /// 最后一条用户消息的下标。
    fn last_user(&self) -> Option<usize> {
        self.transcript.iter().rposition(|i| matches!(i, ChatItem::User { .. }))
    }

    /// 本轮（最后一条用户消息之后）的第一行。
    fn turn_start(&self) -> usize {
        self.last_user().map_or(0, |i| i + 1)
    }

    /// 这条事件值不值得发系统通知（窗口不在前台时用）：要授权、本轮出错、回复完了。
    /// 返回通知正文；在 `apply_event` 之前调用（看的是事件到达前的状态）。
    /// 用户自己停下的、手动压缩那一轮不算。
    pub fn attention(&self, event: &KernelEvent) -> Option<String> {
        match event {
            KernelEvent::PermissionRequest { title, .. } => Some(format!("等你授权：{title}")),
            KernelEvent::UserQuestion { questions, .. } => {
                let first = questions.first().map(|q| q.question.as_str()).unwrap_or("");
                Some(format!("等你回答：{first}"))
            }
            KernelEvent::PromptFailed { message } => {
                Some(format!("出错了：{}", preview_line(message).unwrap_or_default()))
            }
            KernelEvent::TurnEnded { stop_reason }
                if stop_reason != "cancelled" && !self.compact_turn =>
            {
                let reply =
                    self.transcript[self.turn_start()..].iter().rev().find_map(|i| match i {
                        ChatItem::Agent(text) => preview_line(text),
                        _ => None,
                    });
                Some(reply.map_or_else(|| "回复完了".into(), |r| format!("回复完了：{r}")))
            }
            _ => None,
        }
    }

    /// 进行中这一轮的计划（钉在输入框上方，对话里那份只留一行进度）；不忙或本轮没有计划时为 None。
    pub fn pinned_plan(&self) -> Option<usize> {
        // 手动压缩时上一轮的计划已经过去了，不钉
        if !self.busy || self.compact_turn {
            return None;
        }
        let start = self.turn_start();
        self.transcript[start..]
            .iter()
            .rposition(|i| matches!(i, ChatItem::Plan(_)))
            .map(|i| i + start)
    }

    /// 本轮输出已经有多少 token：端点上一次的 `UsageUpdated`（已经算准的部分）
    /// + 之后又流式收到、端点还没数的那部分估算。
    pub fn live_output_tokens(&self) -> u64 {
        self.turn_usage.output + self.turn_streamed.tokens()
    }

    /// 一轮结束：取走本轮用量、算出用时，记到这一轮名下（回复末尾显示），也交给调用方写日志。
    pub fn close_turn(&mut self) -> TurnSummary {
        self.flush_stream();
        let summary = TurnSummary {
            elapsed_ms: self.turn_started.take().map(|t| t.elapsed().as_millis() as u64),
            usage: std::mem::take(&mut self.turn_usage),
        };
        self.turn_streamed.clear();
        self.live_think = None;
        if let Some(user) = self.last_user() {
            self.turn_summaries.insert(user, summary);
            self.layout_rev = self.layout_rev.wrapping_add(1);
        }
        summary
    }

    /// `ix` 所在这一轮的用时和用量（这一轮结束后才有）。
    pub fn turn_summary(&self, ix: usize) -> Option<&TurnSummary> {
        let user =
            self.transcript[..ix].iter().rposition(|i| matches!(i, ChatItem::User { .. }))?;
        self.turn_summaries.get(&user)
    }

    /// `ix` 是不是一轮回复的最后一段正文：它后面（跳过备注）紧跟用户消息，
    /// 或者它就是最后一段且这一轮已经结束。这样的回复下面挂「复制」等操作。
    pub fn is_turn_end(&self, ix: usize) -> bool {
        if !matches!(self.transcript.get(ix), Some(ChatItem::Agent(_))) {
            return false;
        }
        match self.transcript[ix + 1..].iter().find(|i| {
            !matches!(
                i,
                ChatItem::Note(_)
                    | ChatItem::Failed { .. }
                    | ChatItem::Compaction { .. }
                    | ChatItem::Question { .. }
            )
        }) {
            Some(ChatItem::User { .. }) => true,
            Some(_) => false,
            None => !self.busy,
        }
    }

    /// 这一轮没有正文回复（被打断，或者只跑了工具）时，把回合末尾挂在这一行上：
    /// 它之后没有别的要画、有内容的行，这一轮也已经结束。
    pub fn is_turn_fallback_tail(&self, ix: usize, view: ViewMode) -> bool {
        let t = &self.transcript;
        let Some(item) = t.get(ix) else { return false };
        // 用户气泡、正文有自己的末尾；当前视图看不见的行（如 `AutoApproved`）不能当锚点
        if matches!(item, ChatItem::User { .. } | ChatItem::Agent(_))
            || !item.visible_in(view)
            || !item.has_content()
        {
            return false;
        }
        let start =
            t[..ix].iter().rposition(|i| matches!(i, ChatItem::User { .. })).map_or(0, |p| p + 1);
        let end = t[ix + 1..]
            .iter()
            .position(|i| matches!(i, ChatItem::User { .. }))
            .map_or(t.len(), |p| ix + 1 + p);
        if end == t.len() && self.busy {
            return false;
        }
        if t[start..end].iter().any(|i| matches!(i, ChatItem::Agent(_))) {
            return false;
        }
        !t[ix + 1..end].iter().any(|i| i.visible_in(view) && i.has_content())
    }

    /// 最新一轮的末尾：优先带正文的那一行，没有正文就退到这一轮最后一个可见步骤。
    pub fn latest_turn_anchor(&self, view: ViewMode) -> Option<usize> {
        (0..self.transcript.len())
            .rev()
            .find(|&i| self.is_turn_end(i) || self.is_turn_fallback_tail(i, view))
    }

    /// 某个工具行的输出这会儿真的画在屏幕上（展开、行可见）：用来跳过看不见的进度重绘。
    pub fn is_tool_output_visible(&self, id: &str, view: ViewMode) -> bool {
        if let Some(ix) = self
            .transcript
            .iter()
            .position(|i| matches!(i, ChatItem::Tool(c) if c.id.as_deref() == Some(id)))
        {
            if matches!(self.live_preview(view), LivePreview::Tool(live_ix) if live_ix == ix) {
                return true;
            }
            let ChatItem::Tool(card) = &self.transcript[ix] else { return false };
            // 详细视图里工具行总是展开的，不用 `expanded` 也画
            return (card.expanded || view == ViewMode::Verbose)
                && self.transcript[ix].visible_in(view)
                && self.layout.get(ix).is_some_and(|l| l.body);
        }
        // 子任务详情页里的工具行单独记在 `subagents` 里，页开着并且这条展开了才算看得见
        let Some(task) = self.viewing_task.as_deref() else { return false };
        self.subagents.get(task).is_some_and(|run| {
            run.items.iter().any(|i| {
                matches!(
                    i,
                    ChatItem::Tool(c) if c.id.as_deref() == Some(id) && c.expanded
                )
            })
        })
    }

    /// 屏幕上有没有正在转的过程行（运行中的工具 / 压缩），状态条据此避免再画一个转圈。
    /// 思考中的扫光不是转圈（见 `render_thinking`），不算。
    pub fn has_visible_running_spinner(&self) -> bool {
        let subagent =
            self.viewing_task.as_deref().and_then(|task| self.subagents.get(task)).is_some_and(
                |run| {
                    run.items
                        .iter()
                        .any(|i| matches!(i, ChatItem::Tool(c) if c.phase == ToolPhase::Running))
                },
            );
        subagent
            || self.transcript.iter().enumerate().any(|(i, item)| {
                if !self.layout.get(i).is_some_and(|l| l.body) {
                    return false;
                }
                match item {
                    ChatItem::Tool(c) => c.phase == ToolPhase::Running,
                    ChatItem::Compaction { state, .. } => matches!(state, CompactState::Running),
                    _ => false,
                }
            })
    }

    /// 对话里最后一条用户消息是第几条（从 0 数）。
    pub fn latest_turn(&self) -> Option<usize> {
        self.transcript.iter().filter(|i| matches!(i, ChatItem::User { .. })).count().checked_sub(1)
    }

    /// 最新一轮的撤回情况（[`Session::undo`] 记的是更早一轮时不算）。
    pub fn latest_undo(&self) -> Option<TurnUndo> {
        self.undo.filter(|u| Some(u.turn) == self.latest_turn())
    }

    /// `ix` 所在这一轮（上一条用户消息之后、下一条之前）改过的文件。
    pub fn turn_files(&self, ix: usize) -> Vec<TurnFile> {
        let is_user = |i: &ChatItem| matches!(i, ChatItem::User { .. });
        let start = self.transcript[..ix].iter().rposition(is_user).map_or(0, |p| p + 1);
        let end = self.transcript[ix..]
            .iter()
            .position(is_user)
            .map_or(self.transcript.len(), |p| ix + p);
        turn_files(&self.transcript[start..end], start)
    }

    /// 一轮回复里的全部正文（从上一条用户消息到 `ix`），段与段之间空一行；「复制」用。
    pub fn turn_text(&self, ix: usize) -> String {
        let start = self.transcript[..ix]
            .iter()
            .rposition(|i| matches!(i, ChatItem::User { .. }))
            .map_or(0, |p| p + 1);
        self.transcript[start..=ix]
            .iter()
            .filter_map(|i| match i {
                ChatItem::Agent(t) => Some(t.trim()),
                _ => None,
            })
            .filter(|t| !t.is_empty())
            .collect::<Vec<_>>()
            .join("\n\n")
    }

    /// 发消息时重新跟到底部（哪怕之前往上翻过）
    pub fn follow_tail(&mut self) {
        self.display_scroll = None;
        self.wheel = None;
        self.list.set_follow_mode(FollowMode::Tail);
        let now = Instant::now();
        self.follow.away = false;
        self.follow.armed = false;
        self.follow.quiet_until = Some(now + FOLLOW_QUIET);
        self.follow.force_until = Some(now + FOLLOW_FORCE);
    }

    /// 往上翻过、离开了底部（列表不再自动跟随）：显示「回到底部」按钮。
    /// 用滞回状态（见 [`ScrollFollow`]），不是逐帧拿几何距离判——否则按钮会一直闪。
    pub fn scrolled_away(&self) -> bool {
        self.follow.away && !self.list.is_following_tail()
    }

    /// 离开底部之后又来了新内容（「回到底部」按钮上带个点）
    pub fn has_unseen(&self) -> bool {
        self.content_seq > self.seen_seq
    }

    pub fn user_message_count(&self) -> usize {
        if !self.history_loaded && self.display_snapshot.is_some() {
            self.cached_user_count
        } else {
            self.transcript.iter().filter(|item| matches!(item, ChatItem::User { .. })).count()
        }
    }

    pub fn has_user_message(&self) -> bool {
        (!self.history_loaded && self.meta.is_some())
            || self.transcript.iter().any(|i| matches!(i, ChatItem::User { .. }))
    }

    /// 能否在空闲回收时断开外部 agent；任何待发送、待应答或进行中的状态都排除。
    pub fn idle_detachable(&self) -> bool {
        self.history_loaded
            && self.has_user_message()
            && self.acp_id.is_some()
            && !self.busy
            && !self.stopping
            && !self.resuming
            && (!self.requested || self.ready)
            && self.autoprompt.is_none()
            && self.queued.is_empty()
            && self.worktree_busy.is_none()
            && !self.waiting_permission()
            && !self.waiting_question()
    }

    /// 能否把自研内核的完整历史休眠；连接和会话 ID 保留，显示缓存可另行暂存。
    pub fn idle_hibernatable(&self) -> bool {
        self.idle_detachable()
            && !self.history_loading
            && self.history_error.is_none()
            && !self.compact_queued
            && !self.kernel_rewinding
            && !self.transcript.iter().any(|item| {
                matches!(item, ChatItem::Tool(card) if card.task && card.phase == ToolPhase::Running)
            })
    }

    /// 能否把显示数据（正文、布局表、子任务过程）换成磁盘占位：冷会话化。
    /// 忙、有排队的消息 / 压缩、等应答、正在建 worktree、正在读日志、上次读取失败
    /// （重读同一份多半还失败，先留着错误行）、还连着内核的会话都排除；
    /// 连接中的自研会话走 `display_evictable`，不能用这条整对象替换的路径。
    /// 没落盘的（`meta` 不在）也不动——换占位会把活会话标成「历史会话」。
    /// 换成占位后一切走 `ensure_history_loaded` 的重读路径，和启动占位完全一样。
    pub fn evictable(&self) -> bool {
        self.history_loaded
            && !self.history_loading
            && self.meta.is_some()
            && !self.busy
            && !self.stopping
            && !self.resuming
            && self.autoprompt.is_none()
            && self.queued.is_empty()
            && !self.compact_queued
            && !self.kernel_rewinding
            && self.worktree_busy.is_none()
            && self.history_error.is_none()
            && self.acp_id.is_none()
            && !self.waiting_permission()
            && !self.waiting_question()
    }

    /// 显示数据换回占位会话（元信息、模式、滚动、展开状态保留；正文释放）。
    /// 令牌不变，侧栏和 `last_active` 的引用不受影响。
    pub fn evict_to_placeholder(&mut self, settings: &crate::settings::Settings) {
        let Some(meta) = self.meta.clone() else { return };
        let reveal = self.pending_reveal_turn.take();
        // 显示状态不跟着正文释放：下一次读到日志后仍回到原来的位置和展开状态
        let scroll = Some((self.list.logical_scroll_top(), self.list.is_following_tail()));
        let follow = self.follow;
        let open_groups = std::mem::take(&mut self.open_groups);
        let folded_live_groups = std::mem::take(&mut self.folded_live_groups);
        let changes_open = std::mem::take(&mut self.changes_open);
        let plan_folded = self.plan_folded;
        let mut cold = Self::placeholder(self.token, meta, settings);
        cold.pending_reveal_turn = reveal;
        cold.display_scroll = scroll;
        cold.follow = follow;
        cold.open_groups = open_groups;
        cold.folded_live_groups = folded_live_groups;
        cold.changes_open = changes_open;
        cold.plan_folded = plan_folded;
        // 正在看的子任务详情页是内存态：重开时回到对话本身
        cold.viewing_task = None;
        *self = cold;
    }

    pub fn retained_breakdown(&self) -> DisplayBytes {
        let mut out = DisplayBytes::default();
        for item in self.transcript.iter().chain(self.subagents.values().flat_map(|run| &run.items))
        {
            out.add(item.heap_breakdown());
        }
        out.other += self.layout.len() * std::mem::size_of::<RowLayout>()
            + self.measured.len() * std::mem::size_of::<u64>()
            // 拆块后的文本是复制出来的（渲染要整块 &str），大回复会多占一份
            + self.agent_parts.values().map(|c| c.iter().map(|s| s.len()).sum::<usize>()).sum::<usize>()
            + self
                .subagents
                .values()
                .map(|run| run.prompt.len() + run.agent.len() + 64)
                .sum::<usize>();
        out
    }

    /// 这一页显示数据保留的堆字节（估算，见 [`ChatItem::heap_bytes`]）。
    pub fn retained_bytes(&self) -> usize {
        self.retained_breakdown().total()
    }

    /// 侧栏分组按哪个目录算：worktree 会话归到**主仓库**那一组（`cwd` 是数据目录里的 worktree，
    /// 不该单独占一组），其余就是工作目录。聊天会话没有工作目录（`cwd` 是占位目录），不按它分组。
    pub fn group_dir(&self) -> &std::path::Path {
        self.worktree.as_ref().map_or(self.cwd.as_path(), |w| w.repo.as_path())
    }

    /// 是不是聊天会话（没有工作目录：没有目录 chip、权限模式、`@` 文件补全那套）。
    pub fn is_chat(&self) -> bool {
        self.kind == SessionKind::Chat
    }

    pub fn mode_id(&self) -> Option<&str> {
        if self.is_chat() {
            return None;
        }
        self.modes.get(self.permission_mode).map(|m| m.id.as_str())
    }

    pub fn sync_builtin_modes(&mut self, auto_available: bool) {
        if self.kernel != KernelChoice::Builtin || self.is_chat() {
            return;
        }
        let selected = self.mode_id().map(str::to_string);
        self.replace_modes(
            claudesktop_builtin::permission_modes(auto_available),
            selected.as_deref(),
        );
    }

    fn replace_modes(&mut self, modes: Vec<ModeView>, current: Option<&str>) {
        self.modes = modes
            .into_iter()
            .map(|m| ModeEntry {
                icon: mode_icon(&m.id),
                desc: m.description.unwrap_or_default().into(),
                name: m.name.into(),
                id: m.id,
            })
            .collect();
        self.permission_mode =
            current.and_then(|id| self.modes.iter().position(|m| m.id == id)).unwrap_or(0);
    }

    fn remember_mode(&mut self) {
        let mode = self.mode_id().map(str::to_string);
        if let Some(meta) = &mut self.meta {
            meta.mode = mode;
        }
    }

    /// 新会话默认的权限模式（设置里开着「自动」时是「自动」）：先把这一项补进模式表并选中
    /// （chip、下拉立刻显示它），内核会话建立（Started）后再交给内核；
    /// 内核没提供这个模式就退回它报的那个。
    pub fn default_to(&mut self, mode: &str) {
        let i = match self.modes.iter().position(|m| m.id == mode) {
            Some(i) => i,
            None => {
                self.modes.push(auto_mode_entry());
                self.modes.len() - 1
            }
        };
        self.permission_mode = i;
        self.pending_mode = Some(mode.to_string());
    }

    /// 用户选了模式（下标）：立即生效，与「新对话的默认模式」无关了
    pub fn choose_mode_index(&mut self, i: usize) {
        if i >= self.modes.len() {
            return;
        }
        self.permission_mode = i;
        self.pending_mode = None;
        self.remember_mode();
    }

    /// 有还没应答的权限请求
    pub fn waiting_permission(&self) -> bool {
        self.pending_permission().is_some()
    }

    /// 最早一条还没应答的权限请求（Alt+数字键应答的就是它）
    pub fn pending_permission(&self) -> Option<usize> {
        self.transcript
            .iter()
            .position(|i| matches!(i, ChatItem::Permission { resolved: None, .. }))
    }

    /// 在等用户处理：授权或回答问题（侧栏红点、通知都看这个）
    pub fn waiting_user(&self) -> bool {
        self.waiting_permission() || self.waiting_question() || self.auth.is_some()
    }

    /// 有还没回答的提问
    pub fn waiting_question(&self) -> bool {
        self.pending_question().is_some()
    }

    /// 最早一条还没回答的提问（数字键 / 回车应答的就是它）
    pub fn pending_question(&self) -> Option<usize> {
        self.transcript.iter().position(|i| matches!(i, ChatItem::Question { answer: None, .. }))
    }

    /// 第 `idx` 条是刚发出去、这一轮还在跑的用户消息：返回发出的时刻（入场动画用）。
    pub fn fresh_user(&self, idx: usize) -> Option<Instant> {
        (self.busy && self.last_user() == Some(idx)).then_some(self.turn_started).flatten()
    }

    /// 一轮开始：置忙并开始计时。
    pub fn begin_turn(&mut self) {
        self.flush_stream();
        self.busy = true;
        self.compact_turn = false;
        self.retry = None;
        self.turn_streamed.clear();
        self.turn_started = Some(Instant::now());
        // 上一轮留下的组折叠状态（按行下标）对新一轮没有意义
        self.folded_live_groups.clear();
        self.animated_group = None;
        self.live_think = None;
    }

    /// 本轮此刻的活动：等授权 > 等自动重试 > 正在输出正文（且没有工具在跑）> 其余都算「工作中」。
    /// 只看本轮（最后一条用户消息之后）。
    pub fn activity(&self) -> Activity {
        if self.waiting_permission() {
            return Activity::WaitingPermission;
        }
        if self.waiting_question() {
            return Activity::WaitingQuestion;
        }
        if self.retry.is_some() {
            return Activity::Retrying;
        }
        let turn = &self.transcript[self.turn_start()..];
        if turn
            .iter()
            .any(|i| matches!(i, ChatItem::Compaction { state: CompactState::Running, .. }))
        {
            return Activity::Compacting;
        }
        let running =
            turn.iter().any(|i| matches!(i, ChatItem::Tool(c) if c.phase == ToolPhase::Running));
        match turn.last() {
            Some(ChatItem::Agent(_)) if !running => Activity::Replying,
            _ => Activity::Working,
        }
    }

    pub fn note(&mut self, text: impl Into<SharedString>) {
        self.transcript.push(ChatItem::Note(text.into()));
    }

    /// 记一次「用过」（切到、来事件、收尾一轮）：LRU 回收把最久没碰的先冷掉。
    pub fn touch(&mut self) {
        self.last_touch = Instant::now();
    }

    /// 最近一次用过（或来过事件）的时刻。
    pub fn last_touched(&self) -> Instant {
        self.last_touch
    }

    /// 本轮失败：对话流里放一张带「重试」的卡片（超出上下文的另给「新开会话」）。
    pub fn fail(&mut self, message: impl Into<String>) {
        let message = message.into();
        let kind = FailureKind::of(&message);
        self.transcript.push(ChatItem::Failed { message: message.into(), kind });
    }

    /// 最近一条进行中的压缩行收尾（完成 / 失败）；没有进行中的就什么都不做。
    pub fn settle_compaction(&mut self, result: CompactState) {
        if let Some(ChatItem::Compaction { state, .. }) = self
            .transcript
            .iter_mut()
            .rev()
            .find(|i| matches!(i, ChatItem::Compaction { state: CompactState::Running, .. }))
        {
            *state = result;
        }
    }

    fn apply_acp_compaction(&mut self, compaction: CompactionView) {
        let state = match compaction.status.as_str() {
            "completed" => CompactState::Done(compaction.summary.unwrap_or_default().into()),
            "failed" | "cancelled" => CompactState::Failed(
                compaction.error.or(compaction.summary).unwrap_or(compaction.status.clone()).into(),
            ),
            _ => CompactState::Running,
        };
        if let Some(ChatItem::Compaction { state: current, .. }) =
            self.transcript.iter_mut().find(|item| {
                matches!(item, ChatItem::Compaction { id: Some(id), .. } if *id == compaction.id)
            })
        {
            *current = state;
            return;
        }
        self.transcript.push(ChatItem::Compaction { id: Some(compaction.id), state, open: false });
    }

    /// 第 `ix` 行（失败卡片）所在这一轮在失败前有没有进展（说了话，或者有工具做完了）。
    /// 有的话「重试」会把做过的事再做一遍，失败卡片改给「继续」。
    pub fn turn_progressed(&self, ix: usize) -> bool {
        let start = self.turn_prompt(ix).map_or(0, |u| u + 1);
        self.transcript[start..ix.min(self.transcript.len())].iter().any(|i| match i {
            ChatItem::Agent(text) => !text.trim().is_empty(),
            ChatItem::Tool(card) => card.phase == ToolPhase::Done,
            _ => false,
        })
    }

    /// 第 `ix` 行（失败卡片）是不是最近一轮的：后面没有用户消息了。更早的失败卡片后面
    /// 已经接着聊过，不再给按钮（「重试」会把那一轮的提问在末尾再发一遍，容易误点）。
    /// 卡片上的按钮组受它影响，所以 `row_key` 也读它。
    pub fn failure_is_latest(&self, ix: usize) -> bool {
        matches!(self.transcript.get(ix), Some(ChatItem::Failed { .. }))
            && !self.transcript[ix..].iter().any(|i| matches!(i, ChatItem::User { .. }))
    }

    /// 第 `ix` 行（失败卡片）要不要给「继续」：得是最近一轮而且失败前有进展。
    pub fn failure_resume(&self, ix: usize) -> bool {
        self.failure_is_latest(ix) && self.turn_progressed(ix)
    }

    /// 第 `ix` 行（失败卡片）所在这一轮的用户消息：往前找最近的一条。
    pub fn turn_prompt(&self, ix: usize) -> Option<usize> {
        self.transcript[..ix.min(self.transcript.len())]
            .iter()
            .rposition(|i| matches!(i, ChatItem::User { .. }))
    }

    /// 整段对话导出为 Markdown（「分享」按钮用）。
    pub fn to_markdown(&self, acp: &crate::settings::AcpSettings) -> String {
        let mut out = format!(
            "# {}\n\n> 内核：{} · 模型：{}\n\n",
            self.title,
            self.kernel.label(acp),
            self.model_label
        );
        for item in self.transcript.iter() {
            match item {
                ChatItem::User { text, images, .. } => {
                    out += &format!("## 我\n\n{text}\n\n");
                    if !images.is_empty() {
                        out += &format!("（{} 张图片附件）\n\n", images.len());
                    }
                }
                ChatItem::Agent(text) => out += &format!("## Claude\n\n{text}\n\n"),
                ChatItem::Thinking { text, .. } => {
                    out += &format!(
                        "<details><summary>思考过程</summary>\n\n{text}\n\n</details>\n\n"
                    );
                }
                ChatItem::Info { text, .. } => {
                    out += &format!(
                        "<details><summary>启动信息</summary>\n\n```{text}```\n\n</details>\n\n"
                    );
                }
                ChatItem::Tool(card) => {
                    out += &format!("> **{}** `{}` → {}", card.name, card.arg, card.result);
                    if let Some(grant) = &card.grant {
                        out += &format!("（{}）", grant.label);
                    }
                    out += "\n\n";
                }
                ChatItem::AutoApproved { title, chosen } => {
                    out += &format!("> 已自动批准 `{title}`（{chosen}）\n\n");
                }
                ChatItem::Permission { merged: true, .. } => {}
                ChatItem::Permission { title, resolved, .. } => {
                    let state = resolved.as_deref().unwrap_or("待处理");
                    out += &format!("> 权限请求 `{title}`（{state}）\n\n");
                }
                ChatItem::Question { questions, answer, .. } => {
                    out += "> **提问**\n";
                    for (n, q) in questions.iter().enumerate() {
                        let picked = match answer {
                            Some(claudesktop_core::QuestionAnswer::Choices { choices }) => choices
                                .get(n)
                                .and_then(|i| q.options.get(*i))
                                .map(|o| format!(" → {}", o.label))
                                .unwrap_or_default(),
                            Some(a) => format!("（{}）", a.label()),
                            None => "（待回答）".to_string(),
                        };
                        out += &format!("> - {}{}\n", q.question, picked);
                    }
                    out += "\n";
                }
                ChatItem::Plan(rows) => {
                    out += "**计划**\n\n";
                    for row in rows.iter() {
                        out += &match row.status {
                            PlanStatus::Done => format!("- [x] {}\n", row.text),
                            PlanStatus::InProgress => format!("- [ ] {}（进行中）\n", row.text),
                            PlanStatus::Pending => format!("- [ ] {}\n", row.text),
                        };
                    }
                    out += "\n";
                }
                ChatItem::Note(text) => out += &format!("*{text}*\n\n"),
                ChatItem::Failed { message, .. } => out += &format!("*本轮失败：{message}*\n\n"),
                ChatItem::Compaction { state, .. } => {
                    out += &match state {
                        CompactState::Done(summary) => format!(
                            "*上下文已压缩*\n\n<details><summary>摘要</summary>\n\n{summary}\n\n</details>\n\n"
                        ),
                        CompactState::Failed(message) => format!("*压缩上下文失败：{message}*\n\n"),
                        CompactState::Running => "*正在压缩上下文…*\n\n".into(),
                    }
                }
            }
        }
        out
    }

    /// 把一个流式 chunk 攒进缓冲；距离上次并进够 `STREAM_FLUSH` 了就并进正文。
    /// 重放不缓冲：那是一次性补完，只画一次。
    fn buffer_chunk(&mut self, text: String, thought: bool) {
        if !self.stream_pending.is_empty() && self.stream_pending_thought != thought {
            self.flush_stream();
        }
        self.stream_pending_thought = thought;
        self.stream_pending.push_str(&text);
        if self.replaying || self.stream_flushed_at.elapsed() >= self.stream_flush_interval() {
            self.flush_stream();
        }
    }

    /// 当前该用的合批间隔：随正在写的那条消息的长度在
    /// `STREAM_FLUSH..STREAM_FLUSH_MAX` 之间线性爬坡。
    fn stream_flush_interval(&self) -> std::time::Duration {
        let len = match self.transcript.last() {
            Some(ChatItem::Agent(t)) | Some(ChatItem::Info { text: t, .. }) => t.len(),
            Some(ChatItem::Thinking { text, .. }) => text.len(),
            _ => 0,
        } + self.stream_pending.len();
        let ramp = (STREAM_FLUSH_LONG - STREAM_FLUSH_RAMP) as u64;
        let extra = len.saturating_sub(STREAM_FLUSH_RAMP).min(ramp as usize) as u64;
        STREAM_FLUSH + (STREAM_FLUSH_MAX - STREAM_FLUSH).mul_f64(extra as f64 / ramp as f64)
    }

    /// 把缓冲的流式文本并进正文（思考或回复行）。任何实质事件、回退、停止、轮末到来前
    /// 都必须先并进，让对话状态始终一致；调用方在并进后需要重画（[`Session::take_stream_dirty`]）。
    pub fn flush_stream(&mut self) {
        if self.stream_pending.is_empty() {
            return;
        }
        let text = std::mem::take(&mut self.stream_pending);
        if self.stream_pending_thought {
            match self.transcript.last_mut() {
                Some(ChatItem::Thinking { text: buf, streaming: true, .. }) => buf.push_str(&text),
                // 默认收起：流式时只扫光「思考中…」，不先展开再收回（那样每段思考都会跳一下）
                _ => {
                    let started = (!self.replaying).then(Instant::now);
                    self.transcript.push(ChatItem::thinking(text, started));
                }
            }
        } else {
            // 首条提示之前到达的文本是 agent 的启动信息，折叠显示
            let before_first_prompt = !self.has_user_message();
            match self.transcript.last_mut() {
                Some(ChatItem::Info { text: buf, .. }) if before_first_prompt => {
                    buf.push_str(&text)
                }
                Some(ChatItem::Agent(buf)) => buf.push_str(&text),
                _ if before_first_prompt => {
                    self.transcript.push(ChatItem::Info { text, open: false })
                }
                _ => self.transcript.push(ChatItem::Agent(text)),
            }
        }
        self.stream_dirty = true;
        self.stream_flushed_at = Instant::now();
    }

    /// 上次取走进标志之后，流式缓冲是否又并进过正文。`false` 表示 chunk 只是进了缓冲，
    /// 对话内容没变，调用方不用重画。
    pub fn take_stream_dirty(&mut self) -> bool {
        std::mem::take(&mut self.stream_dirty)
    }

    /// 正在流式的思考块收尾，记下想了多久（计时的才有）。
    /// 展开与否保持原样（默认就是收起的，用户点开过的不替他收）。
    pub fn finish_thinking(&mut self) {
        self.live_think = None;
        if let Some(ChatItem::Thinking { streaming, started, elapsed_ms, .. }) =
            self.transcript.last_mut()
        {
            *streaming = false;
            if let Some(t) = started.take() {
                let ms = t.elapsed().as_millis() as u64;
                *elapsed_ms = Some(ms);
                self.thought_done = Some(ms);
            }
        }
    }

    /// 取走刚收尾的那段思考的用时（调用方写进日志，重放时还原「已思考 N 秒」）。
    pub fn take_thought_done(&mut self) -> Option<u64> {
        self.thought_done.take()
    }

    /// 应答某条权限请求：`Some((选项 ID, 展示名))` 选择，`None` 取消。
    /// 选的是「允许一次」而且卡片上取消勾选了一部分块时，把剩下的块作为按块同意交给内核。
    pub fn resolve_permission(
        &mut self,
        idx: usize,
        choice: Option<(String, String)>,
        kernel: Option<&Arc<dyn Kernel>>,
    ) {
        let Some(kernel) = kernel else {
            return;
        };
        let Some(ChatItem::Permission { id, resolved, keep, .. }) = self.transcript.get_mut(idx)
        else {
            return;
        };
        if resolved.is_some() {
            return;
        }
        let id = *id;
        let label: SharedString = match &choice {
            Some((_, name)) => name.clone().into(),
            None => "已取消".into(),
        };
        // 只有「允许一次」才按块写：选「总是允许」是存规则，之后整个动作都放行
        let allow_once = choice.as_ref().is_some_and(|(id, _)| id == "allow_once");
        let (grant, label) = match (allow_once, keep.take()) {
            (true, Some(hunks)) => {
                let text: SharedString = format!("{label}（只写 {} 块）", hunks.len()).into();
                (Some(claudesktop_core::PartialGrant { hunks }), text)
            }
            _ => (None, label),
        };
        *resolved = Some(match kernel.resolve_permission_with(id, choice.map(|(i, _)| i), grant) {
            Ok(()) => label,
            Err(err) => format!("应答失败：{err}").into(),
        });
        self.merge_permission(idx);
    }

    /// 回答一次提问：把答案交给内核，卡片收成一行。
    /// 内核那边已经不在了（重放出来的、或这一轮刚被停掉）时按取消收尾并说明一句。
    pub fn resolve_question(
        &mut self,
        idx: usize,
        answer: claudesktop_core::QuestionAnswer,
        kernel: Option<&Arc<dyn Kernel>>,
    ) {
        let Some(ChatItem::Question { id, answer: slot, .. }) = self.transcript.get_mut(idx) else {
            return;
        };
        if slot.is_some() {
            return;
        }
        let id = *id;
        let Some(kernel) = kernel else {
            return;
        };
        let (answer, err) = match kernel.resolve_question(id, answer.clone()) {
            Ok(()) => (answer, None),
            Err(err) => (claudesktop_core::QuestionAnswer::Cancelled, Some(err)),
        };
        if let Some(ChatItem::Question { answer: slot, .. }) = self.transcript.get_mut(idx) {
            *slot = Some(answer);
        }
        if let Some(err) = err {
            self.note(format!("这个提问已经不在内核那边了：{err}"));
        }
    }

    /// 点选提问里第 `q` 题的第 `option` 个选项（还没提交时才动）。
    pub fn pick_question(&mut self, idx: usize, q: usize, option: usize) {
        let Some(ChatItem::Question {
            questions, picks, multi_picks, answered, answer: None, ..
        }) = self.transcript.get_mut(idx)
        else {
            return;
        };
        if questions.get(q).is_some_and(|item| item.multi_select && option < item.options.len())
            && let Some(selected) = multi_picks.get_mut(q)
        {
            if let Some(existing) = selected.iter().position(|value| *value == option) {
                selected.remove(existing);
            } else {
                selected.push(option);
            }
            return;
        }
        if questions.get(q).is_some_and(|item| option < item.options.len())
            && let Some(slot) = picks.get_mut(q)
        {
            *slot = Some(option);
            if let Some(done) = answered.get_mut(q) {
                *done = true;
            }
        }
    }

    /// 数字键选选项：选第一道还没选的题，都选过了就改最后一题。
    pub fn pick_question_key(&mut self, idx: usize, option: usize) {
        let Some(q) = self.question_cursor(idx) else {
            return;
        };
        self.pick_question(idx, q, option);
    }

    /// 多选保留当前面板，点「提交 / 继续」后才进入下一项。
    pub fn confirm_question(&mut self, idx: usize) -> bool {
        let Some(q) = self.question_cursor(idx) else {
            return false;
        };
        let Some(ChatItem::Question { questions, multi_picks, answered, answer: None, .. }) =
            self.transcript.get_mut(idx)
        else {
            return false;
        };
        let Some(question) = questions.get(q).filter(|question| question.multi_select) else {
            return false;
        };
        if question.required && multi_picks.get(q).is_none_or(|selected| selected.is_empty()) {
            return false;
        }
        let Some(done) = answered.get_mut(q) else {
            return false;
        };
        *done = true;
        true
    }

    pub fn answer_text_question(&mut self, idx: usize, q: usize, text: String) {
        let Some(ChatItem::Question { questions, text_answers, answered, answer: None, .. }) =
            self.transcript.get_mut(idx)
        else {
            return;
        };
        if !questions.get(q).is_some_and(|question| {
            matches!(
                question.kind,
                claudesktop_core::QuestionKind::Text
                    | claudesktop_core::QuestionKind::Number
                    | claudesktop_core::QuestionKind::Integer
            )
        }) {
            return;
        }
        if let Some(value) = text_answers.get_mut(q) {
            *value = Some(text);
        }
        if let Some(done) = answered.get_mut(q) {
            *done = true;
        }
    }

    /// 每题都选好了才能提交：返回每题选中的下标。
    pub fn question_choices(&self, idx: usize) -> Option<Vec<usize>> {
        let ChatItem::Question { questions, picks, answer: None, .. } = self.transcript.get(idx)?
        else {
            return None;
        };
        if questions.iter().any(|question| question.multi_select) {
            return None;
        }
        picks.iter().copied().collect()
    }

    pub fn pending_freeform(&self) -> Option<(usize, usize, claudesktop_core::QuestionKind)> {
        self.transcript.iter().enumerate().find_map(|(idx, item)| {
            let ChatItem::Question { questions, answer: None, .. } = item else {
                return None;
            };
            let q = self.question_cursor(idx)?;
            let question = questions.get(q)?;
            matches!(
                question.kind,
                claudesktop_core::QuestionKind::Text
                    | claudesktop_core::QuestionKind::Number
                    | claudesktop_core::QuestionKind::Integer
            )
            .then_some((idx, q, question.kind))
        })
    }

    pub fn form_answers(&self, idx: usize) -> Option<Vec<claudesktop_core::FormValueView>> {
        let ChatItem::Question {
            questions, picks, multi_picks, text_answers, answer: None, ..
        } = self.transcript.get(idx)?
        else {
            return None;
        };
        let mut answers = Vec::with_capacity(questions.len());
        for (q, question) in questions.iter().enumerate() {
            let id = question.id.clone()?;
            let option = |index: usize| {
                let option = question.options.get(index)?;
                Some(option.value.clone().unwrap_or_else(|| option.label.clone()))
            };
            let value = match (question.kind, question.multi_select) {
                (_, true) => {
                    let selected = multi_picks.get(q)?;
                    if question.required && selected.is_empty() {
                        return None;
                    }
                    claudesktop_core::FormValueKind::Strings(
                        selected.iter().map(|index| option(*index)).collect::<Option<Vec<_>>>()?,
                    )
                }
                (claudesktop_core::QuestionKind::Choice, false) => {
                    let Some(selected) = picks.get(q).copied().flatten() else {
                        if question.required {
                            return None;
                        }
                        continue;
                    };
                    claudesktop_core::FormValueKind::Text(option(selected)?)
                }
                (claudesktop_core::QuestionKind::Boolean, false) => {
                    let Some(selected) = picks.get(q).copied().flatten() else {
                        if question.required {
                            return None;
                        }
                        continue;
                    };
                    claudesktop_core::FormValueKind::Boolean(selected == 0)
                }
                (kind, false) => {
                    let text = match text_answers.get(q).and_then(|text| text.clone()) {
                        Some(text) => text,
                        None if question.required => return None,
                        None => continue,
                    };
                    if question.required && text.trim().is_empty() {
                        return None;
                    }
                    match kind {
                        claudesktop_core::QuestionKind::Text => {
                            claudesktop_core::FormValueKind::Text(text)
                        }
                        claudesktop_core::QuestionKind::Number => {
                            if text.trim().is_empty() && !question.required {
                                continue;
                            }
                            let number =
                                text.trim().parse::<f64>().ok().filter(|n| n.is_finite())?;
                            claudesktop_core::FormValueKind::Number(number)
                        }
                        claudesktop_core::QuestionKind::Integer => {
                            if text.trim().is_empty() && !question.required {
                                continue;
                            }
                            claudesktop_core::FormValueKind::Integer(text.trim().parse().ok()?)
                        }
                        _ => return None,
                    }
                }
            };
            answers.push(claudesktop_core::FormValueView { id, value });
        }
        Some(answers)
    }

    /// 提问面板当前停在哪一题：第一道还没答完的；都答过了就停在最后一题。
    pub fn question_cursor(&self, idx: usize) -> Option<usize> {
        let ChatItem::Question { questions, picks, answered, answer: None, .. } =
            self.transcript.get(idx)?
        else {
            return None;
        };
        let last = questions.len().saturating_sub(1);
        if answered.len() == questions.len() {
            Some(answered.iter().position(|done| !done).unwrap_or(last))
        } else {
            Some(picks.iter().position(|pick| pick.is_none()).unwrap_or(last))
        }
    }

    /// 面板上的「上一题」：把最近答过的一题退回去重答，再答就顺着往下走。
    pub fn back_question(&mut self, idx: usize) {
        let Some(ChatItem::Question {
            questions,
            picks,
            multi_picks,
            text_answers,
            answered,
            answer: None,
            ..
        }) = self.transcript.get_mut(idx)
        else {
            return;
        };
        if answered.len() == questions.len() {
            if let Some(q) = answered.iter().rposition(|done| *done) {
                answered[q] = false;
                picks[q] = None;
                multi_picks[q].clear();
                text_answers[q] = None;
            }
        } else if let Some(slot) = picks.iter_mut().rev().find(|pick| pick.is_some()) {
            *slot = None;
        }
    }
}

/// 权限应答的展示名去掉按块写的后缀（「允许一次（只写 2 块）」→「允许一次」），
/// 用来对回内核给的选项名。只认这一种后缀：agent 自己的选项名里也可能带括号（「允许（本次会话）」）。
fn partial_base(label: &SharedString) -> SharedString {
    let base = label.strip_suffix(" 块）").and_then(|rest| {
        let at = rest.rfind("（只写 ")?;
        let n = &rest[at + "（只写 ".len()..];
        (!n.is_empty() && n.chars().all(|c| c.is_ascii_digit())).then(|| &rest[..at])
    });
    base.map_or_else(|| label.clone(), SharedString::from)
}

/// 一段（Markdown）文字的第一行有内容的话，去掉标题 / 列表 / 引用记号，截到通知里放得下。
fn preview_line(text: &str) -> Option<String> {
    const MAX_CHARS: usize = 80;
    let line = text
        .lines()
        .map(|l| l.trim_start_matches(['#', '>', '-', '*', ' ']).trim())
        .find(|l| !l.is_empty() && !l.starts_with("```"))?;
    let line = line.replace("**", "").replace('`', "");
    Some(match line.char_indices().nth(MAX_CHARS) {
        Some((at, _)) => format!("{}…", &line[..at]),
        None => line,
    })
}

/// 回退后对话里的那行备注（实时回退和重放日志共用）。
pub fn rewind_note(conversation: bool, files: usize, prompt: &str) -> String {
    let prompt = crate::model::title_from_prompt(prompt);
    match (conversation, files) {
        (true, 0) => format!("已回退到「{prompt}」之前"),
        (true, n) => format!("已回退到「{prompt}」之前，还原了 {n} 个文件"),
        (false, n) => format!("已把 {n} 个文件还原到「{prompt}」之前的样子"),
    }
}

#[cfg(test)]
mod tests {
    use super::layout::split_markdown;
    use super::*;
    use claudesktop_core::{ConfigChoiceView, PermissionOptionView};

    // 注意：本文件顶层 `use gpui_kit::*` 会把 gpui 的 test 宏带进作用域，
    // 遮蔽内建 #[test]，必须用全路径（原文件其他测试同理）。

    /// 渐隐跟着滚动位置收放：滚不动时两条都不画（不然最后一行会被底下那条盖住一截）。
    #[core::prelude::v1::test]
    fn scroll_edge_hides_fades_at_the_ends() {
        assert_eq!(ScrollEdge::of(px(0.), px(0.)), ScrollEdge::Fits);
        assert_eq!(ScrollEdge::Fits.heights(), (Rems::ZERO, Rems::ZERO));
        // 差 1px 以内算到头
        assert_eq!(ScrollEdge::of(px(0.5), px(300.)), ScrollEdge::Top);
        assert_eq!(ScrollEdge::Top.heights(), (Rems::ZERO, FADE_BOTTOM));
        assert_eq!(ScrollEdge::of(px(300.), px(0.5)), ScrollEdge::Bottom);
        assert_eq!(ScrollEdge::Bottom.heights(), (FADE_TOP, Rems::ZERO));
        assert_eq!(ScrollEdge::of(px(40.), px(40.)), ScrollEdge::Middle);
        assert_eq!(ScrollEdge::Middle.heights(), (FADE_TOP, FADE_BOTTOM));
    }

    #[core::prelude::v1::test]
    fn edge_fades_ramp_with_distance() {
        let rem = px(16.);
        // 不满一屏：两条都不画
        assert_eq!(edge_fades(px(0.), px(0.), rem), (px(0.), px(0.)));
        // 贴顶：顶上那条 0，底下满格
        let (top, bottom) = edge_fades(px(0.), px(1000.), rem);
        assert_eq!(top, px(0.));
        assert_eq!(bottom, FADE_BOTTOM.to_pixels(rem));
        // 中间：顶上那条按距离连续长高，不到满格
        let (top, _) = edge_fades(px(12.), px(1000.), rem);
        assert!(top > px(1.) && top < FADE_TOP.to_pixels(rem));
        // 贴底：底下那条 0
        let (_, bottom) = edge_fades(px(1000.), px(0.), rem);
        assert_eq!(bottom, px(0.));
    }

    #[core::prelude::v1::test]
    fn split_markdown_breaks_at_paragraphs() {
        let para = "这是一段很长的说明文字，用来把回复撑过长回复的阈值。".repeat(12);
        let text: String = (0..12).map(|_| format!("{para}\n\n")).collect();
        let chunks = split_markdown(&text);
        assert!(chunks.len() > 1, "{}", chunks.len());
        assert!(chunks.iter().all(|c| !c.is_empty()));
        // 空行处断，拼回去和原文一字不差
        assert_eq!(chunks.concat(), text);
    }

    #[core::prelude::v1::test]
    fn split_markdown_reopens_code_fences() {
        let code: String = (0..400).map(|_| "let value = compute(1);\n").collect();
        let text = format!("# 示例\n\n```rust\n{code}```\n");
        let chunks = split_markdown(&text);
        assert!(chunks.len() > 1);
        // 每块的围栏行成对（断在代码块里时补了收尾、下一块重开）
        for chunk in &chunks {
            let fences = chunk.lines().filter(|l| l.trim_start().starts_with("```")).count();
            assert_eq!(fences % 2, 0, "围栏没配平：{chunk:?}");
        }
        assert!(chunks.iter().all(|c| c.contains("let value")));
    }

    #[core::prelude::v1::test]
    fn split_markdown_repeats_table_header() {
        let head = "| 名称 | 说明 |\n| --- | --- |\n";
        let rows: String = (0..600).map(|i| format!("| 行 {i} | 说明文字 |\n")).collect();
        let text = format!("{head}{rows}");
        let chunks = split_markdown(&text);
        assert!(chunks.len() > 1);
        for chunk in chunks.iter().skip(1) {
            assert!(chunk.starts_with(head), "表格块缺表头：{chunk:?}");
        }
    }

    #[core::prelude::v1::test]
    fn split_markdown_has_no_ghost_header_blocks() {
        let head = "| a | b |\n| --- | --- |\n";
        let rows: String = (0..500).map(|i| format!("| {i} | x |\n")).collect();
        let text = format!("{head}{rows}\n收尾段落。\n");
        let chunks = split_markdown(&text);
        assert!(chunks.len() > 1);
        for chunk in &chunks {
            if chunk.starts_with(head) {
                assert!(chunk.len() > head.len() + 2, "只有表头没有表格行的块：{chunk:?}");
            }
        }
        // 表格后面的正文没有被空表头隔开
        assert!(chunks.iter().any(|c| c.contains("收尾段落")));
    }

    #[core::prelude::v1::test]
    fn split_markdown_matches_fence_char_and_length() {
        // 4 个反引号开的块里，3 个反引号的行不能当收尾
        let inner: String = (0..400).map(|_| "```\nlet x = 1;\n").collect();
        let text = format!("````markdown\n{inner}````\n");
        let chunks = split_markdown(&text);
        assert!(chunks.len() > 1);
        let fences = |c: &str| c.lines().filter(|l| l.trim_start().starts_with("````")).count();
        for chunk in &chunks {
            assert_eq!(fences(chunk) % 2, 0, "4 反引号围栏没配平：{chunk:?}");
        }
    }

    #[core::prelude::v1::test]
    fn user_input_cancels_force_follow() {
        let mut s = Session::new(1, KernelChoice::Builtin, PathBuf::from("."));
        s.follow_tail();
        assert!(s.follow.force_until.is_some());
        // 往上滚（以及 Ctrl+Home 走的 note_input(false)）当场作废
        s.note_input(false);
        assert!(s.follow.force_until.is_none());
        s.follow_tail();
        s.pin_row(0);
        assert!(s.follow.force_until.is_none());
        s.follow_tail();
        s.drop_force_follow();
        assert!(s.follow.force_until.is_none());
    }

    /// 按钮显隐只按几何滞回：滚轮余量还在、也没有向下的输入方向时，
    /// 只要已经回到距底 80px 内，「回到底部」也要立刻消失（拖滚动条回底部同理）。
    #[core::prelude::v1::test]
    fn away_clears_near_bottom_while_wheel_is_settling() {
        let mut s = Session::new(1, KernelChoice::Builtin, PathBuf::from("."));
        s.list.pause_following_tail();
        s.follow.away = true;
        // 未布局时距底按 0 算；余量是往上翻，且 last_input_down 不是 Some(true)
        let now = Instant::now();
        s.wheel = Some(WheelInput::new(now, -px(10.)));
        s.sync_follow(now, false);
        assert!(!s.follow.away, "距底 80px 内按钮要消失");
        // 只清按钮，不自动吸附（没有向下的输入方向）
        assert!(!s.list.is_following_tail());
    }

    /// 距底 80px 内松手：不瞬移视口，只进入待命；真来了新内容才平滑滑回底部。
    #[core::prelude::v1::test]
    fn sync_follow_arms_within_80px_without_teleporting() {
        let detach = px(600.);
        // 往下滚停在距底 50px：原地待命，不吸底
        assert_eq!(follow_step(px(50.), detach, true, Some(true), false, false), FollowStep::Arm);
        // 待命期间没有新内容：保持在待命态（不重复置位、也不吸底）
        assert_eq!(follow_step(px(50.), detach, true, Some(true), true, false), FollowStep::Hold);
        // 待命期间真来了新内容：平滑滑回底部
        assert_eq!(
            follow_step(px(50.), detach, true, Some(true), true, true),
            FollowStep::GlideToTail
        );
        // 已经贴到 1px 内：直接交回跟底，没有视觉跳变
        assert_eq!(follow_step(px(0.5), detach, true, Some(true), true, true), FollowStep::Attach);
        // 没往下滚（往上翻）别自动吸附
        assert_eq!(follow_step(px(50.), detach, true, Some(false), false, false), FollowStep::Hold);
        // 尾部还没测出来时不吸附（估算高度会让「到底了」说谎）
        assert_eq!(follow_step(px(50.), detach, false, Some(true), false, false), FollowStep::Hold);
        // 翻出一屏半：脱离跟底
        assert_eq!(
            follow_step(px(900.), detach, true, Some(true), false, false),
            FollowStep::Detach
        );
    }

    /// 用户主动往上翻 / 重定位会当场清掉吸附带的待命态。
    #[core::prelude::v1::test]
    fn user_actions_clear_the_armed_state() {
        let mut s = Session::new(1, KernelChoice::Builtin, PathBuf::from("."));
        s.follow.armed = true;
        s.note_input(false);
        assert!(!s.follow.armed);
        s.follow.armed = true;
        s.drop_force_follow();
        assert!(!s.follow.armed);
        s.follow.armed = true;
        s.follow_tail();
        assert!(!s.follow.armed);
    }

    #[core::prelude::v1::test]
    fn large_finished_reply_splits_into_rows() {
        let para = "拆块之后每行只排自己那一块，滚动时不用整篇重排。".repeat(10);
        let text: String = (0..20).map(|_| format!("{para}\n\n")).collect();
        let mut s = Session::new(1, KernelChoice::Builtin, PathBuf::from("."));
        s.transcript.push(ChatItem::Agent(text.clone()));
        s.transcript.push(ChatItem::Agent("短回复不拆".into()));
        s.sync_list(ViewMode::Normal);
        let parts = s.part_count(0).expect("大回复要拆块");
        assert!(parts > 1);
        assert_eq!(s.rows.len(), parts + 1, "第二条整条一行");
        assert_eq!(s.row_of(0), 0);
        assert_eq!(s.row_of(1), parts);
        assert_eq!(s.list.item_count(), parts + 2, "末尾还有运行状态行");
        assert_eq!(s.measured.len(), s.rows.len());
        // 记录下标仍能在 `layout` 里查（渲染用）
        assert_eq!(s.layout.len(), 2);
        let joined: String =
            (0..parts).filter_map(|p| s.part_text(0, p)).map(|t| t.to_string()).collect();
        assert_eq!(joined, text);
    }

    #[core::prelude::v1::test]
    fn streaming_reply_is_not_split() {
        let para = "流式写的时候每来一个 chunk 都要重排，拆了反而白花功夫。".repeat(10);
        let text: String = (0..20).map(|_| format!("{para}\n\n")).collect();
        let mut s = Session::new(1, KernelChoice::Builtin, PathBuf::from("."));
        s.transcript.push(ChatItem::Agent(text));
        s.busy = true;
        s.sync_list(ViewMode::Normal);
        assert_eq!(s.part_count(0), None);
        assert_eq!(s.rows.len(), 1);
        // 这一轮结束：同一段文本开始拆块，行数跟着变
        s.busy = false;
        s.sync_list(ViewMode::Normal);
        assert!(s.part_count(0).is_some_and(|n| n > 1));
        assert_eq!(s.rows.len(), s.part_count(0).unwrap());
    }

    #[core::prelude::v1::test]
    fn markdown_export_covers_item_kinds() {
        let mut s = Session::new(1, KernelChoice::Demo, PathBuf::from("C:/proj"));
        s.title = "测试会话".into();
        s.note("一条备注");
        let user =
            ChatItem::User { text: "你好".into(), images: Vec::new(), files: Default::default() };
        let agent = ChatItem::Agent("**回复**".into());
        let card = ToolCard {
            id: None,
            kind: "read".into(),
            name: "Read".into(),
            arg: "README.md".into(),
            result: "12 行".into(),
            output: SharedString::default(),
            content: SharedString::default(),
            phase: ToolPhase::Done,
            diffs: Vec::new(),
            expanded: false,
            task: false,
            grant: None,
            images: Vec::new(),
        };
        let tool = ChatItem::Tool(card);
        let row = PlanRow { status: PlanStatus::Pending, text: "第一步".into() };
        let plan = ChatItem::Plan(vec![row]);
        s.transcript.push(user);
        s.transcript.push(agent);
        s.transcript.push(tool);
        s.transcript.push(plan);
        let md = s.to_markdown(&Default::default());
        assert!(md.starts_with("# 测试会话"));
        assert!(md.contains("## 我"));
        assert!(md.contains("你好"));
        assert!(md.contains("**Read**"));
        assert!(md.contains("README.md"));
        assert!(md.contains("- [ ] 第一步"));
        assert!(md.contains("一条备注"));
    }

    /// 失败卡片只在「最近一轮 + 失败前有进展」时给「继续」；更早的失败后面已经接着聊过了。
    #[core::prelude::v1::test]
    fn failure_resume_needs_progress_and_latest_turn() {
        let mut s = Session::new(1, KernelChoice::Demo, PathBuf::from("C:/proj"));
        let user = |t: &str| ChatItem::User {
            text: t.into(),
            images: Vec::new(),
            files: Default::default(),
        };
        let tool = || {
            ChatItem::Tool(ToolCard {
                id: None,
                kind: "edit".into(),
                name: "Edit".into(),
                arg: "a.rs".into(),
                result: "ok".into(),
                output: SharedString::default(),
                content: SharedString::default(),
                phase: ToolPhase::Done,
                diffs: Vec::new(),
                expanded: false,
                task: false,
                grant: None,
                images: Vec::new(),
            })
        };
        *s.transcript = vec![
            user("改一下"),
            tool(),
            ChatItem::Failed {
                message: "读取流失败: reset".into(),
                kind: crate::model::FailureKind::Interrupted,
            },
        ];
        // 最近一轮、有进展 → 给「继续」
        assert!(s.failure_resume(2));
        assert!(s.failure_is_latest(2));
        // 后面又聊过了 → 不给「继续」，卡片上也不再有按钮
        s.transcript.push(user("再来"));
        assert!(!s.failure_resume(2));
        assert!(!s.failure_is_latest(2));
        // 不是失败卡片
        assert!(!s.failure_is_latest(0));
        // 失败前什么都没做 → 不给（那会儿该「重试」）
        let mut fresh = Session::new(2, KernelChoice::Demo, PathBuf::from("C:/proj"));
        *fresh.transcript = vec![
            user("改一下"),
            ChatItem::Failed {
                message: "HTTP 500".into(),
                kind: crate::model::FailureKind::Server,
            },
        ];
        assert!(!fresh.failure_resume(1));
        // 不是失败卡片 → 不给
        assert!(!fresh.failure_resume(0));
    }

    fn meta() -> SessionMeta {
        SessionMeta {
            id: "s1".into(),
            title: "读 README".into(),
            cwd: "C:/proj".into(),
            agent: "mock".into(),
            kernel_session_id: Some("acp-1".into()),
            kernel_sessions: Vec::new(),
            created_ms: 0,
            updated_ms: 0,
            archived: false,
            task: false,
            worktree: None,
            kind: SessionKind::Code,
            web: None,
            mode: None,
            queued: Vec::new(),
        }
    }

    fn ev(event: KernelEvent) -> Record {
        Record::Event { event }
    }

    fn permission(id: u64) -> Record {
        ev(KernelEvent::PermissionRequest {
            id,
            title: "Write a.txt".into(),
            options: vec![PermissionOptionView {
                id: "allow".into(),
                name: "Yes".into(),
                kind: "allow_once".into(),
            }],
            // 旧日志：不知道是哪个工具调用的，应答后照旧单独一行
            tool: None,
            hunks: None,
        })
    }

    #[core::prelude::v1::test]
    fn placeholder_keeps_history_identity_without_transcript() {
        let s = Session::placeholder(3, meta(), &Default::default());
        assert!(s.restored && s.has_user_message());
        assert!(!s.history_loaded && !s.history_loading);
        assert!(s.transcript.is_empty());
        assert_eq!(s.title.as_ref(), "读 README");
    }

    #[core::prelude::v1::test]
    fn idle_detachable_excludes_unloaded_and_busy_sessions() {
        let mut s = Session::new(4, KernelChoice::Acp("a".into()), PathBuf::from("."));
        s.transcript.push(ChatItem::User {
            text: "已结束".into(),
            images: Vec::new(),
            files: Default::default(),
        });
        s.acp_id = Some("remote".into());
        s.requested = true;
        s.ready = true;
        assert!(s.idle_detachable());
        s.busy = true;
        assert!(!s.idle_detachable());
        s.busy = false;
        s.history_loaded = false;
        assert!(!s.idle_detachable());
    }

    #[core::prelude::v1::test]
    fn idle_hibernatable_excludes_compaction_and_kernel_rewind() {
        let mut s = Session::new(4, KernelChoice::Builtin, PathBuf::from("."));
        s.transcript.push(ChatItem::User {
            text: "已结束".into(),
            images: Vec::new(),
            files: Default::default(),
        });
        s.acp_id = Some("builtin-1-1".into());
        s.requested = true;
        s.ready = true;
        assert!(s.idle_hibernatable());
        s.compact_queued = true;
        assert!(!s.idle_hibernatable());
        s.compact_queued = false;
        s.kernel_rewinding = true;
        assert!(!s.idle_hibernatable());
    }

    #[core::prelude::v1::test]
    fn evictable_excludes_live_and_connected_sessions() {
        let mut s = Session::from_records(1, meta(), vec![Record::user("问")], &Default::default());
        assert!(s.evictable(), "未连内核的历史会话可冷");
        // 连着内核会话的（ACP 或自研）不动：内核上下文还活着，界面上丢了正文对不上
        s.acp_id = Some("remote".into());
        assert!(!s.evictable());
        s.acp_id = None;
        s.busy = true;
        assert!(!s.evictable());
        s.busy = false;
        s.queued.push(crate::model::QueuedPrompt {
            text: "排队".into(),
            images: Vec::new(),
            files: Vec::new(),
        });
        assert!(!s.evictable());
        s.queued.clear();
        s.compact_queued = true;
        assert!(!s.evictable());
        s.compact_queued = false;
        s.history_loading = true;
        assert!(!s.evictable());
        s.history_loading = false;
        s.history_error = Some("读不了".into());
        assert!(!s.evictable());
        s.history_error = None;
        s.stopping = true;
        assert!(!s.evictable());
        s.stopping = false;
        assert!(s.evictable());
    }

    #[core::prelude::v1::test]
    fn evict_to_placeholder_keeps_identity_and_frees_transcript() {
        let records = vec![
            Record::user("读 README"),
            ev(KernelEvent::AgentMessageChunk { text: "总结完毕".into() }),
            Record::Turn {
                at_ms: 0,
                model: Some("m".into()),
                input_tokens: 1,
                output_tokens: 2,
                elapsed_ms: Some(3),
                context_tokens: Some(4),
            },
        ];
        let mut s = Session::from_records(9, meta(), records, &Default::default());
        let before = s.retained_bytes();
        assert!(before > 0);
        let token = s.token;
        let title = s.title.clone();
        let view = s.viewing_task.is_some();
        s.evict_to_placeholder(&Default::default());
        assert_eq!(s.token, token, "令牌不变（侧栏和 last_active 的引用不受影响）");
        assert_eq!(s.title, title);
        assert!(s.transcript.is_empty());
        assert!(!s.history_loaded && !s.history_loading);
        assert!(s.has_user_message(), "占位对删除保护也算已发过消息");
        assert!(s.retained_bytes() < before);
        assert!(!view);
    }

    #[core::prelude::v1::test]
    fn retained_bytes_counts_tools_and_subagents() {
        let mut s = Session::new(1, KernelChoice::Builtin, PathBuf::from("."));
        assert_eq!(s.retained_bytes(), 0);
        s.transcript.push(ChatItem::Agent("一句话".into()));
        let one = s.retained_bytes();
        assert!(one > 0);
        s.transcript.push(ChatItem::Tool(ToolCard {
            id: None,
            kind: "read".into(),
            name: "Read".into(),
            arg: "src/a.rs".into(),
            result: "120 行".into(),
            output: SharedString::default(),
            content: SharedString::default(),
            phase: ToolPhase::Done,
            diffs: vec![DiffView {
                path: "src/a.rs".into(),
                created: false,
                added: 1,
                removed: 0,
                lines: vec![claudesktop_core::DiffLineView {
                    tag: '+',
                    no: 1,
                    text: "let x = 1;".into(),
                }],
                truncated: false,
            }],
            expanded: false,
            task: false,
            grant: None,
            images: Vec::new(),
        }));
        assert!(s.retained_bytes() > one);
        s.subagents.insert(
            "t1".into(),
            SubagentRun {
                agent: "explore".into(),
                prompt: "找一找".into(),
                items: vec![ChatItem::Agent("找到了".into())],
            },
        );
        assert!(s.retained_bytes() > s.transcript.iter().map(ChatItem::heap_bytes).sum::<usize>());
    }

    #[core::prelude::v1::test]
    fn replay_rebuilds_transcript() {
        let records = vec![
            Record::user("读 README"),
            ev(KernelEvent::AgentThoughtChunk { text: "想一想".into() }),
            ev(KernelEvent::ToolCallStarted {
                id: "t1".into(),
                title: "Read README.md".into(),
                status: "in_progress".into(),
                kind: "read".into(),
                parent: None,
            }),
            // 思考的用时记在收尾它的那条事件后面
            Record::Thought { elapsed_ms: 4_200 },
            ev(KernelEvent::ToolCallUpdated {
                id: "t1".into(),
                title: None,
                status: Some("completed".into()),
                raw_output: Some("# claudesktop".into()),
                output: Some(
                    "# claudesktop

桌面端"
                        .into(),
                ),
            }),
            permission(1),
            Record::PermissionResolved { id: 1, label: "Yes".into() },
            ev(KernelEvent::AgentMessageChunk { text: "总结完毕".into() }),
            ev(KernelEvent::TurnEnded { stop_reason: "end_turn".into() }),
            Record::Turn {
                at_ms: 1,
                model: None,
                input_tokens: 900,
                output_tokens: 80,
                elapsed_ms: Some(6_000),
                context_tokens: Some(520),
            },
            // 上次退出时没应答的请求；前面那段思考没记用时（旧日志）
            Record::user("再写个文件"),
            ev(KernelEvent::AgentThoughtChunk { text: "旧的".into() }),
            permission(2),
        ];
        let s = Session::from_records(7, meta(), records, &Default::default());

        assert!(s.restored && !s.ready && !s.busy);
        assert_eq!(s.title.as_ref(), "读 README");
        assert_eq!(s.cwd, PathBuf::from("C:/proj"));
        let kinds: Vec<&str> = s
            .transcript
            .iter()
            .map(|i| match i {
                ChatItem::User { .. } => "user",
                ChatItem::Thinking { .. } => "thinking",
                ChatItem::Tool(_) => "tool",
                ChatItem::Permission { .. } => "permission",
                ChatItem::Agent(_) => "agent",
                ChatItem::Failed { kind: FailureKind::Unfinished, .. } => "unfinished",
                _ => "other",
            })
            .collect();
        // 上次退出时那一轮没做完：末尾补一张「没做完」的卡片
        assert_eq!(
            kinds,
            [
                "user",
                "thinking",
                "tool",
                "permission",
                "agent",
                "user",
                "thinking",
                "permission",
                "unfinished"
            ]
        );
        // 用时从日志还原；重放本身不计时，没记用时的就不知道
        assert!(matches!(
            &s.transcript[1],
            ChatItem::Thinking { streaming: false, elapsed_ms: Some(4_200), started: None, .. }
        ));
        assert!(matches!(&s.transcript[6], ChatItem::Thinking { elapsed_ms: None, .. }));
        assert_eq!(s.context_tokens, 520);
        assert!(matches!(&s.transcript[2], ChatItem::Tool(c) if c.phase == ToolPhase::Done));
        // 输出预览随记录恢复，点开工具行还能看
        assert!(
            matches!(&s.transcript[2], ChatItem::Tool(c) if c.output.as_ref() == "# claudesktop

桌面端")
        );
        let labels: Vec<_> = s
            .transcript
            .iter()
            .filter_map(|i| match i {
                ChatItem::Permission { resolved, .. } => resolved.as_ref().map(|r| r.to_string()),
                _ => None,
            })
            .collect();
        assert_eq!(labels, ["Yes", "已取消"]);
    }

    #[core::prelude::v1::test]
    fn diffs_attach_to_tool_card_and_replace_by_path() {
        let started = ev(KernelEvent::ToolCallStarted {
            id: "e1".into(),
            title: "Edit a.txt".into(),
            status: "pending".into(),
            kind: "edit".into(),
            parent: None,
        });
        let diff = |new: &str| {
            ev(KernelEvent::ToolCallDiff {
                id: "e1".into(),
                diff: DiffView::compute(
                    "a.txt",
                    Some(
                        "x
",
                    ),
                    new,
                ),
            })
        };
        let records = vec![
            Record::user("改"),
            started,
            diff(
                "y
",
            ),
            diff(
                "z
",
            ),
            ev(KernelEvent::ToolCallDiff {
                id: "e1".into(),
                diff: DiffView::compute(
                    "b.txt", None, "b
",
                ),
            }),
        ];
        let s = Session::from_records(1, meta(), records, &Default::default());
        let ChatItem::Tool(card) = &s.transcript[1] else { panic!("tool card") };
        let paths: Vec<_> = card.diffs.iter().map(|d| d.path.as_str()).collect();
        assert_eq!(paths, ["a.txt", "b.txt"]);
        assert_eq!(card.diffs[0].lines.last().unwrap().text, "z");
    }

    #[core::prelude::v1::test]
    fn stream_flush_interval_ramps_with_message_length() {
        let mut s = Session::new(1, KernelChoice::Demo, PathBuf::from("."));
        s.transcript.push(ChatItem::User {
            text: "hi".into(),
            images: Vec::new(),
            files: Default::default(),
        });
        s.transcript.push(ChatItem::Agent(String::new()));
        assert_eq!(s.stream_flush_interval(), STREAM_FLUSH);
        if let Some(ChatItem::Agent(t)) = s.transcript.last_mut() {
            *t = "x".repeat(STREAM_FLUSH_LONG);
        }
        assert_eq!(s.stream_flush_interval(), STREAM_FLUSH_MAX);
        // 缓冲里还没并进的也算长度
        if let Some(ChatItem::Agent(t)) = s.transcript.last_mut() {
            t.clear();
        }
        s.stream_pending = "x".repeat(STREAM_FLUSH_LONG);
        assert_eq!(s.stream_flush_interval(), STREAM_FLUSH_MAX);
        // 中间档在两端之间
        s.stream_pending = "x".repeat((STREAM_FLUSH_RAMP + STREAM_FLUSH_LONG) / 2);
        let mid = s.stream_flush_interval();
        assert!(mid > STREAM_FLUSH && mid < STREAM_FLUSH_MAX);
    }

    #[core::prelude::v1::test]
    fn list_tracks_new_and_changed_rows() {
        let mut s = Session::new(1, KernelChoice::Demo, PathBuf::from("."));
        s.transcript.push(ChatItem::User {
            text: "hi".into(),
            images: Vec::new(),
            files: Default::default(),
        });
        s.transcript.push(ChatItem::Agent("a".into()));
        s.sync_list(ViewMode::Normal);
        // 记录 2 行 + 末尾状态行
        assert_eq!((s.list.item_count(), s.measured.len()), (3, 2));
        let before = s.measured[1];
        // 流式追加：行数不变，指纹变了
        if let Some(ChatItem::Agent(t)) = s.transcript.last_mut() {
            t.push_str("bc");
        }
        s.transcript.push(ChatItem::Note("n".into()));
        s.sync_list(ViewMode::Normal);
        assert_eq!(s.list.item_count(), 4);
        assert_ne!(s.measured[1], before);
        assert_eq!(s.measured[1], s.row_key(1, ViewMode::Normal));
        // 换视图模式：所有行的指纹都变，整表重测
        let normal = s.measured.clone();
        s.sync_list(ViewMode::Summary);
        assert!(normal.iter().zip(&s.measured).all(|(a, b)| a != b));
        // 折叠 / 展开也算高度变化
        let closed = ChatItem::thinking("x", None);
        let mut open = ChatItem::thinking("x", None);
        if let ChatItem::Thinking { open, .. } = &mut open {
            *open = true;
        }
        assert_ne!(open.fingerprint(), closed.fingerprint());
    }

    #[core::prelude::v1::test]
    fn turn_end_and_turn_text() {
        let mut s = Session::new(1, KernelChoice::Demo, PathBuf::from("."));
        let user =
            || ChatItem::User { text: "q".into(), images: Vec::new(), files: Default::default() };
        *s.transcript = vec![
            user(),
            ChatItem::Agent("a".into()),
            ChatItem::Plan(Vec::new()),
            ChatItem::Agent(" b ".into()),
            ChatItem::Note("已停止".into()),
            user(),
            ChatItem::Agent("c".into()),
        ];
        // 后面还有过程行的不算；跳过备注后紧跟用户消息的算
        assert!(!s.is_turn_end(1));
        assert!(s.is_turn_end(3));
        assert!(!s.is_turn_end(0));
        // 最后一段：这一轮结束了才算
        assert!(s.is_turn_end(6));
        s.busy = true;
        assert!(!s.is_turn_end(6));
        // 复制整轮正文，不跨到上一轮
        assert_eq!(s.turn_text(3), "a\n\nb");
        assert_eq!(s.turn_text(6), "c");
    }

    #[core::prelude::v1::test]
    fn running_command_shows_live_output_until_it_finishes() {
        let mut s = Session::new(1, KernelChoice::Builtin, PathBuf::from("."));
        s.begin_turn();
        s.apply_event(
            KernelEvent::ToolCallStarted {
                id: "c1".into(),
                title: "`cargo build`".into(),
                status: "pending".into(),
                kind: "execute".into(),
                parent: None,
            },
            None,
        );
        fn card(s: &Session) -> &ToolCard {
            match s.transcript.last() {
                Some(ChatItem::Tool(c)) => c,
                _ => panic!("没有工具行"),
            }
        }
        // 还没输出也能点开（看完整命令）
        assert!(card(&s).can_open() && card(&s).output.is_empty());
        let progress =
            |text: &str| KernelEvent::ToolCallProgress { id: "c1".into(), output: text.into() };
        s.apply_event(progress("Compiling a"), None);
        assert_eq!(card(&s).output.as_ref(), "Compiling a");
        s.apply_event(
            KernelEvent::ToolCallUpdated {
                id: "c1".into(),
                title: None,
                status: Some("completed".into()),
                raw_output: Some("完成".into()),
                output: Some("Finished".into()),
            },
            None,
        );
        // 跑完后晚到的进度不覆盖最终输出
        s.apply_event(progress("Compiling b"), None);
        assert_eq!(card(&s).output.as_ref(), "Finished");
    }

    #[core::prelude::v1::test]
    fn acp_structured_content_survives_raw_updates_and_replay() {
        let records = vec![
            Record::user("读取"),
            ev(KernelEvent::ToolCallStarted {
                id: "t1".into(),
                title: "多模态结果".into(),
                status: "in_progress".into(),
                kind: "other".into(),
                parent: None,
            }),
            ev(KernelEvent::ToolCallDiff {
                id: "t1".into(),
                diff: claudesktop_core::DiffView::compute("a.rs", Some("旧"), "新"),
            }),
            ev(KernelEvent::ToolCallContent { id: "t1".into(), output: "资源正文".into() }),
            ev(KernelEvent::ToolCallUpdated {
                id: "t1".into(),
                title: None,
                status: Some("completed".into()),
                raw_output: Some("原始输出".into()),
                output: Some("原始预览".into()),
            }),
        ];
        let s = Session::from_records(1, meta(), records, &Default::default());
        let card = s
            .transcript
            .iter()
            .find_map(|item| match item {
                ChatItem::Tool(card) => Some(card),
                _ => None,
            })
            .expect("工具卡");
        assert_eq!(card.content.as_ref(), "资源正文");
        assert_eq!(card.output.as_ref(), "原始预览");
        assert_eq!(card.diffs.len(), 1);
        assert!(card.can_open());
    }

    #[core::prelude::v1::test]
    fn acp_session_state_updates_stay_out_of_turn_usage() {
        let mut s = Session::new(1, KernelChoice::Acp("custom-1".into()), PathBuf::from("."));
        s.apply_event(
            KernelEvent::Started {
                token: 1,
                agent_name: "mock".into(),
                session_id: "s1".into(),
                model: Some("model-a".into()),
                modes: vec![
                    ModeView {
                        id: "default".into(), name: "逐项确认".into(), description: None
                    },
                    ModeView { id: "plan".into(), name: "计划".into(), description: None },
                ],
                current_mode: Some("default".into()),
                resumed: false,
            },
            None,
        );
        s.apply_event(
            KernelEvent::ConfigOptionsChanged {
                options: vec![ConfigOptionView {
                    id: "model".into(),
                    name: "模型".into(),
                    description: None,
                    category: "model".into(),
                    current_value: "model-b".into(),
                    options: vec![ConfigChoiceView {
                        id: "model-b".into(),
                        name: "模型 B".into(),
                        description: None,
                        group: Some("快速".into()),
                    }],
                }],
            },
            None,
        );
        s.apply_event(KernelEvent::CurrentModeChanged { mode_id: "plan".into() }, None);
        s.apply_event(
            KernelEvent::AvailableCommandsChanged {
                commands: vec![AvailableCommandView {
                    name: "skill:review".into(),
                    description: "审查代码".into(),
                    requires_input: true,
                }],
            },
            None,
        );
        s.apply_event(
            KernelEvent::ContextUsageUpdated {
                usage: ContextUsageView {
                    used: 1200,
                    size: 10000,
                    cost: Some(CostView { amount: 0.125, currency: "USD".into() }),
                },
            },
            None,
        );
        assert_eq!(s.model_label.as_ref(), "model-b");
        assert_eq!(s.mode_id(), Some("plan"));
        assert_eq!(s.config_options[0].options[0].group.as_deref(), Some("快速"));
        assert!(s.available_commands[0].requires_input);
        assert_eq!(s.context_tokens, 1200);
        assert_eq!(s.context_size, Some(10000));
        assert_eq!(s.context_cost.as_ref().map(|cost| cost.amount), Some(0.125));
        assert_eq!(s.turn_usage, TurnUsage::default());
    }

    #[core::prelude::v1::test]
    fn auth_required_keeps_panel_until_success() {
        let mut s = Session::new(1, KernelChoice::Acp("custom-1".into()), PathBuf::from("."));
        let methods = vec![claudesktop_core::AuthMethodView {
            id: "login".into(),
            name: "Mock 登录".into(),
            description: None,
            kind: claudesktop_core::AuthMethodKind::Agent,
        }];
        s.apply_event(
            KernelEvent::AuthRequired { token: 1, methods, message: "需要登录".into() },
            None,
        );
        assert!(s.waiting_user());
        s.apply_event(
            KernelEvent::AuthCompleted {
                method_id: "login".into(),
                success: false,
                message: "再试一次".into(),
            },
            None,
        );
        assert!(s.auth.as_ref().is_some_and(|auth| auth.message.as_ref() == "再试一次"));
        s.apply_event(
            KernelEvent::AuthCompleted {
                method_id: "login".into(),
                success: true,
                message: "登录成功".into(),
            },
            None,
        );
        assert!(s.auth.is_none());
        assert!(!s.waiting_user());
    }

    #[core::prelude::v1::test]
    fn acp_compaction_updates_patch_the_same_row() {
        let mut s = Session::new(1, KernelChoice::Acp("custom-1".into()), PathBuf::from("."));
        for (status, summary) in [("in_progress", None), ("completed", Some("完整摘要".into()))]
        {
            s.apply_event(
                KernelEvent::CompactionUpdated {
                    compaction: CompactionView {
                        id: "compact-1".into(),
                        status: status.into(),
                        summary,
                        error: None,
                    },
                },
                None,
            );
        }
        assert_eq!(s.transcript.len(), 1);
        assert!(matches!(
            &s.transcript[0],
            ChatItem::Compaction { id: Some(id), state: CompactState::Done(summary), .. }
                if id == "compact-1" && summary.as_ref() == "完整摘要"
        ));
        s.apply_event(
            KernelEvent::CompactionUpdated {
                compaction: CompactionView {
                    id: "compact-1".into(),
                    status: "failed".into(),
                    summary: None,
                    error: Some("压缩失败".into()),
                },
            },
            None,
        );
        assert_eq!(s.transcript.len(), 1);
        assert!(matches!(
            &s.transcript[0],
            ChatItem::Compaction { state: CompactState::Failed(message), .. }
                if message.as_ref() == "压缩失败"
        ));
    }

    #[core::prelude::v1::test]
    fn multi_select_and_boolean_form_values_keep_ids_and_types() {
        let mut s = Session::new(1, KernelChoice::Acp("custom-1".into()), PathBuf::from("."));
        let questions = vec![
            claudesktop_core::QuestionView {
                question: "组件".into(),
                header: None,
                options: vec![
                    claudesktop_core::QuestionOptionView {
                        label: "核心".into(),
                        description: None,
                        value: Some("core".into()),
                    },
                    claudesktop_core::QuestionOptionView {
                        label: "界面".into(),
                        description: None,
                        value: Some("ui".into()),
                    },
                ],
                id: Some("multi".into()),
                kind: claudesktop_core::QuestionKind::Choice,
                multi_select: true,
                required: true,
                default_value: None,
            },
            claudesktop_core::QuestionView {
                question: "启用".into(),
                header: None,
                options: vec![
                    claudesktop_core::QuestionOptionView {
                        label: "是".into(),
                        description: None,
                        value: Some("true".into()),
                    },
                    claudesktop_core::QuestionOptionView {
                        label: "否".into(),
                        description: None,
                        value: Some("false".into()),
                    },
                ],
                id: Some("flag".into()),
                kind: claudesktop_core::QuestionKind::Boolean,
                multi_select: false,
                required: true,
                default_value: None,
            },
        ];
        s.apply_event(KernelEvent::UserQuestion { id: 7, tool: None, questions }, None);
        s.pick_question(0, 0, 0);
        s.pick_question(0, 0, 1);
        s.pick_question(0, 1, 0);
        let answers = s.form_answers(0).expect("表单答案");
        assert_eq!(answers.len(), 2);
        assert_eq!(answers[0].id, "multi");
        assert_eq!(answers[1].id, "flag");
        assert!(matches!(
            &answers[0].value,
            claudesktop_core::FormValueKind::Strings(values) if values == &vec!["core".to_string(), "ui".to_string()]
        ));
        assert!(matches!(answers[1].value, claudesktop_core::FormValueKind::Boolean(true)));
    }

    #[core::prelude::v1::test]
    fn mixed_form_waits_for_every_field_and_preserves_types() {
        let option = |label: &str, value: &str| claudesktop_core::QuestionOptionView {
            label: label.into(),
            description: None,
            value: Some(value.into()),
        };
        let field = |id: &str, kind| claudesktop_core::QuestionView {
            id: Some(id.into()),
            question: id.into(),
            kind,
            required: true,
            ..Default::default()
        };
        let mut choice = field("choice", claudesktop_core::QuestionKind::Choice);
        choice.options = vec![option("稳妥", "safe"), option("快速", "fast")];
        let mut multi = field("multi", claudesktop_core::QuestionKind::Choice);
        multi.multi_select = true;
        multi.options = vec![option("核心", "core"), option("界面", "ui")];
        let mut flag = field("flag", claudesktop_core::QuestionKind::Boolean);
        flag.options = vec![option("是", "true"), option("否", "false")];
        let questions = vec![
            choice,
            multi,
            field("name", claudesktop_core::QuestionKind::Text),
            field("ratio", claudesktop_core::QuestionKind::Number),
            field("count", claudesktop_core::QuestionKind::Integer),
            flag,
        ];
        let mut s = Session::new(1, KernelChoice::Acp("custom-1".into()), PathBuf::from("."));
        s.apply_event(KernelEvent::UserQuestion { id: 8, tool: None, questions }, None);

        s.pick_question(0, 0, 0);
        assert_eq!(s.question_cursor(0), Some(1));
        assert!(s.form_answers(0).is_none());
        s.pick_question(0, 1, 0);
        s.pick_question(0, 1, 1);
        assert_eq!(s.question_cursor(0), Some(1));
        assert!(s.confirm_question(0));
        assert_eq!(s.question_cursor(0), Some(2));
        s.answer_text_question(0, 2, "demo".into());
        assert_eq!(s.question_cursor(0), Some(3));
        assert!(s.form_answers(0).is_none());
        s.answer_text_question(0, 3, "3.5".into());
        s.answer_text_question(0, 4, "3".into());
        assert_eq!(s.question_cursor(0), Some(5));
        assert!(s.form_answers(0).is_none());
        s.pick_question(0, 5, 0);

        let answers = s.form_answers(0).expect("完整表单答案");
        assert_eq!(answers.len(), 6);
        assert!(matches!(
            &answers[0].value,
            claudesktop_core::FormValueKind::Text(value) if value == "safe"
        ));
        assert!(matches!(
            &answers[1].value,
            claudesktop_core::FormValueKind::Strings(values)
                if values == &vec!["core".to_string(), "ui".to_string()]
        ));
        assert!(matches!(
            &answers[2].value,
            claudesktop_core::FormValueKind::Text(value) if value == "demo"
        ));
        assert!(matches!(answers[3].value, claudesktop_core::FormValueKind::Number(3.5)));
        assert!(matches!(answers[4].value, claudesktop_core::FormValueKind::Integer(3)));
        assert!(matches!(answers[5].value, claudesktop_core::FormValueKind::Boolean(true)));
    }

    #[core::prelude::v1::test]
    fn subagent_events_go_to_their_own_task() {
        let mut s = Session::new(1, KernelChoice::Builtin, PathBuf::from("."));
        s.transcript.push(ChatItem::User {
            text: "查".into(),
            images: Vec::new(),
            files: Default::default(),
        });
        s.begin_turn();
        let started = |id: &str, parent: Option<&str>| KernelEvent::ToolCallStarted {
            id: id.into(),
            title: format!("Read {id}"),
            status: "pending".into(),
            kind: "read".into(),
            parent: parent.map(Into::into),
        };
        let chunk = |task: &str, text: &str, thought: bool| KernelEvent::SubagentChunk {
            task: task.into(),
            text: text.into(),
            thought,
        };
        s.apply_event(started("t1", None), None);
        s.apply_event(started("t2", None), None);
        s.apply_event(
            KernelEvent::SubagentStarted {
                task: "t1".into(),
                agent: "scout".into(),
                prompt: "找 A".into(),
            },
            None,
        );
        // 两个子任务的事件交错着到
        s.apply_event(chunk("t1", "想", true), None);
        s.apply_event(started("t2/c1", Some("t2")), None);
        s.apply_event(chunk("t1", "想", true), None);
        s.apply_event(started("t1/c1", Some("t1")), None);
        s.apply_event(
            KernelEvent::ToolCallUpdated {
                id: "t1/c1".into(),
                title: None,
                status: Some("completed".into()),
                raw_output: Some("3 行".into()),
                output: None,
            },
            None,
        );
        s.apply_event(chunk("t1", "A 在", false), None);
        s.apply_event(chunk("t1", "这里", false), None);

        // 对话记录里只有两张子任务卡片，不算过程步骤
        assert_eq!(s.transcript.len(), 3);
        for item in &s.transcript[1..] {
            assert!(matches!(item, ChatItem::Tool(c) if c.task) && !item.is_step());
        }
        let t1 = &s.subagents["t1"];
        assert_eq!((t1.agent.as_ref(), t1.prompt.as_ref()), ("scout", "找 A"));
        assert_eq!(t1.items.len(), 3);
        assert!(
            matches!(&t1.items[0], ChatItem::Thinking { text, streaming: false, .. } if text == "想想")
        );
        assert_eq!(t1.steps(), 1);
        assert!(matches!(&t1.items[1], ChatItem::Tool(c) if c.phase == ToolPhase::Done));
        assert_eq!(t1.report(), Some("A 在这里"));
        // 旧日志没有 SubagentStarted：照样归到子任务里
        let t2 = &s.subagents["t2"];
        assert!(t2.prompt.is_empty() && t2.steps() == 1 && t2.report().is_none());
        // 停下时子任务里转圈的调用一起收掉
        s.interrupt_running_tools();
        assert!(
            matches!(&s.subagents["t2"].items[0], ChatItem::Tool(c) if c.phase == ToolPhase::Failed)
        );
    }

    #[core::prelude::v1::test]
    fn retry_shows_until_the_next_real_event() {
        let mut s = Session::new(1, KernelChoice::Builtin, PathBuf::from("."));
        s.transcript.push(ChatItem::User {
            text: "hi".into(),
            images: Vec::new(),
            files: Default::default(),
        });
        s.begin_turn();
        let retrying = KernelEvent::Retrying {
            status: 429,
            attempt: 1,
            delay_ms: 1000,
            reason: "HTTP 429: x".into(),
        };
        s.apply_event(retrying, None);
        assert_eq!(s.activity(), Activity::Retrying);
        assert_eq!(s.retry.as_ref().map(|r| (r.status, r.attempt)), Some((429, 1)));
        // 用量 / 诊断不算重发成功
        s.apply_event(KernelEvent::Status("x".into()), None);
        assert!(s.retry.is_some());
        s.apply_event(KernelEvent::AgentMessageChunk { text: "好".into() }, None);
        assert!(s.retry.is_none());
        assert_eq!(s.activity(), Activity::Replying);
    }

    #[core::prelude::v1::test]
    fn cancelled_turn_settles_tools_and_stopping() {
        let mut s = Session::new(1, KernelChoice::Builtin, PathBuf::from("."));
        s.transcript.push(ChatItem::User {
            text: "跑一下".into(),
            images: Vec::new(),
            files: Default::default(),
        });
        s.begin_turn();
        s.transcript.push(ChatItem::Tool(ToolCard {
            id: None,
            kind: "execute".into(),
            name: "Bash".into(),
            arg: "echo hi".into(),
            result: SharedString::default(),
            output: SharedString::default(),
            content: SharedString::default(),
            phase: ToolPhase::Running,
            diffs: Vec::new(),
            expanded: false,
            task: false,
            grant: None,
            images: Vec::new(),
        }));
        // 用户停下：等内核回结束，期间工具行不该一直转圈
        s.busy = false;
        s.stopping = true;
        s.apply_event(KernelEvent::TurnEnded { stop_reason: "cancelled".into() }, None);
        assert!(!s.stopping);
        assert!(!s.transcript.iter().any(|i| matches!(i, ChatItem::Note(_))));
        let Some(ChatItem::Tool(card)) = s.transcript.get(1) else { panic!() };
        assert!(card.phase == ToolPhase::Failed && card.result.as_ref() == "已中断");
    }

    #[core::prelude::v1::test]
    fn compaction_row_tracks_progress_and_drops_the_gauge() {
        let mut s = Session::new(1, KernelChoice::Builtin, PathBuf::from("."));
        s.transcript.push(ChatItem::User {
            text: "hi".into(),
            images: Vec::new(),
            files: Default::default(),
        });
        s.transcript.push(ChatItem::Agent("好".into()));
        s.context_tokens = 900;
        // 空闲时手动压缩：自成一轮
        s.begin_turn();
        s.apply_event(KernelEvent::Compacting, None);
        assert_eq!(s.activity(), Activity::Compacting);
        s.apply_event(KernelEvent::Compacted { summary: "摘要".into(), context_tokens: 120 }, None);
        assert_eq!(s.context_tokens, 120);
        assert!(matches!(
            s.transcript.last(),
            Some(ChatItem::Compaction { state: CompactState::Done(t), .. }) if t.as_ref() == "摘要"
        ));
        s.apply_event(KernelEvent::TurnEnded { stop_reason: "end_turn".into() }, None);
        assert!(!s.busy);
        // 压缩行不挡住上一段回复下面的「复制」
        assert!(s.is_turn_end(1));
        // 停在压缩中途：那一行收成「已取消」，不会一直转圈
        s.begin_turn();
        s.apply_event(KernelEvent::Compacting, None);
        s.apply_event(KernelEvent::TurnEnded { stop_reason: "cancelled".into() }, None);
        let row = s.transcript.iter().rev().find(|i| matches!(i, ChatItem::Compaction { .. }));
        assert!(matches!(
            row,
            Some(ChatItem::Compaction { state: CompactState::Failed(t), .. }) if t.as_ref() == "已取消"
        ));
    }

    /// 按块同意的后缀只去掉自己加的那种，agent 选项名里本来的括号不动。
    #[core::prelude::v1::test]
    fn partial_base_strips_only_the_hunk_suffix() {
        let base = |s: &str| partial_base(&SharedString::from(s.to_string())).to_string();
        assert_eq!(base("允许一次（只写 2 块）"), "允许一次");
        assert_eq!(base("允许（本次会话）"), "允许（本次会话）");
        assert_eq!(base("允许（本次会话）（只写 12 块）"), "允许（本次会话）");
        assert_eq!(base("允许一次（只写 几 块）"), "允许一次（只写 几 块）");
    }

    /// 排着的压缩是给旧内核会话的：换内核、内核出错都撤掉并说一句，不留到新会话里和第一轮撞上。
    #[core::prelude::v1::test]
    fn queued_compaction_is_dropped_on_detach_and_fatal() {
        let notes = |s: &Session| {
            s.transcript
                .iter()
                .filter(|i| matches!(i, ChatItem::Note(t) if t.contains("压缩上下文")))
                .count()
        };
        let mut s = Session::new(1, KernelChoice::Acp("custom-1".into()), PathBuf::from("."));
        s.compact_queued = true;
        s.detach_kernel(None);
        assert!(!s.compact_queued);
        assert_eq!(notes(&s), 1);
        // 没排着就什么都不说
        s.detach_kernel(None);
        assert_eq!(notes(&s), 1);
        s.compact_queued = true;
        s.on_fatal("断了", None);
        assert!(!s.compact_queued);
        assert_eq!(notes(&s), 2);
    }

    #[core::prelude::v1::test]
    fn detach_kernel_interrupts_turn_and_marks_for_resume() {
        let mut s = Session::new(1, KernelChoice::Acp("custom-1".into()), PathBuf::from("."));
        s.apply_event(
            KernelEvent::Started {
                token: 1,
                agent_name: "a".into(),
                session_id: "k1".into(),
                model: Some("m".into()),
                modes: Vec::new(),
                current_mode: None,
                resumed: false,
            },
            None,
        );
        assert!(s.ready);
        // 空会话：脱离后等新内核重新创建
        s.detach_kernel(None);
        assert!(!s.ready && !s.restored && s.acp_id.is_none() && !s.requested);

        s.acp_id = Some("k2".into());
        s.ready = true;
        s.transcript.push(ChatItem::User {
            text: "hi".into(),
            images: Vec::new(),
            files: Default::default(),
        });
        s.apply_event(
            KernelEvent::ToolCallStarted {
                id: "t".into(),
                title: "Read a".into(),
                status: "in_progress".into(),
                kind: "read".into(),
                parent: None,
            },
            None,
        );
        s.busy = true;
        s.detach_kernel(None);
        // 有对话：变成历史会话，进行中的工具收尾
        assert!(s.restored && !s.busy && !s.ready);
        assert!(
            s.transcript
                .iter()
                .any(|i| matches!(i, ChatItem::Tool(c) if c.phase == ToolPhase::Failed))
        );
        assert!(matches!(s.transcript.last(), Some(ChatItem::Note(_))));
    }

    #[core::prelude::v1::test]
    fn activity_follows_turn_and_usage_accumulates() {
        let mut s = Session::new(1, KernelChoice::Acp("custom-1".into()), PathBuf::from("."));
        // 上一轮遗留的执行中工具不算进本轮
        s.transcript.push(ChatItem::User {
            text: "上一轮".into(),
            images: Vec::new(),
            files: Default::default(),
        });
        s.apply_event(
            KernelEvent::ToolCallStarted {
                id: "old".into(),
                title: "Read a".into(),
                status: "in_progress".into(),
                kind: "read".into(),
                parent: None,
            },
            None,
        );
        s.transcript.push(ChatItem::User {
            text: "这一轮".into(),
            images: Vec::new(),
            files: Default::default(),
        });
        s.begin_turn();
        assert!(s.busy && s.turn_started.is_some());
        assert_eq!(s.activity(), Activity::Working);

        s.apply_event(KernelEvent::AgentThoughtChunk { text: "想".into() }, None);
        assert_eq!(s.activity(), Activity::Working);

        s.apply_event(
            KernelEvent::ToolCallStarted {
                id: "t".into(),
                title: "Bash cargo test".into(),
                status: "in_progress".into(),
                kind: "execute".into(),
                parent: None,
            },
            None,
        );
        assert_eq!(s.activity(), Activity::Working);
        // 有工具在跑时，哪怕中间插了一段正文也还是「工作中」
        s.apply_event(KernelEvent::AgentMessageChunk { text: "先跑测试".into() }, None);
        assert_eq!(s.activity(), Activity::Working);

        s.apply_event(
            KernelEvent::ToolCallUpdated {
                id: "t".into(),
                title: None,
                status: Some("completed".into()),
                raw_output: None,
                output: None,
            },
            None,
        );
        s.apply_event(KernelEvent::AgentMessageChunk { text: "好了".into() }, None);
        assert_eq!(s.activity(), Activity::Replying);

        s.apply_event(permission_event(7), None);
        assert_eq!(s.activity(), Activity::WaitingPermission);

        for _ in 0..2 {
            s.apply_event(
                KernelEvent::UsageUpdated {
                    input_tokens: 100,
                    output_tokens: 30,
                    reasoning_tokens: 10,
                    context_tokens: 130,
                    cache_read_tokens: 80,
                },
                None,
            );
        }
        assert_eq!(
            s.turn_usage,
            TurnUsage { input: 200, output: 60, reasoning: 20, reported: true }
        );
        assert_eq!(s.cache_hit, Some(80));
        // 上下文用量取最近一次报告的，不累加
        assert_eq!(s.context_tokens, 130);
        // 压缩 / 子会话的用量（不带上下文大小）只并进累计，不顶掉主会话的命中率和上下文大小
        s.apply_event(
            KernelEvent::UsageUpdated {
                input_tokens: 50,
                output_tokens: 5,
                reasoning_tokens: 0,
                context_tokens: 0,
                cache_read_tokens: 0,
            },
            None,
        );
        assert_eq!(s.cache_hit, Some(80));
        assert_eq!(s.context_tokens, 130);
        assert_eq!(s.turn_usage.input, 250);
        // 一轮结束：用量和用时记到这一轮名下，回复那行能查到
        let summary = s.close_turn();
        assert_eq!(summary.usage.input, 250);
        assert!(summary.elapsed_ms.is_some() && s.turn_started.is_none());
        assert_eq!(s.turn_usage, TurnUsage::default());
        let reply = s.transcript.len() - 2;
        assert_eq!(s.turn_summary(reply), Some(&summary));
        // 上一轮没有统计
        assert_eq!(s.turn_summary(1), None);
    }

    /// 状态行上的实时 token：端点报过的 + 之后还在流式（靠字数估）的，端点一报就清掉估算。
    #[core::prelude::v1::test]
    fn live_output_tokens_adds_reported_and_streamed() {
        let mut s = Session::new(1, KernelChoice::Acp("custom-1".into()), PathBuf::from("."));
        s.transcript.push(ChatItem::User {
            text: "q".into(),
            images: Vec::new(),
            files: Default::default(),
        });
        s.begin_turn();
        assert_eq!(s.live_output_tokens(), 0);

        // 端点还什么都没报：英文 8 字节 ≈ 2 token、中文 4 字 = 4 token
        s.apply_event(KernelEvent::AgentMessageChunk { text: "abcdefgh".into() }, None);
        s.apply_event(KernelEvent::AgentThoughtChunk { text: "想一想啥".into() }, None);
        assert_eq!(s.live_output_tokens(), 6);

        // 端点报了一次用量：之前那批算过了，估算从现在重新开始
        s.apply_event(
            KernelEvent::UsageUpdated {
                input_tokens: 100,
                output_tokens: 12,
                reasoning_tokens: 0,
                context_tokens: 0,
                cache_read_tokens: 0,
            },
            None,
        );
        assert_eq!(s.live_output_tokens(), 12);
        s.apply_event(KernelEvent::AgentMessageChunk { text: "再".into() }, None);
        assert_eq!(s.live_output_tokens(), 13);
        // 一轮收尾后不再挂着流式估算
        s.close_turn();
        assert_eq!(s.live_output_tokens(), 0);
    }

    #[core::prelude::v1::test]
    fn live_thinking_is_timed_once() {
        let mut s = Session::new(1, KernelChoice::Acp("custom-1".into()), PathBuf::from("."));
        s.transcript.push(ChatItem::User {
            text: "q".into(),
            images: Vec::new(),
            files: Default::default(),
        });
        s.apply_event(thought("想"), None);
        s.apply_event(thought("再想"), None);
        assert!(matches!(&s.transcript[1], ChatItem::Thinking { started: Some(_), .. }));
        assert_eq!(s.take_thought_done(), None);
        s.apply_event(read("a"), None);
        assert!(matches!(
            &s.transcript[1],
            ChatItem::Thinking { elapsed_ms: Some(_), started: None, .. }
        ));
        // 用时只交出一次；之后再收尾（已经不在流式）不重复
        assert!(s.take_thought_done().is_some());
        s.finish_thinking();
        assert_eq!(s.take_thought_done(), None);
    }

    fn read(id: &str) -> KernelEvent {
        KernelEvent::ToolCallStarted {
            id: id.into(),
            title: format!("Read {id}.rs"),
            status: "completed".into(),
            kind: "read".into(),
            parent: None,
        }
    }

    fn thought(text: &str) -> KernelEvent {
        KernelEvent::AgentThoughtChunk { text: text.into() }
    }

    fn started(modes: &[(&str, &str)]) -> KernelEvent {
        KernelEvent::Started {
            token: 1,
            agent_name: "a".into(),
            session_id: "k1".into(),
            model: Some("m".into()),
            modes: modes
                .iter()
                .map(|(id, name)| claudesktop_core::ModeView {
                    id: (*id).into(),
                    name: (*name).into(),
                    description: None,
                })
                .collect(),
            current_mode: Some("default".into()),
            resumed: false,
        }
    }

    #[core::prelude::v1::test]
    fn new_session_default_mode_reaches_the_kernel() {
        let modes = [("default", "逐项确认"), ("auto", "自动")];
        // 设置里开着「自动」：新会话一建就把 chip 显示成「自动」，Started 后交给内核
        let mut s = Session::new(1, KernelChoice::Builtin, PathBuf::from("."));
        s.default_to(MODE_AUTO_ID);
        assert_eq!(s.modes[s.permission_mode].name.as_ref(), "自动");
        s.apply_event(started(&modes), None);
        assert_eq!(s.modes[s.permission_mode].id, MODE_AUTO_ID);
        // 只在这一次生效：之后重开的（如回退）不会又改回「自动」
        assert!(s.pending_mode.is_none());
        s.choose_mode_index(0);
        s.apply_event(started(&modes), None);
        assert_eq!(s.modes[s.permission_mode].id, "default");

        // 内核不认这个模式（ACP agent）：退回它报的那个，不留下「自动」
        let mut s = Session::new(2, KernelChoice::Acp("custom-1".into()), PathBuf::from("."));
        s.default_to(MODE_AUTO_ID);
        s.apply_event(started(&[("default", "逐项确认"), ("plan", "计划")]), None);
        assert_eq!(s.modes[s.permission_mode].id, "default");
        assert!(s.modes.iter().all(|m| m.id != MODE_AUTO_ID));

        // 内核报的当前模式不在第一位：退回的是它，不是列表第一项
        let mut s = Session::new(3, KernelChoice::Acp("custom-1".into()), PathBuf::from("."));
        s.default_to(MODE_AUTO_ID);
        s.apply_event(started(&[("plan", "计划"), ("default", "逐项确认")]), None);
        assert_eq!(s.modes[s.permission_mode].id, "default");
    }

    fn auto_mode_settings() -> crate::settings::Settings {
        let mut settings = crate::settings::Settings::default();
        settings.builtin.current_mut().base_url = "http://127.0.0.1:9/v1".into();
        settings.builtin.current_mut().model = "test-model".into();
        settings.auto_mode.enabled = true;
        settings.usage_gateway = false;
        settings
    }

    #[core::prelude::v1::test]
    fn history_sessions_keep_their_own_mode() {
        let mut meta = meta();
        meta.agent = "builtin:test".into();
        let s = Session::from_records(1, meta, vec![Record::user("问")], &auto_mode_settings());
        assert!(s.restored && !s.ready);
        assert!(s.modes.iter().any(|m| m.id == MODE_AUTO_ID));
        assert_eq!(s.mode_id(), Some("default"));
        assert!(s.pending_mode.is_none());
    }

    #[core::prelude::v1::test]
    fn history_modes_roundtrip_and_reach_a_resumed_kernel() {
        use claudesktop_builtin::{BuiltinConfig, BuiltinKernel};
        use claudesktop_core::{SessionSpec, SessionStore};
        use std::sync::Mutex;

        let dir =
            std::env::temp_dir().join(format!("claudesktop-session-mode-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("workspace")).unwrap();
        let store = SessionStore::open(dir.join("sessions")).unwrap();
        let settings = auto_mode_settings();
        let mut config =
            BuiltinConfig::new(settings.builtin.current().provider_config(), dir.join("workspace"));
        config.auto_mode = settings.auto_mode.to_config(settings.builtin.current());
        config.memory_path = None;
        config.rules_path = None;
        config.user_instructions = None;
        config.mcp_config = None;
        config.session_titles = false;
        let events = Arc::new(Mutex::new(Vec::new()));
        let captured = events.clone();
        let kernel: Arc<dyn Kernel> = Arc::new(
            BuiltinKernel::spawn(
                config,
                Arc::new(move |event| {
                    captured.lock().unwrap().push(event.event);
                }),
            )
            .unwrap(),
        );
        for (i, mode) in
            ["auto", "default", "plan", "acceptEdits", "bypassPermissions"].into_iter().enumerate()
        {
            let (meta, mut log) =
                store.create("恢复权限模式", dir.join("workspace"), "builtin:test").unwrap();
            log.append(Record::user("继续")).unwrap();
            drop(log);
            let records = store.load(&meta.id).unwrap();
            let mut session = Session::from_records(i as u64, meta, records.clone(), &settings);
            let selected = session.modes.iter().position(|m| m.id == mode).unwrap();
            session.choose_mode_index(selected);
            store.save_meta(session.meta.as_ref().unwrap()).unwrap();
            let id = session.meta.as_ref().unwrap().id.clone();
            let saved = store.list().into_iter().find(|m| m.id == id).unwrap();
            let mut restored = Session::from_records(i as u64, saved, records.clone(), &settings);
            assert_eq!(restored.mode_id(), Some(mode));
            assert_eq!(restored.pending_mode.as_deref(), Some(mode));
            kernel
                .resume_session(
                    restored.token,
                    "previous",
                    SessionSpec::code(Some(dir.join("workspace"))),
                    &records,
                )
                .unwrap();
            let event = events
                .lock()
                .unwrap()
                .iter()
                .rev()
                .find(|event| matches!(event, KernelEvent::Started { token, .. } if *token == restored.token))
                .cloned()
                .unwrap();
            restored.apply_event(event, Some(&kernel));
            assert_eq!(restored.mode_id(), Some(mode));
            assert!(restored.pending_mode.is_none());
            assert_eq!(restored.meta.as_ref().unwrap().mode.as_deref(), Some(mode));
            assert!(!restored.transcript.iter().any(|row| matches!(row, ChatItem::Note(_))));
            kernel.close_session(restored.acp_id.as_deref().unwrap());
        }
        kernel.shutdown();
        drop(kernel);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[core::prelude::v1::test]
    fn restored_modes_fall_back_when_unavailable() {
        let mut settings = auto_mode_settings();
        settings.auto_mode.enabled = false;
        let mut saved = meta();
        saved.agent = "builtin:test".into();
        saved.mode = Some("auto".into());
        let mut s = Session::from_records(1, saved.clone(), Vec::new(), &settings);
        assert!(s.modes.iter().all(|m| m.id != MODE_AUTO_ID));
        assert_eq!(s.mode_id(), Some("default"));
        s.apply_event(started(&[("default", "逐项确认"), ("plan", "计划")]), None);
        assert_eq!(s.mode_id(), Some("default"));
        assert!(s.pending_mode.is_none());
        assert_eq!(s.meta.as_ref().unwrap().mode.as_deref(), Some("default"));

        saved.agent = "agent".into();
        saved.mode = Some("agent-mode".into());
        let mut agent = Session::from_records(2, saved.clone(), Vec::new(), &auto_mode_settings());
        assert!(agent.modes.iter().all(|m| m.id != MODE_AUTO_ID));
        agent.apply_event(started(&[("default", "逐项确认"), ("agent-mode", "自定义")]), None);
        assert_eq!(agent.mode_id(), Some("agent-mode"));

        saved.kind = SessionKind::Chat;
        let chat = Session::from_records(3, saved, Vec::new(), &auto_mode_settings());
        assert!(chat.modes.is_empty());
        assert!(chat.mode_id().is_none() && chat.pending_mode.is_none());
    }

    #[core::prelude::v1::test]
    fn restored_sessions_keep_their_task_flag() {
        // 侧栏按元信息把对话分到项目区 / 任务区，重放时不能丢
        let mut with_task = meta();
        with_task.task = true;
        let s = Session::from_records(1, with_task, vec![Record::user("问")], &Default::default());
        assert!(s.task);
        let s = Session::from_records(2, meta(), vec![Record::user("问")], &Default::default());
        assert!(!s.task);
    }

    #[core::prelude::v1::test]
    fn restored_chats_hand_their_web_switch_to_the_kernel() {
        // 内核开会话时按设置的默认值；不把界面上的开关交回去，两边就可能对不上
        let mut chat = meta();
        chat.kind = SessionKind::Chat;
        chat.web = Some(false);
        let s =
            Session::from_records(1, chat.clone(), vec![Record::user("问")], &Default::default());
        assert!(!s.web);
        assert_eq!(s.pending_web, Some(false));
        // 老元信息没记：当开着，同样交给内核
        chat.web = None;
        let s = Session::from_records(2, chat, vec![Record::user("问")], &Default::default());
        assert!(s.web);
        assert_eq!(s.pending_web, Some(true));
        // Code 会话没有这个开关
        let s = Session::from_records(3, meta(), vec![Record::user("问")], &Default::default());
        assert_eq!(s.pending_web, None);
    }

    #[core::prelude::v1::test]
    fn restored_sessions_remember_undone_changes() {
        use claudesktop_core::review::HunkRange;
        let range = HunkRange { old_start: 1, old_len: 1, new_start: 1, new_len: 2 };
        let changed = |path: &str, reverted: Option<Vec<HunkRange>>| Record::FileChanged {
            path: path.into(),
            before: Some("a".into()),
            after: Some("b".into()),
            reverted,
        };
        // 最新一轮在审查页里撤回过：先记成「撤回过、没数清」，切过来时再数
        let s = Session::from_records(
            1,
            meta(),
            vec![
                Record::user("问"),
                changed("a.rs", None),
                changed("a.rs", Some(vec![range])),
                changed("b.rs", None),
            ],
            &Default::default(),
        );
        assert_eq!(s.latest_undo(), Some(TurnUndo { turn: 0, all: false, exact: false }));
        // 撤回后又全部恢复（撤回集合清空）：不算撤回过
        let s = Session::from_records(
            1,
            meta(),
            vec![
                Record::user("问"),
                changed("a.rs", Some(vec![range])),
                changed("a.rs", Some(vec![])),
            ],
            &Default::default(),
        );
        assert_eq!(s.latest_undo(), None);
        // 撤回的是上一轮：新的一轮不带
        let s = Session::from_records(
            1,
            meta(),
            vec![Record::user("问"), changed("a.rs", Some(vec![range])), Record::user("再问")],
            &Default::default(),
        );
        assert_eq!(s.latest_undo(), None);
    }

    #[core::prelude::v1::test]
    fn steps_group_and_fold() {
        let mut s = Session::new(1, KernelChoice::Acp("custom-1".into()), PathBuf::from("."));
        s.transcript.push(ChatItem::User {
            text: "q".into(),
            images: Vec::new(),
            files: Default::default(),
        });
        s.begin_turn();
        // 只有一步：照常一行，没有组头
        s.apply_event(read("a"), None);
        let l = s.compute_layout(ViewMode::Normal);
        assert!(l[1].body && l[1].head.is_none() && !l[1].nested);

        // 空思考不算一步；再来四步，进行中只露出最近三步
        for event in [thought("\n\n"), read("b"), thought("想"), read("c"), read("d")] {
            s.apply_event(event, None);
        }
        let l = s.compute_layout(ViewMode::Normal);
        let head = l[1].head.expect("组头在第一步上");
        assert!(head.live && !head.open && head.end == 7);
        let shown: Vec<usize> = (1..7).filter(|&i| l[i].body).collect();
        assert_eq!(shown, [4, 5, 6]);
        assert!(l[4].nested && l[4].tight && l[5].tight && !l[6].tight);
        // 组头那行自己没露出来，但下面紧跟着组内的步骤
        assert!(!l[1].body && l[1].tight);

        // 出了正文：组收起，只剩组头（流式 chunk 先进合批缓冲，并进正文才算出了正文）
        s.apply_event(KernelEvent::AgentMessageChunk { text: "好了".into() }, None);
        s.flush_stream();
        let l = s.compute_layout(ViewMode::Normal);
        assert!(!l[1].head.unwrap().live);
        assert!((1..7).all(|i| !l[i].body) && !l[1].tight);
        assert!(l[7].body);
        // 收起了也还记着是组里的一步（收起动画要按进度画完）；空思考和正文不算
        let members: Vec<usize> = (0..8).filter(|&i| l[i].member).collect();
        assert_eq!(members, [1, 3, 4, 5, 6]);

        // 点开：除了空思考全都露出来
        s.toggle_group(1);
        let l = s.compute_layout(ViewMode::Normal);
        assert_eq!((1..7).filter(|&i| l[i].body).collect::<Vec<_>>(), [1, 3, 4, 5, 6]);
        s.toggle_group(1);
        assert!(s.open_groups.is_empty());

        // 详细视图总是展开；摘要视图整组不画
        let l = s.compute_layout(ViewMode::Verbose);
        assert!(l[1].head.unwrap().open && l[6].body);
        let l = s.compute_layout(ViewMode::Summary);
        assert!((1..7).all(|i| !l[i].body && l[i].head.is_none()));
    }

    #[core::prelude::v1::test]
    fn diff_tools_and_permissions_split_groups() {
        let mut s = Session::new(1, KernelChoice::Acp("custom-1".into()), PathBuf::from("."));
        s.transcript.push(ChatItem::User {
            text: "q".into(),
            images: Vec::new(),
            files: Default::default(),
        });
        s.apply_event(read("a"), None);
        s.apply_event(read("b"), None);
        s.apply_event(
            KernelEvent::ToolCallStarted {
                id: "e".into(),
                title: "Edit a.rs".into(),
                status: "pending".into(),
                kind: "edit".into(),
                parent: None,
            },
            None,
        );
        // 编辑还没 diff 时算一步，和前面连成一组
        assert_eq!(s.compute_layout(ViewMode::Verbose)[1].head.unwrap().end, 4);
        s.apply_event(
            KernelEvent::ToolCallDiff {
                id: "e".into(),
                diff: DiffView::compute("a.rs", Some("x\n"), "y\n"),
            },
            None,
        );
        s.apply_event(
            KernelEvent::PermissionAuto {
                title: "Bash ls".into(),
                chosen: "总是允许".into(),
                tool: None,
            },
            None,
        );
        s.apply_event(read("c"), None);
        // 自动批准的行只在「详细」视图里画（`visible_in`），分组也按可见性算
        let l = s.compute_layout(ViewMode::Verbose);
        // 带 diff 的编辑单独成行，把前后断成两组
        assert_eq!(l[1].head.unwrap().end, 3);
        assert!(l[3].body && l[3].head.is_none());
        assert_eq!(l[4].head.unwrap().end, 6);
        assert!(matches!(s.transcript[4], ChatItem::AutoApproved { .. }));
        // 正式视图里那一行不画，也不会凭空制造一个空组
        let l = s.compute_layout(ViewMode::Normal);
        assert!(!l[4].body && l[4].head.is_none());
        assert!(l[5].body);
    }

    /// 带工具 ID 的权限请求：应答后结果标在工具行上，这一行不再画，也不把前后的步骤拆成两组；
    /// 自动批准同样标在工具行上。
    #[core::prelude::v1::test]
    fn answered_permissions_merge_into_their_tool_rows() {
        let ask = |id: u64, tool: &str| {
            ev(KernelEvent::PermissionRequest {
                id,
                title: format!("Bash {tool}"),
                options: ["allow_once", "allow_always", "reject_once"]
                    .iter()
                    .map(|k| PermissionOptionView {
                        id: k.to_string(),
                        name: format!("选 {k}"),
                        kind: k.to_string(),
                    })
                    .collect(),
                tool: Some(tool.into()),
                hunks: None,
            })
        };
        let records = vec![
            Record::user("跑一下"),
            ev(read("a")),
            ask(1, "a"),
            Record::PermissionResolved { id: 1, label: "选 allow_always".into() },
            ev(read("b")),
            ask(2, "b"),
            Record::PermissionResolved { id: 2, label: "选 reject_once".into() },
            ev(read("c")),
            ev(KernelEvent::PermissionAuto {
                title: "Bash c".into(),
                chosen: "允许".into(),
                tool: Some("c".into()),
            }),
            // 上次退出时还没应答：按取消收尾，同样并进去
            ev(read("d")),
            ask(3, "d"),
        ];
        let s = Session::from_records(7, meta(), records, &Default::default());
        let grants: Vec<Option<GrantKind>> = s
            .transcript
            .iter()
            .filter_map(|i| match i {
                ChatItem::Tool(c) => Some(c.grant.as_ref().map(|g| g.kind)),
                _ => None,
            })
            .collect();
        assert_eq!(
            grants,
            [
                Some(GrantKind::Always),
                Some(GrantKind::Denied),
                Some(GrantKind::Auto),
                Some(GrantKind::Cancelled)
            ]
        );
        assert!(!s.transcript.iter().any(|i| matches!(i, ChatItem::AutoApproved { .. })));
        assert!(s.transcript.iter().all(|i| match i {
            ChatItem::Permission { merged, .. } => *merged,
            _ => true,
        }));
        // 四个工具行连成一组（中间并掉的权限行不拆组）
        let l = s.compute_layout(ViewMode::Normal);
        let head = l.iter().find_map(|r| r.head).unwrap();
        assert_eq!(head.end, s.transcript.len() - 1);
        assert!(
            !l.iter()
                .enumerate()
                .any(|(i, r)| r.body && matches!(s.transcript[i], ChatItem::Permission { .. }))
        );
    }

    #[core::prelude::v1::test]
    fn live_small_group_starts_open_and_folds_by_click() {
        let mut s = Session::new(1, KernelChoice::Acp("custom-1".into()), PathBuf::from("."));
        s.transcript.push(ChatItem::User {
            text: "q".into(),
            images: Vec::new(),
            files: Default::default(),
        });
        s.begin_turn();
        s.apply_event(read("a"), None);
        s.apply_event(read("b"), None);
        s.sync_list(ViewMode::Normal);
        let head = s.layout[1].head.expect("两步就有组头");
        assert!(head.live && head.live_small && head.open);
        assert!(s.layout[1].body && s.layout[2].body);
        // 点一下收起：活跃小组走 `folded_live_groups`
        s.toggle_group(1);
        s.sync_list(ViewMode::Normal);
        assert!(!s.layout[1].body && !s.layout[2].body);
        assert!(s.open_groups.is_empty() && s.folded_live_groups.contains(&1));
        s.toggle_group(1);
        s.sync_list(ViewMode::Normal);
        assert!(s.layout[1].body && s.layout[2].body);

        // 步骤超过 LIVE_TAIL 之后回到「露出尾巴、点开看全部」
        for event in [read("c"), read("d")] {
            s.apply_event(event, None);
        }
        s.sync_list(ViewMode::Normal);
        let head = s.layout[1].head.expect("组头还在");
        assert!(!head.live_small && !head.open);
        assert!(!s.layout[1].body && s.layout[3].body && s.layout[4].body);
    }

    /// 这一轮没有正文回复时，回合末尾（改动卡 / 用时）挂在最后一步上。
    #[core::prelude::v1::test]
    fn turn_without_reply_anchors_on_the_last_step() {
        let s = Session::from_records(
            7,
            meta(),
            vec![
                Record::user("跑一下"),
                ev(read("a")),
                ev(read("b")),
                ev(KernelEvent::TurnEnded { stop_reason: "end_turn".into() }),
            ],
            &Default::default(),
        );
        assert!(!s.is_turn_fallback_tail(1, ViewMode::Normal));
        assert!(s.is_turn_fallback_tail(2, ViewMode::Normal));
        assert_eq!(s.latest_turn_anchor(ViewMode::Normal), Some(2));

        // 有正文回复的回合仍按正文那一行算
        let s = Session::from_records(
            7,
            meta(),
            vec![
                Record::user("跑一下"),
                ev(read("a")),
                ev(KernelEvent::AgentMessageChunk { text: "好了".into() }),
                ev(KernelEvent::TurnEnded { stop_reason: "end_turn".into() }),
            ],
            &Default::default(),
        );
        assert!(!s.is_turn_fallback_tail(1, ViewMode::Normal));
        assert_eq!(s.latest_turn_anchor(ViewMode::Normal), Some(2));
    }

    #[core::prelude::v1::test]
    fn plan_pins_while_turn_runs() {
        let mut s = Session::new(1, KernelChoice::Acp("custom-1".into()), PathBuf::from("."));
        s.transcript.push(ChatItem::User {
            text: "q".into(),
            images: Vec::new(),
            files: Default::default(),
        });
        s.begin_turn();
        let plan = |statuses: &[&str]| KernelEvent::PlanReplaced {
            entries: statuses
                .iter()
                .map(|st| claudesktop_core::PlanEntryView {
                    content: "步骤".into(),
                    priority: "medium".into(),
                    status: (*st).into(),
                })
                .collect(),
        };
        s.apply_event(plan(&["in_progress", "pending"]), None);
        s.apply_event(read("a"), None);
        s.apply_event(plan(&["completed", "in_progress"]), None);
        // 同一轮里原地替换，状态保留「进行中」
        assert_eq!(s.transcript.len(), 3);
        let ChatItem::Plan(rows) = &s.transcript[1] else { panic!("plan") };
        assert_eq!(rows[1].status, PlanStatus::InProgress);
        assert_eq!(s.pinned_plan(), Some(1));
        assert!(s.compute_layout(ViewMode::Normal)[1].compact);
        // 一轮结束：不再钉着，对话里那份完整显示
        s.busy = false;
        assert_eq!(s.pinned_plan(), None);
        assert!(!s.compute_layout(ViewMode::Normal)[1].compact);
    }

    #[core::prelude::v1::test]
    fn replay_shows_questions_and_their_answers() {
        let asked = |id: u64| {
            ev(KernelEvent::UserQuestion {
                id,
                tool: None,
                questions: vec![claudesktop_core::QuestionView {
                    question: "走哪条？".into(),
                    header: None,
                    options: vec![
                        claudesktop_core::QuestionOptionView {
                            label: "重写".into(),
                            description: None,
                            value: None,
                        },
                        claudesktop_core::QuestionOptionView {
                            label: "保留".into(),
                            description: Some("还有引用".into()),
                            value: None,
                        },
                    ],
                    ..Default::default()
                }],
            })
        };
        // 答过的：卡片留着，显示选中的项，不能再点
        let s = Session::from_records(
            7,
            meta(),
            vec![
                Record::user("改一下"),
                asked(3),
                Record::QuestionAnswered {
                    id: 3,
                    answer: claudesktop_core::QuestionAnswer::Choices { choices: vec![1] },
                },
                ev(KernelEvent::TurnEnded { stop_reason: "end_turn".into() }),
            ],
            &Default::default(),
        );
        let item = s.transcript.iter().find_map(|i| match i {
            ChatItem::Question { picks, answer, .. } => Some((picks.clone(), answer.clone())),
            _ => None,
        });
        let (picks, answer) = item.expect("提问卡片应该在");
        assert_eq!(picks, vec![Some(1)]);
        assert_eq!(answer, Some(claudesktop_core::QuestionAnswer::Choices { choices: vec![1] }));
        assert_eq!(s.pending_question(), None);

        // 上次退出时还没答的：按取消收尾，并给一张「未完成」卡片
        let s = Session::from_records(
            7,
            meta(),
            vec![Record::user("改一下"), asked(4)],
            &Default::default(),
        );
        assert_eq!(s.pending_question(), None);
        assert!(s.transcript.iter().any(|i| matches!(
            i,
            ChatItem::Question { answer: Some(claudesktop_core::QuestionAnswer::Cancelled), .. }
        )));
    }

    #[core::prelude::v1::test]
    fn question_panel_walks_questions_one_at_a_time() {
        let mut s = Session::new(1, KernelChoice::Acp("custom-1".into()), PathBuf::from("."));
        let q = |text: &str| claudesktop_core::QuestionView {
            question: text.into(),
            header: None,
            options: vec![
                claudesktop_core::QuestionOptionView {
                    label: "甲".into(),
                    description: None,
                    value: None,
                },
                claudesktop_core::QuestionOptionView {
                    label: "乙".into(),
                    description: None,
                    value: None,
                },
            ],
            ..Default::default()
        };
        s.apply_event(
            KernelEvent::UserQuestion {
                id: 9, tool: None, questions: vec![q("先问"), q("再问")]
            },
            None,
        );
        let idx = s.pending_question().expect("挂着提问");
        assert_eq!(s.question_cursor(idx), Some(0));
        // 选完第一题就停在第二题
        s.pick_question(idx, 0, 1);
        assert_eq!(s.question_cursor(idx), Some(1));
        assert_eq!(s.question_choices(idx), None);
        // 「上一题」把那题退回去重选
        s.back_question(idx);
        assert_eq!(s.question_cursor(idx), Some(0));
        s.pick_question_key(idx, 1);
        s.pick_question_key(idx, 0);
        assert_eq!(s.question_choices(idx), Some(vec![1, 0]));
        assert_eq!(s.question_cursor(idx), Some(1), "都选过了停在最后一题");
    }

    #[core::prelude::v1::test]
    fn replay_drops_rewound_turns() {
        let records = vec![
            Record::user("第一问"),
            ev(KernelEvent::AgentMessageChunk { text: "答".into() }),
            ev(KernelEvent::TurnEnded { stop_reason: "end_turn".into() }),
            Record::user("第二问"),
            Record::FileChanged {
                path: "a.rs".into(),
                before: None,
                after: Some("h".into()),
                reverted: None,
            },
            ev(KernelEvent::TurnEnded { stop_reason: "end_turn".into() }),
            Record::Rewind { turn: 1, conversation: true, files: 1, prompt: "第二问".into() },
        ];
        let s = Session::from_records(1, meta(), records, &Default::default());
        let users = s.transcript.iter().filter(|i| matches!(i, ChatItem::User { .. })).count();
        assert_eq!(users, 1);
        assert!(matches!(
            s.transcript.last(),
            Some(ChatItem::Note(t)) if t.as_ref() == "已回退到「第二问」之前，还原了 1 个文件"
        ));
    }

    #[core::prelude::v1::test]
    fn replay_remembers_reverted_hunks_until_the_next_message() {
        let reviewed = Record::FileChanged {
            path: "a.rs".into(),
            before: Some("h1".into()),
            after: Some("h2".into()),
            reverted: Some(Vec::new()),
        };
        let mut records = vec![Record::user("改"), reviewed];
        assert!(
            Session::from_records(1, meta(), records.clone(), &Default::default()).review_pending
        );
        // 已经发过下一条（说明附在那条上了）
        records.push(Record::user("继续"));
        assert!(!Session::from_records(1, meta(), records, &Default::default()).review_pending);
    }

    #[core::prelude::v1::test]
    fn replay_attaches_turn_summary() {
        let records = vec![
            Record::user("q"),
            ev(KernelEvent::AgentMessageChunk { text: "a".into() }),
            ev(KernelEvent::TurnEnded { stop_reason: "end_turn".into() }),
            Record::Turn {
                at_ms: 1,
                model: None,
                input_tokens: 10,
                output_tokens: 2,
                elapsed_ms: Some(65_000),
                context_tokens: None,
            },
        ];
        let s = Session::from_records(1, meta(), records, &Default::default());
        let summary = s.turn_summary(1).expect("回复那行有统计");
        assert_eq!(summary.elapsed_ms, Some(65_000));
        assert_eq!((summary.usage.input, summary.usage.output), (10, 2));
    }

    #[core::prelude::v1::test]
    fn replay_restores_model_label() {
        // 重开的会话在内核连上（Started）之前也显示真模型名，
        // 上下文用量才能按它查到窗口大小显示百分比
        let records = vec![
            Record::user("q"),
            ev(KernelEvent::TurnEnded { stop_reason: "end_turn".into() }),
            Record::Turn {
                at_ms: 1,
                model: Some("cn:deepseek-v4.1-flash".into()),
                input_tokens: 10,
                output_tokens: 2,
                elapsed_ms: None,
                context_tokens: Some(1508),
            },
            Record::user("再问"),
            ev(KernelEvent::TurnEnded { stop_reason: "end_turn".into() }),
            Record::Turn {
                at_ms: 2,
                model: Some("other-model".into()),
                input_tokens: 5,
                output_tokens: 1,
                elapsed_ms: None,
                context_tokens: Some(2000),
            },
        ];
        let s = Session::from_records(1, meta(), records, &Default::default());
        // 最后一次上报的模型胜出；早于它的照样能恢复
        assert_eq!(s.model_label.as_ref(), "other-model");
        assert_eq!(s.context_tokens, 2000);

        let first = vec![
            Record::user("q"),
            ev(KernelEvent::TurnEnded { stop_reason: "end_turn".into() }),
            Record::Turn {
                at_ms: 1,
                model: Some("cn:deepseek-v4.1-flash".into()),
                input_tokens: 10,
                output_tokens: 2,
                elapsed_ms: None,
                context_tokens: Some(1508),
            },
        ];
        let s = Session::from_records(1, meta(), first, &Default::default());
        assert_eq!(s.model_label.as_ref(), "cn:deepseek-v4.1-flash");

        // 旧日志没记模型：维持「默认模型」
        let none = vec![
            Record::user("q"),
            Record::Turn {
                at_ms: 1,
                model: None,
                input_tokens: 1,
                output_tokens: 1,
                elapsed_ms: None,
                context_tokens: None,
            },
        ];
        let s = Session::from_records(1, meta(), none, &Default::default());
        assert_eq!(s.model_label.as_ref(), UNKNOWN_MODEL);
    }

    fn permission_event(id: u64) -> KernelEvent {
        match permission(id) {
            Record::Event { event } => event,
            _ => unreachable!(),
        }
    }

    #[core::prelude::v1::test]
    fn attention_picks_events_worth_a_notification() {
        let mut s = Session::new(1, KernelChoice::Builtin, PathBuf::from("."));
        s.transcript.push(ChatItem::User {
            text: "q".into(),
            images: Vec::new(),
            files: Default::default(),
        });
        s.transcript.push(ChatItem::Agent(
            "## 结论\n\n**改好了**，测试 `cargo test` 全过。\n第二行".into(),
        ));
        let ended = |r: &str| KernelEvent::TurnEnded { stop_reason: r.into() };
        assert_eq!(s.attention(&ended("end_turn")).as_deref(), Some("回复完了：结论"));
        assert_eq!(s.attention(&ended("cancelled")), None);
        assert_eq!(s.attention(&permission_event(1)).as_deref(), Some("等你授权：Write a.txt"));
        assert_eq!(
            s.attention(&KernelEvent::PromptFailed { message: "HTTP 500\n详情".into() }).as_deref(),
            Some("出错了：HTTP 500")
        );
        assert_eq!(s.attention(&KernelEvent::AgentMessageChunk { text: "x".into() }), None);
        // 手动压缩那一轮不提醒；本轮没有回复时只说「回复完了」
        s.compact_turn = true;
        assert_eq!(s.attention(&ended("end_turn")), None);
        s.compact_turn = false;
        s.transcript.push(ChatItem::User {
            text: "q2".into(),
            images: Vec::new(),
            files: Default::default(),
        });
        assert_eq!(s.attention(&ended("end_turn")).as_deref(), Some("回复完了"));
        // 长回复截断
        let long = "很长".repeat(100);
        assert!(preview_line(&long).unwrap().ends_with('…'));
        assert_eq!(preview_line(&long).unwrap().chars().count(), 81);
    }

    fn live_test_session() -> Session {
        Session::new(
            1,
            crate::settings::KernelChoice::Acp("custom-1".into()),
            std::path::PathBuf::from("."),
        )
    }

    fn live_tool(kind: &str, name: &str, phase: ToolPhase) -> ChatItem {
        ChatItem::Tool(ToolCard {
            id: None,
            kind: kind.into(),
            name: name.into(),
            arg: "arg".into(),
            result: String::new().into(),
            output: SharedString::default(),
            content: SharedString::default(),
            phase,
            diffs: Vec::new(),
            expanded: false,
            task: false,
            grant: None,
            images: Vec::new(),
        })
    }

    fn live_think(text: &str, streaming: bool) -> ChatItem {
        ChatItem::Thinking {
            text: text.into(),
            streaming,
            open: false,
            started: None,
            elapsed_ms: None,
        }
    }

    /// 预览槽跟着本轮当前状态走：思考 → 单个工具 → 并行 → 非 Working 时收起。
    #[core::prelude::v1::test]
    fn live_preview_follows_the_running_step() {
        let mut s = live_test_session();
        s.busy = true;
        // 刚发出去、还没出思考或工具
        assert_eq!(s.live_preview(ViewMode::Normal), LivePreview::None);

        // 正在流式思考
        s.transcript.push(live_think("先想一想", true));
        assert_eq!(s.live_preview(ViewMode::Normal), LivePreview::Thinking(0));
        // 详细视图里思考整段展开着，不重复预览
        assert_eq!(s.live_preview(ViewMode::Verbose), LivePreview::None);
        s.transcript[0] = live_think("想完了", false);

        // 单个简单工具：不弹（防止高频调用闪烁）
        s.transcript.push(live_tool("read", "Read", ToolPhase::Running));
        assert_eq!(s.live_preview(ViewMode::Normal), LivePreview::None);

        // 单个非简单工具：弹
        let mut s = live_test_session();
        s.busy = true;
        s.transcript.push(live_tool("execute", "Bash", ToolPhase::Running));
        assert_eq!(s.live_preview(ViewMode::Normal), LivePreview::Tool(0));
        // Summary 模式同样给
        assert_eq!(s.live_preview(ViewMode::Summary), LivePreview::Tool(0));

        // 两个以上并行：列成多行
        s.transcript.push(live_tool("fetch", "Fetch", ToolPhase::Running));
        assert_eq!(s.live_preview(ViewMode::Normal), LivePreview::ParallelTools(vec![0, 1]));

        // 等授权 / 等回答 / 回复正文时都不是 Working：这里只覆盖回合结束这一种
        s.busy = false;
        assert_eq!(s.live_preview(ViewMode::Normal), LivePreview::None);
    }

    /// 思考预览：先逐行填满 4 行，溢出后冻结 3 秒，再按慢速 / 高速两种方式整块替换。
    #[core::prelude::v1::test]
    fn live_think_pager_freezes_then_pages() {
        let mut s = live_test_session();
        s.busy = true;
        // 6 行：前 4 行上屏并标记还有更多
        let short = (0..6).map(|i| format!("第{i}行")).collect::<Vec<_>>().join("\n");
        s.transcript.push(live_think(&short, true));
        let t0 = Instant::now();
        s.sync_live_think(ViewMode::Normal, t0);
        let (lines, more) = s.live_think_block().unwrap();
        assert_eq!(lines.len(), layout::LIVE_THINK_LINES);
        assert!(more);
        assert_eq!(lines[0].as_ref(), "第0行");

        // 未满 3 秒：内容冻结不动
        s.sync_live_think(ViewMode::Normal, t0 + Duration::from_secs(1));
        assert_eq!(s.live_think_block().unwrap().0[0].as_ref(), "第0行");

        // 满 3 秒：剩余只有 2 行（≤ 4），顺延到下一块、不丢行
        s.sync_live_think(ViewMode::Normal, t0 + layout::LIVE_THINK_HOLD);
        let (lines, more) = s.live_think_block().unwrap();
        assert_eq!(lines[0].as_ref(), "第4行");
        assert!(!more);

        // 高速思考：长文中途溢出，3 秒后直接跳到最新块
        let long = (0..80).map(|i| format!("长思考第{i}行")).collect::<Vec<_>>().join("\n");
        let mut s = live_test_session();
        s.busy = true;
        s.transcript.push(live_think(&long, true));
        let t0 = Instant::now();
        s.sync_live_think(ViewMode::Normal, t0);
        let first = s.live_think_block().unwrap().0[0].clone();
        assert!(s.live_think_block().unwrap().1);
        s.sync_live_think(ViewMode::Normal, t0 + layout::LIVE_THINK_HOLD);
        let (lines, more) = s.live_think_block().unwrap();
        assert!(!more);
        assert_ne!(lines[0], first);
        assert!(lines[0].as_ref().contains("长思考第"));

        // 预览消失（例如切到正文回复）时清掉翻页器
        s.busy = false;
        s.sync_live_think(ViewMode::Normal, t0);
        assert!(s.live_think_block().is_none());
    }

    /// 折行：ASCII 记 1 列、中文记 2 列，收满 4 行就停，超长无换行段落也安全切。
    #[core::prelude::v1::test]
    fn wrap_lines_counts_columns_and_stops_at_limit() {
        let (lines, more) = layout::wrap_lines_with_offsets("abcdefghij", 4, Some(2));
        assert_eq!(lines.iter().map(|l| l.text.as_ref()).collect::<Vec<_>>(), ["abcd", "efgh"]);
        assert!(more);
        // 中文每字 2 列
        let (lines, _) = layout::wrap_lines_with_offsets("你好世界你好", 4, None);
        assert_eq!(
            lines.iter().map(|l| l.text.as_ref()).collect::<Vec<_>>(),
            ["你好", "世界", "你好"]
        );
        // 空行跳过
        let (lines, _) = layout::wrap_lines_with_offsets("\n\n实际内容\n\n", 20, None);
        assert_eq!(lines.len(), 1);
    }

    /// Summary 模式里工具行收在过程组中，但底部预览槽放行它的进度重绘；预览之外仍按原规则。
    #[core::prelude::v1::test]
    fn live_preview_lets_tool_progress_through() {
        let mut s = live_test_session();
        s.busy = true;
        s.transcript.push(live_tool("execute", "Bash", ToolPhase::Running));
        if let ChatItem::Tool(card) = &mut s.transcript[0] {
            card.id = Some("a".into());
        }
        // 底部预览槽正显示这条工具：Summary / Normal 都要刷新
        assert!(s.is_tool_output_visible("a", ViewMode::Summary));
        assert!(s.is_tool_output_visible("a", ViewMode::Normal));
        // 回合结束、预览收起后回到原规则（没展开 = 不画）
        s.busy = false;
        assert!(!s.is_tool_output_visible("a", ViewMode::Summary));
        assert!(!s.is_tool_output_visible("a", ViewMode::Normal));
    }

    /// 状态行指纹：思考冻结期间不变，翻块 / 换预览状态才变（否则虚拟列表不重测末行高度）。
    #[core::prelude::v1::test]
    fn status_row_key_tracks_the_preview() {
        let mut s = live_test_session();
        s.busy = true;
        let text = (0..6).map(|i| format!("第{i}行")).collect::<Vec<_>>().join("\n");
        s.transcript.push(live_think(&text, true));
        let t0 = Instant::now();
        s.sync_live_think(ViewMode::Normal, t0);
        let frozen = s.status_row_key(ViewMode::Normal);
        // 冻结期内指纹不变
        s.sync_live_think(ViewMode::Normal, t0 + Duration::from_secs(1));
        assert_eq!(s.status_row_key(ViewMode::Normal), frozen);
        // 满 3 秒翻到下一块：指纹变化，末行才会重测
        s.sync_live_think(ViewMode::Normal, t0 + layout::LIVE_THINK_HOLD);
        assert_ne!(s.status_row_key(ViewMode::Normal), frozen);
        // 换到别的状态（回合结束）也变
        let paged = s.status_row_key(ViewMode::Normal);
        s.busy = false;
        assert_ne!(s.status_row_key(ViewMode::Normal), paged);
    }
}
