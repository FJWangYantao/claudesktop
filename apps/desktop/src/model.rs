//! 对话记录的数据模型、模式表与工具行辅助函数（UI 无关逻辑）。

use claudesktop_core::{DiffView, PermissionOptionView};
use gpui_kit::assets::IconName as IconAll;
use gpui_kit::SharedString;

#[derive(Clone, Copy, PartialEq, Hash)]
pub enum ToolPhase {
    Running,
    Done,
    Failed,
}

/// 对话的视图模式（对照官方 Summary / Normal / Verbose）：只决定看多少过程，不影响内核怎么跑。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ViewMode {
    /// 只看结果：隐藏思考、启动信息、计划和工具调用（正在等你看 diff 的那一步除外）
    Summary,
    /// 工具调用折叠成一行，思考默认收起
    #[default]
    Normal,
    /// 全部展开：思考、启动信息、完整 diff
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
            ViewMode::Normal => "工具调用折叠成一行，思考默认收起",
            ViewMode::Verbose => "展开思考、启动信息和完整 diff",
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

/// 工具调用：`● Name arg` + `└ result`（Claude Code 的紧凑样式）
pub struct ToolCard {
    /// 内核侧的 tool_call_id（演示脚本为 None）
    pub id: Option<String>,
    /// 工具种类（read / execute / …），标题里没有工具名时据此命名
    pub kind: String,
    pub name: SharedString,
    pub arg: SharedString,
    pub result: SharedString,
    pub phase: ToolPhase,
    /// 该调用改动的文件（同一路径只保留最新一份）
    pub diffs: Vec<DiffView>,
    /// diff 已展开全部行（默认只显示前 [`DIFF_PREVIEW_ROWS`] 行）
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
}

pub struct PlanRow {
    pub text: SharedString,
    pub done: bool,
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
    },
    /// agent 在首条提示之前推送的启动信息（如 pi 的 Skills / Extensions 列表），默认折叠
    Info {
        text: String,
        open: bool,
    },
    Tool(ToolCard),
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

impl ChatItem {
    /// 这一行在某个视图模式下是否显示。摘要模式只留回复、用户消息、权限卡和备注；
    /// 工具调用只在「执行中且带 diff」时留着——那是正等你对着改动决定放不放行的一步。
    pub fn visible_in(&self, view: ViewMode) -> bool {
        match (view, self) {
            (ViewMode::Summary, Self::Thinking { .. } | Self::Info { .. } | Self::Plan(_)) => false,
            (ViewMode::Summary, Self::Tool(c)) => c.phase == ToolPhase::Running && !c.diffs.is_empty(),
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
            Self::Thinking {
                text,
                streaming,
                open,
            } => (1u8, text.len(), streaming, open).hash(&mut h),
            Self::Info { text, open } => (2u8, text.len(), open).hash(&mut h),
            Self::Tool(c) => (
                3u8,
                c.name.len(),
                c.arg.len(),
                c.result.len(),
                c.phase,
                c.expanded,
                c.diffs.iter().map(|d| d.lines.len() + 1).sum::<usize>(),
            )
                .hash(&mut h),
            Self::Permission {
                title,
                options,
                resolved,
                ..
            } => (4u8, title.len(), options.len(), resolved.as_ref().map(|r| r.len())).hash(&mut h),
            Self::Plan(rows) => (
                5u8,
                rows.len(),
                rows.iter().map(|r| r.text.len()).sum::<usize>(),
                rows.iter().filter(|r| r.done).count(),
            )
                .hash(&mut h),
            Self::Agent(t) => (6u8, t.len()).hash(&mut h),
            Self::Note(t) => (7u8, t.len()).hash(&mut h),
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
    if let Some((head, rest)) = t.split_once(' ') {
        if is_name(head) {
            return (head.to_string().into(), rest.trim().to_string().into());
        }
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

#[cfg(test)]
mod tests {
    use super::*;

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
        let thinking = ChatItem::Thinking { text: "t".into(), streaming: false, open: false };
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
