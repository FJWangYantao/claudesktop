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
    /// 额外的请求体字段（JSON 对象）；浅合并进请求体，同名键覆盖默认值
    pub extra: Value,
}

impl RequestParams {
    pub fn is_empty(&self) -> bool {
        self.temperature.is_none()
            && self.max_tokens.is_none()
            && self.extra.as_object().is_none_or(|o| o.is_empty())
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
}

impl ChatMessage {
    pub fn new(role: &str, text: impl Into<String>) -> Self {
        Self::with_parts(role, vec![ContentPart::Text(text.into())])
    }

    pub fn with_parts(role: &str, parts: Vec<ContentPart>) -> Self {
        Self { role: role.to_string(), parts, tool_calls: Vec::new(), tool_call_id: None }
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
}
