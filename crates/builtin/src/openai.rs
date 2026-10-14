//! OpenAI 兼容的 Chat Completions 流式 provider。
//!
//! 一个实现覆盖 OpenAI / OpenRouter / Ollama / vLLM / LM Studio，以及
//! Antigravity Tools 这类本地网关。思考内容兼容 `reasoning_content`
//! （DeepSeek / 多数网关）与 `reasoning`（OpenRouter）两种字段；
//! 工具调用按 `delta.tool_calls[]` 流式拼接。

use crate::provider::{
    ChatMessage, ChatProvider, RequestParams, StreamEvent, StreamOutcome, ToolCall, Usage,
    debug_http, error_message, post_sse_with_retry, read_sse,
};
use serde_json::Value;

/// OpenAI 兼容端点的 provider（协议细节见模块文档）。
pub struct OpenAiProvider {
    config: crate::provider::ProviderConfig,
    agent: ureq::Agent,
}

impl OpenAiProvider {
    pub fn new(config: crate::provider::ProviderConfig) -> Self {
        let agent = ureq::Agent::config_builder()
            // 非 2xx 也要读响应体拿错误信息
            .http_status_as_error(false)
            .timeout_connect(Some(std::time::Duration::from_secs(10)))
            .build()
            .new_agent();
        Self { config, agent }
    }

    fn auth_header(&self) -> Option<(&'static str, String)> {
        self.config
            .api_key
            .as_ref()
            .map(|key| ("Authorization", format!("Bearer {key}")))
    }

    fn endpoint(&self, path: &str) -> String {
        format!("{}{path}", self.config.base_url.trim_end_matches('/'))
    }
}

impl ChatProvider for OpenAiProvider {
    fn list_models(&self) -> anyhow::Result<Vec<String>> {
        let url = self.endpoint("/models");
        let mut request = self.agent.get(&url).header("Accept", "application/json");
        if let Some((name, value)) = self.auth_header() {
            request = request.header(name, &value);
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

    fn stream_chat(
        &self,
        model: &str,
        messages: &[ChatMessage],
        tools: &[Value],
        should_stop: &dyn Fn() -> bool,
        on_event: &mut dyn FnMut(StreamEvent),
    ) -> anyhow::Result<StreamOutcome> {
        let url = self.endpoint("/chat/completions");
        let mut body = serde_json::json!({
            "model": model,
            "stream": true,
            "stream_options": { "include_usage": true },
            "messages": messages,
        });
        if !tools.is_empty() {
            body["tools"] = Value::from(tools.to_vec());
        }
        apply_params(&mut body, &self.config.params);
        let body = body.to_string();

        let headers = self.auth_header().into_iter().collect::<Vec<_>>();
        let reader = post_sse_with_retry(&self.agent, &url, &headers, &body, should_stop, on_event)?;

        let mut outcome = StreamOutcome::default();
        let mut calls = ToolCallAccumulator::default();
        let completed = read_sse(reader, should_stop, &mut |payload| {
            match parse_chunk(payload) {
                Ok(chunk) => {
                    for event in chunk.events {
                        on_event(event);
                    }
                    for delta in chunk.tool_deltas {
                        calls.push(delta);
                    }
                    if chunk.finish_reason.is_some() {
                        outcome.finish_reason = chunk.finish_reason;
                    }
                    if chunk.usage.is_some() {
                        outcome.usage = chunk.usage;
                    }
                    Ok(())
                }
                Err(message) => {
                    debug_http(|| format!("✗ {message}"));
                    Err(message)
                }
            }
        })?;
        outcome.completed = completed;
        outcome.tool_calls = calls.finish();
        Ok(outcome)
    }
}

/// 把设置里透传的参数合并进请求体（`extra` 最后合入，可覆盖默认键）。
fn apply_params(body: &mut Value, params: &RequestParams) {
    if let Some(t) = params.temperature {
        body["temperature"] = serde_json::json!(t);
    }
    if let Some(n) = params.max_tokens {
        body["max_tokens"] = serde_json::json!(n);
    }
    if let Some(extra) = params.extra.as_object() {
        for (key, value) in extra {
            body[key] = value.clone();
        }
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
                    function: crate::provider::FunctionCall {
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::provider::StreamEvent;

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
    fn params_are_merged_into_body() {
        let mut body = serde_json::json!({"model": "m", "messages": []});
        apply_params(
            &mut body,
            &RequestParams {
                temperature: Some(0.2),
                max_tokens: Some(4096),
                extra: serde_json::json!({"top_k": 5, "model": "override"}),
            },
        );
        assert_eq!(body["temperature"], 0.2);
        assert_eq!(body["max_tokens"], 4096);
        assert_eq!(body["top_k"], 5);
        // extra 显式覆盖默认键
        assert_eq!(body["model"], "override");
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
