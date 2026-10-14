//! 用户设置：内核选择与各内核的配置，存在数据目录的 `settings.json`。
//!
//! 自研内核可存多套 provider 档案（协议 / 端点 / key / 模型 / 请求参数），
//! 输入框旁的菜单可快速切换。
//!
//! 启动时环境变量优先于文件，方便脚本和调试：`CLAUDESKTOP_KERNEL` /
//! `CLAUDESKTOP_BASE_URL` / `CLAUDESKTOP_MODEL` / `CLAUDESKTOP_API_KEY`（key 也认 `OPENAI_API_KEY`）
//! / `CLAUDESKTOP_PROTOCOL` / `CLAUDESKTOP_TEMPERATURE` / `CLAUDESKTOP_MAX_TOKENS` /
//! `CLAUDESKTOP_EXTRA_JSON`（后三个覆盖请求参数）以及 `ACP_AGENT`。
//! 设置页保存时写下的是当时界面上的值。

use claudesktop_builtin::{BuiltinConfig, Protocol, ProviderConfig, RequestParams};
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

/// 自研内核的一套 provider 档案：协议 + 端点 + key + 模型。
/// 可以存多套（DeepSeek 一套、Kimi 一套……），输入框旁一键切换。
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ProviderProfile {
    /// 显示名（如 `DeepSeek`）；空 = 用 base_url
    pub name: String,
    /// 协议（默认 OpenAI 兼容）
    pub protocol: Protocol,
    /// OpenAI 兼容 / Anthropic 原生端点的 base URL（不含路径后缀）
    pub base_url: String,
    pub api_key: String,
    /// 新会话的默认模型
    pub model: String,
    /// 上次从端点获取的模型列表（模型下拉用）
    pub models: Vec<String>,
    /// 透传参数（温度 / 最大 token / 额外 JSON 字段）
    pub params: RequestParams,
}

impl ProviderProfile {
    /// 档案显示名：没起名就用 base_url。
    pub fn display_name(&self) -> &str {
        if self.name.trim().is_empty() {
            &self.base_url
        } else {
            &self.name
        }
    }

    pub fn is_configured(&self) -> bool {
        !self.base_url.trim().is_empty() && !self.model.trim().is_empty()
    }

    pub fn provider_config(&self) -> ProviderConfig {
        ProviderConfig {
            protocol: self.protocol,
            base_url: self.base_url.trim().to_string(),
            api_key: Some(self.api_key.trim().to_string()).filter(|k| !k.is_empty()),
            model: self.model.trim().to_string(),
            params: self.params.clone(),
        }
    }
}

/// 自研内核设置：多套档案 + 当前用哪套。
/// 兼容旧版（单套扁平字段）的读取。
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct BuiltinSettings {
    pub profiles: Vec<ProviderProfile>,
    pub active: usize,
}

impl<'de> Deserialize<'de> for BuiltinSettings {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        // 旧版是扁平的 { base_url, api_key, model, models }，读成单档案
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum Raw {
            New {
                profiles: Vec<ProviderProfile>,
                #[serde(default)]
                active: usize,
            },
            Legacy(ProviderProfile),
        }
        Ok(match Raw::deserialize(d)? {
            Raw::New { profiles, active } => Self {
                active: active.min(profiles.len().saturating_sub(1)),
                profiles,
            },
            Raw::Legacy(p) => Self {
                profiles: vec![p],
                active: 0,
            },
        })
    }
}

/// 常用服务的预设档案（设置页一键添加，key 和模型仍需自己填）。
pub fn preset_profiles() -> Vec<ProviderProfile> {
    let make = |name: &str, protocol: Protocol, base_url: &str, model: &str| ProviderProfile {
        name: name.into(),
        protocol,
        base_url: base_url.into(),
        model: model.into(),
        ..Default::default()
    };
    vec![
        make("DeepSeek", Protocol::OpenAiCompat, "https://api.deepseek.com/v1", "deepseek-chat"),
        make("GLM 智谱", Protocol::OpenAiCompat, "https://open.bigmodel.cn/api/paas/v4", "glm-4.6"),
        make("Kimi 月之暗面", Protocol::OpenAiCompat, "https://api.moonshot.cn/v1", "kimi-k2-0905-preview"),
        make("Anthropic", Protocol::Anthropic, "https://api.anthropic.com/v1", "claude-sonnet-4-5-20250929"),
        make("OpenAI", Protocol::OpenAiCompat, "https://api.openai.com/v1", "gpt-4.1-mini"),
        make("OpenRouter", Protocol::OpenAiCompat, "https://openrouter.ai/api/v1", ""),
        make("Ollama 本地", Protocol::OpenAiCompat, "http://127.0.0.1:11434/v1", ""),
    ]
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct AcpSettings {
    /// agent 启动命令；空 = 仓库内 mock agent
    pub command: String,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
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
            self.builtin.current_mut().base_url = v;
        }
        if let Some(v) = var("CLAUDESKTOP_MODEL") {
            self.builtin.current_mut().model = v;
        }
        if let Some(v) = var("CLAUDESKTOP_API_KEY").or_else(|| var("OPENAI_API_KEY")) {
            self.builtin.current_mut().api_key = v;
        }
        if let Some(v) = var("CLAUDESKTOP_PROTOCOL") {
            self.builtin.current_mut().protocol = match v.to_ascii_lowercase().as_str() {
                "anthropic" => Protocol::Anthropic,
                _ => Protocol::OpenAiCompat,
            };
        }
        // 请求参数覆盖（temperature / max_tokens / 额外 JSON 字段），写进当前档案
        let param_names = [
            "CLAUDESKTOP_TEMPERATURE",
            "CLAUDESKTOP_MAX_TOKENS",
            "CLAUDESKTOP_EXTRA_JSON",
        ];
        if param_names.iter().any(|k| var(k).is_some()) {
            let p = self.builtin.current_mut();
            if let Some(v) = var("CLAUDESKTOP_TEMPERATURE").and_then(|v| v.parse::<f64>().ok()) {
                p.params.temperature = Some(v);
            }
            if let Some(v) = var("CLAUDESKTOP_MAX_TOKENS").and_then(|v| v.parse::<u64>().ok()) {
                p.params.max_tokens = Some(v);
            }
            if let Some(v) = var("CLAUDESKTOP_EXTRA_JSON").and_then(|v| serde_json::from_str(&v).ok()) {
                p.params.extra = v;
            }
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
        let p = self.builtin.current();
        if p.base_url.trim().is_empty() {
            anyhow::bail!("还没有设置端点地址（设置 → 自研内核）");
        }
        if p.model.trim().is_empty() {
            anyhow::bail!("还没有选择模型（设置 → 自研内核）");
        }
        Ok(BuiltinConfig::new(p.provider_config(), cwd))
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
    /// 当前生效的档案（一份都没有时给空档案占位）。
    pub fn current(&self) -> &ProviderProfile {
        static EMPTY: ProviderProfile = ProviderProfile {
            name: String::new(),
            protocol: Protocol::OpenAiCompat,
            base_url: String::new(),
            api_key: String::new(),
            model: String::new(),
            models: Vec::new(),
            params: RequestParams {
                temperature: None,
                max_tokens: None,
                extra: serde_json::Value::Null,
            },
        };
        self.profiles.get(self.active).unwrap_or(&EMPTY)
    }

    /// 可变访问当前档案；一份都没有时先补一套空的。
    pub fn current_mut(&mut self) -> &mut ProviderProfile {
        if self.profiles.is_empty() {
            self.profiles.push(ProviderProfile::default());
            self.active = 0;
        }
        let idx = self.active.min(self.profiles.len() - 1);
        self.active = idx;
        &mut self.profiles[idx]
    }

    /// 切到第 `i` 套档案（越界忽略）。
    pub fn select(&mut self, i: usize) {
        if i < self.profiles.len() {
            self.active = i;
        }
    }

    pub fn is_configured(&self) -> bool {
        self.current().is_configured()
    }

    pub fn base_url(&self) -> &str {
        &self.current().base_url
    }

    pub fn model(&self) -> &str {
        &self.current().model
    }

    pub fn models(&self) -> &[String] {
        &self.current().models
    }

    pub fn provider(&self) -> ProviderConfig {
        self.current().provider_config()
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
        s.builtin.current_mut().base_url = "http://x/v1".into();
        s.builtin.current_mut().model = "m".into();
        // 没选过内核但配好了自研内核 → 用它
        assert_eq!(s.kernel(), KernelChoice::Builtin);
        s.kernel = Some(KernelChoice::Demo);
        s.save(&path).unwrap();
        assert_eq!(Settings::load(Some(&path)).0, s);

        std::fs::write(&path, "{not json").unwrap();
        let (s, err) = Settings::load(Some(&path));
        assert!(err.is_some() && s == Settings::default());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 旧版（单套扁平字段）读取成单档案；新版 profiles 数组 + active 正常。
    #[core::prelude::v1::test]
    fn profile_migration_and_accessors() {
        // 旧版扁平格式
        let legacy = r#"{
            "kernel": "builtin",
            "builtin": {
                "base_url": "http://127.0.0.1:8045/v1",
                "api_key": "k",
                "model": "m",
                "models": ["m", "m2"]
            }
        }"#;
        let s: Settings = serde_json::from_str(legacy).unwrap();
        assert_eq!(s.builtin.profiles.len(), 1);
        assert_eq!(s.builtin.current().base_url, "http://127.0.0.1:8045/v1");
        assert_eq!(s.builtin.current().models, vec!["m", "m2"]);
        assert!(s.builtin.is_configured());
        assert_eq!(s.builtin.model(), "m");
        // 旧格式省略字段
        let s: Settings = serde_json::from_str(r#"{"builtin":{}}"#).unwrap();
        assert_eq!(s.builtin.profiles.len(), 1);
        assert!(!s.builtin.is_configured());
        assert!(s.builtin_config(PathBuf::from(".")).is_err());

        // 新版多档案
        let s: Settings = serde_json::from_str(
            r#"{"builtin":{"profiles":[
                {"name":"DeepSeek","base_url":"https://api.deepseek.com/v1","model":"deepseek-chat"},
                {"name":"Kimi","base_url":"https://api.moonshot.cn/v1","model":"kimi","protocol":"anthropic"}
            ],"active":1}}"#,
        )
        .unwrap();
        assert_eq!(s.builtin.profiles.len(), 2);
        assert_eq!(s.builtin.current().name, "Kimi");
        assert_eq!(s.builtin.current().protocol, Protocol::Anthropic);
        // 越界的 active 收回到最后一套
        let s: Settings = serde_json::from_str(
            r#"{"builtin":{"profiles":[{"base_url":"a","model":"m"}],"active":9}}"#,
        )
        .unwrap();
        assert_eq!(s.builtin.active, 0);
        // provider 配置把空 key 变 None
        let cfg = s.builtin.provider();
        assert_eq!((cfg.base_url.as_str(), cfg.model.as_str()), ("a", "m"));
        assert!(cfg.api_key.is_none());
    }

    /// 请求参数随档案持久化，并原样进 provider 配置。
    #[core::prelude::v1::test]
    fn params_roundtrip_into_provider_config() {
        let dir = std::env::temp_dir().join(format!("claudesktop-params-{}", std::process::id()));
        let path = dir.join("settings.json");
        let mut s = Settings::default();
        {
            let p = s.builtin.current_mut();
            p.base_url = "http://x/v1".into();
            p.model = "m".into();
            p.params.temperature = Some(0.3);
            p.params.max_tokens = Some(2048);
        }
        s.save(&path).unwrap();
        let back = Settings::load(Some(&path)).0;
        assert_eq!(back.builtin.current().params.temperature, Some(0.3));
        assert_eq!(back.builtin.current().params.max_tokens, Some(2048));
        let cfg = back.builtin.provider();
        assert_eq!(cfg.params.temperature, Some(0.3));
        assert_eq!(cfg.params.max_tokens, Some(2048));
        // 空档案的 provider 配置参数为空
        assert!(BuiltinSettings::default().provider().params.is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// current_mut 在空档案列表上自动补一套。
    #[core::prelude::v1::test]
    fn current_mut_creates_profile() {
        let mut b = BuiltinSettings::default();
        assert!(!b.is_configured());
        b.current_mut().base_url = "http://x/v1".into();
        b.current_mut().model = "m".into();
        assert_eq!(b.profiles.len(), 1);
        assert!(b.is_configured());
        b.select(5); // 越界忽略
        assert_eq!(b.active, 0);
    }

    #[core::prelude::v1::test]
    fn presets_have_distinct_endpoints() {        let presets = preset_profiles();
        assert!(presets.len() >= 5);
        let urls: Vec<&str> = presets.iter().map(|p| p.base_url.as_str()).collect();
        let mut sorted = urls.clone();
        sorted.sort();
        sorted.dedup();
        assert_eq!(urls.len(), sorted.len());
        // Anthropic 预设用原生协议
        let a = presets.iter().find(|p| p.name == "Anthropic").unwrap();
        assert_eq!(a.protocol, Protocol::Anthropic);
    }

    #[core::prelude::v1::test]
    fn kernel_of_history_and_slots() {
        assert_eq!(KernelChoice::of_agent("builtin:http://x/v1#m"), KernelChoice::Builtin);
        assert_eq!(KernelChoice::of_agent("builtin:anthropic:http://x/v1#m"), KernelChoice::Builtin);
        assert_eq!(KernelChoice::of_agent("node mock.mjs"), KernelChoice::Acp);
        assert_eq!(KernelChoice::of_agent(""), KernelChoice::Demo);
        // 槽下标互不相同且都在 0..3
        let mut idx: Vec<usize> = KernelChoice::ALL.iter().map(|k| k.index()).collect();
        idx.sort();
        assert_eq!(idx, vec![0, 1, 2]);
    }
}
