//! Provider 抽象：协议无关的消息 / 事件类型与 [`ChatProvider`] trait。
//!
//! 当前实现：
//! - [`crate::openai::OpenAiProvider`]：OpenAI 兼容 Chat Completions（一个协议覆盖
//!   OpenAI / DeepSeek / GLM / Kimi / OpenRouter / Ollama / vLLM 等绝大多数端点）
//! - [`crate::anthropic::AnthropicProvider`]：Anthropic 原生 `/v1/messages`
//!   （思考块、tool_use、提示缓存）
//!
//! 消息（[`ChatMessage`]）对上承载多模态分段（文本 / 图片），对下由各实现
//! 翻译成自己协议的请求体；工具定义统一用 OpenAI 的 JSON 形状传入。

use crate::gateway::Route;
use serde::{Serialize, Serializer};
use serde_json::Value;
use std::io::{BufRead, BufReader};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

/// 还没收到任何输出时，这些状态码按瞬态错误退避重试。
pub(crate) const RETRYABLE: [u16; 5] = [429, 500, 502, 503, 504];
/// 重试等待（毫秒）；长度即最大重试次数。
pub(crate) const BACKOFF_MS: [u64; 2] = [1_000, 3_000];

/// 端点使用的协议。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum Protocol {
    /// OpenAI 兼容 Chat Completions（默认）
    #[serde(rename = "openai")]
    #[default]
    OpenAiCompat,
    /// Anthropic 原生 Messages API
    #[serde(rename = "anthropic")]
    Anthropic,
    /// OpenAI Responses API（`POST /responses`，思考摘要与内置工具）
    #[serde(rename = "responses")]
    Responses,
}

impl Protocol {
    pub fn label(self) -> &'static str {
        match self {
            Self::OpenAiCompat => "OpenAI 兼容",
            Self::Anthropic => "Anthropic 原生",
            Self::Responses => "OpenAI Responses",
        }
    }
}

/// 透传给 provider 的请求参数（设置页可配）。
#[derive(Debug, Clone, Default, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct RequestParams {
    pub temperature: Option<f64>,
    pub max_tokens: Option<u64>,
    /// 思考强度。`None` = 不指定（请求里不带思考字段，用模型自己的默认）；
    /// `Some(ThinkingLevel::Off)` = 明确关掉。由 [`apply_thinking`] 按协议翻译成请求字段
    pub thinking: Option<ThinkingLevel>,
    /// 旧字段：只读兼容老设置文件（加载时折算进 `thinking`，见桌面端的 `Settings::normalize`）。
    /// 不再发进任何请求体
    #[serde(skip_serializing)]
    pub thinking_budget: Option<u64>,
    /// 用服务端工具（Anthropic 官方 `web_search` / `web_fetch` / `code_execution`，见
    /// [`crate::anthropic::SERVER_TOOLS`]）；只有 Anthropic 原生协议认
    pub server_tools: Vec<String>,
    /// 额外的请求体字段（JSON 对象）；浅合并进请求体，同名键覆盖默认值
    pub extra: Value,
}

impl RequestParams {
    /// `thinking.budget_tokens` 的官方下限：小于它的请求 Anthropic 直接拒（400）。
    /// 只在旧格式（[`ThinkingFormat::AnthropicBudget`]）的编码里用。
    pub const MIN_THINKING_BUDGET: u64 = 1_024;

    pub fn is_empty(&self) -> bool {
        self.temperature.is_none()
            && self.max_tokens.is_none()
            && self.thinking.is_none()
            && self.thinking_budget.is_none()
            && self.server_tools.is_empty()
            && self.extra.as_object().is_none_or(|o| o.is_empty())
    }

    /// 可选的服务端工具（内部名字 + 一句说明），设置页「服务端工具」的多选项用它。
    /// 名单本身在 [`crate::anthropic::SERVER_TOOLS`]（带版本号的请求体片段也在那里）。
    pub fn server_tool_choices() -> impl Iterator<Item = (&'static str, &'static str)> {
        crate::anthropic::SERVER_TOOLS.iter().map(|tool| (tool.name, tool.desc))
    }

    /// 名字是不是认得的服务端工具（设置文件里手写错的、以后被删掉的都当不存在）
    pub fn knows_server_tool(name: &str) -> bool {
        crate::anthropic::SERVER_TOOLS.iter().any(|tool| tool.name == name)
    }
}

/// 思考强度档位：对外的唯一选项，预算 / effort 只是翻译出来的值。
///
/// 顺序即强弱（`Ord` 用来降级，见 [`resolve_level`]）。`Off` 最小、`XHigh` 最大。
/// 比 `"默认"` 那个 `None` 多一档「兜底」：
/// `Default` 不是「什么都不发」（那是 `None`），它表示**能力表里没有的模型也按
/// [`FALLBACK_LEVELS`] 发 effort**，认得的模型上则落到最低一档（见 [`ThinkingSpec::resolve`]）。
#[derive(
    Debug,
    Clone,
    Copy,
    Default,
    PartialEq,
    Eq,
    PartialOrd,
    Ord,
    serde::Serialize,
    serde::Deserialize,
)]
#[serde(rename_all = "snake_case")]
pub enum ThinkingLevel {
    /// 明确关掉思考（不带字段 = 用模型默认，那是 `None`，不是这一档）
    Off,
    Minimal,
    #[default]
    Low,
    Medium,
    High,
    XHigh,
    /// `max`：DeepSeek / GLM / Kimi K3 / MiniMax / Doubao / Grok 的最高档。
    ///
    /// 它们没有 `xhigh`，最高就是 `max`，所以「极高」在这些模型上会被 `resolve` 降成一档
    /// 用不上它。给成一档之后，选「最高」才真的发 `max`。
    Max,
    /// 「默认（兜底）」：能力表里没有的模型也发 effort（见 [`FALLBACK_LEVELS`]）。
    ///
    /// 排在最后是为了让 `Ord` 仍然是「越大越强」的顺序——它不是一档强度，
    /// 用 `resolve` 换成了具体档位，别的按强度比大小的代码不会撞上它
    Default,
}

impl ThinkingLevel {
    /// 除「关」以外的档位（按强弱）。
    pub const ON: [ThinkingLevel; 6] = [
        ThinkingLevel::Minimal,
        ThinkingLevel::Low,
        ThinkingLevel::Medium,
        ThinkingLevel::High,
        ThinkingLevel::XHigh,
        ThinkingLevel::Max,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Self::Off => "关",
            Self::Minimal => "极低",
            Self::Low => "低",
            Self::Medium => "中",
            Self::High => "高",
            Self::XHigh => "极高",
            Self::Max => "最高",
            Self::Default => "兜底",
        }
    }

    /// 档位里有没有「关」这一档（没有 = 模型关不掉思考，见 [`min_level`]）。
    pub fn is_off(self) -> bool {
        self == Self::Off
    }
}

/// 各协议怎么把档位翻译成请求字段。
///
/// **不是枚举**：这几家「开关字段」和「档位字段」是两件事，可以同时出现
/// （DeepSeek 的 `thinking.type` + `reasoning_effort`、GLM-5.2/5.3 同、MiniMax-M3.1、Doubao），
/// 枚举表达不了「两个都发」和「只有其中一个」。拆成两个可空字段之后，
/// 「只有开关」「只有档位」「两个都发」都是同一个结构体的取值。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ThinkingFormat {
    /// 厂商私有的开 / 关字段（值为 `extra` 里那个键的内容：`true` / `{"type":"enabled"}`）。
    /// 「开」对应哪一档由能力表决定（都存成 `High`）
    pub toggle: Option<ToggleField>,
    /// effort 字段怎么写。`None` = 不发 effort
    pub effort: Option<EffortField>,
    /// `budget_tokens` 的老格式（Anthropic 4.5 及更早、按 Anthropic 格式兼容的第三方端点）
    pub budget: bool,
}

/// 一个开 / 关字段：`key` + 开和关时的取值。
///
/// 手写 `PartialEq` 而不是 derive：函数指针的地址不保证唯一，derive 出来的比较没有意义
#[derive(Debug, Clone, Copy)]
pub struct ToggleField {
    pub key: &'static str,
    pub on: fn() -> Value,
    pub off: fn() -> Value,
}

impl PartialEq for ToggleField {
    fn eq(&self, other: &Self) -> bool {
        // 字段一样就算一样：取值函数由 `key` 唯一决定
        self.key == other.key
    }
}

impl Eq for ToggleField {}

/// effort 字段放哪、叫什么
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EffortField {
    /// 顶层 `reasoning_effort`（OpenAI 兼容、DeepSeek、Kimi K3、MiniMax、Doubao、GLM）
    TopLevel,
    /// Anthropic 的 `output_config.effort`（新模型、DeepSeek / MiniMax 的 Anthropic 兼容端点）
    OutputConfig,
    /// Responses 的 `reasoning:{effort, summary:"auto"}`。
    /// 不带 `summary` 就收不到 `reasoning_summary_text.delta`，思考区是空的
    Responses,
}

impl EffortField {
    /// 兜底（[`ThinkingFormat::FALLBACK`]）按协议发哪个 effort 字段
    fn for_protocol(protocol: Protocol) -> Self {
        match protocol {
            Protocol::OpenAiCompat => Self::TopLevel,
            Protocol::Responses => Self::Responses,
            Protocol::Anthropic => Self::OutputConfig,
        }
    }
}

impl ThinkingFormat {
    /// 什么都不发（认不出的模型、`ThinkingSpec::NONE`）
    pub const NONE: ThinkingFormat = ThinkingFormat { toggle: None, effort: None, budget: false };

    /// 只有开 / 关，没有档位（Qwen3、GLM-4.5~5.1、DeepSeek V3 系、OpenRouter 上的旧模型）
    pub const fn toggle(key: &'static str, on: fn() -> Value, off: fn() -> Value) -> Self {
        ThinkingFormat { toggle: Some(ToggleField { key, on, off }), effort: None, budget: false }
    }

    /// 开关 + 档位并存（DeepSeek V4、GLM-5.2/5.3、MiniMax-M3.1、Doubao）
    pub const fn toggle_with_effort(
        key: &'static str,
        on: fn() -> Value,
        off: fn() -> Value,
        effort: EffortField,
    ) -> Self {
        ThinkingFormat {
            toggle: Some(ToggleField { key, on, off }),
            effort: Some(effort),
            budget: false,
        }
    }

    /// 只有 effort，没有开关（OpenAI o / gpt-5 系、Grok）
    pub const fn effort(field: EffortField) -> Self {
        ThinkingFormat { toggle: None, effort: Some(field), budget: false }
    }

    /// `budget_tokens` 老格式（Anthropic 4.5 及更早）
    pub const fn budget() -> Self {
        ThinkingFormat { toggle: None, effort: None, budget: true }
    }

    pub fn has_toggle(&self) -> bool {
        self.toggle.is_some()
    }
}

/// 一门协议 + 一个模型支持什么：能选哪些档位，字段怎么发。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ThinkingSpec {
    /// 支持的档位（升序）。空 = 认不出这个模型：请求里默认不带思考字段
    /// （用户选了「默认（兜底）」才会按 [`FALLBACK_LEVELS`] 发）
    pub levels: &'static [ThinkingLevel],
    pub format: ThinkingFormat,
    /// 认不认非默认的 `temperature`。Opus 4.7 以后的新模型一律不认（400），
    /// 和数据里有没有「开着思考」无关，所以单独一个标志
    pub accepts_temperature: bool,
}

/// 「默认（兜底）」：能力表里没有的模型也能选档位，按这套档位发 effort。
///
/// 不是凭空的猜测——这几档的取值（`low` / `medium` / `high` / `xhigh`）是 2026 年
/// 主流推理模型的共同子集，中转点上的新模型（`cn:xxx`）基本都收。**兜底只发 effort，
/// 不发厂商私有的开关字段**（`thinking` / `enable_thinking`）：那些字段写错了就是 400，
/// 而 `reasoning_effort` 在 OpenAI 兼容端点上早就是通用字段，不认的模型一般直接忽略。
/// `max` 故意不在里面：它比 `xhigh` 还高一档，且只有一半厂商认（见 [`ThinkingLevel::Max`]）
pub const FALLBACK_LEVELS: &[ThinkingLevel] =
    &[ThinkingLevel::Low, ThinkingLevel::Medium, ThinkingLevel::High, ThinkingLevel::XHigh];

impl ThinkingSpec {
    /// 认不出（界面除非用户选了「默认（兜底）」否则不显示档位、请求不带思考字段）
    pub const NONE: ThinkingSpec =
        ThinkingSpec { levels: &[], format: ThinkingFormat::NONE, accepts_temperature: true };

    /// 认不出 → 用「兜底」的写法（按协议编 effort，见 `apply_thinking`）。
    /// effort 字段到编码时才按协议定（`EffortField::for_protocol`），这里先占 `TopLevel`
    pub const FALLBACK: ThinkingSpec = ThinkingSpec {
        levels: FALLBACK_LEVELS,
        format: ThinkingFormat::effort(EffortField::TopLevel),
        accepts_temperature: true,
    };

    pub fn is_known(&self) -> bool {
        !self.levels.is_empty()
    }

    /// 能选的全部档位：认得就用它的，认不出就用 [`FALLBACK_LEVELS`]
    pub fn choices(&self) -> &'static [ThinkingLevel] {
        if self.is_known() { self.levels } else { FALLBACK_LEVELS }
    }

    /// 能不能明确关掉思考（没有「关」这一档的模型关不掉）
    pub fn can_disable(&self) -> bool {
        self.levels.contains(&ThinkingLevel::Off)
    }

    /// 档位就低的那一档（能关就是「关」，关不掉就是最低的开档）。
    /// 补全、分类器这类「只要最短判断」的旁路请求用它
    pub fn min_level(&self) -> Option<ThinkingLevel> {
        self.levels.first().copied()
    }

    /// 是不是只有开 / 关、没有中间档（Qwen3、GLM-4.5~5.1 这类）。
    /// 界面据此只给「关 / 开」，编码时中间档按「开」处理
    pub fn is_on_off(&self) -> bool {
        self.format.has_toggle() && self.format.effort.is_none()
    }

    /// 选中的档位落在这个模型上的实际档位（见 [`resolve_level`]）。
    ///
    /// 开 / 关型（只有 toggle、没有 effort）多一步：它只有「开 / 关」，用户选中间几档都按「开」处理，
    /// 不能因为「高」比「中」大就降成「关」——那等于把用户要的思考关掉了。
    /// 开关 + 档位并存的（DeepSeek V4、GLM-5.2/5.3）不走这条：它们真的能分档，照常降级
    pub fn resolve(&self, chosen: ThinkingLevel) -> Option<ThinkingLevel> {
        // 「兜底」排在 `Ord` 最后，直接按强度降级会落成最强那档（xhigh）；
        // 它的意思是「也发 effort」，落到能选的最低一个开档
        let chosen = if chosen == ThinkingLevel::Default {
            self.choices().iter().copied().find(|l| !l.is_off())?
        } else {
            chosen
        };
        if !self.is_known() {
            // 兜底档位：和认得的模型一样按 [`FALLBACK_LEVELS`] 降级
            return resolve_level(chosen, FALLBACK_LEVELS);
        }
        if self.is_on_off()
            && let Some(on) = self.levels.iter().rev().find(|l| !l.is_off())
        {
            return Some(if chosen.is_off() && self.can_disable() {
                ThinkingLevel::Off
            } else {
                *on
            });
        }
        resolve_level(chosen, self.levels)
    }
}

/// 去掉模型名的供应商前缀和一些装饰后缀，拿来做能力表匹配。
///
/// `anthropic/claude-opus-4-7`、`anthropic.claude-opus-4-7`、`us.anthropic.claude-opus-4-7-v1:0`、
/// `claude-opus-4-7-20260101` 都要落成 `claude-opus-4-7`。
/// 多模型中转点的模型名常带一层端点标签（`cn:glm-5.3`、`global:gpt-5.5`），也一起剥，
/// 否则 `glm-` / `qwen3` / `deepseek-` 这些按厂商名开头的规则一条也认不出。
fn model_key(model: &str) -> String {
    let mut name = model.trim().to_ascii_lowercase();
    // 供应商前缀（`vendor/`、`vendor.` 都见过；Bedrock 的 `us.anthropic.` 是两层）。
    // 只在「后面还有一截带字母的名字」时才剥，版本号里的点（`glm-4.5`）不动
    while !name.starts_with("claude") {
        let cut = ['/', ':']
            .iter()
            .filter_map(|sep| name.find(*sep))
            .chain(
                name.find('.')
                    .filter(|i| name[*i + 1..].starts_with(|c: char| c.is_ascii_alphabetic())),
            )
            .min();
        // 前缀那一截要像厂商名（纯字母或带连字符 / 下划线）：`qwen3-coder:free` 的 `free` 是后缀，
        // 剥了就认不出 qwen3；`cn:` / `global:` / `deepseek_ai/` 才是前缀
        let Some(cut) = cut else { break };
        let tag = &name[..cut];
        if !tag.chars().all(|c| c.is_ascii_alphabetic() || c == '-' || c == '_') {
            break;
        }
        let rest = &name[cut + 1..];
        if !rest.chars().any(|c| c.is_ascii_alphabetic()) {
            break;
        }
        name = rest.to_string();
    }
    name
}

/// 认得的、靠厂商字段「开 / 关」的模型：认得就返回它那份 spec（字段写法在 [`openai_spec`] 里
/// 同一份定义）。Anthropic 格式的第三方端点照搬这个开关字段——它和协议无关
/// （Qwen 的 `enable_thinking`、GLM / DeepSeek 的 `thinking.type` 两条线的端点都收）。
fn vendor_toggle(name: &str) -> Option<ThinkingSpec> {
    let spec = openai_spec(name);
    spec.format.has_toggle().then_some(spec)
}

/// OpenAI 兼容 / Responses 协议下的能力表（按 [`model_key`] 后的名字匹配）。
///
/// 各家厂商字段改得勤，**每一条都对着它当前的文档确认过**；确认不了的先不进表——
/// 不进表的后果只是「界面不显示」，进错了就是 400。
fn openai_spec(name: &str) -> ThinkingSpec {
    let owned = || ThinkingSpec {
        levels: &[
            ThinkingLevel::Minimal,
            ThinkingLevel::Low,
            ThinkingLevel::Medium,
            ThinkingLevel::High,
        ],
        format: ThinkingFormat::effort(EffortField::TopLevel),
        accepts_temperature: true,
    };
    // GPT-5 / o 系列：reasoning_effort 是官方参数
    let o_series =
        ["o1", "o3", "o4"].iter().any(|p| name == *p || name.starts_with(&format!("{p}-")));
    if let Some(rest) = name.strip_prefix("gpt-5") {
        // `gpt-5-chat-latest` 这类是不思考的对话模型，发 reasoning_effort 就是 400
        if rest.contains("chat") {
            return ThinkingSpec::NONE;
        }
        match gpt5_minor(rest) {
            // gpt-5 / gpt-5-mini / gpt-5-nano：minimal ~ high，关不掉
            Some(0) => return owned(),
            // gpt-5.1：none / low / medium / high（官方模型页，没有 minimal 和 xhigh）
            Some(1) => {
                return ThinkingSpec {
                    levels: &[
                        ThinkingLevel::Off,
                        ThinkingLevel::Low,
                        ThinkingLevel::Medium,
                        ThinkingLevel::High,
                    ],
                    ..owned()
                };
            }
            // gpt-5.2 起：再加 xhigh（gpt-5.1-codex-max 之后的模型都认）；minimal 不认
            Some(_) => {
                return ThinkingSpec {
                    levels: &[
                        ThinkingLevel::Off,
                        ThinkingLevel::Low,
                        ThinkingLevel::Medium,
                        ThinkingLevel::High,
                        ThinkingLevel::XHigh,
                    ],
                    ..owned()
                };
            }
            // `gpt-50` 之类：不是这一家
            None => {}
        }
    }
    if o_series {
        return owned();
    }
    // Qwen 3 系：`enable_thinking` 布尔（阿里云百炼「混合思考」，可开关）
    if name.starts_with("qwen3") || name == "qwq" || name.starts_with("qwq-") {
        return ThinkingSpec {
            levels: &[ThinkingLevel::Off, ThinkingLevel::High],
            format: ThinkingFormat::toggle(
                "enable_thinking",
                || Value::Bool(true),
                || Value::Bool(false),
            ),
            accepts_temperature: true,
        };
    }
    // GLM 4.5 ~ 5.2：`thinking.type` 开 / 关；5.2 起再叠 `reasoning_effort`；
    // 5.3 起关不掉（传 disabled 直接报错），所以不在表里给「关」
    if let Some(rest) = name.strip_prefix("glm-") {
        // 版本号取第一个 `-` 之前那段（`4.5-air` → 4.5）；GLM-4 这种老版本给不出小数就认不出
        let version: f32 = rest.split('-').next().unwrap_or_default().parse().unwrap_or(0.);
        if version < 4.5 {
            return ThinkingSpec::NONE;
        }
        let thinking_on = || serde_json::json!({ "type": "enabled" });
        let thinking_off = || serde_json::json!({ "type": "disabled" });
        // 5.2 起多一个 `reasoning_effort`（官方：5.2 收 none/minimal/low/medium/high/xhigh/max，
        // low/medium→high、xhigh→max；5.3 只认 low/high/max，传别的报错）。
        // 两边都认的那几档取交集，免得给 5.3 发 medium 吃 400
        if version >= 5.2 {
            return ThinkingSpec {
                levels: if version >= 5.3 {
                    // 不能关：`disabled` 报错
                    &[ThinkingLevel::Low, ThinkingLevel::High, ThinkingLevel::Max]
                } else {
                    &[
                        ThinkingLevel::Off,
                        ThinkingLevel::Low,
                        ThinkingLevel::High,
                        ThinkingLevel::Max,
                    ]
                },
                format: ThinkingFormat::toggle_with_effort(
                    "thinking",
                    thinking_on,
                    thinking_off,
                    EffortField::TopLevel,
                ),
                accepts_temperature: true,
            };
        }
        return ThinkingSpec {
            levels: &[ThinkingLevel::Off, ThinkingLevel::High],
            format: ThinkingFormat::toggle("thinking", thinking_on, thinking_off),
            accepts_temperature: true,
        };
    }
    // DeepSeek V4（`deepseek-flash` / `deepseek-v4-pro`）：`thinking.type` + 顶层 `reasoning_effort`。
    // 官方取值 `none` / `low` / `high` / `max`，`none` 关；`minimal`→low、`medium`/`xhigh`→high、
    // `ultra`→max，所以表里只列它真认的四档（多了会被映射，看起来像生效其实不是）
    if is_deepseek_v4(name) {
        return ThinkingSpec {
            levels: &[
                ThinkingLevel::Off,
                ThinkingLevel::Low,
                ThinkingLevel::High,
                ThinkingLevel::Max,
            ],
            format: ThinkingFormat::toggle_with_effort(
                "thinking",
                || serde_json::json!({ "type": "enabled" }),
                || serde_json::json!({ "type": "disabled" }),
                EffortField::TopLevel,
            ),
            accepts_temperature: true,
        };
    }
    // DeepSeek V3.1+（中转站上的 `deepseek-v3.2` 这类）：只有 `thinking.type` 开 / 关，
    // 不敢发 `reasoning_effort`（V4 之前的部署不一定认）
    if name.starts_with("deepseek-") && !name.contains("reasoner") && !name.contains("-r1") {
        return ThinkingSpec {
            levels: &[ThinkingLevel::Off, ThinkingLevel::High],
            format: ThinkingFormat::toggle(
                "thinking",
                || serde_json::json!({ "type": "enabled" }),
                || serde_json::json!({ "type": "disabled" }),
            ),
            accepts_temperature: true,
        };
    }
    ThinkingSpec::NONE
}

/// DeepSeek V4 一代（`deepseek-v4-pro`、`deepseek-v4.1-flash`，官方 API 上的 `deepseek-flash` / `deepseek-pro`）。
fn is_deepseek_v4(name: &str) -> bool {
    ["deepseek-v4", "deepseek-flash", "deepseek-pro"].iter().any(|p| name.starts_with(p))
}

/// `gpt-5` 后面那截 → 小版本号：`""` / `-mini` / `-2025-08-07` 是 0，`.1` / `-1-codex` 是 1。
/// 不是 `gpt-5` 系列（`gpt-50`）返回 None。
fn gpt5_minor(rest: &str) -> Option<u32> {
    let Some(tail) = rest.strip_prefix(['.', '-']) else {
        return rest.is_empty().then_some(0);
    };
    let digits: String = tail.chars().take_while(char::is_ascii_digit).collect();
    // 一两位才是小版本号；四位的是日期快照（`gpt-5-2025-08-07`）
    match digits.len() {
        1 | 2 => digits.parse().ok(),
        _ => Some(0),
    }
}

/// Anthropic 原生协议下的能力表（按 [`model_key`] 后的名字匹配，走 claude 自己的命名）。
fn anthropic_spec(name: &str) -> ThinkingSpec {
    let adaptive = |levels: &'static [ThinkingLevel], accepts_temperature: bool| ThinkingSpec {
        levels,
        format: ThinkingFormat::effort(EffortField::OutputConfig),
        accepts_temperature,
    };
    // 名字里不带 claude 的是 Anthropic 格式的第三方端点。认两种：
    // - OpenAI 兼容表里认得的那几家（Qwen / GLM / DeepSeek）：照搬同一份 format；
    // - 确认过收旧格式预算的 GLM。
    // 别的认不出，什么都不发
    if !name.contains("claude") {
        // 预算格式只留给 GLM：DeepSeek 的 Anthropic 兼容页明确写 `budget_tokens` is ignored、
        // 只认 `output_config.effort`，所以它走上面的 `vendor_toggle`（带 effort），不能走预算
        let budget_ok = name.starts_with("glm-");
        // 总要思考的模型（`deepseek-reasoner` / `*-r1`）：没有「关」这一档，
        // 连这一行都不给（OpenAI 兼容表里也是认不出的）
        let always_thinks = name.contains("reasoner") || name.contains("-r1");
        let spec = if let Some(spec) = vendor_toggle(name)
            && !budget_ok
        {
            // 端点换成 Anthropic 格式照样发同一套字段，不能因为换了协议就没这一项。
            // 但 effort 的落点要按协议改：OpenAI 兼容是顶层 `reasoning_effort`，
            // Anthropic 兼容是 `output_config.effort`（DeepSeek / MiniMax 的 Anthropic 端点都只认这个）
            let mut spec = spec;
            if spec.format.effort.is_some() {
                spec.format.effort = Some(EffortField::OutputConfig);
            }
            spec
        } else if budget_ok {
            ThinkingSpec {
                levels: &[
                    ThinkingLevel::Off,
                    ThinkingLevel::Low,
                    ThinkingLevel::Medium,
                    ThinkingLevel::High,
                ],
                format: ThinkingFormat::budget(),
                accepts_temperature: true,
            }
        } else {
            return ThinkingSpec::NONE;
        };
        return if always_thinks { ThinkingSpec::NONE } else { spec };
    }
    // 关不掉的那一代（`disabled` 一律 400）：低 ~ 极高
    let no_off = [
        "claude-opus-5-5",
        "claude-opus-5.5",
        "claude-sonnet-5-5",
        "claude-fable-5",
        "claude-fable-5-1",
        "claude-mythos",
    ];
    if no_off.iter().any(|p| name.starts_with(p)) {
        return adaptive(
            &[ThinkingLevel::Low, ThinkingLevel::Medium, ThinkingLevel::High, ThinkingLevel::XHigh],
            false,
        );
    }
    // 4.6：能选到「高」，4.7 才加 `xhigh`
    let four_six = ["claude-opus-4-6", "claude-sonnet-4-6"];
    if four_six.iter().any(|p| name.starts_with(p)) {
        return adaptive(
            &[ThinkingLevel::Off, ThinkingLevel::Low, ThinkingLevel::Medium, ThinkingLevel::High],
            true,
        );
    }
    // 4.7 / 4.8 / 5 / 5.5 及更新的 claude：全档；新模型不吃非默认 temperature
    let modern = [
        "claude-opus-4-7",
        "claude-opus-4-8",
        "claude-opus-5",
        "claude-sonnet-5",
        "claude-haiku-5",
    ];
    if modern.iter().any(|p| name.starts_with(p)) {
        return adaptive(
            &[
                ThinkingLevel::Off,
                ThinkingLevel::Low,
                ThinkingLevel::Medium,
                ThinkingLevel::High,
                ThinkingLevel::XHigh,
            ],
            false,
        );
    }
    // 4.5 及更早：`budget_tokens`
    if name.starts_with("claude-") {
        let old = ["claude-opus-4-5", "claude-sonnet-4-5", "claude-haiku-4-5", "claude-opus-4-1"];
        if old.iter().any(|p| name.starts_with(p)) || name.contains("3-5") || name.contains("3-7") {
            return ThinkingSpec {
                levels: &[
                    ThinkingLevel::Off,
                    ThinkingLevel::Low,
                    ThinkingLevel::Medium,
                    ThinkingLevel::High,
                ],
                format: ThinkingFormat::budget(),
                accepts_temperature: true,
            };
        }
        // 认得出是 claude 但不在表里（比本表更新的型号）：最新一代的共同子集——
        // adaptive、低 ~ 极高、关不掉、不吃 temperature
        return adaptive(
            &[ThinkingLevel::Low, ThinkingLevel::Medium, ThinkingLevel::High, ThinkingLevel::XHigh],
            false,
        );
    }
    ThinkingSpec::NONE
}

/// 按「协议 + 模型名」查能力表（纯函数，界面和请求体组装共用这一份）。
pub fn thinking_spec(protocol: Protocol, model: &str) -> ThinkingSpec {
    let key = model_key(model);
    if key.is_empty() {
        return ThinkingSpec::NONE;
    }
    match protocol {
        Protocol::Anthropic => anthropic_spec(&key),
        // Responses 只是字段名不同（`reasoning.effort`），认得的模型是同一批
        Protocol::OpenAiCompat | Protocol::Responses => {
            let mut spec = openai_spec(&key);
            if protocol == Protocol::Responses
                && let Some(EffortField::TopLevel) = spec.format.effort
            {
                // 顶层 `reasoning_effort` → `reasoning:{effort, summary}`
                spec.format.effort = Some(EffortField::Responses);
            }
            spec
        }
    }
}

/// 选的档位当前模型不支持时降级：取不高于它的最近一档（Claude Code 的规则）；
/// 一档都比它高就取最低一档。
pub fn resolve_level(chosen: ThinkingLevel, levels: &[ThinkingLevel]) -> Option<ThinkingLevel> {
    if levels.is_empty() {
        return None;
    }
    levels.iter().copied().filter(|l| *l <= chosen).max().or_else(|| levels.first().copied())
}

/// 老格式（`budget_tokens`）每一档换算出来的 `budget_tokens`。
/// 界面上在档位旁边标一句「约 N token」，请求体组装也按它算。
pub fn thinking_budget_for(level: ThinkingLevel) -> u64 {
    match level {
        ThinkingLevel::Off | ThinkingLevel::Minimal | ThinkingLevel::Low => 2_048,
        ThinkingLevel::Medium => 8_192,
        ThinkingLevel::High | ThinkingLevel::XHigh | ThinkingLevel::Max => 16_000,
        // 「兜底」不是一档强度，进到这里前已经由 `resolve` 换成具体档位；真到了按最低算
        ThinkingLevel::Default => 2_048,
    }
}

/// 把界面 / 内核之间传的档位字符串解析成档位（`set_thinking` 用）。
///
/// 认的是 [`ThinkingLevel`] 的 serde 名字（`off` / `low` / `high` / `x_high` / `max` …），
/// 也就是设置文件里存的那一套；大小写和首尾空白不敏感。
/// `x_high` 另收 `xhigh`：`snake_case` 会把它拆成 `x_high`，手写设置文件的人几乎都写成 `xhigh`
pub fn parse_thinking_level(text: &str) -> anyhow::Result<ThinkingLevel> {
    let name = text.trim().to_ascii_lowercase();
    let name = if name == "xhigh" { "x_high".to_string() } else { name };
    serde_json::from_value::<ThinkingLevel>(serde_json::json!(name))
        .map_err(|_| anyhow::anyhow!("未知思考档位 {text}"))
}

/// effort 档位的请求体取值（Anthropic 的 `output_config.effort` / OpenAI 的 `reasoning_effort`）
fn effort_for(level: ThinkingLevel) -> &'static str {
    match level {
        ThinkingLevel::Off => "none",
        ThinkingLevel::Minimal => "minimal",
        ThinkingLevel::Low => "low",
        ThinkingLevel::Medium => "medium",
        ThinkingLevel::High => "high",
        ThinkingLevel::XHigh => "xhigh",
        ThinkingLevel::Max => "max",
        // 「兜底」只管「发不发」，具体档位由 `resolve` 现挑；漏到这里按最低的发
        ThinkingLevel::Default => "low",
    }
}

/// 把档位写进请求体（三个 provider 组完请求体、合 `extra` **之前**各调一次）。
///
/// - `params.thinking == None`：什么都不做（用模型自己的默认）
/// - 认不出的模型：只有选「默认（兜底）」（[`ThinkingLevel::Default`]）时才发，
///   按 [`FALLBACK_LEVELS`] 换成 effort 字段，其余档位一律不动
/// - 模型不支持选的这一档：按 [`resolve_level`] 降级后再编
/// - 开着思考时不发 `temperature`；`accepts_temperature == false` 的模型（Opus 4.7 以后）
///   不管开没开都不发——这是以前「填了预算才不发」之外的另一半
pub fn apply_thinking(body: &mut Value, protocol: Protocol, model: &str, params: &RequestParams) {
    let spec = thinking_spec(protocol, model);
    if !spec.accepts_temperature {
        body.as_object_mut().map(|o| o.remove("temperature"));
    }
    let known = spec.is_known();
    let Some(chosen) = params.thinking else { return };
    // 认不出的模型：只有选「默认（兜底）」才走兜底那份 spec。
    // `"默认"`（None）的意思一直是「什么都不发」，别的具体档位也不敢猜，一律不动
    if !known && chosen != ThinkingLevel::Default {
        return;
    }
    let spec = if known { spec } else { ThinkingSpec::FALLBACK };
    // 「兜底」不是一档强度：落到它支持的最低一档（Low）。
    // 不这么办的话 `resolve` 会按 `Ord` 把 `Default`（排在最后）当成「最强」，
    // 一路降级到 `xhigh`——那是「兜底」最贵的跑法，正好反了
    let chosen = if chosen == ThinkingLevel::Default { FALLBACK_LEVELS[0] } else { chosen };
    let Some(level) = spec.resolve(chosen) else { return };
    let thinking_on = !level.is_off();
    if thinking_on {
        // Anthropic 要求开 thinking 时 temperature 只能是默认值
        body.as_object_mut().map(|o| o.remove("temperature"));
    }
    // 1) 厂商私有的开 / 关字段（`thinking.type` / `enable_thinking`）。
    //    关的时候**必须**发 disabled：DeepSeek / GLM / Kimi 默认开着思考，不发等于没关
    if let Some(toggle) = spec.format.toggle {
        body[toggle.key] = if thinking_on { (toggle.on)() } else { (toggle.off)() };
    }
    // 2) `budget_tokens` 老格式（Anthropic 4.5 及更早）
    if spec.format.budget {
        if level.is_off() {
            // 老格式：不带 thinking 就是不思考
            if spec.format.toggle.is_none() {
                return;
            }
        } else {
            let max = body["max_tokens"].as_u64().unwrap_or(0);
            let mut budget = thinking_budget_for(level).max(RequestParams::MIN_THINKING_BUDGET);
            // `budget_tokens` 必须小于 `max_tokens`：用户没设上限时抬上去（按 `params` 判断）；
            // 自己设了且不够就把预算压下来，低于下限就不开（发出去只会 400）
            if let Some(limit) = params.max_tokens {
                if budget >= limit {
                    let room = limit.saturating_sub(1_024);
                    if room < RequestParams::MIN_THINKING_BUDGET {
                        return;
                    }
                    budget = room;
                }
            } else if max <= budget {
                body["max_tokens"] = serde_json::json!(budget + 4_096);
            }
            body["thinking"] = serde_json::json!({ "type": "enabled", "budget_tokens": budget });
        }
    }
    // 3) effort 字段
    if let Some(field) = spec.format.effort {
        // 兜底（认不出的模型选了「Default」）拿到的 `field` 是占位的 `TopLevel`，按协议重新定。
        // 判断用「原来那张 spec 认不认得」——换成 `FALLBACK` 之后 `is_known()` 会变成真
        let field = if known { field } else { EffortField::for_protocol(protocol) };
        // 关：开关型模型已经发了 `disabled`（DeepSeek / GLM 的 `none` 和它是两种写法，
        // 同时发容易被解读成「关掉档位但打开思考」），这时不再塞 effort；
        // 纯 effort 的模型（gpt-5.1）才用 `none` 表达关
        if !thinking_on && spec.format.toggle.is_some() {
            return;
        }
        let effort = effort_for(level);
        match field {
            EffortField::TopLevel => body["reasoning_effort"] = serde_json::json!(effort),
            EffortField::OutputConfig => {
                // Anthropic 自己的新模型（没有 toggle、不是预算格式）：
                // 开要显式声明 adaptive + display，关要显式 disabled 且不发 effort
                // （`display:"summarized"` 必须带，4.7 以后默认 omitted，思考块正文是空串）
                let adaptive_style = known && spec.format.toggle.is_none() && !spec.format.budget;
                if adaptive_style {
                    if level.is_off() {
                        body["thinking"] = serde_json::json!({ "type": "disabled" });
                    } else {
                        body["thinking"] =
                            serde_json::json!({ "type": "adaptive", "display": "summarized" });
                        body["output_config"] = serde_json::json!({ "effort": effort });
                    }
                } else {
                    // 第三方 Anthropic 兼容端点（DeepSeek / MiniMax）：只认 `output_config.effort`，
                    // 开关另外由上面的 toggle 写
                    body["output_config"] = serde_json::json!({ "effort": effort });
                }
            }
            // 不带 `summary:"auto"` 就收不到 `reasoning_summary_text.delta`，思考区是空的
            EffortField::Responses => {
                body["reasoning"] = serde_json::json!({ "effort": effort, "summary": "auto" });
            }
        }
    }
}

/// 连接某个端点的配置。
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ProviderConfig {
    pub protocol: Protocol,
    /// 如 `http://127.0.0.1:8045/v1`（不含 `/chat/completions`、`/messages`）
    pub base_url: String,
    pub api_key: Option<String>,
    pub model: String,
    pub params: RequestParams,
    /// 用量网关：有就把请求经它转发、每个请求记一行日志（见 `gateway` 模块）；None = 直连
    pub gateway: Option<crate::gateway::GatewayHandle>,
    /// `web_search` 的 DeepSeek 后端复用哪把 key（None = 这个档案不是 DeepSeek 端点）。
    /// 界面按档案的 base_url / 名字挑一把填进来，内核起 `set_key_hint` 交给 `websearch`；
    /// 状态文件（搜索后端的选择）里存不了 key，所以 key 跟着 provider 配置走。
    pub deepseek_key: Option<String>,
}

impl ProviderConfig {
    /// 写进会话元信息的内核标识：同一端点 + 模型才算同一个「agent」。
    /// Anthropic 原生加协议前缀，与 OpenAI 兼容端点区分。
    pub fn identity(&self) -> String {
        let proto = match self.protocol {
            Protocol::OpenAiCompat => "",
            Protocol::Anthropic => "anthropic:",
            Protocol::Responses => "responses:",
        };
        format!("builtin:{proto}{}#{}", self.base_url, self.model)
    }
}

/// 消息内容的一段：文本或图片（base64）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ContentPart {
    Text(String),
    Image {
        /// 如 `image/png`
        media_type: String,
        /// 图片字节（base64 编码）
        data: String,
    },
}

/// 发给 provider 的一条消息。内部表示协议无关，序列化成 OpenAI 形状
/// （单段文本 → `content` 为字符串，兼容所有网关；带图片 → 分段数组）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChatMessage {
    pub role: String,
    pub parts: Vec<ContentPart>,
    /// 只有工具调用的 assistant 消息为空
    pub tool_calls: Vec<ToolCall>,
    /// role = tool 时对应的调用 ID
    pub tool_call_id: Option<String>,
    /// 模型这一步的思考块原样（Anthropic 的 `thinking` / `redacted_thinking`，带 `signature`）：
    /// 开了思考预算时，工具循环里回传的 assistant 消息必须以它们开头。只有原生 Anthropic
    /// 协议发回去，别的协议序列化时忽略；不进日志（续接后没有，也不需要）。
    pub thinking: Vec<Value>,
}

impl ChatMessage {
    pub fn new(role: &str, text: impl Into<String>) -> Self {
        Self::with_parts(role, vec![ContentPart::Text(text.into())])
    }

    pub fn with_parts(role: &str, parts: Vec<ContentPart>) -> Self {
        Self {
            role: role.to_string(),
            parts,
            tool_calls: Vec::new(),
            tool_call_id: None,
            thinking: Vec::new(),
        }
    }

    /// 带工具调用的 assistant 消息（`text` 为同一步里模型说的话，可为空）
    pub fn assistant_tool_calls(text: String, tool_calls: Vec<ToolCall>) -> Self {
        let mut m = Self::new("assistant", text);
        m.tool_calls = tool_calls;
        m
    }

    pub fn tool_result(call_id: String, content: String) -> Self {
        let mut m = Self::new("tool", content);
        m.tool_call_id = Some(call_id);
        m
    }

    /// 带图片的工具结果：图片段跟在文本段后面（`run_tools` 里 MCP 的多模态结果用）。
    pub fn tool_result_with_images(
        call_id: String,
        content: String,
        images: Vec<crate::tools::OutputImage>,
    ) -> Self {
        let mut m = Self::tool_result(call_id, content);
        if !images.is_empty() {
            m.parts.extend(
                images
                    .into_iter()
                    .map(|img| ContentPart::Image { media_type: img.media_type, data: img.data }),
            );
        }
        m
    }

    /// 文本内容（各文本段拼接）；没有文本返回 None
    /// 在文字末尾补一段（没有文字段就加一段；图片段留在原位）。请求前临时挂提醒用。
    pub fn append_text(&mut self, extra: &str) {
        match self.parts.iter_mut().rev().find_map(|p| match p {
            ContentPart::Text(t) => Some(t),
            _ => None,
        }) {
            Some(t) => t.push_str(extra),
            None => self.parts.insert(0, ContentPart::Text(extra.trim_start().to_string())),
        }
    }

    pub fn text(&self) -> Option<String> {
        let mut out = String::new();
        let mut any = false;
        for part in &self.parts {
            if let ContentPart::Text(t) = part {
                out.push_str(t);
                any = true;
            }
        }
        any.then_some(out)
    }

    fn has_image(&self) -> bool {
        self.parts.iter().any(|p| matches!(p, ContentPart::Image { .. }))
    }

    /// 同样的判断，给 `lib.rs` 用（`has_image` 私有，不想为它开一个 pub）
    pub fn has_image_any(&self) -> bool {
        self.has_image()
    }

    /// 取走全部图片段，留下文字（图片改装进一条 user 消息时用，见 `images_as_user_message`）。
    pub fn take_images(&mut self) -> Vec<ContentPart> {
        let mut taken = Vec::new();
        self.parts.retain(|part| {
            if matches!(part, ContentPart::Image { .. }) {
                taken.push(part.clone());
                false
            } else {
                true
            }
        });
        taken
    }

    /// 把图片段换成文字占位（当前模型看不了图时，见设计说明 7.3）。
    /// 只影响这一份请求里的副本，历史不动。
    pub fn replace_images_with_placeholder(&mut self) {
        if !self.has_image() {
            return;
        }
        self.parts = std::mem::take(&mut self.parts)
            .into_iter()
            .map(|part| match part {
                ContentPart::Image { .. } => ContentPart::Text(
                    "[image omitted: the current model cannot view images]".into(),
                ),
                other => other,
            })
            .collect();
    }
}

/// 序列化为 OpenAI Chat Completions 的消息形状。
impl Serialize for ChatMessage {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        use serde::ser::Error as _;
        let mut message = serde_json::Map::new();
        message.insert("role".into(), Value::String(self.role.clone()));
        // 纯文本的工具结果和单段文本的普通消息保持字符串形状（网关兼容性最好）；
        // 带图片的工具结果（MCP 多模态）用分段数组，文本段在前图片段在后。
        // 空文本段一律跳过：纯工具调用的 assistant 消息 content 为 null。
        let multipart = |msg: &ChatMessage| -> Option<Value> {
            if !msg.has_image() {
                return None;
            }
            let parts: Vec<Value> = msg
                .parts
                .iter()
                .filter(|part| match part {
                    ContentPart::Text(t) => !t.is_empty(),
                    _ => true,
                })
                .map(|part| match part {
                    ContentPart::Text(text) => serde_json::json!({
                        "type": "text",
                        "text": text,
                    }),
                    ContentPart::Image { media_type, data } => serde_json::json!({
                        "type": "image_url",
                        "image_url": { "url": format!("data:{media_type};base64,{data}") },
                    }),
                })
                .collect();
            (!parts.is_empty()).then(|| Value::Array(parts))
        };
        let content =
            multipart(self).unwrap_or_else(|| match self.text().filter(|t| !t.is_empty()) {
                Some(text) => Value::String(text),
                None => Value::Null,
            });
        message.insert("content".into(), content);
        if !self.tool_calls.is_empty() {
            message.insert(
                "tool_calls".into(),
                serde_json::to_value(&self.tool_calls).map_err(S::Error::custom)?,
            );
        }
        if let Some(id) = &self.tool_call_id {
            message.insert("tool_call_id".into(), Value::String(id.clone()));
        }
        Value::Object(message).serialize(serializer)
    }
}

/// 模型发起的一次函数调用。
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ToolCall {
    pub id: String,
    #[serde(rename = "type")]
    pub kind: String,
    pub function: FunctionCall,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct FunctionCall {
    pub name: String,
    /// JSON 文本（流式拼接而来）
    pub arguments: String,
}

/// 流式过程中交给调用方的事件。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StreamEvent {
    Text(String),
    Reasoning(String),
    /// 即将重试（第 `attempt` 次，等待 `delay_ms`），`status` / `reason` 为上一次的错误
    Retrying {
        status: u16,
        attempt: usize,
        delay_ms: u64,
        reason: String,
    },
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Usage {
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub reasoning_tokens: u64,
    /// 输入里命中提示缓存的部分（包含在 `input_tokens` 里）
    pub cache_read_tokens: u64,
}

impl Usage {
    pub fn add(&mut self, other: Usage) {
        self.input_tokens += other.input_tokens;
        self.output_tokens += other.output_tokens;
        self.reasoning_tokens += other.reasoning_tokens;
        self.cache_read_tokens += other.cache_read_tokens;
    }
}

/// 端点模型列表里的一项。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelInfo {
    pub id: String,
    /// 上下文窗口（token）；端点没报告时为 None
    pub context_window: Option<u64>,
}

/// 模型列表响应（`{"data":[{"id":…}, …]}`，OpenAI / Anthropic 同形）→ 模型条目；没有 id 的跳过。
/// 窗口大小各家字段名不一：OpenRouter 和多数网关 `context_length`，vLLM `max_model_len`，
/// Anthropic `max_input_tokens`，其余几种也认。
pub(crate) fn parse_model_list(data: &[Value]) -> Vec<ModelInfo> {
    const WINDOW_KEYS: [&str; 6] = [
        "context_length",
        "context_window",
        "max_context_length",
        "max_model_len",
        "max_input_tokens",
        "input_token_limit",
    ];
    data.iter()
        .filter_map(|m| {
            let id = m.get("id")?.as_str()?.to_owned();
            let context_window = WINDOW_KEYS
                .iter()
                .find_map(|k| m.get(*k).and_then(Value::as_u64))
                .filter(|n| *n > 0);
            Some(ModelInfo { id, context_window })
        })
        .collect()
}

/// 一次流式补全的结果（协议无关；`finish_reason` 用 OpenAI 词汇）。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct StreamOutcome {
    /// 结束原因（stop / length / tool_calls …）
    pub finish_reason: Option<String>,
    pub usage: Option<Usage>,
    /// false = 被 `should_stop` 叫停
    pub completed: bool,
    /// 本步模型请求的工具调用（按出现顺序）
    pub tool_calls: Vec<ToolCall>,
    /// 本步的思考块原样（见 [`ChatMessage::thinking`]；只有 Anthropic 原生协议给）
    pub thinking: Vec<Value>,
}

/// 协议无关的对话 provider。实现方负责把统一消息形状翻译成自己的请求体。
pub trait ChatProvider: Send + Sync {
    /// 发起一轮流式补全。`tools` 为 OpenAI 形状的工具定义（空 = 不带）；
    /// `should_stop` 每读一行检查一次；事件经 `on_event` 同步交付
    /// （调用线程即读流线程）。
    fn stream_chat(
        &self,
        model: &str,
        messages: &[ChatMessage],
        tools: &[Value],
        should_stop: &dyn Fn() -> bool,
        on_event: &mut dyn FnMut(StreamEvent),
    ) -> anyhow::Result<StreamOutcome>;

    /// 端点提供的模型（含上下文窗口，端点报告的话）。
    fn list_models(&self) -> anyhow::Result<Vec<ModelInfo>>;
}

/// 带退避重试的 SSE POST（两个 provider 共用）：非 2xx 读出错误体，
/// 可重试状态码在出字前按 [`BACKOFF_MS`] 退避重试；成功返回响应体的行读取器。
/// 经用量网关时每次尝试带上请求号和第几次（网关日志里重试归到同一个请求）。
pub(crate) fn post_sse_with_retry(
    agent: &ureq::Agent,
    route: &Route,
    headers: &[(&str, String)],
    body: &str,
    should_stop: &dyn Fn() -> bool,
    on_event: &mut dyn FnMut(StreamEvent),
) -> anyhow::Result<impl BufRead + use<>> {
    static NEXT_REQUEST: AtomicU64 = AtomicU64::new(1);
    let url = &route.real;
    debug_http(|| format!("→ POST {url}\n{body}"));
    let request_id = NEXT_REQUEST.fetch_add(1, Ordering::Relaxed).to_string();
    let mut attempt = 0;
    loop {
        let mut request = agent
            .post(&route.url)
            .header("Content-Type", "application/json")
            .header("Accept", "text/event-stream");
        for (name, value) in headers.iter().chain(&route.headers) {
            request = request.header(*name, value.as_str());
        }
        if route.via_gateway() {
            request = request
                .header(crate::gateway::REQUEST, request_id.as_str())
                .header(crate::gateway::ATTEMPT, (attempt + 1).to_string());
        }
        let response = request.send(body).map_err(|e| anyhow::anyhow!("请求 {url} 失败: {e}"))?;
        let status = response.status().as_u16();
        if (200..300).contains(&status) {
            return Ok(BufReader::new(response.into_body().into_reader()));
        }
        let gateway_error = response.headers().contains_key(crate::gateway::GATEWAY_ERROR);
        let text = response.into_body().read_to_string().unwrap_or_default();
        // 网关没连上上游：和直连时连不上一样报（界面按「网络问题」处理）
        if gateway_error {
            anyhow::bail!("请求 {url} 失败: {}", error_message(&text));
        }
        let reason = format!("HTTP {status}: {}", error_message(&text));
        if !RETRYABLE.contains(&status) || attempt >= BACKOFF_MS.len() || should_stop() {
            anyhow::bail!(reason);
        }
        let delay_ms = BACKOFF_MS[attempt];
        attempt += 1;
        on_event(StreamEvent::Retrying { status, attempt, delay_ms, reason });
        std::thread::sleep(Duration::from_millis(delay_ms));
    }
}

/// GET 一个 JSON 接口（列模型）：非 2xx 报 `HTTP 状态: 信息`，连不上报「请求 … 失败」（经网关时同样）。
pub(crate) fn get_json(
    agent: &ureq::Agent,
    route: &Route,
    headers: &[(&str, String)],
) -> anyhow::Result<String> {
    let url = &route.real;
    let mut request = agent.get(&route.url).header("Accept", "application/json");
    for (name, value) in headers.iter().chain(&route.headers) {
        request = request.header(*name, value.as_str());
    }
    let response = request.call().map_err(|e| anyhow::anyhow!("请求 {url} 失败: {e}"))?;
    let status = response.status().as_u16();
    let gateway_error = response.headers().contains_key(crate::gateway::GATEWAY_ERROR);
    let text = response.into_body().read_to_string().unwrap_or_default();
    if gateway_error {
        anyhow::bail!("请求 {url} 失败: {}", error_message(&text));
    }
    if !(200..300).contains(&status) {
        anyhow::bail!("HTTP {status}: {}", error_message(&text));
    }
    Ok(text)
}

/// 从错误响应体里取人类可读的信息（`error.message` 或 `message`），否则原文截断。
pub(crate) fn error_message(body: &str) -> String {
    let parsed: Option<Value> = serde_json::from_str(body).ok();
    let message = parsed.as_ref().and_then(|v| {
        v.pointer("/error/message")
            .or_else(|| v.get("message"))
            .and_then(Value::as_str)
            .map(str::to_owned)
    });
    let text = message.unwrap_or_else(|| body.trim().to_string());
    if text.chars().count() > 300 {
        format!("{}…", text.chars().take(300).collect::<String>())
    } else {
        text
    }
}

/// `CLAUDEDESKTOP_DEBUG_HTTP=1` 时把请求 / 错误细节打到 stderr（排障用）。
pub(crate) fn debug_http(make: impl FnOnce() -> String) {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    if *ON.get_or_init(|| std::env::var("CLAUDEDESKTOP_DEBUG_HTTP").as_deref() == Ok("1")) {
        eprintln!("[http] {}", make());
    }
}

/// 一条流是怎么结束的。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SseEnd {
    /// 被 `should_stop` 叫停
    Stopped,
    /// 收到 `[DONE]`
    Done,
    /// 连接关了，没有 `[DONE]`
    Eof,
}

/// 逐条读取 SSE 事件直到流结束（共享 [`crate::sse::read_events`] 的取消语义）。
pub(crate) fn read_sse(
    reader: impl BufRead,
    should_stop: &dyn Fn() -> bool,
    on_data: &mut dyn FnMut(&str) -> Result<(), String>,
) -> anyhow::Result<SseEnd> {
    let mut done = false;
    let finished = crate::sse::read_events(reader, should_stop, |event| match event {
        crate::sse::SseEvent::Data(payload) => on_data(&payload).map_err(std::io::Error::other),
        crate::sse::SseEvent::Done => {
            done = true;
            Ok(())
        }
    })
    .map_err(|e| anyhow::anyhow!("读取流失败: {e}"))?;
    Ok(match (finished, done) {
        (false, _) => SseEnd::Stopped,
        (true, true) => SseEnd::Done,
        (true, false) => SseEnd::Eof,
    })
}

/// 读完流的收尾：返回 `StreamOutcome::completed`（叫停为 false）。连接在没有任何结束标记
/// （`[DONE]`、finish_reason / stop_reason）时就关了，算中途断开报错——
/// 否则截断的回复会被当成说完了，界面上看不出来。
pub(crate) fn finish_stream(end: SseEnd, finish_reason: bool) -> anyhow::Result<bool> {
    match end {
        SseEnd::Stopped => Ok(false),
        SseEnd::Eof if !finish_reason => anyhow::bail!("读取流失败: 连接在回复结束前断开了"),
        _ => Ok(true),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn serializes_plain_text_as_string() {
        let m = ChatMessage::new("user", "你好");
        let v = serde_json::to_value(&m).unwrap();
        assert_eq!(v["role"], "user");
        assert_eq!(v["content"], "你好");
        assert!(v.get("tool_calls").is_none() && v.get("tool_call_id").is_none());
    }

    #[test]
    fn serializes_image_as_segmented_content() {
        let m = ChatMessage::with_parts(
            "user",
            vec![
                ContentPart::Text("看图".into()),
                ContentPart::Image { media_type: "image/png".into(), data: "QUJD".into() },
            ],
        );
        let v = serde_json::to_value(&m).unwrap();
        let content = v["content"].as_array().unwrap();
        assert_eq!(content[0]["type"], "text");
        assert_eq!(content[0]["text"], "看图");
        assert_eq!(content[1]["type"], "image_url");
        assert_eq!(content[1]["image_url"]["url"], "data:image/png;base64,QUJD");
        // 文本段拼接
        assert_eq!(m.text().as_deref(), Some("看图"));
    }

    #[test]
    fn images_become_placeholder_text_for_text_only_models() {
        // 当前模型看不了图：图片段换成占位文字，原来的文字段还在，序列化回字符串
        let mut m = ChatMessage::with_parts(
            "user",
            vec![
                ContentPart::Text("看图".into()),
                ContentPart::Image { media_type: "image/png".into(), data: "QUJD".into() },
            ],
        );
        m.replace_images_with_placeholder();
        let v = serde_json::to_value(&m).unwrap();
        assert!(v["content"].is_string(), "{v}");
        let text = v["content"].as_str().unwrap();
        assert!(text.contains("[image omitted"), "{text}");
        assert!(text.contains("看图"), "{text}");
        assert!(!v.to_string().contains("image_url"), "{v}");
    }

    #[test]
    fn tool_message_is_always_string() {
        let m = ChatMessage::tool_result("c1".into(), "1 行".into());
        let v = serde_json::to_value(&m).unwrap();
        assert_eq!(v["role"], "tool");
        assert_eq!(v["content"], "1 行");
        assert_eq!(v["tool_call_id"], "c1");
    }

    #[test]
    fn tool_message_with_image_is_segmented() {
        // MCP 多模态工具结果：文本段在前，图片段在后
        let m = ChatMessage::tool_result_with_images(
            "c2".into(),
            "截图完成".into(),
            vec![crate::tools::OutputImage { media_type: "image/png".into(), data: "QUJD".into() }],
        );
        let v = serde_json::to_value(&m).unwrap();
        assert_eq!(v["role"], "tool");
        assert_eq!(v["tool_call_id"], "c2");
        let content = v["content"].as_array().unwrap();
        assert_eq!(content[0]["type"], "text");
        assert_eq!(content[0]["text"], "截图完成");
        assert_eq!(content[1]["type"], "image_url");
        assert_eq!(content[1]["image_url"]["url"], "data:image/png;base64,QUJD");
    }

    #[test]
    fn assistant_tool_calls_shape() {
        let call = ToolCall {
            id: "c1".into(),
            kind: "function".into(),
            function: FunctionCall { name: "shell".into(), arguments: "{}".into() },
        };
        let m = ChatMessage::assistant_tool_calls(String::new(), vec![call]);
        let v = serde_json::to_value(&m).unwrap();
        assert_eq!(v["content"], Value::Null);
        assert_eq!(v["tool_calls"][0]["type"], "function");
        assert_eq!(v["tool_calls"][0]["function"]["name"], "shell");
    }

    #[test]
    fn identity_marks_anthropic_protocol() {
        let mut config = ProviderConfig {
            protocol: Protocol::OpenAiCompat,
            base_url: "http://x/v1".into(),
            api_key: None,
            model: "m".into(),
            params: RequestParams::default(),
            gateway: None,
            deepseek_key: None,
        };
        assert_eq!(config.identity(), "builtin:http://x/v1#m");
        config.protocol = Protocol::Anthropic;
        assert_eq!(config.identity(), "builtin:anthropic:http://x/v1#m");
    }

    // ---- 思考强度 ----

    /// 模型名归一：供应商前缀（含 Bedrock 的两层）剥掉，版本号里的点不动。
    #[test]
    fn model_keys_strip_vendor_prefixes() {
        let same = [
            "claude-opus-4-7",
            "anthropic/claude-opus-4-7",
            "anthropic.claude-opus-4-7",
            "us.anthropic.claude-opus-4-7-v1:0",
            "anthropic/claude-opus-4-7-20260101",
            "Claude-Opus-4-7",
        ];
        for name in same {
            assert!(model_key(name).starts_with("claude-opus-4-7"), "{name}");
        }
        assert_eq!(model_key("anthropic/claude-opus-4-7-20260101"), "claude-opus-4-7-20260101");
        // 版本号里的点、名字里的下划线都不当供应商前缀剥
        assert_eq!(model_key("glm-4.5-air"), "glm-4.5-air");
        assert_eq!(model_key("qwen3-coder"), "qwen3-coder");
        assert_eq!(model_key("deepseek_ai/deepseek-chat"), "deepseek-chat");
        // `:free` 这类后缀不剥（前缀那一截得像个厂商名：纯字母或带连字符）
        assert_eq!(model_key("qwen3-coder:free"), "qwen3-coder:free");
    }

    /// 多模型中转点的模型名带端点标签（`cn:` / `global:`）：剥完要落回原来的厂商名，
    /// 否则 glm / qwen3 / deepseek 这些按厂商名开头的规则一条也认不出。
    #[test]
    fn model_keys_strip_relay_tags() {
        assert_eq!(model_key("cn:glm-5.3"), "glm-5.3");
        assert_eq!(model_key("global:gpt-5.5"), "gpt-5.5");
        assert_eq!(model_key("cn:deepseek-v4.1-flash"), "deepseek-v4.1-flash");
        let spec = |m: &str| thinking_spec(Protocol::OpenAiCompat, m);
        assert!(spec("cn:glm-5.3").is_known());
        assert!(spec("cn:deepseek-v4.1-flash").is_known());
        assert!(spec("global:gpt-5.5").is_known());
    }

    /// Anthropic 原生协议：按型号给档位和编码方式。
    #[test]
    fn anthropic_capability_table() {
        let spec = |m: &str| thinking_spec(Protocol::Anthropic, m);

        // 4.7 及以上：全档 adaptive，不吃非默认 temperature
        let opus47 = spec("claude-opus-4-7");
        assert_eq!(opus47.format.effort, Some(EffortField::OutputConfig));
        assert!(opus47.format.toggle.is_none() && !opus47.format.budget);
        assert_eq!(
            opus47.levels,
            &[
                ThinkingLevel::Off,
                ThinkingLevel::Low,
                ThinkingLevel::Medium,
                ThinkingLevel::High,
                ThinkingLevel::XHigh,
            ]
        );
        assert!(!opus47.accepts_temperature);

        // 4.6 没有 xhigh
        let opus46 = spec("claude-sonnet-4-6");
        assert!(!opus46.levels.contains(&ThinkingLevel::XHigh));
        assert!(opus46.levels.contains(&ThinkingLevel::Off));

        // 5.5 / Fable：关不掉（disabled 一律 400）
        let opus55 = spec("claude-opus-5-5");
        assert!(!opus55.can_disable(), "{:?}", opus55.levels);
        assert_eq!(opus55.min_level(), Some(ThinkingLevel::Low));
        assert!(!spec("claude-fable-5-1").can_disable());

        // 4.5 及更早：预算格式，能关
        let old = spec("claude-sonnet-4-5");
        assert!(old.format.budget && old.format.effort.is_none());
        assert!(old.can_disable());
        assert!(!old.levels.contains(&ThinkingLevel::XHigh));

        // 比本表更新、但仍是 claude：按最新一代的共同子集
        let newer = spec("claude-opus-6-1");
        assert_eq!(newer.format.effort, Some(EffortField::OutputConfig));
        assert!(!newer.can_disable());
        assert!(newer.levels.contains(&ThinkingLevel::XHigh));

        // 走 Anthropic 格式的第三方端点：DeepSeek 的官方兼容页明确写 `budget_tokens` is ignored、
        // 只认 `output_config.effort`，所以它不能走预算格式
        let ds = spec("deepseek-v4-pro");
        assert!(!ds.format.budget, "DeepSeek 不能再发 budget_tokens");
        assert_eq!(ds.format.effort, Some(EffortField::OutputConfig));
        assert!(ds.format.has_toggle() && ds.can_disable());
        assert_eq!(
            ds.levels,
            &[ThinkingLevel::Off, ThinkingLevel::Low, ThinkingLevel::High, ThinkingLevel::Max,]
        );
        // GLM 仍然走预算（它的 Anthropic 兼容端点还没反证过）
        assert!(spec("glm-4.6").format.budget);
        // 靠厂商字段开 / 关的那几家（Qwen，OpenAI 兼容表里认得的）：
        // 端点换成 Anthropic 格式照样发同一个开关字段，不能因为换了协议就没有这一项
        let qwen = spec("qwen3-max");
        assert_eq!(qwen.format.toggle.map(|t| t.key), Some("enable_thinking"));
        assert!(qwen.is_on_off());
        assert_eq!(qwen.levels, &[ThinkingLevel::Off, ThinkingLevel::High]);
        assert!(spec("cn:glm-5.3").is_known());
        assert!(spec("claude-nonexistent-1").is_known());
        // 别家第三方端点认不出：什么都不发（和 OpenAI 兼容表同一个原则）
        assert!(!spec("kimi-k2").is_known());
        assert!(!spec("llama-3-8b").is_known());
        assert!(!spec("deepseek-reasoner").is_known());
    }

    /// OpenAI 兼容 / Responses：认得出的模型才有档位，字段按各家写法。
    #[test]
    fn openai_capability_table() {
        let spec = |m: &str| thinking_spec(Protocol::OpenAiCompat, m);

        let gpt5 = spec("gpt-5-mini");
        assert_eq!(gpt5.format.effort, Some(EffortField::TopLevel));
        assert!(gpt5.format.toggle.is_none());
        assert!(gpt5.levels.contains(&ThinkingLevel::High));
        // gpt-5 / o 系列关不掉思考（以前「补全不发」靠名字猜，现在有表）
        assert!(!gpt5.can_disable());
        assert!(gpt5.levels.contains(&ThinkingLevel::Minimal));
        // 日期快照还是 gpt-5
        assert_eq!(spec("gpt-5-2025-08-07").levels, gpt5.levels);
        // gpt-5.1：能关，没有 minimal / xhigh
        let gpt51 = spec("gpt-5.1");
        assert_eq!(
            gpt51.levels,
            &[ThinkingLevel::Off, ThinkingLevel::Low, ThinkingLevel::Medium, ThinkingLevel::High]
        );
        assert_eq!(spec("gpt-5-1-codex").levels, gpt51.levels);
        // gpt-5.2 起有 xhigh
        let gpt52 = spec("gpt-5.2");
        assert!(gpt52.can_disable() && gpt52.levels.contains(&ThinkingLevel::XHigh));
        assert!(!gpt52.levels.contains(&ThinkingLevel::Minimal));
        // 不思考的对话版、不是这一家的
        assert!(!spec("gpt-5-chat-latest").is_known());
        assert!(!spec("gpt-50").is_known());

        assert!(spec("o3-mini").is_known());
        assert!(spec("o4-mini").is_known());

        // Qwen 3 是纯开 / 关型
        let qwen = spec("qwen3-max");
        assert_eq!(qwen.levels, &[ThinkingLevel::Off, ThinkingLevel::High]);
        assert_eq!(qwen.format.toggle.map(|t| t.key), Some("enable_thinking"));
        assert!(qwen.is_on_off());

        // GLM-4.5 ~ 5.1：只有开关
        let glm = spec("glm-4.6");
        assert_eq!(glm.levels, &[ThinkingLevel::Off, ThinkingLevel::High]);
        assert!(glm.is_on_off());
        // GLM-5.2 起开关 + effort 并存，最高到 max
        let glm52 = spec("glm-5.2");
        assert!(glm52.format.has_toggle() && glm52.format.effort.is_some());
        assert!(!glm52.is_on_off());
        assert_eq!(
            glm52.levels,
            &[ThinkingLevel::Off, ThinkingLevel::Low, ThinkingLevel::High, ThinkingLevel::Max]
        );
        // 5.3 起关不掉（传 disabled 报错）
        let glm53 = spec("glm-5.3");
        assert!(!glm53.can_disable());
        assert_eq!(glm53.levels, &[ThinkingLevel::Low, ThinkingLevel::High, ThinkingLevel::Max]);

        // DeepSeek V4：官方取值 none / low / high / max，开关和档位一起发
        let deepseek = spec("deepseek-flash");
        assert!(deepseek.format.has_toggle());
        assert_eq!(deepseek.format.effort, Some(EffortField::TopLevel));
        assert_eq!(
            deepseek.levels,
            &[ThinkingLevel::Off, ThinkingLevel::Low, ThinkingLevel::High, ThinkingLevel::Max,]
        );
        // V4 之前的（中转站上的 deepseek-v3.2）只给开 / 关，不发 effort
        let v3 = spec("deepseek-v3.2");
        assert!(v3.format.has_toggle() && v3.format.effort.is_none());
        assert_eq!(v3.levels, &[ThinkingLevel::Off, ThinkingLevel::High]);

        // Responses 只换编码方式，认得的模型一样
        let responses = thinking_spec(Protocol::Responses, "gpt-5");
        assert_eq!(responses.format.effort, Some(EffortField::Responses));
        assert_eq!(responses.levels, spec("gpt-5").levels);
        // 开关型模型换到 Responses 也还是开关：effort 不给它加一个
        let ds_responses = thinking_spec(Protocol::Responses, "deepseek-flash");
        assert_eq!(ds_responses.format.toggle.map(|t| t.key), Some("thinking"));
        assert_eq!(ds_responses.format.effort, Some(EffortField::Responses));

        // 认不出就什么都没有（界面不显示、请求也不发）
        assert!(!spec("llama-3-8b").is_known());
        // 总要先思考的模型不给「关」这一档：关不掉
        assert!(!spec("deepseek-reasoner").is_known());
    }

    /// 降级规则：取不高于所选档位的最近一档；都比它高就取最低一档。
    #[test]
    fn resolves_unsupported_levels() {
        let four_six = thinking_spec(Protocol::Anthropic, "claude-opus-4-6").levels;
        // 4.6 上选极高 → 高
        assert_eq!(resolve_level(ThinkingLevel::XHigh, four_six), Some(ThinkingLevel::High));
        assert_eq!(resolve_level(ThinkingLevel::Low, four_six), Some(ThinkingLevel::Low));

        let opus55 = thinking_spec(Protocol::Anthropic, "claude-opus-5-5").levels;
        // 关不掉的模型上选「关」→ 最低的开档
        assert_eq!(resolve_level(ThinkingLevel::Off, opus55), Some(ThinkingLevel::Low));

        // 不支持的档位（极低）按低处理
        let budget = thinking_spec(Protocol::Anthropic, "claude-sonnet-4-5").levels;
        assert_eq!(resolve_level(ThinkingLevel::Minimal, budget), Some(ThinkingLevel::Off));

        assert_eq!(resolve_level(ThinkingLevel::High, &[]), None);
    }

    /// `None` / 认不出的模型：请求体不被改动。
    #[test]
    fn unset_thinking_leaves_the_body_alone() {
        let params = RequestParams::default();
        let mut body = serde_json::json!({"model": "claude-opus-4-7", "temperature": 0.3});
        let before = body.clone();
        apply_thinking(&mut body, Protocol::Anthropic, "claude-opus-4-7", &params);
        // 4.7 以后不吃非默认 temperature：即使没设档位也不发
        assert!(body.get("temperature").is_none());
        assert!(body.get("thinking").is_none());
        assert_eq!(before["model"], body["model"]);

        // 认不出的模型：档位设了也不发字段
        let params = RequestParams { thinking: Some(ThinkingLevel::High), ..Default::default() };
        let mut body = serde_json::json!({"model": "llama-3-8b", "temperature": 0.3});
        apply_thinking(&mut body, Protocol::OpenAiCompat, "llama-3-8b", &params);
        assert_eq!(body, serde_json::json!({"model": "llama-3-8b", "temperature": 0.3}));
    }

    /// 「兜底」：能力表里没有的模型也按 `FALLBACK_LEVELS` 发 effort；
    /// 「默认」（`None`）和别的具体档位都不发，免得猜错一个字段让端点 400。
    #[test]
    fn fallback_sends_effort_for_unknown_models() {
        let params = RequestParams { thinking: Some(ThinkingLevel::Default), ..Default::default() };

        // OpenAI 兼容：`reasoning_effort`，最低落到 low；开着思考不发 temperature
        let mut body = serde_json::json!({"temperature": 0.3});
        apply_thinking(&mut body, Protocol::OpenAiCompat, "some-relay-model", &params);
        assert_eq!(body["reasoning_effort"], "low");
        assert!(body.get("temperature").is_none(), "{body}");

        // Responses：`reasoning.effort` + summary
        let mut body = serde_json::json!({});
        apply_thinking(&mut body, Protocol::Responses, "some-relay-model", &params);
        assert_eq!(body["reasoning"]["effort"], "low");
        assert_eq!(body["reasoning"]["summary"], "auto");

        // Anthropic 原生：`output_config.effort`，不发 thinking（开关字段不猜）
        let mut body = serde_json::json!({});
        apply_thinking(&mut body, Protocol::Anthropic, "some-relay-model", &params);
        assert_eq!(body["output_config"]["effort"], "low");
        assert!(body.get("thinking").is_none(), "{body}");

        // 「默认」和具体档位：认不出的模型上什么都不发（连 temperature 都不动）
        for level in [None, Some(ThinkingLevel::High), Some(ThinkingLevel::Off)] {
            let params = RequestParams { thinking: level, ..Default::default() };
            let mut body = serde_json::json!({"temperature": 0.2});
            apply_thinking(&mut body, Protocol::OpenAiCompat, "some-relay-model", &params);
            assert_eq!(body, serde_json::json!({"temperature": 0.2}), "{level:?}");
        }

        // 认得的模型上「兜底」= 它最低的那一档：glm-5.3 最低是「低」
        let mut body = serde_json::json!({});
        let glm = RequestParams { thinking: Some(ThinkingLevel::Default), ..Default::default() };
        apply_thinking(&mut body, Protocol::OpenAiCompat, "glm-5.3", &glm);
        assert_eq!(body["thinking"], serde_json::json!({ "type": "enabled" }), "{body}");
        assert_eq!(body["reasoning_effort"], "low", "{body}");

        // 只有开 / 关的模型（GLM-4.6）兜底仍然只发开关，不硬塞 effort
        let mut body = serde_json::json!({});
        apply_thinking(&mut body, Protocol::OpenAiCompat, "glm-4.6", &glm);
        assert_eq!(body["thinking"], serde_json::json!({ "type": "enabled" }), "{body}");
        assert!(body.get("reasoning_effort").is_none(), "{body}");

        // 纯 effort 的模型上「兜底」落最低的开档，不是最强那档
        let mut body = serde_json::json!({"max_tokens": 16_384});
        apply_thinking(&mut body, Protocol::Anthropic, "claude-opus-4-7", &glm);
        let spec = thinking_spec(Protocol::Anthropic, "claude-opus-4-7");
        let lowest = spec.levels.iter().copied().find(|l| !l.is_off()).unwrap();
        assert_eq!(body["output_config"]["effort"], effort_for(lowest), "{body}");
        assert_ne!(body["output_config"]["effort"], "xhigh", "{body}");
    }

    /// Adaptive 格式：开着思考时发 `adaptive` + `display` + `output_config.effort`，不发 temperature。
    #[test]
    fn encodes_adaptive_thinking() {
        let mut body = serde_json::json!({"max_tokens": 16_384, "temperature": 0.3});
        let params = RequestParams { thinking: Some(ThinkingLevel::High), ..Default::default() };
        apply_thinking(&mut body, Protocol::Anthropic, "claude-opus-4-7", &params);
        assert_eq!(body["thinking"]["type"], "adaptive");
        // 不带 display 时思考块正文是空串
        assert_eq!(body["thinking"]["display"], "summarized");
        assert_eq!(body["output_config"]["effort"], "high");
        assert!(body.get("temperature").is_none(), "{body}");

        // 关：显式 disabled
        let params = RequestParams { thinking: Some(ThinkingLevel::Off), ..Default::default() };
        let mut body = serde_json::json!({"max_tokens": 16_384});
        apply_thinking(&mut body, Protocol::Anthropic, "claude-opus-4-7", &params);
        assert_eq!(body["thinking"]["type"], "disabled");
        assert!(body.get("output_config").is_none());
    }

    /// 旧格式：`budget_tokens` 且顶到 `max_tokens` 之下；关掉时不带 thinking。
    #[test]
    fn encodes_budget_thinking() {
        let params = RequestParams { thinking: Some(ThinkingLevel::High), ..Default::default() };
        let mut body = serde_json::json!({"max_tokens": 16_384});
        apply_thinking(&mut body, Protocol::Anthropic, "claude-sonnet-4-5", &params);
        assert_eq!(body["thinking"]["type"], "enabled");
        assert_eq!(body["thinking"]["budget_tokens"], 16_000);
        assert_eq!(body["max_tokens"], 16_384);

        // 没设 max_tokens：抬到 预算 + 4096
        let mut body = serde_json::json!({});
        apply_thinking(&mut body, Protocol::Anthropic, "claude-sonnet-4-5", &params);
        assert_eq!(body["max_tokens"], 16_000 + 4_096);

        // 用户设的上限不够放默认预算：把预算压到上限以下
        let params_tight = RequestParams {
            thinking: Some(ThinkingLevel::High),
            max_tokens: Some(4_096),
            ..Default::default()
        };
        let mut body = serde_json::json!({"max_tokens": 4_096});
        apply_thinking(&mut body, Protocol::Anthropic, "claude-sonnet-4-5", &params_tight);
        assert_eq!(body["thinking"]["budget_tokens"], 3_072);

        // 压到下限以下就不开思考（发出去只会 400）
        let params_tiny = RequestParams {
            thinking: Some(ThinkingLevel::Low),
            max_tokens: Some(1_500),
            ..Default::default()
        };
        let mut body = serde_json::json!({"max_tokens": 1_500});
        apply_thinking(&mut body, Protocol::Anthropic, "claude-sonnet-4-5", &params_tiny);
        assert!(body.get("thinking").is_none(), "{body}");
        // 「关」：老格式不带字段
        let params_off = RequestParams { thinking: Some(ThinkingLevel::Off), ..Default::default() };
        let mut body = serde_json::json!({"max_tokens": 8_192});
        apply_thinking(&mut body, Protocol::Anthropic, "claude-sonnet-4-5", &params_off);
        assert!(body.get("thinking").is_none());
    }

    /// OpenAI 两套协议：effort 字段名不同，Responses 要带 summary。
    #[test]
    fn encodes_openai_effort() {
        let params = RequestParams { thinking: Some(ThinkingLevel::Medium), ..Default::default() };
        let mut body = serde_json::json!({});
        apply_thinking(&mut body, Protocol::OpenAiCompat, "gpt-5", &params);
        assert_eq!(body["reasoning_effort"], "medium");
        assert!(body.get("reasoning").is_none());

        let mut body = serde_json::json!({});
        apply_thinking(&mut body, Protocol::Responses, "o3", &params);
        assert_eq!(body["reasoning"]["effort"], "medium");
        // 不带 summary 就收不到思考摘要
        assert_eq!(body["reasoning"]["summary"], "auto");

        // 开 / 关型（Qwen）：档位只有「关 / 开」，中档及以上都当「开」
        let mut body = serde_json::json!({});
        apply_thinking(&mut body, Protocol::OpenAiCompat, "qwen3-max", &params);
        assert!(body.get("reasoning_effort").is_none(), "{body}");
        assert_eq!(body["enable_thinking"], true);
        let params_off = RequestParams { thinking: Some(ThinkingLevel::Off), ..Default::default() };
        let mut body = serde_json::json!({});
        apply_thinking(&mut body, Protocol::OpenAiCompat, "qwen3-max", &params_off);
        assert_eq!(body["enable_thinking"], false);
    }

    /// 开关 + 档位并存的那几家（DeepSeek V4 / GLM-5.2+）：两个字段一起发，
    /// 而且「关」必须发 `disabled`——它们默认开着思考，不发等于没关
    #[test]
    fn encodes_toggle_with_effort() {
        let high = RequestParams { thinking: Some(ThinkingLevel::High), ..Default::default() };
        let mut body = serde_json::json!({});
        apply_thinking(&mut body, Protocol::OpenAiCompat, "deepseek-flash", &high);
        assert_eq!(body["thinking"], serde_json::json!({ "type": "enabled" }), "{body}");
        assert_eq!(body["reasoning_effort"], "high", "{body}");

        // 关：显式 disabled，不发 effort
        let off = RequestParams { thinking: Some(ThinkingLevel::Off), ..Default::default() };
        let mut body = serde_json::json!({});
        apply_thinking(&mut body, Protocol::OpenAiCompat, "deepseek-flash", &off);
        assert_eq!(body["thinking"], serde_json::json!({ "type": "disabled" }), "{body}");

        // 最高档 `max`：DeepSeek / GLM 的最高就是 max，不能被降成 high
        let max = RequestParams { thinking: Some(ThinkingLevel::Max), ..Default::default() };
        let mut body = serde_json::json!({});
        apply_thinking(&mut body, Protocol::OpenAiCompat, "deepseek-v4-pro", &max);
        assert_eq!(body["reasoning_effort"], "max", "{body}");

        // GLM-5.3：关不掉，选「关」也要发 enabled（发 disabled 会 400）
        let mut body = serde_json::json!({});
        apply_thinking(&mut body, Protocol::OpenAiCompat, "glm-5.3", &off);
        assert_eq!(body["thinking"], serde_json::json!({ "type": "enabled" }), "{body}");
        assert_eq!(body["reasoning_effort"], "low", "{body}");

        // 同一个模型换到 Responses：开关照旧，effort 换成 reasoning.effort + summary
        let mut body = serde_json::json!({});
        apply_thinking(&mut body, Protocol::Responses, "deepseek-flash", &high);
        assert_eq!(body["thinking"], serde_json::json!({ "type": "enabled" }), "{body}");
        assert_eq!(body["reasoning"]["effort"], "high", "{body}");
        assert_eq!(body["reasoning"]["summary"], "auto", "{body}");
    }

    /// 走 Anthropic 兼容端点的第三方（DeepSeek）：effort 落点是 `output_config.effort`，
    /// **不能**发 `budget_tokens`（官方明确说它被忽略）
    #[test]
    fn encodes_deepseek_on_anthropic_endpoint() {
        let params = RequestParams { thinking: Some(ThinkingLevel::High), ..Default::default() };
        let mut body = serde_json::json!({"max_tokens": 16_384});
        apply_thinking(&mut body, Protocol::Anthropic, "deepseek-v4-pro", &params);
        assert_eq!(body["output_config"]["effort"], "high", "{body}");
        assert_eq!(body["thinking"], serde_json::json!({ "type": "enabled" }), "{body}");
        assert!(body["thinking"].get("budget_tokens").is_none(), "{body}");
        // 不是 Anthropic 自家模型，不要塞 adaptive / display
        assert!(body["thinking"].get("display").is_none(), "{body}");
        // max_tokens 不该被预算逻辑改动
        assert_eq!(body["max_tokens"], 16_384);

        let off = RequestParams { thinking: Some(ThinkingLevel::Off), ..Default::default() };
        let mut body = serde_json::json!({});
        apply_thinking(&mut body, Protocol::Anthropic, "deepseek-v4-pro", &off);
        assert_eq!(body["thinking"], serde_json::json!({ "type": "disabled" }), "{body}");
        assert!(body.get("output_config").is_none(), "{body}");
    }

    /// 新增的 `Max` 档排在 `XHigh` 后面：认得出 `max` 的模型用它，
    /// 认不出的按「取不高于它的最近一档」降级
    #[test]
    fn max_is_the_top_level() {
        assert!(ThinkingLevel::Max > ThinkingLevel::XHigh);
        // claude 没有 max：选最高 → 极高
        let claude = thinking_spec(Protocol::Anthropic, "claude-opus-4-7").levels;
        assert_eq!(resolve_level(ThinkingLevel::Max, claude), Some(ThinkingLevel::XHigh));
        // 4.6 连 xhigh 都没有 → 高
        let four_six = thinking_spec(Protocol::Anthropic, "claude-opus-4-6").levels;
        assert_eq!(resolve_level(ThinkingLevel::Max, four_six), Some(ThinkingLevel::High));
        // 认不出的模型：兜底列表里没有 max，落在 xhigh
        assert_eq!(resolve_level(ThinkingLevel::Max, FALLBACK_LEVELS), Some(ThinkingLevel::XHigh));
    }

    /// 界面 / 内核之间传的档位名：认 serde 的名字（`snake_case`，大小写和空白不敏感），别的报错
    #[test]
    fn parses_thinking_wire_names() {
        // `XHigh` 在 `snake_case` 下是 `x_high`；两种写法都收，
        // 因为这是手写设置文件里最容易写错的一个
        for (text, level) in [
            ("off", ThinkingLevel::Off),
            ("minimal", ThinkingLevel::Minimal),
            ("low", ThinkingLevel::Low),
            ("medium", ThinkingLevel::Medium),
            ("high", ThinkingLevel::High),
            ("x_high", ThinkingLevel::XHigh),
            ("xhigh", ThinkingLevel::XHigh),
            ("max", ThinkingLevel::Max),
            ("default", ThinkingLevel::Default),
            ("  HIGH ", ThinkingLevel::High),
        ] {
            assert_eq!(parse_thinking_level(text).unwrap(), level, "{text}");
        }
        // 界面上的中文档位名不是 wire 格式
        assert!(parse_thinking_level("极高").is_err());
        assert!(parse_thinking_level("ultra").is_err());
        assert!(parse_thinking_level("").is_err());
    }
}
