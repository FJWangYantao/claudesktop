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
}

#[derive(Debug, Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
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
}

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

/// 项目根（向上找几层 `.claudesktop`，工作目录本身优先）。
pub fn project_root(cwd: &Path) -> Option<&Path> {
    let mut dir = Some(cwd);
    // 最多向上 4 层：太深的项目配置反而让人意外
    for _ in 0..4 {
        let d = dir?;
        if path_in(d).is_file() || d.join(".git").is_dir() {
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
        }
        Err(e) => {
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
/// 返回文件位置；`warnings` 不落盘。
pub fn save(root: &Path, config: &ProjectConfig) -> Result<PathBuf, String> {
    let path = path_in(root);
    let file = FileFormat::from(config);
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
        }
    }

    fn is_empty(&self) -> bool {
        self.model.is_none()
            && self.permissions.is_none()
            && self.mcp_disabled.is_none()
            && self.plugins_enabled.is_none()
            && self.instructions.is_none()
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
        assert_eq!(c.warnings.len(), 1);
        assert!(c.warnings[0].contains("settings.json"), "{}", c.warnings[0]);
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

    #[test]
    fn root_or_falls_back_to_the_working_directory() {
        let dir = tmp("root-or");
        assert_eq!(root_or(&dir), dir.as_path());
        std::fs::create_dir_all(dir.join(".git")).unwrap();
        assert_eq!(root_or(&dir), dir.as_path());
    }
}
