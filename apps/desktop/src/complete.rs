//! 输入框补全的纯逻辑：识别光标处的 `/指令` 和 `@文件` 触发词，列出工作区文件并按查询排序。
//! 弹出列表、键盘接管和执行动作在 `app/complete_view.rs`。

use std::ops::Range;
use std::path::Path;

/// 光标处正在输入的补全触发词。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Trigger {
    pub kind: TriggerKind,
    /// 触发词（含 `/` 或 `@`）在输入文字里的字节范围，接受补全时整个替换掉
    pub range: Range<usize>,
    /// 去掉前缀符号后的查询
    pub query: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TriggerKind {
    /// `/指令`：消息以 `/` 开头、光标在第一行
    Slash,
    /// `@文件`：任意位置，前面是空白或行首
    Mention,
}

/// 光标（字节偏移）所在的词是不是补全触发词。词从上一个空白之后开始、到光标为止。
pub fn trigger_at(text: &str, cursor: usize) -> Option<Trigger> {
    let cursor = cursor.min(text.len());
    if !text.is_char_boundary(cursor) {
        return None;
    }
    let before = &text[..cursor];
    // `/指令` 占整个第一行（指令名后面可以跟空格和参数，如「/mode 计划」）
    if text.starts_with('/') && !before.contains('\n') {
        let end = text.find('\n').unwrap_or(text.len());
        return Some(Trigger {
            kind: TriggerKind::Slash,
            range: 0..end,
            query: before[1..].to_string(),
        });
    }
    let start = before
        .char_indices()
        .rev()
        .find(|(_, c)| c.is_whitespace())
        .map_or(0, |(i, c)| i + c.len_utf8());
    let word = &before[start..];
    if !word.starts_with('@') {
        return None;
    }
    // 光标后面紧跟着的非空白也算这个词（在词中间改的时候整个替换）
    let after = &text[cursor..];
    let end = cursor + after.find(char::is_whitespace).unwrap_or(after.len());
    Some(Trigger { kind: TriggerKind::Mention, range: start..end, query: word[1..].to_string() })
}

/// 列工作区里的文件（相对路径、正斜杠），跳过隐藏目录和常见的构建 / 依赖目录，最多 `limit` 个。
pub fn list_files(root: &Path, limit: usize) -> Vec<String> {
    const SKIP: [&str; 8] =
        ["target", "node_modules", "dist", "build", "out", "__pycache__", "venv", "vendor"];
    let mut out = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        let mut entries: Vec<_> = entries.flatten().collect();
        entries.sort_by_key(|e| e.file_name());
        for entry in entries {
            let name = entry.file_name().to_string_lossy().to_string();
            let Ok(kind) = entry.file_type() else {
                continue;
            };
            if kind.is_dir() {
                let skip = name.starts_with('.')
                    || SKIP.contains(&name.as_str())
                    || name.starts_with("target-");
                if !skip {
                    stack.push(entry.path());
                }
            } else if kind.is_file()
                && let Ok(rel) = entry.path().strip_prefix(root)
            {
                out.push(rel.to_string_lossy().replace('\\', "/"));
                if out.len() >= limit {
                    return out;
                }
            }
        }
    }
    out
}

/// 按查询挑出最相关的 `n` 个文件：文件名开头命中 > 文件名包含 > 路径包含，同档路径短的在前。
/// 不区分大小写；空查询按路径深度和字母序。
pub fn rank_files(files: &[String], query: &str, n: usize) -> Vec<String> {
    let q = query.to_lowercase();
    let mut scored: Vec<(u8, usize, &String)> = files
        .iter()
        .filter_map(|path| {
            let lower = path.to_lowercase();
            let name = lower.rsplit('/').next().unwrap_or(&lower);
            let tier = if q.is_empty() || name.starts_with(&q) {
                0
            } else if name.contains(&q) {
                1
            } else if lower.contains(&q) {
                2
            } else {
                return None;
            };
            Some((tier, path.matches('/').count() * 1000 + path.len(), path))
        })
        .collect();
    scored.sort();
    scored.into_iter().take(n).map(|(_, _, p)| p.clone()).collect()
}

/// 指令是否匹配查询：关键字开头命中，或中文名 / 说明里包含。
pub fn command_matches(keyword: &str, label: &str, query: &str) -> bool {
    let q = query.to_lowercase();
    keyword.to_lowercase().starts_with(&q) || label.to_lowercase().contains(&q)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slash_only_at_start() {
        let t = trigger_at("/mo", 3).unwrap();
        assert_eq!((t.kind, t.range, t.query.as_str()), (TriggerKind::Slash, 0..3, "mo"));
        assert_eq!(trigger_at("看 /mo", 8), None);
        let t = trigger_at("/mode 计\n下一行", 9).unwrap();
        assert_eq!((t.range, t.query.as_str()), (0..9, "mode 计"));
        assert_eq!(trigger_at("/mode\n下一行", 9), None);
    }

    #[test]
    fn mention_anywhere_after_whitespace() {
        let text = "看一下 @src/ma 里的";
        let cursor = text.find(" 里").unwrap();
        let t = trigger_at(text, cursor).unwrap();
        assert_eq!(t.kind, TriggerKind::Mention);
        assert_eq!(&text[t.range.clone()], "@src/ma");
        assert_eq!(t.query, "src/ma");
        // 词中间：后半截也替换
        let t = trigger_at("@abc", 2).unwrap();
        assert_eq!((t.range, t.query.as_str()), (0..4, "a"));
        assert_eq!(trigger_at("a@b", 3), None);
        assert_eq!(trigger_at("@a b", 4), None);
    }

    #[test]
    fn ranks_name_prefix_first() {
        let files: Vec<String> =
            ["docs/main-notes.md", "src/app/main.rs", "src/main.rs", "src/domain.rs"]
                .iter()
                .map(|s| s.to_string())
                .collect();
        assert_eq!(
            rank_files(&files, "MAIN", 3),
            ["src/main.rs", "docs/main-notes.md", "src/app/main.rs"]
        );
        assert_eq!(rank_files(&files, "app", 5), ["src/app/main.rs"]);
        assert_eq!(rank_files(&files, "", 1), ["src/main.rs"]);
    }

    #[test]
    fn lists_files_skipping_build_dirs() {
        let dir = std::env::temp_dir().join(format!("cd-complete-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        for p in ["src/a.rs", "target/debug/x", ".git/HEAD", "README.md"] {
            let path = dir.join(p);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, "").unwrap();
        }
        let mut files = list_files(&dir, 100);
        files.sort();
        assert_eq!(files, ["README.md", "src/a.rs"]);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
