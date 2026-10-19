//! 用户设置：内核选择与各内核的配置，存在数据目录的 `settings.json`。
//!
//! 自研内核可存多套 provider 档案（协议 / 端点 / key / 模型 / 请求参数），
//! 输入框旁的菜单可快速切换。
//!
//! 启动时环境变量优先于文件，方便脚本和调试：`CLAUDESKTOP_KERNEL` /
//! `CLAUDESKTOP_BASE_URL` / `CLAUDESKTOP_MODEL` / `CLAUDESKTOP_API_KEY`（key 也认 `OPENAI_API_KEY`）
//! / `CLAUDESKTOP_PROTOCOL` / `CLAUDESKTOP_TEMPERATURE` / `CLAUDESKTOP_MAX_TOKENS` /
//! `CLAUDESKTOP_EXTRA_JSON`（后三个覆盖请求参数）/ `CLAUDESKTOP_MAX_STEPS`（一轮最多几步）/
//! `CLAUDESKTOP_GATEWAY`（`0` / `1`，用量网关）以及 `ACP_AGENT`。
//! 设置页保存时写下的是当时界面上的值。

use crate::model::ViewMode;
use claudesktop_builtin::{BuiltinConfig, ModelInfo, Protocol, ProviderConfig, RequestParams};
use claudesktop_core::AcpKernelConfig;
use gpui_kit::SharedString;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
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
    /// 同一次获取里端点报告的上下文窗口（模型 ID → token；输入框旁的上下文用量用）
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub context_windows: BTreeMap<String, u64>,
    /// 透传参数（温度 / 最大 token / 额外 JSON 字段）
    pub params: RequestParams,
}

impl ProviderProfile {
    /// 给人看的名字：没起名就用端点地址。
    pub fn display_name(&self) -> &str {
        let name = self.name.trim();
        if name.is_empty() { self.base_url.trim() } else { name }
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
            // 网关由 `Settings::builtin_config` 按开关补上
            gateway: None,
        }
    }

    /// 记下一次获取到的模型列表（连同窗口大小）。
    pub fn set_models(&mut self, models: &[ModelInfo]) {
        self.models = models.iter().map(|m| m.id.clone()).collect();
        self.context_windows =
            models.iter().filter_map(|m| Some((m.id.clone(), m.context_window?))).collect();
    }

    /// 某个模型的上下文窗口。顶栏显示的模型名可能只是 ID 的最后一段（`provider/name` → `name`），
    /// 对不上完整 ID 时按最后一段找。
    pub fn context_window(&self, model: &str) -> Option<u64> {
        self.context_windows.get(model).copied().or_else(|| {
            self.context_windows
                .iter()
                .find(|(id, _)| id.rsplit('/').next() == Some(model))
                .map(|(_, n)| *n)
        })
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
            // 旧档案比 New 分支小得多，Box 一层避免整个枚举被撑到 256 字节
            Legacy(Box<ProviderProfile>),
        }
        Ok(match Raw::deserialize(d)? {
            Raw::New { profiles, active } => {
                Self { active: active.min(profiles.len().saturating_sub(1)), profiles }
            }
            Raw::Legacy(p) => Self { profiles: vec![*p], active: 0 },
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
        make(
            "Kimi 月之暗面",
            Protocol::OpenAiCompat,
            "https://api.moonshot.cn/v1",
            "kimi-k2-0905-preview",
        ),
        make(
            "Anthropic",
            Protocol::Anthropic,
            "https://api.anthropic.com/v1",
            "claude-sonnet-4-5-20250929",
        ),
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
    /// agent 的请求也经用量网关（启动时把它的 `ANTHROPIC_BASE_URL` / `OPENAI_BASE_URL` 指到网关）；
    /// 默认关：改的是别的程序的网络设置，agent 不认这两个变量时不起作用。用量网关关着时也不起作用
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub gateway: bool,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    /// None = 从没选过：配好了自研内核就用它，否则 ACP
    pub kernel: Option<KernelChoice>,
    pub builtin: BuiltinSettings,
    pub acp: AcpSettings,
    /// 对话视图模式（摘要 / 标准 / 详细），顶栏下拉或 Ctrl+O 切换后立即写回
    pub view_mode: ViewMode,
    /// 外观（跟随系统 / 浅色 / 深色），设置页切换后立即生效并写回
    pub appearance: Appearance,
    /// 侧栏底部列出各内核状态（「自研内核 · 已连接」），设置页切换后立即生效并写回。
    /// 默认不显示（状态另有设置页与侧栏「进行中」可看）；老设置文件里没有这一项时
    /// 容器级 `#[serde(default)]` 会用下面的 `Default` 实现补齐，这里取 `false` 干净利落。
    pub sidebar_kernel_status: bool,
    /// 上次在输入框上方的目录 chip 里选的工作目录（新会话默认在这里开）；
    /// None = 用启动目录。环境变量 `ACP_CWD` 优先于它（启动时处理，不写进这里）
    pub workspace: Option<PathBuf>,
    /// 侧栏按日期（今天 / 昨天 / 最近 7 天 / 更早）而不是按工作目录分组，侧栏组名旁的按钮切换后写回
    pub sidebar_by_date: bool,
    /// 侧栏里折叠起来的组（`SessionGroup::key`：`dir:<工作目录>` 或 `date:<日期组名>`），点组名切换后写回
    pub sidebar_folded: Vec<String>,
    /// 启用了的插件（按插件名）；插件目录里新放进来的不在这里 = 停用，在设置页「插件」里打开
    pub plugins_enabled: Vec<String>,
    /// 自研内核一轮里最多请求模型的次数（工具循环上限，默认 200）；
    /// 越界的值读取时夹进 [`claudesktop_builtin::MIN_MAX_STEPS`] .. [`claudesktop_builtin::MAX_MAX_STEPS`]
    pub max_steps: usize,
    /// 自研内核的请求经用量网关转发、每个请求记一行日志（数据目录 `gateway/`）；默认开。
    /// 关掉就直连端点。设置页「内核与模型」有开关，`CLAUDESKTOP_GATEWAY` 覆盖它
    pub usage_gateway: bool,
    /// 网关另存每个请求的原文（请求体和响应），首页「请求」页签点开一次请求可看；默认开
    pub gateway_bodies: bool,
    /// 估算费用用的模型单价表；None = 没改过，用内置参考价（[`crate::pricing::default_prices`]）
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model_prices: Option<Vec<crate::pricing::Price>>,
    /// 峰谷价的时段（本地时间）；None = 没改过，用内置的 [`crate::pricing::default_peaks`]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub price_peaks: Option<Vec<crate::pricing::PeakWindow>>,
    /// 窗口不在前台时，回复完成 / 等待授权 / 出错发系统通知；默认开
    pub notifications: bool,
    /// 界面缩放倍率（1.0 = 标准），设置页即时生效并写回
    pub font_scale: f32,
    /// 界面字体族；空串 = 用内嵌的 Inter（默认）
    pub font_family: String,
    /// 动画（跟随系统的「动画效果」/ 总是播放 / 关闭），设置页切换后立即生效并写回
    pub motion: MotionPref,
}

/// `bool` 字段的默认值为 true（`bool::default()` 是 false，不能只靠派生）。
fn yes() -> bool {
    true
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            kernel: None,
            builtin: BuiltinSettings::default(),
            acp: AcpSettings::default(),
            view_mode: ViewMode::default(),
            appearance: Appearance::default(),
            sidebar_kernel_status: false,
            workspace: None,
            sidebar_by_date: false,
            sidebar_folded: Vec::new(),
            plugins_enabled: Vec::new(),
            max_steps: claudesktop_builtin::DEFAULT_MAX_STEPS,
            usage_gateway: yes(),
            gateway_bodies: yes(),
            model_prices: None,
            price_peaks: None,
            notifications: yes(),
            font_scale: crate::theme::FONT_SCALE_DEFAULT,
            font_family: String::new(),
            motion: MotionPref::default(),
        }
    }
}

/// 外观偏好。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Appearance {
    #[default]
    System,
    Light,
    Dark,
}

impl Appearance {
    pub const ALL: [Appearance; 3] = [Appearance::System, Appearance::Light, Appearance::Dark];

    pub fn label(self) -> &'static str {
        match self {
            Appearance::System => "跟随系统",
            Appearance::Light => "浅色",
            Appearance::Dark => "深色",
        }
    }
}

/// 动画偏好：系统「动画效果」关掉时 gpui-kit 会停掉所有动效，这里可以单独给本应用打开或关掉。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MotionPref {
    #[default]
    System,
    On,
    Off,
}

impl MotionPref {
    pub const ALL: [MotionPref; 3] = [MotionPref::System, MotionPref::On, MotionPref::Off];

    pub fn label(self) -> &'static str {
        match self {
            MotionPref::System => "跟随系统",
            MotionPref::On => "开",
            MotionPref::Off => "关",
        }
    }
}

impl Settings {
    /// 生效的模型单价表。
    pub fn prices(&self) -> Vec<crate::pricing::Price> {
        self.model_prices.clone().unwrap_or_else(crate::pricing::default_prices)
    }

    /// 生效的峰谷时段。
    pub fn price_peaks(&self) -> Vec<crate::pricing::PeakWindow> {
        self.price_peaks.clone().unwrap_or_else(crate::pricing::default_peaks)
    }

    /// 默认位置：`<数据目录>/settings.json`
    pub fn default_path() -> Option<PathBuf> {
        claudesktop_core::default_data_dir().ok().map(|d| d.join("settings.json"))
    }

    /// 读取；文件不存在或损坏时返回默认值（损坏的原因放进第二项）。
    pub fn load(path: Option<&Path>) -> (Self, Option<String>) {
        let Some(path) = path else {
            return (Self::default(), None);
        };
        match std::fs::read_to_string(path) {
            Err(_) => (Self::default(), None),
            Ok(text) => match serde_json::from_str(&text) {
                Ok(mut s) => {
                    Self::normalize(&mut s);
                    (s, None)
                }
                Err(e) => (Self::default(), Some(format!("设置文件损坏，已用默认值：{e}"))),
            },
        }
    }

    /// 夹住不可能的值：手改过设置文件时也别让界面缩放到看不见或写坏的字体名。
    fn normalize(s: &mut Self) {
        if !s.font_scale.is_finite() {
            s.font_scale = crate::theme::FONT_SCALE_DEFAULT;
        }
        s.font_scale =
            s.font_scale.clamp(crate::theme::FONT_SCALE_MIN, crate::theme::FONT_SCALE_MAX);
        if s.font_family.trim().is_empty() {
            s.font_family = String::new();
        }
        // 手改坏了：步数上限夹进允许范围（0 或天文数字都别让工具循环停不下来 / 走一步就断）
        s.max_steps = s
            .max_steps
            .clamp(claudesktop_builtin::MIN_MAX_STEPS, claudesktop_builtin::MAX_MAX_STEPS);
    }

    /// 界面字体族：空串表示用内嵌的默认字体。
    pub fn font_family_or_default(&self) -> Option<SharedString> {
        let name = self.font_family.trim();
        (!name.is_empty()).then(|| SharedString::from(name.to_string()))
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
                claudesktop_builtin::responses::PROTOCOL_NAME => Protocol::Responses,
                _ => Protocol::OpenAiCompat,
            };
        }
        // 请求参数覆盖（temperature / max_tokens / 额外 JSON 字段），写进当前档案
        let param_names =
            ["CLAUDESKTOP_TEMPERATURE", "CLAUDESKTOP_MAX_TOKENS", "CLAUDESKTOP_EXTRA_JSON"];
        if param_names.iter().any(|k| var(k).is_some()) {
            let p = self.builtin.current_mut();
            if let Some(v) = var("CLAUDESKTOP_TEMPERATURE").and_then(|v| v.parse::<f64>().ok()) {
                p.params.temperature = Some(v);
            }
            if let Some(v) = var("CLAUDESKTOP_MAX_TOKENS").and_then(|v| v.parse::<u64>().ok()) {
                p.params.max_tokens = Some(v);
            }
            if let Some(v) =
                var("CLAUDESKTOP_EXTRA_JSON").and_then(|v| serde_json::from_str(&v).ok())
            {
                p.params.extra = v;
            }
        }
        if let Some(v) = var("ACP_AGENT") {
            self.acp.command = v;
        }
        if let Some(v) = var("CLAUDESKTOP_GATEWAY") {
            self.usage_gateway = v.trim() != "0";
        }
        if let Some(v) = var("CLAUDESKTOP_MAX_STEPS").and_then(|v| v.parse::<usize>().ok()) {
            self.max_steps =
                v.clamp(claudesktop_builtin::MIN_MAX_STEPS, claudesktop_builtin::MAX_MAX_STEPS);
        }
        self
    }

    /// 实际使用的内核。从没选过时：填了 ACP 命令（含 `ACP_AGENT`）用 ACP，否则自研。
    pub fn kernel(&self) -> KernelChoice {
        self.kernel.unwrap_or(
            if self.acp.command.trim().is_empty() || self.builtin.is_configured() {
                KernelChoice::Builtin
            } else {
                KernelChoice::Acp
            },
        )
    }

    /// 新装、还没配过任何内核：没选过内核，自研内核没填端点 / 模型，ACP 也没填命令。
    /// 这时不起内核（ACP 空命令是仓库里的 mock agent，装好的程序里没有），首页换成配置引导。
    pub fn needs_setup(&self) -> bool {
        self.kernel.is_none() && !self.builtin.is_configured() && self.acp.command.trim().is_empty()
    }

    pub fn builtin_config(&self, cwd: PathBuf) -> anyhow::Result<BuiltinConfig> {
        let p = self.builtin.current();
        if p.base_url.trim().is_empty() {
            anyhow::bail!("还没有设置端点地址（设置 → 自研内核）");
        }
        if p.model.trim().is_empty() {
            anyhow::bail!("还没有选择模型（设置 → 自研内核）");
        }
        let mut config = BuiltinConfig::new(p.provider_config(), cwd);
        // 自动压缩要知道窗口多大
        config.context_windows = p.context_windows.clone().into_iter().collect();
        config.max_steps = self.max_steps;
        config.provider.gateway = self.gateway();
        Ok(config)
    }

    /// 用量网关（开着才有；进程里共用一个，第一次用到时起）。起不来就直连，不影响用。
    pub fn gateway(&self) -> Option<claudesktop_builtin::gateway::GatewayHandle> {
        use claudesktop_builtin::gateway;
        claudesktop_builtin::request_bodies::set_enabled(self.gateway_bodies);
        self.usage_gateway.then(gateway::default_dir).flatten().and_then(|d| gateway::shared(&d))
    }

    /// ACP 配置：命令为空时沿用默认（mock agent）；开了「经过用量网关」就给 agent 设好 base URL。
    pub fn acp_config(&self, mut base: AcpKernelConfig) -> AcpKernelConfig {
        if !self.acp.command.trim().is_empty() {
            base.command = self.acp.command.trim().to_string();
        }
        if self.acp.gateway
            && let Some(gateway) = self.gateway()
        {
            base.env.extend(claudesktop_builtin::gateway::agent_env(&gateway));
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
            context_windows: BTreeMap::new(),
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

    pub fn is_configured(&self) -> bool {
        self.current().is_configured()
    }

    pub fn base_url(&self) -> &str {
        &self.current().base_url
    }

    pub fn models(&self) -> &[String] {
        &self.current().models
    }

    pub fn context_window(&self, model: &str) -> Option<u64> {
        self.current().context_window(model)
    }

    pub fn provider(&self) -> ProviderConfig {
        self.current().provider_config()
    }
}

/// 本机装了 Antigravity Tools 时，读出它本地网关的地址和 key。
pub fn antigravity_gateway() -> Option<(String, String)> {
    let home = std::env::var_os("USERPROFILE").or_else(|| std::env::var_os("HOME"))?;
    let path = PathBuf::from(home).join(".antigravity_tools").join("gui_config.json");
    let value: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(path).ok()?).ok()?;
    let proxy = value.get("proxy")?;
    let port = proxy.get("port").and_then(|p| p.as_u64()).unwrap_or(8045);
    let key = proxy.get("api_key").and_then(|k| k.as_str()).unwrap_or_default().to_string();
    Some((format!("http://127.0.0.1:{port}/v1"), key))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[core::prelude::v1::test]
    fn ui_prefs_default_and_parse() {
        // 旧设置文件没有这几项：跟随系统 + 标准视图 + 不显示内核状态
        let s: Settings = serde_json::from_str("{}").unwrap();
        assert_eq!((s.appearance, s.view_mode), (Appearance::System, ViewMode::Normal));
        assert!(!s.sidebar_kernel_status);
        assert!(s.notifications);
        let s: Settings = serde_json::from_str(
            r#"{"appearance":"dark","view_mode":"verbose","sidebar_kernel_status":false}"#,
        )
        .unwrap();
        assert_eq!((s.appearance, s.view_mode), (Appearance::Dark, ViewMode::Verbose));
        assert!(!s.sidebar_kernel_status);
    }

    /// 步数上限：旧设置文件缺这项时用默认 200；手改成越界的值读取时夹住；
    /// 值原样进自研内核配置。
    #[core::prelude::v1::test]
    fn max_steps_default_clamp_and_roundtrip() {
        let default = claudesktop_builtin::DEFAULT_MAX_STEPS;
        assert_eq!(default, 200);
        let s: Settings = serde_json::from_str("{}").unwrap();
        assert_eq!(s.max_steps, default);

        let dir = std::env::temp_dir().join(format!("claudesktop-steps-{}", std::process::id()));
        let path = dir.join("settings.json");

        // 配好了内核才能拿到 `BuiltinConfig`
        let mut s = Settings::default();
        s.builtin.current_mut().base_url = "http://x/v1".into();
        s.builtin.current_mut().model = "m".into();
        s.max_steps = 30;
        s.save(&path).unwrap();
        let back = Settings::load(Some(&path)).0;
        assert_eq!(back.max_steps, 30);
        assert_eq!(back.builtin_config(PathBuf::from(".")).unwrap().max_steps, 30);

        // 手改坏了：0 抬到下限，天文数字压到上限
        std::fs::write(&path, r#"{"max_steps":0}"#).unwrap();
        assert_eq!(Settings::load(Some(&path)).0.max_steps, claudesktop_builtin::MIN_MAX_STEPS);
        std::fs::write(&path, r#"{"max_steps":999999}"#).unwrap();
        assert_eq!(Settings::load(Some(&path)).0.max_steps, claudesktop_builtin::MAX_MAX_STEPS);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[core::prelude::v1::test]
    fn roundtrip_and_defaults() {
        let dir = std::env::temp_dir().join(format!("claudesktop-settings-{}", std::process::id()));
        let path = dir.join("settings.json");
        let (s, err) = Settings::load(Some(&path));
        assert!(err.is_none());
        // 新装：什么都没配，也没有 ACP 命令 → 自研内核，并且算「还没配过」
        assert_eq!(s.kernel(), KernelChoice::Builtin);
        assert!(s.needs_setup());

        // 填了 ACP 命令（含 ACP_AGENT 带进来的）→ 用它
        let mut s = Settings::default();
        s.acp.command = "npx agent".into();
        assert_eq!(s.kernel(), KernelChoice::Acp);
        assert!(!s.needs_setup());

        let mut s = Settings::default();
        s.builtin.current_mut().base_url = "http://x/v1".into();
        s.builtin.current_mut().model = "m".into();
        // 没选过内核但配好了自研内核 → 用它
        assert_eq!(s.kernel(), KernelChoice::Builtin);
        assert!(!s.needs_setup());
        // 两个都配了：自研优先，和以前一样
        s.acp.command = "npx agent".into();
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
        assert_eq!(s.builtin.current().model, "m");
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
    }

    /// 字号 / 字体族：旧设置文件没有这两项时用标准档 + 内嵌字体；
    /// 越界或写坏的值在读取时被夹住。
    #[core::prelude::v1::test]
    fn font_prefs_default_clamp_and_roundtrip() {
        // 旧文件缺字段：标准档 + 空字体族（= 内嵌 Inter）
        let s: Settings = serde_json::from_str("{}").unwrap();
        assert_eq!(s.font_scale, crate::theme::FONT_SCALE_DEFAULT);
        assert_eq!(s.font_family_or_default(), None);

        let dir = std::env::temp_dir().join(format!("claudesktop-font-{}", std::process::id()));
        let path = dir.join("settings.json");

        let s = Settings {
            font_scale: 1.25,
            font_family: "Microsoft YaHei UI".into(),
            ..Default::default()
        };
        s.save(&path).unwrap();
        let back = Settings::load(Some(&path)).0;
        assert_eq!(back.font_scale, 1.25);
        assert_eq!(back.font_family_or_default().as_deref(), Some("Microsoft YaHei UI"));

        // 手改坏了：超大倍率被夹住、非有限值回到默认、空白字体族当成没选
        std::fs::write(&path, r#"{"font_scale":99.0,"font_family":"   "}"#).unwrap();
        let (back, err) = Settings::load(Some(&path));
        assert!(err.is_none());
        assert_eq!(back.font_scale, crate::theme::FONT_SCALE_MAX);
        assert_eq!(back.font_family_or_default(), None);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[core::prelude::v1::test]
    fn presets_have_distinct_endpoints() {
        let presets = preset_profiles();
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
        assert_eq!(
            KernelChoice::of_agent("builtin:anthropic:http://x/v1#m"),
            KernelChoice::Builtin
        );
        assert_eq!(KernelChoice::of_agent("node mock.mjs"), KernelChoice::Acp);
        assert_eq!(KernelChoice::of_agent(""), KernelChoice::Demo);
        // 槽下标互不相同且都在 0..3
        let mut idx: Vec<usize> = KernelChoice::ALL.iter().map(|k| k.index()).collect();
        idx.sort();
        assert_eq!(idx, vec![0, 1, 2]);
    }
}
