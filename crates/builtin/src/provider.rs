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
}

impl ThinkingLevel {
    /// 除「关」以外的档位（按强弱）。
    pub const ON: [ThinkingLevel; 5] = [
        ThinkingLevel::Minimal,
        ThinkingLevel::Low,
        ThinkingLevel::Medium,
        ThinkingLevel::High,
        ThinkingLevel::XHigh,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Self::Off => "关",
            Self::Minimal => "极低",
            Self::Low => "低",
            Self::Medium => "中",
            Self::High => "高",
            Self::XHigh => "极高",
        }
    }

    /// 档位里有没有「关」这一档（没有 = 模型关不掉思考，见 [`min_level`]）。
    pub fn is_off(self) -> bool {
        self == Self::Off
    }
}

/// 各协议怎么把档位翻译成请求字段。
#[derive(Debug, Clone, Copy)]
pub enum ThinkingFormat {
    /// `thinking:{type:"adaptive",display:"summarized"}` + `output_config.effort`（新模型）
    AnthropicAdaptive,
    /// `thinking:{type:"enabled",budget_tokens}`（老模型 / 按 Anthropic 格式兼容的第三方端点）
    AnthropicBudget,
    /// `reasoning_effort`（OpenAI 兼容 Chat Completions）
    OpenAiEffort,
    /// `reasoning:{effort, summary:"auto"}`（OpenAI Responses）
    ResponsesEffort,
    /// 只有开 / 关的厂商字段，值为 `extra` 里那个键的内容（`true` / `{"type":"enabled"}`）。
    /// 「开」对应哪一档由能力表决定（都存成 `High`）。
    ///
    /// 手写 `PartialEq` 而不是 derive：函数指针的地址不保证唯一，derive 出来的比较没有意义
    Toggle {
        key: &'static str,
        /// 开 / 关时这个键的取值
        on: fn() -> Value,
        off: fn() -> Value,
    },
}

impl PartialEq for ThinkingFormat {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::AnthropicAdaptive, Self::AnthropicAdaptive)
            | (Self::AnthropicBudget, Self::AnthropicBudget)
            | (Self::OpenAiEffort, Self::OpenAiEffort)
            | (Self::ResponsesEffort, Self::ResponsesEffort) => true,
            // 字段一样就算一样：取值函数由 `key` 唯一决定
            (Self::Toggle { key: a, .. }, Self::Toggle { key: b, .. }) => a == b,
            _ => false,
        }
    }
}

impl Eq for ThinkingFormat {}

/// 一门协议 + 一个模型支持什么：能选哪些档位，字段怎么发。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ThinkingSpec {
    /// 支持的档位（升序）。空 = 认不出这个模型：界面不显示、请求里也不带
    pub levels: &'static [ThinkingLevel],
    pub format: ThinkingFormat,
    /// 认不认非默认的 `temperature`。Opus 4.7 以后的新模型一律不认（400），
    /// 和数据里有没有「开着思考」无关，所以单独一个标志
    pub accepts_temperature: bool,
}

impl ThinkingSpec {
    /// 认不出（界面整行不显示、请求不带任何思考字段）
    pub const NONE: ThinkingSpec = ThinkingSpec {
        levels: &[],
        format: ThinkingFormat::AnthropicBudget,
        accepts_temperature: true,
    };

    pub fn is_known(&self) -> bool {
        !self.levels.is_empty()
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

    /// 选中的档位落在这个模型上的实际档位（见 [`resolve_level`]）。
    ///
    /// 开 / 关型（`Toggle`）多一步：它只有「开 / 关」，用户选中间几档都按「开」处理，
    /// 不能因为「高」比「中」大就降成「关」——那等于把用户要的思考关掉了。
    pub fn resolve(&self, chosen: ThinkingLevel) -> Option<ThinkingLevel> {
        if matches!(self.format, ThinkingFormat::Toggle { .. })
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
        let Some(cut) = cut else { break };
        let rest = &name[cut + 1..];
        if !rest.chars().any(|c| c.is_ascii_alphabetic()) {
            break;
        }
        name = rest.to_string();
    }
    name
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
        format: ThinkingFormat::OpenAiEffort,
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
            format: ThinkingFormat::Toggle {
                key: "enable_thinking",
                on: || Value::Bool(true),
                off: || Value::Bool(false),
            },
            accepts_temperature: true,
        };
    }
    // GLM 4.5 ~ 5.2：`thinking.type` 开 / 关；5.3 起关不掉，所以不在表里给「关」
    if let Some(rest) = name.strip_prefix("glm-") {
        // 版本号取第一个 `-` 之前那段（`4.5-air` → 4.5）；GLM-4 这种老版本给不出小数就认不出
        let version: f32 = rest.split('-').next().unwrap_or_default().parse().unwrap_or(0.);
        if version < 4.5 {
            return ThinkingSpec::NONE;
        }
        let thinking_of = |levels: &'static [ThinkingLevel]| ThinkingSpec {
            levels,
            format: ThinkingFormat::Toggle {
                key: "thinking",
                on: || serde_json::json!({ "type": "enabled" }),
                off: || serde_json::json!({ "type": "disabled" }),
            },
            accepts_temperature: true,
        };
        if version >= 5.3 {
            return thinking_of(&[ThinkingLevel::High]);
        }
        return thinking_of(&[ThinkingLevel::Off, ThinkingLevel::High]);
    }
    // DeepSeek V3.1+ / V4：`thinking.type` 开 / 关（官方示例：`{"thinking":{"type":"enabled"}}`）
    if name.starts_with("deepseek-") && !name.contains("reasoner") && !name.contains("-r1") {
        return ThinkingSpec {
            levels: &[ThinkingLevel::Off, ThinkingLevel::High],
            format: ThinkingFormat::Toggle {
                key: "thinking",
                on: || serde_json::json!({ "type": "enabled" }),
                off: || serde_json::json!({ "type": "disabled" }),
            },
            accepts_temperature: true,
        };
    }
    ThinkingSpec::NONE
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
        format: ThinkingFormat::AnthropicAdaptive,
        accepts_temperature,
    };
    // 名字里不带 claude 的是 Anthropic 格式的第三方端点。只认确认过收旧格式预算的那几家
    // （DeepSeek、GLM，和 OpenAI 兼容表里认得的同一批）；别的认不出，什么都不发
    if !name.contains("claude") {
        let budget_ok = name.starts_with("deepseek-") || name.starts_with("glm-");
        if !budget_ok || !openai_spec(name).is_known() {
            return ThinkingSpec::NONE;
        }
        return ThinkingSpec {
            levels: &[
                ThinkingLevel::Off,
                ThinkingLevel::Low,
                ThinkingLevel::Medium,
                ThinkingLevel::High,
            ],
            format: ThinkingFormat::AnthropicBudget,
            accepts_temperature: true,
        };
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
                format: ThinkingFormat::AnthropicBudget,
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
            let spec = openai_spec(&key);
            match spec.format {
                ThinkingFormat::OpenAiEffort => ThinkingSpec {
                    format: if protocol == Protocol::Responses {
                        ThinkingFormat::ResponsesEffort
                    } else {
                        ThinkingFormat::OpenAiEffort
                    },
                    ..spec
                },
                _ => spec,
            }
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

/// 老格式（[`ThinkingFormat::AnthropicBudget`]）每一档换算出来的 `budget_tokens`。
/// 界面上在档位旁边标一句「约 N token」，请求体组装也按它算。
pub fn thinking_budget_for(level: ThinkingLevel) -> u64 {
    match level {
        ThinkingLevel::Off | ThinkingLevel::Minimal | ThinkingLevel::Low => 2_048,
        ThinkingLevel::Medium => 8_192,
        ThinkingLevel::High | ThinkingLevel::XHigh => 16_000,
    }
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
    }
}

/// 把档位写进请求体（三个 provider 组完请求体、合 `extra` **之前**各调一次）。
///
/// - `params.thinking == None`：什么都不做（用模型自己的默认）
/// - 认不出的模型：什么都不做（宁可没有这个选项，也不要发个对方不认的字段）
/// - 模型不支持选的这一档：按 [`resolve_level`] 降级后再编
/// - 开着思考时不发 `temperature`；`accepts_temperature == false` 的模型（Opus 4.7 以后）
///   不管开没开都不发——这是以前「填了预算才不发」之外的另一半
pub fn apply_thinking(body: &mut Value, protocol: Protocol, model: &str, params: &RequestParams) {
    let spec = thinking_spec(protocol, model);
    if !spec.accepts_temperature {
        body.as_object_mut().map(|o| o.remove("temperature"));
    }
    let Some(chosen) = params.thinking else { return };
    if !spec.is_known() {
        return;
    }
    let Some(level) = spec.resolve(chosen) else { return };
    let thinking_on = !level.is_off();
    if thinking_on {
        // Anthropic 要求开 thinking 时 temperature 只能是默认值
        body.as_object_mut().map(|o| o.remove("temperature"));
    }
    match spec.format {
        ThinkingFormat::AnthropicAdaptive => {
            if level.is_off() {
                body["thinking"] = serde_json::json!({ "type": "disabled" });
            } else {
                // `display:"summarized"` 必须带：4.7 以后默认 omitted，思考块正文是空串
                body["thinking"] =
                    serde_json::json!({ "type": "adaptive", "display": "summarized" });
                body["output_config"] = serde_json::json!({ "effort": effort_for(level) });
            }
        }
        ThinkingFormat::AnthropicBudget => {
            if level.is_off() {
                // 老格式：不带 thinking 就是不思考
                return;
            }
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
        ThinkingFormat::OpenAiEffort => {
            body["reasoning_effort"] = serde_json::json!(effort_for(level));
        }
        ThinkingFormat::ResponsesEffort => {
            // 不带 `summary:"auto"` 就收不到 `reasoning_summary_text.delta`，思考区是空的
            body["reasoning"] =
                serde_json::json!({ "effort": effort_for(level), "summary": "auto" });
        }
        ThinkingFormat::Toggle { key, on, off } => {
            body[key] = if thinking_on { on() } else { off() };
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
    }

    /// Anthropic 原生协议：按型号给档位和编码方式。
    #[test]
    fn anthropic_capability_table() {
        let spec = |m: &str| thinking_spec(Protocol::Anthropic, m);

        // 4.7 及以上：全档 adaptive，不吃非默认 temperature
        let opus47 = spec("claude-opus-4-7");
        assert_eq!(opus47.format, ThinkingFormat::AnthropicAdaptive);
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
        assert_eq!(old.format, ThinkingFormat::AnthropicBudget);
        assert!(old.can_disable());
        assert!(!old.levels.contains(&ThinkingLevel::XHigh));

        // 比本表更新、但仍是 claude：按最新一代的共同子集
        let newer = spec("claude-opus-6-1");
        assert_eq!(newer.format, ThinkingFormat::AnthropicAdaptive);
        assert!(!newer.can_disable());
        assert!(newer.levels.contains(&ThinkingLevel::XHigh));

        // 走 Anthropic 格式的第三方端点（DeepSeek / GLM）：认旧格式预算
        assert_eq!(spec("deepseek-v4-pro").format, ThinkingFormat::AnthropicBudget);
        assert!(spec("deepseek-v4-pro").can_disable());
        assert_eq!(spec("glm-5.3").format, ThinkingFormat::AnthropicBudget);
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
        assert_eq!(gpt5.format, ThinkingFormat::OpenAiEffort);
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

        // Qwen 3 / GLM / DeepSeek 是开 / 关型
        let qwen = spec("qwen3-max");
        assert_eq!(qwen.levels, &[ThinkingLevel::Off, ThinkingLevel::High]);
        assert!(matches!(qwen.format, ThinkingFormat::Toggle { key: "enable_thinking", .. }));

        let glm = spec("glm-4.6");
        assert_eq!(glm.levels, &[ThinkingLevel::Off, ThinkingLevel::High]);
        // 5.3 起关不掉
        let glm53 = spec("glm-5.3");
        assert_eq!(glm53.levels, &[ThinkingLevel::High]);

        let deepseek = spec("deepseek-chat");
        assert_eq!(deepseek.levels, &[ThinkingLevel::Off, ThinkingLevel::High]);

        // Responses 只换编码方式，认得的模型一样
        let responses = thinking_spec(Protocol::Responses, "gpt-5");
        assert_eq!(responses.format, ThinkingFormat::ResponsesEffort);
        assert_eq!(responses.levels, spec("gpt-5").levels);

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
}
