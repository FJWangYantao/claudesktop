//! 文件改动的紧凑 diff：内核侧把「旧文本 / 新文本」算成带上下文的若干 hunk，
//! 事件和会话日志里只存这些行，不存整个文件。

use serde::{Deserialize, Serialize};
use similar::{ChangeTag, TextDiff};

/// 每个 hunk 前后保留的上下文行数
const CONTEXT: usize = 3;
/// 最多保留的行数（含上下文与 hunk 分隔），超出标记 `truncated`
const MAX_LINES: usize = 400;
/// 单行最多保留的字符数
const LINE_MAX_CHARS: usize = 400;

/// diff 的一行。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct DiffLineView {
    /// `' '` 上下文 / `'+'` 新增 / `'-'` 删除 / `'~'` 两个 hunk 之间省略的部分
    pub tag: char,
    /// 行号：删除行是旧文件行号，其余是新文件行号；`'~'` 为 0
    pub no: u32,
    pub text: String,
}

/// 一个文件的改动。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct DiffView {
    /// 显示用路径（内核决定相对 / 绝对）
    pub path: String,
    /// 新建文件（没有旧内容）
    pub created: bool,
    pub added: u32,
    pub removed: u32,
    pub lines: Vec<DiffLineView>,
    /// 行数超过上限被截断
    pub truncated: bool,
}

impl DiffView {
    /// 由旧 / 新全文算出 diff；`old = None` 表示新建文件。
    pub fn compute(path: impl Into<String>, old: Option<&str>, new: &str) -> Self {
        let created = old.is_none();
        let old = old.unwrap_or("");
        let diff = TextDiff::from_lines(old, new);
        let (mut added, mut removed) = (0u32, 0u32);
        let mut lines = Vec::new();
        let mut truncated = false;
        for (i, group) in diff.grouped_ops(CONTEXT).iter().enumerate() {
            if i > 0 {
                lines.push(DiffLineView { tag: '~', no: 0, text: String::new() });
            }
            for op in group {
                for change in diff.iter_changes(op) {
                    let (tag, no) = match change.tag() {
                        ChangeTag::Insert => {
                            added += 1;
                            ('+', change.new_index())
                        }
                        ChangeTag::Delete => {
                            removed += 1;
                            ('-', change.old_index())
                        }
                        ChangeTag::Equal => (' ', change.new_index()),
                    };
                    if lines.len() >= MAX_LINES {
                        truncated = true;
                        continue;
                    }
                    let text = change.value().trim_end_matches(['\n', '\r']);
                    lines.push(DiffLineView {
                        tag,
                        no: no.map_or(0, |n| n as u32 + 1),
                        text: clip(text),
                    });
                }
            }
        }
        Self { path: path.into(), created, added, removed, lines, truncated }
    }
}

pub(crate) fn clip(s: &str) -> String {
    // tab 展开成 4 个空格，UI 里对齐更稳定
    let s = s.replace('\t', "    ");
    if s.chars().count() <= LINE_MAX_CHARS {
        s
    } else {
        format!("{}…", s.chars().take(LINE_MAX_CHARS).collect::<String>())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rows(d: &DiffView) -> Vec<String> {
        d.lines.iter().map(|l| format!("{}{} {}", l.tag, l.no, l.text)).collect()
    }

    #[test]
    fn hunks_with_context_and_gap() {
        let old: String = (1..=20).map(|i| format!("l{i}\n")).collect();
        let new = old.replace("l2\n", "L2\n").replace("l18\n", "l18\nX\n");
        let d = DiffView::compute("a.txt", Some(&old), &new);
        assert_eq!((d.added, d.removed, d.created, d.truncated), (2, 1, false, false));
        assert_eq!(
            rows(&d),
            [
                " 1 l1", "-2 l2", "+2 L2", " 3 l3", " 4 l4", " 5 l5", "~0 ", " 16 l16", " 17 l17",
                " 18 l18", "+19 X", " 20 l19", " 21 l20",
            ]
        );
    }

    #[test]
    fn new_file_and_crlf() {
        let d = DiffView::compute("n.txt", None, "a\r\nb\r\n");
        assert!(d.created);
        assert_eq!(rows(&d), ["+1 a", "+2 b"]);
    }

    #[test]
    fn truncates_long_diffs() {
        let new: String = (0..1000).map(|i| format!("{i}\n")).collect();
        let d = DiffView::compute("big", None, &new);
        assert_eq!(d.added, 1000);
        assert_eq!(d.lines.len(), MAX_LINES);
        assert!(d.truncated);
    }
}
