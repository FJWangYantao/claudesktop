//! 用户设置：内核选择与各内核的配置，存在数据目录的 `settings.json`。
//!
//! 自研内核可存多套 provider 档案（协议 / 端点 / key / 模型 / 请求参数），
//! 输入框旁的菜单可快速切换。
//!
//! 启动时环境变量优先于文件，方便脚本和调试：`CLAUDESKTOP_KERNEL` /
//! `CLAUDESKTOP_BASE_URL` / `CLAUDESKTOP_MODEL` / `CLAUDESKTOP_API_KEY`（key 也认 `OPENAI_API_KEY`）
//! / `CLAUDESKTOP_PROTOCOL` / `CLAUDESKTOP_TEMPERATURE` / `CLAUDESKTOP_MAX_TOKENS` /
//! `CLAUDESKTOP_EXTRA_JSON`（后三个覆盖请求参数）/ `CLAUDESKTOP_MAX_STEPS`（一轮最多几步）/
//! `CLAUDESKTOP_GATEWAY`（`0` / `1`，用量网关）/ `CLAUDESKTOP_THEME`（配色 key，见 `ThemeChoice`）
//! 以及 `ACP_AGENT`。
//! 设置页保存时写下的是当时界面上的值。

use crate::model::ViewMode;
use claudesktop_builtin::{
    AutoModeConfig, BuiltinConfig, ModelInfo, Protocol, ProviderConfig, RequestParams,
    ThinkingLevel,
};
use claudesktop_core::AcpKernelConfig;
use gpui_kit::SharedString;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum KernelChoice {
    /// ACP agent（带档案 ID；命令为空时用仓库内的 mock agent）
    Acp(String),
    /// 自研内核（OpenAI 兼容端点）
    Builtin,
    /// 纯演示回放
    Demo,
}

/// 手写反序列化：老设置文件里 ACP 是不带档案 ID 的 `"acp"`，派生出来的会因为
/// 「newtype 变体读到了字符串」整份设置读失败。读成 `Acp("")`，`Settings::normalize` 再落到第一个档案。
impl<'de> Deserialize<'de> for KernelChoice {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(rename_all = "lowercase")]
        enum Tagged {
            Acp(String),
        }
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum Raw {
            Bare(String),
            Tagged(Tagged),
        }
        match Raw::deserialize(d)? {
            Raw::Bare(name) => match name.as_str() {
                "acp" => Ok(Self::Acp(String::new())),
                "builtin" => Ok(Self::Builtin),
                "demo" => Ok(Self::Demo),
                other => Err(serde::de::Error::unknown_variant(other, &["acp", "builtin", "demo"])),
            },
            Raw::Tagged(Tagged::Acp(id)) => Ok(Self::Acp(id)),
        }
    }
}

impl KernelChoice {
    /// 内核槽的键（`ChatApp::kernels`）：自研 / 演示固定，ACP 按档案 ID 各一个。
    pub fn key(&self) -> String {
        match self {
            Self::Builtin => "builtin".into(),
            Self::Demo => "demo".into(),
            Self::Acp(id) => format!("acp:{id}"),
        }
    }

    /// 是不是演示回放（不需要内核会话）。
    pub fn is_demo(&self) -> bool {
        matches!(self, Self::Demo)
    }

    /// 是不是同一种内核（ACP 的不比档案 ID：「默认内核」三选一只分自研 / agent / 演示）。
    pub fn same_kind(&self, other: &Self) -> bool {
        matches!(
            (self, other),
            (Self::Builtin, Self::Builtin)
                | (Self::Demo, Self::Demo)
                | (Self::Acp(_), Self::Acp(_))
        )
    }

    pub fn label(&self, acp: &AcpSettings) -> String {
        match self {
            Self::Builtin => "自研内核".into(),
            Self::Demo => "演示回放".into(),
            Self::Acp(id) => {
                acp.get(id).map(|a| a.display_name()).unwrap_or_else(|| "ACP agent".into())
            }
        }
    }

    /// 历史会话属于哪种内核：按元信息里记的 agent 标识判断。
    /// 老会话（`acp:<命令>`、没有档案 ID）归到第一个启用的档案。
    pub fn of_agent(agent: &str, acp: &AcpSettings) -> Self {
        if agent.starts_with("builtin:") {
            Self::Builtin
        } else if agent.is_empty() {
            Self::Demo
        } else if let Some(rest) = agent.strip_prefix("acp:") {
            // 新格式 `acp:<档案 ID>:<命令>`
            let id = rest.split(':').next().unwrap_or_default();
            if acp.get(id).is_some() {
                Self::Acp(id.to_string())
            } else {
                Self::Acp(acp.first().map(|a| a.id.clone()).unwrap_or_else(|| id.to_string()))
            }
        } else {
            // 老格式：命令本身就是标识。按命令找档案（单条命令迁成的 `custom-1` 正好对上），找不到归第一个
            let by_command = acp.agents.iter().find(|a| a.command_for(cfg!(windows)) == agent);
            Self::Acp(
                by_command
                    .or(acp.first())
                    .map(|a| a.id.clone())
                    .unwrap_or_else(|| "custom-1".into()),
            )
        }
    }

    fn parse(s: &str, acp: &AcpSettings) -> Option<Self> {
        match s.trim() {
            "acp" => Some(Self::Acp(acp.first().map(|a| a.id.clone())?)),
            "builtin" => Some(Self::Builtin),
            "demo" => Some(Self::Demo),
            other => other.strip_prefix("acp:").map(|id| Self::Acp(id.to_string())),
        }
    }

    /// 由槽的 key 还原（事件分发用）。档案已停用时仍带原 ID，label 退回通用称呼。
    pub fn from_key(key: &str, acp: &AcpSettings) -> Self {
        Self::parse(key, acp).unwrap_or(Self::Demo)
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
            // 网关由 `Settings::builtin_config` 按开关补上；`web_search` 用的 DeepSeek key 也在那儿填
            gateway: None,
            deepseek_key: None,
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

/// ACP 相关设置：启用了哪些本机 agent（每个一条档案），以及各自的选项。
///
/// 一条档案 = 一个 ACP 内核进程（懒启动：用到它的会话第一次连接时才起）。可以同时启用好几家，
/// 新建会话时选。预设档案（[`claudesktop_core::agents::AgentPreset`]）每次按预设现算启动命令，
/// 预设改了命令跟着变；自定义档案（`custom-1`）的命存在自己身上。
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct AcpSettings {
    /// 启用了的 agent，按添加顺序；第一项是新会话的默认
    pub agents: Vec<AgentProfile>,
    /// 老字段（只能填一条启动命令）：只读不写，读到非空时迁移成一条 `custom-1` 档案
    #[serde(default, skip_serializing)]
    pub command: String,
    /// 老字段：整个 ACP 的「经过用量网关」开关，迁移时带到那条自定义档案上
    #[serde(default, skip_serializing)]
    pub gateway: bool,
}

/// 启用的一家 agent（见 [`AcpSettings`]）。
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct AgentProfile {
    /// 预设 ID（`claude` / `opencode` / …），自定义档案是 `custom-<n>`
    pub id: String,
    /// 显示名；预设的取预设名（改不了），自定义的可以起
    pub name: String,
    /// 只有自定义档案有；预设档案按预设现算（预设改了命令跟着变）
    #[serde(skip_serializing_if = "Option::is_none")]
    pub command: Option<String>,
    /// 这一家的请求也经用量网关（只有认 `ANTHROPIC_BASE_URL` / `OPENAI_BASE_URL` 的才给开）
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub gateway: bool,
    /// 上次在这个 agent 上选的模型（新会话 `Started` 后按它切过去）
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_model: Option<String>,
}

impl AgentProfile {
    /// 预设档案：按预设 ID 建一条。
    pub fn from_preset(preset: &claudesktop_core::agents::AgentPreset) -> Self {
        Self { id: preset.id.to_string(), name: preset.name.to_string(), ..Default::default() }
    }

    /// 自定义档案（「自定义命令」那一栏）。
    pub fn custom(n: usize, command: &str) -> Self {
        Self {
            id: format!("custom-{n}"),
            name: "自定义 agent".into(),
            command: Some(command.trim().to_string()),
            ..Default::default()
        }
    }

    /// 对应的预设（自定义档案为 None）。
    pub fn preset(&self) -> Option<&'static claudesktop_core::agents::AgentPreset> {
        claudesktop_core::agents::AgentPreset::find(&self.id)
    }

    /// 显示名：预设的按预设现取（预设在之后的版本里改了名也跟着变），否则用存的。
    pub fn display_name(&self) -> String {
        match self.preset() {
            Some(preset) => preset.name.to_string(),
            None if self.name.trim().is_empty() => "自定义 agent".into(),
            None => self.name.clone(),
        }
    }

    /// 真正要跑的启动命令（`windows` = 当前平台是不是 Windows）。
    pub fn command_for(&self, windows: bool) -> String {
        match self.preset() {
            Some(preset) => preset.command_for(windows).to_string(),
            None => self.command.clone().unwrap_or_default(),
        }
    }

    /// 这一家能不能经用量网关（自定义档案当它认这两个环境变量）。
    pub fn supports_gateway(&self) -> bool {
        match self.preset() {
            Some(preset) => preset.gateway,
            None => true,
        }
    }
}

impl AcpSettings {
    /// 读到老的单条命令时迁移成一条自定义档案（只迁移一次：再存回去时老字段就不写了）。
    pub fn migrate(&mut self) {
        if self.command.trim().is_empty() {
            return;
        }
        let command = std::mem::take(&mut self.command);
        if self.agents.iter().any(|a| a.command.as_deref() == Some(command.trim())) {
            return;
        }
        let mut profile = AgentProfile::custom(self.next_custom_index(), &command);
        profile.gateway = self.gateway;
        self.gateway = false;
        self.agents.push(profile);
    }

    /// 下一个自定义档案的编号（`custom-<n>` 里的 n 不比现有的任何一条小）。
    fn next_custom_index(&self) -> usize {
        self.agents
            .iter()
            .filter_map(|a| a.id.strip_prefix("custom-").and_then(|n| n.parse::<usize>().ok()))
            .max()
            .map_or(1, |n| n + 1)
    }

    /// 第一个启用的档案（新会话的默认；老设置里 `kernel = acp` 就是这个）。
    pub fn first(&self) -> Option<&AgentProfile> {
        self.agents.first()
    }

    /// 按 ID 找档案。
    pub fn get(&self, id: &str) -> Option<&AgentProfile> {
        self.agents.iter().find(|a| a.id == id)
    }

    /// 按 ID 找档案（可改）。
    pub fn get_mut(&mut self, id: &str) -> Option<&mut AgentProfile> {
        self.agents.iter_mut().find(|a| a.id == id)
    }

    /// 加一条自定义档案，返回它的 ID（编号不复用：删掉的 `custom-2` 不会被新的顶替）。
    pub fn add_custom(&mut self, command: &str) -> String {
        let profile = AgentProfile::custom(self.next_custom_index(), command);
        let id = profile.id.clone();
        self.agents.push(profile);
        id
    }

    /// 记下某个档案上次用的模型。
    pub fn set_last_model(&mut self, id: &str, model: &str) {
        if let Some(profile) = self.get_mut(id) {
            profile.last_model = Some(model.trim().to_string());
        }
    }
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
    /// 配色（暖纸 / 墨纸 / 玫瑰松 / 常青 / 北境 / 拿铁摩卡），每套都有浅深两版
    pub theme: ThemeChoice,
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
    /// 「自动」模式：开不开、分类器用哪个模型、信任边界（默认关）
    pub auto_mode: AutoModeSettings,
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
    /// 回复里的网络图片自动加载（默认关）。关着时 `![](https://…)` 先摆占位卡
    /// 「图片 · example.com」+「加载」，点了才请求——不点就把用户的 IP 和
    /// 「正在看这条回复」告诉了图片服务器（见「设计说明-聊天与Code分离.md」7.5 节）
    pub auto_load_images: bool,
    /// 输入框的 Tab 补全：停顿一下就让模型猜一句下文，灰字画在光标后面。
    /// 默认关：每次停顿都是一次请求，开之前最好在 `suggest_model` 填个便宜的小模型
    pub suggestions: bool,
    /// 一轮回复结束后猜「用户下一句会说什么」，空输入框里用淡色显示，按 Tab 填入。
    /// 和 `suggestions` 各自独立：那个跟着打字走，这个每轮只猜一次
    pub next_suggestion: bool,
    /// 补全用的模型（同一端点）；空 = 跟会话的模型
    pub suggest_model: String,
    /// 子任务（自研内核的 `task`）用的模型（同一端点）；空 = 跟主会话。插件角色自己写了模型的以角色为准
    pub subagent_model: String,
    /// 一轮结束后让模型提炼「候选」记忆（只进设置页「待确认」，收下才落盘）。默认关：
    /// 每一轮都多一次请求
    pub memory_suggest: bool,
    /// 当前空间（侧栏顶部「聊天 | 代码」）；切换即写回，下次启动回到上次那个。
    /// 默认 Code：升级后打开还是熟悉的样子
    pub space: Space,
    /// 被端点回过 400、确认看不了图片的模型（下次直接按「看不了」处理）。
    /// 自动记的，设置页模型那一行能看到、能清掉（万一是误判）
    pub no_vision_models: Vec<String>,
    /// 聊天空间的一组设置（见 [`ChatSettings`]）
    pub chat: ChatSettings,
}

/// 当前空间：侧栏顶部「聊天 | 代码」那段切换。
///
/// 和 [`claudesktop_core::SessionKind`] 一一对应，但那是会话的类型（存进元信息），
/// 这是界面此刻在看哪个空间（存进设置），两者的默认值也不同（这里默认 Code）。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Space {
    /// 写代码的会话（有工作目录、全套工具）
    #[default]
    Code,
    /// 纯聊天
    Chat,
}

impl Space {
    pub fn kind(self) -> claudesktop_core::SessionKind {
        match self {
            Self::Code => claudesktop_core::SessionKind::Code,
            Self::Chat => claudesktop_core::SessionKind::Chat,
        }
    }

    pub fn of(kind: claudesktop_core::SessionKind) -> Self {
        match kind {
            claudesktop_core::SessionKind::Code => Self::Code,
            claudesktop_core::SessionKind::Chat => Self::Chat,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Code => "代码",
            Self::Chat => "聊天",
        }
    }
}

/// 聊天空间（见「设计说明-聊天与Code分离.md」第五章）。
///
/// 三项都只影响**之后新建**的聊天会话——说明例外：它每轮重读，改了下一轮就生效。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ChatSettings {
    /// 聊天用的模型（同一服务商）；空 = 跟随档案里的默认模型
    pub model: String,
    /// 新建的聊天默认开着「联网」（输入框里可以随时关）
    pub web: bool,
    /// 聊天说明文件的路径（每轮读一次，进系统提示词的 `# User instructions`）
    pub instructions: String,
}

impl Default for ChatSettings {
    /// `web` 默认开（`bool::default()` 是 false，不能只靠派生）
    fn default() -> Self {
        Self { model: String::new(), web: true, instructions: String::new() }
    }
}

impl ChatSettings {
    /// 聊天说明文件的路径；空 = 没有。
    pub fn instructions_path(&self) -> Option<PathBuf> {
        let p = self.instructions.trim();
        (!p.is_empty()).then(|| PathBuf::from(p))
    }
}

/// `bool` 字段的默认值为 true（`bool::default()` 是 false，不能只靠派生）。
fn yes() -> bool {
    true
}

/// 这份档案是不是 DeepSeek 官方端点（`web_search` 的 DeepSeek 后端要复用它的 key）。
/// 只认 host 是 `api.deepseek.com` 的：搜索请求固定发往官方，中转站的 key 发过去既用不了，
/// 也等于把凭据交给了签发方以外的服务器；名字叫 DeepSeek 不算数。
pub fn is_deepseek_endpoint(p: &ProviderProfile) -> bool {
    let url = p.base_url.trim().to_ascii_lowercase();
    let rest = url.strip_prefix("https://").or_else(|| url.strip_prefix("http://")).unwrap_or(&url);
    let host = rest.split(['/', '?', '#']).next().unwrap_or_default();
    // 去掉 userinfo 和端口
    let host = host.rsplit('@').next().unwrap_or_default();
    let host = host.split(':').next().unwrap_or_default();
    host == "api.deepseek.com"
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            kernel: None,
            builtin: BuiltinSettings::default(),
            acp: AcpSettings::default(),
            view_mode: ViewMode::default(),
            appearance: Appearance::default(),
            theme: ThemeChoice::default(),
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
            // 默认关：远程图片会把用户的 IP 和「正在看这条回复」告诉图片服务器
            auto_load_images: false,
            suggestions: false,
            next_suggestion: false,
            suggest_model: String::new(),
            subagent_model: String::new(),
            memory_suggest: false,
            auto_mode: AutoModeSettings::default(),
            space: Space::default(),
            no_vision_models: Vec::new(),
            chat: ChatSettings::default(),
        }
    }
}

/// 「自动」模式（权限模式里由分类器代替用户审批）的设置。
///
/// 默认关。开着时必须配好：要么跟着当前服务商和会话模型走（`use_session_model`），
/// 要么自己指定一个模型（省钱 / 快的分类器小模型）。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct AutoModeSettings {
    /// 打开「自动」模式（默认关）
    pub enabled: bool,
    /// 分类器跟着当前服务商的模型走；false = 用下面的 `model` 和 `/model` 菜单里挑的分类模型
    pub use_session_model: bool,
    /// 分类器用哪个模型（同一服务商）；空 = 没配
    pub model: String,
    /// 信任边界（自然语言，进分类器的系统提示词）；空 = 只有当前仓库算内部
    pub environment: String,
}

impl Default for AutoModeSettings {
    fn default() -> Self {
        Self {
            enabled: false,
            // 默认跟会话模型走：开关一开就能用，不用再挑一次模型
            use_session_model: true,
            model: String::new(),
            environment: String::new(),
        }
    }
}

impl AutoModeSettings {
    /// 配置是否齐全（开着的时候必须齐全，否则保存就报错）。
    pub fn is_configured(&self, profile: &ProviderProfile) -> bool {
        if !self.enabled {
            return true;
        }
        if self.use_session_model {
            // 跟着会话走：当前档案本身配好就行
            return profile.is_configured();
        }
        profile.is_configured() && !self.model.trim().is_empty()
    }

    /// 交给自己研内核的配置；没开就 None（「自动」模式不出现）。
    pub fn to_config(&self, profile: &ProviderProfile) -> Option<AutoModeConfig> {
        if !self.enabled || !self.is_configured(profile) {
            return None;
        }
        Some(AutoModeConfig {
            environment: self.environment.trim().to_string(),
            model: (!self.use_session_model).then(|| ProviderConfig {
                model: self.model.trim().to_string(),
                ..profile.provider_config()
            }),
        })
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

/// 配色：顺序和 `theme::PALETTES` 一致
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ThemeChoice {
    #[default]
    Warm,
    Flexoki,
    RosePine,
    Everforest,
    Nord,
    Catppuccin,
    Github,
    Pure,
}

impl ThemeChoice {
    pub const ALL: [ThemeChoice; 8] = [
        Self::Warm,
        Self::Flexoki,
        Self::RosePine,
        Self::Everforest,
        Self::Nord,
        Self::Catppuccin,
        Self::Github,
        Self::Pure,
    ];

    pub fn index(self) -> usize {
        self as usize
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Warm => "暖纸",
            Self::Flexoki => "墨纸",
            Self::RosePine => "玫瑰松",
            Self::Everforest => "常青",
            Self::Nord => "北境",
            Self::Catppuccin => "拿铁 / 摩卡",
            Self::Github => "GitHub",
            Self::Pure => "纯白",
        }
    }

    pub fn desc(self) -> &'static str {
        match self {
            Self::Warm => "Claude 默认，暖纸底和赤陶橙",
            Self::Flexoki => "Flexoki · 纸和墨，青绿点缀",
            Self::RosePine => "Rosé Pine · 低饱和的玫瑰和紫灰",
            Self::Everforest => "Everforest · 米黄底配灰绿，护眼",
            Self::Nord => "Nord · 雪白和冰蓝，冷色",
            Self::Catppuccin => "Catppuccin · 粉彩紫",
            Self::Github => "GitHub · 纯白底配浅灰和链接蓝",
            Self::Pure => "纯白 · 只有灰阶，没有彩色强调",
        }
    }

    pub fn parse(v: &str) -> Option<Self> {
        Some(match v.trim().to_ascii_lowercase().as_str() {
            "warm" => Self::Warm,
            "flexoki" => Self::Flexoki,
            "rose_pine" | "rosepine" => Self::RosePine,
            "everforest" => Self::Everforest,
            "nord" => Self::Nord,
            "catppuccin" => Self::Catppuccin,
            "github" => Self::Github,
            "pure" | "pure_white" | "purewhite" => Self::Pure,
            _ => return None,
        })
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
        // 请求参数：老设置文件里的「思考预算」折算成档位（`thinking` 已经有了就不动，
        // 那说明是手改过的新格式）；服务端工具里认不得的名字丢掉（手写错的、以后版本删掉的）
        for profile in &mut s.builtin.profiles {
            let params = &mut profile.params;
            if params.thinking.is_none()
                && let Some(budget) = params.thinking_budget.filter(|b| *b > 0)
            {
                params.thinking = Some(budget_to_level(budget));
            }
            // 折算完就清掉：这个字段只读兼容，不再写盘
            params.thinking_budget = None;
            params
                .server_tools
                .retain(|name| claudesktop_builtin::RequestParams::knows_server_tool(name));
            // 去重（顺序不重要：界面按 `SERVER_TOOLS` 的顺序摆开关）
            params.server_tools.sort();
            params.server_tools.dedup();
        }
        // 老设置里的单条 ACP 命令 → 一条自定义档案
        s.acp.migrate();
        // 档案 ID 不能重（手改过文件时会撞）：留第一个
        let mut seen = std::collections::HashSet::new();
        s.acp.agents.retain(|a| !a.id.trim().is_empty() && seen.insert(a.id.clone()));
        // 老设置的 `"kernel": "acp"`（没有档案 ID）、或选的档案已经没了 → 第一个档案，一个都没有就自研
        if let Some(KernelChoice::Acp(id)) = &s.kernel
            && s.acp.get(id).is_none()
        {
            s.kernel = Some(s.kernel_for(&KernelChoice::Acp(String::new())));
        }
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
        if let Some(k) = var("CLAUDESKTOP_KERNEL").and_then(|v| KernelChoice::parse(&v, &self.acp))
        {
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
            // 环境变量还能覆盖启动命令：当成一条自定义档案（老用户脚本 / 调试用）。
            // 已经有同一条命令的档案就不再加：设置页保存会把它落盘，每次启动都加就越积越多
            if !self.acp.agents.iter().any(|a| a.command.as_deref() == Some(v.trim())) {
                self.acp.add_custom(&v);
            }
        }
        if let Some(v) = var("CLAUDESKTOP_GATEWAY") {
            self.usage_gateway = v.trim() != "0";
        }
        if let Some(v) = var("CLAUDESKTOP_MAX_STEPS").and_then(|v| v.parse::<usize>().ok()) {
            self.max_steps =
                v.clamp(claudesktop_builtin::MIN_MAX_STEPS, claudesktop_builtin::MAX_MAX_STEPS);
        }
        if let Some(t) = var("CLAUDESKTOP_THEME").and_then(|v| ThemeChoice::parse(&v)) {
            self.theme = t;
        }
        self
    }

    /// 实际使用的默认内核。从没选过时：有启用的 ACP 档案且自研内核没配好就用第一个档案，否则自研。
    pub fn kernel(&self) -> KernelChoice {
        self.kernel.clone().unwrap_or(
            if self.acp.first().is_none() || self.builtin.is_configured() {
                KernelChoice::Builtin
            } else {
                KernelChoice::Acp(self.acp_default_id().expect("first() 非空"))
            },
        )
    }

    /// 某一种内核的档案 ID：ACP 用第一个启用的档案。
    pub fn acp_default_id(&self) -> Option<String> {
        self.acp.first().map(|a| a.id.clone())
    }

    /// 「默认内核」三选一落到具体内核：ACP 只选了种类没选档案时用第一个启用的档案。
    /// 一家都没启用时退回自研（设置页选了 agent 但一条档案都没勾）。
    pub fn kernel_for(&self, kind: &KernelChoice) -> KernelChoice {
        match kind {
            KernelChoice::Acp(id) if id.is_empty() => {
                self.acp_default_id().map(KernelChoice::Acp).unwrap_or(KernelChoice::Builtin)
            }
            other => other.clone(),
        }
    }

    /// 新装、还没配过任何内核：没选过内核，自研内核没填端点 / 模型，也没有启用的 agent。
    /// 这时不起内核（ACP 空命令是仓库里的 mock agent，装好的程序里没有），首页换成配置引导。
    pub fn needs_setup(&self) -> bool {
        self.kernel.is_none() && !self.builtin.is_configured() && self.acp.first().is_none()
    }

    /// 能不能开聊天会话：聊天直接连模型（ACP agent 都是写代码的，不能聊天），
    /// 所以自研内核的端点 / 模型填好了才行。只配了 agent 时聊天空间换成配置引导（见 `overview.rs`）。
    pub fn chat_available(&self) -> bool {
        self.builtin.is_configured()
    }

    /// 某个空间新会话用哪套内核：聊天空间只能是自研内核（ACP 都跑不了聊天）；代码空间照旧跟着设置。
    pub fn kernel_for_space(&self, space: Space) -> KernelChoice {
        match space {
            Space::Chat => KernelChoice::Builtin,
            Space::Code => self.kernel(),
        }
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
        // 端点回过 400 的看不了图的模型：历史里的图片换成占位，不让整个会话卡住
        config.no_vision_models = self.no_vision_models.clone();
        config.max_steps = self.max_steps;
        config.provider.gateway = self.gateway();
        // `web_search` 的 DeepSeek 后端复用哪把 key（见 `deepseek_search_key`）
        config.provider.deepseek_key = self.deepseek_search_key();
        // 每轮结束提炼候选记忆（默认关，见 `remember` 模块）
        config.memory_suggest = self.memory_suggest;
        // 子任务用的模型（空 = 跟主会话）
        config.subagent_model =
            Some(self.subagent_model.trim().to_string()).filter(|m| !m.is_empty());
        // 「自动」模式没配好时不出现（设置页保存时会拦住，这里再兜一次）
        config.auto_mode = self.auto_mode.to_config(p);
        // 聊天空间：模型（空 = 跟档案）、默认联网、聊天说明文件
        config.chat_model = Some(self.chat.model.trim().to_string()).filter(|m| !m.is_empty());
        config.chat_web = self.chat.web;
        config.chat_instructions = self.chat.instructions_path();
        Ok(config)
    }

    /// `web_search` 的 DeepSeek 后端用哪把 key：找一份 DeepSeek 官方端点的档案
    /// （见 [`is_deepseek_endpoint`]）的 key，交给内核转给 `websearch`。
    /// 找不到就是 None——搜索时 DeepSeek 那一档按「没配 key」跳过，不报错。
    pub fn deepseek_search_key(&self) -> Option<String> {
        self.builtin
            .profiles
            .iter()
            .filter(|p| is_deepseek_endpoint(p))
            .map(|p| p.api_key.trim().to_string())
            .find(|k| !k.is_empty())
    }

    /// 自研内核上新建的对话是否默认用「自动」模式（设置里开着并且配好了）。
    /// 只对自研内核有意义：它的模式列表里才有「自动」。
    pub fn auto_mode_default(&self) -> bool {
        self.auto_mode.to_config(self.builtin.current()).is_some()
    }

    /// 用量网关（开着才有；进程里共用一个，第一次用到时起）。起不来就直连，不影响用。
    pub fn gateway(&self) -> Option<claudesktop_builtin::gateway::GatewayHandle> {
        use claudesktop_builtin::gateway;
        claudesktop_builtin::request_bodies::set_enabled(self.gateway_bodies);
        self.usage_gateway.then(gateway::default_dir).flatten().and_then(|d| gateway::shared(&d))
    }

    /// ACP 配置：按档案填启动命令；开了「经过用量网关」且这一家认那两个环境变量时给 agent 设好 base URL。
    /// `profile` 为 None 时不动命令（沿用 `base` 里的，即环境变量 / mock agent）。
    pub fn acp_config_for(
        &self,
        mut base: AcpKernelConfig,
        profile: Option<&AgentProfile>,
    ) -> AcpKernelConfig {
        if let Some(profile) = profile {
            let command = profile.command_for(cfg!(windows));
            if !command.trim().is_empty() {
                base.command = command;
            }
            // 检测时也找了 PATH 以外的常见安装目录（从图形界面起来的进程 PATH 常常比终端里短）：
            // 补进 agent 进程的 PATH，否则「检测到」的 agent（和它要的 node）照样起不来
            let extra: Vec<PathBuf> = claudesktop_core::agents::common_install_dirs()
                .into_iter()
                .filter(|d| d.is_dir())
                .collect();
            if !extra.is_empty()
                && !base.env.iter().any(|(k, _)| k.eq_ignore_ascii_case("PATH"))
                && let Ok(path) = std::env::join_paths(
                    claudesktop_core::agents::search_paths().into_iter().chain(extra),
                )
            {
                base.env.push(("PATH".into(), path.to_string_lossy().into_owned()));
            }
            if profile.gateway
                && profile.supports_gateway()
                && let Some(gateway) = self.gateway()
            {
                base.env.extend(claudesktop_builtin::gateway::agent_env(&gateway));
            }
        }
        base
    }
}

/// 写进会话元信息的 ACP agent 标识：`acp:<档案 ID>:<实际启动命令>`。
/// 重开历史会话时按档案 ID 归到对应的 agent（[`KernelChoice::of_agent`]），续接时整串比对（命令变了就不续接）。
pub fn acp_agent_id(profile: &str, command: &str) -> String {
    format!("acp:{profile}:{command}")
}

/// agent 标识里的启动命令（老会话的标识就是命令本身）。
pub fn agent_command(agent: &str) -> &str {
    match agent.strip_prefix("acp:").and_then(|rest| rest.split_once(':')) {
        Some((_, command)) => command,
        None => agent,
    }
}

/// 元信息里记的 agent 和现在这个是不是同一个：新格式整串比；
/// 老会话（只记了命令）比命令，升级后单条命令迁成的 `custom-1` 还能续接。
pub fn same_acp_agent(recorded: &str, current: &str) -> bool {
    if recorded.starts_with("acp:") {
        recorded == current
    } else {
        !recorded.is_empty() && recorded == agent_command(current)
    }
}

/// 老的「思考预算」折算成档位（只用于读旧设置文件）：2048 / 8192 / 16000 三档的分界。
fn budget_to_level(budget: u64) -> ThinkingLevel {
    match budget {
        b if b <= 4_096 => ThinkingLevel::Low,
        b if b <= 12_000 => ThinkingLevel::Medium,
        _ => ThinkingLevel::High,
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
                thinking: None,
                thinking_budget: None,
                server_tools: Vec::new(),
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

    /// 配色：旧设置文件读出默认的暖纸，写盘回读一致。
    #[core::prelude::v1::test]
    fn theme_choice_default_and_roundtrip() {
        let s: Settings = serde_json::from_str("{}").unwrap();
        assert_eq!(s.theme, ThemeChoice::Warm);

        let dir = std::env::temp_dir().join(format!("claudesktop-theme-{}", std::process::id()));
        let path = dir.join("settings.json");
        let s = Settings { theme: ThemeChoice::RosePine, ..Default::default() };
        s.save(&path).unwrap();
        assert_eq!(Settings::load(Some(&path)).0.theme, ThemeChoice::RosePine);
        // 文件里是 key（snake_case），不是枚举名
        assert!(std::fs::read_to_string(&path).unwrap().contains("\"rose_pine\""));
        let _ = std::fs::remove_dir_all(&dir);

        assert_eq!(ThemeChoice::parse("Nord"), Some(ThemeChoice::Nord));
        assert_eq!(ThemeChoice::parse("rosepine"), Some(ThemeChoice::RosePine));
        assert_eq!(ThemeChoice::parse("GitHub"), Some(ThemeChoice::Github));
        assert_eq!(ThemeChoice::parse("pure"), Some(ThemeChoice::Pure));
        assert_eq!(ThemeChoice::parse("nope"), None);
        // 新加的配色也走一遍 key 往返（顺序和 `theme::PALETTES` 对齐）
        for (t, key) in [(ThemeChoice::Github, "github"), (ThemeChoice::Pure, "pure")] {
            assert_eq!(ThemeChoice::parse(key), Some(t));
            let s = Settings { theme: t, ..Default::default() };
            s.save(&path).unwrap();
            assert!(std::fs::read_to_string(&path).unwrap().contains(&format!("\"{key}\"")));
            assert_eq!(Settings::load(Some(&path)).0.theme, t);
        }
        let _ = std::fs::remove_dir_all(&dir);
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

    /// 聊天空间：老设置文件里没有这两项 → 代码空间 + 联网默认开；三项都进自研内核配置。
    #[core::prelude::v1::test]
    fn chat_space_defaults_and_config() {
        let s: Settings = serde_json::from_str("{}").unwrap();
        assert_eq!(s.space, Space::Code);
        assert!(s.chat.web);
        assert!(s.chat.model.is_empty());
        assert!(s.chat.instructions_path().is_none());

        let mut s = Settings::default();
        s.builtin.current_mut().base_url = "http://x/v1".into();
        s.builtin.current_mut().model = "m".into();
        s.chat.model = "cheap".into();
        s.chat.web = false;
        s.chat.instructions = "  C:\\notes.md  ".into();
        let cfg = s.builtin_config(PathBuf::from(".")).unwrap();
        assert_eq!(cfg.chat_model.as_deref(), Some("cheap"));
        assert!(!cfg.chat_web);
        assert_eq!(cfg.chat_instructions, Some(PathBuf::from("C:\\notes.md")));

        // 聊天只能用自研内核；代码空间跟着「默认内核」
        s.space = Space::Chat;
        assert_eq!(s.kernel_for_space(Space::Chat), KernelChoice::Builtin);
        s.space = Space::Code;
        assert_eq!(s.kernel_for_space(Space::Code), KernelChoice::Builtin);
        // 写盘回读
        let dir = std::env::temp_dir().join(format!("claudesktop-space-{}", std::process::id()));
        let path = dir.join("settings.json");
        s.space = Space::Chat;
        s.save(&path).unwrap();
        let back = Settings::load(Some(&path)).0;
        assert_eq!(back.space, Space::Chat);
        assert!(!back.chat.web);
        assert_eq!(back.chat.model, "cheap");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 输入补全每次停顿都是一次请求：没写过这一项的设置文件里是关着的，补全模型留空（跟会话）。
    #[core::prelude::v1::test]
    fn suggestions_are_off_by_default() {
        let s: Settings = serde_json::from_str("{}").unwrap();
        assert!(!s.suggestions);
        assert!(!s.next_suggestion);
        assert!(s.suggest_model.is_empty());
        assert!(s.subagent_model.is_empty());
    }

    /// 「自动」模式：默认关；开着时必须配好（跟会话走 = 档案配好即可，自选模型 = 要填模型名），
    /// 没配好就不交给内核（模式里不出现）。
    #[core::prelude::v1::test]
    fn auto_mode_is_off_by_default_and_needs_a_model_when_on() {
        // 旧设置文件没有这一项：关着、跟会话模型走
        let s: Settings = serde_json::from_str("{}").unwrap();
        assert!(!s.auto_mode.enabled);
        assert!(s.auto_mode.use_session_model);
        assert_eq!(s.auto_mode.to_config(s.builtin.current()), None);

        let mut s = Settings::default();
        s.builtin.current_mut().base_url = "http://x/v1".into();
        s.builtin.current_mut().model = "big".into();
        let profile = s.builtin.current().clone();

        // 开着 + 跟会话：能用，分类器不单独指定模型
        s.auto_mode.enabled = true;
        assert!(s.auto_mode.is_configured(&profile));
        let config = s.auto_mode.to_config(&profile).unwrap();
        assert_eq!(config.model, None);
        assert_eq!(config.environment, "");

        // 开着 + 自选模型但没填：配不齐，不交给内核
        s.auto_mode.use_session_model = false;
        assert!(!s.auto_mode.is_configured(&profile));
        assert_eq!(s.auto_mode.to_config(&profile), None);

        // 填了模型就能用，端点沿用当前档案
        s.auto_mode.model = "small".into();
        s.auto_mode.environment = "  Trusted: github.com/acme  ".into();
        let config = s.auto_mode.to_config(&profile).unwrap();
        assert_eq!(config.model.as_ref().map(|p| p.model.as_str()), Some("small"));
        assert_eq!(config.model.as_ref().map(|p| p.base_url.as_str()), Some("http://x/v1"));
        assert_eq!(config.environment, "Trusted: github.com/acme");

        // 落到内核配置里
        assert!(s.builtin_config(PathBuf::from(".")).unwrap().auto_mode.is_some());
        s.auto_mode.enabled = false;
        assert!(s.builtin_config(PathBuf::from(".")).unwrap().auto_mode.is_none());
    }

    /// 老的「思考预算」读出来换算成档位，写回时不再有 `thinking_budget`。
    #[core::prelude::v1::test]
    fn legacy_thinking_budget_becomes_a_level() {
        let dir = std::env::temp_dir().join(format!("claudesktop-thinking-{}", std::process::id()));
        let path = dir.join("settings.json");
        std::fs::create_dir_all(&dir).unwrap();

        // 8000 → 中档；0 / 没写 → 不指定
        for (budget, level) in [
            ("8000", Some(ThinkingLevel::Medium)),
            ("2000", Some(ThinkingLevel::Low)),
            ("16000", Some(ThinkingLevel::High)),
            ("0", None),
        ] {
            let text = format!(
                r#"{{"builtin":{{"profiles":[{{"base_url":"http://x/v1","model":"m",
                   "params":{{"thinking_budget":{budget}}}}}]}}}}"#
            );
            std::fs::write(&path, text).unwrap();
            let (s, err) = Settings::load(Some(&path));
            assert!(err.is_none(), "{err:?}");
            let params = &s.builtin.current().params;
            assert_eq!(params.thinking, level, "{budget}");
            assert_eq!(params.thinking_budget, None, "{budget}");

            // 往返一次：文件里不再有 thinking_budget
            s.save(&path).unwrap();
            let raw = std::fs::read_to_string(&path).unwrap();
            assert!(!raw.contains("thinking_budget"), "{raw}");
        }

        // 新格式（已经有 thinking）优先，老字段即使还在也不覆盖
        std::fs::write(
            &path,
            r#"{"builtin":{"profiles":[{"base_url":"http://x/v1","model":"m",
               "params":{"thinking":"high","thinking_budget":2048}}]}}"#,
        )
        .unwrap();
        let s = Settings::load(Some(&path)).0;
        assert_eq!(s.builtin.current().params.thinking, Some(ThinkingLevel::High));
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

        // 填了 ACP 命令（含 ACP_AGENT 带进来的）→ 归进第一个启用的档案
        let mut s = Settings::default();
        s.acp.command = "npx agent".into();
        s.acp.migrate();
        assert_eq!(s.kernel(), KernelChoice::Acp("custom-1".into()));
        assert!(!s.needs_setup());

        let mut s = Settings::default();
        s.builtin.current_mut().base_url = "http://x/v1".into();
        s.builtin.current_mut().model = "m".into();
        // 没选过内核但配好了自研内核 → 用它
        assert_eq!(s.kernel(), KernelChoice::Builtin);
        assert!(!s.needs_setup());
        // 两个都配了：自研优先，和以前一样
        s.acp.command = "npx agent".into();
        s.acp.migrate();
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

    /// `web_search` 的 DeepSeek key：只从 DeepSeek 官方端点的档案里挑，挑不到就是 None。
    #[core::prelude::v1::test]
    fn deepseek_search_key_reuses_a_deepseek_profile() {
        let profile = |name: &str, base: &str, key: &str| ProviderProfile {
            name: name.into(),
            base_url: base.into(),
            api_key: key.into(),
            model: "m".into(),
            ..ProviderProfile::default()
        };
        let mut s = Settings::default();
        s.builtin.profiles = vec![
            profile("Kimi", "https://api.moonshot.cn/v1", "sk-kimi"),
            profile("官方", "https://api.deepseek.com/v1", "sk-official"),
            profile("官方 2", "https://API.DeepSeek.com:443", "sk-second"),
        ];
        // 官方 host 就认（大小写、端口、路径无所谓），第一次找到的那个
        assert_eq!(s.deepseek_search_key().as_deref(), Some("sk-official"));
        s.builtin.profiles.remove(1);
        assert_eq!(s.deepseek_search_key().as_deref(), Some("sk-second"));
        // 中转站（路径或子域名里带 deepseek）、只是名字叫 DeepSeek 的：都不认，key 不能发给官方
        s.builtin.profiles = vec![
            profile("中转", "https://relay.example/deepseek", "sk-relay"),
            profile("中转 2", "https://api.deepseek.com.relay.example/v1", "sk-relay2"),
            profile("DeepSeek", "https://api.example/v1", "sk-named"),
        ];
        assert_eq!(s.deepseek_search_key(), None);
        // 只有别的端点：None（搜索时 DeepSeek 那一档按「没配 key」跳过）
        s.builtin.profiles = vec![profile("Kimi", "https://api.moonshot.cn/v1", "sk-kimi")];
        assert_eq!(s.deepseek_search_key(), None);
        // 认出来了但 key 是空的，同样当作没有
        s.builtin.profiles = vec![profile("DeepSeek", "https://api.deepseek.com", "")];
        assert_eq!(s.deepseek_search_key(), None);
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
        let acp = AcpSettings::default();
        let acp = &acp;
        assert_eq!(KernelChoice::of_agent("builtin:http://x/v1#m", acp), KernelChoice::Builtin);
        assert_eq!(
            KernelChoice::of_agent("builtin:anthropic:http://x/v1#m", acp),
            KernelChoice::Builtin
        );
        // 老格式：命令本身就是标识，归进第一个档案
        assert_eq!(
            KernelChoice::of_agent("node mock.mjs", acp),
            KernelChoice::Acp("custom-1".into())
        );
        assert_eq!(KernelChoice::of_agent("", acp), KernelChoice::Demo);
        // 槽 key 互不相同
        let keys: Vec<String> = vec![
            KernelChoice::Builtin.key(),
            KernelChoice::Demo.key(),
            KernelChoice::Acp("x".into()).key(),
        ];
        assert_eq!(keys.len(), keys.iter().collect::<std::collections::BTreeSet<_>>().len());

        // 多家 agent：新格式按档案 ID 归，不是一律给第一家
        let mut acp = AcpSettings::default();
        acp.agents.push(AgentProfile::custom(1, "agent-a"));
        acp.agents.push(AgentProfile::custom(2, "agent-b"));
        let b = acp_agent_id("custom-2", "agent-b");
        assert_eq!(KernelChoice::of_agent(&b, &acp), KernelChoice::Acp("custom-2".into()));
        // 老格式按命令找档案
        assert_eq!(KernelChoice::of_agent("agent-b", &acp), KernelChoice::Acp("custom-2".into()));
        assert_eq!(agent_command(&b), "agent-b");
        assert!(same_acp_agent(&b, &b));
        assert!(same_acp_agent("agent-b", &b), "升级前的会话还能续接");
        assert!(!same_acp_agent(&acp_agent_id("custom-1", "agent-a"), &b));
    }

    /// 老设置文件里的 `"kernel": "acp"`（没有档案 ID）不能把整份设置读坏。
    #[core::prelude::v1::test]
    fn legacy_acp_kernel_loads() {
        let legacy = r#"{"kernel":"acp","acp":{"command":"npx agent"},
            "builtin":{"base_url":"http://x/v1","api_key":"k","model":"m"}}"#;
        let dir =
            std::env::temp_dir().join(format!("claudesktop-legacy-acp-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("settings.json");
        std::fs::write(&path, legacy).unwrap();
        let (s, err) = Settings::load(Some(&path));
        assert!(err.is_none(), "{err:?}");
        assert_eq!(s.builtin.current().api_key, "k");
        assert_eq!(s.kernel, Some(KernelChoice::Acp("custom-1".into())));
        // 存回去再读：新格式也能来回
        s.save(&path).unwrap();
        assert_eq!(Settings::load(Some(&path)).0, s);
        // 没有档案可落：退回自研
        std::fs::write(&path, r#"{"kernel":"acp"}"#).unwrap();
        assert_eq!(Settings::load(Some(&path)).0.kernel, Some(KernelChoice::Builtin));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
