//! 「总是允许 / 总是拒绝」规则：按项目（工作目录）保存，落盘到数据目录的 `permissions.json`。
//!
//! 规则形如 `edit`（本项目内的文件编辑）、`shell:git status`（以这些词开头的
//! 非复合命令）；拒绝规则多一层前缀：`deny:edit`、`deny:shell:rm`。
//!
//! 文件是唯一的真相：每次判断前都重新读（文件很小），这样设置页里删掉的规则
//! 不用重启内核就生效；写入时也先读再改，不会覆盖别处的修改。

use crate::tools::shell_rule_matches;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::io;
use std::path::{Path, PathBuf};

/// 拒绝规则在存储里的前缀（界面和内核判断时剥掉）。
pub const DENY_PREFIX: &str = "deny:";

#[derive(Debug, Default, Serialize, Deserialize)]
struct FileFormat {
    #[serde(default)]
    projects: BTreeMap<String, BTreeSet<String>>,
}

#[derive(Debug, Default)]
pub struct PermissionRules {
    /// None = 只在内存里（测试 / 无数据目录）
    path: Option<PathBuf>,
    data: FileFormat,
}

/// 默认规则文件：数据目录下的 `permissions.json`。
pub fn default_rules_path() -> Option<PathBuf> {
    claudesktop_core::default_data_dir().ok().map(|d| d.join("permissions.json"))
}

impl PermissionRules {
    /// 规则文件的位置（None = 只在内存里）。
    pub fn path(&self) -> Option<&Path> {
        self.path.as_deref()
    }

    /// 读取规则文件；不存在或损坏时从空规则开始。
    pub fn load(path: Option<PathBuf>) -> Self {
        let mut rules = Self { path, data: FileFormat::default() };
        rules.reload();
        rules
    }

    /// 从文件重新读（只在内存里的不动）。
    fn reload(&mut self) {
        if let Some(path) = &self.path {
            self.data = std::fs::read_to_string(path)
                .ok()
                .and_then(|s| serde_json::from_str(&s).ok())
                .unwrap_or_default();
        }
    }

    fn save(&self) -> io::Result<()> {
        let Some(path) = &self.path else {
            return Ok(());
        };
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let tmp = path.with_extension("json.tmp");
        std::fs::write(&tmp, serde_json::to_string_pretty(&self.data)?)?;
        std::fs::rename(&tmp, path)
    }

    /// 全部规则：(项目目录, 规则)，按项目、规则排序。拒绝规则带 `deny:` 前缀。
    pub fn list(&self) -> Vec<(String, Vec<String>)> {
        self.data
            .projects
            .iter()
            .filter(|(_, rules)| !rules.is_empty())
            .map(|(project, rules)| (project.clone(), rules.iter().cloned().collect()))
            .collect()
    }

    /// 删掉一条规则（`project` 用 [`Self::list`] 给出的原样字符串）并落盘。
    pub fn remove(&mut self, project: &str, rule: &str) -> io::Result<()> {
        self.reload();
        if let Some(rules) = self.data.projects.get_mut(project) {
            rules.remove(rule);
            if rules.is_empty() {
                self.data.projects.remove(project);
            }
        }
        self.save()
    }

    fn key(cwd: &Path) -> String {
        cwd.display().to_string()
    }

    /// 这条动作是否命中已保存的拒绝规则。`rule` 来自 [`crate::tools::Action::rule`]，
    /// `command` 为 shell 命令原文（shell 规则按词前缀匹配）。
    pub fn denies(&mut self, cwd: &Path, rule: &str, command: Option<&str>) -> bool {
        self.matching(cwd, rule, command, DENY_PREFIX)
    }

    /// 这条动作是否被已保存的允许规则覆盖。
    pub fn allows(&mut self, cwd: &Path, rule: &str, command: Option<&str>) -> bool {
        self.matching(cwd, rule, command, "")
    }

    /// 在当前项目的规则里找 `prefix` 前缀（`""` = 允许、`deny:` = 拒绝）下
    /// 与 `rule` 匹配的一条。shell 规则按词前缀匹配，其余按整串相等。
    /// 工作目录在项目的子目录里时，项目根下的规则（项目配置并进来的就记在根下）同样算数。
    fn matching(&mut self, cwd: &Path, rule: &str, command: Option<&str>, prefix: &str) -> bool {
        self.reload();
        let mut keys = vec![Self::key(cwd)];
        if let Some(root) = claudesktop_core::project::project_root(cwd)
            && root != cwd
        {
            keys.push(Self::key(root));
        }
        let stored_prefix = format!("{prefix}shell:");
        keys.iter().filter_map(|key| self.data.projects.get(key)).any(|rules| {
            match (rule.strip_prefix("shell:"), command) {
                (Some(_), Some(command)) => rules
                    .iter()
                    .filter_map(|r| r.strip_prefix(&stored_prefix))
                    .any(|r| shell_rule_matches(r, command)),
                _ => rules.contains(&format!("{prefix}{rule}")),
            }
        })
    }

    /// 添加一条规则并立即落盘（原子写）。拒绝规则带 `deny:` 前缀。
    pub fn add(&mut self, cwd: &Path, rule: String) -> io::Result<()> {
        self.reload();
        // 同一动作允许和拒绝不会同时存：加一边时把整串相反的那条删掉（后写的赢）；
        // shell 规则前缀重叠的细粒度情况留给用户在设置页里自己清理
        let opposite = if let Some(rest) = rule.strip_prefix(DENY_PREFIX) {
            rest.to_string()
        } else {
            format!("{DENY_PREFIX}{rule}")
        };
        if let Some(rules) = self.data.projects.get_mut(&Self::key(cwd)) {
            rules.remove(&opposite);
            if rules.is_empty() {
                self.data.projects.remove(&Self::key(cwd));
            }
        }
        self.data.projects.entry(Self::key(cwd)).or_default().insert(rule);
        self.save()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rules_are_per_project_and_persist() {
        let dir = std::env::temp_dir().join(format!("claudesktop-perm-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let file = dir.join("permissions.json");
        let (a, b) = (Path::new("/proj/a"), Path::new("/proj/b"));

        let mut rules = PermissionRules::load(Some(file.clone()));
        rules.add(a, "edit".into()).unwrap();
        rules.add(a, "shell:git status".into()).unwrap();

        let mut rules = PermissionRules::load(Some(file.clone()));
        assert!(rules.allows(a, "edit", None));
        assert!(!rules.allows(b, "edit", None));
        assert!(rules.allows(a, "shell:git status", Some("git status --short")));
        assert!(!rules.allows(a, "shell:git push", Some("git push")));
        assert!(!rules.allows(a, "shell:git status", Some("git status | rm x")));

        // 另一个实例（设置页）删掉规则：正在用的实例下次判断就看到
        let key = a.display().to_string();
        let mut other = PermissionRules::load(Some(file.clone()));
        assert_eq!(
            other.list(),
            vec![(key.clone(), vec!["edit".to_string(), "shell:git status".to_string()])]
        );
        other.remove(&key, "edit").unwrap();
        assert!(!rules.allows(a, "edit", None));
        other.remove(&key, "shell:git status").unwrap();
        assert!(other.list().is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 会话开在项目子目录里：记在项目根下的规则（项目配置并进来的）照样生效
    #[test]
    fn rules_at_the_project_root_cover_subdirectories() {
        let dir =
            std::env::temp_dir().join(format!("claudesktop-perm-root-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let root = dir.join("repo");
        let sub = root.join("crates").join("x");
        std::fs::create_dir_all(root.join(".git")).unwrap();
        std::fs::create_dir_all(&sub).unwrap();
        let mut rules = PermissionRules::load(Some(dir.join("permissions.json")));
        rules.add(&root, "deny:shell:rm".into()).unwrap();
        assert!(rules.denies(&sub, "shell:rm", Some("rm -rf build")));
        assert!(rules.denies(&root, "shell:rm", Some("rm -rf build")));
        assert!(!rules.denies(&dir, "shell:rm", Some("rm -rf build")));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn deny_rules_win_and_replace_allows() {
        let dir =
            std::env::temp_dir().join(format!("claudesktop-perm-deny-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let file = dir.join("permissions.json");
        let project = Path::new("/proj/a");

        // 先允许、再拒绝：只剩拒绝
        let mut rules = PermissionRules::load(Some(file.clone()));
        rules.add(project, "edit".into()).unwrap();
        rules.add(project, "deny:edit".into()).unwrap();
        assert!(rules.denies(project, "edit", None));
        assert!(!rules.allows(project, "edit", None));
        assert_eq!(rules.list(), vec![("/proj/a".into(), vec!["deny:edit".to_string()])]);

        // shell 拒绝按词前缀匹配，不影响别的命令
        rules.add(project, "deny:shell:rm".into()).unwrap();
        assert!(rules.denies(project, "shell:rm", Some("rm -rf target")));
        assert!(!rules.denies(project, "shell:git", Some("git status")));

        // 拒绝换回允许：拒绝那条被替换掉
        rules.add(project, "shell:rm".into()).unwrap();
        assert!(!rules.denies(project, "shell:rm", Some("rm -rf target")));
        assert!(rules.allows(project, "shell:rm", Some("rm -rf target")));

        // 另一个项目不受影响
        assert!(!rules.denies(Path::new("/proj/b"), "edit", None));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn legacy_files_without_deny_rules_still_load() {
        let dir = std::env::temp_dir().join(format!("claudesktop-perm-old-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("permissions.json");
        std::fs::write(&file, r#"{"projects":{"/old":["edit"]}}"#).unwrap();
        let mut rules = PermissionRules::load(Some(file));
        assert!(rules.allows(Path::new("/old"), "edit", None));
        assert!(!rules.denies(Path::new("/old"), "edit", None));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
