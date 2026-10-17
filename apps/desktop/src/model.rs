//! 对话记录的数据模型、模式表与工具行辅助函数（UI 无关逻辑）。

use claudesktop_core::{DiffView, PermissionOptionView, PromptImage, UserPrompt};
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
}

/// 折叠状态下每个 diff 最多显示的行数
pub const DIFF_PREVIEW_ROWS: usize = 40;

impl ToolCard {
    /// 记录一份 diff：同一路径替换（执行前的预览会被之后的结果覆盖）。
    pub fn set_diff(&mut self, diff: DiffView) {
        match self.diffs.iter_mut().find(|d| d.path == diff.path) {
            Some(slot) => *slot = diff,
            None => self.diffs.push(diff),
        }
    }

    /// 工具种类（read / search / execute / …）；内核没给（演示脚本）时按工具名猜。
    pub fn kind_key(&self) -> &str {
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

/// 过程组的一行摘要（「读取 5 个文件，搜索 3 次，运行 2 条命令」）和失败的步数。
/// 读文件按路径去重；只有思考时说「已思考」。
pub fn group_summary<'a>(items: impl IntoIterator<Item = &'a ChatItem>) -> (String, usize) {
    let mut read_args: Vec<&str> = Vec::new();
    let (mut search, mut exec, mut edit, mut fetch, mut other, mut auto, mut think, mut failed) =
        (0, 0, 0, 0, 0, 0, 0, 0);
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
    },
    Plan(Vec<PlanRow>),
    Agent(String),
    Note(SharedString),
    /// 本轮失败（发送失败或内核报错）：卡片写明原因，带「重试」；
    /// `overflow` = 超出模型的上下文长度，另给「新开会话」
    Failed {
        message: SharedString,
        overflow: bool,
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
pub struct PendingImage {
    /// 原始文件名（悬停提示用）
    pub name: SharedString,
    /// 如 `image/png`
    pub media_type: String,
    /// 图片字节（base64 编码；发送与落盘共用）
    pub data: String,
    /// 输入框上方的预览
    pub preview: gpui_kit::Image,
}

impl PendingImage {
    /// 从图片字节构造（格式已知，如剪贴板里的 gpui Image）。
    pub fn from_bytes(name: SharedString, bytes: Vec<u8>, format: gpui_kit::ImageFormat) -> Self {
        use base64::Engine as _;
        Self {
            name,
            media_type: format.mime_type().to_string(),
            data: base64::engine::general_purpose::STANDARD.encode(&bytes),
            preview: gpui_kit::Image::from_bytes(format, bytes),
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
}

impl QueuedPrompt {
    pub fn into_prompt(self) -> UserPrompt {
        let images = self
            .images
            .into_iter()
            .map(|p| PromptImage { media_type: p.media_type, data: p.data })
            .collect();
        UserPrompt { text: self.text, images }
    }

    /// 队列里一行显示的文字：换行压成空格；只有图片时写「N 张图片」。
    pub fn summary(&self) -> String {
        let text = self.text.split_whitespace().collect::<Vec<_>>().join(" ");
        match (text.is_empty(), self.images.len()) {
            (true, n) => format!("{n} 张图片"),
            (false, 0) => text,
            (false, n) => format!("{text}（附 {n} 张图片）"),
        }
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

    /// 这一行在某个视图模式下是否显示。摘要模式只留回复、用户消息、权限卡和备注；
    /// 工具调用只在「执行中且带 diff」时留着——那是正等你对着改动决定放不放行的一步。
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
                c.phase == ToolPhase::Running && !c.diffs.is_empty()
            }
            _ => true,
        }
    }

    /// 过程中的一步：连续的几步合成一个可折叠的组（见 `Session::layout`）。
    /// 带 diff 的工具不算——改了什么要直接摆出来，不收进组里。
    pub fn is_step(&self) -> bool {
        match self {
            Self::Thinking { .. } | Self::AutoApproved { .. } => true,
            Self::Tool(c) => c.diffs.is_empty(),
            _ => false,
        }
    }

    /// 作为一步有没有东西可画：空的思考块（只有换行）不画、不计数。
    pub fn has_content(&self) -> bool {
        match self {
            Self::Thinking { text, streaming, .. } => *streaming || !text.trim().is_empty(),
            _ => true,
        }
    }

    /// 影响这一行渲染高度的内容摘要（虚拟列表据此判断要不要重新测量）。
    /// 内容只会追加或切换状态，所以长度 + 状态位就够了。
    pub fn fingerprint(&self) -> u64 {
        use std::hash::{Hash, Hasher};
        let mut h = std::collections::hash_map::DefaultHasher::new();
        match self {
            Self::User { text, images } => (0u8, text.len(), images.len()).hash(&mut h),
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
            )
                .hash(&mut h),
            Self::Permission { title, options, resolved, .. } => {
                (4u8, title.len(), options.len(), resolved.as_ref().map(|r| r.len())).hash(&mut h)
            }
            Self::Plan(rows) => {
                5u8.hash(&mut h);
                for r in rows {
                    (r.text.len(), r.status).hash(&mut h);
                }
            }
            Self::Agent(t) => (6u8, t.len()).hash(&mut h),
            Self::Note(t) => (7u8, t.len()).hash(&mut h),
            Self::Failed { message, overflow } => (9u8, message.len(), overflow).hash(&mut h),
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

/// 权限模式（对应官方桌面版的 Manual / Accept edits / Plan / Auto）：(名称, 说明, 图标, ACP mode id)
pub const PERMISSION_MODES: [(&str, &str, IconAll, &str); 4] = [
    ("逐项确认", "每次编辑和命令都先询问", IconAll::ShieldCheck, "default"),
    ("自动接受编辑", "自动接受文件编辑，命令仍询问", IconAll::SquarePen, "acceptEdits"),
    ("计划模式", "只出计划，不改动文件", IconAll::ListTodo, "plan"),
    ("自动", "由分类器判断，少打扰", IconAll::Zap, "auto"),
];

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
    if let Some((head, rest)) = t.split_once(' ')
        && is_name(head)
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
        let q = QueuedPrompt { text: "先别改\n  测试\n".into(), images: Vec::new() };
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
        })
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
