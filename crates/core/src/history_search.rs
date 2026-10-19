//! 历史会话全文搜索：扫磁盘上的会话日志（内存里只恢复最近的一百个，更早的搜不到）。
//!
//! 日志是 JSONL，每行一条 [`Record`]。这里不恢复整个会话（不重建对话、不读 blob），
//! 只把用户消息和助手回复的文字抽出来匹配，命中后给「会话 ID + 从第几条用户消息开始 +
//! 一小段上下文」。界面按这个结果恢复那个会话再跳过去。
//!
//! 扫描放在后台线程里跑（见桌面端 `search.rs`）：几百个会话、每个几千条记录时
//! 逐个读文件要几十到几百毫秒，不能占 UI 线程。

use crate::KernelEvent;
use crate::store::{Record, effective};
use std::path::Path;

/// 一个会话最多列几条、整次搜索最多列几条
pub const PER_SESSION: usize = 3;
pub const MAX_HITS: usize = 30;
/// 查询最少几个字符（一个字命中太多）
pub const MIN_QUERY_CHARS: usize = 2;
/// 摘要里命中处前面留几个字、一共最多多少字
const SNIPPET_BEFORE: usize = 16;
pub const SNIPPET_LEN: usize = 72;

/// 一条命中的消息。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HistoryHit {
    /// 会话 ID（日志文件名去掉扩展名）
    pub session: String,
    /// 这条消息属于第几条用户消息（从 0 数；界面按它定位到对话里的位置）
    pub turn: usize,
    /// 是否用户发的（否则是助手回复）
    pub from_user: bool,
    /// 命中处附近的一小段（空白压成一个空格）
    pub snippet: String,
    /// 这一轮的标题线索：那一轮的用户消息开头（列表里没有会话标题时显示）
    pub prompt: String,
}

/// 在 `files`（会话日志路径，按想要的优先级排好）里搜 `query`。
/// 不分大小写；`max_hits` 到了就停，所以给最近的几个文件调用最划算。
pub fn search_logs(
    files: impl IntoIterator<Item = (String, std::path::PathBuf)>,
    query: &str,
) -> Vec<HistoryHit> {
    let needle = query.trim().to_lowercase();
    if needle.chars().count() < MIN_QUERY_CHARS {
        return Vec::new();
    }
    let mut hits = Vec::new();
    for (session, path) in files {
        let Ok(records) = crate::store::read_log(&path) else {
            continue;
        };
        let found = search_records(&session, &records, &needle, &mut hits);
        if found > 0 && hits.len() >= MAX_HITS {
            break;
        }
    }
    hits.truncate(MAX_HITS);
    hits
}

/// 在已读出的记录里搜（单测直接用它）。`hits` 里已有的也算进上限。
/// 返回这个会话命中了几条。
pub fn search_records(
    session: &str,
    records: &[Record],
    needle_lower: &str,
    hits: &mut Vec<HistoryHit>,
) -> usize {
    let mut found = 0;
    let mut turn = 0;
    // 这一轮的用户消息（摘要里的 `prompt` 和「第几轮」都用它）
    let mut prompt = String::new();
    for record in effective(records) {
        match &record {
            Record::User { text, .. } => {
                prompt = first_line(text);
                if let Some(snippet) = snippet(text, needle_lower) {
                    hits.push(HistoryHit {
                        session: session.to_string(),
                        turn,
                        from_user: true,
                        snippet,
                        prompt: prompt.clone(),
                    });
                    found += 1;
                    if found >= PER_SESSION || hits.len() >= MAX_HITS {
                        return found;
                    }
                }
                turn += 1;
            }
            Record::Event { event: KernelEvent::AgentMessageChunk { text } } => {
                if let Some(snippet) = snippet(text, needle_lower) {
                    hits.push(HistoryHit {
                        session: session.to_string(),
                        turn: turn.saturating_sub(1),
                        from_user: false,
                        snippet,
                        prompt: prompt.clone(),
                    });
                    found += 1;
                    if found >= PER_SESSION || hits.len() >= MAX_HITS {
                        return found;
                    }
                }
            }
            _ => {}
        }
    }
    found
}

/// `text` 里有 `needle`（已转小写）就摘出命中处附近的一段：前面留几个字，空白压成一个空格，
/// 截掉的头尾写省略号。没有命中为 None。
fn snippet(text: &str, needle: &str) -> Option<String> {
    let lower = text.to_lowercase();
    let at = lower.find(needle)?;
    // 少数字符转小写后字节数会变，偏移对不上就从头摘
    let at = if lower.len() == text.len() && text.is_char_boundary(at) { at } else { 0 };
    let before: Vec<char> = text[..at].chars().collect();
    let start = before.len().saturating_sub(SNIPPET_BEFORE);
    let head: String = before[start..].iter().collect();
    let piece: String = head.chars().chain(text[at..].chars()).take(SNIPPET_LEN).collect();
    let mut out = piece.split_whitespace().collect::<Vec<_>>().join(" ");
    if start > 0 {
        out.insert(0, '…');
    }
    if text.chars().count().saturating_sub(start) > SNIPPET_LEN {
        out.push('…');
    }
    Some(out)
}

/// 第一行（压成一行、截短）：列表里当标题线索用。
fn first_line(text: &str) -> String {
    let line = text.lines().find(|l| !l.trim().is_empty()).unwrap_or("").trim();
    let mut out: String = line.split_whitespace().collect::<Vec<_>>().join(" ");
    if out.chars().count() > 40 {
        out = out.chars().take(40).collect::<String>() + "…";
    }
    out
}

/// 会话日志文件列表（最近更新的在前），配合 [`search_logs`] 用。
pub fn log_files(dir: &Path) -> Vec<(String, std::path::PathBuf)> {
    let mut found: Vec<(u64, String, std::path::PathBuf)> = std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|e| {
            let path = e.path();
            if path.extension().is_some_and(|x| x == "jsonl") {
                let id = path.file_stem()?.to_string_lossy().into_owned();
                let modified = e
                    .metadata()
                    .and_then(|m| m.modified())
                    .ok()
                    .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                    .map_or(0, |d| d.as_millis() as u64);
                Some((modified, id, path))
            } else {
                None
            }
        })
        .collect();
    found.sort_by_key(|(m, _, _)| std::cmp::Reverse(*m));
    found.into_iter().map(|(_, id, path)| (id, path)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn user(text: &str) -> Record {
        Record::user(text)
    }

    fn agent(text: &str) -> Record {
        Record::Event { event: KernelEvent::AgentMessageChunk { text: text.into() } }
    }

    #[test]
    fn finds_user_and_agent_text_case_insensitively() {
        let records = vec![
            user("帮我看看 Kernel trait"),
            agent("kernel 拆成两个更好"),
            Record::Event { event: KernelEvent::Status("kernel 不算".into()) },
        ];
        let mut hits = Vec::new();
        search_records("s1", &records, "kernel", &mut hits);
        assert_eq!(hits.len(), 2);
        assert!(hits[0].from_user && hits[0].turn == 0);
        assert!(!hits[1].from_user && hits[1].turn == 0);
        // 太短不搜
        assert!(search_logs(Vec::new(), "a").is_empty());
    }

    #[test]
    fn tracks_turns_and_prompts() {
        let records = vec![
            user("第一轮的问题"),
            agent("第一轮的回答"),
            user("第二轮里提到 needle"),
            agent("第二轮的回答也提到 needle"),
        ];
        let mut hits = Vec::new();
        search_records("s", &records, "needle", &mut hits);
        assert_eq!(hits.len(), 2);
        assert_eq!(hits[0].turn, 1);
        assert_eq!(hits[0].prompt, "第二轮里提到 needle");
        assert!(!hits[1].from_user);
    }

    #[test]
    fn caps_hits_per_session() {
        let records: Vec<Record> = (0..10).map(|i| user(&format!("第 {i} 条 needle"))).collect();
        let mut hits = Vec::new();
        search_records("s", &records, "needle", &mut hits);
        assert_eq!(hits.len(), PER_SESSION);
    }

    #[test]
    fn rewound_turns_are_not_searched() {
        let records = vec![
            user("old needle here"),
            Record::Rewind { turn: 0, conversation: true, files: 0, prompt: "old".into() },
            user("新的问题"),
        ];
        let mut hits = Vec::new();
        search_records("s", &records, "needle", &mut hits);
        assert!(hits.is_empty(), "{hits:?}");
    }

    #[test]
    fn snippet_keeps_context_on_one_line() {
        let text = format!(
            "{}\n\n命中的词在这里，后面还有{}",
            "很长的开头".repeat(10),
            "很多字".repeat(30)
        );
        let s = snippet(&text, "命中的词").unwrap();
        assert!(s.starts_with('…') && s.ends_with('…'), "{s}");
        assert!(s.contains("命中的词") && !s.contains('\n'), "{s}");
        assert_eq!(snippet("短句 hit", "hit").unwrap(), "短句 hit");
        assert_eq!(snippet("没有", "hit"), None);
    }

    #[test]
    fn log_files_are_sorted_by_recency() {
        let dir = std::env::temp_dir().join(format!("cd-history-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("old.jsonl"), "").unwrap();
        std::thread::sleep(std::time::Duration::from_millis(20));
        std::fs::write(dir.join("new.jsonl"), "").unwrap();
        std::fs::write(dir.join("meta.json"), "{}").unwrap();
        let files = log_files(&dir);
        assert_eq!(files.len(), 2);
        assert_eq!(files[0].0, "new");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
