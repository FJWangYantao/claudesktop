//! 跨会话的记忆：用户偏好、项目约定、做过的决定、踩过的坑。
//!
//! 存两份 JSON（格式见「设计说明-记忆系统.md」）：
//! - 全局：`<数据目录>/memory.json`（[`global_path`]）
//! - 项目：`<项目根>/.claudesktop/memory.json`（[`project_path`]，和 `AGENTS.md` 一起进 git）
//!
//! **文件是唯一真相**：每次要判断 / 显示之前重读，不在内存里长驻——界面删掉的条目下一轮就没了。
//! 这一层不依赖内核、不依赖 UI，全部可单测（和 `project` / `checkpoint` 的分层一致）。
//!
//! 一条记忆 = 一句话结论 + 为什么，不是流水账；`text` 硬截到 [`MAX_TEXT_CHARS`]。
//! 进上下文时按字符预算截取（[`pick`]），不是全塞。

use serde::{Deserialize, Serialize};
use std::io;
use std::path::{Path, PathBuf};

/// 一条记忆的 `text` 最多多少字符（超了截断）
pub const MAX_TEXT_CHARS: usize = 400;
/// 一条记忆最多带几个标签
pub const MAX_TAGS: usize = 6;
/// 全局记忆最多多少条（超了淘汰最久没被带进上下文的，`pinned` 不淘汰）
pub const GLOBAL_MAX: usize = 300;
/// 项目记忆最多多少条
pub const PROJECT_MAX: usize = 200;
/// 项目记忆进系统提示词的字符预算（全量，一般到不了）
pub const PROJECT_BUDGET: usize = 3_000;
/// 全局记忆进系统提示词的字符预算（按 `pinned` → `updated_ms` 取到这里为止）
pub const GLOBAL_BUDGET: usize = 1_500;

/// 记忆的作用范围。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Scope {
    /// 跨项目通用（用户偏好、环境坑）
    Global,
    /// 只对某个项目（项目约定、做过的决定）
    #[default]
    Project,
}

impl Scope {
    pub const ALL: [Scope; 2] = [Scope::Project, Scope::Global];

    pub fn label(self) -> &'static str {
        match self {
            Scope::Project => "这个项目",
            Scope::Global => "全局",
        }
    }

    /// 存盘 / 事件里用的名字
    pub fn name(self) -> &'static str {
        match self {
            Scope::Project => "project",
            Scope::Global => "global",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "" => None,
            "project" | "项目" => Some(Scope::Project),
            "global" | "全局" => Some(Scope::Global),
            _ => None,
        }
    }

    /// 这个范围最多留几条
    pub fn max_entries(self) -> usize {
        match self {
            Scope::Project => PROJECT_MAX,
            Scope::Global => GLOBAL_MAX,
        }
    }
}

/// 这条记忆是在哪一轮记下的（界面上「来源」跳到那个会话）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Source {
    /// 会话 ID（`SessionMeta::id`）
    pub session: String,
    /// 第几轮（从 1 数）
    #[serde(default)]
    pub turn: u64,
}

/// 一条记忆。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Entry {
    #[serde(default)]
    pub id: String,
    pub text: String,
    #[serde(default)]
    pub tags: Vec<String>,
    #[serde(default)]
    pub pinned: bool,
    #[serde(default)]
    pub created_ms: u64,
    #[serde(default)]
    pub updated_ms: u64,
    /// 最近一次被带进上下文的时间（淘汰按它：最久没用的先走）
    #[serde(default)]
    pub used_ms: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<Source>,
}

impl Entry {
    /// 新记一条（`now_ms` 由调用方给，便于单测）。
    pub fn new(text: impl Into<String>, now_ms: u64) -> Self {
        let text = clip_text(text.into().trim());
        Self {
            id: new_id(now_ms),
            text,
            tags: Vec::new(),
            pinned: false,
            created_ms: now_ms,
            updated_ms: now_ms,
            used_ms: now_ms,
            source: None,
        }
    }

    /// 进提示词的一行
    fn prompt_line(&self) -> String {
        if self.tags.is_empty() {
            format!("- {}", self.text)
        } else {
            format!("- {}（{}）", self.text, self.tags.join("、"))
        }
    }
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct FileFormat {
    #[serde(default)]
    entries: Vec<Entry>,
}

/// 全局记忆文件：数据目录的 `memory.json`。
pub fn global_path() -> Option<PathBuf> {
    crate::default_data_dir().ok().map(|d| d.join("memory.json"))
}

/// 项目记忆文件：`<项目根>/.claudesktop/memory.json`。
pub fn project_path(root: &Path) -> PathBuf {
    root.join(crate::project::DIR).join("memory.json")
}

/// 工作目录对应的项目记忆文件（找不到项目根时为 None：新项目还没有任何标记）。
pub fn project_file(cwd: &Path) -> Option<PathBuf> {
    crate::project::project_root(cwd).map(project_path)
}

/// 工作目录对应的项目记忆文件；没有项目根（不是 git 仓库、也没有 `.claudesktop`）时
/// 放到工作目录下——写和读必须用同一个判断，否则记下的东西下一轮读不回来。
pub fn project_file_or(cwd: &Path) -> PathBuf {
    project_file(cwd).unwrap_or_else(|| project_path(cwd))
}

/// 读一份记忆；文件不存在 = 空表，坏了 = 空表 + 能照着改的中文原因。
pub fn load(path: &Path) -> (Vec<Entry>, Option<String>) {
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        // 没建过 = 还没有记忆
        Err(e) if e.kind() == io::ErrorKind::NotFound => return (Vec::new(), None),
        Err(e) => return (Vec::new(), Some(format!("读不了 {}：{e}", path.display()))),
    };
    if text.trim().is_empty() {
        return (Vec::new(), None);
    }
    match serde_json::from_str::<FileFormat>(&text) {
        Ok(f) => (f.entries, None),
        Err(e) => (
            Vec::new(),
            Some(format!(
                "{} 的格式有问题：{}（先当成空表；改好这一行再重启就回来了）",
                path.display(),
                crate::json_error_text(&e)
            )),
        ),
    }
}

/// 原子写（临时文件 + 改名）。写失败时原来的文件不动。
pub fn save(path: &Path, entries: &[Entry]) -> io::Result<()> {
    let file = FileFormat { entries: entries.to_vec() };
    let mut text = serde_json::to_string_pretty(&file).map_err(io::Error::other)?;
    text.push('\n');
    if let Some(dir) = path.parent().filter(|d| !d.as_os_str().is_empty()) {
        std::fs::create_dir_all(dir)?;
    }
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, text)?;
    std::fs::rename(&tmp, path)
}

/// 新增或更新一条：同一 `tags` + 高相似 `text` 视为同一条（返回 true = 更新了已有的）。
/// 更新时保留 `id` / `created_ms` / `used_ms`，`pinned` 只升不降。
pub fn upsert(entries: &mut Vec<Entry>, entry: Entry) -> bool {
    let entry = Entry { text: clip_text(entry.text.trim()), ..entry };
    if let Some(old) = entries
        .iter_mut()
        .find(|e| similar(&e.text, &entry.text) && same_tags(&e.tags, &entry.tags))
    {
        old.text = entry.text;
        old.tags = entry.tags;
        old.pinned |= entry.pinned;
        old.updated_ms = entry.updated_ms;
        if entry.source.is_some() {
            old.source = entry.source;
        }
        return true;
    }
    entries.push(entry);
    false
}

/// 超上限时淘汰最久没被带进上下文的那条（`pinned` 不动）。
pub fn enforce_limits(entries: &mut Vec<Entry>, max: usize) {
    while entries.len() > max {
        let victim = entries
            .iter()
            .enumerate()
            .filter(|(_, e)| !e.pinned)
            .min_by_key(|(_, e)| e.used_ms)
            .map(|(i, _)| i);
        match victim {
            Some(i) => drop(entries.remove(i)),
            // 全是 pinned：不再删
            None => return,
        }
    }
}

/// 进上下文的顺序：`pinned` 优先，再按 `updated_ms` 新的在前，截到 `budget` 字符。
pub fn pick(entries: &[Entry], budget: usize) -> Vec<&Entry> {
    let mut sorted: Vec<&Entry> = entries.iter().collect();
    sorted.sort_by(|a, b| {
        b.pinned
            .cmp(&a.pinned)
            .then(b.updated_ms.cmp(&a.updated_ms))
            .then(b.used_ms.cmp(&a.used_ms))
    });
    let mut used = 0;
    let mut out = Vec::new();
    for entry in sorted {
        let len = entry.text.chars().count()
            + entry.tags.iter().map(|t| t.chars().count() + 1).sum::<usize>();
        if used + len > budget && !out.is_empty() {
            break;
        }
        used += len;
        out.push(entry);
        if used >= budget {
            break;
        }
    }
    out
}

/// 系统提示词里的 `# Memory` 一节；两份都空时返回 None。
pub fn prompt_section(project: &[Entry], global: &[Entry]) -> Option<String> {
    if project.is_empty() && global.is_empty() {
        return None;
    }
    let mut out = String::from(
        "# Memory\n\
         Notes saved in earlier sessions (user preferences, project conventions, decisions and \
         pitfalls) so the user does not have to repeat them. They are background: when they \
         conflict with the user and project instructions above, the instructions win. \
         Take them into account without restating them. For something that will matter in later \
         sessions but does not belong in the repository, save one short note with `memory_write`; \
         when the user asks you to remember something, save it there instead of repeating the \
         conversation. Never store tokens, passwords or other credentials.",
    );
    let project_picks = pick(project, PROJECT_BUDGET);
    if !project_picks.is_empty() {
        out.push_str("\n\n## This project\n");
        for entry in project_picks {
            out.push_str(&entry.prompt_line());
            out.push('\n');
        }
    }
    let global_picks = pick(global, GLOBAL_BUDGET);
    if !global_picks.is_empty() {
        out.push_str("\n## Everywhere\n");
        for entry in global_picks {
            out.push_str(&entry.prompt_line());
            out.push('\n');
        }
    }
    Some(out.trim_end().to_string())
}

/// 抽词（ASCII 词 + 中文二元组，都转小写）：第 2 期的关键词匹配用。
pub fn keywords(text: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut push = |word: String| {
        if !word.is_empty() && !out.contains(&word) {
            out.push(word);
        }
    };
    let mut ascii = String::new();
    let mut cjk: Vec<char> = Vec::new();
    let flush_ascii = |ascii: &mut String, push: &mut dyn FnMut(String)| {
        if ascii.chars().count() >= 2 {
            push(ascii.to_lowercase());
        }
        ascii.clear();
    };
    let flush_cjk = |cjk: &mut Vec<char>, push: &mut dyn FnMut(String)| {
        // 二元组：单字太泛，三字以上容易被标点切断
        for pair in cjk.windows(2) {
            push(pair.iter().collect());
        }
        cjk.clear();
    };
    for c in text.chars() {
        let is_cjk = matches!(c as u32,
            0x3400..=0x4DBF | 0x4E00..=0x9FFF | 0xF900..=0xFAFF | 0x3040..=0x30FF | 0xAC00..=0xD7AF);
        if is_cjk {
            flush_ascii(&mut ascii, &mut push);
            cjk.push(c);
        } else if c.is_ascii_alphanumeric() || c == '_' {
            flush_cjk(&mut cjk, &mut push);
            ascii.push(c);
        } else {
            flush_ascii(&mut ascii, &mut push);
            flush_cjk(&mut cjk, &mut push);
        }
    }
    flush_ascii(&mut ascii, &mut push);
    flush_cjk(&mut cjk, &mut push);
    out
}

/// 截到 [`MAX_TEXT_CHARS`]，截掉了加省略号。
fn clip_text(text: &str) -> String {
    match text.char_indices().nth(MAX_TEXT_CHARS) {
        Some((cut, _)) => format!("{}…", &text[..cut]),
        None => text.to_string(),
    }
}

/// 比相似用的规范形：小写，只留字母、数字和 CJK。
fn norm(text: &str) -> String {
    text.chars().filter(|c| c.is_alphanumeric()).flat_map(char::to_lowercase).collect()
}

/// 两句话是不是同一条：规范形相同，或者短的是长的子串且长度接近。
fn similar(a: &str, b: &str) -> bool {
    let (a, b) = (norm(a), norm(b));
    if a.is_empty() || b.is_empty() {
        return false;
    }
    if a == b {
        return true;
    }
    let (short, long) = if a.chars().count() <= b.chars().count() { (a, b) } else { (b, a) };
    let short_len = short.chars().count();
    long.contains(short.as_str()) && short_len * 10 >= long.chars().count() * 7
}

/// 标签集合是否相同（不分大小写、不看顺序；一边没有标签时算相同）。
fn same_tags(a: &[String], b: &[String]) -> bool {
    if a.is_empty() || b.is_empty() {
        return true;
    }
    let key = |tags: &[String]| {
        let mut v: Vec<String> = tags.iter().map(|t| t.trim().to_lowercase()).collect();
        v.sort();
        v.dedup();
        v
    };
    key(a) == key(b)
}

fn new_id(now_ms: u64) -> String {
    static COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let n = COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    format!("m{now_ms:x}{n:x}")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("claudesktop-memory-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn entry(text: &str, now: u64) -> Entry {
        Entry::new(text, now)
    }

    #[test]
    fn missing_file_is_empty() {
        let dir = tmp("missing");
        let (entries, warning) = load(&dir.join("memory.json"));
        assert!(entries.is_empty());
        assert!(warning.is_none());
    }

    #[test]
    fn broken_file_yields_reason_and_empty_table() {
        let dir = tmp("broken");
        let path = dir.join("memory.json");
        std::fs::write(&path, "{\n  \"entries\": [ {, } ]\n}").unwrap();
        let (entries, warning) = load(&path);
        assert!(entries.is_empty());
        let warning = warning.unwrap();
        // 「第几行第几列：什么问题」
        assert!(warning.contains("memory.json"), "{warning}");
        assert!(warning.contains('行'), "{warning}");
    }

    #[test]
    fn unknown_fields_are_ignored() {
        let dir = tmp("unknown");
        let path = dir.join("memory.json");
        std::fs::write(
            &path,
            r#"{"version":2,"entries":[{"id":"m1","text":"用 pnpm","extra":{"a":1}}]}"#,
        )
        .unwrap();
        let (entries, warning) = load(&path);
        assert!(warning.is_none());
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].text, "用 pnpm");
    }

    #[test]
    fn save_roundtrip() {
        let dir = tmp("roundtrip");
        let path = dir.join(".claudesktop/memory.json");
        let mut e = entry("构建只用 cargo test -p claudesktop-core", 1_000);
        e.tags = vec!["构建".into(), "测试".into()];
        e.pinned = true;
        e.source = Some(Source { session: "s1".into(), turn: 3 });
        save(&path, &[e.clone()]).unwrap();
        let (back, warning) = load(&path);
        assert!(warning.is_none());
        assert_eq!(back.len(), 1);
        assert_eq!(back[0], e);
        // 格式和文档里的一样：camelCase 之外都是下划线字段名
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains("\"created_ms\""), "{text}");
        assert!(text.contains("\"entries\""), "{text}");
    }

    #[test]
    fn failed_save_keeps_the_old_file() {
        let dir = tmp("failsave");
        let path = dir.join("memory.json");
        save(&path, &[entry("原来那条", 1)]).unwrap();
        // 目标位置被一个目录占着 → 改名失败
        let blocked = dir.join("blocked.json");
        std::fs::create_dir_all(&blocked).unwrap();
        assert!(save(&blocked, &[entry("新的", 2)]).is_err());
        let (back, _) = load(&path);
        assert_eq!(back[0].text, "原来那条");
    }

    #[test]
    fn upsert_adds_then_updates() {
        let mut entries = vec![];
        let mut e = entry("回复一律用中文", 1_000);
        e.tags = vec!["语言".into()];
        assert!(!upsert(&mut entries, e.clone()));
        assert_eq!(entries.len(), 1);

        // 同一句话（带标点差异）再来一次：条数不变，created_ms 不动，updated_ms 变
        let mut again = entry("回复一律用中文。", 2_000);
        again.tags = vec!["语言".into()];
        again.pinned = true;
        assert!(upsert(&mut entries, again));
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].created_ms, 1_000);
        assert_eq!(entries[0].updated_ms, 2_000);
        assert!(entries[0].pinned, "更新时 pinned 只升不降");
        assert_eq!(entries[0].id, e.id);

        // 标签不同 = 另一条
        let mut other = entry("回复一律用中文", 3_000);
        other.tags = vec!["格式".into()];
        assert!(!upsert(&mut entries, other));
        assert_eq!(entries.len(), 2);
    }

    #[test]
    fn long_text_is_clipped() {
        let text = "字".repeat(MAX_TEXT_CHARS + 50);
        let mut entries = vec![];
        upsert(&mut entries, entry(&text, 1));
        assert_eq!(entries[0].text.chars().count(), MAX_TEXT_CHARS + 1);
        assert!(entries[0].text.ends_with('…'));
    }

    #[test]
    fn limits_evict_least_recently_used_but_keep_pinned() {
        let mut entries = vec![];
        for i in 0..5u64 {
            let mut e = entry(&format!("第 {i} 条"), i * 100);
            e.used_ms = i * 100;
            entries.push(e);
        }
        entries[4].pinned = true;
        entries[4].used_ms = 0; // 最久没用的也是 pinned
        enforce_limits(&mut entries, 3);
        let texts: Vec<&str> = entries.iter().map(|e| e.text.as_str()).collect();
        assert_eq!(texts, ["第 2 条", "第 3 条", "第 4 条"]);
        // 全是 pinned 时不再删
        for e in &mut entries {
            e.pinned = true;
        }
        enforce_limits(&mut entries, 1);
        assert_eq!(entries.len(), 3);
    }

    #[test]
    fn pick_orders_and_truncates() {
        let mut a = entry("普通", 1_000);
        a.updated_ms = 1_000;
        let mut b = entry("置顶", 500);
        b.pinned = true;
        let mut c = entry("最新", 2_000);
        c.updated_ms = 2_000;
        let entries = vec![a, b, c];
        let picked: Vec<&str> = pick(&entries, 100).iter().map(|e| e.text.as_str()).collect();
        assert_eq!(picked, ["置顶", "最新", "普通"]);
        // 预算 0：一条都放不下（但仍给第一条，避免整节空白）
        assert_eq!(pick(&entries, 0).len(), 1);
        // 预算 5 只放得下两条（每条按 2 字符算）
        assert_eq!(pick(&entries, 5).len(), 2);
        assert_eq!(pick(&entries, 6).len(), 3);
    }

    #[test]
    fn prompt_section_rules() {
        assert!(prompt_section(&[], &[]).is_none());
        let project = vec![entry("构建只用 cargo test -p claudesktop-core", 1)];
        let only_project = prompt_section(&project, &[]).unwrap();
        assert!(only_project.contains("cargo test"));
        assert!(!only_project.contains("## Everywhere"), "{only_project}");
        assert!(only_project.contains("instructions win"), "{only_project}");
        let global = vec![entry("回复用中文", 2)];
        let both = prompt_section(&project, &global).unwrap();
        assert!(both.contains("## This project") && both.contains("## Everywhere"), "{both}");
        // 超预算的全局记忆不进提示词
        let many: Vec<Entry> =
            (0..100).map(|i| entry(&format!("第 {i} 条{}", "很长".repeat(20)), i)).collect();
        let section = prompt_section(&[], &many).unwrap();
        assert!(section.chars().count() < GLOBAL_BUDGET + 2_000, "{}", section.chars().count());
    }

    #[test]
    fn keywords_split_ascii_and_cjk() {
        assert_eq!(keywords("Cargo test"), ["cargo", "test"]);
        // 中文按二元组切（叠字也一样）
        assert_eq!(keywords("构建只用 cargo"), ["构建", "建只", "只用", "cargo"]);
        // 全角标点切断二元组
        assert_eq!(keywords("构建，只用"), ["构建", "只用"]);
        assert!(keywords("  ").is_empty());
        assert_eq!(keywords("a"), Vec::<String>::new());
    }

    #[test]
    fn scope_names_and_limits() {
        assert_eq!(Scope::parse("global"), Some(Scope::Global));
        assert_eq!(Scope::parse("项目"), Some(Scope::Project));
        assert_eq!(Scope::parse(""), None);
        assert_eq!(Scope::Project.max_entries(), PROJECT_MAX);
        assert_eq!(Scope::Global.max_entries(), GLOBAL_MAX);
    }
}
