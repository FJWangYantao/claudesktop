//! OpenAI Responses API（`POST /responses`）的流式 provider。
//!
//! 和 Chat Completions 的差别都在这里消化：
//! - 请求体：输入项是 `input` 数组（`message` / `function_call` / `function_call_output`），
//!   系统提示词走 `instructions`，工具形状是平的（`{"type":"function","name":…}`）
//! - 流式事件：`response.output_text.delta` / `response.reasoning_summary_text.delta` /
//!   `response.output_item.added`（函数调用的完整参数一次到齐）/
//!   `response.completed`（用量在这里）
//! - 思考力度：`reasoning: {"effort": …}`，从 `RequestParams::extra` 或 [`PROTOCOL_NAME`] 那套设置里取
//!
//! 工具调用在 Responses 里不是「delta 拼接」而是整项到达，所以不共用
//! [`crate::openai`] 的累加器。

use crate::gateway::Route;
use crate::provider::{
    ChatMessage, ChatProvider, ContentPart, FunctionCall, ModelInfo, RequestParams, StreamEvent,
    StreamOutcome, ToolCall, Usage, debug_http, error_message, finish_stream, get_json,
    parse_model_list, post_sse_with_retry, read_sse,
};
use serde_json::{Value, json};

/// 设置页里的协议标识（`ProviderProfile::protocol` 的字符串形式）。
pub const PROTOCOL_NAME: &str = "responses";

/// Responses API 的 provider。
pub struct ResponsesProvider {
    config: crate::provider::ProviderConfig,
    agent: ureq::Agent,
}

impl ResponsesProvider {
    pub fn new(config: crate::provider::ProviderConfig) -> Self {
        let agent = ureq::Agent::config_builder()
            .http_status_as_error(false)
            .timeout_connect(Some(std::time::Duration::from_secs(10)))
            .build()
            .new_agent();
        Self { config, agent }
    }

    fn auth_header(&self) -> Option<(&'static str, String)> {
        self.config.api_key.as_ref().map(|key| ("Authorization", format!("Bearer {key}")))
    }

    fn endpoint(&self, path: &str) -> Route {
        crate::gateway::route(self.config.gateway.as_ref(), &self.config.base_url, path)
    }
}

impl ChatProvider for ResponsesProvider {
    fn list_models(&self) -> anyhow::Result<Vec<ModelInfo>> {
        let headers = self.auth_header().into_iter().collect::<Vec<_>>();
        let text = get_json(&self.agent, &self.endpoint("/models"), &headers)?;
        let value: Value =
            serde_json::from_str(&text).map_err(|e| anyhow::anyhow!("模型列表不是 JSON: {e}"))?;
        let data = value
            .get("data")
            .and_then(Value::as_array)
            .ok_or_else(|| anyhow::anyhow!("模型列表缺少 data 字段"))?;
        Ok(parse_model_list(data))
    }

    fn stream_chat(
        &self,
        model: &str,
        messages: &[ChatMessage],
        tools: &[Value],
        should_stop: &dyn Fn() -> bool,
        on_event: &mut dyn FnMut(StreamEvent),
    ) -> anyhow::Result<StreamOutcome> {
        let route = self.endpoint("/responses");
        let body = request_body(model, messages, tools, &self.config.params).to_string();
        let headers = self.auth_header().into_iter().collect::<Vec<_>>();
        let reader =
            post_sse_with_retry(&self.agent, &route, &headers, &body, should_stop, on_event)?;

        let mut outcome = StreamOutcome::default();
        let mut calls: Vec<ToolCall> = Vec::new();
        // 正在拼参数的函数调用（按 item id）；Responses 一般一次给全，仍按 id 归并以防分片
        let mut pending: Vec<(String, ToolCall)> = Vec::new();
        let end = read_sse(reader, should_stop, &mut |payload| match parse_event(payload) {
            Ok(event) => apply_event(event, &mut outcome, &mut calls, &mut pending, on_event),
            Err(message) => {
                debug_http(|| format!("✗ {message}"));
                Err(message)
            }
        })?;
        outcome.completed = finish_stream(end, outcome.finish_reason.is_some())?;
        for (_, call) in pending {
            if !call.function.name.is_empty() {
                calls.push(call);
            }
        }
        outcome.tool_calls = calls;
        Ok(outcome)
    }
}

/// 请求体：统一消息形状 → Responses 的 `instructions` + `input`。
pub(crate) fn request_body(
    model: &str,
    messages: &[ChatMessage],
    tools: &[Value],
    params: &RequestParams,
) -> Value {
    // 系统消息合成 instructions（多条按空行拼）
    let mut instructions: Vec<String> = Vec::new();
    let mut input: Vec<Value> = Vec::new();
    for message in messages {
        match message.role.as_str() {
            "system" => {
                if let Some(text) = message.text().filter(|t| !t.trim().is_empty()) {
                    instructions.push(text);
                }
            }
            "tool" => {
                // 工具结果：`function_call_output`，文本平铺；图片按 input_image 追加成一条 user 消息
                let text = message.text().unwrap_or_default();
                input.push(json!({
                    "type": "function_call_output",
                    "call_id": message.tool_call_id.clone().unwrap_or_default(),
                    "output": text,
                }));
                let images: Vec<Value> = message
                    .parts
                    .iter()
                    .filter_map(|part| match part {
                        ContentPart::Image { media_type, data } => {
                            Some(image_item(media_type, data))
                        }
                        _ => None,
                    })
                    .collect();
                if !images.is_empty() {
                    input.push(json!({
                        "type": "message",
                        "role": "user",
                        "content": images,
                    }));
                }
            }
            "assistant" => {
                if let Some(text) = message.text().filter(|t| !t.is_empty()) {
                    input.push(json!({
                        "type": "message",
                        "role": "assistant",
                        "content": [{ "type": "output_text", "text": text }],
                    }));
                }
                for call in &message.tool_calls {
                    input.push(json!({
                        "type": "function_call",
                        "call_id": call.id,
                        "name": call.function.name,
                        "arguments": call.function.arguments,
                    }));
                }
            }
            _ => {
                // user（可能带图片）
                let mut content: Vec<Value> = Vec::new();
                for part in &message.parts {
                    match part {
                        ContentPart::Text(text) if !text.is_empty() => content.push(json!({
                            "type": "input_text",
                            "text": text,
                        })),
                        ContentPart::Image { media_type, data } => {
                            content.push(image_item(media_type, data));
                        }
                        _ => {}
                    }
                }
                if !content.is_empty() {
                    input.push(json!({
                        "type": "message",
                        "role": message.role,
                        "content": content,
                    }));
                }
            }
        }
    }
    let mut body = json!({ "model": model, "input": input, "stream": true });
    if !instructions.is_empty() {
        body["instructions"] = Value::String(instructions.join("\n\n"));
    }
    if !tools.is_empty() {
        body["tools"] = Value::Array(tools.iter().map(flatten_tool).collect());
    }
    if let Some(t) = params.temperature {
        body["temperature"] = json!(t);
    }
    if let Some(n) = params.max_tokens {
        // Responses 叫 max_output_tokens
        body["max_output_tokens"] = json!(n);
    }
    if let Some(extra) = params.extra.as_object() {
        for (key, value) in extra {
            body[key] = value.clone();
        }
    }
    body
}

/// Chat Completions 形状的工具定义（`{"type":"function","function":{…}}`）→ Responses 的平形状。
fn flatten_tool(tool: &Value) -> Value {
    let Some(function) = tool.get("function") else {
        return tool.clone();
    };
    let mut out = json!({ "type": "function" });
    for key in ["name", "description", "parameters", "strict"] {
        if let Some(value) = function.get(key) {
            out[key] = value.clone();
        }
    }
    out
}

fn image_item(media_type: &str, data: &str) -> Value {
    json!({
        "type": "input_image",
        "image_url": format!("data:{media_type};base64,{data}"),
    })
}

/// 一个流式事件（只解析我们关心的几种）。
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Event {
    Text(String),
    Reasoning(String),
    /// 一个函数调用的参数片段（`event_id` 归并）
    CallDelta {
        id: String,
        call_id: Option<String>,
        name: Option<String>,
        arguments: Option<String>,
    },
    /// 函数调用的项收尾了（之后不再有它的片段）
    CallDone(String),
    Completed {
        usage: Option<Usage>,
        stop: Option<String>,
    },
    Failed(String),
    /// 其它事件（`response.created` 等）忽略
    Other,
}

pub(crate) fn parse_event(payload: &str) -> Result<Event, String> {
    let value: Value =
        serde_json::from_str(payload).map_err(|e| format!("无法解析流数据（{e}）: {payload}"))?;
    let kind = value.get("type").and_then(Value::as_str).unwrap_or_default();
    let s = |key: &str| value.get(key).and_then(Value::as_str).map(str::to_owned);
    Ok(match kind {
        "response.output_text.delta" => Event::Text(s("delta").unwrap_or_default()),
        "response.reasoning_summary_text.delta" | "response.reasoning_text.delta" => {
            Event::Reasoning(s("delta").unwrap_or_default())
        }
        "response.output_item.added" => {
            let item = &value["item"];
            if item["type"].as_str() != Some("function_call") {
                return Ok(Event::Other);
            }
            Event::CallDelta {
                id: item["id"].as_str().or(value["item_id"].as_str()).unwrap_or_default().into(),
                call_id: item["call_id"].as_str().map(str::to_owned),
                name: item["name"].as_str().map(str::to_owned),
                arguments: item["arguments"].as_str().filter(|a| !a.is_empty()).map(str::to_owned),
            }
        }
        "response.function_call_arguments.delta" => Event::CallDelta {
            id: value["item_id"].as_str().unwrap_or_default().into(),
            call_id: None,
            name: None,
            arguments: s("delta"),
        },
        "response.output_item.done" => {
            Event::CallDone(value["item"]["id"].as_str().unwrap_or_default().into())
        }
        "response.completed" => {
            let response = &value["response"];
            Event::Completed {
                usage: response.get("usage").and_then(parse_usage),
                stop: response["status"].as_str().map(str::to_owned),
            }
        }
        // 没写完就停了（`max_output_tokens`、内容过滤）：也是收尾，按截断报
        "response.incomplete" => Event::Completed {
            usage: value["response"].get("usage").and_then(parse_usage),
            stop: Some("incomplete".into()),
        },
        "response.failed" => {
            let message = value
                .pointer("/response/error/message")
                .and_then(Value::as_str)
                .map_or_else(|| error_message(&value.to_string()), str::to_owned);
            Event::Failed(message)
        }
        "error" => Event::Failed(error_message(&value.to_string())),
        _ => Event::Other,
    })
}

/// 流里的错误（`response.failed` / `error`）返回 `Err`，读流就此中断，原文报给用户。
fn apply_event(
    event: Event,
    outcome: &mut StreamOutcome,
    calls: &mut Vec<ToolCall>,
    pending: &mut Vec<(String, ToolCall)>,
    on_event: &mut dyn FnMut(StreamEvent),
) -> Result<(), String> {
    match event {
        Event::Text(text) => {
            if !text.is_empty() {
                on_event(StreamEvent::Text(text));
            }
        }
        Event::Reasoning(text) => {
            if !text.is_empty() {
                on_event(StreamEvent::Reasoning(text));
            }
        }
        Event::CallDelta { id, call_id, name, arguments } => {
            let pos = pending.iter().position(|(key, _)| {
                key == &id || (call_id.is_some() && call_id.as_deref() == Some(key.as_str()))
            });
            let index = pos.unwrap_or_else(|| {
                pending.push((
                    id.clone(),
                    ToolCall {
                        id: String::new(),
                        kind: "function".into(),
                        function: FunctionCall { name: String::new(), arguments: String::new() },
                    },
                ));
                pending.len() - 1
            });
            let call = &mut pending[index].1;
            if let Some(call_id) = call_id {
                call.id = call_id;
            }
            if let Some(name) = name {
                call.function.name.push_str(&name);
            }
            if let Some(args) = arguments {
                call.function.arguments.push_str(&args);
            }
        }
        Event::CallDone(id) => {
            if let Some(pos) = pending.iter().position(|(key, _)| key == &id) {
                let (_, mut call) = pending.remove(pos);
                if call.id.is_empty() {
                    call.id = format!("call_{}", calls.len() + 1);
                }
                if !call.function.name.is_empty() {
                    calls.push(call);
                }
            }
        }
        Event::Completed { usage, stop } => {
            outcome.usage = usage;
            // 结束原因用 OpenAI 词汇（内核按它判断这一轮怎么结束）
            outcome.finish_reason = Some(match stop.as_deref() {
                Some("incomplete") => "length".into(),
                _ if !pending.is_empty() || !calls.is_empty() => "tool_calls".into(),
                _ => "stop".into(),
            });
        }
        Event::Failed(message) => {
            debug_http(|| format!("✗ {message}"));
            return Err(message);
        }
        Event::Other => {}
    }
    Ok(())
}

fn parse_usage(usage: &Value) -> Option<Usage> {
    let n = |path: &str| usage.pointer(path).and_then(Value::as_u64).unwrap_or(0);
    let input = n("/input_tokens");
    let output = n("/output_tokens");
    if input == 0 && output == 0 {
        return None;
    }
    Some(Usage {
        input_tokens: input,
        output_tokens: output,
        reasoning_tokens: n("/output_tokens_details/reasoning_tokens"),
        cache_read_tokens: n("/input_tokens_details/cached_tokens"),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn call() -> ToolCall {
        ToolCall {
            id: "call_1".into(),
            kind: "function".into(),
            function: FunctionCall {
                name: "read_file".into(),
                arguments: r#"{"path":"a"}"#.into(),
            },
        }
    }

    #[test]
    fn body_uses_instructions_and_flat_tools() {
        let messages = vec![
            ChatMessage::new("system", "你是助手"),
            ChatMessage::new("user", "读一下"),
            ChatMessage::assistant_tool_calls(String::new(), vec![call()]),
            ChatMessage::tool_result("call_1".into(), "内容".into()),
        ];
        let tools = vec![json!({
            "type": "function",
            "function": { "name": "read_file", "description": "读", "parameters": { "type": "object" } }
        })];
        let body = request_body(
            "gpt-5",
            &messages,
            &tools,
            &RequestParams { max_tokens: Some(1000), ..Default::default() },
        );
        assert_eq!(body["model"], "gpt-5");
        assert_eq!(body["instructions"], "你是助手");
        assert_eq!(body["max_output_tokens"], 1000);
        assert_eq!(body["tools"][0]["name"], "read_file");
        assert!(body["tools"][0].get("function").is_none());
        let input = body["input"].as_array().unwrap();
        assert_eq!(input[0]["type"], "message");
        assert_eq!(input[0]["content"][0]["type"], "input_text");
        assert_eq!(input[1]["type"], "function_call");
        assert_eq!(input[1]["call_id"], "call_1");
        assert_eq!(input[2]["type"], "function_call_output");
        assert_eq!(input[2]["output"], "内容");
    }

    #[test]
    fn images_become_input_image() {
        let messages = vec![ChatMessage::with_parts(
            "user",
            vec![
                ContentPart::Text("看图".into()),
                ContentPart::Image { media_type: "image/png".into(), data: "QUJD".into() },
            ],
        )];
        let body = request_body("m", &messages, &[], &RequestParams::default());
        let content = &body["input"][0]["content"];
        assert_eq!(content[0]["type"], "input_text");
        assert_eq!(content[1]["type"], "input_image");
        assert_eq!(content[1]["image_url"], "data:image/png;base64,QUJD");
    }

    #[test]
    fn parses_text_reasoning_and_completion() {
        assert_eq!(
            parse_event(r#"{"type":"response.output_text.delta","delta":"你好"}"#).unwrap(),
            Event::Text("你好".into())
        );
        assert_eq!(
            parse_event(r#"{"type":"response.reasoning_summary_text.delta","delta":"想"}"#)
                .unwrap(),
            Event::Reasoning("想".into())
        );
        let Event::Completed { usage, stop } =
            parse_event(
                r#"{"type":"response.completed","response":{"status":"completed","usage":{"input_tokens":10,"output_tokens":4,"output_tokens_details":{"reasoning_tokens":2},"input_tokens_details":{"cached_tokens":6}}}}"#,
            )
            .unwrap()
        else {
            panic!("completed");
        };
        assert_eq!(stop.as_deref(), Some("completed"));
        let usage = usage.unwrap();
        assert_eq!((usage.input_tokens, usage.output_tokens), (10, 4));
        assert_eq!((usage.reasoning_tokens, usage.cache_read_tokens), (2, 6));
        assert!(matches!(
            parse_event(r#"{"type":"error","message":"bad"}"#).unwrap(),
            Event::Failed(_)
        ));
        assert_eq!(parse_event(r#"{"type":"response.created"}"#).unwrap(), Event::Other);
    }

    #[test]
    fn collects_function_calls_from_items() {
        let mut outcome = StreamOutcome::default();
        let mut calls = Vec::new();
        let mut pending = Vec::new();
        let mut spoken = Vec::new();
        let mut on_event = |e: StreamEvent| spoken.push(e);
        for payload in [
            r#"{"type":"response.output_item.added","item":{"type":"function_call","id":"fc_1","call_id":"call_1","name":"read_file","arguments":""}}"#,
            r#"{"type":"response.function_call_arguments.delta","item_id":"fc_1","delta":"{\"path\":\"a\"}"}"#,
            r#"{"type":"response.output_item.done","item":{"type":"function_call","id":"fc_1"}}"#,
            r#"{"type":"response.completed","response":{"status":"completed","usage":{"input_tokens":1,"output_tokens":2}}}"#,
        ] {
            apply_event(
                parse_event(payload).unwrap(),
                &mut outcome,
                &mut calls,
                &mut pending,
                &mut on_event,
            )
            .unwrap();
        }
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].id, "call_1");
        assert_eq!(calls[0].function.name, "read_file");
        assert_eq!(calls[0].function.arguments, r#"{"path":"a"}"#);
        assert_eq!(outcome.finish_reason.as_deref(), Some("tool_calls"));
        assert_eq!(outcome.usage.unwrap().input_tokens, 1);
    }

    /// `response.failed` 的原文报出来；`response.incomplete` 按截断（`length`）收尾
    #[test]
    fn failures_surface_and_incomplete_means_length() {
        let mut outcome = StreamOutcome::default();
        let (mut calls, mut pending) = (Vec::new(), Vec::new());
        let mut on_event = |_: StreamEvent| {};
        let failed = parse_event(
            r#"{"type":"response.failed","response":{"error":{"message":"context too long"}}}"#,
        )
        .unwrap();
        assert_eq!(
            apply_event(failed, &mut outcome, &mut calls, &mut pending, &mut on_event),
            Err("context too long".into())
        );
        let incomplete = parse_event(
            r#"{"type":"response.incomplete","response":{"status":"incomplete","incomplete_details":{"reason":"max_output_tokens"}}}"#,
        )
        .unwrap();
        apply_event(incomplete, &mut outcome, &mut calls, &mut pending, &mut on_event).unwrap();
        assert_eq!(outcome.finish_reason.as_deref(), Some("length"));
    }
}
