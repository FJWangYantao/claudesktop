//! 对话记录的数据模型、模式表与工具行辅助函数（UI 无关逻辑）。

use claudesktop_core::PermissionOptionView;
use gpui_kit::assets::IconName as IconAll;
use gpui_kit::SharedString;

#[derive(Clone, Copy, PartialEq)]
pub enum ToolPhase {
    Running,
    Done,
    Failed,
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
}

/// diff 行：(' ' 上下文 / '+' 新增 / '-' 删除, 文本)
pub type DiffRows = &'static [(char, &'static str)];

pub struct PlanRow {
    pub text: SharedString,
    pub done: bool,
}

pub enum ChatItem {
    User(SharedString),
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
    Diff {
        path: &'static str,
        rows: DiffRows,
    },
    Plan(Vec<PlanRow>),
    Agent(String),
    Note(SharedString),
}

pub struct StatCard {
    pub label: &'static str,
    pub value: SharedString,
}

/// 权限模式（名称沿用官方桌面版）：(名称, 说明, 图标, ACP mode id)
pub const PERMISSION_MODES: [(&str, &str, IconAll, &str); 4] = [
    ("Manual", "每次编辑和命令都先询问", IconAll::ShieldCheck, "default"),
    ("Accept edits", "自动接受文件编辑，命令仍询问", IconAll::SquarePen, "acceptEdits"),
    ("Plan", "只出计划，不改动文件", IconAll::ListTodo, "plan"),
    ("Auto", "由分类器判断，少打扰", IconAll::Zap, "auto"),
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
    fn tool_phase_mapping() {
        assert!(tool_phase("completed") == ToolPhase::Done);
        assert!(tool_phase("failed") == ToolPhase::Failed);
        assert!(tool_phase("in_progress") == ToolPhase::Running);
    }
}
