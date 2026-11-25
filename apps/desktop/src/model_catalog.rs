//! 输入框模型选择器的纯数据层：品牌识别、价格档位、上下文窗口、能力标签与英文档位文案。
//!
//! 这里不依赖 gpui，只做「从设置和模型名推导要显示什么」，可以直接单测；
//! 绘制在 `app/model_picker.rs`。设置页那一栏（中文文案、`MenuChip`）不经过这里。

use crate::pricing::{self, Currency, Price};
use claudesktop_builtin::ThinkingLevel;
use std::sync::OnceLock;

/// 模型清单里的品牌：决定左侧图标、着色和无图标时的首字母徽章。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ModelBrand {
    Anthropic,
    OpenAi,
    Gemini,
    Mistral,
    DeepSeek,
    Kimi,
    Zhipu,
    Ollama,
    OpenRouter,
    Generic,
}

impl ModelBrand {
    /// 先按模型 ID 认（中转 / OpenRouter 上会是 `厂商/模型`），认不出再按端点地址兜底。
    pub fn detect(model: &str, base_url: &str) -> Self {
        let m = model.to_ascii_lowercase();
        let model = if m.contains("claude") || m.contains("anthropic") {
            Some(Self::Anthropic)
        } else if m.contains("gpt") || m.contains("openai") || m.contains("chatgpt") {
            Some(Self::OpenAi)
        } else if m.contains("gemini") || m.contains("google/") || m.contains("gemma") {
            Some(Self::Gemini)
        } else if m.contains("mistral") || m.contains("mixtral") || m.contains("codestral") {
            Some(Self::Mistral)
        } else if m.contains("deepseek") {
            Some(Self::DeepSeek)
        } else if m.contains("kimi") || m.contains("moonshot") {
            Some(Self::Kimi)
        } else if m.contains("glm") || m.contains("zhipu") || m.contains("chatglm") {
            Some(Self::Zhipu)
        } else {
            None
        };
        // `o1` / `o3` / `o4` 这类没有 `gpt` 前缀的 OpenAI 推理模型
        let model = model.or_else(|| {
            let last = m.rsplit('/').next().unwrap_or(&m);
            (last.starts_with("o1") || last.starts_with("o3") || last.starts_with("o4"))
                .then_some(Self::OpenAi)
        });
        model.unwrap_or_else(|| match crate::assets::provider_logo_for_url(base_url) {
            Some("anthropic") => Self::Anthropic,
            Some("openai") => Self::OpenAi,
            Some("deepseek") => Self::DeepSeek,
            Some("kimi") => Self::Kimi,
            Some("zhipu") => Self::Zhipu,
            Some("ollama") => Self::Ollama,
            Some("openrouter") => Self::OpenRouter,
            _ => Self::Generic,
        })
    }

    /// 内置的单色 SVG；`None` 用圆角首字母徽章（Mistral 这类没有收录图标的）。
    pub fn svg_path(self) -> Option<&'static str> {
        match self {
            Self::Anthropic => crate::assets::provider_logo_path("anthropic"),
            Self::OpenAi => crate::assets::provider_logo_path("openai"),
            Self::Gemini => crate::assets::kernel_logo_path("gemini"),
            Self::DeepSeek => crate::assets::provider_logo_path("deepseek"),
            Self::Kimi => crate::assets::provider_logo_path("kimi"),
            Self::Zhipu => crate::assets::provider_logo_path("zhipu"),
            Self::Ollama => crate::assets::provider_logo_path("ollama"),
            Self::OpenRouter => crate::assets::provider_logo_path("openrouter"),
            Self::Mistral | Self::Generic => None,
        }
    }

    /// 品牌主色（0xRRGGBB）；`None` 跟随主题前景色（OpenAI / Kimi / Ollama 用中性黑，徽章也用它）。
    /// 深色版针对深色卡片底整体提亮，保证图标在 `#1F1F1F` 附近仍然清楚。
    pub fn tint_hex(self, dark: bool) -> Option<u32> {
        let pair = match self {
            Self::Anthropic => (0xD97757, 0xEC9272),
            Self::Gemini => (0x4A7CF7, 0x7BA4FF),
            Self::DeepSeek => (0x4D6BFE, 0x7E97FF),
            Self::Zhipu => (0x3859FF, 0x738BFF),
            Self::OpenRouter => (0x6366F1, 0x8B8EFF),
            Self::OpenAi | Self::Kimi | Self::Ollama | Self::Mistral | Self::Generic => {
                return None;
            }
        };
        Some(if dark { pair.1 } else { pair.0 })
    }

    /// 没有 SVG 时的徽章字符：Mistral 固定 `M`，其余取模型名里的第一个字母数字。
    pub fn monogram(self, model: &str) -> char {
        match self {
            Self::Mistral => 'M',
            _ => model
                .rsplit('/')
                .next()
                .and_then(|s| s.chars().find(|c| c.is_ascii_alphanumeric()))
                .map(|c| c.to_ascii_uppercase())
                .unwrap_or('?'),
        }
    }
}

fn default_price_cache() -> &'static [Price] {
    static CACHE: OnceLock<Vec<Price>> = OnceLock::new();
    CACHE.get_or_init(pricing::default_prices)
}

/// 价格档位 1..=4，对应清单右侧的 `$` 格数；查不到单价返回 `None`（该列留空，不瞎猜）。
///
/// 用输入价 3 份 + 输出价 1 份加权，人民币按 7.2 折算成美元等效价——只用来做相对分档，
/// 和用量页的实际计费无关（那边直接读 [`pricing`]）。
pub fn cost_tier(custom: Option<&[Price]>, model: &str) -> Option<u8> {
    let prices: &[Price] = match custom {
        Some(prices) => prices,
        None => default_price_cache(),
    };
    let p = pricing::find(prices, model)?;
    let fx = match p.currency {
        Currency::Usd => 1.0,
        Currency::Cny => 1.0 / 7.2,
    };
    let blended = (p.input * 0.75 + p.output * 0.25) * fx;
    Some(if blended >= 6.5 {
        4
    } else if blended >= 2.2 {
        3
    } else if blended >= 0.7 {
        2
    } else {
        1
    })
}

/// 静态模型说明：一句话英文定位 + 端点没上报窗口时的兜底上下文大小。
pub struct ModelMetaEntry {
    /// 包含匹配的关键字（小写，`.` 归一成 `-`，和 [`pricing::find`] 同一套规则）
    pub pattern: &'static str,
    pub summary: &'static str,
    pub fallback_context: Option<u64>,
}

const CATALOG: &[ModelMetaEntry] = &[
    ModelMetaEntry {
        pattern: "claude-opus",
        summary: "The most capable Claude. For long agent runs and the hardest coding work.",
        fallback_context: Some(1_000_000),
    },
    ModelMetaEntry {
        pattern: "claude-sonnet",
        summary: "Best balance of intelligence and speed for everyday coding and agent tasks.",
        fallback_context: Some(1_000_000),
    },
    ModelMetaEntry {
        pattern: "claude-haiku",
        summary: "Fast and lightweight. Built for quick edits, triage, and high-throughput runs.",
        fallback_context: Some(1_000_000),
    },
    ModelMetaEntry {
        pattern: "gpt-5-pro",
        summary: "Deepest OpenAI reasoning for complex architecture and research tasks.",
        fallback_context: Some(400_000),
    },
    ModelMetaEntry {
        pattern: "gpt-5",
        summary: "Strong general-purpose coding and reasoning across large codebases.",
        fallback_context: Some(1_050_000),
    },
    ModelMetaEntry {
        pattern: "gemini-3-8-flash",
        summary: "High-speed multimodal reasoning with generous context at lower cost.",
        fallback_context: Some(1_000_000),
    },
    ModelMetaEntry {
        pattern: "gemini-",
        summary: "Large-context multimodal model well suited for big files and visual inputs.",
        fallback_context: Some(1_000_000),
    },
    ModelMetaEntry {
        pattern: "deepseek-v4-pro",
        summary: "Flagship DeepSeek reasoning model for tough engineering and math.",
        fallback_context: Some(128_000),
    },
    ModelMetaEntry {
        pattern: "deepseek-",
        summary: "Cost-efficient model for daily coding, refactoring, and fast iteration.",
        fallback_context: Some(128_000),
    },
    ModelMetaEntry {
        pattern: "kimi-k3",
        summary: "Strong agentic coding and long-horizon problem solving.",
        fallback_context: Some(256_000),
    },
    ModelMetaEntry {
        pattern: "glm-5",
        summary: "Balanced bilingual coding and tool-use model for daily workflows.",
        fallback_context: Some(200_000),
    },
    ModelMetaEntry {
        pattern: "mistral-large",
        summary: "Capable European flagship for multilingual reasoning and structured tasks.",
        fallback_context: Some(524_288),
    },
];

fn normalize(name: &str) -> String {
    name.trim().to_ascii_lowercase().replace('.', "-")
}

/// 最长匹配的静态说明；没有条目返回 `None`（不编造介绍）。
pub fn lookup(model: &str) -> Option<&'static ModelMetaEntry> {
    let norm = normalize(model);
    CATALOG.iter().filter(|e| norm.contains(e.pattern)).max_by_key(|e| e.pattern.len())
}

pub fn summary(model: &str) -> Option<&'static str> {
    lookup(model).map(|e| e.summary)
}

/// 上下文窗口的兜底值（端点上报过的不走这里）。
pub fn fallback_context(model: &str) -> Option<u64> {
    lookup(model).and_then(|e| e.fallback_context)
}

/// `1_000_000` → `1M`，`1_050_000` → `1.05M`，`524_288` → `524K`，`200_000` → `200K`。
pub fn format_context(tokens: u64) -> String {
    if tokens >= 1_000_000 {
        let m = tokens as f64 / 1_000_000.0;
        if (m - m.round()).abs() < 0.005 {
            format!("{}M", m.round() as u64)
        } else {
            let s = format!("{m:.2}");
            format!("{}M", s.trim_end_matches('0').trim_end_matches('.'))
        }
    } else if tokens >= 1_000 {
        format!("{}K", tokens / 1_000)
    } else {
        tokens.to_string()
    }
}

/// 档位的英文文案（输入框模型面板用；设置页仍用 `ThinkingLevel::label` 的中文）。
pub fn effort_label_en(level: Option<ThinkingLevel>) -> &'static str {
    match level {
        None => "Default",
        Some(ThinkingLevel::Off) => "Off",
        Some(ThinkingLevel::Minimal) => "Min",
        Some(ThinkingLevel::Low) => "Low",
        Some(ThinkingLevel::Medium) => "Medium",
        Some(ThinkingLevel::High) => "High",
        Some(ThinkingLevel::XHigh) => "XHigh",
        Some(ThinkingLevel::Max) => "Max",
        Some(ThinkingLevel::Default) => "Auto",
    }
}

/// 按当前模型和能力配置推导能力胶囊，不虚标：
/// - `Vision`：能力表明确能看图，且没被端点 400 记进 `no_vision_models`
/// - `Files`：代码模式有 @ 引用 / read_file，聊天模式有文本附件
/// - `Tools`：只有代码模式的工具循环才用得上
/// - `Search`：代码模式常驻 `web_search`；聊天模式要开联网
pub fn capabilities(
    protocol: &str,
    model: &str,
    banned: bool,
    is_chat: bool,
    web: bool,
) -> Vec<&'static str> {
    let mut caps = Vec::with_capacity(4);
    if !banned && claudesktop_core::image_prep::vision_support(protocol, model) == Some(true) {
        caps.push("Vision");
    }
    caps.push("Files");
    if !is_chat {
        caps.push("Tools");
    }
    if !is_chat || web {
        caps.push("Search");
    }
    caps
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_brand_from_model_id_before_endpoint() {
        assert_eq!(ModelBrand::detect("claude-opus-5-5", ""), ModelBrand::Anthropic);
        assert_eq!(ModelBrand::detect("openai/gpt-5.4", ""), ModelBrand::OpenAi);
        assert_eq!(ModelBrand::detect("o3-mini", "https://example.com"), ModelBrand::OpenAi);
        assert_eq!(ModelBrand::detect("gemini-3-1-pro", ""), ModelBrand::Gemini);
        assert_eq!(ModelBrand::detect("deepseek-v4-pro", ""), ModelBrand::DeepSeek);
        assert_eq!(ModelBrand::detect("glm-5", ""), ModelBrand::Zhipu);
    }

    #[test]
    fn falls_back_to_endpoint_and_generic() {
        assert_eq!(
            ModelBrand::detect("some-model", "https://api.deepseek.com/v1"),
            ModelBrand::DeepSeek
        );
        assert_eq!(
            ModelBrand::detect("some-model", "https://api.example.com"),
            ModelBrand::Generic
        );
    }

    #[test]
    fn prices_spread_over_four_tiers() {
        assert_eq!(cost_tier(None, "claude-opus-5-5"), Some(4));
        assert_eq!(cost_tier(None, "claude-sonnet-5-5"), Some(3));
        assert_eq!(cost_tier(None, "gemini-3-8-flash"), Some(2));
        assert_eq!(cost_tier(None, "claude-haiku-5-5"), Some(1));
        assert_eq!(cost_tier(None, "no-such-model"), None);
    }

    #[test]
    fn cny_models_convert_before_tiering() {
        // GLM-5.3 是 ¥8 / ¥28，折成美元后落在第 2 档
        assert_eq!(cost_tier(None, "glm-5-3"), Some(2));
        assert_eq!(cost_tier(None, "glm-5-3-flash"), Some(1));
    }

    #[test]
    fn formats_context_compactly() {
        assert_eq!(format_context(1_000_000), "1M");
        assert_eq!(format_context(1_050_000), "1.05M");
        assert_eq!(format_context(524_288), "524K");
        assert_eq!(format_context(200_000), "200K");
        assert_eq!(format_context(999), "999");
    }

    #[test]
    fn longest_pattern_wins() {
        assert_eq!(summary("deepseek-v4-pro"), Some(CATALOG[7].summary));
        assert_eq!(summary("deepseek-chat"), Some(CATALOG[8].summary));
        assert_eq!(summary("mystery-model"), None);
    }

    #[test]
    fn capabilities_do_not_overclaim() {
        let caps = capabilities("openai", "deepseek-chat", false, true, false);
        assert!(!caps.contains(&"Vision"));
        assert!(caps.contains(&"Files"));
        assert!(!caps.contains(&"Tools"));
        assert!(!caps.contains(&"Search"));
        assert_eq!(
            capabilities("openai", "gpt-5", false, false, false),
            vec!["Vision", "Files", "Tools", "Search"]
        );
        // 被记进 no_vision_models 的，就算能力表说能看也不显示
        assert_eq!(
            capabilities("openai", "gpt-5", true, false, true),
            vec!["Files", "Tools", "Search"]
        );
    }

    #[test]
    fn effort_labels_are_english() {
        assert_eq!(effort_label_en(None), "Default");
        assert_eq!(effort_label_en(Some(ThinkingLevel::XHigh)), "XHigh");
        assert_eq!(effort_label_en(Some(ThinkingLevel::Default)), "Auto");
    }
}
