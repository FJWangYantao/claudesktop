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

use serde::Deserialize;
use serde_json::Value;
use std::path::{Path, PathBuf};

/// 项目配置文件的名字（在项目根下）。
pub const FILE: &str = ".claudesktop/settings.json";

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

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct FileFormat {
    #[serde(default)]
    model: Option<String>,
    #[serde(default)]
    permissions: Option<Permissions>,
    #[serde(default)]
    mcp_disabled: Option<Vec<String>>,
    #[serde(default)]
    plugins_enabled: Option<Vec<String>>,
    #[serde(default)]
    instructions: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
struct Permissions {
    #[serde(default)]
    allow: Vec<String>,
    #[serde(default)]
    deny: Vec<String>,
}

/// 项目根（向上找几层 `.claudesktop`，工作目录本身优先）。
pub fn project_root(cwd: &Path) -> Option<&Path> {
    let mut dir = Some(cwd);
    // 最多向上 4 层：太深的项目配置反而让人意外
    for _ in 0..4 {
        let d = dir?;
        if d.join(FILE).is_file() || d.join(".git").is_dir() {
            return Some(d);
        }
        dir = d.parent();
    }
    None
}

/// 读项目配置；没有文件返回 None，文件坏了返回带警告的默认（还能用）。
pub fn load(cwd: &Path) -> Option<ProjectConfig> {
    let root = project_root(cwd)?;
    let path = root.join(FILE);
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
    fn reads_all_fields() {
        let dir = tmp("fields");
        std::fs::create_dir_all(dir.join(".claudesktop")).unwrap();
        std::fs::write(
            dir.join(FILE),
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
        std::fs::write(dir.join(FILE), "{ not json").unwrap();
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
}
