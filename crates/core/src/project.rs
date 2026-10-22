//! 项目级配置：项目根（会话工作目录）下 `.claudesktop/settings.json`。
//!
//! 这是一层**轻覆盖**——全局设置（数据目录的 settings.json）永远在，项目文件只写
//! 和全局不一样的字段，生效规则是「项目值优先」：
//! - `model`：这个项目默认用的模型 ID（要存在于全局模型列表里，否则忽略并提示）
//! - `permissions`：`{"allow": ["edit", "shell:git status"], "deny": ["shell:rm"]}`，
//!   和权限卡片落盘的规则同格式，合并进同一张表（项目里写的规则对整个项目目录生效）
//! - `mcpDisabled`：这个项目里要停用的 MCP 服务器名列表
//! - `pluginsEnabled`：这个项目启用的插件列表（空 / 缺省 = 跟全局）
//! - `instructions`：项目说明文件的路径（相对项目根；缺省还是 AGENTS.md / CLAUDE.md）
//! - `worktree`：建 worktree 后要跟着过去的东西，见 [`WorktreeConfig`]
//!
//! 读取发生在开自研内核会话时；文件坏了给能照着改的中文错误，按没有项目配置处理。

use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::path::{Path, PathBuf};

/// 项目根的配置目录名（`.claudesktop`，项目设置和项目记忆都放这里）。
pub const DIR: &str = ".claudesktop";

/// 配置文件的完整路径：`root/.claudesktop/settings.json`（分层拼接，Win 上也是 `\`）。
pub fn path_in(root: &Path) -> PathBuf {
    root.join(DIR).join("settings.json")
}

/// 拒绝规则的前缀（和 `claudesktop_builtin::permissions::DENY_PREFIX` 同值；
/// core 不能依赖 builtin，这里各自留一份）。
const DENY: &str = "deny:";

/// 解析好的项目配置。
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ProjectConfig {
    /// 这个项目默认的模型 ID
    pub model: Option<String>,
    /// 权限规则：`edit` / `shell:git status` / `deny:…`（和 permissions.json 同格式）
    pub permissions: Vec<String>,
    /// 要停用的 MCP 服务器名
    pub mcp_disabled: Vec<String>,
    /// 启用的插件名（None = 跟全局）
    pub plugins_enabled: Option<Vec<String>>,
    /// 项目说明文件（相对项目根）
    pub instructions: Option<PathBuf>,
    /// 建 worktree 之后做的事（复制未跟踪文件、跑初始化命令）
    pub worktree: WorktreeConfig,
    /// 读文件时发现的问题（不影响可用字段的生效）
    pub warnings: Vec<String>,
    /// 文件解析不了（不是 JSON、字段类型不对）：可用字段都不生效，[`save`] 也不会覆盖它
    pub broken: bool,
}

#[derive(Debug, Default, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct FileFormat {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    permissions: Option<Permissions>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    mcp_disabled: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    plugins_enabled: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    instructions: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    worktree: Option<WorktreeFormat>,
    /// 不认识的键（`$schema`、以后的新字段、手写的备注）：读时提示，写回时原样留着
    #[serde(flatten)]
    other: serde_json::Map<String, Value>,
}

/// `worktree` 一栏的文件格式：`{"copy": [".env*", "data/"], "setup": "npm install"}`。
#[derive(Debug, Default, Deserialize, Serialize)]
struct WorktreeFormat {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    copy: Vec<String>,
    /// 初始化命令；空串（或只有空白）= 不跑
    #[serde(default, skip_serializing_if = "Option::is_none")]
    setup: Option<String>,
}

/// 编辑器常用的 `$schema` 键：认得，不提示
const SCHEMA_KEY: &str = "$schema";

#[derive(Debug, Default, Deserialize, Serialize)]
struct Permissions {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    allow: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    deny: Vec<String>,
}

/// 项目根；还没有任何标记（不是 git 仓库、也没有 `.claudesktop`）时就是工作目录本身。
/// 界面上的「项目」页要一个确定的根（保存时写进哪里），用这个。
pub fn root_or(cwd: &Path) -> &Path {
    project_root(cwd).unwrap_or(cwd)
}

/// 项目根（向上找几层 `.claudesktop` / `.git`，工作目录本身优先）。
/// worktree 里的 `.git` 是**文件**（`gitdir: <主仓库>/.git/worktrees/<名>`），也算项目根：
/// 项目配置、说明文件读到的是**这个分支上的版本**，和代码一致。
/// submodule 的 `.git` 也是文件，但它**不算**：在 submodule 里干活仍归外面那个项目（配置、规则照旧）。
pub fn project_root(cwd: &Path) -> Option<&Path> {
    climb(cwd, |d| path_in(d).is_file() || d.join(".git").is_dir() || is_worktree_checkout(d))
}

/// `dir` 是不是一个 git worktree 的检出目录（`.git` 文件指向 `<主仓库>/.git/worktrees/<名>`）。
fn is_worktree_checkout(dir: &Path) -> bool {
    dir.join(".git").is_file() && gitdir_target(dir).and_then(|g| main_repo_of(&g)).is_some()
}

/// 共享根：项目记忆、权限规则这些「对这个项目」的东西按它算。
/// 工作目录在 git worktree 里时是**主仓库根**（在 worktree 里记下的事回到主目录也算数，反之亦然），
/// 其余情况和 [`project_root`] 一样。找不到项目根时为 None。
pub fn shared_root(cwd: &Path) -> Option<PathBuf> {
    let root = project_root(cwd)?;
    Some(
        gitdir_target(root)
            .and_then(|gitdir| main_repo_of(&gitdir))
            .unwrap_or_else(|| root.to_path_buf()),
    )
}

/// `.git` 是文件（worktree / submodule）时，它指向的 git 目录（纯文件解析，不调 git）。
/// `.git` 是目录（普通仓库）或没写 `gitdir:` 时返回 None。
pub fn gitdir_target(root: &Path) -> Option<PathBuf> {
    let text = std::fs::read_to_string(root.join(".git")).ok()?;
    let target = text.trim().strip_prefix("gitdir:")?.trim();
    if target.is_empty() {
        return None;
    }
    let target = PathBuf::from(target);
    // git 写的是绝对路径，个别情况（`core.worktree`、手写的）是相对项目根的
    let joined = if target.is_absolute() { target } else { root.join(target) };
    Some(lexical(&joined))
}

/// 词法上消掉 `.` / `..`（不碰磁盘：这些路径可能已经不在，比如被手动删掉的 worktree）。
fn lexical(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for part in path.components() {
        match part {
            std::path::Component::CurDir => {}
            std::path::Component::ParentDir => {
                // 盘符 / 根上没法再往上：留着，别把路径弄成相对的
                if !out.pop() {
                    out.push("..");
                }
            }
            other => out.push(other),
        }
    }
    out
}

/// `<主仓库>/.git/worktrees/<名>` → `<主仓库>`；不是 worktree 的 git 目录（submodule 等）返回 None。
fn main_repo_of(gitdir: &Path) -> Option<PathBuf> {
    gitdir.file_name()?;
    let worktrees = gitdir.parent().filter(|p| p.file_name() == Some("worktrees".as_ref()))?;
    let dot_git = worktrees.parent().filter(|p| p.file_name() == Some(".git".as_ref()))?;
    dot_git.parent().map(Path::to_path_buf)
}

/// 从 `cwd` 往上最多 4 层找 `is_root` 为真的目录（`cwd` 本身优先）。
/// 判断单独抽出来，测试不用依赖临时目录在不在仓库里。
fn climb(cwd: &Path, is_root: impl Fn(&Path) -> bool) -> Option<&Path> {
    let mut dir = Some(cwd);
    for _ in 0..4 {
        let d = dir?;
        if is_root(d) {
            return Some(d);
        }
        dir = d.parent();
    }
    None
}

/// 工作目录在主仓库里对应的那个目录：worktree 里的 `<worktree>/crates/x` → `<主仓库>/crates/x`，
/// 其余情况就是工作目录本身。权限规则按它记，在 worktree 里点的「始终允许」管的范围
/// 和在主目录同一个子目录里点的一样大（不会因为在 worktree 里就放大到整个仓库）。
pub fn shared_dir(cwd: &Path) -> PathBuf {
    if let Some(root) = project_root(cwd)
        && let Some(main) = gitdir_target(root).and_then(|g| main_repo_of(&g))
    {
        return match cwd.strip_prefix(root) {
            Ok(rel) if !rel.as_os_str().is_empty() => main.join(rel),
            _ => main,
        };
    }
    cwd.to_path_buf()
}

/// 建 worktree 之后要做的两件事（项目配置的 `worktree` 一栏，见「设计说明-worktree.md」第七节）：
///
/// - `copy`：从主目录复制过去的文件 / glob，典型是 `.env*`。未跟踪文件（`.env`、依赖目录）
///   不会跟着 `git worktree add` 过去，而 `.env` 缺了很多项目根本跑不起来。
/// - `setup`：初始化命令（如 `npm install`），在 worktree 里跑一遍，输出进对话里的一张工具卡。
///
/// 两项都没配时什么都不做，界面上也不提。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct WorktreeConfig {
    /// 要复制过去的文件 / 目录，相对主仓库根的 glob（`*` / `?` / `**` 按段匹配；
    /// 以 `/` 结尾表示只要目录）。写绝对路径按主仓库相对算不了，会进警告。
    pub copy: Vec<String>,
    /// 建好之后在 worktree 里跑的初始化命令
    pub setup: Option<String>,
}

impl WorktreeConfig {
    /// 什么都没配：建 worktree 时不用多做任何事。
    pub fn is_empty(&self) -> bool {
        self.copy.iter().all(|c| c.trim().is_empty()) && self.setup.is_none()
    }

    /// 规整一下：去掉空白项、`setup` 去掉首尾空白（空串当没配）。
    pub fn normalized(&self) -> Self {
        Self {
            copy: self
                .copy
                .iter()
                .map(|c| c.trim().to_string())
                .filter(|c| !c.is_empty())
                .collect(),
            setup: self.setup.as_ref().map(|s| s.trim().to_string()).filter(|s| !s.is_empty()),
        }
    }
}

/// 从模板列出一条复制规则在 `repo` 下匹配到的路径（相对 `repo`），排序去重。
/// 规则里 `*` / `?` 在**一段路径**内匹配，`**` 匹配任意层；以 `/` 结尾的只看目录，
/// 其余文件和目录都算。写绝对路径的规则直接跳过（[`WorktreeConfig::copy`] 说的是相对路径）。
pub fn copy_matches(repo: &Path, pattern: &str) -> Vec<PathBuf> {
    let pattern = pattern.trim().trim_start_matches("./").replace('\\', "/");
    let pattern = pattern.trim_matches('/');
    if pattern.is_empty() || Path::new(pattern).is_absolute() {
        return Vec::new();
    }
    let dir_only = pattern.ends_with('/') || pattern.ends_with("/*");
    let pattern = pattern.trim_end_matches('/');
    let segments: Vec<&str> = pattern.split('/').filter(|s| !s.is_empty()).collect();

    let mut out = Vec::new();
    let mut dirs = vec![repo.to_path_buf()];
    for (i, segment) in segments.iter().enumerate() {
        let last = i + 1 == segments.len();
        let mut next = Vec::new();
        for dir in &dirs {
            // `**`：从这一层往下所有目录都算（`**/x` 的 `x` 在每一层里找）
            let deep = *segment == "**";
            let bases: Vec<PathBuf> =
                if deep { walk_dirs(dir.to_path_buf()) } else { vec![dir.clone()] };
            for base in bases {
                for entry in std::fs::read_dir(&base).into_iter().flatten().flatten() {
                    let name = entry.file_name().to_string_lossy().to_string();
                    // `.git`（目录或 worktree 里的文件）哪一层都不算：`.*` 也不能把它带过去
                    if name == ".git" {
                        continue;
                    }
                    let path = entry.path();
                    if deep {
                        // `**` 这一层不消耗路径段：把子目录继续交给后面的段
                        if is_real_dir(&path) {
                            next.push(path);
                        }
                        continue;
                    }
                    if !glob_match(segment, &name) {
                        continue;
                    }
                    if last {
                        if !dir_only || path.is_dir() {
                            out.push(path);
                        }
                    } else if is_real_dir(&path) {
                        // 往下找只走真目录：指向上层的链接（pnpm、monorepo）会绕成环
                        next.push(path);
                    }
                }
            }
        }
        match next.is_empty() {
            true => break,
            false => dirs = next,
        }
    }
    out.sort();
    out.dedup();
    out
}

/// `dir` 自己和它下面所有子目录（`**` 用；跳过 `.git`，它不该被复制过去）。
fn walk_dirs(dir: PathBuf) -> Vec<PathBuf> {
    let mut out = vec![dir.clone()];
    let mut queue = vec![dir];
    while let Some(dir) = queue.pop() {
        for entry in std::fs::read_dir(&dir).into_iter().flatten().flatten() {
            if entry.file_name() == ".git" {
                continue;
            }
            // 不跟着符号链接走：链回上层的目录会让这里永远走不完
            if is_real_dir(&entry.path()) {
                out.push(entry.path());
                queue.push(entry.path());
            }
        }
    }
    out
}

/// 是目录而且不是符号链接。
fn is_real_dir(path: &Path) -> bool {
    std::fs::symlink_metadata(path).is_ok_and(|m| m.is_dir())
}

/// 一段路径的简单 glob：`*` 任意字符、`?` 单个字符，其余按字面比（区分大小写之外都按原样）。
fn glob_match(pattern: &str, text: &str) -> bool {
    fn go(p: &[char], t: &[char]) -> bool {
        match p.split_first() {
            None => t.is_empty(),
            Some(('*', rest)) => (0..=t.len()).any(|i| go(rest, &t[i..])),
            Some(('?', rest)) => !t.is_empty() && go(rest, &t[1..]),
            Some((c, rest)) => t.first() == Some(c) && go(rest, &t[1..]),
        }
    }
    let p: Vec<char> = pattern.chars().collect();
    let t: Vec<char> = text.chars().collect();
    go(&p, &t)
}

/// 把一条匹配到的文件 / 目录从 `repo` 复制到 `worktree`（同名覆盖；目录递归复制）。
/// 返回复制了几个条目（目录按里面有几个文件算）。
///
/// 目标路径上不能有符号链接：分支里提交了 `config -> /etc/x` 时，往 `config/` 里写就写到 worktree 外面去了。
pub fn copy_into(repo: &Path, worktree: &Path, relative: &Path) -> Result<usize, String> {
    let from = repo.join(relative);
    let mut to = worktree.to_path_buf();
    for part in relative.components() {
        to.push(part);
        refuse_symlink(&to)?;
    }
    copy_path(&from, &to, true)
}

/// 目标已经是符号链接时不往里写（写进去等于写到链接指向的地方）。
fn refuse_symlink(to: &Path) -> Result<(), String> {
    match std::fs::symlink_metadata(to) {
        Ok(m) if m.file_type().is_symlink() => {
            Err(format!("{} 在 worktree 里是符号链接，没往里复制", to.display()))
        }
        _ => Ok(()),
    }
}

/// 递归复制：普通文件按字节复制（保留权限位）；目录先建再逐个孩子。
/// 符号链接：规则直接点名的那一项（`follow`）按目标复制内容，避免把主仓库的路径带进 worktree；
/// 目录里面的链接到目录的不跟（会绕成环，或把仓库外的大目录整份拷进来），链接到文件的照样复制内容。
fn copy_path(from: &Path, to: &Path, follow: bool) -> Result<usize, String> {
    let meta =
        std::fs::symlink_metadata(from).map_err(|e| format!("读不了 {}：{e}", from.display()))?;
    let meta = if meta.is_symlink() {
        let target =
            std::fs::metadata(from).map_err(|e| format!("读不了 {}：{e}", from.display()))?;
        if target.is_dir() && !follow {
            return Ok(0);
        }
        target
    } else {
        meta
    };
    refuse_symlink(to)?;
    if meta.is_dir() {
        std::fs::create_dir_all(to).map_err(|e| format!("建不了 {}：{e}", to.display()))?;
        let mut n = 0;
        for entry in
            std::fs::read_dir(from).map_err(|e| format!("读不了 {}：{e}", from.display()))?
        {
            let entry = entry.map_err(|e| format!("读不了 {}：{e}", from.display()))?;
            if entry.file_name() == ".git" {
                continue;
            }
            n += copy_path(&entry.path(), &to.join(entry.file_name()), false)?;
        }
        return Ok(n);
    }
    if let Some(parent) = to.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("建不了 {}：{e}", parent.display()))?;
    }
    std::fs::copy(from, to).map_err(|e| format!("复制不了 {}：{e}", from.display()))?;
    Ok(1)
}

/// 读项目配置；没有文件返回 None，文件坏了返回带警告的默认（还能用）。
pub fn load(cwd: &Path) -> Option<ProjectConfig> {
    let root = project_root(cwd)?;
    let path = path_in(root);
    let text = std::fs::read_to_string(&path).ok()?;
    let mut config = ProjectConfig::default();
    let parsed: Result<FileFormat, _> = serde_json::from_str(&text);
    match parsed {
        Ok(f) => {
            config.model = f.model.filter(|m| !m.trim().is_empty()).map(|m| m.trim().to_string());
            if let Some(p) = f.permissions {
                let mut rules = p.allow;
                rules.extend(p.deny.iter().map(|d| format!("deny:{d}")));
                config.permissions = rules;
            }
            config.mcp_disabled = f.mcp_disabled.unwrap_or_default();
            config.plugins_enabled = f.plugins_enabled;
            config.instructions =
                f.instructions.filter(|s| !s.trim().is_empty()).map(PathBuf::from);
            if let Some(w) = f.worktree {
                let setup = w.setup.unwrap_or_default();
                config.worktree = WorktreeConfig { copy: w.copy, setup: Some(setup) }.normalized();
            }
            for key in f.other.keys().filter(|k| *k != SCHEMA_KEY) {
                config.warnings.push(format!(
                    "{} 里有不认识的字段 `{key}`（忽略；保存时原样留着）",
                    path.display()
                ));
            }
        }
        Err(e) => {
            config.broken = true;
            config.warnings.push(format!(
                "{} 的格式有问题：{}（这个文件先按不存在处理，可用字段都不生效）",
                path.display(),
                crate::json_error_text(&e)
            ));
        }
    }
    Some(config)
}

/// 写回项目配置（原子写）：只写和全局不一样的字段，全空就删掉这个文件。
/// 返回文件位置；`warnings` 不落盘。文件里不认识的键原样留着；
/// 文件解析不了时不写（会把手写的内容整个盖掉），返回原因。
pub fn save(root: &Path, config: &ProjectConfig) -> Result<PathBuf, String> {
    let path = path_in(root);
    let mut file = FileFormat::from(config);
    match std::fs::read_to_string(&path) {
        Ok(text) if !text.trim().is_empty() => match serde_json::from_str::<FileFormat>(&text) {
            Ok(old) => file.other = old.other,
            Err(e) => {
                return Err(format!(
                    "{} 的格式有问题：{}（先改好这个文件，这次没有写）",
                    path.display(),
                    crate::json_error_text(&e)
                ));
            }
        },
        Ok(_) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(format!("读不了 {}：{e}（这次没有写）", path.display())),
    }
    if file.is_empty() {
        // 一个字段都没写：不留一个空对象在仓库里
        return match std::fs::remove_file(&path) {
            Ok(()) => Ok(path),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(path),
            Err(e) => Err(format!("删不掉 {}：{e}", path.display())),
        };
    }
    let mut text = serde_json::to_string_pretty(&file).map_err(|e| e.to_string())?;
    text.push('\n');
    if let Some(dir) = path.parent().filter(|d| !d.as_os_str().is_empty()) {
        std::fs::create_dir_all(dir).map_err(|e| format!("建不了 {}：{e}", dir.display()))?;
    }
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, text).map_err(|e| format!("写不了 {}：{e}", tmp.display()))?;
    std::fs::rename(&tmp, &path).map_err(|e| format!("写不了 {}：{e}", path.display()))?;
    Ok(path)
}

impl FileFormat {
    /// 解析好的配置 → 文件里那几个字段（`allow` / `deny` 拆回去）。
    fn from(config: &ProjectConfig) -> Self {
        let (allow, deny) = split_rules(&config.permissions);
        let permissions = (config.permissions.iter().any(|r| !r.trim().is_empty()))
            .then_some(Permissions { allow, deny });
        Self {
            model: config.model.clone(),
            permissions,
            mcp_disabled: (!config.mcp_disabled.is_empty()).then(|| config.mcp_disabled.clone()),
            plugins_enabled: config.plugins_enabled.clone(),
            instructions: config
                .instructions
                .as_ref()
                .map(|p| p.display().to_string())
                .filter(|s| !s.trim().is_empty()),
            worktree: {
                let w = config.worktree.normalized();
                (!w.is_empty()).then_some(WorktreeFormat { copy: w.copy, setup: w.setup })
            },
            other: serde_json::Map::new(),
        }
    }

    fn is_empty(&self) -> bool {
        self.model.is_none()
            && self.permissions.is_none()
            && self.mcp_disabled.is_none()
            && self.plugins_enabled.is_none()
            && self.instructions.is_none()
            && self.worktree.is_none()
            && self.other.is_empty()
    }
}

/// 规则列表按前缀分成「允许 / 拒绝」两栏（写回时用）。
fn split_rules(rules: &[String]) -> (Vec<String>, Vec<String>) {
    let mut allow = Vec::new();
    let mut deny = Vec::new();
    for rule in rules {
        let rule = rule.trim();
        if rule.is_empty() {
            continue;
        }
        match rule.strip_prefix(DENY) {
            Some(rest) => deny.push(rest.to_string()),
            None => allow.push(rule.to_string()),
        }
    }
    (allow, deny)
}

/// 把项目里的权限规则合并进权限文件的内存表示（`permissions.json` 的 JSON 值）：
/// 项目规则加在这个项目的键下（已有同名的去重，项目值在后不覆盖——规则是集合语义）。
/// 返回改过的 JSON（没变返回 None）。
pub fn merge_permissions(file: &Value, root: &Path, config: &ProjectConfig) -> Option<Value> {
    if config.permissions.is_empty() {
        return None;
    }
    let mut file = file.clone();
    if !file.is_object() {
        file = serde_json::json!({});
    }
    let key = root.display().to_string();
    let projects = file
        .as_object_mut()?
        .entry("projects")
        .or_insert_with(|| Value::Object(Default::default()));
    let rules = projects.as_object_mut()?.entry(key).or_insert_with(|| Value::Array(Vec::new()));
    if !rules.is_array() {
        *rules = Value::Array(Vec::new());
    }
    let arr = rules.as_array_mut()?;
    let mut changed = false;
    for rule in &config.permissions {
        if !arr.iter().any(|r| r.as_str() == Some(rule.as_str())) {
            arr.push(Value::String(rule.clone()));
            changed = true;
        }
    }
    changed.then_some(file)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(name: &str) -> PathBuf {
        // 和 `git.rs` 的测试一样：按「名字 + 时间戳」建唯一目录。不能用「进程号」——并发跑的测试
        // 同进程号，另一个测试刚把它删掉时这里会撞上正在被删的目录（Windows 上报「拒绝访问」）。
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_nanos());
        let dir = std::env::temp_dir()
            .join(format!("claudesktop-project-{name}-{}-{stamp}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn finds_project_root_by_marker() {
        let dir = tmp("root");
        let nested = dir.join("a/b");
        std::fs::create_dir_all(&nested).unwrap();
        // 工作目录本身有 .git
        std::fs::create_dir_all(dir.join(".git")).unwrap();
        assert_eq!(project_root(&nested), Some(dir.as_path()));
        assert_eq!(project_root(&dir), Some(dir.as_path()));
    }

    #[test]
    fn config_path_uses_one_separator_style() {
        // 界面上直接把 path_in 的结果显示给用户看：Win 上不能出现 `…\proj/.claudesktop`
        let root = Path::new("proj");
        let path = path_in(root);
        assert_eq!(path, root.join(".claudesktop").join("settings.json"));
        let shown = path.display().to_string();
        #[cfg(windows)]
        assert!(!shown.contains('/'), "{shown}");
        assert!(
            shown.starts_with(".claudesktop")
                || shown.contains(&format!("{}{}", DIR, std::path::MAIN_SEPARATOR))
        );
    }

    #[test]
    fn reads_all_fields() {
        let dir = tmp("fields");
        std::fs::create_dir_all(dir.join(".claudesktop")).unwrap();
        std::fs::write(
            path_in(&dir),
            r#"{
                "model": "claude-sonnet-4",
                "permissions": { "allow": ["edit"], "deny": ["shell:rm"] },
                "mcpDisabled": ["browser"],
                "pluginsEnabled": ["lint"],
                "instructions": "docs/AGENTS.md"
            }"#,
        )
        .unwrap();
        let c = load(&dir).unwrap();
        assert_eq!(c.model.as_deref(), Some("claude-sonnet-4"));
        assert_eq!(c.permissions, vec!["edit".to_string(), "deny:shell:rm".to_string()]);
        assert_eq!(c.mcp_disabled, vec!["browser".to_string()]);
        assert_eq!(c.plugins_enabled.as_deref(), Some(&["lint".to_string()][..]));
        assert_eq!(c.instructions, Some(PathBuf::from("docs/AGENTS.md")));
        assert!(c.warnings.is_empty());
    }

    #[test]
    fn broken_file_yields_warnings() {
        let dir = tmp("broken");
        std::fs::create_dir_all(dir.join(".claudesktop")).unwrap();
        std::fs::write(path_in(&dir), "{ not json").unwrap();
        let c = load(&dir).unwrap();
        assert!(c.model.is_none());
        assert!(c.broken);
        assert_eq!(c.warnings.len(), 1);
        assert!(c.warnings[0].contains("settings.json"), "{}", c.warnings[0]);
        // 坏文件不被保存覆盖
        let config = ProjectConfig { model: Some("m".into()), ..Default::default() };
        assert!(save(&dir, &config).is_err());
        assert_eq!(std::fs::read_to_string(path_in(&dir)).unwrap(), "{ not json");
    }

    /// 多一个 `$schema`（或别的不认识的键）不算坏文件：字段照常生效，保存时原样留着
    #[test]
    fn unknown_keys_are_kept() {
        let dir = tmp("unknown");
        std::fs::create_dir_all(dir.join(".claudesktop")).unwrap();
        std::fs::write(
            path_in(&dir),
            r#"{"$schema":"x.json","model":"a","mcpDisabled":["git"],"note":"手写"}"#,
        )
        .unwrap();
        let mut c = load(&dir).unwrap();
        assert!(!c.broken);
        assert_eq!(c.model.as_deref(), Some("a"));
        assert_eq!(c.mcp_disabled, ["git"]);
        assert_eq!(c.warnings.len(), 1, "{:?}", c.warnings); // 只提示 note，不提示 $schema
        c.model = Some("b".into());
        save(&dir, &c).unwrap();
        let text = std::fs::read_to_string(path_in(&dir)).unwrap();
        assert!(
            text.contains("$schema") && text.contains("手写") && text.contains("\"b\""),
            "{text}"
        );
        assert!(text.contains("mcpDisabled"), "{text}");
    }

    #[test]
    fn no_file_is_none() {
        let dir = tmp("empty");
        assert!(load(&dir).is_none());
    }

    /// `worktree` 一栏：读得出来、写回去、空白项被清掉；什么都没配时文件里不出现这一栏
    #[test]
    fn reads_and_writes_the_worktree_section() {
        let dir = tmp("worktree-config");
        std::fs::create_dir_all(dir.join(".claudesktop")).unwrap();
        std::fs::write(
            path_in(&dir),
            r#"{"worktree":{"copy":[".env*"," ","data/"],"setup":"  npm install  "}}"#,
        )
        .unwrap();
        let mut c = load(&dir).unwrap();
        assert_eq!(c.worktree.copy, vec![".env*".to_string(), "data/".to_string()]);
        assert_eq!(c.worktree.setup.as_deref(), Some("npm install"));
        assert!(!c.worktree.is_empty());
        // 写回去还是这两项
        c.model = None;
        save(&dir, &c).unwrap();
        let text = std::fs::read_to_string(path_in(&dir)).unwrap();
        assert!(text.contains("\".env*\""), "{text}");
        assert!(text.contains("npm install"), "{text}");
        // 都空着：这一栏不写（一个字段都不剩时整个文件都删掉）
        c.worktree = WorktreeConfig::default();
        save(&dir, &c).unwrap();
        assert!(!path_in(&dir).exists(), "全空时不该留一个空对象在仓库里");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 复制规则：`*` 只在一段路径内、`**` 跨层、以 `/` 结尾的只要目录、绝对路径跳过
    #[test]
    fn copy_patterns_match_the_way_users_expect() {
        let dir = tmp("copy-patterns");
        std::fs::create_dir_all(dir.join("config")).unwrap();
        std::fs::create_dir_all(dir.join("crates/a/src")).unwrap();
        let touch = |p: &str| std::fs::write(dir.join(p), "x").unwrap();
        touch(".env");
        touch(".env.local");
        touch("config/db.json");
        touch("crates/a/src/main.rs");
        touch("readme.md");

        let names = |pattern: &str| -> Vec<String> {
            copy_matches(&dir, pattern)
                .iter()
                .map(|p| p.strip_prefix(&dir).unwrap().display().to_string().replace('\\', "/"))
                .collect()
        };
        assert_eq!(names(".env*"), [".env", ".env.local"]);
        assert_eq!(names("config/*.json"), ["config/db.json"]);
        // `**` 跨层：每个 crates 下面的 main.rs 都算
        assert_eq!(names("**/main.rs"), ["crates/a/src/main.rs"]);
        // 以 / 结尾的只要目录
        assert_eq!(names("crates/"), ["crates"]);
        // 文件也匹配（不带 / 时）
        assert_eq!(names("readme.md"), ["readme.md"]);
        // 匹配不到、绝对路径、空规则都给空
        assert!(names("nope*").is_empty());
        assert!(copy_matches(&dir, "C:/Windows/win.ini").is_empty());
        assert!(copy_matches(&dir, "  ").is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 符号链接成环不会走不完；`.*` 不带 `.git`；worktree 里提交的链接不往里写。
    #[cfg(unix)]
    #[test]
    fn copy_skips_symlink_loops_git_and_linked_targets() {
        let base = std::env::temp_dir().join(format!("cd-copy-links-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let (repo, wt, outside) = (base.join("repo"), base.join("wt"), base.join("outside"));
        std::fs::create_dir_all(repo.join("pkg/node_modules")).unwrap();
        std::fs::create_dir_all(repo.join(".git")).unwrap();
        std::fs::create_dir_all(&wt).unwrap();
        std::fs::create_dir_all(&outside).unwrap();
        std::fs::write(repo.join("pkg/node_modules/a.txt"), "a").unwrap();
        std::fs::write(repo.join(".env"), "K=1").unwrap();
        // 链回上层：跟着走就是死循环
        std::os::unix::fs::symlink(&repo, repo.join("pkg/node_modules/loop")).unwrap();

        assert_eq!(copy_matches(&repo, "**/a.txt"), [repo.join("pkg/node_modules/a.txt")]);
        assert_eq!(copy_matches(&repo, ".*"), [repo.join(".env")]);
        assert_eq!(copy_into(&repo, &wt, Path::new("pkg")), Ok(1));
        assert!(wt.join("pkg/node_modules/a.txt").is_file());

        // worktree 里 `.env` 是指到外面的链接：不覆盖
        std::fs::remove_dir_all(&wt).unwrap();
        std::fs::create_dir_all(&wt).unwrap();
        std::os::unix::fs::symlink(outside.join("x"), wt.join(".env")).unwrap();
        assert!(copy_into(&repo, &wt, Path::new(".env")).is_err());
        assert!(!outside.join("x").exists());
        let _ = std::fs::remove_dir_all(&base);
    }

    /// 复制一个目录过去：里面的文件都跟着，目标目录不存在时自动建
    #[test]
    fn copies_a_directory_into_the_worktree() {
        let dir = tmp("copy-into");
        let repo = dir.join("repo");
        let wt = dir.join("wt");
        std::fs::create_dir_all(repo.join("conf/nested")).unwrap();
        std::fs::create_dir_all(&wt).unwrap();
        std::fs::write(repo.join(".env"), "K=1").unwrap();
        std::fs::write(repo.join("conf/nested/x.json"), "{}").unwrap();

        assert_eq!(copy_into(&repo, &wt, Path::new(".env")).unwrap(), 1);
        assert_eq!(std::fs::read_to_string(wt.join(".env")).unwrap(), "K=1");
        assert_eq!(copy_into(&repo, &wt, Path::new("conf")).unwrap(), 1);
        assert_eq!(std::fs::read_to_string(wt.join("conf/nested/x.json")).unwrap(), "{}");
        // 源不存在：给能看懂的原因
        let err = copy_into(&repo, &wt, Path::new("nope")).unwrap_err();
        assert!(err.contains("nope"), "{err}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn merges_permissions_into_project_key() {
        let root = Path::new("C:/proj");
        let config = ProjectConfig {
            permissions: vec!["edit".into(), "deny:shell:rm".into()],
            ..Default::default()
        };
        let file = serde_json::json!({ "projects": { "C:/proj": ["shell:git status"] } });
        let merged = merge_permissions(&file, root, &config).unwrap();
        let rules = merged["projects"]["C:/proj"].as_array().unwrap();
        assert_eq!(rules.len(), 3);
        assert!(rules.iter().any(|r| r.as_str() == Some("edit")));
        // 幂等：再合一次不变
        assert!(merge_permissions(&merged, root, &config).is_none());
        // 空规则不动
        assert!(merge_permissions(&file, root, &ProjectConfig::default()).is_none());
    }

    #[test]
    fn save_writes_only_the_fields_that_differ() {
        let dir = tmp("save");
        let config = ProjectConfig {
            model: Some("deepseek-chat".into()),
            permissions: vec!["edit".into(), "deny:shell:rm".into()],
            ..Default::default()
        };
        let path = save(&dir, &config).unwrap();
        assert_eq!(path, path_in(&dir));
        let text = std::fs::read_to_string(&path).unwrap();
        // 没设的字段不写进文件（这一层只记和全局不一样的）
        assert!(!text.contains("mcpDisabled"), "{text}");
        assert!(!text.contains("instructions"), "{text}");
        let back = load(&dir).unwrap();
        assert_eq!(back.model.as_deref(), Some("deepseek-chat"));
        assert_eq!(back.permissions, vec!["edit".to_string(), "deny:shell:rm".to_string()]);
        assert!(back.warnings.is_empty());
    }

    #[test]
    fn save_with_nothing_set_removes_the_file() {
        let dir = tmp("save-empty");
        let path =
            save(&dir, &ProjectConfig { model: Some("m".into()), ..Default::default() }).unwrap();
        assert!(path.is_file());
        save(&dir, &ProjectConfig::default()).unwrap();
        assert!(!path.exists(), "全空时不该留一个空对象在仓库里");
        // 再存一次也不报错
        save(&dir, &ProjectConfig::default()).unwrap();
    }

    /// 没有任何标记时返回 cwd 本身；标记在上级时返回上级。
    /// 用 `climb` 直接测（临时目录本身可能就在某个仓库里，真去磁盘上找会读到宿主的标记）。
    #[test]
    fn root_or_falls_back_to_the_working_directory() {
        let base = Path::new("base");
        let cwd = base.join("a/b");
        assert_eq!(climb(&cwd, |_| false), None);
        assert_eq!(climb(&cwd, |d| d == base), Some(base));
        // 工作目录本身有标记时优先用自己
        assert_eq!(climb(&cwd, |d| d == cwd), Some(cwd.as_path()));
        // 太深（超过 4 层）的标记不算
        let deep = base.join("a/b/c/d/e");
        assert_eq!(climb(&deep, |d| d == base), None);
    }

    /// 磁盘上真走一遍：cwd 自己有标记时就是它自己。
    #[test]
    fn root_or_uses_cwd_when_it_has_a_marker() {
        let dir = tmp("root-or-cwd");
        std::fs::create_dir_all(dir.join(".git")).unwrap();
        assert_eq!(root_or(&dir), dir.as_path());
    }

    /// worktree 的 `.git` 是文件：它自己就是项目根（子目录也算它）。
    #[test]
    fn worktree_is_a_project_root() {
        let main = tmp("wt-main");
        std::fs::create_dir_all(main.join(".git").join("worktrees").join("b1")).unwrap();
        let wt = main.join("b1");
        std::fs::create_dir_all(wt.join("src")).unwrap();
        std::fs::write(
            wt.join(".git"),
            format!("gitdir: {}\n", main.join(".git/worktrees/b1").display()),
        )
        .unwrap();
        assert_eq!(project_root(&wt), Some(wt.as_path()));
        assert_eq!(project_root(&wt.join("src")), Some(wt.as_path()));
        // 项目配置读的是这个分支上的版本
        assert_eq!(root_or(&wt.join("src")), wt.as_path());
    }

    /// 光有 `.git` 文件不算：得是 worktree 那种内容。
    #[test]
    fn a_junk_git_file_is_not_a_marker() {
        let dir = tmp("junk-git");
        std::fs::write(dir.join(".git"), "这不是 git 标记").unwrap();
        assert_eq!(project_root(&dir), None);
    }

    /// 子模块的 `.git` 也是文件，但它不是 worktree：共享根停在主仓库，不被它带走。
    #[test]
    fn submodule_gitdir_is_not_a_worktree() {
        let main = tmp("sub-main");
        let sub = main.join("vendor/lib");
        std::fs::create_dir_all(main.join(".git").join("modules").join("lib")).unwrap();
        std::fs::create_dir_all(&sub).unwrap();
        std::fs::write(sub.join(".git"), "gitdir: ../../.git/modules/lib\n").unwrap();
        // 共享根就是主仓库，不是 `…/modules/lib` 的某个上级
        assert_eq!(shared_root(&sub).as_deref(), Some(main.as_path()));
        assert_eq!(shared_dir(&sub), sub);
    }

    /// 记忆和权限跨 worktree 共享：worktree 里问出来的，回到主目录还在。
    #[test]
    fn shared_root_points_at_the_main_repo_for_worktrees() {
        let main = tmp("shared-main");
        std::fs::create_dir_all(main.join(".git/worktrees/b1")).unwrap();
        let wt = main.join("b1");
        std::fs::create_dir_all(wt.join("src")).unwrap();
        std::fs::write(
            wt.join(".git"),
            // 相对写法（git 默认就这么写）也要能解出来
            format!("gitdir: {}\n", main.join(".git/worktrees/b1").display()),
        )
        .unwrap();
        assert_eq!(shared_root(&wt).as_deref(), Some(main.as_path()));
        assert_eq!(shared_root(&wt.join("src")).as_deref(), Some(main.as_path()));
        // 主仓库自己不受影响
        assert_eq!(shared_root(&main).as_deref(), Some(main.as_path()));
    }

    /// 没有 git 时共享根就是项目根（有 `.claudesktop` 的那一层，否则是 cwd）。
    #[test]
    fn shared_root_falls_back_to_the_project_root() {
        let dir = tmp("shared-plain");
        let nested = dir.join("a/b");
        std::fs::create_dir_all(dir.join(DIR)).unwrap();
        std::fs::create_dir_all(&nested).unwrap();
        std::fs::write(path_in(&dir), "{}").unwrap();
        assert_eq!(shared_root(&nested).as_deref(), Some(dir.as_path()));
    }

    /// git 在 `.git` 文件里写正斜杠，别处是原生分隔符：共享根要走归一，
    /// 否则同一目录两种 `display()` 串，权限规则会变成两张表。
    #[test]
    fn shared_root_uses_the_native_separator() {
        let dir = tmp("shared-sep");
        let main = dir.join("main");
        let wt = main.join("b1");
        std::fs::create_dir_all(main.join(".git/worktrees/b1")).unwrap();
        std::fs::create_dir_all(&wt).unwrap();
        let slashed = main.join(".git/worktrees/b1").display().to_string().replace('\\', "/");
        std::fs::write(wt.join(".git"), format!("gitdir: {slashed}\n")).unwrap();
        assert_eq!(shared_root(&wt).as_deref(), Some(main.as_path()));
        assert_eq!(shared_root(&wt).unwrap().display().to_string(), main.display().to_string());
    }
}
