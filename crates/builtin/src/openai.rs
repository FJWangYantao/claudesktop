//! OpenAI 兼容的 Chat Completions 流式 provider。
//!
//! 一个实现覆盖 OpenAI / OpenRouter / Ollama / vLLM / LM Studio，以及
//! Antigravity Tools 这类本地网关。思考内容兼容 `reasoning_content`
//! （DeepSeek / 多数网关）与 `reasoning`（OpenRouter）两种字段；
//! 工具调用按 `delta.tool_calls[]` 流式拼接。

use crate::sse::{self, SseEvent};
use serde::Serialize;
use serde_json::Value;
use std::io::BufReader;
use std::time::Duration;

/// 还没收到任何输出时，这些状态码按瞬态错误退避重试。
const RETRYABLE: [u16; 5] = [429, 500, 502, 503, 504];
/// 重试等待（毫秒）；长度即最大重试次数。
const BACKOFF_MS: [u64; 2] = [1_000, 3_000];

#[derive(Debug, Clone)]
pub struct ProviderConfig {
    /// 如 `http://127.0.0.1:8045/v1`（不含 `/chat/completions`）
    pub base_url: String,
    pub api_key: Option<String>,
    pub model: String,
}

/// 发给 provider 的一条消息。
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ChatMessage {
    pub role: String,
    /// 只有工具调用的 assistant 消息为 null
    pub content: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub tool_calls: Vec<ToolCall>,
    /// role = tool 时对应的调用 ID
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
}

impl ChatMessage {
    pub fn new(role: &str, content: impl Into<String>) -> Self {
        Self {
            role: role.to_string(),
            content: Some(content.into()),
            tool_calls: Vec::new(),
            tool_call_id: None,
        }
    }

    /// 带工具调用的 assistant 消息（`text` 为同一步里模型说的话，可为空）
    pub fn assistant_tool_calls(text: String, tool_calls: Vec<ToolCall>) -> Self {
        Self {
            role: "assistant".into(),
            content: (!text.is_empty()).then_some(text),
            tool_calls,
            tool_call_id: None,
        }
    }

    pub fn tool_result(call_id: String, content: String) -> Self {
        Self {
            role: "tool".into(),
            content: Some(content),
            tool_calls: Vec::new(),
            tool_call_id: Some(call_id),
        }
    }
}

/// 模型发起的一次函数调用（OpenAI `tool_calls[]`）。
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
    /// 即将重试（第 `attempt` 次，等待 `delay_ms`），`reason` 为上一次的错误
    Retrying {
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
}

impl Usage {
    pub fn add(&mut self, other: Usage) {
        self.input_tokens += other.input_tokens;
        self.output_tokens += other.output_tokens;
        self.reasoning_tokens += other.reasoning_tokens;
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct StreamOutcome {
    /// provider 的 `finish_reason`（stop / length / tool_calls …）
    pub finish_reason: Option<String>,
    pub usage: Option<Usage>,
    /// false = 被 `should_stop` 叫停
    pub completed: bool,
    /// 本步模型请求的工具调用（按 index 顺序）
    pub tool_calls: Vec<ToolCall>,
}

pub struct OpenAiProvider {
    config: ProviderConfig,
    agent: ureq::Agent,
}

impl OpenAiProvider {
    pub fn new(config: ProviderConfig) -> Self {
        let agent = ureq::Agent::config_builder()
            // 非 2xx 也要读响应体拿错误信息
            .http_status_as_error(false)
            .timeout_connect(Some(Duration::from_secs(10)))
            .build()
            .new_agent();
        Self { config, agent }
    }

    /// 配置里的默认模型
    pub fn model(&self) -> &str {
        &self.config.model
    }

    /// 端点提供的模型 ID（`GET /models`），按返回顺序。
    pub fn list_models(&self) -> anyhow::Result<Vec<String>> {
        let url = format!("{}/models", self.config.base_url.trim_end_matches('/'));
        let mut request = self.agent.get(&url).header("Accept", "application/json");
        if let Some(key) = &self.config.api_key {
            request = request.header("Authorization", format!("Bearer {key}"));
        }
        let response = request
            .call()
            .map_err(|e| anyhow::anyhow!("请求 {url} 失败: {e}"))?;
        let status = response.status().as_u16();
        let text = response.into_body().read_to_string().unwrap_or_default();
        if !(200..300).contains(&status) {
            anyhow::bail!("HTTP {status}: {}", error_message(&text));
        }
        parse_models(&text)
    }

    /// 发起一轮流式补全（`model` 为本次使用的模型 ID）。`tools` 为 OpenAI 工具定义（空 = 不带）；
    /// `should_stop` 每读一行检查一次；事件经 `on_event` 同步交付（调用线程即读流线程）。
    pub fn stream_chat(
        &self,
        model: &str,
        messages: &[ChatMessage],
        tools: &[Value],
        should_stop: &dyn Fn() -> bool,
        on_event: &mut dyn FnMut(StreamEvent),
    ) -> anyhow::Result<StreamOutcome> {
        let url = format!(
            "{}/chat/completions",
            self.config.base_url.trim_end_matches('/')
        );
        let mut body = serde_json::json!({
            "model": model,
            "stream": true,
            "stream_options": { "include_usage": true },
            "messages": messages,
        });
        if !tools.is_empty() {
            body["tools"] = Value::from(tools.to_vec());
        }
        let body = body.to_string();

        let mut attempt = 0;
        let response = loop {
            let mut request = self
                .agent
                .post(&url)
                .header("Content-Type", "application/json")
                .header("Accept", "text/event-stream");
            if let Some(key) = &self.config.api_key {
                request = request.header("Authorization", format!("Bearer {key}"));
            }
            let response = request
                .send(&body)
                .map_err(|e| anyhow::anyhow!("请求 {url} 失败: {e}"))?;
            let status = response.status().as_u16();
            if (200..300).contains(&status) {
                break response;
            }
            let text = response
                .into_body()
                .read_to_string()
                .unwrap_or_default();
            let reason = format!("HTTP {status}: {}", error_message(&text));
            if !RETRYABLE.contains(&status) || attempt >= BACKOFF_MS.len() || should_stop() {
                anyhow::bail!(reason);
            }
            let delay_ms = BACKOFF_MS[attempt];
            attempt += 1;
            on_event(StreamEvent::Retrying {
                attempt,
                delay_ms,
                reason,
            });
            std::thread::sleep(Duration::from_millis(delay_ms));
        };

        let reader = BufReader::new(response.into_body().into_reader());
        let mut outcome = StreamOutcome::default();
        let mut calls = ToolCallAccumulator::default();
        let mut stream_error = None;
        let completed = sse::read_events(reader, should_stop, |event| {
            if let SseEvent::Data(payload) = event {
                match parse_chunk(&payload) {
                    Ok(chunk) => {
                        for e in chunk.events {
                            on_event(e);
                        }
                        for d in chunk.tool_deltas {
                            calls.push(d);
                        }
                        if chunk.finish_reason.is_some() {
                            outcome.finish_reason = chunk.finish_reason;
                        }
                        if chunk.usage.is_some() {
                            outcome.usage = chunk.usage;
                        }
                    }
                    Err(message) => stream_error = Some(message),
                }
            }
            Ok(())
        })
        .map_err(|e| anyhow::anyhow!("读取流失败: {e}"))?;
        if let Some(message) = stream_error {
            anyhow::bail!(message);
        }
        outcome.completed = completed;
        outcome.tool_calls = calls.finish();
        Ok(outcome)
    }
}

/// `GET /models` 的响应 → 模型 ID 列表（OpenAI 格式 `{"data": [{"id": …}]}`）。
fn parse_models(text: &str) -> anyhow::Result<Vec<String>> {
    let value: Value = serde_json::from_str(text).map_err(|e| anyhow::anyhow!("模型列表不是 JSON: {e}"))?;
    let data = value
        .get("data")
        .and_then(Value::as_array)
        .ok_or_else(|| anyhow::anyhow!("模型列表缺少 data 字段"))?;
    Ok(data
        .iter()
        .filter_map(|m| m.get("id").and_then(Value::as_str))
        .map(str::to_owned)
        .collect())
}

/// 工具调用的一个流式片段（`delta.tool_calls[]`）。
#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct ToolCallDelta {
    pub index: Option<u64>,
    pub id: Option<String>,
    pub name: Option<String>,
    pub arguments: Option<String>,
}

/// 把片段拼成完整调用：按 `index` 归并；没有 `index` 的（部分网关一次给全）
/// 按 `id` 归并，都没有就接到最后一个上。
#[derive(Default)]
struct ToolCallAccumulator {
    calls: Vec<(u64, ToolCall)>,
}

impl ToolCallAccumulator {
    fn push(&mut self, d: ToolCallDelta) {
        let pos = match (d.index, &d.id) {
            (Some(i), _) => self.calls.iter().position(|(k, _)| *k == i),
            (None, Some(id)) => self.calls.iter().position(|(_, c)| &c.id == id),
            (None, None) => self.calls.len().checked_sub(1),
        };
        let pos = pos.unwrap_or_else(|| {
            let key = d.index.unwrap_or(self.calls.len() as u64);
            self.calls.push((
                key,
                ToolCall {
                    id: String::new(),
                    kind: "function".into(),
                    function: FunctionCall {
                        name: String::new(),
                        arguments: String::new(),
                    },
                },
            ));
            self.calls.len() - 1
        });
        let call = &mut self.calls[pos].1;
        if let Some(id) = d.id.filter(|id| !id.is_empty()) {
            call.id = id;
        }
        if let Some(name) = d.name {
            call.function.name.push_str(&name);
        }
        if let Some(args) = d.arguments {
            call.function.arguments.push_str(&args);
        }
    }

    fn finish(mut self) -> Vec<ToolCall> {
        self.calls.sort_by_key(|(k, _)| *k);
        self.calls
            .into_iter()
            .filter(|(_, c)| !c.function.name.is_empty())
            .map(|(_, mut c)| {
                if c.id.is_empty() {
                    c.id = next_call_id();
                }
                c
            })
            .collect()
    }
}

/// provider 没给 ID 时自己编一个（同一进程内唯一）
fn next_call_id() -> String {
    use std::sync::atomic::{AtomicU64, Ordering};
    static N: AtomicU64 = AtomicU64::new(1);
    format!("call_local_{}", N.fetch_add(1, Ordering::Relaxed))
}

/// 一个 SSE chunk 解析出的内容。
#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct Chunk {
    pub events: Vec<StreamEvent>,
    pub tool_deltas: Vec<ToolCallDelta>,
    pub finish_reason: Option<String>,
    pub usage: Option<Usage>,
}

/// 解析一个 `chat.completion.chunk`；流中途的 `{"error": …}` 作为错误返回。
pub(crate) fn parse_chunk(payload: &str) -> Result<Chunk, String> {
    let value: Value =
        serde_json::from_str(payload).map_err(|e| format!("无法解析流数据（{e}）: {payload}"))?;
    if let Some(err) = value.get("error") {
        return Err(format!("provider 错误: {}", error_message(&err.to_string())));
    }
    let mut chunk = Chunk::default();
    if let Some(choice) = value.pointer("/choices/0") {
        let delta = &choice["delta"];
        for key in ["reasoning_content", "reasoning"] {
            if let Some(t) = delta.get(key).and_then(Value::as_str).filter(|t| !t.is_empty()) {
                chunk.events.push(StreamEvent::Reasoning(t.to_string()));
                break;
            }
        }
        if let Some(t) = delta["content"].as_str().filter(|t| !t.is_empty()) {
            chunk.events.push(StreamEvent::Text(t.to_string()));
        }
        for tc in delta["tool_calls"].as_array().into_iter().flatten() {
            let s = |p: &str| tc.pointer(p).and_then(Value::as_str).map(str::to_owned);
            chunk.tool_deltas.push(ToolCallDelta {
                index: tc["index"].as_u64(),
                id: s("/id"),
                name: s("/function/name"),
                // 个别网关把 arguments 给成对象而不是 JSON 文本
                arguments: match tc.pointer("/function/arguments") {
                    Some(Value::String(a)) => Some(a.clone()),
                    Some(v @ Value::Object(_)) => Some(v.to_string()),
                    _ => None,
                },
            });
        }
        chunk.finish_reason = choice["finish_reason"].as_str().map(str::to_owned);
    }
    if let Some(u) = value.get("usage").filter(|u| u.is_object()) {
        let n = |p: &str| u.pointer(p).and_then(Value::as_u64).unwrap_or(0);
        chunk.usage = Some(Usage {
            input_tokens: n("/prompt_tokens"),
            output_tokens: n("/completion_tokens"),
            reasoning_tokens: n("/completion_tokens_details/reasoning_tokens"),
        });
    }
    Ok(chunk)
}

/// 从错误响应体里取人类可读的信息（`error.message` 或 `message`），否则原文截断。
fn error_message(body: &str) -> String {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_model_list() {
        let ids = parse_models(r#"{"object":"list","data":[{"id":"a"},{"id":"b","owned_by":"x"},{"x":1}]}"#).unwrap();
        assert_eq!(ids, ["a", "b"]);
        assert!(parse_models("{}").is_err());
    }

    #[test]
    fn parses_reasoning_text_finish_and_usage() {
        let c = parse_chunk(r#"{"choices":[{"index":0,"delta":{"role":"assistant","content":null,"reasoning_content":"想"},"finish_reason":null}]}"#).unwrap();
        assert_eq!(c.events, vec![StreamEvent::Reasoning("想".into())]);
        assert_eq!(c.finish_reason, None);

        let c = parse_chunk(r#"{"choices":[{"index":0,"delta":{"content":"你好"},"finish_reason":"stop"}]}"#).unwrap();
        assert_eq!(c.events, vec![StreamEvent::Text("你好".into())]);
        assert_eq!(c.finish_reason.as_deref(), Some("stop"));

        let c = parse_chunk(r#"{"choices":[],"usage":{"prompt_tokens":6,"completion_tokens":14,"total_tokens":428,"completion_tokens_details":{"reasoning_tokens":408}}}"#).unwrap();
        assert!(c.events.is_empty());
        assert_eq!(
            c.usage,
            Some(Usage { input_tokens: 6, output_tokens: 14, reasoning_tokens: 408 })
        );
    }

    #[test]
    fn openrouter_reasoning_field_and_stream_error() {
        let c = parse_chunk(r#"{"choices":[{"delta":{"reasoning":"r"}}]}"#).unwrap();
        assert_eq!(c.events, vec![StreamEvent::Reasoning("r".into())]);
        let err = parse_chunk(r#"{"error":{"message":"quota exhausted"}}"#).unwrap_err();
        assert!(err.contains("quota exhausted"), "{err}");
    }

    #[test]
    fn accumulates_streamed_tool_calls() {
        let mut acc = ToolCallAccumulator::default();
        for payload in [
            r#"{"choices":[{"delta":{"tool_calls":[{"index":0,"id":"call_a","type":"function","function":{"name":"read_file","arguments":""}}]}}]}"#,
            r#"{"choices":[{"delta":{"tool_calls":[{"index":0,"function":{"arguments":"{\"path\":"}}]}}]}"#,
            r#"{"choices":[{"delta":{"tool_calls":[{"index":1,"id":"call_b","function":{"name":"shell","arguments":{"command":"ls"}}}]}}]}"#,
            r#"{"choices":[{"delta":{"tool_calls":[{"index":0,"function":{"arguments":"\"a.rs\"}"}}]}}]}"#,
            r#"{"choices":[{"delta":{},"finish_reason":"tool_calls"}]}"#,
        ] {
            for d in parse_chunk(payload).unwrap().tool_deltas {
                acc.push(d);
            }
        }
        let calls = acc.finish();
        let got: Vec<_> = calls
            .iter()
            .map(|c| (c.id.as_str(), c.function.name.as_str(), c.function.arguments.as_str()))
            .collect();
        assert_eq!(
            got,
            [
                ("call_a", "read_file", r#"{"path":"a.rs"}"#),
                ("call_b", "shell", r#"{"command":"ls"}"#),
            ]
        );
    }

    #[test]
    fn tool_calls_without_index_or_id() {
        let mut acc = ToolCallAccumulator::default();
        let payload = r#"{"choices":[{"delta":{"tool_calls":[{"function":{"name":"list_dir","arguments":"{}"}}]}}]}"#;
        for d in parse_chunk(payload).unwrap().tool_deltas {
            acc.push(d);
        }
        let calls = acc.finish();
        assert_eq!(calls.len(), 1);
        assert!(calls[0].id.starts_with("call_local_"));
    }

    #[test]
    fn serializes_tool_messages() {
        let call = ToolCall {
            id: "c1".into(),
            kind: "function".into(),
            function: FunctionCall { name: "shell".into(), arguments: "{}".into() },
        };
        let m = serde_json::to_value(ChatMessage::assistant_tool_calls(String::new(), vec![call])).unwrap();
        assert_eq!(m["content"], Value::Null);
        assert_eq!(m["tool_calls"][0]["type"], "function");
        assert_eq!(m["tool_calls"][0]["function"]["name"], "shell");
        let m = serde_json::to_value(ChatMessage::tool_result("c1".into(), "ok".into())).unwrap();
        assert_eq!(m["role"], "tool");
        assert_eq!(m["tool_call_id"], "c1");
        let m = serde_json::to_value(ChatMessage::new("user", "hi")).unwrap();
        assert!(m.get("tool_calls").is_none() && m.get("tool_call_id").is_none());
    }

    #[test]
    fn error_message_prefers_structured() {
        assert_eq!(
            error_message(r#"{"error":{"message":"Resource has been exhausted"}}"#),
            "Resource has been exhausted"
        );
        assert_eq!(error_message("plain text"), "plain text");
    }
}
