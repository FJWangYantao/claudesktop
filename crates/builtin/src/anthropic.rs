//! Anthropic 原生 Messages API（`/v1/messages`）流式 provider。
//!
//! 相比走 OpenAI 兼容层，原生协议补齐了：思考块（`thinking_delta`）、
//! 提示缓存（`cache_control`，长会话省费）、图片输入，以及两种只有原生协议认的
//! 请求能力——思考强度（`thinking` / `output_config.effort`，档位来自
//! [`crate::provider::thinking_spec`]）和 Anthropic 在服务端执行的工具（[`SERVER_TOOLS`]）。
//! 请求与解析都在本模块内完成；对上层暴露统一的 [`ChatProvider`] 接口。

use crate::provider::{
    ChatMessage, ChatProvider, ModelInfo, ProviderConfig, RequestParams, StreamEvent,
    StreamOutcome, ToolCall, debug_http, finish_stream, get_json, parse_model_list,
    post_sse_with_retry, read_sse,
};
use serde_json::{Value, json};

/// Anthropic 要求必填的 max_tokens 默认值（设置里可覆盖）
const MAX_TOKENS_DEFAULT: u64 = 16_384;

/// 一个服务端工具结果块转成文本时的字数上限（搜索结果 / 抓到的网页都很长，
/// 全塞进正文会把下一轮的上下文吃掉）
const MAX_BLOCK_TEXT: usize = 4_000;

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
        let mut headers = vec![("anthropic-version", "2023-06-01".to_string())];
        if let Some(key) = &self.config.api_key {
            headers.push(("x-api-key", key.clone()));
        }
        headers
    }

    fn endpoint(&self, path: &str) -> crate::gateway::Route {
        crate::gateway::route(self.config.gateway.as_ref(), &self.config.base_url, path)
    }
}

impl ChatProvider for AnthropicProvider {
    fn list_models(&self) -> anyhow::Result<Vec<ModelInfo>> {
        let text = get_json(&self.agent, &self.endpoint("/models?limit=1000"), &self.headers())?;
        let value: Value =
            serde_json::from_str(&text).map_err(|e| anyhow::anyhow!("模型列表不是 JSON: {e}"))?;
        Ok(parse_model_list(value.get("data").and_then(Value::as_array).map_or(&[], Vec::as_slice)))
    }

    fn stream_chat(
        &self,
        model: &str,
        messages: &[ChatMessage],
        tools: &[Value],
        should_stop: &dyn Fn() -> bool,
        on_event: &mut dyn FnMut(StreamEvent),
    ) -> anyhow::Result<StreamOutcome> {
        let route = self.endpoint("/messages");
        let body = build_body(model, messages, tools, &self.config.params).to_string();

        let headers = self.headers();
        let reader =
            post_sse_with_retry(&self.agent, &route, &headers, &body, should_stop, on_event)?;

        let mut outcome = StreamOutcome::default();
        let mut usage = crate::provider::Usage::default();
        let mut usage_seen = false;
        let mut blocks = ToolBlocks::default();
        let end = read_sse(reader, should_stop, &mut |payload| match parse_event(
            payload,
            &mut blocks,
            &mut usage,
            &mut usage_seen,
            on_event,
        ) {
            Ok(Some(finish)) => {
                outcome.finish_reason = Some(finish);
                Ok(())
            }
            Ok(None) => Ok(()),
            Err(message) => {
                debug_http(|| format!("✗ {message}"));
                Err(message)
            }
        })?;
        outcome.completed = finish_stream(end, outcome.finish_reason.is_some())?;
        outcome.usage = usage_seen.then_some(usage);
        outcome.thinking = blocks.take_thinking();
        outcome.tool_calls = blocks.finish();
        Ok(outcome)
    }
}

/// 一个 Anthropic 服务端工具：内部名字 + 请求体片段。
pub struct ServerTool {
    /// 内部名字（设置里存的就是它，也是返回块里 `name` 字段的值）
    pub name: &'static str,
    /// 设置页给人看的说明
    pub desc: &'static str,
    /// 请求体里 `tools` 数组的那一项
    pub spec: fn() -> Value,
}

/// Anthropic 在服务端执行的工具名单（`tools` 里多一项带版本号的 `type`，返回
/// `server_tool_use` / `*_tool_result` 块，不由我们执行）。
///
/// **版本号需按官方文档核对**——这几个日期后缀是工具版本，Anthropic 会发新版本
/// 并逐步废弃旧的（新模型上旧版本会被拒）。核实来源与日期：
/// - `web_search_20250305`：https://platform.claude.com/docs/en/agents-and-tools/tool-use/web-search-tool
///   （基础版；`web_search_20260209` 起加动态过滤，`20260318` 起加 `response_inclusion`）
/// - `web_fetch_20250910`：https://platform.claude.com/docs/en/agents-and-tools/tool-use/web-fetch-tool
///   （基础版；`20260209` / `20260309` / `20260318` 是加了动态过滤的后续版本）
/// - `code_execution_20250825`：https://platform.claude.com/docs/en/agents-and-tools/tool-use/code-execution-tool
///   （三个版本都仍受支持，此版在全部支持模型上行为一致；`20260120` 起加程序化工具调用/REPL 状态）
///
/// 这里统一选「基础版」：老模型也认，不需要 beta 头。
pub const SERVER_TOOLS: &[ServerTool] = &[
    ServerTool {
        name: "web_search",
        desc: "网络搜索（服务端执行，按次计费）",
        spec: || json!({ "type": "web_search_20250305", "name": "web_search", "max_uses": 5 }),
    },
    ServerTool {
        name: "web_fetch",
        desc: "抓取网页正文（服务端执行）",
        spec: || json!({ "type": "web_fetch_20250910", "name": "web_fetch", "max_uses": 10 }),
    },
    ServerTool {
        name: "code_execution",
        desc: "在 Anthropic 沙箱里跑代码（服务端执行）",
        spec: || json!({ "type": "code_execution_20250825", "name": "code_execution" }),
    },
];

/// 服务端工具的内部名字 → 请求体片段；不认识的名字返回 None（设置文件里手写错的忽略掉）。
fn server_tool_spec(name: &str) -> Option<Value> {
    SERVER_TOOLS.iter().find(|t| t.name == name).map(|t| (t.spec)())
}

/// 组装 Messages API 请求体（纯函数，测试直接比 JSON）。
///
/// 几处取舍：
/// - 思考强度与 `temperature` 不共存：Anthropic 要求开启 thinking 时 `temperature` 只能是
///   默认值 1；Opus 4.7 以后的新模型**不管开没开**都不认非默认的 `temperature`。两处判断都在
///   [`apply_thinking`] 里（能力表的 `accepts_temperature`）
/// - 服务端工具（[`SERVER_TOOLS`]）和客户端工具合进同一个 `tools` 数组；
///   缓存断点仍打在最后一个客户端工具上。
/// - `extra` 最后浅合并，同名键覆盖上面所有默认值（留一个总开关）。
fn build_body(
    model: &str,
    messages: &[ChatMessage],
    tools: &[Value],
    params: &RequestParams,
) -> Value {
    let (system, converted) = convert_messages(messages);
    let mut body = json!({
        "model": model,
        "max_tokens": params.max_tokens.unwrap_or(MAX_TOKENS_DEFAULT),
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
    let server: Vec<Value> =
        params.server_tools.iter().filter_map(|n| server_tool_spec(n)).collect();
    if !tools.is_empty() || !server.is_empty() {
        let mut all = convert_tools(tools).as_array().cloned().unwrap_or_default();
        all.extend(server);
        body["tools"] = Value::Array(all);
    }
    if let Some(t) = params.temperature {
        body["temperature"] = json!(t);
    }
    // 思考强度（档位 → `thinking` / `output_config`，开着时不发 temperature）；
    // 修 temperature 的那半也在这里
    crate::provider::apply_thinking(&mut body, crate::provider::Protocol::Anthropic, model, params);
    if let Some(extra) = params.extra.as_object() {
        for (key, value) in extra {
            body[key] = value.clone();
        }
    }
    body
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
            "tool" => {
                // tool_result 的 content 可以是纯文本，也可以是 text + image 块（MCP 多模态结果）：
                // 没有图片时保持纯字符串（和以前一样，也少一层嵌套）
                let images: Vec<Value> = message
                    .parts
                    .iter()
                    .filter_map(|part| match part {
                        crate::provider::ContentPart::Image { media_type, data } => Some(json!({
                            "type": "image",
                            "source": { "type": "base64", "media_type": media_type, "data": data },
                        })),
                        _ => None,
                    })
                    .collect();
                let content = if images.is_empty() {
                    json!(message.text().unwrap_or_default())
                } else {
                    let mut blocks = vec![json!({
                        "type": "text",
                        "text": message.text().unwrap_or_default(),
                    })];
                    blocks.extend(images);
                    json!(blocks)
                };
                tool_results.push(json!({
                    "type": "tool_result",
                    "tool_use_id": message.tool_call_id.clone().unwrap_or_default(),
                    "content": content,
                }));
            }
            "user" => {
                flush_results(&mut out, &mut tool_results);
                let blocks = content_blocks(message);
                if !blocks.is_empty() {
                    out.push(json!({ "role": "user", "content": blocks }));
                }
            }
            "assistant" => {
                flush_results(&mut out, &mut tool_results);
                // 思考块必须在最前（开了思考时，带 tool_use 的 assistant 轮缺了它会被拒）
                let mut blocks = message.thinking.clone();
                blocks.extend(content_blocks(message));
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
/// input JSON 由后续 `input_json_delta` 流式拼出），以及思考块（原样留着回传，
/// 正文由 `thinking_delta` 拼、`signature_delta` 给签名；`redacted_thinking` 开始时就是完整的）。
#[derive(Default)]
struct ToolBlocks {
    blocks: Vec<Option<ToolCall>>,
    thinking: Vec<(usize, Value)>,
}

impl ToolBlocks {
    fn start_thinking(&mut self, index: usize, block: &Value) {
        self.thinking.push((index, block.clone()));
    }

    fn thinking_at(&mut self, index: usize) -> Option<&mut Value> {
        self.thinking.iter_mut().find(|(i, _)| *i == index).map(|(_, b)| b)
    }

    /// 追加到某个思考块的字符串字段（`thinking` 正文 / `signature`）
    fn push_thinking(&mut self, index: usize, field: &str, text: &str) {
        if let Some(block) = self.thinking_at(index) {
            let mut value =
                block.get(field).and_then(Value::as_str).unwrap_or_default().to_string();
            value.push_str(text);
            block[field] = Value::String(value);
        }
    }

    /// 本步的思考块（按出现顺序）
    fn take_thinking(&mut self) -> Vec<Value> {
        let mut list = std::mem::take(&mut self.thinking);
        list.sort_by_key(|(i, _)| *i);
        list.into_iter().map(|(_, b)| b).collect()
    }

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
        self.blocks.into_iter().flatten().filter(|c| !c.function.name.is_empty()).collect()
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
///
/// 服务端工具（`server_tool_use` / `web_search_tool_result` / `web_fetch_tool_result` /
/// `*_code_execution_tool_result`）的块由 Anthropic 自己执行，**不是**要我们跑的工具调用，
/// 所以**不进** [`ToolBlocks`]（那里出来的是 `outcome.tool_calls`，会进工具循环）。
/// 结果转成一段给人看的文本，当作正文经 `StreamEvent::Text` 送出：内核会把它拼进本步的
/// assistant 文本、写进日志，下一轮自然带着历史发回去。
/// `server_tool_use` 本身（只要执行什么）不送，参数我们也不需要（结果块里带 `tool_use_id`）。
type Parsed = Result<Option<String>, String>;

fn parse_event(
    payload: &str,
    blocks: &mut ToolBlocks,
    usage: &mut crate::provider::Usage,
    usage_seen: &mut bool,
    on_event: &mut dyn FnMut(StreamEvent),
) -> Parsed {
    let value: Value =
        serde_json::from_str(payload).map_err(|e| format!("无法解析流数据（{e}）: {payload}"))?;
    match value.get("type").and_then(Value::as_str).unwrap_or("") {
        "message_start" => {
            // 开了提示缓存时 input_tokens 只是没命中缓存的部分；三项加起来才是整个提示
            // （和 OpenAI 的 prompt_tokens 同口径，上下文用量据此计算）
            if let Some(u) = value.pointer("/message/usage").filter(|u| u.is_object()) {
                let n = |k: &str| u.get(k).and_then(Value::as_u64).unwrap_or(0);
                usage.input_tokens = n("input_tokens")
                    + n("cache_read_input_tokens")
                    + n("cache_creation_input_tokens");
                usage.cache_read_tokens = n("cache_read_input_tokens");
                *usage_seen = true;
            }
            Ok(None)
        }
        "content_block_start" => {
            let index = value.get("index").and_then(Value::as_u64).unwrap_or(0) as usize;
            let block = &value["content_block"];
            let kind = block.get("type").and_then(Value::as_str).unwrap_or("");
            if kind == "thinking" || kind == "redacted_thinking" {
                blocks.start_thinking(index, block);
            } else if kind == "tool_use" {
                blocks.start(
                    index,
                    block.get("id").and_then(Value::as_str).unwrap_or_default(),
                    block.get("name").and_then(Value::as_str).unwrap_or_default(),
                );
            } else if let Some(text) = server_result_text(kind, block) {
                // 前后留空行：服务端工具的结果接在模型正文里也要读得清
                on_event(StreamEvent::Text(format!("\n{text}\n")));
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
                        blocks.push_thinking(index, "thinking", text);
                        on_event(StreamEvent::Reasoning(text.to_string()));
                    }
                }
                "signature_delta" => {
                    if let Some(sig) = value.pointer("/delta/signature").and_then(Value::as_str) {
                        blocks.push_thinking(index, "signature", sig);
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
            let message =
                value.pointer("/error/message").and_then(Value::as_str).unwrap_or("provider 错误");
            Err(format!("provider 错误: {message}"))
        }
        // ping / message_stop / content_block_stop / message_start 之外的事件忽略
        _ => Ok(None),
    }
}

/// 服务端工具的结果块 → 给模型看的文本；不是结果块返回 None。
///
/// 覆盖官方文档里的几种形状（`content` 有的是列表、有的是对象、有的只是字符串）：
/// - `web_search_tool_result`：`[{type: web_search_result, title, url, page_age}]`，
///   或出错时的 `{type: web_search_tool_result_error, error_code}`
/// - `web_fetch_tool_result`：`{type: web_fetch_result, url, content: {source: {data}}}`
/// - `bash_code_execution_tool_result` / `text_editor_code_execution_tool_result`：
///   `{type: bash_code_execution_result, stdout, stderr, return_code}` 等
///
/// 认不出来但块名是 `*_tool_result` 的，把整个 `content` 折成一行 JSON，
/// 至少别把结果丢了。长度卡在 [`MAX_BLOCK_TEXT`]。
fn server_result_text(kind: &str, block: &Value) -> Option<String> {
    let content = block.get("content")?;
    let body = if kind == "web_search_tool_result" {
        search_results_text(content)
    } else if kind == "web_fetch_tool_result" {
        // content.content.source.data 是正文（也可能是 base64 的 PDF，那就给一句提示）
        content
            .pointer("/content/source/data")
            .and_then(Value::as_str)
            .and_then(|data| {
                content
                    .pointer("/content/source/media_type")
                    .and_then(Value::as_str)
                    .filter(|m| !m.starts_with("text/"))
                    .map(|m| format!("（抓到的内容不是文本：{m}）"))
                    .or_else(|| Some(data.to_string()))
            })
            .or_else(|| {
                content.get("error_code").and_then(Value::as_str).map(|e| format!("抓取失败：{e}"))
            })
            .unwrap_or_else(|| compact_json(content))
    } else if kind.ends_with("_tool_result") {
        // 代码执行：stdout / stderr / return_code
        let field = |k: &str| content.get(k).and_then(Value::as_str).unwrap_or_default();
        match content.get("type").and_then(Value::as_str) {
            Some("bash_code_execution_result") => {
                let mut out = field("stdout").to_string();
                if !field("stderr").is_empty() {
                    out.push_str("\n[stderr] ");
                    out.push_str(field("stderr"));
                }
                if content.get("return_code").and_then(Value::as_i64).is_some_and(|c| c != 0) {
                    out.push_str(&format!(
                        "\n[exit] {}",
                        content.get("return_code").and_then(Value::as_i64).unwrap_or_default()
                    ));
                }
                if out.trim().is_empty() { compact_json(content) } else { out }
            }
            Some("text_editor_code_execution_view_result") => field("content").to_string(),
            Some("text_editor_code_execution_str_replace_result") => content
                .get("lines")
                .and_then(Value::as_array)
                .map(|lines| lines.iter().filter_map(Value::as_str).collect::<Vec<_>>().join("\n"))
                .unwrap_or_default(),
            _ => content
                .get("error_code")
                .and_then(Value::as_str)
                .map(|e| format!("执行失败：{e}"))
                .unwrap_or_else(|| compact_json(content)),
        }
    } else {
        return None;
    };
    let body = truncate_chars(body.trim().to_string(), MAX_BLOCK_TEXT);
    (!body.is_empty()).then(|| format!("[服务端工具 {kind}] {body}"))
}

/// `web_search_tool_result.content`：结果列表或错误对象。
fn search_results_text(content: &Value) -> String {
    if let Some(error) = content.get("error_code").and_then(Value::as_str) {
        return format!("搜索失败：{error}");
    }
    let Some(results) = content.as_array() else {
        return compact_json(content);
    };
    if results.is_empty() {
        return "（没有结果）".to_string();
    }
    results
        .iter()
        .enumerate()
        .map(|(i, r)| {
            let title = r.get("title").and_then(Value::as_str).unwrap_or("（无标题）");
            let url = r.get("url").and_then(Value::as_str).unwrap_or_default();
            let age = r.get("page_age").and_then(Value::as_str).unwrap_or_default();
            if age.is_empty() {
                format!("{}. {title} — {url}", i + 1)
            } else {
                format!("{}. {title} — {url}（{age}）", i + 1)
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// 认不出形状时把 JSON 压成一行（去掉 `encrypted_content` 这类密文，太占地方也没用）。
fn compact_json(value: &Value) -> String {
    fn scrub(value: &Value) -> Value {
        match value {
            Value::Object(map) => Value::Object(
                map.iter()
                    .filter(|(k, _)| !k.starts_with("encrypted"))
                    .map(|(k, v)| (k.clone(), scrub(v)))
                    .collect(),
            ),
            Value::Array(items) => Value::Array(items.iter().map(scrub).collect()),
            other => other.clone(),
        }
    }
    scrub(value).to_string()
}

/// 按字符数截断（不切坏 UTF-8）。
fn truncate_chars(text: String, max: usize) -> String {
    if text.chars().count() <= max {
        return text;
    }
    let mut out: String = text.chars().take(max).collect();
    out.push('…');
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::provider::{ChatMessage as M, ContentPart};

    #[test]
    fn extracts_system_and_converts_user_parts() {
        let messages = vec![M::new("system", "You are helpful."), M::new("user", "看图")];
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
        let roles: Vec<&str> = out.iter().map(|m| m["role"].as_str().unwrap()).collect();
        assert_eq!(roles, ["user", "assistant", "user", "assistant"]);
        let results = &out[2]["content"];
        assert_eq!(results.as_array().unwrap().len(), 2);
        assert_eq!(results[0]["tool_use_id"], "t1");
        // 纯文本工具结果的 content 是字符串
        assert_eq!(results[0]["content"], "内容1");
        // 工具调用转成 tool_use，参数解析成对象（空文本不产生块）
        assert_eq!(out[1]["content"][0]["type"], "tool_use");
        assert_eq!(out[1]["content"][0]["input"]["path"], "a");

        // 带图片的工具结果：tool_result 的 content 变成 text + image 块
        let with_image = vec![M::tool_result_with_images(
            "t1".into(),
            "截图".into(),
            vec![crate::tools::OutputImage { media_type: "image/png".into(), data: "QUJD".into() }],
        )];
        let (_, out) = convert_messages(&with_image);
        let block = &out[0]["content"][0];
        assert_eq!(block["type"], "tool_result");
        assert_eq!(block["content"][0]["type"], "text");
        assert_eq!(block["content"][0]["text"], "截图");
        assert_eq!(block["content"][1]["type"], "image");
        assert_eq!(block["content"][1]["source"]["data"], "QUJD");
    }

    #[test]
    fn prepends_user_when_history_starts_with_assistant() {
        let messages = vec![M::new("assistant", "继续")];
        let (_, out) = convert_messages(&messages);
        assert_eq!(out[0]["role"], "user");
        assert_eq!(out[1]["role"], "assistant");
    }

    /// 带工具调用的 assistant 轮：思考块排在最前，后面才是正文和 tool_use
    #[test]
    fn thinking_blocks_lead_the_assistant_turn() {
        let mut call = M::assistant_tool_calls(
            "先看".into(),
            vec![ToolCall {
                id: "toolu_1".into(),
                kind: "function".into(),
                function: crate::provider::FunctionCall {
                    name: "read_file".into(),
                    arguments: r#"{"path":"a"}"#.into(),
                },
            }],
        );
        call.thinking = vec![json!({ "type": "thinking", "thinking": "想", "signature": "sig" })];
        let (_, out) = convert_messages(&[M::new("user", "读"), call]);
        let content = out[1]["content"].as_array().unwrap();
        let kinds: Vec<_> = content.iter().map(|b| b["type"].as_str().unwrap()).collect();
        assert_eq!(kinds, ["thinking", "text", "tool_use"]);
        assert_eq!(content[0]["signature"], "sig");
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
            r#"{"type":"content_block_delta","index":0,"delta":{"type":"signature_delta","signature":"sig"}}"#,
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
        // 思考块连签名原样留着，回传时用
        assert_eq!(
            blocks.take_thinking(),
            vec![json!({ "type": "thinking", "thinking": "想", "signature": "sig" })]
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

    /// 思考强度：老格式（4.5 那代）发 `budget_tokens`，且**不发** temperature
    /// （Anthropic 要求开 thinking 时 temperature 为默认值 1）。
    #[test]
    fn budget_thinking_wins_over_temperature() {
        let messages = vec![M::new("user", "算一下")];
        let params = RequestParams {
            temperature: Some(0.3),
            thinking: Some(crate::provider::ThinkingLevel::High),
            ..Default::default()
        };
        let body = build_body("claude-sonnet-4-5", &messages, &[], &params);
        assert_eq!(body["thinking"]["type"], "enabled");
        assert_eq!(body["thinking"]["budget_tokens"], 16_000);
        assert!(body.get("temperature").is_none(), "{body}");
        assert_eq!(body["max_tokens"], MAX_TOKENS_DEFAULT);

        // 没设档位：照旧发 temperature，不发 thinking（4.5 收非默认 temperature）
        let only_temp = RequestParams { temperature: Some(0.3), ..Default::default() };
        let body = build_body("claude-sonnet-4-5", &messages, &[], &only_temp);
        assert_eq!(body["temperature"], 0.3);
        assert!(body.get("thinking").is_none());
        // 都不设：两个键都不出现
        let body = build_body("m", &messages, &[], &RequestParams::default());
        assert!(body.get("thinking").is_none() && body.get("temperature").is_none());
        // 「关」：老格式不带 thinking，temperature 照发
        let off = RequestParams {
            temperature: Some(0.2),
            thinking: Some(crate::provider::ThinkingLevel::Off),
            ..Default::default()
        };
        let body = build_body("claude-sonnet-4-5", &messages, &[], &off);
        assert!(body.get("thinking").is_none());
        assert_eq!(body["temperature"], 0.2);
    }

    /// 新格式：`adaptive` + `display` + `output_config.effort`；而且这一类模型
    /// **无论开不开思考**都不接受非默认的 temperature（以前填了预算才不发，是 bug）。
    #[test]
    fn adaptive_thinking_never_sends_temperature() {
        let messages = vec![M::new("user", "算一下")];
        let params = RequestParams {
            temperature: Some(0.3),
            thinking: Some(crate::provider::ThinkingLevel::XHigh),
            ..Default::default()
        };
        let body = build_body("claude-opus-4-7", &messages, &[], &params);
        assert_eq!(body["thinking"]["type"], "adaptive");
        assert_eq!(body["thinking"]["display"], "summarized");
        assert_eq!(body["output_config"]["effort"], "xhigh");
        assert!(body.get("temperature").is_none(), "{body}");

        // 只设了 temperature、没开思考：同样不发（4.7 以后不认非默认值）
        let only_temp = RequestParams { temperature: Some(0.3), ..Default::default() };
        let body = build_body("claude-opus-4-7", &messages, &[], &only_temp);
        assert!(body.get("temperature").is_none(), "{body}");
        assert!(body.get("thinking").is_none());

        // 关不掉思考的模型上选「关」：降到最低的开档
        let off = RequestParams {
            thinking: Some(crate::provider::ThinkingLevel::Off),
            ..Default::default()
        };
        let body = build_body("claude-opus-5-5", &messages, &[], &off);
        assert_eq!(body["thinking"]["type"], "adaptive");
        assert_eq!(body["output_config"]["effort"], "low");
    }

    /// `extra` 里的手写字段仍然盖过档位（总开关）。
    #[test]
    fn extra_overrides_thinking() {
        let messages = vec![M::new("user", "算一下")];
        let params = RequestParams {
            thinking: Some(crate::provider::ThinkingLevel::Low),
            extra: json!({"thinking": {"type": "enabled", "budget_tokens": 4000}}),
            ..Default::default()
        };
        let body = build_body("claude-opus-4-7", &messages, &[], &params);
        assert_eq!(body["thinking"]["budget_tokens"], 4_000);
    }

    /// 服务端工具和客户端工具合进同一个 tools 数组；缓存断点仍在最后一个客户端工具上。
    #[test]
    fn merges_server_tools_with_client_tools() {
        let messages = vec![M::new("user", "搜一下")];
        let tools = vec![
            json!({"type":"function","function":{"name":"read_file","parameters":{"type":"object"}}}),
            json!({"type":"function","function":{"name":"shell","parameters":{"type":"object"}}}),
        ];
        let params = RequestParams {
            server_tools: vec!["web_search".into(), "code_execution".into(), "不认识的".into()],
            ..Default::default()
        };
        let body = build_body("m", &messages, &tools, &params);
        let arr = body["tools"].as_array().unwrap();
        // 两客户端 + 两个认识的服务端；不认识的丢掉
        assert_eq!(arr.len(), 4);
        assert_eq!(arr[0]["name"], "read_file");
        assert_eq!(arr[1]["name"], "shell");
        assert_eq!(arr[1]["cache_control"]["type"], "ephemeral");
        assert_eq!(arr[2]["type"], "web_search_20250305");
        assert_eq!(arr[2]["name"], "web_search");
        assert_eq!(arr[2]["max_uses"], 5);
        assert!(arr[2].get("cache_control").is_none());
        assert_eq!(arr[3]["type"], "code_execution_20250825");

        // 只有服务端工具（没有客户端工具）也要发 tools 数组
        let body = build_body("m", &messages, &[], &params);
        assert_eq!(body["tools"].as_array().unwrap().len(), 2);
        // 只有客户端工具：和以前一样
        let body = build_body("m", &messages, &tools, &RequestParams::default());
        assert_eq!(body["tools"].as_array().unwrap().len(), 2);
        // 都没有：不发 tools
        let body = build_body("m", &messages, &[], &RequestParams::default());
        assert!(body.get("tools").is_none());

        // 名单里每个工具都有名字 / 说明 / 片段，且片段里的 name 与内部名字一致
        for tool in SERVER_TOOLS {
            assert!(!tool.desc.is_empty());
            let spec = (tool.spec)();
            assert_eq!(spec["name"], tool.name);
            assert!(spec["type"].as_str().unwrap().contains('_'), "{spec}");
        }
    }

    /// `server_tool_use` / `web_search_tool_result` 解析：结果转成正文文本经
    /// `StreamEvent::Text` 送出（内核拼进本步 assistant 文本 → 进日志 → 下一轮带上历史），
    /// **不**变成 tool_calls（否则工具循环会去找一个我们跑不了的工具）。
    #[test]
    fn parses_server_tool_blocks_into_text_events() {
        let mut blocks = ToolBlocks::default();
        let mut usage = crate::provider::Usage::default();
        let mut seen = false;
        let mut events = Vec::new();
        let lines = [
            r#"{"type":"content_block_start","index":0,"content_block":{"type":"server_tool_use","id":"srvtoolu_1","name":"web_search"}}"#,
            r#"{"type":"content_block_delta","index":0,"delta":{"type":"input_json_delta","partial_json":"{\"query\":\"rust\"}"}}"#,
            r#"{"type":"content_block_start","index":1,"content_block":{"type":"web_search_tool_result","tool_use_id":"srvtoolu_1","content":[{"type":"web_search_result","url":"https://example.com/a","title":"甲","page_age":"2025-04-30","encrypted_content":"EqgfCioIARgB"},{"type":"web_search_result","url":"https://example.com/b","title":"乙"}]}}"#,
            r#"{"type":"content_block_start","index":2,"content_block":{"type":"web_fetch_tool_result","tool_use_id":"srvtoolu_2","content":{"type":"web_fetch_result","url":"https://example.com/a","content":{"type":"document","source":{"type":"text","media_type":"text/plain","data":"正文内容"}}}}}"#,
            r#"{"type":"content_block_start","index":3,"content_block":{"type":"bash_code_execution_tool_result","tool_use_id":"srvtoolu_3","content":{"type":"bash_code_execution_result","stdout":"1 2 3\n","stderr":"","return_code":0,"content":[]}}}"#,
            r#"{"type":"message_delta","delta":{"stop_reason":"end_turn"},"usage":{"output_tokens":7}}"#,
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
        assert_eq!(finish.as_deref(), Some("stop"));
        // 三个结果块 → 三条正文事件（`server_tool_use` 本身不送）
        let texts: Vec<String> = events
            .into_iter()
            .map(|e| match e {
                StreamEvent::Text(t) => t,
                other => panic!("意外的事件：{other:?}"),
            })
            .collect();
        assert_eq!(texts.len(), 3, "{texts:?}");
        let search = &texts[0];
        assert!(search.contains("甲") && search.contains("https://example.com/a"), "{search}");
        assert!(search.contains("（2025-04-30）"), "{search}");
        assert!(search.contains("乙"), "{search}");
        // 密文（encrypted_content）不该出现在文本里
        assert!(!search.contains("EqgfCioIARgB") && !search.contains("encrypted"), "{search}");
        assert!(texts[1].contains("正文内容"), "{}", texts[1]);
        assert!(texts[2].contains("1 2 3"), "{}", texts[2]);
        // 服务端工具不算客户端工具调用
        assert!(blocks.finish().is_empty());

        // 搜索出错：error_code 转成一句人话
        let err_block = json!({
            "type": "web_search_tool_result",
            "tool_use_id": "srvtoolu_9",
            "content": { "type": "web_search_tool_result_error", "error_code": "max_uses_exceeded" },
        });
        let text = server_result_text("web_search_tool_result", &err_block).unwrap();
        assert!(text.contains("max_uses_exceeded"), "{text}");
        // 不认识的块名不产生文本
        assert!(server_result_text("thinking", &json!({ "content": "x" })).is_none());
    }

    /// 服务端工具的结果文本进历史：拼进 assistant 文本后原样发回去（顺序在正文之后）。
    #[test]
    fn server_tool_text_goes_back_into_history() {
        let mut m = M::new("assistant", "我查一下");
        m.parts.push(ContentPart::Text("[服务端工具 web_search_tool_result] 1. 甲 — u".into()));
        let blocks = content_blocks(&m);
        assert_eq!(blocks.len(), 2);
        assert_eq!(blocks[0]["text"], "我查一下");
        assert!(blocks[1]["text"].as_str().unwrap().contains("web_search_tool_result"));
    }
}
