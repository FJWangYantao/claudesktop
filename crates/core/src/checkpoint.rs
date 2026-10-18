//! 检查点：内核改文件时记下改动前后的内容，回退到某条用户消息之前时据此还原文件。
//!
//! - 内核在改文件前后各读一次（[`read_snapshot`]），发 `KernelEvent::FileChanged`；
//!   UI 把内容存成 blob（`SessionStore::save_blob`，按 [`content_hash`] 去重），日志里记 `Record::FileChanged`。
//! - 回退时 [`restore_plan`] 从有效记录（`store::effective`）算出每个文件要还原成什么、
//!   现在应该是什么；现状对不上的（agent 改完之后又被改过）算冲突，由 UI 决定覆不覆盖。
//! - 只覆盖内核自己的写文件 / 改文件工具：shell 命令改的文件看不见（UI 里提示）。
//!
//! 设计见仓库根目录「设计说明-检查点与回退.md」。

use crate::{KernelEvent, Record, SessionStore};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::path::Path;

/// 超过这个大小的文件不记快照（编辑工具本来就只处理文本）
pub const SNAPSHOT_MAX_BYTES: u64 = 2 << 20;

/// 文件内容的哈希（sha256 十六进制）：blob 的文件名，也用来比对现状。
pub fn content_hash(text: &str) -> String {
    Sha256::digest(text.as_bytes()).iter().map(|b| format!("{b:02x}")).collect()
}

/// 读文件当前内容作快照：`Some(None)` = 文件不存在，`Some(Some(内容))` = 文本内容；
/// 太大、不是 UTF-8 或读不了时为 None（这次改动不记检查点）。
pub fn read_snapshot(path: &Path) -> Option<Option<String>> {
    match std::fs::metadata(path) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Some(None),
        Err(_) => None,
        Ok(m) if !m.is_file() || m.len() > SNAPSHOT_MAX_BYTES => None,
        Ok(_) => std::fs::read_to_string(path).ok().map(Some),
    }
}

/// 回退时一个文件怎么还原。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileRestore {
    pub path: String,
    /// 还原成的内容（blob 哈希）；None = 这个文件原本不存在，还原就是删掉
    pub target: Option<String>,
    /// agent 最后一次改完的内容（blob 哈希）；磁盘上的现状对不上就是冲突
    pub expected: Option<String>,
}

/// 第 `turn` 条用户消息（从 0 数）在有效记录里的下标；没有这么多条时为 None。
pub fn user_index(records: &[Record], turn: usize) -> Option<usize> {
    records
        .iter()
        .enumerate()
        .filter(|(_, r)| matches!(r, Record::User { .. }))
        .nth(turn)
        .map(|(i, _)| i)
}

/// 回到第 `turn` 条用户消息之前要还原的文件（`records` 是有效记录）：那条消息之后改过的每个文件，
/// 还原成最早一次改动之前的样子；前后一样的（改了又改回去）不用动。按路径排序。
pub fn restore_plan(records: &[Record], turn: usize) -> Vec<FileRestore> {
    let Some(start) = user_index(records, turn) else {
        return Vec::new();
    };
    // 路径 → (最早的 before, 最晚的 after)
    let mut files: BTreeMap<&str, (Option<&String>, Option<&String>)> = BTreeMap::new();
    for record in &records[start..] {
        if let Record::FileChanged { path, before, after } = record {
            files
                .entry(path)
                .and_modify(|(_, last)| *last = after.as_ref())
                .or_insert((before.as_ref(), after.as_ref()));
        }
    }
    files
        .into_iter()
        .filter(|(_, (first, last))| first != last)
        .map(|(path, (first, last))| FileRestore {
            path: path.to_string(),
            target: first.cloned(),
            expected: last.cloned(),
        })
        .collect()
}

/// 第 `turn` 条用户消息起跑过几条命令（shell 改的文件还原不了，确认时提示）。
pub fn commands_after(records: &[Record], turn: usize) -> usize {
    let Some(start) = user_index(records, turn) else {
        return 0;
    };
    let records = &records[start..];
    // 被拒绝的命令没跑过，不会改文件
    let denied: std::collections::HashSet<&str> = records
        .iter()
        .filter_map(|r| match r {
            Record::Event {
                event: KernelEvent::ToolCallUpdated { id, raw_output: Some(raw), .. },
            } if is_denial(raw) => Some(id.as_str()),
            _ => None,
        })
        .collect();
    records
        .iter()
        .filter(|r| {
            matches!(r, Record::Event { event: KernelEvent::ToolCallStarted { kind, id, .. } }
                if kind == "execute" && !denied.contains(id.as_str()))
        })
        .count()
}

/// 工具结果说的是「用户拒绝了」：自研内核写「已拒绝」，ACP agent 各有说法（Claude Code 是
/// 「The user doesn't want to proceed …」）。
fn is_denial(raw: &str) -> bool {
    let lower = raw.to_lowercase();
    raw.trim() == "已拒绝"
        || ["doesn't want to proceed", "rejected", "denied", "declined"]
            .iter()
            .any(|m| lower.contains(m))
}

/// 把一个文件还原成 `restore.target`：写回那份内容，原本不存在的就删掉（已经没了也算成功）。
pub fn restore(store: &SessionStore, id: &str, restore: &FileRestore) -> std::io::Result<()> {
    let path = Path::new(&restore.path);
    match &restore.target {
        None => match std::fs::remove_file(path) {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            other => other,
        },
        Some(hash) => {
            let text = store.load_blob(id, hash)?;
            if let Some(dir) = path.parent() {
                std::fs::create_dir_all(dir)?;
            }
            std::fs::write(path, text)
        }
    }
}

/// 磁盘上的现状和 agent 最后改完的样子对不上（之后又被用户、命令或别的会话改过）。
pub fn is_conflict(restore: &FileRestore) -> bool {
    match read_snapshot(Path::new(&restore.path)) {
        Some(current) => current.as_deref().map(content_hash) != restore.expected,
        None => true,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::effective;

    fn user(text: &str) -> Record {
        Record::user(text)
    }

    fn changed(path: &str, before: Option<&str>, after: Option<&str>) -> Record {
        Record::FileChanged {
            path: path.into(),
            before: before.map(Into::into),
            after: after.map(Into::into),
        }
    }

    fn rewind(turn: usize, conversation: bool) -> Record {
        Record::Rewind { turn, conversation, files: 0, prompt: String::new() }
    }

    #[test]
    fn plan_takes_first_before_and_last_after() {
        let records = vec![
            user("一"),
            changed("a", None, Some("a1")),
            user("二"),
            changed("a", Some("a1"), Some("a2")),
            changed("b", Some("b0"), Some("b1")),
            user("三"),
            changed("a", Some("a2"), Some("a3")),
            // 改了又改回去：不用动
            changed("c", Some("c0"), Some("c1")),
            changed("c", Some("c1"), Some("c0")),
        ];
        let plan = restore_plan(&records, 1);
        assert_eq!(
            plan,
            vec![
                FileRestore {
                    path: "a".into(),
                    target: Some("a1".into()),
                    expected: Some("a3".into())
                },
                FileRestore {
                    path: "b".into(),
                    target: Some("b0".into()),
                    expected: Some("b1".into())
                },
            ]
        );
        // 回到第一条之前：a 原本不存在，还原就是删掉
        assert_eq!(restore_plan(&records, 0)[0].target, None);
        assert!(restore_plan(&records, 5).is_empty());
    }

    #[test]
    fn effective_cuts_rewound_turns() {
        let records = vec![
            user("一"),
            changed("a", None, Some("a1")),
            user("二"),
            changed("a", Some("a1"), Some("a2")),
            rewind(1, true),
            user("二改"),
            // 只还原文件：对话不动
            rewind(0, false),
        ];
        let eff = effective(&records);
        let users: Vec<_> = eff
            .iter()
            .filter_map(|r| match r {
                Record::User { text, .. } => Some(text.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(users, ["一", "二改"]);
        assert_eq!(eff.len(), 5);
        // 回退后再算：被撤回那一轮的改动不在计划里
        assert_eq!(restore_plan(&eff, 1), Vec::new());
    }

    #[test]
    fn counts_commands_after_turn() {
        let started = |kind: &str| Record::Event {
            event: KernelEvent::ToolCallStarted {
                id: "1".into(),
                title: "t".into(),
                status: "pending".into(),
                kind: kind.into(),
                parent: None,
            },
        };
        let records =
            vec![user("一"), started("execute"), user("二"), started("execute"), started("read")];
        assert_eq!(commands_after(&records, 0), 2);
        assert_eq!(commands_after(&records, 1), 1);
        // 被拒绝的那条没跑过，不算
        let denied = Record::Event {
            event: KernelEvent::ToolCallUpdated {
                id: "1".into(),
                title: None,
                status: Some("failed".into()),
                raw_output: Some("已拒绝".into()),
                output: None,
            },
        };
        assert_eq!(commands_after(&[user("一"), started("execute"), denied], 0), 0);
    }

    #[test]
    fn snapshot_and_conflict() {
        let dir = std::env::temp_dir().join(format!("claudesktop-ckpt-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("x.txt");
        let _ = std::fs::remove_file(&file);
        assert_eq!(read_snapshot(&file), Some(None));
        std::fs::write(&file, "新内容").unwrap();
        assert_eq!(read_snapshot(&file), Some(Some("新内容".into())));
        let path = file.to_string_lossy().into_owned();
        let ok = FileRestore {
            path: path.clone(),
            target: None,
            expected: Some(content_hash("新内容")),
        };
        assert!(!is_conflict(&ok));
        let stale = FileRestore {
            path: path.clone(),
            target: None,
            expected: Some(content_hash("旧内容")),
        };
        assert!(is_conflict(&stale));
        // 还原：写回 blob 里的内容；目标是「不存在」时删掉
        let store = SessionStore::open(dir.join("sessions")).unwrap();
        let hash = store.save_blob("s", "旧内容").unwrap();
        assert_eq!(store.save_blob("s", "旧内容").unwrap(), hash);
        restore(
            &store,
            "s",
            &FileRestore { path: path.clone(), target: Some(hash), expected: None },
        )
        .unwrap();
        assert_eq!(std::fs::read_to_string(&file).unwrap(), "旧内容");
        restore(&store, "s", &FileRestore { path: path.clone(), target: None, expected: None })
            .unwrap();
        assert!(!file.exists());
        restore(&store, "s", &FileRestore { path, target: None, expected: None }).unwrap();
        let _ = std::fs::remove_dir_all(&dir);
    }
}
