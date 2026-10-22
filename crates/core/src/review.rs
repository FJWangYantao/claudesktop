//! 逐块审查改动：最新一轮里 agent 改过的文件，按块（一段连续的增删）单独撤回 / 恢复。
//!
//! - 审查对象是一个文件在这一轮里的累计改动：`base`（这一轮最早的改动前）对 `head`（agent 最后写完）。
//!   两份都是检查点 blob，块列表每次现算，不持久化；块的身份是行范围（[`HunkRange`]）。
//! - 撤回 / 恢复由 UI 写盘，再追加一条带 `reverted` 的 `Record::FileChanged`：存的是写完以后
//!   哪些块处于撤回状态。它仍是普通的检查点记录，回退照常算得对。
//! - 撤回后的内容由 [`splice`] 按块从 base / head 取原始行拼出来，不在磁盘内容上打补丁。
//! - 下一条用户消息发出时，[`review_note`] 把撤回的事实写成一段说明附在前面，免得模型再改回去。
//!
//! 设计见仓库根目录「设计说明-逐块审查改动.md」。

use crate::Record;
use crate::checkpoint::user_index;
pub use crate::diff::{DiffHunk, HunkRange};
use crate::diff::{Run, hunks_with_context, runs};
use similar::TextDiff;
use std::path::Path;

/// 一块改动，带显示用的行（前后各带几行上下文，和相邻块不重复）。
pub type Hunk = DiffHunk;

/// `base` → `head` 的全部块，按位置排好（块的划分在 `diff::hunks_with_context`，和内核按块写盘同一份）。
pub fn hunks(base: &str, head: &str) -> Vec<Hunk> {
    hunks_with_context(base, head)
}
/// 按块拼出撤回后的内容：`reverted` 里的块取 base 的行，其余取 head 的行；
/// 同时返回每个撤回的块在结果里从第几行起（从 1 数）。拼的是原始行，换行符原样保留。
pub fn splice_with_positions(base: &str, head: &str, reverted: &[HunkRange]) -> (String, Vec<u32>) {
    let diff = TextDiff::from_lines(base, head);
    let (old, new) = (diff.old_slices(), diff.new_slices());
    let mut out = String::with_capacity(head.len());
    let mut line = 0u32;
    let mut positions = Vec::new();
    for run in runs(&diff) {
        let slice: &[&str] = match run {
            Run::Equal { new_start, len } => &new[new_start..new_start + len],
            Run::Change(h) if reverted.contains(&h) => {
                positions.push(line + 1);
                let s = h.old_start as usize;
                &old[s..s + h.old_len as usize]
            }
            Run::Change(h) => {
                let s = h.new_start as usize;
                &new[s..s + h.new_len as usize]
            }
        };
        for l in slice {
            out.push_str(l);
        }
        line += slice.len() as u32;
    }
    (out, positions)
}

/// 撤回 `reverted` 这几块之后文件应该是什么；None = 文件不存在（agent 新建的文件整个撤回）。
/// `base` / `head` 为 None 表示那时文件不存在。
pub fn splice(base: Option<&str>, head: Option<&str>, reverted: &[HunkRange]) -> Option<String> {
    let all = hunks(base.unwrap_or(""), head.unwrap_or(""));
    let n = all.iter().filter(|h| reverted.contains(&h.range)).count();
    if n == 0 {
        head.map(str::to_string)
    } else if n == all.len() {
        base.map(str::to_string)
    } else {
        Some(splice_with_positions(base.unwrap_or(""), head.unwrap_or(""), reverted).0)
    }
}

/// 一个文件在某一轮里的审查状态（三份内容都是 blob 哈希，None = 文件不存在）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileReview {
    pub path: String,
    /// 这一轮最早一次改动之前
    pub base: Option<String>,
    /// agent 最后一次写完（审查自己写的不算）
    pub head: Option<String>,
    /// 最后一次写完，也就是磁盘上应该是的样子
    pub current: Option<String>,
    /// 处于撤回状态的块
    pub reverted: Vec<HunkRange>,
}

/// 第 `turn` 条用户消息（从 0 数）那一轮里改过的文件，按第一次改动的顺序；`records` 是有效记录。
/// 前后一样的（改了又改回去）不列。
pub fn turn_review(records: &[Record], turn: usize) -> Vec<FileReview> {
    let Some(start) = user_index(records, turn) else {
        return Vec::new();
    };
    let mut files: Vec<FileReview> = Vec::new();
    for record in records[start + 1..].iter().take_while(|r| !matches!(r, Record::User { .. })) {
        let Record::FileChanged { path, before, after, reverted } = record else { continue };
        let i = match files.iter().position(|f| &f.path == path) {
            Some(i) => i,
            None => {
                files.push(FileReview {
                    path: path.clone(),
                    base: before.clone(),
                    head: before.clone(),
                    current: before.clone(),
                    reverted: Vec::new(),
                });
                files.len() - 1
            }
        };
        let f = &mut files[i];
        match reverted {
            Some(set) => f.reverted = set.clone(),
            None => {
                f.head = after.clone();
                f.reverted.clear();
            }
        }
        f.current = after.clone();
    }
    files.retain(|f| f.base != f.head);
    files
}

/// 最新一轮（最后一条用户消息之后）有撤回过的块时，给下一条消息附上的说明；没有时为 None。
/// `load` 按哈希取 blob 内容，路径显示成相对 `cwd` 的。
pub fn review_note(
    records: &[Record],
    cwd: Option<&Path>,
    load: impl Fn(&str) -> Option<String>,
) -> Option<String> {
    let users = records.iter().filter(|r| matches!(r, Record::User { .. })).count();
    let turn = users.checked_sub(1)?;
    let mut lines = Vec::new();
    for f in turn_review(records, turn) {
        if f.reverted.is_empty() {
            continue;
        }
        let read = |hash: &Option<String>| match hash {
            None => Some(None),
            Some(h) => load(h).map(Some),
        };
        let (Some(base), Some(head)) = (read(&f.base), read(&f.head)) else { continue };
        let (base_text, head_text) = (base.as_deref().unwrap_or(""), head.as_deref().unwrap_or(""));
        let total = hunks(base_text, head_text).len();
        let (_, positions) = splice_with_positions(base_text, head_text, &f.reverted);
        if positions.is_empty() {
            continue;
        }
        let path = display_path(&f.path, cwd);
        let what = if positions.len() == total && base.is_none() {
            "deleted (the user reverted the whole new file)".to_string()
        } else if positions.len() == total {
            format!("all {total} changes reverted; the file is back to its previous content")
        } else {
            let at: Vec<String> = positions.iter().map(u32::to_string).collect();
            format!(
                "{} of {total} changes reverted; the previous content is back around line {}",
                positions.len(),
                at.join(", ")
            )
        };
        lines.push(format!("- {path}: {what}"));
    }
    if lines.is_empty() {
        return None;
    }
    Some(format!(
        "<user-review>\nThe user reviewed the file changes from your last turn and reverted some \
         of them, so these files no longer match what you wrote:\n{}\nDo not redo the reverted \
         changes unless the user asks. Re-read a file before editing it again.\n</user-review>",
        lines.join("\n")
    ))
}

/// 路径显示成相对 `cwd` 的（不在里面就原样）。
pub fn display_path(path: &str, cwd: Option<&Path>) -> String {
    cwd.and_then(|c| Path::new(path).strip_prefix(c).ok())
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::checkpoint::restore_plan;

    fn rows(h: &Hunk) -> Vec<String> {
        h.lines.iter().map(|l| format!("{}{} {}", l.tag, l.no, l.text)).collect()
    }

    fn numbered(n: usize) -> String {
        (1..=n).map(|i| format!("l{i}\n")).collect()
    }

    #[test]
    fn hunks_split_changes_and_share_context() {
        let base = numbered(12);
        let head = base.replace("l2\n", "L2\n").replace("l5\n", "").replace("l11\n", "l11\nX\n");
        let all = hunks(&base, &head);
        assert_eq!(all.len(), 3);
        assert_eq!(rows(&all[0]), [" 1 l1", "-2 l2", "+2 L2", " 3 l3", " 4 l4"]);
        // 和上一块之间只剩两行，都已经画在上一块后面了
        assert_eq!(rows(&all[1]), ["-5 l5", " 5 l6", " 6 l7", " 7 l8"]);
        assert_eq!(rows(&all[2]), [" 8 l9", " 9 l10", " 10 l11", "+11 X", " 12 l12"]);
        assert_eq!((all[0].added(), all[0].removed()), (1, 1));
        assert_eq!((all[1].added(), all[1].removed()), (0, 1));
    }

    #[test]
    fn splice_round_trips_every_subset() {
        let base = "a\r\nb\r\nc\r\nd\r\ne";
        let head = "a\r\nB\r\nc\r\nd\r\nE\r\nf";
        let all: Vec<HunkRange> = hunks(base, head).iter().map(|h| h.range).collect();
        assert_eq!(all.len(), 2);
        assert_eq!(splice(Some(base), Some(head), &[]).as_deref(), Some(head));
        assert_eq!(splice(Some(base), Some(head), &all).as_deref(), Some(base));
        // 只撤回一块：剩下的 diff 正好是另一块
        let one = splice(Some(base), Some(head), &all[..1]).unwrap();
        assert_eq!(one, "a\r\nb\r\nc\r\nd\r\nE\r\nf");
        let left: Vec<HunkRange> = hunks(base, &one).iter().map(|h| h.range).collect();
        assert_eq!(left, [all[1]]);
        let (_, at) = splice_with_positions(base, head, &all[1..]);
        assert_eq!(at, [5]);
    }

    #[test]
    fn new_file_reverts_to_nothing() {
        let all: Vec<HunkRange> = hunks("", "x\ny\n").iter().map(|h| h.range).collect();
        assert_eq!(all.len(), 1);
        assert_eq!(splice(None, Some("x\ny\n"), &all), None);
        assert_eq!(splice(None, Some("x\ny\n"), &[]).as_deref(), Some("x\ny\n"));
        // 对不上的范围（比如升级了 diff 算法）当作没撤回
        let stale = HunkRange { old_start: 9, old_len: 1, new_start: 9, new_len: 1 };
        assert_eq!(splice(None, Some("x\ny\n"), &[stale]).as_deref(), Some("x\ny\n"));
    }

    fn changed(path: &str, before: Option<&str>, after: Option<&str>) -> Record {
        Record::FileChanged {
            path: path.into(),
            before: before.map(Into::into),
            after: after.map(Into::into),
            reverted: None,
        }
    }

    fn reviewed(path: &str, before: &str, after: Option<&str>, set: Vec<HunkRange>) -> Record {
        Record::FileChanged {
            path: path.into(),
            before: Some(before.into()),
            after: after.map(Into::into),
            reverted: Some(set),
        }
    }

    const H: HunkRange = HunkRange { old_start: 0, old_len: 1, new_start: 0, new_len: 1 };

    #[test]
    fn turn_review_tracks_base_head_and_current() {
        let records = vec![
            Record::user("一"),
            changed("a", Some("a0"), Some("a1")),
            Record::user("二"),
            changed("a", Some("a1"), Some("a2")),
            changed("b", None, Some("b1")),
            changed("a", Some("a2"), Some("a3")),
            // 改了又改回去：不列
            changed("c", Some("c0"), Some("c1")),
            changed("c", Some("c1"), Some("c0")),
            reviewed("a", "a3", Some("a4"), vec![H]),
        ];
        let files = turn_review(&records, 1);
        assert_eq!(
            files,
            [
                FileReview {
                    path: "a".into(),
                    base: Some("a1".into()),
                    head: Some("a3".into()),
                    current: Some("a4".into()),
                    reverted: vec![H],
                },
                FileReview {
                    path: "b".into(),
                    base: None,
                    head: Some("b1".into()),
                    current: Some("b1".into()),
                    reverted: vec![],
                },
            ]
        );
        assert_eq!(turn_review(&records, 0)[0].head, Some("a1".into()));
        // agent 审查之后又写了一次：head 跟着走，撤回状态清空
        let mut more = records.clone();
        more.push(changed("a", Some("a4"), Some("a5")));
        let a = &turn_review(&more, 1)[0];
        assert_eq!((a.head.as_deref(), a.reverted.len()), (Some("a5"), 0));
        // 审查写的记录照常进回退：回到这一轮之前还原成 base，预期现状是审查写完的
        let plan = restore_plan(&records, 1);
        assert_eq!(plan[0].target.as_deref(), Some("a1"));
        assert_eq!(plan[0].expected.as_deref(), Some("a4"));
    }

    #[test]
    fn old_logs_without_reverted_still_parse() {
        let line = r#"{"t":"file_changed","path":"a","before":"x","after":"y"}"#;
        let record: Record = serde_json::from_str(line).unwrap();
        assert_eq!(record, changed("a", Some("x"), Some("y")));
        let back = serde_json::to_string(&record).unwrap();
        assert!(!back.contains("reverted"), "{back}");
    }

    #[test]
    fn note_lists_reverted_files_of_the_latest_turn() {
        let blobs = [
            ("b0", "1\n2\n3\n4\n5\n6\n7\n8\n9\n"),
            ("b1", "1\nTWO\n3\n4\n5\n6\n7\n8\nNINE\n"),
            ("n1", "new\n"),
        ];
        let load = |h: &str| blobs.iter().find(|(k, _)| *k == h).map(|(_, v)| v.to_string());
        let two = hunks(blobs[0].1, blobs[1].1);
        let new = hunks("", "new\n");
        let mut records = vec![
            Record::user("改"),
            changed("/w/src/a.rs", Some("b0"), Some("b1")),
            changed("/w/n.rs", None, Some("n1")),
        ];
        assert_eq!(review_note(&records, Some(Path::new("/w")), load), None);
        records.push(reviewed("/w/src/a.rs", "b1", Some("x"), vec![two[1].range]));
        records.push(Record::FileChanged {
            path: "/w/n.rs".into(),
            before: Some("n1".into()),
            after: None,
            reverted: Some(vec![new[0].range]),
        });
        let note = review_note(&records, Some(Path::new("/w")), load).unwrap();
        assert!(note.contains("- src/a.rs: 1 of 2 changes reverted"), "{note}");
        assert!(note.contains("around line 9"), "{note}");
        assert!(note.contains("- n.rs: deleted"), "{note}");
        // 又全部恢复：不附
        records.push(reviewed("/w/src/a.rs", "x", Some("b1"), vec![]));
        records.push(reviewed("/w/n.rs", "n1", Some("n1"), vec![]));
        assert_eq!(review_note(&records, Some(Path::new("/w")), load), None);
        // 发过下一条消息之后不再附
        records.push(reviewed("/w/src/a.rs", "b1", Some("x"), vec![two[1].range]));
        records.push(Record::user("下一条"));
        assert_eq!(review_note(&records, Some(Path::new("/w")), load), None);
    }
}
