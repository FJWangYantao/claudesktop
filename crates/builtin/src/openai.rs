//! OpenAI 兼容的 Chat Completions 流式 provider。
//!
//! 一个实现覆盖 OpenAI / OpenRouter / Ollama / vLLM / LM Studio，以及
//! Antigravity Tools 这类本地网关。思考内容兼容 `reasoning_content`
//! （DeepSeek / 多数网关）与 `reasoning`（OpenRouter）两种字段。

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
    pub content: String,
}

impl ChatMessage {
    pub fn new(role: &str, content: impl Into<String>) -> Self {
        Self {
            role: role.to_string(),
            content: content.into(),
        }
    }
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

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct StreamOutcome {
    /// provider 的 `finish_reason`（stop / length / tool_calls …）
    pub finish_reason: Option<String>,
    pub usage: Option<Usage>,
    /// false = 被 `should_stop` 叫停
    pub completed: bool,
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

    pub fn model(&self) -> &str {
        &self.config.model
    }

    /// 发起一轮流式补全。`should_stop` 每读一行检查一次；
    /// 事件经 `on_event` 同步交付（调用线程即读流线程）。
    pub fn stream_chat(
        &self,
        messages: &[ChatMessage],
        should_stop: &dyn Fn() -> bool,
        on_event: &mut dyn FnMut(StreamEvent),
    ) -> anyhow::Result<StreamOutcome> {
        let url = format!(
            "{}/chat/completions",
            self.config.base_url.trim_end_matches('/')
        );
        let body = serde_json::json!({
            "model": self.config.model,
            "stream": true,
            "stream_options": { "include_usage": true },
            "messages": messages,
        })
        .to_string();

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
        let mut stream_error = None;
        let completed = sse::read_events(reader, should_stop, |event| {
            if let SseEvent::Data(payload) = event {
                match parse_chunk(&payload) {
                    Ok(chunk) => {
                        for e in chunk.events {
                            on_event(e);
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
        Ok(outcome)
    }
}

/// 一个 SSE chunk 解析出的内容。
#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct Chunk {
    pub events: Vec<StreamEvent>,
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
    fn error_message_prefers_structured() {
        assert_eq!(
            error_message(r#"{"error":{"message":"Resource has been exhausted"}}"#),
            "Resource has been exhausted"
        );
        assert_eq!(error_message("plain text"), "plain text");
    }
}
