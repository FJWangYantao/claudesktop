//! 文件改动的紧凑 diff：内核侧把「旧文本 / 新文本」算成带上下文的若干 hunk，
//! 事件和会话日志里只存这些行，不存整个文件。
//!
//! 除了给界面看的 [`DiffView`]，这里还有「按块」的两个纯函数：[`hunk_ranges`] 算改动块的范围
//! （逐块审查、按块部分同意共用同一份编号），[`splice_ranges`] 按块拼出只写了一部分的内容。

use serde::{Deserialize, Serialize};
use similar::{ChangeTag, DiffTag, TextDiff};

/// 每个 hunk 前后保留的上下文行数
pub(crate) const CONTEXT: usize = 3;
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

/// 一块改动在旧 / 新文件里的范围（从 0 数）。块的身份就是这个范围
/// （见「设计说明-逐块审查改动.md」4.1）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct HunkRange {
    pub old_start: u32,
    pub old_len: u32,
    pub new_start: u32,
    pub new_len: u32,
}

impl HunkRange {
    pub fn added(&self) -> u32 {
        self.new_len
    }

    pub fn removed(&self) -> u32 {
        self.old_len
    }
}

/// 一块改动，带显示用的行（前后各带几行上下文，和相邻块不重复）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DiffHunk {
    /// 块编号：就是 [`hunk_ranges`] 里的下标，权限卡上按它勾选「这一块不写」
    pub index: u32,
    pub range: HunkRange,
    pub lines: Vec<DiffLineView>,
}

impl DiffHunk {
    /// 这块加了几行（新文件里的行数）
    pub fn added(&self) -> u32 {
        self.range.added()
    }

    /// 这块删了几行（旧文件里的行数）
    pub fn removed(&self) -> u32 {
        self.range.removed()
    }
}

/// diff 的一段：相同的部分，或者一块改动。
pub(crate) enum Run {
    Equal { new_start: usize, len: usize },
    Change(HunkRange),
}

/// 按行比较，连续的增删合成一块（中间只要隔了一行相同的就是两块）。
pub(crate) fn runs(diff: &TextDiff<'_, '_, '_, str>) -> Vec<Run> {
    let mut out: Vec<Run> = Vec::new();
    for op in diff.ops() {
        let (tag, old, new) = op.as_tag_tuple();
        if tag == DiffTag::Equal {
            out.push(Run::Equal { new_start: new.start, len: new.len() });
            continue;
        }
        match out.last_mut() {
            Some(Run::Change(h)) => {
                h.old_len += old.len() as u32;
                h.new_len += new.len() as u32;
            }
            _ => out.push(Run::Change(HunkRange {
                old_start: old.start as u32,
                old_len: old.len() as u32,
                new_start: new.start as u32,
                new_len: new.len() as u32,
            })),
        }
    }
    out
}

/// `old` → `new` 的改动块范围，按位置排好；块的编号就是这里的下标。
pub fn hunk_ranges(old: &str, new: &str) -> Vec<HunkRange> {
    runs(&TextDiff::from_lines(old, new))
        .into_iter()
        .filter_map(|r| match r {
            Run::Change(h) => Some(h),
            Run::Equal { .. } => None,
        })
        .collect()
}

/// `old` → `new` 的改动块，带前后各 [`CONTEXT`] 行上下文（和相邻块不重复）。
pub fn hunks_with_context(old: &str, new: &str) -> Vec<DiffHunk> {
    let diff = TextDiff::from_lines(old, new);
    let (o, n) = (diff.old_slices(), diff.new_slices());
    let ranges: Vec<HunkRange> = runs(&diff)
        .into_iter()
        .filter_map(|r| match r {
            Run::Change(h) => Some(h),
            Run::Equal { .. } => None,
        })
        .collect();
    let mut out = Vec::with_capacity(ranges.len());
    let mut shown_to = 0; // 新文件里已经画到第几行
    for (i, range) in ranges.iter().enumerate() {
        let start = range.new_start as usize;
        let end = start + range.new_len as usize;
        let next = ranges.get(i + 1).map_or(n.len(), |x| x.new_start as usize);
        let mut lines = Vec::new();
        let ctx = |no: usize| line_view(' ', no, n[no]);
        lines.extend((start.saturating_sub(CONTEXT).max(shown_to)..start).map(ctx));
        let old_start = range.old_start as usize;
        lines.extend(
            (old_start..old_start + range.old_len as usize).map(|no| line_view('-', no, o[no])),
        );
        lines.extend((start..end).map(|no| line_view('+', no, n[no])));
        let after = (end + CONTEXT).min(next);
        lines.extend((end..after).map(ctx));
        shown_to = after;
        out.push(DiffHunk { index: i as u32, range: *range, lines });
    }
    out
}

fn line_view(tag: char, no: usize, text: &str) -> DiffLineView {
    DiffLineView { tag, no: no as u32 + 1, text: clip(text.trim_end_matches(['\n', '\r'])) }
}

/// 只把 `keep` 里列出的块写进结果，其余块保持旧内容：`keep = None` = 全部放行（返回 `new`）。
///
/// 拼的是**原始行**（含换行符），CRLF、末尾没换行、tab 都原样保留；`old = None` 表示新建文件。
/// `keep` 里有对不上的编号（块的数量变了）时，那些编号就当不存在——宁可少写，不错写。
pub fn splice_ranges(old: Option<&str>, new: &str, keep: Option<&[u32]>) -> String {
    let Some(keep) = keep else {
        return new.to_string();
    };
    let old = old.unwrap_or("");
    let diff = TextDiff::from_lines(old, new);
    let (o, n) = (diff.old_slices(), diff.new_slices());
    let mut out = String::with_capacity(new.len());
    let mut index = 0u32;
    for run in runs(&diff) {
        let slice: &[&str] = match run {
            Run::Equal { new_start, len } => &n[new_start..new_start + len],
            Run::Change(h) => {
                let wanted = keep.contains(&index);
                index += 1;
                let (start, len) = if wanted {
                    (h.new_start as usize, h.new_len as usize)
                } else {
                    (h.old_start as usize, h.old_len as usize)
                };
                let src = if wanted { n } else { o };
                &src[start..start + len]
            }
        };
        for line in slice {
            out.push_str(line);
        }
    }
    out
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
        let old: String = (1..=500).map(|i| format!("l{i}\n")).collect();
        let new: String = (1..=500).map(|i| format!("L{i}\n")).collect();
        let d = DiffView::compute("a.txt", Some(&old), &new);
        assert!(d.truncated);
        assert_eq!(d.lines.len(), MAX_LINES);
    }

    #[test]
    fn hunk_ranges_merge_adjacent_changes_only() {
        let old = "a\nb\nc\nd\ne\nf\n";
        // 第 0 块：删 b、改 c（相邻）；第 1 块：在 f 后面加一行
        let new = "a\nC\nd\ne\nf\ng\n";
        let ranges = hunk_ranges(old, new);
        assert_eq!(ranges.len(), 2, "{ranges:?}");
        assert_eq!((ranges[0].old_start, ranges[0].old_len), (1, 2));
        assert_eq!((ranges[1].old_start, ranges[1].old_len), (6, 0));
        let hunks = hunks_with_context(old, new);
        assert_eq!(hunks.len(), 2);
        assert_eq!(hunks[0].index, 0);
        assert_eq!(hunks[1].lines.last().unwrap().tag, '+');
    }

    #[test]
    fn splice_ranges_keeps_unselected_hunks_old() {
        let old = "a\nb\nc\nd\ne\nf\n";
        let new = "a\nC\nd\ne\nf\ng\n";
        // 全部放行 = new；一块都不放行 = old
        assert_eq!(splice_ranges(Some(old), new, None), new);
        assert_eq!(splice_ranges(Some(old), new, Some(&[])), old);
        assert_eq!(splice_ranges(Some(old), new, Some(&[0])), "a\nC\nd\ne\nf\n");
        assert_eq!(splice_ranges(Some(old), new, Some(&[1])), "a\nb\nc\nd\ne\nf\ng\n");
        assert_eq!(splice_ranges(Some(old), new, Some(&[0, 1])), new);
        // 对不上的编号当作不放行
        assert_eq!(splice_ranges(Some(old), new, Some(&[7])), old);
        // CRLF 和末尾没换行原样保留
        let old = "a\r\nb\r\nc\r\nd";
        let new = "a\r\nB\r\nc\r\nD";
        assert_eq!(splice_ranges(Some(old), new, Some(&[0])), "a\r\nB\r\nc\r\nd");
        assert_eq!(splice_ranges(Some(old), new, Some(&[])), old);
        // 中间隔了一行相同就是两块，各自保留
        assert_eq!(splice_ranges(Some(old), new, Some(&[1])), "a\r\nb\r\nc\r\nD");
        // 新建文件
        assert_eq!(splice_ranges(None, "x\ny\n", Some(&[])), "");
        assert_eq!(splice_ranges(None, "x\ny\n", None), "x\ny\n");
    }
}
