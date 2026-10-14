//! 用户设置：内核选择与各内核的配置，存在数据目录的 `settings.json`。
//!
//! 启动时环境变量（`CLAUDESKTOP_KERNEL` / `CLAUDESKTOP_BASE_URL` / `CLAUDESKTOP_MODEL` /
//! `CLAUDESKTOP_API_KEY` / `ACP_AGENT`）优先于文件，方便脚本和调试；
//! 在设置页保存时写下的是当时界面上的值。

use claudesktop_builtin::{BuiltinConfig, ProviderConfig};
use claudesktop_core::AcpKernelConfig;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum KernelChoice {
    /// ACP agent（命令为空时用仓库内的 mock agent）
    #[default]
    Acp,
    /// 自研内核（OpenAI 兼容端点）
    Builtin,
    /// 纯演示回放
    Demo,
}

impl KernelChoice {
    pub const ALL: [KernelChoice; 3] = [Self::Builtin, Self::Acp, Self::Demo];

    /// 内核槽下标（`ChatApp::kernels`）
    pub fn index(self) -> usize {
        match self {
            Self::Acp => 0,
            Self::Builtin => 1,
            Self::Demo => 2,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Acp => "ACP agent",
            Self::Builtin => "自研内核",
            Self::Demo => "演示回放",
        }
    }

    /// 侧栏会话行上的短标签
    pub fn tag(self) -> &'static str {
        match self {
            Self::Acp => "ACP",
            Self::Builtin => "自研",
            Self::Demo => "演示",
        }
    }

    /// 历史会话属于哪种内核：按元信息里记的 agent 标识判断
    pub fn of_agent(agent: &str) -> Self {
        if agent.starts_with("builtin:") {
            Self::Builtin
        } else if agent.is_empty() {
            Self::Demo
        } else {
            Self::Acp
        }
    }

    fn parse(s: &str) -> Option<Self> {
        match s.trim() {
            "acp" => Some(Self::Acp),
            "builtin" => Some(Self::Builtin),
            "demo" => Some(Self::Demo),
            _ => None,
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct BuiltinSettings {
    /// OpenAI 兼容端点，如 `http://127.0.0.1:8045/v1`
    pub base_url: String,
    pub api_key: String,
    /// 新会话的默认模型
    pub model: String,
    /// 上次从端点获取的模型列表（模型下拉用）
    pub models: Vec<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct AcpSettings {
    /// agent 启动命令；空 = 仓库内 mock agent
    pub command: String,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    /// None = 从没选过：配好了自研内核就用它，否则 ACP
    pub kernel: Option<KernelChoice>,
    pub builtin: BuiltinSettings,
    pub acp: AcpSettings,
}

impl Settings {
    /// 默认位置：`<数据目录>/settings.json`
    pub fn default_path() -> Option<PathBuf> {
        claudesktop_core::default_data_dir()
            .ok()
            .map(|d| d.join("settings.json"))
    }

    /// 读取；文件不存在或损坏时返回默认值（损坏的原因放进第二项）。
    pub fn load(path: Option<&Path>) -> (Self, Option<String>) {
        let Some(path) = path else {
            return (Self::default(), None);
        };
        match std::fs::read_to_string(path) {
            Err(_) => (Self::default(), None),
            Ok(text) => match serde_json::from_str(&text) {
                Ok(s) => (s, None),
                Err(e) => (Self::default(), Some(format!("设置文件损坏，已用默认值：{e}"))),
            },
        }
    }

    /// 原子写入（先写临时文件再改名）。
    pub fn save(&self, path: &Path) -> anyhow::Result<()> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let tmp = path.with_extension("json.tmp");
        std::fs::write(&tmp, serde_json::to_string_pretty(self)?)?;
        std::fs::rename(&tmp, path)?;
        Ok(())
    }

    /// 用环境变量覆盖（启动时调用一次）。
    pub fn with_env(mut self) -> Self {
        let var = |k: &str| std::env::var(k).ok().filter(|v| !v.trim().is_empty());
        if let Some(k) = var("CLAUDESKTOP_KERNEL").and_then(|v| KernelChoice::parse(&v)) {
            self.kernel = Some(k);
        }
        if let Some(v) = var("CLAUDESKTOP_BASE_URL") {
            self.builtin.base_url = v;
        }
        if let Some(v) = var("CLAUDESKTOP_MODEL") {
            self.builtin.model = v;
        }
        if let Some(v) = var("CLAUDESKTOP_API_KEY").or_else(|| var("OPENAI_API_KEY")) {
            self.builtin.api_key = v;
        }
        if let Some(v) = var("ACP_AGENT") {
            self.acp.command = v;
        }
        self
    }

    /// 实际使用的内核。
    pub fn kernel(&self) -> KernelChoice {
        self.kernel.unwrap_or(if self.builtin.is_configured() {
            KernelChoice::Builtin
        } else {
            KernelChoice::Acp
        })
    }

    pub fn builtin_config(&self, cwd: PathBuf) -> anyhow::Result<BuiltinConfig> {
        let b = &self.builtin;
        if b.base_url.trim().is_empty() {
            anyhow::bail!("还没有设置端点地址（设置 → 自研内核）");
        }
        if b.model.trim().is_empty() {
            anyhow::bail!("还没有选择模型（设置 → 自研内核）");
        }
        Ok(BuiltinConfig::new(b.provider(), cwd))
    }

    /// ACP 配置：命令为空时沿用默认（mock agent）。
    pub fn acp_config(&self, mut base: AcpKernelConfig) -> AcpKernelConfig {
        if !self.acp.command.trim().is_empty() {
            base.command = self.acp.command.trim().to_string();
        }
        base
    }
}

impl BuiltinSettings {
    pub fn is_configured(&self) -> bool {
        !self.base_url.trim().is_empty() && !self.model.trim().is_empty()
    }

    pub fn provider(&self) -> ProviderConfig {
        ProviderConfig {
            base_url: self.base_url.trim().to_string(),
            api_key: Some(self.api_key.trim().to_string()).filter(|k| !k.is_empty()),
            model: self.model.trim().to_string(),
        }
    }
}

/// 本机装了 Antigravity Tools 时，读出它本地网关的地址和 key。
pub fn antigravity_gateway() -> Option<(String, String)> {
    let home = std::env::var_os("USERPROFILE").or_else(|| std::env::var_os("HOME"))?;
    let path = PathBuf::from(home).join(".antigravity_tools").join("gui_config.json");
    let value: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(path).ok()?).ok()?;
    let proxy = value.get("proxy")?;
    let port = proxy.get("port").and_then(|p| p.as_u64()).unwrap_or(8045);
    let key = proxy
        .get("api_key")
        .and_then(|k| k.as_str())
        .unwrap_or_default()
        .to_string();
    Some((format!("http://127.0.0.1:{port}/v1"), key))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[core::prelude::v1::test]
    fn roundtrip_and_defaults() {
        let dir = std::env::temp_dir().join(format!("claudesktop-settings-{}", std::process::id()));
        let path = dir.join("settings.json");
        let (s, err) = Settings::load(Some(&path));
        assert!(err.is_none());
        assert_eq!(s.kernel(), KernelChoice::Acp);

        let mut s = Settings::default();
        s.builtin.base_url = "http://x/v1".into();
        s.builtin.model = "m".into();
        // 没选过内核但配好了自研内核 → 用它
        assert_eq!(s.kernel(), KernelChoice::Builtin);
        s.kernel = Some(KernelChoice::Demo);
        s.save(&path).unwrap();
        assert_eq!(Settings::load(Some(&path)).0, s);

        std::fs::write(&path, "{not json").unwrap();
        let (s, err) = Settings::load(Some(&path));
        assert!(err.is_some() && s == Settings::default());
        // 缺字段时用默认值
        std::fs::write(&path, r#"{"kernel":"builtin","builtin":{"model":"x"}}"#).unwrap();
        let (s, _) = Settings::load(Some(&path));
        assert_eq!((s.kernel, s.builtin.model.as_str()), (Some(KernelChoice::Builtin), "x"));
        assert!(s.builtin_config(PathBuf::from(".")).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[core::prelude::v1::test]
    fn kernel_of_history_and_slots() {
        assert_eq!(KernelChoice::of_agent("builtin:http://x/v1#m"), KernelChoice::Builtin);
        assert_eq!(KernelChoice::of_agent("node mock.mjs"), KernelChoice::Acp);
        assert_eq!(KernelChoice::of_agent(""), KernelChoice::Demo);
        // 槽下标互不相同且都在 0..3
        let mut idx: Vec<usize> = KernelChoice::ALL.iter().map(|k| k.index()).collect();
        idx.sort();
        assert_eq!(idx, vec![0, 1, 2]);
    }
}
