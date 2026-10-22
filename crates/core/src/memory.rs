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
//! 进上下文时按字符预算截取（[`pick`]），不是全塞；这一轮用户消息里提到的条目另附在
//! 用户消息尾部（[`search`] + [`context_block`]，同 `prompt_hooks` 的 `<plugin-hook-context>` 写法），
//! 系统提示词那一节保持稳定，不破坏提示缓存。

use serde::{Deserialize, Serialize};
use std::io;
use std::path::{Path, PathBuf};

/// 一条记忆的 `text` 最多多少字符（超了截断）
pub const MAX_TEXT_CHARS: usize = 400;
/// 附在用户消息尾部的 `<memory-context>` 最多多少字符
pub const CONTEXT_BUDGET: usize = 1_200;
/// 关键词命中的最低分：撞上一个词就算相关（中文二元组往往只重合一个），
/// 整串出现在正文里另加 [`CONTAINS_SCORE`] 分，排到前面
pub const MIN_SCORE: usize = 1;
/// 一条记忆最多带几个标签
pub const MAX_TAGS: usize = 6;
/// 全局记忆最多多少条（超了淘汰最久没更新的，`pinned` 不淘汰）
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
    /// 最近一次被带进上下文的时间。目前不更新（每轮写一次文件会让进 git 的项目记忆不停变）：
    /// 淘汰和进上下文都按 `updated_ms`；字段留着兼容已有文件和设计说明里的第 2 期
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

/// 现在几点（毫秒，从 1970 算）。调用方给 `Entry::new` / `mark_used` / `edit` 用，
/// 单测里换成固定值就不用等时钟。
pub fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as u64)
}

/// 项目记忆文件：项目根（[`crate::project::shared_root`]，worktree 里是主仓库根）下的
/// `.claudesktop/memory.json`。worktree 里记的东西和主仓库是同一份
/// （否则每开一个 worktree 都是一份全新的记忆）。
pub fn project_file(cwd: &Path) -> Option<PathBuf> {
    crate::project::shared_root(cwd).map(|root| project_path(&root))
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

/// 文件解析不了（格式坏了）时 [`update`] 怎么办。读不了（被占用、没权限）一律不动文件、报错。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OnBroken {
    /// 不改，报错（界面上的删除：用户能看到原因、自己去修）
    Refuse,
    /// 挪到旁边（不覆盖已有的备份），从空表开始（内核记新的一条：不能因为文件坏了就记不下）
    MoveAside,
}

/// 同一进程里所有对记忆文件的「读 → 改 → 写」排队（内核的几个会话、设置页都在这个进程里），
/// 否则两边同时改，后写的会把先写的盖掉。
static UPDATE_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// 读 → `change` → 原子写回，全程持锁。返回 `change` 的结果和一句要告诉用户的话
/// （文件坏了被挪开时）；读不了、坏了且 `Refuse`、写失败都返回 Err（文件不动）。
pub fn update<R>(
    path: &Path,
    on_broken: OnBroken,
    change: impl FnOnce(&mut Vec<Entry>) -> R,
) -> Result<(R, Option<String>), String> {
    let _guard = UPDATE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let mut notice = None;
    let mut entries = match std::fs::read_to_string(path) {
        Ok(text) if text.trim().is_empty() => Vec::new(),
        Ok(text) => match serde_json::from_str::<FileFormat>(&text) {
            Ok(f) => f.entries,
            Err(e) => {
                let reason =
                    format!("{} 的格式有问题：{}", path.display(), crate::json_error_text(&e));
                if on_broken == OnBroken::Refuse {
                    return Err(format!("{reason}（先改好这个文件，这次没有改动）"));
                }
                let aside = broken_path(path);
                std::fs::rename(path, &aside)
                    .map_err(|e| format!("{reason}；挪开它也失败了：{e}"))?;
                notice =
                    Some(format!("{reason}；原文件挪到了 {}，从空表重新开始", aside.display()));
                Vec::new()
            }
        },
        Err(e) if e.kind() == io::ErrorKind::NotFound => Vec::new(),
        Err(e) => return Err(format!("读不了 {}：{e}（这次没有改动）", path.display())),
    };
    let value = change(&mut entries);
    save(path, &entries).map_err(|e| format!("没能写进 {}：{e}", path.display()))?;
    Ok((value, notice))
}

/// 坏文件挪去的位置：`memory.json.broken`，已有就 `.broken-2`、`-3`…（不覆盖之前的备份）
fn broken_path(path: &Path) -> PathBuf {
    let first = path.with_extension("json.broken");
    if !first.exists() {
        return first;
    }
    (2..)
        .map(|n| path.with_extension(format!("json.broken-{n}")))
        .find(|p| !p.exists())
        .unwrap_or(first)
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

/// 新增或更新一条：同一 `tags` + 同一句话（只差大小写、空白、标点）视为同一条
/// （返回 true = 更新了已有的）。更新时保留 `id` / `created_ms` / `used_ms`，`pinned` 只升不降，
/// 新的没带标签时保留原来的标签。
pub fn upsert(entries: &mut Vec<Entry>, entry: Entry) -> bool {
    let entry = Entry { text: clip_text(entry.text.trim()), ..entry };
    if let Some(old) = entries
        .iter_mut()
        .find(|e| similar(&e.text, &entry.text) && same_tags(&e.tags, &entry.tags))
    {
        old.text = entry.text;
        if !entry.tags.is_empty() {
            old.tags = entry.tags;
        }
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

/// 超上限时淘汰「最久没被带进上下文」的那条（`pinned` 不动）。
///
/// 排序键取 `used_ms` 和 `updated_ms` 里大的那个：刚记下 / 刚改过的也算「最近用过」。
/// 老文件里 `used_ms` 和 `updated_ms` 一样，退化成按更新时间淘汰。
pub fn enforce_limits(entries: &mut Vec<Entry>, max: usize) {
    while entries.len() > max {
        let victim = entries
            .iter()
            .enumerate()
            .filter(|(_, e)| !e.pinned)
            .min_by_key(|(_, e)| e.used_ms.max(e.updated_ms))
            .map(|(i, _)| i);
        match victim {
            Some(i) => drop(entries.remove(i)),
            // 全是 pinned：不再删
            None => return,
        }
    }
}

/// 把「这一轮带进过上下文」的条目标成刚用过（[`enforce_limits`] 据此淘汰）。
///
/// **只在本来就要写盘的时候调**（记新的一条、界面上的增删改）：每轮都写一次会让
/// 进 git 的项目记忆一天变几十次，而淘汰只差几个字节的精度。返回改了几条。
pub fn mark_used(entries: &mut [Entry], ids: &[String], now_ms: u64) -> usize {
    let mut touched = 0;
    for entry in entries.iter_mut().filter(|e| ids.iter().any(|id| id == &e.id)) {
        entry.used_ms = now_ms;
        touched += 1;
    }
    touched
}

/// 删一条（`memory_forget` 工具、界面删除共用）。`needle` 是条目 id 或正文（不分大小写、
/// 忽略标点后相等，或者正文里包含它）。
///
/// `only_from` 有值时只删那个会话记下的条目（`source.session` 对得上）：
/// 模型只能删**自己这一轮 / 这个会话**记下的，误删别人的代价比误记大得多
/// （设计说明第十二节第 3 条）。返回删掉的条数。
pub fn forget(entries: &mut Vec<Entry>, needle: &str, only_from: Option<&str>) -> usize {
    let needle = needle.trim();
    if needle.is_empty() {
        return 0;
    }
    let key = norm(needle);
    let before = entries.len();
    entries.retain(|e| {
        let mine = only_from.is_none_or(|s| e.source.as_ref().is_some_and(|src| src.session == s));
        if !mine {
            return true;
        }
        let (same_id, same_text) =
            (e.id == needle, !key.is_empty() && norm(&e.text).contains(&key));
        !(same_id || same_text)
    });
    before - entries.len()
}

/// 改一条（界面上编辑正文 / 标签 / `pinned`）：按 id 找到，改 `text`（截到 [`MAX_TEXT_CHARS`]）
/// 和标签、`pinned`，`updated_ms` 换新的。找不到返回 false。
pub fn edit(
    entries: &mut [Entry],
    id: &str,
    text: &str,
    tags: Vec<String>,
    pinned: bool,
    now_ms: u64,
) -> bool {
    let Some(entry) = entries.iter_mut().find(|e| e.id == id) else {
        return false;
    };
    let text = clip_text(text.trim());
    if !text.is_empty() {
        entry.text = text;
    }
    entry.tags = tags
        .into_iter()
        .map(|t| t.trim().to_string())
        .filter(|t| !t.is_empty())
        .take(MAX_TAGS)
        .collect();
    entry.pinned = pinned;
    entry.updated_ms = now_ms;
    true
}

/// 进上下文的顺序：`pinned` 优先，再按「最近用过 / 改过」，截到 `budget` 字符。
pub fn pick(entries: &[Entry], budget: usize) -> Vec<&Entry> {
    pick_missed(entries, budget).0
}

/// 同 [`pick`]，另外告诉调用方还有几条没带上（提示词末尾据此让模型用 `memory_search` 找）。
pub fn pick_missed(entries: &[Entry], budget: usize) -> (Vec<&Entry>, usize) {
    let mut sorted: Vec<&Entry> = entries.iter().collect();
    sorted.sort_by(|a, b| {
        b.pinned
            .cmp(&a.pinned)
            .then(b.used_ms.max(b.updated_ms).cmp(&a.used_ms.max(a.updated_ms)))
            .then(b.updated_ms.cmp(&a.updated_ms))
    });
    let total = sorted.len();
    let mut used = 0;
    let mut out = Vec::new();
    for entry in sorted {
        let len = entry_len(entry);
        if used + len > budget && !out.is_empty() {
            break;
        }
        used += len;
        out.push(entry);
        if used >= budget {
            break;
        }
    }
    let missed = total.saturating_sub(out.len());
    (out, missed)
}

/// 条目在提示词里占多少字符（正文 + 标签）。
fn entry_len(entry: &Entry) -> usize {
    entry.text.chars().count() + entry.tags.iter().map(|t| t.chars().count() + 1).sum::<usize>()
}

/// 按关键词找条目（`memory_search` 工具、以及挑「这一轮相关」的条目共用）。
///
/// 匹配是两路：`query` 抽出的词（[`keywords`]）和条目正文 / 标签抽出的词求交集，
/// 命中的词多的在前；查询整串是条目正文的子串也直接算命中（「cargo test」这种带空格的写法）。
/// 不分大小写、不用索引——几百条的规模上几十行就够（同 `history_search`）。
pub fn ranked<'a>(
    entries: impl IntoIterator<Item = &'a Entry>,
    query: &str,
    min_score: usize,
) -> Vec<(&'a Entry, usize)> {
    let words = keywords(query);
    let needle = norm(query);
    if words.is_empty() && needle.is_empty() {
        return Vec::new();
    }
    let mut out: Vec<(&Entry, usize)> = entries
        .into_iter()
        .filter_map(|entry| {
            let mut haystack = keywords(&entry.text);
            for tag in &entry.tags {
                haystack.extend(keywords(tag));
            }
            let mut score = words.iter().filter(|w| haystack.contains(w)).count();
            // 整串命中（不分大小写、忽略标点）比凑巧撞上几个词可靠得多
            if !needle.is_empty() && norm(&entry.text).contains(&needle) {
                score += CONTAINS_SCORE;
            }
            (score >= min_score.max(1)).then_some((entry, score))
        })
        .collect();
    // 分数高的在前，同分看谁最近用过 / 改过
    out.sort_by(|a, b| {
        b.1.cmp(&a.1).then(b.0.used_ms.max(b.0.updated_ms).cmp(&a.0.used_ms.max(a.0.updated_ms)))
    });
    out
}

/// 整串出现在正文里的加分（比撞上几个词更可信）
const CONTAINS_SCORE: usize = 3;

/// 系统提示词里的 `# Memory` 一节；两份都空时返回 None。
///
/// 只放预算以内的（`pick_missed`），装不下的条数写在末尾。这一节要保持稳定（提示缓存），
/// 所以「这一轮相关、但这里没带上的」另走 [`context`] 附在用户消息尾部。
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
    let mut missed = 0;
    let (project_picks, project_missed) = pick_missed(project, PROJECT_BUDGET);
    if !project_picks.is_empty() {
        out.push_str("\n\n## This project\n");
        for entry in project_picks {
            out.push_str(&entry.prompt_line());
            out.push('\n');
        }
    }
    missed += project_missed;
    let (global_picks, global_missed) = pick_missed(global, GLOBAL_BUDGET);
    if !global_picks.is_empty() {
        out.push_str("\n## Everywhere\n");
        for entry in global_picks {
            out.push_str(&entry.prompt_line());
            out.push('\n');
        }
    }
    missed += global_missed;
    if missed > 0 {
        // 预算装不下的那些不是没了：告诉模型还能找回来（不然它会以为只记着上面几条）
        out.push_str(&format!(
            "\n{missed} more notes did not fit here; use `memory_search` to look them up when the \
             topic comes up."
        ));
    }
    Some(out.trim_end().to_string())
}

/// 系统提示词那一节里**已经出现**的条目 id（`<memory-context>` 不重复附它们）。
pub fn shown_ids(project: &[Entry], global: &[Entry]) -> Vec<String> {
    pick(project, PROJECT_BUDGET)
        .into_iter()
        .chain(pick(global, GLOBAL_BUDGET))
        .map(|e| e.id.clone())
        .collect()
}

/// 附在**这条用户消息尾部**的相关记忆（`<memory-context>`）：和 `# Memory` 一节分开，
/// 系统提示词保持稳定，不破坏提示缓存（同 `prompt_hooks` 的 `<plugin-hook-context>`）。
///
/// `exclude` 是系统提示词里已经出现过的条目 id，这里不重复；`query` 是这条用户消息。
/// 没有相关条目、或消息是空的，返回 None。
pub fn context_block(
    project: &[Entry],
    global: &[Entry],
    query: &str,
    exclude: &[&str],
) -> Option<String> {
    context(project, global, query, exclude).0
}

/// 同 [`context_block`]，另外给出进了这一节的条目 id（内核把它们记成「这一轮用过的」）。
pub fn context(
    project: &[Entry],
    global: &[Entry],
    query: &str,
    exclude: &[&str],
) -> (Option<String>, Vec<String>) {
    let candidates: Vec<&Entry> =
        project.iter().chain(global).filter(|e| !exclude.contains(&e.id.as_str())).collect();
    let mut used = 0;
    let mut picks: Vec<&Entry> = Vec::new();
    for (entry, _) in ranked(candidates, query, MIN_SCORE) {
        let len = entry_len(entry);
        if used + len > CONTEXT_BUDGET && !picks.is_empty() {
            break;
        }
        used += len;
        picks.push(entry);
        if used >= CONTEXT_BUDGET {
            break;
        }
    }
    if picks.is_empty() {
        return (None, Vec::new());
    }
    let ids = picks.iter().map(|e| e.id.clone()).collect();
    let lines: Vec<String> = picks.iter().map(|e| e.prompt_line()).collect();
    (
        Some(format!(
            "<memory-context>\nNotes from earlier sessions that may bear on this message \
             (background only: the instructions above win). Do not restate them.\n{}\n\
             </memory-context>",
            lines.join("\n")
        )),
        ids,
    )
}

/// 抽词（ASCII 词 + 中文二元组，都转小写）：`ranked` 的关键词匹配、`memory_search` 都用它。
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

/// 两句话是不是同一条：规范形（小写、去掉空白和标点）相同。
/// 不按子串算：「用 pnpm」和「不用 pnpm」只差一个否定词，意思正相反。
fn similar(a: &str, b: &str) -> bool {
    let (a, b) = (norm(a), norm(b));
    !a.is_empty() && a == b
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

    /// worktree 和主仓库共用一份项目记忆：在 worktree 里记的，回到主目录也在。
    #[test]
    fn worktree_shares_the_main_repo_memory_file() {
        let dir = tmp("worktree");
        let main = dir.join("main");
        let wt = main.join("b1");
        std::fs::create_dir_all(main.join(".git/worktrees/b1")).unwrap();
        std::fs::create_dir_all(wt.join("src")).unwrap();
        std::fs::write(
            wt.join(".git"),
            format!("gitdir: {}\n", main.join(".git/worktrees/b1").display()),
        )
        .unwrap();
        let expected = project_path(&main);
        assert_eq!(project_file(&wt), Some(expected.clone()));
        assert_eq!(project_file(&wt.join("src")), Some(expected.clone()));
        assert_eq!(project_file(&main), Some(expected.clone()));
        let _ = std::fs::remove_dir_all(&dir);
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

    /// 只差一个否定词的两句话意思相反，不能合成一条
    #[test]
    fn negations_are_separate_notes() {
        let mut entries = vec![];
        upsert(&mut entries, entry("用 pnpm", 1));
        assert!(!upsert(&mut entries, entry("不用 pnpm", 2)));
        assert!(!upsert(&mut entries, entry("never use tabs", 3)));
        assert!(!upsert(&mut entries, entry("use tabs", 4)));
        assert_eq!(entries.len(), 4);
    }

    /// 同一句话再记一次但没带标签：原来的标签留着
    #[test]
    fn untagged_repeat_keeps_the_tags() {
        let mut entries = vec![];
        let mut e = entry("构建只用 cargo", 1);
        e.tags = vec!["构建".into(), "测试".into()];
        upsert(&mut entries, e);
        assert!(upsert(&mut entries, entry("构建只用 cargo。", 2)));
        assert_eq!(entries[0].tags, ["构建", "测试"]);
    }

    /// 读不了的文件不动；格式坏了按 `OnBroken` 处理，挪开时不覆盖已有的备份
    #[test]
    fn update_never_clobbers_unreadable_or_broken_files() {
        let dir = tmp("update");
        // 读不了（这里是个目录）：报错，什么都不动
        let unreadable = dir.join("dir.json");
        std::fs::create_dir_all(&unreadable).unwrap();
        assert!(update(&unreadable, OnBroken::MoveAside, |e| e.push(entry("新", 1))).is_err());
        assert!(unreadable.is_dir());

        let path = dir.join("memory.json");
        std::fs::write(&path, "{ broken").unwrap();
        assert!(update(&path, OnBroken::Refuse, |e| e.clear()).is_err());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "{ broken");

        let (_, notice) =
            update(&path, OnBroken::MoveAside, |e| e.push(entry("第一条", 1))).unwrap();
        assert!(notice.unwrap().contains("memory.json.broken"));
        std::fs::write(&path, "{ broken again").unwrap();
        update(&path, OnBroken::MoveAside, |e| e.push(entry("第二条", 2))).unwrap();
        assert_eq!(
            std::fs::read_to_string(path.with_extension("json.broken")).unwrap(),
            "{ broken"
        );
        assert_eq!(
            std::fs::read_to_string(path.with_extension("json.broken-2")).unwrap(),
            "{ broken again"
        );
        assert_eq!(load(&path).0[0].text, "第二条");
    }

    /// 几个线程同时记：一条都不丢
    #[test]
    fn concurrent_updates_keep_every_entry() {
        let dir = tmp("concurrent");
        let path = dir.join("memory.json");
        let threads: Vec<_> = (0..8)
            .map(|i| {
                let path = path.clone();
                std::thread::spawn(move || {
                    update(&path, OnBroken::Refuse, |e| {
                        upsert(e, entry(&format!("第 {i} 条"), i));
                    })
                    .unwrap();
                })
            })
            .collect();
        for t in threads {
            t.join().unwrap();
        }
        assert_eq!(load(&path).0.len(), 8);
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
    fn limits_evict_least_recently_updated_but_keep_pinned() {
        let mut entries = vec![];
        for i in 0..5u64 {
            entries.push(entry(&format!("第 {i} 条"), i * 100));
        }
        entries[4].pinned = true;
        entries[4].updated_ms = 0; // 最久没更新的也是 pinned
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

    /// `pick` 按「最近用过 / 改过」排，超预算的条数报出来（提示词末尾据此提示 `memory_search`）
    #[test]
    fn pick_reports_what_did_not_fit() {
        let mut a = entry("用过很久以前", 100);
        a.used_ms = 9_000;
        let mut b = entry("刚更新", 8_000);
        b.used_ms = 0;
        let entries = vec![a, b];
        let (picked, missed) = pick_missed(&entries, 100);
        assert_eq!(picked[0].text, "用过很久以前");
        assert_eq!(missed, 0);
        let (picked, missed) = pick_missed(&entries, 2);
        assert_eq!(picked.len(), 1);
        assert_eq!(missed, 1);
        // 提示词里说明还有几条没带上，怎么找回来
        let section = prompt_section(&[], &entries).unwrap();
        assert!(!section.contains("memory_search"), "{section}");
        let many: Vec<Entry> =
            (0..50).map(|i| entry(&format!("第 {i} 条{}", "长".repeat(60)), i)).collect();
        let section = prompt_section(&[], &many).unwrap();
        assert!(section.contains("memory_search"), "{section}");
    }

    /// 淘汰按 `used_ms` 和 `updated_ms` 里大的那个：刚被带进上下文的不淘汰
    #[test]
    fn used_time_protects_entries_from_eviction() {
        let mut entries = vec![];
        for i in 0..3u64 {
            entries.push(entry(&format!("第 {i} 条"), i * 100));
        }
        // 最老的那条刚被带进上下文
        let ids = vec![entries[0].id.clone()];
        assert_eq!(mark_used(&mut entries, &ids, 5_000), 1);
        assert_eq!(entries[0].used_ms, 5_000);
        enforce_limits(&mut entries, 2);
        let texts: Vec<&str> = entries.iter().map(|e| e.text.as_str()).collect();
        assert_eq!(texts, ["第 0 条", "第 2 条"]);
    }

    #[test]
    fn ranked_matches_words_and_whole_phrases() {
        let mut builds = entry("构建只用 cargo test -p claudesktop-core，全量太慢", 1);
        builds.tags = vec!["构建".into()];
        let chinese = entry("回复一律用中文", 2);
        let unrelated = entry("日志一律走 tracing", 3);
        let entries = vec![builds, chinese, unrelated];
        // 中文二元组命中
        let hits = ranked(&entries, "构建", 1);
        assert_eq!(hits[0].0.text, "构建只用 cargo test -p claudesktop-core，全量太慢");
        // 整串（带空格）命中加分
        let hits = ranked(&entries, "cargo test", 1);
        assert_eq!(hits[0].0.text, "构建只用 cargo test -p claudesktop-core，全量太慢");
        // 大小写不敏感
        assert_eq!(ranked(&entries, "TRACING", 1).len(), 1);
        // 没有交集：什么都没匹配上
        assert!(ranked(&entries, "python", 1).is_empty());
        assert!(ranked(&entries, "", 1).is_empty());
        // 门槛：整串命中加 3 分，「构建」对那条构建记忆是 1 + 3 = 4 分
        assert_eq!(ranked(&entries, "中文", MIN_SCORE).len(), 1);
        assert_eq!(ranked(&entries, "构建", 4).len(), 1);
        assert!(ranked(&entries, "构建", 5).is_empty());
    }

    /// `<memory-context>`：只放相关的、系统提示词里已经有的不重复；太泛的不附
    #[test]
    fn context_block_only_carries_relevant_new_notes() {
        let mut builds = entry("构建只用 cargo test -p claudesktop-core，全量太慢", 1);
        builds.tags = vec!["构建".into()];
        let chinese = entry("回复一律用中文", 2);
        let project = vec![builds.clone()];
        let global = vec![chinese.clone()];
        let block = context_block(&project, &global, "这次构建怎么跑？", &[]).unwrap();
        assert!(block.starts_with("<memory-context>"), "{block}");
        assert!(block.contains("cargo test"), "{block}");
        // 已经在系统提示词里的那条：没有再可附的了
        assert!(
            context_block(&project, &global, "这次构建怎么跑？", &[builds.id.as_str()]).is_none()
        );
        // 没关系的话不附
        assert!(context_block(&project, &global, "今天天气怎么样", &[]).is_none());
        assert!(context_block(&[], &[], "构建", &[]).is_none()); // 太长时截到预算
        let many: Vec<Entry> =
            (0..60).map(|i| entry(&format!("构建第 {i} 条{}", "很长".repeat(30)), i)).collect();
        let block = context_block(&many, &[], "构建", &[]).unwrap();
        assert!(block.chars().count() < CONTEXT_BUDGET + 400, "{}", block.chars().count());
    }

    /// `memory_forget` 只删本会话记下的；界面删除（`only_from` 为 None）不限来源
    #[test]
    fn forget_is_scoped_to_the_session_that_saved_it() {
        let mut a = entry("构建用 pnpm", 1);
        a.source = Some(Source { session: "s1".into(), turn: 1 });
        let mut b = entry("回复用中文", 2);
        b.source = Some(Source { session: "s2".into(), turn: 3 });
        let mut entries = vec![a, b];
        assert_eq!(forget(&mut entries, "构建用 pnpm", Some("s1")), 1);
        assert_eq!(entries.len(), 1);
        // 别的会话记下的：这条会话删不掉
        assert_eq!(forget(&mut entries, "回复用中文", Some("s1")), 0);
        assert_eq!(entries.len(), 1);
        // 界面删除不限来源
        assert_eq!(forget(&mut entries, "回复用中文", None), 1);
        assert!(entries.is_empty());
        // 空查询什么都不删
        assert_eq!(forget(&mut entries, "  ", None), 0);
    }

    #[test]
    fn edit_changes_text_tags_and_pin() {
        let mut a = entry("原来的话", 1);
        a.tags = vec!["旧".into()];
        let mut entries = vec![a];
        let id = entries[0].id.clone();
        assert!(edit(&mut entries, &id, " 改过的话 ", vec!["新".into()], true, 2_000));
        assert_eq!(entries[0].text, "改过的话");
        assert_eq!(entries[0].tags, ["新"]);
        assert!(entries[0].pinned);
        assert_eq!(entries[0].updated_ms, 2_000);
        assert_eq!(entries[0].created_ms, 1);
        assert!(!edit(&mut entries, "没这个 id", "x", vec![], false, 3));
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
