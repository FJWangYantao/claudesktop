//! Anthropic 原生 Messages API（`/v1/messages`）流式 provider。
//!
//! 相比走 OpenAI 兼容层，原生协议补齐了：思考块（`thinking_delta`）、
//! 提示缓存（`cache_control`，长会话省费）、图片输入。请求与解析都在本模块
//! 内完成；对上层暴露统一的 [`ChatProvider`] 接口。

use crate::provider::{
    ChatMessage, ChatProvider, ModelInfo, ProviderConfig, StreamEvent, StreamOutcome, ToolCall,
    debug_http, error_message, parse_model_list, post_sse_with_retry, read_sse,
};
use serde_json::{Value, json};

/// Anthropic 要求必填的 max_tokens 默认值（设置里可覆盖）
const MAX_TOKENS_DEFAULT: u64 = 16_384;

pub struct AnthropicProvider {
    config: ProviderConfig,
    agent: ureq::Agent,
}

impl AnthropicProvider {
    pub fn new(config: ProviderConfig) -> Self {
        let agent = ureq::Agent::config_builder()
            .http_status_as_error(false)
            .timeout_connect(Some(std::time::Duration::from_secs(10)))
            .build()
            .new_agent();
        Self { config, agent }
    }

    fn headers(&self) -> Vec<(&'static str, String)> {
        let mut headers = vec![(
            "anthropic-version",
            "2023-06-01".to_string(),
        )];
        if let Some(key) = &self.config.api_key {
            headers.push(("x-api-key", key.clone()));
        }
        headers
    }

    fn endpoint(&self, path: &str) -> String {
        format!("{}{path}", self.config.base_url.trim_end_matches('/'))
    }
}

impl ChatProvider for AnthropicProvider {
    fn list_models(&self) -> anyhow::Result<Vec<ModelInfo>> {
        let url = self.endpoint("/models?limit=1000");
        let mut request = self.agent.get(&url).header("Accept", "application/json");
        for (name, value) in self.headers() {
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
        let value: Value =
            serde_json::from_str(&text).map_err(|e| anyhow::anyhow!("模型列表不是 JSON: {e}"))?;
        Ok(parse_model_list(
            value.get("data").and_then(Value::as_array).map_or(&[], Vec::as_slice),
        ))
    }

    fn stream_chat(
        &self,
        model: &str,
        messages: &[ChatMessage],
        tools: &[Value],
        should_stop: &dyn Fn() -> bool,
        on_event: &mut dyn FnMut(StreamEvent),
    ) -> anyhow::Result<StreamOutcome> {
        let url = self.endpoint("/messages");
        let (system, converted) = convert_messages(messages);
        let mut body = json!({
            "model": model,
            "max_tokens": self.config.params.max_tokens.unwrap_or(MAX_TOKENS_DEFAULT),
            "stream": true,
            "messages": converted,
        });
        if !system.is_empty() {
            // 提示缓存：系统提示词标记为可缓存（命中后按缓存价计费）
            body["system"] = json!([{
                "type": "text",
                "text": system,
                "cache_control": { "type": "ephemeral" },
            }]);
        }
        if !tools.is_empty() {
            body["tools"] = convert_tools(tools);
        }
        if let Some(t) = self.config.params.temperature {
            body["temperature"] = json!(t);
        }
        if let Some(extra) = self.config.params.extra.as_object() {
            for (key, value) in extra {
                body[key] = value.clone();
            }
        }
        let body = body.to_string();

        let headers = self.headers();
        let reader = post_sse_with_retry(&self.agent, &url, &headers, &body, should_stop, on_event)?;

        let mut outcome = StreamOutcome::default();
        let mut usage = crate::provider::Usage::default();
        let mut usage_seen = false;
        let mut blocks = ToolBlocks::default();
        let completed = read_sse(reader, should_stop, &mut |payload| {
            match parse_event(payload, &mut blocks, &mut usage, &mut usage_seen, on_event) {
                Ok(Some(finish)) => {
                    outcome.finish_reason = Some(finish);
                    Ok(())
                }
                Ok(None) => Ok(()),
                Err(message) => {
                    debug_http(|| format!("✗ {message}"));
                    Err(message)
                }
            }
        })?;
        outcome.completed = completed;
        outcome.usage = usage_seen.then_some(usage);
        outcome.tool_calls = blocks.finish();
        Ok(outcome)
    }
}

/// 把统一消息形状翻译成 Anthropic messages：
/// - 开头的 system 消息抽出为顶层 system 文本
/// - 连续的 tool 结果合并进一条 user 消息（Anthropic 要求 tool_result 在 user 轮里）
/// - 图片段 → `image` block（base64 source）；工具调用 → `tool_use` block
/// - 首条必须是 user：历史以 assistant 开头时补一条占位
pub(crate) fn convert_messages(messages: &[ChatMessage]) -> (String, Vec<Value>) {
    let mut system_parts: Vec<String> = Vec::new();
    let mut out: Vec<Value> = Vec::new();
    // 待合并的 tool_result 块（遇到非 tool 消息或收尾时冲成一条 user 消息）
    let mut tool_results: Vec<Value> = Vec::new();
    let flush_results = |out: &mut Vec<Value>, tool_results: &mut Vec<Value>| {
        if !tool_results.is_empty() {
            out.push(json!({ "role": "user", "content": tool_results.clone() }));
            tool_results.clear();
        }
    };

    for message in messages {
        match message.role.as_str() {
            "system" => {
                if let Some(text) = message.text() {
                    system_parts.push(text);
                }
            }
            "tool" => tool_results.push(json!({
                "type": "tool_result",
                "tool_use_id": message.tool_call_id.clone().unwrap_or_default(),
                "content": message.text().unwrap_or_default(),
            })),
            "user" => {
                flush_results(&mut out, &mut tool_results);
                let blocks = content_blocks(message);
                if !blocks.is_empty() {
                    out.push(json!({ "role": "user", "content": blocks }));
                }
            }
            "assistant" => {
                flush_results(&mut out, &mut tool_results);
                let mut blocks = content_blocks(message);
                for call in &message.tool_calls {
                    blocks.push(json!({
                        "type": "tool_use",
                        "id": call.id,
                        "name": call.function.name,
                        "input": serde_json::from_str::<Value>(&call.function.arguments)
                            .unwrap_or_else(|_| json!({})),
                    }));
                }
                if !blocks.is_empty() {
                    out.push(json!({ "role": "assistant", "content": blocks }));
                }
            }
            _ => {}
        }
    }
    flush_results(&mut out, &mut tool_results);
    // 首条必须 user（续接场景历史可能以 assistant 开头）
    if out.first().and_then(|m| m.get("role")).and_then(Value::as_str) == Some("assistant") {
        out.insert(0, json!({ "role": "user", "content": "(conversation continues)" }));
    }
    (system_parts.join("\n\n"), out)
}

/// 一条消息的文本 / 图片分段 → Anthropic content blocks（空文本段跳过，
/// Anthropic 不接受空文本块）。
fn content_blocks(message: &ChatMessage) -> Vec<Value> {
    message
        .parts
        .iter()
        .filter(|part| match part {
            crate::provider::ContentPart::Text(t) => !t.is_empty(),
            _ => true,
        })
        .map(|part| match part {
            crate::provider::ContentPart::Text(text) => json!({ "type": "text", "text": text }),
            crate::provider::ContentPart::Image { media_type, data } => json!({
                "type": "image",
                "source": { "type": "base64", "media_type": media_type, "data": data },
            }),
        })
        .collect()
}

/// OpenAI 形状的工具定义 → Anthropic 形状；最后一个工具标缓存断点。
pub(crate) fn convert_tools(tools: &[Value]) -> Value {
    let n = tools.len();
    Value::Array(
        tools
            .iter()
            .enumerate()
            .map(|(i, tool)| {
                let function = &tool["function"];
                let mut converted = json!({
                    "name": function["name"],
                    "input_schema": function["parameters"],
                });
                if let Some(desc) = function.get("description").filter(|d| !d.is_null()) {
                    converted["description"] = desc.clone();
                }
                if i + 1 == n {
                    converted["cache_control"] = json!({ "type": "ephemeral" });
                }
                converted
            })
            .collect(),
    )
}

/// 按 content block 下标累积的 tool_use（id / name 在 block 开始时给出，
/// input JSON 由后续 `input_json_delta` 流式拼出）。
#[derive(Default)]
struct ToolBlocks {
    blocks: Vec<Option<ToolCall>>,
}

impl ToolBlocks {
    fn start(&mut self, index: usize, id: &str, name: &str) {
        if self.blocks.len() <= index {
            self.blocks.resize_with(index + 1, || None);
        }
        self.blocks[index] = Some(ToolCall {
            id: id.to_string(),
            kind: "function".into(),
            function: crate::provider::FunctionCall {
                name: name.to_string(),
                arguments: String::new(),
            },
        });
    }

    fn push_input(&mut self, index: usize, json: &str) {
        if let Some(Some(call)) = self.blocks.get_mut(index) {
            call.function.arguments.push_str(json);
        }
    }

    fn finish(self) -> Vec<ToolCall> {
        self.blocks
            .into_iter()
            .flatten()
            .filter(|c| !c.function.name.is_empty())
            .collect()
    }
}

/// Anthropic stop_reason → OpenAI 词汇（上层 `stop_reason()` 统一映射）。
fn finish_reason(stop: &str) -> &'static str {
    match stop {
        "tool_use" => "tool_calls",
        "max_tokens" => "length",
        _ => "stop",
    }
}

/// 解析一条 Messages API 流事件；返回 `Some(finish)` 表示拿到 stop_reason。
type Parsed = Result<Option<String>, String>;

fn parse_event(
    payload: &str,
    blocks: &mut ToolBlocks,
    usage: &mut crate::provider::Usage,
    usage_seen: &mut bool,
    on_event: &mut dyn FnMut(StreamEvent),
) -> Parsed {
    let value: Value = serde_json::from_str(payload)
        .map_err(|e| format!("无法解析流数据（{e}）: {payload}"))?;
    match value.get("type").and_then(Value::as_str).unwrap_or("") {
        "message_start" => {
            // 开了提示缓存时 input_tokens 只是没命中缓存的部分；三项加起来才是整个提示
            // （和 OpenAI 的 prompt_tokens 同口径，上下文用量据此计算）
            if let Some(u) = value.pointer("/message/usage").filter(|u| u.is_object()) {
                let n = |k: &str| u.get(k).and_then(Value::as_u64).unwrap_or(0);
                usage.input_tokens =
                    n("input_tokens") + n("cache_read_input_tokens") + n("cache_creation_input_tokens");
                usage.cache_read_tokens = n("cache_read_input_tokens");
                *usage_seen = true;
            }
            Ok(None)
        }
        "content_block_start" => {
            let index = value.get("index").and_then(Value::as_u64).unwrap_or(0) as usize;
            if value.pointer("/content_block/type").and_then(Value::as_str) == Some("tool_use") {
                let block = &value["content_block"];
                blocks.start(
                    index,
                    block.get("id").and_then(Value::as_str).unwrap_or_default(),
                    block.get("name").and_then(Value::as_str).unwrap_or_default(),
                );
            }
            Ok(None)
        }
        "content_block_delta" => {
            let index = value.get("index").and_then(Value::as_u64).unwrap_or(0) as usize;
            match value.pointer("/delta/type").and_then(Value::as_str).unwrap_or("") {
                "text_delta" => {
                    if let Some(text) = value.pointer("/delta/text").and_then(Value::as_str) {
                        on_event(StreamEvent::Text(text.to_string()));
                    }
                }
                "thinking_delta" => {
                    if let Some(text) = value.pointer("/delta/thinking").and_then(Value::as_str) {
                        on_event(StreamEvent::Reasoning(text.to_string()));
                    }
                }
                "input_json_delta" => {
                    if let Some(json) = value.pointer("/delta/partial_json").and_then(Value::as_str)
                    {
                        blocks.push_input(index, json);
                    }
                }
                _ => {}
            }
            Ok(None)
        }
        "message_delta" => {
            if let Some(output) = value.pointer("/usage/output_tokens").and_then(Value::as_u64) {
                usage.output_tokens = output;
                *usage_seen = true;
            }
            Ok(value
                .pointer("/delta/stop_reason")
                .and_then(Value::as_str)
                .map(|stop| finish_reason(stop).to_string()))
        }
        "error" => {
            let message = value
                .pointer("/error/message")
                .and_then(Value::as_str)
                .unwrap_or("provider 错误");
            Err(format!("provider 错误: {message}"))
        }
        // ping / message_stop / content_block_stop / message_start 之外的事件忽略
        _ => Ok(None),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::provider::{ChatMessage as M, ContentPart};

    #[test]
    fn extracts_system_and_converts_user_parts() {
        let messages = vec![
            M::new("system", "You are helpful."),
            M::new("user", "看图"),
        ];
        let (system, out) = convert_messages(&messages);
        assert_eq!(system, "You are helpful.");
        assert_eq!(out.len(), 1);
        assert_eq!(out[0]["role"], "user");
        assert_eq!(out[0]["content"][0]["type"], "text");

        let with_image = vec![M::with_parts(
            "user",
            vec![
                ContentPart::Text("图".into()),
                ContentPart::Image { media_type: "image/png".into(), data: "QUJD".into() },
            ],
        )];
        let (_, out) = convert_messages(&with_image);
        assert_eq!(out[0]["content"][1]["type"], "image");
        assert_eq!(out[0]["content"][1]["source"]["media_type"], "image/png");
        assert_eq!(out[0]["content"][1]["source"]["data"], "QUJD");
    }

    #[test]
    fn merges_consecutive_tool_results_into_one_user_turn() {
        let call = ToolCall {
            id: "t1".into(),
            kind: "function".into(),
            function: crate::provider::FunctionCall {
                name: "read_file".into(),
                arguments: r#"{"path":"a"}"#.into(),
            },
        };
        let messages = vec![
            M::new("user", "读两个文件"),
            M::assistant_tool_calls(String::new(), vec![call]),
            M::tool_result("t1".into(), "内容1".into()),
            M::tool_result("t2".into(), "内容2".into()),
            M::new("assistant", "好了"),
        ];
        let (_, out) = convert_messages(&messages);
        let roles: Vec<&str> = out
            .iter()
            .map(|m| m["role"].as_str().unwrap())
            .collect();
        assert_eq!(roles, ["user", "assistant", "user", "assistant"]);
        let results = &out[2]["content"];
        assert_eq!(results.as_array().unwrap().len(), 2);
        assert_eq!(results[0]["tool_use_id"], "t1");
        assert_eq!(results[0]["content"], "内容1");
        // 工具调用转成 tool_use，参数解析成对象（空文本不产生块）
        assert_eq!(out[1]["content"][0]["type"], "tool_use");
        assert_eq!(out[1]["content"][0]["input"]["path"], "a");
    }

    #[test]
    fn prepends_user_when_history_starts_with_assistant() {
        let messages = vec![M::new("assistant", "继续")];
        let (_, out) = convert_messages(&messages);
        assert_eq!(out[0]["role"], "user");
        assert_eq!(out[1]["role"], "assistant");
    }

    #[test]
    fn converts_tool_specs_with_cache_marker_on_last() {
        let tools = vec![
            json!({"type":"function","function":{"name":"read_file","description":"读","parameters":{"type":"object"}}}),
            json!({"type":"function","function":{"name":"shell","parameters":{"type":"object"}}}),
        ];
        let converted = convert_tools(&tools);
        let arr = converted.as_array().unwrap();
        assert_eq!(arr.len(), 2);
        assert_eq!(arr[0]["name"], "read_file");
        assert_eq!(arr[0]["description"], "读");
        assert_eq!(arr[0]["input_schema"]["type"], "object");
        assert!(arr[0].get("cache_control").is_none());
        assert_eq!(arr[1]["cache_control"]["type"], "ephemeral");
    }

    #[test]
    fn parses_stream_lifecycle() {
        let mut blocks = ToolBlocks::default();
        let mut usage = crate::provider::Usage::default();
        let mut seen = false;
        let mut events = Vec::new();
        let lines = [
            r#"{"type":"message_start","message":{"usage":{"input_tokens":5,"cache_read_input_tokens":12,"cache_creation_input_tokens":8}}}"#,
            r#"{"type":"content_block_start","index":0,"content_block":{"type":"thinking","thinking":""}}"#,
            r#"{"type":"content_block_delta","index":0,"delta":{"type":"thinking_delta","thinking":"想"}}"#,
            r#"{"type":"content_block_start","index":1,"content_block":{"type":"tool_use","id":"toolu_1","name":"read_file","input":{}}}"#,
            r#"{"type":"content_block_delta","index":1,"delta":{"type":"text_delta","text":"先看"}}"#,
            r#"{"type":"content_block_delta","index":1,"delta":{"type":"input_json_delta","partial_json":"{\"path\":"}}"#,
            r#"{"type":"content_block_delta","index":1,"delta":{"type":"input_json_delta","partial_json":"\"a\"}"}}"#,
            r#"{"type":"ping"}"#,
            r#"{"type":"message_delta","delta":{"stop_reason":"tool_use"},"usage":{"output_tokens":41}}"#,
            r#"{"type":"message_stop"}"#,
        ];
        let mut finish = None;
        for line in lines {
            let out =
                parse_event(line, &mut blocks, &mut usage, &mut seen, &mut |e| events.push(e))
                    .unwrap();
            if out.is_some() {
                finish = out;
            }
        }
        assert_eq!(finish.as_deref(), Some("tool_calls"));
        assert_eq!(usage.input_tokens, 25);
        assert_eq!(usage.cache_read_tokens, 12);
        assert_eq!(usage.output_tokens, 41);
        assert_eq!(
            events,
            vec![StreamEvent::Reasoning("想".into()), StreamEvent::Text("先看".into())]
        );
        let calls = blocks.finish();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].id, "toolu_1");
        assert_eq!(calls[0].function.name, "read_file");
        assert_eq!(calls[0].function.arguments, r#"{"path":"a"}"#);
    }

    #[test]
    fn maps_stop_reasons_and_stream_errors() {
        assert_eq!(finish_reason("end_turn"), "stop");
        assert_eq!(finish_reason("tool_use"), "tool_calls");
        assert_eq!(finish_reason("max_tokens"), "length");
        assert_eq!(finish_reason("stop_sequence"), "stop");

        let mut blocks = ToolBlocks::default();
        let mut usage = crate::provider::Usage::default();
        let mut seen = false;
        let err = parse_event(
            r#"{"type":"error","error":{"type":"overloaded_error","message":"Overloaded"}}"#,
            &mut blocks,
            &mut usage,
            &mut seen,
            &mut |_| {},
        )
        .unwrap_err();
        assert!(err.contains("Overloaded"), "{err}");
    }
}
