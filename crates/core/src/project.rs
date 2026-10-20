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
    /// 不认识的键（`$schema`、以后的新字段、手写的备注）：读时提示，写回时原样留着
    #[serde(flatten)]
    other: serde_json::Map<String, Value>,
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

/// 项目根（向上找几层 `.claudesktop` 或 git 标记，工作目录本身优先）。
///
/// git 标记两种都算：`.git` 是**目录**（普通仓库）或**文件**（worktree，内容是
/// `gitdir: <主仓库>/.git/worktrees/<名>`）。worktree 自己就是根——项目配置、
/// `instructions` 指的文件从这里读，读到的是这个分支上的版本，和代码一致。
pub fn project_root(cwd: &Path) -> Option<&Path> {
    climb(cwd, has_marker)
}

/// 这个目录里有没有 `.claudesktop` 或 git 标记（[`project_root`] 的判断条件）。
fn has_marker(d: &Path) -> bool {
    path_in(d).is_file() || d.join(".git").is_dir() || worktree_gitdir(d).is_some()
}

/// 读 `d/.git` 文件，确认是 worktree 标记后返回里面的 gitdir 路径（相对路径按 `d` 解析）。
///
/// 不能只看「是文件」：随便一个叫 `.git` 的文件不是仓库标记。也**不能**只认 `gitdir:`
/// ——子模块的 `.git` 文件长这样（`gitdir: ../.git/modules/x`），它不是 worktree，
/// 共享根不能被它引到主仓库去。判据是路径里那段 `worktrees/<名>`。
pub fn worktree_gitdir(d: &Path) -> Option<PathBuf> {
    let text = std::fs::read_to_string(d.join(".git")).ok()?;
    let rest = text.trim().strip_prefix("gitdir:")?.trim();
    if rest.is_empty() {
        return None;
    }
    let gitdir = PathBuf::from(rest);
    if !gitdir.components().any(|c| c.as_os_str() == "worktrees") {
        return None;
    }
    let gitdir = if gitdir.is_absolute() { gitdir } else { d.join(gitdir) };
    Some(normalize(&gitdir))
}

/// 按组件重组路径：git 在 `.git` 文件里写的是正斜杠（`C:/repo/.git/worktrees/b1`），
/// 而别处的 cwd 是原生分隔符。不统一的话同一个目录会有两种 `display()` 串，
/// 权限规则（拿 `display()` 当键）会变成两张互不可见的表。
fn normalize(path: &Path) -> PathBuf {
    path.components().collect()
}

/// worktree 对应的主仓库工作区根（不是 worktree、或者路径对不上 `…/.git/worktrees/<名>` 时 None）。
fn worktree_main_root(d: &Path) -> Option<PathBuf> {
    let gitdir = worktree_gitdir(d)?;
    // `…/<名>` → `…/worktrees` → `…/.git` → `<主仓库>`
    let dotgit = gitdir.parent()?.parent()?;
    if dotgit.file_name() != Some(std::ffi::OsStr::new(".git")) {
        return None;
    }
    dotgit.parent().map(Path::to_path_buf)
}

/// 项目记忆和权限规则落在哪个根下（和 [`project_root`] 不一定相同）。
///
/// - 普通仓库 / 没有 git：就是 [`project_root`]（没有标记时是 `cwd` 本身）；
/// - worktree：换成**主仓库根**。在 worktree 里说过的「记住……」、点过的「始终允许」，
///   回到主目录也算数，反之亦然（否则每开一个 worktree 都是一份全新的记忆和权限表）。
///
/// 纯文件解析，不调 git，可单测。
pub fn shared_root(cwd: &Path) -> PathBuf {
    let root = root_or(cwd);
    worktree_main_root(root).unwrap_or_else(|| root.to_path_buf())
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
            other: serde_json::Map::new(),
        }
    }

    fn is_empty(&self) -> bool {
        self.model.is_none()
            && self.permissions.is_none()
            && self.mcp_disabled.is_none()
            && self.plugins_enabled.is_none()
            && self.instructions.is_none()
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
        let dir =
            std::env::temp_dir().join(format!("claudesktop-project-{name}-{}", std::process::id()));
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
        assert!(worktree_gitdir(&dir).is_none());
    }

    /// 子模块的 `.git` 文件也以 `gitdir:` 开头，但它不是 worktree，共享根不能被它带走。
    #[test]
    fn submodule_gitdir_is_not_a_worktree() {
        let main = tmp("sub-main");
        let sub = main.join("vendor/lib");
        std::fs::create_dir_all(main.join(".git").join("modules").join("lib")).unwrap();
        std::fs::create_dir_all(&sub).unwrap();
        std::fs::write(sub.join(".git"), "gitdir: ../../.git/modules/lib\n").unwrap();
        assert!(worktree_gitdir(&sub).is_none());
        // 共享根就是主仓库，不是 `…/modules/lib` 的某个上级
        assert_eq!(shared_root(&sub), main);
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
        assert_eq!(shared_root(&wt), main);
        assert_eq!(shared_root(&wt.join("src")), main);
        // 主仓库自己不受影响
        assert_eq!(shared_root(&main), main);
    }

    /// 没有 git 时共享根就是项目根（有 `.claudesktop` 的那一层，否则是 cwd）。
    #[test]
    fn shared_root_falls_back_to_the_project_root() {
        let dir = tmp("shared-plain");
        let nested = dir.join("a/b");
        std::fs::create_dir_all(dir.join(DIR)).unwrap();
        std::fs::create_dir_all(&nested).unwrap();
        std::fs::write(path_in(&dir), "{}").unwrap();
        assert_eq!(shared_root(&nested), dir);
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
        assert_eq!(shared_root(&wt), main);
        assert_eq!(shared_root(&wt).display().to_string(), main.display().to_string());
    }
}
