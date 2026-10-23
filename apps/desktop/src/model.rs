//! 对话记录的数据模型、模式表与工具行辅助函数（UI 无关逻辑）。

use claudesktop_core::{
    DiffView, PermissionHunks, PermissionOptionView, PromptImage, QuestionAnswer, QuestionView,
    UserPrompt,
};
use gpui_kit::SharedString;
use gpui_kit::assets::IconName as IconAll;
use std::time::Instant;

#[derive(Clone, Copy, PartialEq, Hash)]
pub enum ToolPhase {
    Running,
    Done,
    Failed,
}

/// 对话的视图模式（对照官方 Summary / Normal / Verbose）：只决定看多少过程，不影响内核怎么跑。
#[derive(
    Clone, Copy, Debug, Default, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize,
)]
#[serde(rename_all = "snake_case")]
pub enum ViewMode {
    /// 只看结果：隐藏思考、启动信息、计划和工具调用（正在等你看 diff 的那一步除外）
    Summary,
    /// 连续的过程步骤收成一行摘要（进行中时露出最近几步），思考默认收起
    #[default]
    Normal,
    /// 全部展开：每一步、思考、启动信息、完整 diff
    Verbose,
}

impl ViewMode {
    pub const ALL: [ViewMode; 3] = [ViewMode::Summary, ViewMode::Normal, ViewMode::Verbose];

    pub fn label(self) -> &'static str {
        match self {
            ViewMode::Summary => "摘要",
            ViewMode::Normal => "标准",
            ViewMode::Verbose => "详细",
        }
    }

    pub fn desc(self) -> &'static str {
        match self {
            ViewMode::Summary => "只看回复和需要你处理的事",
            ViewMode::Normal => "过程步骤收成一行摘要，思考默认收起",
            ViewMode::Verbose => "展开每一步、思考和完整 diff",
        }
    }

    pub fn icon(self) -> IconAll {
        match self {
            ViewMode::Summary => IconAll::ListCollapse,
            ViewMode::Normal => IconAll::Rows3,
            ViewMode::Verbose => IconAll::ScrollText,
        }
    }

    /// Ctrl+O 循环的下一档
    pub fn next(self) -> Self {
        match self {
            ViewMode::Summary => ViewMode::Normal,
            ViewMode::Normal => ViewMode::Verbose,
            ViewMode::Verbose => ViewMode::Summary,
        }
    }
}

/// 工具调用：一行「种类图标 + 名称 + 参数 + 结果摘要」；带 diff 时 diff 卡片的表头兼当这一行
pub struct ToolCard {
    /// 内核侧的 tool_call_id（演示脚本为 None）
    pub id: Option<String>,
    /// 工具种类（read / execute / …），标题里没有工具名时据此命名
    pub kind: String,
    pub name: SharedString,
    pub arg: SharedString,
    pub result: SharedString,
    /// 输出正文预览（内核截好的，见 `claudesktop_core::output_preview`）；空 = 没有可看的
    pub output: SharedString,
    pub phase: ToolPhase,
    /// 该调用改动的文件（同一路径只保留最新一份）
    pub diffs: Vec<DiffView>,
    /// 带 diff：已展开全部行（默认只显示前 [`DIFF_PREVIEW_ROWS`] 行）；
    /// 不带 diff：工具行已点开，显示输出预览
    pub expanded: bool,
    /// 派出了子 agent 的 task 调用：画成子任务卡片（不收进过程组），点开看它的过程
    /// （`Session::subagents`）；子 agent 自己的调用不在对话记录里
    pub task: bool,
    /// 这次调用的授权结果（权限请求应答后并进这一行，不再单独占一行）
    pub grant: Option<Grant>,
    /// 工具带回来的图片（`read_file` 读到的图、MCP 的多模态结果）：工具行里画缩略图。
    /// 不进日志，续接后不再有（图片文件还在磁盘上）。
    pub images: Vec<ToolImage>,
}

/// 工具行上的授权结果：种类决定短标签，`label` 是选的那个选项的全名（悬停看）。
#[derive(Clone, Debug, PartialEq)]
pub struct Grant {
    pub kind: GrantKind,
    pub label: SharedString,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum GrantKind {
    /// 你点了「允许一次」
    Once,
    /// 你点了「总是允许…」（存成了规则）
    Always,
    /// 按策略自动批准（没问你）
    Auto,
    /// 你拒绝了
    Denied,
    /// 没等你应答这一轮就停了
    Cancelled,
}

impl Grant {
    /// 按内核给的选项种类（`allow_once` / `allow_always` / `reject_*`）归类；取消单独算。
    pub fn from_choice(option_kind: Option<&str>, label: SharedString) -> Self {
        let kind = match option_kind {
            Some("allow_once") => GrantKind::Once,
            Some("allow_always") => GrantKind::Always,
            Some(k) if k.starts_with("allow") => GrantKind::Once,
            Some(_) => GrantKind::Denied,
            None => GrantKind::Cancelled,
        };
        Self { kind, label }
    }

    /// 工具行末尾的短标签。
    pub fn short(&self) -> &'static str {
        match self.kind {
            GrantKind::Once => "已允许",
            GrantKind::Always => "总是允许",
            GrantKind::Auto => "自动允许",
            GrantKind::Denied => "已拒绝",
            GrantKind::Cancelled => "未应答",
        }
    }
}

/// 折叠状态下每个 diff 最多显示的行数
pub const DIFF_PREVIEW_ROWS: usize = 40;

impl ToolCard {
    /// 命令类工具：点开后先列完整命令（工具行上只有截断的一行）。
    pub fn shows_command(&self) -> bool {
        self.kind == "execute" && !self.arg.trim().is_empty()
    }

    /// 工具行能不能点开：有输出可看、有图片可看，或者是命令（跑着、还没输出时也能点开看完整命令）。
    pub fn can_open(&self) -> bool {
        self.shows_command() || !self.output.trim().is_empty() || !self.images.is_empty()
    }

    /// 记录一份 diff：同一路径替换（执行前的预览会被之后的结果覆盖）。
    pub fn set_diff(&mut self, diff: DiffView) {
        match self.diffs.iter_mut().find(|d| d.path == diff.path) {
            Some(slot) => *slot = diff,
            None => self.diffs.push(diff),
        }
    }

    /// 工具种类（read / search / execute / …）；内核没给（演示脚本）时按工具名猜。
    pub fn kind_key(&self) -> &str {
        // 派子 agent 的那一行（自研内核和 Claude Code 都叫 Task）
        if self.name.as_ref() == "Task" {
            return "task";
        }
        if !self.kind.is_empty() {
            return &self.kind;
        }
        match self.name.as_ref() {
            "Read" => "read",
            "Grep" | "Glob" | "Search" => "search",
            "Bash" => "execute",
            "Edit" | "Write" | "MultiEdit" => "edit",
            "WebFetch" | "Fetch" | "WebSearch" => "fetch",
            _ => "other",
        }
    }
}

/// 工具结果里带回来的图片（`read_file` 读到的图、MCP 的多模态结果）：
/// 缩略图 + 原始字节（点开看大图、另存为都要用原字节）。
#[derive(Clone)]
pub struct ToolImage {
    pub image: std::sync::Arc<gpui_kit::Image>,
    /// 内核给的字节（已经重编码过；另存为写这份）
    pub bytes: std::sync::Arc<Vec<u8>>,
    /// 另存为时用的扩展名
    pub ext: &'static str,
}

/// 内核发来的图片 → 工具行缩略图。内核给的已经是重编码过的（PNG / JPEG / GIF），
/// 解不开就跳过——宁可没图，也不要界面上留一个空框。
pub fn thumb_from_prompt_image(image: &PromptImage) -> Option<ToolImage> {
    use base64::Engine as _;
    let bytes = base64::engine::general_purpose::STANDARD.decode(image.data.as_bytes()).ok()?;
    let (format, ext) = match image.media_type.as_str() {
        "image/png" => (gpui_kit::ImageFormat::Png, "png"),
        "image/gif" => (gpui_kit::ImageFormat::Gif, "gif"),
        "image/webp" => (gpui_kit::ImageFormat::Webp, "webp"),
        "image/bmp" => (gpui_kit::ImageFormat::Bmp, "bmp"),
        _ => (gpui_kit::ImageFormat::Jpeg, "jpg"),
    };
    Some(ToolImage {
        image: std::sync::Arc::new(gpui_kit::Image::from_bytes(format, bytes.clone())),
        bytes: std::sync::Arc::new(bytes),
        ext,
    })
}

/// 工具行左侧的种类图标。
pub fn tool_icon(kind: &str) -> IconAll {
    match kind {
        "read" => IconAll::FileText,
        "search" => IconAll::TextSearch,
        "execute" => IconAll::SquareTerminal,
        "edit" => IconAll::FilePen,
        "delete" => IconAll::Trash,
        "move" => IconAll::ArrowRightLeft,
        "fetch" => IconAll::Globe,
        "think" => IconAll::Brain,
        "task" => IconAll::Bot,
        _ => IconAll::Wrench,
    }
}

/// 计划条目的状态（ACP：pending / in_progress / completed）。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum PlanStatus {
    Pending,
    InProgress,
    Done,
}

impl PlanStatus {
    pub fn parse(status: &str) -> Self {
        match status {
            "completed" => Self::Done,
            "in_progress" => Self::InProgress,
            _ => Self::Pending,
        }
    }
}

pub struct PlanRow {
    pub text: SharedString,
    pub status: PlanStatus,
}

/// 计划进度：(已完成, 总数, 正在做的那一项)。
pub fn plan_progress(rows: &[PlanRow]) -> (usize, usize, Option<&PlanRow>) {
    let done = rows.iter().filter(|r| r.status == PlanStatus::Done).count();
    let current = rows.iter().find(|r| r.status == PlanStatus::InProgress);
    (done, rows.len(), current)
}

/// 同一个文件的两条路径：Windows 上大小写和分隔符都不保证统一（`C:\a\B.rs` / `c:/a/b.rs`）。
pub(crate) fn same_path(a: &str, b: &str) -> bool {
    fn norm(p: &str) -> String {
        p.replace('\\', "/").to_lowercase()
    }
    a == b || norm(a) == norm(b)
}

/// 一轮里改过的一个文件（回复末尾改动卡片里的一行）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TurnFile {
    pub path: String,
    pub added: u32,
    pub removed: u32,
    /// 这一轮新建的
    pub created: bool,
    /// 最后一次改它的工具行（点了跳过去）
    pub item: usize,
}

/// `items`（一轮的对话行，下标从 `offset` 起算）里改过的文件：按第一次出现的顺序，
/// 同一文件改了几次就把增删行数加起来（`created` 取最后一次：先建后改就成了普通修改）。
/// 没改成的（失败、还在等授权）不算。
/// 同一个文件的路径大小写可能不一样（Windows 上工具回报的路径不保证规范），按忽略大小写合并。
pub fn turn_files(items: &[ChatItem], offset: usize) -> Vec<TurnFile> {
    let mut files: Vec<TurnFile> = Vec::new();
    for (i, item) in items.iter().enumerate() {
        let ChatItem::Tool(card) = item else { continue };
        if card.phase != ToolPhase::Done {
            continue;
        }
        for diff in &card.diffs {
            match files.iter_mut().find(|f| same_path(&f.path, &diff.path)) {
                Some(f) => {
                    f.added += diff.added;
                    f.removed += diff.removed;
                    // 后面又整份写过一次就不算「新文件」了（`created` 只认最后一次的状态）
                    f.created = diff.created;
                    f.item = offset + i;
                }
                None => files.push(TurnFile {
                    path: diff.path.clone(),
                    added: diff.added,
                    removed: diff.removed,
                    created: diff.created,
                    item: offset + i,
                }),
            }
        }
    }
    files
}

/// 过程组的一行摘要（「读取 5 个文件，搜索 3 次，运行 2 条命令」）和失败的步数。
/// 读文件按路径去重；只有思考时说「已思考」。
pub fn group_summary<'a>(items: impl IntoIterator<Item = &'a ChatItem>) -> (String, usize) {
    let mut read_args: Vec<&str> = Vec::new();
    let (mut search, mut exec, mut edit, mut fetch, mut other, mut auto, mut think, mut failed) =
        (0, 0, 0, 0, 0, 0, 0, 0);
    let mut tasks = 0;
    // 思考总用时；有一段不知道（旧日志）就整体不报
    let mut think_ms = Some(0);
    for item in items {
        match item {
            ChatItem::Tool(card) => {
                if card.phase == ToolPhase::Failed {
                    failed += 1;
                }
                match card.kind_key() {
                    "read" => {
                        if !read_args.contains(&card.arg.as_ref()) {
                            read_args.push(card.arg.as_ref());
                        }
                    }
                    "search" => search += 1,
                    "execute" => exec += 1,
                    "edit" => edit += 1,
                    "fetch" => fetch += 1,
                    "task" => tasks += 1,
                    _ => other += 1,
                }
            }
            ChatItem::AutoApproved { .. } => auto += 1,
            ChatItem::Thinking { elapsed_ms, .. } => {
                think += 1;
                think_ms = think_ms.zip(*elapsed_ms).map(|(a, b)| a + b);
            }
            _ => {}
        }
    }
    let mut parts = Vec::new();
    let mut push = |n: usize, text: String| {
        if n > 0 {
            parts.push(text);
        }
    };
    push(tasks, format!("派出 {tasks} 个子任务"));
    push(read_args.len(), format!("读取 {} 个文件", read_args.len()));
    push(search, format!("搜索 {search} 次"));
    push(exec, format!("运行 {exec} 条命令"));
    push(edit, format!("编辑 {edit} 次"));
    push(fetch, format!("访问网页 {fetch} 次"));
    push(other, format!("调用工具 {other} 次"));
    push(auto, format!("自动批准 {auto} 次"));
    if parts.is_empty() && think > 0 {
        parts.push(match (think, think_ms) {
            (1, ms) => thought_label(ms),
            (n, Some(ms)) => format!("思考 {n} 次，共 {}", duration_label(ms)),
            (n, None) => format!("已思考 {n} 次"),
        });
    }
    (parts.join("，"), failed)
}

/// 思考块的标题：「已思考 12 秒」；不知道用时（旧日志）时只说「已思考」。
pub fn thought_label(elapsed_ms: Option<u64>) -> String {
    match elapsed_ms {
        Some(ms) => format!("已思考 {}", duration_label(ms)),
        None => "已思考".into(),
    }
}

/// `12 秒` / `1 分 5 秒` / `2 分钟`；四舍五入到秒，不足 1 秒按 1 秒说。
fn duration_label(ms: u64) -> String {
    let secs = ms.saturating_add(500) / 1000;
    match (secs.max(1) / 60, secs.max(1) % 60) {
        (0, s) => format!("{s} 秒"),
        (m, 0) => format!("{m} 分钟"),
        (m, s) => format!("{m} 分 {s} 秒"),
    }
}

pub enum ChatItem {
    User {
        text: SharedString,
        /// 图片附件的绝对路径（气泡里渲染缩略图；发给内核时读回 base64）
        images: Vec<std::path::PathBuf>,
        /// 聊天的文本附件（重试 / 换内核重跑时原样再带上）。只在这次运行里有：
        /// 从日志恢复的会话为空（内容只在日志的 `sent` 里，已经拼进上下文了）
        files: std::sync::Arc<Vec<claudesktop_core::attach::TextAttachment>>,
    },
    Thinking {
        text: String,
        streaming: bool,
        open: bool,
        /// 开始流式的时刻（只有实时的才有；重放历史时为 None，用时从日志里取）
        started: Option<Instant>,
        /// 想了多久：收尾时算出，或从日志恢复；旧日志没有
        elapsed_ms: Option<u64>,
    },
    /// agent 在首条提示之前推送的启动信息（如 pi 的 Skills / Extensions 列表），默认折叠
    Info {
        text: String,
        open: bool,
    },
    Tool(ToolCard),
    /// 权限请求已按规则自动应答（不打断用户，作为过程中的一步显示）
    AutoApproved {
        title: SharedString,
        chosen: SharedString,
    },
    /// agent 的权限请求；`resolved` 有值后按钮消失，显示所选结果
    Permission {
        id: u64,
        title: SharedString,
        options: Vec<PermissionOptionView>,
        resolved: Option<SharedString>,
        /// 触发请求的工具调用 ID（旧日志没有）
        tool: Option<String>,
        /// 应答结果已并进那个工具行：这一行不再画（见 `Session::merge_permission`）
        merged: bool,
        /// 这次改动可以按块部分同意（自研内核的写 / 改文件）：卡片上列出这些块，可以取消勾选
        hunks: Option<PermissionHunks>,
        /// 用户在卡片上取消勾选之后放行的块（`None` = 没动过，全放行）
        keep: Option<Vec<u32>>,
    },
    /// 模型向你提问（`ask_user`）：选项卡片。`answer` 有值后不能再改
    Question {
        id: u64,
        /// 触发它的工具调用 ID（应答后结果并进那一行；现在没有工具行，留着给以后）
        tool: Option<String>,
        questions: Vec<QuestionView>,
        /// 每题当前点着的选项（提交前可改；`None` = 这题还没点）
        picks: Vec<Option<usize>>,
        /// 提交后的结果；`None` = 还在等你答
        answer: Option<QuestionAnswer>,
    },
    Plan(Vec<PlanRow>),
    Agent(String),
    Note(SharedString),
    /// 本轮失败（发送失败或内核报错）：卡片按原因写标题和建议，带「重试」等按钮
    Failed {
        message: SharedString,
        kind: FailureKind,
    },
    /// 上下文压缩（自研内核）：一条分隔线，完成后可以点开看摘要
    Compaction {
        state: CompactState,
        open: bool,
    },
}

/// 上下文压缩的进度。
#[derive(Clone, Debug, PartialEq)]
pub enum CompactState {
    Running,
    /// 完成，带模型写的摘要
    Done(SharedString),
    /// 失败或被中断，带原因
    Failed(SharedString),
}

/// 输入框里待发送的一张图片附件（已编好 base64，预览用 gpui Image）。
#[derive(Clone)]
pub struct PendingImage {
    /// 这次运行里唯一的编号：后台处理完按它对回（下标会因删除而挪，名字会重复，如「剪贴板图片」）
    pub id: u64,
    /// 原始文件名（悬停提示用）
    pub name: SharedString,
    /// 如 `image/png`
    pub media_type: String,
    /// 图片字节（base64 编码；发送与落盘共用）
    pub data: String,
    /// 输入框上方的预览
    pub preview: gpui_kit::Image,
    /// 处理后的宽高（悬停提示「1568×1176 · 约 2.5k token」）；还没处理完是 None
    pub size: Option<(u32, u32)>,
    /// 还在后台转正 / 缩小（缩略图先半透明占位）
    pub preparing: bool,
}

impl PendingImage {
    /// 从图片字节构造（格式已知，如剪贴板里的 gpui Image）。
    pub fn from_bytes(name: SharedString, bytes: Vec<u8>, format: gpui_kit::ImageFormat) -> Self {
        use base64::Engine as _;
        static NEXT_ID: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
        Self {
            id: NEXT_ID.fetch_add(1, std::sync::atomic::Ordering::Relaxed),
            name,
            media_type: format.mime_type().to_string(),
            data: base64::engine::general_purpose::STANDARD.encode(&bytes),
            preview: gpui_kit::Image::from_bytes(format, bytes),
            size: None,
            preparing: true,
        }
    }

    /// 已经处理好的那张（发送前转正 / 缩小 / 重编码的结果，见 `image_prep`）；`id` 沿用原来那张的。
    pub fn from_prepared(
        id: u64,
        name: SharedString,
        prepared: &claudesktop_core::image_prep::Prepared,
    ) -> Self {
        use base64::Engine as _;
        // 小动图原样保留（`image/gif`），其余重编码成 PNG / JPEG
        let format = match prepared.media_type.as_str() {
            "image/png" => gpui_kit::ImageFormat::Png,
            "image/gif" => gpui_kit::ImageFormat::Gif,
            _ => gpui_kit::ImageFormat::Jpeg,
        };
        Self {
            id,
            name,
            media_type: prepared.media_type.clone(),
            data: base64::engine::general_purpose::STANDARD.encode(&prepared.bytes),
            preview: gpui_kit::Image::from_bytes(format, prepared.bytes.clone()),
            size: Some((prepared.width, prepared.height)),
            preparing: false,
        }
    }

    /// 悬停提示：文件名 + 尺寸 + 大约多少 token。
    pub fn tip(&self) -> String {
        match self.size {
            Some((w, h)) => {
                let tokens = claudesktop_core::image_prep::Prepared {
                    media_type: self.media_type.clone(),
                    bytes: Vec::new(),
                    width: w,
                    height: h,
                }
                .approx_tokens();
                format!("{} · {w}×{h} · 约 {:.1}k token", self.name, tokens as f64 / 1000.)
            }
            None => self.name.to_string(),
        }
    }

    /// 从磁盘文件构造（按扩展名认格式）；认不出的扩展名或读不了返回 None。
    pub fn from_path(path: &std::path::Path) -> Option<Self> {
        use gpui_kit::ImageFormat;
        let ext = path.extension()?.to_str()?.to_ascii_lowercase();
        let format = match ext.as_str() {
            "png" => ImageFormat::Png,
            "jpg" | "jpeg" => ImageFormat::Jpeg,
            "gif" => ImageFormat::Gif,
            "webp" => ImageFormat::Webp,
            "bmp" => ImageFormat::Bmp,
            _ => return None,
        };
        let bytes = std::fs::read(path).ok()?;
        let name = path.file_name()?.to_string_lossy().into_owned().into();
        Some(Self::from_bytes(name, bytes, format))
    }
}

/// 一轮还在跑时用户又发的一条消息：先排队，这一轮正常结束后再发。
/// 附件原样留着（不转成 `PromptImage`），退回输入框时还有预览。
pub struct QueuedPrompt {
    pub text: String,
    pub images: Vec<PendingImage>,
    /// 聊天会话的文本附件（内容发送时才拼进去，退回输入框时还是原样）
    pub files: Vec<claudesktop_core::attach::TextAttachment>,
}

impl QueuedPrompt {
    pub fn into_prompt(self) -> UserPrompt {
        let images = self
            .images
            .into_iter()
            .map(|p| PromptImage { media_type: p.media_type, data: p.data })
            .collect();
        UserPrompt { text: self.text, images, attachments: self.files }
    }

    /// 队列里一行显示的文字：换行压成空格；只有图片时写「N 张图片」，带文件时在后面补一句。
    pub fn summary(&self) -> String {
        let text = self.text.split_whitespace().collect::<Vec<_>>().join(" ");
        let mut out = match (text.is_empty(), self.images.len()) {
            (true, n) => format!("{n} 张图片"),
            (false, 0) => text,
            (false, n) => format!("{text}（附 {n} 张图片）"),
        };
        if !self.files.is_empty() {
            out.push_str(&format!("（附 {} 个文件）", self.files.len()));
        }
        out
    }
}

impl ChatItem {
    /// 新开一段流式中的思考（默认收起）；`started` 为 None 时不计时（重放历史）。
    pub fn thinking(text: impl Into<String>, started: Option<Instant>) -> Self {
        Self::Thinking {
            text: text.into(),
            streaming: true,
            open: false,
            started,
            elapsed_ms: None,
        }
    }

    /// 这一行在某个视图模式下是否显示。摘要模式只留回复、用户消息、权限卡、备注和子任务卡片；
    /// 别的工具调用只在「执行中且带 diff」时留着——那是正等你对着改动决定放不放行的一步。
    pub fn visible_in(&self, view: ViewMode) -> bool {
        match (view, self) {
            (
                ViewMode::Summary,
                Self::Thinking { .. }
                | Self::Info { .. }
                | Self::Plan(_)
                | Self::AutoApproved { .. },
            ) => false,
            (ViewMode::Summary, Self::Tool(c)) => {
                c.task || c.phase == ToolPhase::Running && !c.diffs.is_empty()
            }
            _ => true,
        }
    }

    /// 过程中的一步：连续的几步合成一个可折叠的组（见 `Session::layout`）。
    /// 带 diff 的工具、子任务卡片不算——改了什么、派了谁去做要直接摆出来，不收进组里。
    pub fn is_step(&self) -> bool {
        match self {
            Self::Thinking { .. } | Self::AutoApproved { .. } => true,
            Self::Tool(c) => c.diffs.is_empty() && !c.task,
            // 并进工具行的权限请求：当成一步（不把前后的步骤拆成两组），但没东西可画
            Self::Permission { merged, .. } => *merged,
            _ => false,
        }
    }

    /// 作为一步有没有东西可画：空的思考块（只有换行）不画、不计数。
    pub fn has_content(&self) -> bool {
        match self {
            Self::Thinking { text, streaming, .. } => *streaming || !text.trim().is_empty(),
            Self::Permission { merged, .. } => !*merged,
            _ => true,
        }
    }

    /// 影响这一行渲染高度的内容摘要（虚拟列表据此判断要不要重新测量）。
    /// 内容只会追加或切换状态，所以长度 + 状态位就够了。
    pub fn fingerprint(&self) -> u64 {
        use std::hash::{Hash, Hasher};
        let mut h = std::collections::hash_map::DefaultHasher::new();
        match self {
            Self::User { text, images, .. } => (0u8, text.len(), images.len()).hash(&mut h),
            Self::Thinking { text, streaming, open, elapsed_ms, .. } => {
                (1u8, text.len(), streaming, open, elapsed_ms.is_some()).hash(&mut h)
            }
            Self::Info { text, open } => (2u8, text.len(), open).hash(&mut h),
            Self::Tool(c) => (
                3u8,
                c.name.len(),
                c.arg.len(),
                c.result.len(),
                c.output.len(),
                c.phase,
                c.expanded,
                c.diffs.iter().map(|d| d.lines.len() + 1).sum::<usize>(),
                c.task,
                c.grant.as_ref().map(|g| g.kind),
            )
                .hash(&mut h),
            // 按块同意：块数 / 行数决定卡片多高，勾掉几块会换标题文字（可能多折一行）
            Self::Permission { title, options, resolved, merged, hunks, keep, .. } => (
                4u8,
                title.len(),
                options.len(),
                resolved.as_ref().map(|r| r.len()),
                merged,
                hunks.as_ref().map(|h| {
                    h.files.iter().flat_map(|f| &f.hunks).map(|x| x.lines.len() + 1).sum::<usize>()
                }),
                keep.as_ref().map(Vec::len),
            )
                .hash(&mut h),
            Self::Question { questions, picks, answer, .. } => (
                11u8,
                questions.len(),
                questions
                    .iter()
                    .map(|q| {
                        q.question.len() + q.options.iter().map(|o| o.label.len()).sum::<usize>()
                    })
                    .sum::<usize>(),
                picks.iter().filter(|p| p.is_some()).count(),
                answer.is_some(),
            )
                .hash(&mut h),
            Self::Plan(rows) => {
                5u8.hash(&mut h);
                for r in rows {
                    (r.text.len(), r.status).hash(&mut h);
                }
            }
            Self::Agent(t) => (6u8, t.len()).hash(&mut h),
            Self::Note(t) => (7u8, t.len()).hash(&mut h),
            Self::Failed { message, kind } => (9u8, message.len(), kind).hash(&mut h),
            Self::Compaction { state, open } => {
                let (phase, len) = match state {
                    CompactState::Running => (0u8, 0),
                    CompactState::Done(s) => (1, s.len()),
                    CompactState::Failed(s) => (2, s.len()),
                };
                (10u8, phase, len, open).hash(&mut h)
            }
            Self::AutoApproved { title, chosen } => (8u8, title.len(), chosen.len()).hash(&mut h),
        }
        h.finish()
    }
}

pub struct StatCard {
    pub label: &'static str,
    pub value: SharedString,
}

/// 权限模式（对应官方桌面版的 Manual / Accept edits / Plan / Auto）：(名称, 说明, 图标, ACP mode id)。
/// 「自动」不在这张静态表里：要连上内核、并在设置页「内核与模型」里配好之后才出现。
pub const PERMISSION_MODES: [(&str, &str, IconAll, &str); 3] = [
    ("逐项确认", "每次编辑和命令都先询问", IconAll::ShieldCheck, "default"),
    ("自动接受编辑", "自动接受文件编辑，命令仍询问", IconAll::SquarePen, "acceptEdits"),
    ("计划模式", "只出计划，不改动文件", IconAll::ListTodo, "plan"),
];

/// 「自动」模式的 mode id（设置里开着时新会话默认用它；见 `Session::default_to`）
pub const MODE_AUTO_ID: &str = "auto";

/// 「自动」这一项本身。设置里开着「自动」时，会话还没连上内核（模式列表还是静态表）
/// 就先把它补进去，chip 上显示的名字才和新建对话的默认模式对得上。
pub fn auto_mode_entry() -> ModeEntry {
    ModeEntry {
        id: MODE_AUTO_ID.into(),
        name: "自动".into(),
        desc: "由分类器判断：放行常规操作，拦下越界的和不可逆的".into(),
        icon: IconAll::Zap,
    }
}

/// 下拉里的一项模式：mock / 演示用上面的静态表；连上真实 agent 后改用
/// agent 自己上报的（Claude = 权限模式，pi = 思考强度）。
pub struct ModeEntry {
    pub id: String,
    pub name: SharedString,
    pub desc: SharedString,
    pub icon: IconAll,
}

pub fn default_modes() -> Vec<ModeEntry> {
    PERMISSION_MODES
        .iter()
        .map(|(name, desc, icon, id)| ModeEntry {
            id: (*id).to_string(),
            name: (*name).into(),
            desc: (*desc).into(),
            icon: *icon,
        })
        .collect()
}

pub fn mode_icon(id: &str) -> IconAll {
    match id {
        "default" => IconAll::ShieldCheck,
        "acceptEdits" => IconAll::SquarePen,
        "plan" => IconAll::ListTodo,
        "auto" => IconAll::Zap,
        "bypassPermissions" => IconAll::ShieldOff,
        _ => IconAll::Brain,
    }
}

/// 把 ACP 的工具标题拆成「名称 + 参数」：`Read /a/b` → (Read, /a/b)，
/// `` `ls -la` `` → (Bash, ls -la)，其余 → (Tool, 原标题)。
pub fn split_tool_title(title: &str) -> (SharedString, SharedString) {
    let t = title.trim();
    let is_name = |w: &str| {
        w.chars().next().is_some_and(|c| c.is_ascii_uppercase())
            && w.chars().all(|c| c.is_ascii_alphanumeric())
    };
    if t.starts_with('`') {
        return ("Bash".into(), t.trim_matches('`').to_string().into());
    }
    // MCP 工具：`服务器:工具 {参数}`（自研内核的标题）
    let is_mcp = |w: &str| {
        w.split_once(':').is_some_and(|(server, tool)| {
            let ok = |s: &str| {
                !s.is_empty()
                    && s.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
            };
            ok(server) && ok(tool)
        })
    };
    if let Some((head, rest)) = t.split_once(' ')
        && (is_name(head) || is_mcp(head))
    {
        return (head.to_string().into(), rest.trim().to_string().into());
    }
    if is_name(t) {
        return (t.to_string().into(), SharedString::default());
    }
    ("Tool".into(), t.to_string().into())
}

/// 工具行的「名称 + 参数」：标题自带工具名（`Read /a/b`）就直接拆；
/// 否则（如 pi：标题是命令 `ls -la` 或小写工具名 `read`）按工具种类命名。
pub fn tool_label(kind: &str, title: &str) -> (SharedString, SharedString) {
    let (name, arg) = split_tool_title(title);
    if name.as_ref() != "Tool" {
        return (name, arg);
    }
    let by_kind = match kind {
        "execute" => "Bash",
        "read" => "Read",
        "edit" => "Edit",
        "delete" => "Delete",
        "move" => "Move",
        "search" => "Search",
        "fetch" => "Fetch",
        "think" => "Think",
        _ => return (name, arg),
    };
    let t = title.trim();
    let arg = if t.eq_ignore_ascii_case(by_kind) || t.eq_ignore_ascii_case(kind) {
        SharedString::default()
    } else {
        t.to_string().into()
    };
    (by_kind.into(), arg)
}

pub fn tool_phase(status: &str) -> ToolPhase {
    match status {
        "completed" => ToolPhase::Done,
        "failed" => ToolPhase::Failed,
        _ => ToolPhase::Running,
    }
}

/// 工具原始输出 → 一行摘要（去引号/换行，截断）。
pub fn summarize_output(raw: &str) -> String {
    let t = raw.trim().trim_matches('"').replace("\\n", " ").replace("\\\"", "\"");
    let t = t.split_whitespace().collect::<Vec<_>>().join(" ");
    if t.chars().count() > 140 {
        format!("{}…", t.chars().take(140).collect::<String>())
    } else {
        t
    }
}

pub use claudesktop_core::is_context_overflow;

/// 一轮失败的原因（按报错文字认，各内核、网关说法不一，认不出的归 `Other`），
/// 决定失败卡片的标题、建议和按钮。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum FailureKind {
    /// 超出模型的上下文长度
    Overflow,
    /// 401 / 403：API Key 不对或没有权限
    Auth,
    /// ACP agent 要先登录（Claude Code 没登录时报「Authentication required」）
    Login,
    /// 429：太频繁或额度用完
    RateLimit,
    /// 5xx：服务端的问题（自研内核已经退避重试过）
    Server,
    /// 404 且说的是模型：模型名写错了，或者这个 Key 用不了它
    Model,
    /// 端点回了 400，说的是「这个模型不支持图片」（第七章 7.3：翻译成中文并记住这个模型）
    NoVision,
    /// 其余的 404：多半是端点地址不对（少了 `/v1` 这类）
    Endpoint,
    /// 请求没发出去：连不上、超时、证书
    Network,
    /// 回复到一半流断了
    Interrupted,
    /// 关应用时这一轮还在进行（重放会话时补上，不从报错文字里认）
    Unfinished,
    Other,
}

impl FailureKind {
    pub fn of(message: &str) -> Self {
        // 「这个模型看不了图片」：先把这一条认出来——它的文案里常有 multimodal 之类的词，
        // 别的判断（如「请求本身太长」）容易被带偏
        if claudesktop_core::image_prep::image_rejection(message) {
            return Self::NoVision;
        }
        // 先看状态码：5xx / 429 的响应体里也可能捎带 "context length" 字样，
        // 那会儿真正该修的是服务端或额度，不是「新开会话」
        if let Some(status) = http_status(message) {
            return match status {
                401 | 403 => Self::Auth,
                429 => Self::RateLimit,
                404 if message.to_lowercase().contains("model") => Self::Model,
                404 => Self::Endpoint,
                500..=599 => Self::Server,
                // 4xx 里剩下的才可能是「请求本身太长」
                _ if is_context_overflow(message) => Self::Overflow,
                _ => Self::Other,
            };
        }
        if is_context_overflow(message) {
            return Self::Overflow;
        }
        let lower = message.to_lowercase();
        if ["authentication required", "/login", "not logged in", "please log in", "please login"]
            .iter()
            .any(|m| lower.contains(m))
        {
            return Self::Login;
        }
        // 自研内核的两种传输错误（见 builtin 的 provider.rs）
        if message.starts_with("读取流失败") {
            Self::Interrupted
        } else if message.starts_with("请求 ") && message.contains(" 失败: ") {
            Self::Network
        } else {
            Self::Other
        }
    }

    /// 要到设置里改服务商配置才能好（失败卡片上给「打开设置」）。
    pub fn settings_fix(self) -> bool {
        matches!(self, Self::Auth | Self::Model | Self::Endpoint | Self::Network)
    }

    pub fn title(self) -> &'static str {
        match self {
            Self::Overflow => "超出了模型的上下文长度",
            Self::Auth => "模型服务拒绝了请求",
            Self::Login => "agent 还没登录",
            Self::RateLimit => "请求太频繁或额度用完了",
            Self::Server => "模型服务出错了",
            Self::Model => "找不到这个模型",
            Self::NoVision => "这个模型看不了图片",
            Self::Endpoint => "找不到模型服务的接口",
            Self::Network => "连不上模型服务",
            Self::Interrupted => "回复中途断开了",
            Self::Unfinished => "上次关闭时这一轮还没做完",
            Self::Other => "本轮失败",
        }
    }

    /// 标题下面一句该怎么办；`Other` 没有（只看原文）。
    pub fn hint(self) -> Option<&'static str> {
        Some(match self {
            Self::Overflow => {
                "这个会话已经长到模型装不下了。新开一个会话接着做，或者换一个上下文更大的模型再重试。"
            }
            Self::Auth => "API Key 不对、过期了，或者没有这个模型的权限。到设置里检查服务商配置。",
            Self::RateLimit => "等一会儿再试，或者到服务商那边看看额度。",
            Self::Login => {
                "agent 要先登录才能用。Claude Code：在终端里运行 claude，按提示登录一次（或者设好 ANTHROPIC_API_KEY 环境变量），然后回来点重试。"
            }
            // 自研内核会自己退避重试几次，ACP 内核的重试策略由 agent 决定，这里不断言
            Self::Server => "是服务端的问题，不是这个会话的错。稍后再试通常就好。",
            Self::Model => {
                "模型名写错了，或者这个 Key 用不了它。点输入框右下角的模型名换一个，再点重试。"
            }
            Self::NoVision => {
                "点输入框右下角的模型名换一个能看图的模型重发；这个模型已经记进设置，下次直接按「看不了图片」处理。"
            }
            Self::Endpoint => {
                "端点地址多半不对（很多服务的地址以 /v1 结尾）。到设置里检查服务商的端点地址。"
            }
            Self::Network => "检查网络、代理，以及设置里的端点地址。",
            Self::Interrupted => "连接在输出途中断了，已经做完的步骤都还在。",
            Self::Unfinished => "已经做完的步骤都还在，点「继续」接着做。",
            Self::Other => return None,
        })
    }
}

/// 设置页「获取列表」失败时给人看的话：说清是哪个字段的问题、怎么改，原文另行显示。
pub fn fetch_failure_text(message: &str, base_url: &str) -> String {
    let base = base_url.trim().trim_end_matches('/');
    match FailureKind::of(message) {
        FailureKind::Auth => "API Key 不对，或者没有权限。检查 Key 有没有复制全、是不是这家服务商的".into(),
        FailureKind::Endpoint | FailureKind::Model if !base.ends_with("/v1") => {
            format!("端点地址不对：这个地址下没有模型列表。很多服务以 /v1 结尾，试试 {base}/v1")
        }
        FailureKind::Endpoint | FailureKind::Model => {
            "端点地址下没有模型列表。确认地址是服务商文档里的 base URL；有的服务不提供列表，那就直接填模型名"
                .into()
        }
        FailureKind::Network => "连不上这个地址。检查端点地址、网络和代理".into(),
        FailureKind::RateLimit => "请求太频繁或额度用完了，稍后再试".into(),
        FailureKind::Server => "服务商那边出错了，稍后再试".into(),
        _ => "获取失败".into(),
    }
}

/// 内核起不来时给人看的话。找不到命令单独说：`npx` / `node` 多半是没装 Node.js。
pub fn kernel_failure_text(label: &str, command: &str, message: &str) -> String {
    // 「ACP agent」后面接中文要隔一个空格，「自研内核」不用
    let label = if label.ends_with(|c: char| c.is_ascii_alphanumeric()) {
        format!("{label} ")
    } else {
        label.to_string()
    };
    let missing =
        ["os error 2", "not found", "No such file", "不是内部或外部命令", "is not recognized"]
            .iter()
            .any(|m| message.contains(m));
    // Windows 上的命令是 `cmd /c npx …`，真正要找的程序在后面
    let program = command
        .split_whitespace()
        .find(|w| !w.eq_ignore_ascii_case("cmd") && !w.eq_ignore_ascii_case("/c"))
        .unwrap_or_default();
    match (missing, program) {
        (true, "npx" | "node" | "npm") => format!(
            "{label}启动不了：找不到 {program}。先装 Node.js（nodejs.org），装好后再发一次消息；或者到设置里换一种接法"
        ),
        (true, _) if !program.is_empty() => {
            format!("{label}启动不了：找不到命令 {program}。到设置里检查启动命令")
        }
        _ => format!("{label}出错了：{message}（可在设置里修改）"),
    }
}

/// 报错里的 HTTP 状态码（「HTTP 429: …」）。
///
/// 按「独立成词」找 `HTTP`：`error_message` 会把服务端 body 原样带进来，里面引用的
/// 地址（`https://x/HTTP 404`）或状态码片段都可能误伤，所以要求 `HTTP` 前面不是字母 /
/// 数字 / `/`、后面正好跟三位状态码，找不到再往后接着找。
fn http_status(message: &str) -> Option<u16> {
    const TAG: &str = "HTTP ";
    let mut from = 0;
    while let Some(found) = message[from..].find(TAG) {
        let at = from + found;
        from = at + TAG.len();
        let boundary = message[..at].chars().next_back().is_none_or(|c| {
            !(c.is_ascii_alphanumeric() || c == '/' || c == ':' || c == '_' || c == '-')
        });
        if !boundary {
            continue;
        }
        let digits = message.get(from..from + 3)?;
        let rest = message[from + 3..].chars().next();
        // 后面不能再跟数字（「HTTP 4001」不是状态码），否则往后接着找
        if rest.is_some_and(|c| c.is_ascii_digit()) {
            continue;
        }
        if let Ok(status) = digits.parse::<u16>()
            && (100..600).contains(&status)
        {
            return Some(status);
        }
    }
    None
}

/// 首条消息 → 会话标题：换行和连续空白并成一个空格（标题只占一行），取前 18 个字符。
pub fn title_from_prompt(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ").chars().take(18).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[core::prelude::v1::test]
    fn title_is_one_line() {
        assert_eq!(title_from_prompt("  修一下\n\n  这个报错 "), "修一下 这个报错");
        assert_eq!(title_from_prompt(&"字".repeat(30)).chars().count(), 18);
    }

    #[core::prelude::v1::test]
    fn queued_summary_is_one_line() {
        let q =
            QueuedPrompt {
                text: "先别改\n  测试\n".into(), images: Vec::new(), files: Vec::new()
            };
        assert_eq!(q.summary(), "先别改 测试");
        assert_eq!(q.into_prompt(), UserPrompt::text("先别改\n  测试\n"));
    }

    #[core::prelude::v1::test]
    fn tool_title_split() {
        let (n, a) = split_tool_title("Read /a/b.rs");
        assert_eq!((n.as_ref(), a.as_ref()), ("Read", "/a/b.rs"));
        let (n, a) = split_tool_title("`cargo build -p x`");
        assert_eq!((n.as_ref(), a.as_ref()), ("Bash", "cargo build -p x"));
        let (n, a) = split_tool_title("Edit");
        assert_eq!((n.as_ref(), a.as_ref()), ("Edit", ""));
        let (n, a) = split_tool_title(r#"github:search_issues {"q":"bug"}"#);
        assert_eq!((n.as_ref(), a.as_ref()), ("github:search_issues", r#"{"q":"bug"}"#));
        let (n, a) = split_tool_title("读取 file.mjs");
        assert_eq!((n.as_ref(), a.as_ref()), ("Tool", "读取 file.mjs"));
    }

    #[core::prelude::v1::test]
    fn tool_label_by_kind() {
        let (n, a) = tool_label("execute", "ls -la");
        assert_eq!((n.as_ref(), a.as_ref()), ("Bash", "ls -la"));
        let (n, a) = tool_label("read", "read");
        assert_eq!((n.as_ref(), a.as_ref()), ("Read", ""));
        // 标题自带工具名时不被 kind 覆盖
        let (n, a) = tool_label("read", "Read /a/b.rs");
        assert_eq!((n.as_ref(), a.as_ref()), ("Read", "/a/b.rs"));
        let (n, a) = tool_label("other", "something");
        assert_eq!((n.as_ref(), a.as_ref()), ("Tool", "something"));
    }

    #[core::prelude::v1::test]
    fn output_summary() {
        assert_eq!(summarize_output("\"a\nb  c\""), "a b c");
        let long = "x".repeat(300);
        let out = summarize_output(&long);
        assert_eq!(out.chars().count(), 141);
        assert!(out.ends_with('…'));
    }

    #[core::prelude::v1::test]
    fn summary_view_keeps_results_and_pending_diffs() {
        let card = |phase, with_diff: bool| {
            ChatItem::Tool(ToolCard {
                id: None,
                kind: "edit".into(),
                name: "Edit".into(),
                arg: "a.rs".into(),
                result: "".into(),
                output: SharedString::default(),
                phase,
                diffs: if with_diff {
                    vec![DiffView {
                        path: "a.rs".into(),
                        created: false,
                        added: 1,
                        removed: 0,
                        lines: Vec::new(),
                        truncated: false,
                    }]
                } else {
                    Vec::new()
                },
                expanded: false,
                task: false,
                grant: None,
            })
        };
        let thinking = thought("t", None);
        let agent = ChatItem::Agent("a".into());
        for item in [&thinking, &agent, &card(ToolPhase::Done, true)] {
            assert!(item.visible_in(ViewMode::Normal));
            assert!(item.visible_in(ViewMode::Verbose));
        }
        assert!(!thinking.visible_in(ViewMode::Summary));
        assert!(agent.visible_in(ViewMode::Summary));
        // 摘要模式：只留正在执行、带 diff 的那一步
        assert!(card(ToolPhase::Running, true).visible_in(ViewMode::Summary));
        assert!(!card(ToolPhase::Running, false).visible_in(ViewMode::Summary));
        assert!(!card(ToolPhase::Done, true).visible_in(ViewMode::Summary));
    }

    fn tool(kind: &str, name: &str, arg: &str, phase: ToolPhase) -> ChatItem {
        ChatItem::Tool(ToolCard {
            id: None,
            kind: kind.into(),
            name: name.into(),
            arg: arg.into(),
            result: "".into(),
            output: SharedString::default(),
            phase,
            diffs: Vec::new(),
            expanded: false,
            task: false,
            grant: None,
        })
    }

    #[core::prelude::v1::test]
    fn failures_are_classified_by_message() {
        let cases = [
            ("HTTP 401: invalid api key", FailureKind::Auth),
            ("HTTP 429: rate limited", FailureKind::RateLimit),
            ("HTTP 529: overloaded", FailureKind::Server),
            ("HTTP 400: prompt is too long", FailureKind::Overflow),
            ("HTTP 404: no such model", FailureKind::Model),
            ("HTTP 404: The model `x` does not exist", FailureKind::Model),
            ("HTTP 404: Not Found: /chat/completions", FailureKind::Endpoint),
            // 5xx 的响应体里带上 context 字样时仍按服务端错误处理（先修的该是服务端）
            ("HTTP 500: upstream context length exceeded?", FailureKind::Server),
            ("HTTP 429: too many tokens per minute", FailureKind::RateLimit),
            ("请求 https://x/v1/chat 失败: Connection refused", FailureKind::Network),
            ("读取流失败: connection reset", FailureKind::Interrupted),
            ("agent 崩了", FailureKind::Other),
            ("Authentication required", FailureKind::Login),
            ("Invalid API key · Please run /login", FailureKind::Login),
        ];
        for (message, kind) in cases {
            assert_eq!(FailureKind::of(message), kind, "{message}");
        }
    }

    #[core::prelude::v1::test]
    fn fetch_failures_point_at_the_field() {
        let text = fetch_failure_text("HTTP 404: Not Found: /models", "http://127.0.0.1:8080/");
        assert!(text.contains("试试 http://127.0.0.1:8080/v1"), "{text}");
        let text = fetch_failure_text("HTTP 404: Not Found: /models", "https://x.com/v1");
        assert!(!text.contains("试试"), "{text}");
        assert!(fetch_failure_text("HTTP 401: bad key", "https://x.com/v1").contains("API Key"));
        let text = fetch_failure_text("请求 https://x/v1/models 失败: Connection refused", "");
        assert!(text.contains("连不上"), "{text}");
    }

    #[core::prelude::v1::test]
    fn kernel_failures_name_the_missing_program() {
        let text = kernel_failure_text(
            "ACP agent",
            "cmd /c npx -y @agentclientprotocol/claude-agent-acp",
            "spawn: No such file or directory (os error 2)",
        );
        assert!(text.contains("找不到 npx") && text.contains("Node.js"), "{text}");
        let text = kernel_failure_text("ACP agent", "my-agent --acp", "os error 2");
        assert!(text.contains("找不到命令 my-agent"), "{text}");
        let text = kernel_failure_text("ACP agent", "npx x", "agent exited: 1");
        assert!(text.contains("出错了：agent exited: 1"), "{text}");
    }

    #[core::prelude::v1::test]
    fn http_status_ignores_lookalikes() {
        assert_eq!(http_status("HTTP 429: rate limited"), Some(429));
        assert_eq!(http_status("upstream said HTTP 503: busy"), Some(503));
        // 引用的地址里带 HTTP 字样不算（前面的 `/` 说明它是路径的一部分）
        assert_eq!(http_status("请求 https://x/HTTP 404 失败"), None);
        // 跳过这个之后再找到真正的状态码
        assert_eq!(
            http_status("见 https://x/HTTP 404 ，本次失败: HTTP 502 Bad Gateway"),
            Some(502)
        );
        assert_eq!(http_status("GET /a HTTP 4001"), None);
        assert_eq!(http_status("HTTP X00"), None);
        assert_eq!(http_status("no status here"), None);
    }

    #[core::prelude::v1::test]
    fn turn_files_merge_edits_and_skip_failures() {
        let edit = |path: &str, added, removed, phase| {
            let mut item = tool("edit", "Edit", path, phase);
            if let ChatItem::Tool(card) = &mut item {
                card.diffs.push(DiffView {
                    path: path.into(),
                    created: added == 9,
                    added,
                    removed,
                    lines: Vec::new(),
                    truncated: false,
                });
            }
            item
        };
        let items = [
            edit("a.rs", 2, 1, ToolPhase::Done),
            edit("new.rs", 9, 0, ToolPhase::Done),
            ChatItem::Agent("改好了".into()),
            edit("a.rs", 3, 0, ToolPhase::Done),
            edit("b.rs", 5, 5, ToolPhase::Failed),
        ];
        let files = turn_files(&items, 10);
        let got: Vec<_> = files
            .iter()
            .map(|f| (f.path.as_str(), f.added, f.removed, f.created, f.item))
            .collect();
        assert_eq!(got, [("a.rs", 5, 1, false, 13), ("new.rs", 9, 0, true, 11)]);
    }

    /// 新建之后又整份改写过：算普通修改（`created` 只看最后一次）。
    #[core::prelude::v1::test]
    fn turn_files_created_follows_the_last_edit() {
        let edit = |created: bool, added| {
            let mut item = tool("edit", "Edit", "a.rs", ToolPhase::Done);
            if let ChatItem::Tool(card) = &mut item {
                card.diffs.push(DiffView {
                    path: "a.rs".into(),
                    created,
                    added,
                    removed: 0,
                    lines: Vec::new(),
                    truncated: false,
                });
            }
            item
        };
        let items = [edit(true, 9), edit(false, 2)];
        let files = turn_files(&items, 0);
        assert_eq!((files[0].added, files[0].created), (11, false));
    }

    /// 同一个文件的两条路径大小写 / 分隔符不一样（Windows 上工具回报的不保证统一）要算一个文件。
    #[core::prelude::v1::test]
    fn turn_files_merge_paths_ignoring_case_and_separators() {
        let edit = |path: &str, added| {
            let mut item = tool("edit", "Edit", path, ToolPhase::Done);
            if let ChatItem::Tool(card) = &mut item {
                card.diffs.push(DiffView {
                    path: path.into(),
                    created: false,
                    added,
                    removed: 0,
                    lines: Vec::new(),
                    truncated: false,
                });
            }
            item
        };
        let items = [
            edit(r"C:\Proj\src\Main.rs", 2),
            edit("c:/proj/src/main.rs", 3),
            edit(r"C:\Proj\SRC\main.rs", 1),
            edit(r"C:\Proj\src\other.rs", 7),
        ];
        let files = turn_files(&items, 0);
        let got: Vec<_> = files.iter().map(|f| (f.path.as_str(), f.added)).collect();
        // 保留第一次出现的写法，行数合起来
        assert_eq!(got, [(r"C:\Proj\src\Main.rs", 6), (r"C:\Proj\src\other.rs", 7)]);
    }

    #[core::prelude::v1::test]
    fn group_summary_counts_by_kind() {
        let items = [
            tool("read", "Read", "a.rs", ToolPhase::Done),
            tool("read", "Read", "a.rs", ToolPhase::Done),
            tool("read", "Read", "b.rs", ToolPhase::Done),
            // 演示脚本没有 kind：按工具名猜
            tool("", "Grep", "x", ToolPhase::Done),
            tool("execute", "Bash", "cargo test", ToolPhase::Failed),
            thought("想", None),
        ];
        let (text, failed) = group_summary(&items);
        assert_eq!(text, "读取 2 个文件，搜索 1 次，运行 1 条命令");
        assert_eq!(failed, 1);
        let only_thinking = [thought("想", None)];
        assert_eq!(group_summary(&only_thinking).0, "已思考");
        // 知道用时就报出来；多段时报总用时，有一段不知道就不报
        assert_eq!(group_summary(&[thought("想", Some(12_300))]).0, "已思考 12 秒");
        let timed = [thought("想", Some(30_000)), thought("再想", Some(45_000))];
        assert_eq!(group_summary(&timed).0, "思考 2 次，共 1 分 15 秒");
        let partly = [thought("想", Some(30_000)), thought("再想", None)];
        assert_eq!(group_summary(&partly).0, "已思考 2 次");
    }

    /// 已收尾的一段思考。
    fn thought(text: &str, elapsed_ms: Option<u64>) -> ChatItem {
        ChatItem::Thinking {
            text: text.into(),
            streaming: false,
            open: false,
            started: None,
            elapsed_ms,
        }
    }

    #[core::prelude::v1::test]
    fn thought_label_rounds_to_seconds() {
        assert_eq!(thought_label(None), "已思考");
        assert_eq!(thought_label(Some(300)), "已思考 1 秒");
        assert_eq!(thought_label(Some(12_499)), "已思考 12 秒");
        assert_eq!(thought_label(Some(12_500)), "已思考 13 秒");
        assert_eq!(thought_label(Some(120_000)), "已思考 2 分钟");
        assert_eq!(thought_label(Some(65_000)), "已思考 1 分 5 秒");
    }

    #[core::prelude::v1::test]
    fn steps_exclude_diffs_and_empty_thinking() {
        assert!(tool("read", "Read", "a", ToolPhase::Done).is_step());
        let mut edit = tool("edit", "Edit", "a", ToolPhase::Done);
        if let ChatItem::Tool(card) = &mut edit {
            card.set_diff(DiffView::compute("a", Some("x\n"), "y\n"));
        }
        assert!(!edit.is_step());
        assert!(!ChatItem::Agent("a".into()).is_step());
        let blank = thought("\n\n", None);
        assert!(blank.is_step() && !blank.has_content());
        let streaming = ChatItem::thinking(String::new(), None);
        assert!(streaming.has_content());
    }

    #[core::prelude::v1::test]
    fn plan_status_and_progress() {
        let rows: Vec<PlanRow> = ["completed", "in_progress", "pending"]
            .iter()
            .enumerate()
            .map(|(i, s)| PlanRow {
                text: format!("第{i}步").into(), status: PlanStatus::parse(s)
            })
            .collect();
        let (done, total, current) = plan_progress(&rows);
        assert_eq!((done, total), (1, 3));
        assert_eq!(current.unwrap().text.as_ref(), "第1步");
    }

    #[core::prelude::v1::test]
    fn view_mode_cycles_through_all() {
        let mut v = ViewMode::default();
        assert_eq!(v, ViewMode::Normal);
        for _ in 0..ViewMode::ALL.len() {
            v = v.next();
        }
        assert_eq!(v, ViewMode::Normal);
    }

    #[core::prelude::v1::test]
    fn tool_phase_mapping() {
        assert!(tool_phase("completed") == ToolPhase::Done);
        assert!(tool_phase("failed") == ToolPhase::Failed);
        assert!(tool_phase("in_progress") == ToolPhase::Running);
    }
}
