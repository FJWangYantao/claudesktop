//! 单个对话会话的状态，以及把内核事件 / 演示步骤应用到对话记录上。

use crate::demo::*;
use crate::model::*;
use crate::settings::KernelChoice;
use crate::usage::Streamed;
use claudesktop_core::{DiffView, Kernel, KernelEvent, Record, SessionLog, SessionMeta};
use gpui_kit::*;
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;

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
    pub transcript: Vec<ChatItem>,
    pub busy: bool,
    /// 这一轮是手动压缩上下文（没有用户消息；结束时不记用时用量，不钉上一轮的计划）
    pub compact_turn: bool,
    /// 演示回放用：递增即取消正在播放的脚本
    pub cancel_token: u64,
    /// 对话记录的虚拟列表状态（只渲染可见的行）；默认跟随到底，用户往上翻就停，翻回底部再跟
    pub list: ListState,
    /// 每行上次测量时的指纹（[`Session::row_key`]）
    pub measured: Vec<u64>,
    /// 列表末尾的运行状态行上次测量时是否在忙（忙时有一行，不忙时高度为 0）
    measured_busy: bool,
    /// 当前可选模式（默认静态表，Started 后换成 agent 上报的）
    pub modes: Vec<ModeEntry>,
    pub permission_mode: usize, // modes 下标
    pub model_label: SharedString,
    /// 自动演示：会话就绪后发送的提示词
    pub autoprompt: Option<claudesktop_core::UserPrompt>,
    /// 会话工作目录（侧栏按它分组；session/new 用它）
    pub cwd: PathBuf,
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
    /// 这次运行里试过重新检出丢了的 worktree 没有（只试一次，见 `ChatApp::repair_worktree`）
    pub worktree_repair_tried: bool,
    /// 持久化元信息：首条消息发出后才建档（空会话不落盘）
    pub meta: Option<SessionMeta>,
    /// 追加式日志；恢复的会话在第一次写入时才打开
    pub log: Option<SessionLog>,
    /// 从磁盘恢复、还没接上内核会话的历史会话（不自动 session/new）
    pub restored: bool,
    /// 历史会话正在续接：Started 到达时告诉用户上下文是否延续
    pub resuming: bool,
    /// 内核会话建立前选的模型：Started 之后再告诉内核
    pub pending_model: Option<String>,
    /// agent 档案记的上次模型（`AcpSettings::last_model` 的快照）：Started 后 agent 没报模型就切过去
    pub last_model: Option<String>,
    /// 待同步给内核的权限模式：新会话的默认模式（设置里开着「自动」时是 `"auto"`），
    /// 或用户在会话建立前自己选的。`Started` 时交给内核后清掉
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
    /// 每轮的用时和用量（按这一轮用户消息的下标），挂在这一轮回复末尾
    pub turn_summaries: HashMap<usize, TurnSummary>,
    /// 输入框上方的计划收成一行
    pub plan_folded: bool,
    /// 上下文里的 token（最近一轮结束时内核报告的；0 = 不知道，输入框旁不显示用量）
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
    /// 用户按了停止、内核还没回这一轮的结束：这期间发的消息先排队，等结束到了再发
    /// （不然迟到的「已取消」会落在新的一轮头上，把它提前收掉）
    pub stopping: bool,
    /// 下次同步列表时滚到这一行（从搜索结果跳过来）
    pub reveal: Option<usize>,
    /// 滚轮平滑滚动进行中（见 `ChatApp::step_wheel`）；跳转、回到底部时清掉，免得两边抢
    pub wheel: Option<WheelScroll>,
    /// 刚在权限卡上「拒绝并说明」：输入框的占位文字请用户说要怎么改，下一条消息发出时清掉
    pub steering: bool,
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
    pub changes_open: HashSet<usize>,
    /// 最新一轮的改动撤回到了什么程度（审查页或改动卡片上的「撤回」写盘后更新）。
    /// 只在内存里：重启后卡片照常显示「撤回」，再点一次结果不变
    pub undo: Option<TurnUndo>,
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
#[derive(Default)]
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

/// 滚轮平滑滚动：滚轮只推进 `target`（离列表顶部的像素），实际位置每帧往它逼近一步。
#[derive(Clone, Copy, Debug)]
pub struct WheelScroll {
    pub target: Pixels,
    /// 这一串滚轮的最后一格何时到的（滑太久就放弃，防止目标够不着时一直排帧）
    pub started: Instant,
    pub last: Instant,
}

/// 对话记录里一行在当前视图下怎么画。连续的过程步骤（[`ChatItem::is_step`]）合成一组：
/// 组头画在组里第一行的顶上，组内各步缩进在组头下面，折叠时不画。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct RowLayout {
    /// 这一行顶上先画过程组的组头
    pub head: Option<StepGroup>,
    /// 这一行自己的内容画不画
    pub body: bool,
    /// 组内的一步：缩进、左侧细线
    pub nested: bool,
    /// 下面紧跟同组的下一行（行距收紧，细线连上）
    pub tight: bool,
    /// 属于一个多步过程组（不管这会儿露不露出来）：展开 / 收起时按弹簧进度裁切
    pub member: bool,
    /// 计划正钉在输入框上方：这里只留一行进度
    pub compact: bool,
}

/// 一个过程组的状态（组头用）。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct StepGroup {
    /// 组的末尾（不含）；组从带组头的那一行开始
    pub end: usize,
    /// 进行中：本轮在这之后还没出正文
    pub live: bool,
    /// 展开看全部步骤（用户点开过，或详细视图）
    pub open: bool,
}

/// 一轮的用时和用量（回复末尾显示）。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TurnSummary {
    /// 旧日志没记用时
    pub elapsed_ms: Option<u64>,
    pub usage: TurnUsage,
}

/// 一轮里累计的 token 用量（内核的 `UsageUpdated` 逐次累加）。
#[derive(Debug, Default, Clone, Copy, PartialEq)]
pub struct TurnUsage {
    pub input: u64,
    pub output: u64,
    /// 其中思考消耗的部分（包含在 `output` 里；内核不报告时为 0）
    pub reasoning: u64,
    /// 本轮端点报过用量没有：没报过时状态行上的数字是估的（界面标成 `≈`）
    pub reported: bool,
}

/// 进行中的一轮此刻在干什么（状态条的左侧文字）。只分三档：
/// 思考和调工具在一轮里来回切换很快，细分会让文字一闪一闪；具体在做哪一步看过程组。
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Activity {
    /// 有权限请求等用户应答
    WaitingPermission,
    /// 思考、调工具，或刚发出还没回音
    Working,
    /// 正在输出正文
    Replying,
    /// 端点限流 / 出错，等着自动重试（详情在 [`Session::retry`]）
    Retrying,
    /// 正在把较早的对话压缩成摘要
    Compacting,
}

fn new_list_state() -> ListState {
    // overdraw：视口外多渲染的高度，滚动时少一点空白。
    // 初始 1 行 = 末尾的运行状态行（对话记录的行都插在它前面）
    let state = ListState::new(1, ListAlignment::Top, px(600.));
    state.set_follow_mode(FollowMode::Tail);
    state
}

impl Session {
    /// 新会话；演示回放不需要内核会话，直接可用。
    pub fn new(token: u64, kernel: KernelChoice, cwd: PathBuf) -> Self {
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
            transcript: Vec::new(),
            busy: false,
            compact_turn: false,
            cancel_token: 0,
            list: new_list_state(),
            measured: Vec::new(),
            measured_busy: false,
            modes: default_modes(),
            permission_mode: 0,
            // 演示回放用假模型名；真内核等 Started 上报，没上报就一直显示「默认模型」
            model_label: if is_demo { "GLM-5.3" } else { UNKNOWN_MODEL }.into(),
            autoprompt: None,
            cwd,
            task: false,
            worktree: None,
            worktree_busy: None,
            worktree_state: None,
            worktree_blocker: None,
            worktree_repair_tried: false,
            meta: None,
            log: None,
            restored: false,
            resuming: false,
            pending_model: None,
            last_model: None,
            pending_mode: None,
            thinking: None,
            pending_thinking: None,
            turn_usage: TurnUsage::default(),
            turn_streamed: Streamed::default(),
            turn_started: None,
            renamed: false,
            title_generated: false,
            layout: Vec::new(),
            open_groups: HashSet::new(),
            turn_summaries: HashMap::new(),
            plan_folded: false,
            context_tokens: 0,
            replaying: false,
            thought_done: None,
            retry: None,
            unread: false,
            queued: Vec::new(),
            reveal: None,
            wheel: None,
            stopping: false,
            steering: false,
            cache_hit: None,
            content_seq: 0,
            seen_seq: 0,
            subagents: HashMap::new(),
            viewing_task: None,
            review_pending: false,
            changes_open: HashSet::new(),
            undo: None,
        }
    }

    /// 从持久化记录重放出一个历史会话：事件走与实时完全相同的映射。
    pub fn from_records(
        token: u64,
        meta: SessionMeta,
        records: Vec<Record>,
        acp: &crate::settings::AcpSettings,
    ) -> Self {
        let mut s = Self::new(token, KernelChoice::of_agent(&meta.agent, acp), meta.cwd.clone());
        s.ready = false;
        // agent 档案记的上次模型：续接后 agent 没报模型时切过去（`apply_event` 的 Started 分支）
        if let KernelChoice::Acp(aid) = &s.kernel {
            s.last_model = acp.get(aid).and_then(|a| a.last_model.clone());
        }
        // 历史会话不该被「新对话默认自动」改掉模式：它的模式由重建的日志决定
        s.pending_mode = None;
        // 是不是「任务」跟着元信息走：建的时候没选过文件夹的就是任务
        s.task = meta.task;
        // 跑在 git worktree 里的会话：侧栏仍归到主项目，分支 chip 用它
        s.worktree = meta.worktree.clone();
        s.title = meta.title.clone().into();
        s.restored = true;
        s.replaying = true;
        // 最新一轮每个文件最后一次写盘时处于「已撤回」的块（agent 写的清空），恢复改动卡片的撤回状态
        let mut reverted_now: HashMap<String, bool> = HashMap::new();
        // 回退过的轮次不显示（日志只追加，读的时候折算）
        for record in claudesktop_core::effective(&records) {
            match record {
                Record::User { text, images, .. } => {
                    s.review_pending = false;
                    reverted_now.clear();
                    s.transcript.push(ChatItem::User {
                        text: text.into(),
                        images: images.iter().map(PathBuf::from).collect(),
                    })
                }
                Record::Event { event } => {
                    s.apply_event(event, None);
                }
                Record::Turn {
                    model,
                    input_tokens,
                    output_tokens,
                    elapsed_ms,
                    context_tokens,
                    ..
                } => {
                    // 模型名从日志恢复：内核连上（Started）之前顶栏就能显示真名，
                    // 上下文用量也才能按它查到窗口大小
                    if let Some(model) = model {
                        s.model_label = model.into();
                    }
                    if let Some(user) = s.last_user() {
                        // 日志里的用量是准数（写日志前端点报过）
                        let usage = TurnUsage {
                            input: input_tokens,
                            output: output_tokens,
                            reasoning: 0,
                            reported: true,
                        };
                        s.turn_summaries.insert(user, TurnSummary { elapsed_ms, usage });
                    }
                    if let Some(n) = context_tokens {
                        s.context_tokens = n;
                    }
                }
                Record::Thought { elapsed_ms: ms } => {
                    let last = s.transcript.iter_mut().rev().find_map(|i| match i {
                        ChatItem::Thinking { elapsed_ms, .. } => Some(elapsed_ms),
                        _ => None,
                    });
                    if let Some(slot) = last {
                        *slot = Some(ms);
                    }
                }
                Record::PermissionResolved { id, label } => {
                    let at = s.transcript.iter().rposition(|i| {
                        matches!(i, ChatItem::Permission { id: pid, resolved: None, .. } if *pid == id)
                    });
                    if let Some(at) = at {
                        if let ChatItem::Permission { resolved, .. } = &mut s.transcript[at] {
                            *resolved = Some(label.into());
                        }
                        s.merge_permission(at);
                    }
                }
                Record::Rewind { conversation, files, prompt, .. } => {
                    s.note(rewind_note(conversation, files, &prompt))
                }
                // 检查点只在回退 / 审查时用，界面上不显示
                Record::FileChanged { path, reverted, .. } => {
                    s.review_pending |= reverted.is_some();
                    reverted_now.insert(path, reverted.is_some_and(|r| !r.is_empty()));
                }
            }
        }
        // 撤回了多少要对着 diff 才数得清，这里只知道「撤回过」：先按撤回了一部分显示，
        // 点开审查页或再点一次「撤回」时按实际情况更新（`ChatApp::sync_undo`）
        if reverted_now.values().any(|r| *r)
            && let Some(turn) = s.latest_turn()
        {
            s.undo = Some(TurnUndo { turn, all: false, exact: false });
        }
        // 上次退出时还在进行的东西一律收尾
        s.finish_thinking();
        s.replaying = false;
        let mut unfinished = s.busy;
        for item in &mut s.transcript {
            match item {
                ChatItem::Permission { resolved: r @ None, .. } => {
                    *r = Some("已取消".into());
                    unfinished = true;
                }
                ChatItem::Tool(card) if card.phase == ToolPhase::Running => {
                    card.phase = ToolPhase::Failed;
                    if card.result.is_empty() {
                        card.result = "已中断".into();
                    }
                    unfinished = true;
                }
                _ => {}
            }
        }
        s.merge_permissions();
        // 关应用时这一轮还在跑：补一张卡片说清楚，并给「继续」（不然只剩几行「已中断」，不知道下一步）
        if unfinished {
            s.transcript.push(ChatItem::Failed {
                message: SharedString::default(),
                kind: FailureKind::Unfinished,
            });
        }
        s.busy = false;
        // 重放出来的会话不带着「正在流式」的计数
        s.turn_streamed.clear();
        s.meta = Some(meta);
        s
    }

    /// 把对话记录的变化同步给虚拟列表：新增的行追加，高度可能变了的行重新测量。
    /// 列表比记录多一行：第 `n` 行是运行状态行，忙 / 闲切换时重新测量。
    /// 每次渲染前调用；先重算分组（[`Session::layout`]），再只比对指纹，不做布局，1 万行也很便宜。
    /// 指纹里带着视图模式和分组结果，切模式、展开 / 收起组时相关的行自动重测。
    pub fn sync_list(&mut self, view: ViewMode) {
        // 跟在底部就是都看到了
        if self.list.is_following_tail() {
            self.seen_seq = self.content_seq;
        }
        self.layout = self.compute_layout(view);
        let n = self.transcript.len();
        if n < self.measured.len() {
            // 记录只会变长；万一变短就整体重来
            self.list.reset(1);
            self.measured.clear();
        }
        let old = self.measured.len();
        for i in 0..old {
            let f = self.row_key(i, view);
            if f != self.measured[i] {
                self.measured[i] = f;
                self.list.remeasure_items(i..i + 1);
            }
        }
        if n > old {
            for i in old..n {
                let f = self.row_key(i, view);
                self.measured.push(f);
            }
            // 插在状态行前面
            self.list.splice(old..old, n - old);
        }
        // 状态行在忙或续接中时出现（见 `render_turn_status`）
        let status = self.busy || self.resuming;
        if status != self.measured_busy {
            self.measured_busy = status;
            self.list.remeasure_items(n..n + 1);
        }
        // 行数到位之后再滚，否则会被截到末尾；再往上让一点，别让顶上的渐隐盖住这一行
        if let Some(ix) = self.reveal.take().filter(|&ix| ix < n) {
            self.wheel = None;
            self.list.scroll_to(ListOffset { item_ix: ix, offset_in_item: px(0.) });
            self.list.scroll_by(-px(24.));
        }
    }

    /// 一行的测量指纹：内容指纹 + 视图模式 + 会不会跟着状态改行高（回合末尾的回复下面多一行
    /// 操作、失败卡片的按钮组会多出「继续」）+ 分组结果（组头、折叠、行距）。
    fn row_key(&self, i: usize, view: ViewMode) -> u64 {
        use std::hash::{Hash, Hasher};
        let mut h = std::collections::hash_map::DefaultHasher::new();
        let layout = self.layout.get(i).copied().unwrap_or_default();
        let live = if matches!(self.transcript[i], ChatItem::Failed { .. }) {
            (self.failure_is_latest(i), self.failure_resume(i))
        } else {
            (self.is_turn_end(i), false)
        };
        (self.transcript[i].fingerprint(), view, live, layout).hash(&mut h);
        // 回复末尾的改动卡片：展开了全部文件、撤回过，高度和按钮都会变
        if live.0 {
            (self.changes_open.contains(&i), self.latest_undo()).hash(&mut h);
        }
        h.finish()
    }

    /// 算出每行怎么画：连续的过程步骤合成一组。只有一步（不算空思考）时照常一行、不要组头；
    /// 组默认收起，进行中露出最近 [`LIVE_TAIL`] 步，点开或详细视图看全部；摘要视图整组不画。
    pub fn compute_layout(&self, view: ViewMode) -> Vec<RowLayout> {
        let t = &self.transcript;
        let mut out = vec![RowLayout::default(); t.len()];
        // 最后一段正文 / 用户消息之后的组还在长
        let last_text =
            t.iter().rposition(|i| matches!(i, ChatItem::Agent(_) | ChatItem::User { .. }));
        let pinned = self.pinned_plan();
        let mut i = 0;
        while i < t.len() {
            if !t[i].is_step() {
                out[i].body = t[i].visible_in(view);
                out[i].compact = pinned == Some(i);
                i += 1;
                continue;
            }
            let start = i;
            while i < t.len() && t[i].is_step() {
                i += 1;
            }
            if view == ViewMode::Summary {
                continue;
            }
            let steps: Vec<usize> = (start..i).filter(|&k| t[k].has_content()).collect();
            match steps.len() {
                0 => {}
                1 => out[steps[0]].body = true,
                n => {
                    let live = self.busy && last_text.is_none_or(|p| p < start);
                    let open = view == ViewMode::Verbose || self.open_groups.contains(&start);
                    let shown = if open {
                        &steps[..]
                    } else if live {
                        &steps[n.saturating_sub(LIVE_TAIL)..]
                    } else {
                        &[]
                    };
                    out[start].head = Some(StepGroup { end: i, live, open });
                    out[start].tight = !shown.is_empty();
                    for &s in &steps {
                        out[s].member = true;
                    }
                    for (k, &s) in shown.iter().enumerate() {
                        out[s].body = true;
                        out[s].nested = true;
                        out[s].tight = k + 1 < shown.len();
                    }
                }
            }
        }
        out
    }

    /// 展开 / 收起从 `start` 开始的过程组。
    pub fn toggle_group(&mut self, start: usize) {
        if !self.open_groups.remove(&start) {
            self.open_groups.insert(start);
        }
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
        let summary = TurnSummary {
            elapsed_ms: self.turn_started.take().map(|t| t.elapsed().as_millis() as u64),
            usage: std::mem::take(&mut self.turn_usage),
        };
        self.turn_streamed.clear();
        if let Some(user) = self.last_user() {
            self.turn_summaries.insert(user, summary);
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
            !matches!(i, ChatItem::Note(_) | ChatItem::Failed { .. } | ChatItem::Compaction { .. })
        }) {
            Some(ChatItem::User { .. }) => true,
            Some(_) => false,
            None => !self.busy,
        }
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
        self.wheel = None;
        self.list.set_follow_mode(FollowMode::Tail);
    }

    /// 往上翻过、离开了底部（列表不再自动跟随）：显示「回到底部」按钮
    pub fn scrolled_away(&self) -> bool {
        !self.list.is_following_tail() && self.list.is_scrolled_to_end() == Some(false)
    }

    /// 离开底部之后又来了新内容（「回到底部」按钮上带个点）
    pub fn has_unseen(&self) -> bool {
        self.content_seq > self.seen_seq
    }

    pub fn has_user_message(&self) -> bool {
        self.transcript.iter().any(|i| matches!(i, ChatItem::User { .. }))
    }

    /// 侧栏分组按哪个目录算：worktree 会话归到**主仓库**那一组（`cwd` 是数据目录里的 worktree，
    /// 不该单独占一组），其余就是工作目录。
    pub fn group_dir(&self) -> &std::path::Path {
        self.worktree.as_ref().map_or(self.cwd.as_path(), |w| w.repo.as_path())
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
        self.permission_mode = i;
        self.pending_mode = None;
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

    /// 第 `idx` 条是刚发出去、这一轮还在跑的用户消息：返回发出的时刻（入场动画用）。
    pub fn fresh_user(&self, idx: usize) -> Option<Instant> {
        (self.busy && self.last_user() == Some(idx)).then_some(self.turn_started).flatten()
    }

    /// 一轮开始：置忙并开始计时。
    pub fn begin_turn(&mut self) {
        self.busy = true;
        self.compact_turn = false;
        self.retry = None;
        self.turn_streamed.clear();
        self.turn_started = Some(Instant::now());
    }

    /// 本轮此刻的活动：等授权 > 等自动重试 > 正在输出正文（且没有工具在跑）> 其余都算「工作中」。
    /// 只看本轮（最后一条用户消息之后）。
    pub fn activity(&self) -> Activity {
        if self.waiting_permission() {
            return Activity::WaitingPermission;
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
        for item in &self.transcript {
            match item {
                ChatItem::User { text, images } => {
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

    /// 正在流式的思考块收尾，记下想了多久（计时的才有）。
    /// 展开与否保持原样（默认就是收起的，用户点开过的不替他收）。
    pub fn finish_thinking(&mut self) {
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
    pub fn resolve_permission(
        &mut self,
        idx: usize,
        choice: Option<(String, String)>,
        kernel: Option<&Arc<dyn Kernel>>,
    ) {
        let Some(kernel) = kernel else {
            return;
        };
        if let Some(ChatItem::Permission { id, resolved, .. }) = self.transcript.get_mut(idx) {
            if resolved.is_some() {
                return;
            }
            let label: SharedString = match &choice {
                Some((_, name)) => name.clone().into(),
                None => "已取消".into(),
            };
            *resolved = Some(match kernel.resolve_permission(*id, choice.map(|(i, _)| i)) {
                Ok(()) => label,
                Err(err) => format!("应答失败：{err}").into(),
            });
            self.merge_permission(idx);
        }
    }

    /// 应答过的权限请求并进它的工具行（工具行末尾出「已允许」之类的短标签），这一行不再画。
    /// 找不到工具行（旧日志没有工具 ID）、认不出选的是什么或应答失败时，照旧单独一行。
    fn merge_permission(&mut self, idx: usize) {
        let Some(ChatItem::Permission {
            tool: Some(tool),
            options,
            resolved: Some(label),
            merged: false,
            ..
        }) = self.transcript.get(idx)
        else {
            return;
        };
        let grant = if label.as_ref() == "已取消" {
            Grant::from_choice(None, label.clone())
        } else {
            match options.iter().find(|o| o.name == label.as_ref()) {
                Some(option) => Grant::from_choice(Some(&option.kind), label.clone()),
                None => return,
            }
        };
        let tool = tool.clone();
        let Some(card) = self.tool_card(&tool) else {
            return;
        };
        card.grant = Some(grant);
        if let Some(ChatItem::Permission { merged, .. }) = self.transcript.get_mut(idx) {
            *merged = true;
        }
    }

    /// 把所有应答过、还没并进工具行的权限请求并过去（重放收尾、整轮取消时用）。
    fn merge_permissions(&mut self) {
        for idx in 0..self.transcript.len() {
            if matches!(
                self.transcript[idx],
                ChatItem::Permission { resolved: Some(_), merged: false, .. }
            ) {
                self.merge_permission(idx);
            }
        }
    }

    /// 本轮结束 / 停止 / 内核出错时，仍在等待的请求一律按「取消」收尾。
    pub fn cancel_pending_permissions(&mut self, kernel: Option<&Arc<dyn Kernel>>) {
        let Some(kernel) = kernel else {
            return;
        };
        for item in &mut self.transcript {
            if let ChatItem::Permission { id, resolved, .. } = item
                && resolved.is_none()
            {
                let _ = kernel.resolve_permission(*id, None);
                *resolved = Some("已取消".into());
            }
        }
        self.merge_permissions();
    }

    /// 回退对话：对话截到第 `idx` 行之前（那条用户消息起都不要了），挂在这些行上的状态一并清掉。
    pub fn truncate_to(&mut self, idx: usize) {
        self.transcript.truncate(idx);
        self.turn_summaries.retain(|&i, _| i < idx);
        self.open_groups.retain(|&i| i < idx);
        self.plan_folded = false;
        // 上下文变短了，用量等下一轮结束时内核再报
        self.context_tokens = 0;
        self.cache_hit = None;
    }

    /// 这一轮停了 / 断了，还在转圈的工具行收成「已中断」（内核不会再报它们的结果）。
    pub fn interrupt_running_tools(&mut self) {
        let runs = self.subagents.values_mut().flat_map(|r| r.items.iter_mut());
        for item in self.transcript.iter_mut().chain(runs) {
            if let ChatItem::Tool(card) = item
                && card.phase == ToolPhase::Running
            {
                card.phase = ToolPhase::Failed;
                if card.result.is_empty() {
                    card.result = "已中断".into();
                }
            }
        }
    }

    /// 内核要换了：收尾进行中的一轮，断开与旧内核会话的关联。
    /// 有对话的会话变成「历史会话」，下次发消息时在新内核上续接。
    pub fn detach_kernel(&mut self, kernel: Option<&Arc<dyn Kernel>>) {
        self.cancel_pending_permissions(kernel);
        self.cancel_token += 1;
        if self.busy {
            self.busy = false;
            self.finish_thinking();
            self.interrupt_running_tools();
            self.note("内核已切换，这一轮已中断");
        }
        self.stopping = false;
        self.acp_id = None;
        self.requested = false;
        self.resuming = false;
        self.ready = false;
        self.autoprompt = None;
        self.restored = self.has_user_message();
        // 当前模式随新内核会话重新交回去：用户选的（或新对话默认的）模式不该因为重连丢掉
        self.pending_mode = self.modes.get(self.permission_mode).map(|m| m.id.clone());
    }

    /// 内核致命错误：这个会话不再可用。
    pub fn on_fatal(&mut self, message: &str, kernel: Option<&Arc<dyn Kernel>>) {
        self.busy = false;
        self.stopping = false;
        self.ready = false;
        self.cancel_pending_permissions(kernel);
        self.note(format!("内核错误：{message}"));
    }

    /// 内核事件 → 本会话的界面内容（ACP `session/update` 的 UI 投影）。
    /// 返回值：会话刚就绪且有待发的自动提示词时，交给调用方发送。
    pub fn apply_event(
        &mut self,
        event: KernelEvent,
        kernel: Option<&Arc<dyn Kernel>>,
    ) -> Option<claudesktop_core::UserPrompt> {
        if Record::worth_keeping(&event) {
            self.content_seq += 1;
        }
        // 重试的倒计时只撑到下一个实质事件（重发后出字、出错或结束）
        if !matches!(
            event,
            KernelEvent::Retrying { .. }
                | KernelEvent::Status(_)
                | KernelEvent::Notice(_)
                | KernelEvent::UsageUpdated { .. }
        ) {
            self.retry = None;
        }
        match event {
            KernelEvent::Retrying { status, attempt, delay_ms, .. } => {
                self.retry = Some(Retry {
                    status,
                    attempt,
                    at: Instant::now() + std::time::Duration::from_millis(delay_ms),
                });
            }
            KernelEvent::Started { session_id, model, modes, current_mode, resumed, .. } => {
                // 换了新的内核会话（如回退后重开）：旧的不再用，交还内核收尾
                if let (Some(old), Some(kernel)) = (&self.acp_id, kernel)
                    && *old != session_id
                {
                    kernel.close_session(old);
                }
                self.acp_id = Some(session_id);
                self.ready = true;
                // 续接成功是理所当然的，不往对话里写；接不上才要说（agent 看不到之前的内容了）
                if std::mem::take(&mut self.resuming) && !resumed {
                    self.note("agent 不支持续接：已新开会话，agent 看不到之前的上下文");
                }
                if let Some(model) = model {
                    // provider/family/name → 只留最后一段，顶栏空间有限
                    self.model_label =
                        model.rsplit('/').next().unwrap_or(&model).to_string().into();
                }
                if !modes.is_empty() {
                    self.modes = modes
                        .into_iter()
                        .map(|m| ModeEntry {
                            icon: mode_icon(&m.id),
                            desc: m.description.unwrap_or_default().into(),
                            name: m.name.into(),
                            id: m.id,
                        })
                        .collect();
                    self.permission_mode = current_mode
                        .and_then(|id| self.modes.iter().position(|m| m.id == id))
                        .unwrap_or(0);
                }
                // 新建对话的默认模式 / 会话建立前选的模式：内核会话建立了才告诉它。
                // 内核没报这个模式（如 ACP agent）就作罢，保留上面按 `current_mode` 定下的那个
                if let Some(mode) = self.pending_mode.take()
                    && let Some(i) = self.modes.iter().position(|m| m.id == mode)
                {
                    self.permission_mode = i;
                    if let (Some(kernel), Some(id)) = (kernel, self.acp_id.clone())
                        && let Err(err) = kernel.set_mode(&id, &mode)
                    {
                        self.note(format!("切换权限模式失败：{err}"));
                    }
                }
                // 会话建立前在下拉里选过模型：现在告诉内核
                if let (Some(model), Some(kernel), Some(id)) =
                    (self.pending_model.take(), kernel, self.acp_id.clone())
                {
                    match kernel.set_model(&id, &model) {
                        Ok(()) => self.model_label = model.into(),
                        Err(err) => self.note(format!("切换模型失败：{err}")),
                    }
                }
                // agent 档案记着上次用的模型：新会话接上来就切过去（agent 没报模型时才补）
                else if let Some(model) = self.last_model.clone()
                    && self.model_label == UNKNOWN_MODEL
                    && let Some(kernel) = kernel
                    && let Some(id) = self.acp_id.clone()
                    && let Err(err) = kernel.set_model(&id, &model)
                {
                    self.note(format!("切换模型失败：{err}"));
                }
                return self.autoprompt.take();
            }
            KernelEvent::AgentMessageChunk { text } => {
                self.finish_thinking();
                self.turn_streamed.push(&text);
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
            KernelEvent::AgentThoughtChunk { text } => {
                self.turn_streamed.push(&text);
                match self.transcript.last_mut() {
                    Some(ChatItem::Thinking { text: buf, streaming: true, .. }) => {
                        buf.push_str(&text)
                    }
                    // 默认收起：流式时只扫光「思考中…」，不先展开再收回（那样每段思考都会跳一下）
                    _ => {
                        let started = (!self.replaying).then(Instant::now);
                        self.transcript.push(ChatItem::thinking(text, started));
                    }
                }
            }
            KernelEvent::SubagentStarted { task, agent, prompt } => {
                *self.subagent(&task) =
                    SubagentRun { agent: agent.into(), prompt: prompt.into(), items: Vec::new() };
            }
            KernelEvent::SubagentChunk { task, text, thought } => {
                self.turn_streamed.push(&text);
                self.subagent(&task).push_text(&text, thought);
            }
            KernelEvent::ToolCallStarted { id, title, status, kind, parent } => {
                self.finish_thinking();
                let (name, arg) = tool_label(&kind, &title);
                let card = ToolCard {
                    id: Some(id),
                    kind,
                    name,
                    arg,
                    result: SharedString::default(),
                    output: SharedString::default(),
                    phase: tool_phase(&status),
                    diffs: Vec::new(),
                    expanded: false,
                    task: false,
                    grant: None,
                };
                // 子 agent 的调用归到它那个子任务里，不进对话记录
                match parent {
                    Some(task) => self.subagent(&task).push_tool(card),
                    None => self.transcript.push(ChatItem::Tool(card)),
                }
            }
            KernelEvent::ToolCallUpdated { id, title, status, raw_output, output } => {
                if let Some(card) = self.tool_card(&id) {
                    if let Some(title) = title {
                        let (name, arg) = tool_label(&card.kind, &title);
                        card.name = name;
                        card.arg = arg;
                    }
                    if let Some(status) = status {
                        card.phase = tool_phase(&status);
                    }
                    if let Some(raw) = raw_output {
                        card.result = summarize_output(&raw).into();
                    }
                    if let Some(output) = output {
                        card.output = output.into();
                    }
                }
            }
            KernelEvent::ToolCallProgress { id, output } => {
                if let Some(card) = self.tool_card(&id)
                    && card.phase == ToolPhase::Running
                {
                    card.output = output.into();
                }
            }
            KernelEvent::ToolCallDiff { id, diff } => {
                if let Some(card) = self.tool_card(&id) {
                    card.set_diff(diff);
                }
            }
            KernelEvent::PlanReplaced { entries } => {
                self.finish_thinking();
                let rows: Vec<PlanRow> = entries
                    .into_iter()
                    .map(|e| PlanRow {
                        status: PlanStatus::parse(&e.status),
                        text: e.content.into(),
                    })
                    .collect();
                let turn_start = self.turn_start();
                let existing = self.transcript[turn_start..]
                    .iter()
                    .rposition(|i| matches!(i, ChatItem::Plan(_)))
                    .map(|i| i + turn_start);
                match existing {
                    Some(i) => self.transcript[i] = ChatItem::Plan(rows),
                    None => self.transcript.push(ChatItem::Plan(rows)),
                }
            }
            KernelEvent::PermissionAuto { title, chosen, tool } => {
                // 知道是哪个工具调用的：标在那一行上，不另起一行
                let card = tool.and_then(|tool| {
                    self.transcript.iter_mut().rev().find_map(|i| match i {
                        ChatItem::Tool(card) if card.id.as_deref() == Some(tool.as_str()) => {
                            Some(card)
                        }
                        _ => None,
                    })
                });
                match card {
                    Some(card) => {
                        card.grant = Some(Grant { kind: GrantKind::Auto, label: chosen.into() })
                    }
                    None => self.transcript.push(ChatItem::AutoApproved {
                        title: title.into(),
                        chosen: chosen.into(),
                    }),
                }
            }
            KernelEvent::PermissionRequest { id, title, options, tool } => {
                self.finish_thinking();
                self.transcript.push(ChatItem::Permission {
                    id,
                    title: title.into(),
                    options,
                    resolved: None,
                    tool,
                    merged: false,
                });
            }
            KernelEvent::Compacting => {
                self.finish_thinking();
                self.transcript
                    .push(ChatItem::Compaction { state: CompactState::Running, open: false });
            }
            KernelEvent::Compacted { summary, context_tokens } => {
                self.settle_compaction(CompactState::Done(summary.into()));
                if context_tokens > 0 {
                    self.context_tokens = context_tokens;
                }
            }
            KernelEvent::CompactFailed { message } => {
                self.settle_compaction(CompactState::Failed(message.into()));
            }
            KernelEvent::PromptFailed { message } => {
                self.settle_compaction(CompactState::Failed("已中断".into()));
                self.finish_thinking();
                self.busy = false;
                self.stopping = false;
                self.cancel_pending_permissions(kernel);
                self.interrupt_running_tools();
                self.fail(message);
            }
            KernelEvent::TurnEnded { stop_reason } => {
                self.settle_compaction(CompactState::Failed(
                    if stop_reason == "cancelled" { "已取消" } else { "已中断" }.into(),
                ));
                self.finish_thinking();
                self.busy = false;
                // 用户停下的：「已请求停止」已经说过了，不再补一行「本轮结束：cancelled」
                let stopped = std::mem::take(&mut self.stopping) && stop_reason == "cancelled";
                self.cancel_pending_permissions(kernel);
                self.interrupt_running_tools();
                if stop_reason != "end_turn" && !stopped {
                    self.note(format!("本轮结束：{stop_reason}"));
                }
            }
            // 插件钩子出错等：会话里提示一行
            KernelEvent::Notice(text) => self.note(text),
            // 连接级事件由 ChatApp 处理；其余（状态、fs、回显）界面不展示
            KernelEvent::Connected { .. }
            | KernelEvent::SessionFailed { .. }
            | KernelEvent::Fatal(_)
            | KernelEvent::Status(_)
            | KernelEvent::UserMessageChunk { .. }
            | KernelEvent::FsRead { .. }
            | KernelEvent::FsWrite { .. }
            | KernelEvent::FileChanged { .. }
            | KernelEvent::McpStatus { .. }
            | KernelEvent::BackgroundTasks { .. }
            | KernelEvent::MemoryChanged { .. }
            | KernelEvent::MemorySuggested { .. } => {}
            KernelEvent::TitleGenerated { title } => {
                // 模型起的标题替换截断版（用户手动改过的不覆盖）
                if !self.renamed {
                    self.title = title.clone().into();
                    self.title_generated = true;
                }
            }
            KernelEvent::UsageUpdated {
                input_tokens,
                output_tokens,
                reasoning_tokens,
                context_tokens,
                cache_read_tokens,
            } => {
                // 端点不报缓存时是 0，和「一点没命中」分不开，所以 0 也当不知道
                self.cache_hit = crate::usage::cache_hit_percent(cache_read_tokens, input_tokens);
                // 到这里的输出已经被端点数过，估算的那部分清掉（之后新收到的再重新估）
                self.turn_streamed.clear();
                self.turn_usage.reported = true;
                self.turn_usage.input += input_tokens;
                self.turn_usage.output += output_tokens;
                self.turn_usage.reasoning += reasoning_tokens;
                if context_tokens > 0 {
                    self.context_tokens = context_tokens;
                }
            }
        }
        None
    }

    /// 第 `task` 个 task 调用的子任务（没有就建一个），并把那一行标成子任务卡片。
    fn subagent(&mut self, task: &str) -> &mut SubagentRun {
        if let Some(card) = self.tool_card(task) {
            card.task = true;
        }
        self.subagents.entry(task.to_string()).or_default()
    }

    /// 按 tool_call_id 找最近的工具行；子 agent 的调用（`task 调用 ID/调用 ID`）在它那个子任务里找。
    fn tool_card(&mut self, id: &str) -> Option<&mut ToolCard> {
        fn find<'a>(item: &'a mut ChatItem, id: &str) -> Option<&'a mut ToolCard> {
            match item {
                ChatItem::Tool(card) if card.id.as_deref() == Some(id) => Some(card),
                _ => None,
            }
        }
        if let Some((task, _)) = id.split_once('/')
            && let Some(run) = self.subagents.get_mut(task)
        {
            return run.items.iter_mut().rev().find_map(|i| find(i, id));
        }
        self.transcript.iter_mut().rev().find_map(|i| find(i, id))
    }

    pub fn apply_mock(&mut self, action: &MockAction) {
        match action {
            MockAction::BeginThinking => {
                self.transcript.push(ChatItem::thinking(String::new(), Some(Instant::now())));
            }
            MockAction::ThinkChunk(chunk) => {
                if let Some(ChatItem::Thinking { text, .. }) = self.transcript.last_mut() {
                    text.push_str(chunk);
                }
            }
            MockAction::EndThinking => self.finish_thinking(),
            MockAction::BeginTool(name, arg) => {
                self.transcript.push(ChatItem::Tool(ToolCard {
                    id: None,
                    kind: String::new(),
                    name: (*name).into(),
                    arg: (*arg).into(),
                    result: SharedString::default(),
                    output: SharedString::default(),
                    phase: ToolPhase::Running,
                    diffs: Vec::new(),
                    expanded: false,
                    task: false,
                    grant: None,
                }));
            }
            MockAction::ToolDone(result) => {
                if let Some(ChatItem::Tool(card)) = self.transcript.last_mut() {
                    card.result = (*result).into();
                    card.phase = ToolPhase::Done;
                }
            }
            MockAction::Diff(path, old, new) => {
                if let Some(ChatItem::Tool(card)) = self.transcript.last_mut() {
                    card.set_diff(DiffView::compute(*path, Some(old), new));
                }
            }
            MockAction::BeginPlan => {
                self.transcript.push(ChatItem::Plan(Vec::new()));
            }
            MockAction::PlanRowAdd(row) => {
                if let Some(ChatItem::Plan(rows)) = self.transcript.last_mut() {
                    // 第一项一加进来就算在做
                    let status =
                        if rows.is_empty() { PlanStatus::InProgress } else { PlanStatus::Pending };
                    rows.push(PlanRow { text: (*row).into(), status });
                }
            }
            MockAction::PlanRowDone(i) => {
                if let Some(ChatItem::Plan(rows)) = self.transcript.last_mut() {
                    if let Some(row) = rows.get_mut(*i) {
                        row.status = PlanStatus::Done;
                    }
                    // 下一项接着做
                    if let Some(next) = rows.get_mut(*i + 1)
                        && next.status == PlanStatus::Pending
                    {
                        next.status = PlanStatus::InProgress;
                    }
                }
            }
            MockAction::BeginAgent => {
                self.transcript.push(ChatItem::Agent(String::new()));
            }
            MockAction::AgentChunk(chunk) => {
                if let Some(ChatItem::Agent(text)) = self.transcript.last_mut() {
                    text.push_str(chunk);
                }
            }
            MockAction::EndAgent => {}
            MockAction::Note(text) => {
                self.transcript.push(ChatItem::Note((*text).into()));
            }
        }
    }
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
    use super::*;
    use claudesktop_core::PermissionOptionView;

    // 注意：本文件顶层 `use gpui_kit::*` 会把 gpui 的 test 宏带进作用域，
    // 遮蔽内建 #[test]，必须用全路径（原文件其他测试同理）。
    #[core::prelude::v1::test]
    fn markdown_export_covers_item_kinds() {
        let mut s = Session::new(1, KernelChoice::Demo, PathBuf::from("C:/proj"));
        s.title = "测试会话".into();
        s.note("一条备注");
        let user = ChatItem::User { text: "你好".into(), images: Vec::new() };
        let agent = ChatItem::Agent("**回复**".into());
        let card = ToolCard {
            id: None,
            kind: "read".into(),
            name: "Read".into(),
            arg: "README.md".into(),
            result: "12 行".into(),
            output: SharedString::default(),
            phase: ToolPhase::Done,
            diffs: Vec::new(),
            expanded: false,
            task: false,
            grant: None,
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
        let user = |t: &str| ChatItem::User { text: t.into(), images: Vec::new() };
        let tool = || {
            ChatItem::Tool(ToolCard {
                id: None,
                kind: "edit".into(),
                name: "Edit".into(),
                arg: "a.rs".into(),
                result: "ok".into(),
                output: SharedString::default(),
                phase: ToolPhase::Done,
                diffs: Vec::new(),
                expanded: false,
                task: false,
                grant: None,
            })
        };
        s.transcript = vec![
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
        fresh.transcript = vec![
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
        })
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
    fn list_tracks_new_and_changed_rows() {
        let mut s = Session::new(1, KernelChoice::Demo, PathBuf::from("."));
        s.transcript.push(ChatItem::User { text: "hi".into(), images: Vec::new() });
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
        let user = || ChatItem::User { text: "q".into(), images: Vec::new() };
        s.transcript = vec![
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
    fn subagent_events_go_to_their_own_task() {
        let mut s = Session::new(1, KernelChoice::Builtin, PathBuf::from("."));
        s.transcript.push(ChatItem::User { text: "查".into(), images: Vec::new() });
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
        s.transcript.push(ChatItem::User { text: "hi".into(), images: Vec::new() });
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
        s.transcript.push(ChatItem::User { text: "跑一下".into(), images: Vec::new() });
        s.begin_turn();
        s.transcript.push(ChatItem::Tool(ToolCard {
            id: None,
            kind: "execute".into(),
            name: "Bash".into(),
            arg: "echo hi".into(),
            result: SharedString::default(),
            output: SharedString::default(),
            phase: ToolPhase::Running,
            diffs: Vec::new(),
            expanded: false,
            task: false,
            grant: None,
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
        s.transcript.push(ChatItem::User { text: "hi".into(), images: Vec::new() });
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
        s.transcript.push(ChatItem::User { text: "hi".into(), images: Vec::new() });
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
        s.transcript.push(ChatItem::User { text: "上一轮".into(), images: Vec::new() });
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
        s.transcript.push(ChatItem::User { text: "这一轮".into(), images: Vec::new() });
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
        // 一轮结束：用量和用时记到这一轮名下，回复那行能查到
        let summary = s.close_turn();
        assert_eq!(summary.usage.input, 200);
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
        s.transcript.push(ChatItem::User { text: "q".into(), images: Vec::new() });
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
        s.transcript.push(ChatItem::User { text: "q".into(), images: Vec::new() });
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

    #[core::prelude::v1::test]
    fn history_sessions_keep_their_own_mode() {
        let mut meta = meta();
        meta.agent = "builtin:test".into();
        let s = Session::from_records(1, meta, vec![Record::user("问")], &Default::default());
        assert!(s.pending_mode.is_none());
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
        s.transcript.push(ChatItem::User { text: "q".into(), images: Vec::new() });
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

        // 出了正文：组收起，只剩组头
        s.apply_event(KernelEvent::AgentMessageChunk { text: "好了".into() }, None);
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
        s.transcript.push(ChatItem::User { text: "q".into(), images: Vec::new() });
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
        assert_eq!(s.compute_layout(ViewMode::Normal)[1].head.unwrap().end, 4);
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
        let l = s.compute_layout(ViewMode::Normal);
        // 带 diff 的编辑单独成行，把前后断成两组
        assert_eq!(l[1].head.unwrap().end, 3);
        assert!(l[3].body && l[3].head.is_none());
        assert_eq!(l[4].head.unwrap().end, 6);
        assert!(matches!(s.transcript[4], ChatItem::AutoApproved { .. }));
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
    fn plan_pins_while_turn_runs() {
        let mut s = Session::new(1, KernelChoice::Acp("custom-1".into()), PathBuf::from("."));
        s.transcript.push(ChatItem::User { text: "q".into(), images: Vec::new() });
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
        assert!(Session::from_records(1, meta(), records.clone(), &Default::default()).review_pending);
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
        s.transcript.push(ChatItem::User { text: "q".into(), images: Vec::new() });
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
        s.transcript.push(ChatItem::User { text: "q2".into(), images: Vec::new() });
        assert_eq!(s.attention(&ended("end_turn")).as_deref(), Some("回复完了"));
        // 长回复截断
        let long = "很长".repeat(100);
        assert!(preview_line(&long).unwrap().ends_with('…'));
        assert_eq!(preview_line(&long).unwrap().chars().count(), 81);
    }
}
