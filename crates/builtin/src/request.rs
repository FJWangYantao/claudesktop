use crate::provider::{ChatMessage, Protocol, ToolSpec};
use serde::Serialize;
use serde::ser::{SerializeMap, SerializeSeq};
use serde_json::Value;
use std::borrow::Cow;
use std::fmt;

pub(crate) struct RequestBody<'a, Input, System = (), Tools = ()> {
    fields: Value,
    extra: &'a Value,
    input_key: &'static str,
    input: Input,
    system: Option<(&'static str, System)>,
    tools: Option<Tools>,
}

impl<'a, Input> RequestBody<'a, Input> {
    pub fn new(fields: Value, extra: &'a Value, input_key: &'static str, input: Input) -> Self {
        Self { fields, extra, input_key, input, system: None, tools: None }
    }
}

impl<'a, Input, System, Tools> RequestBody<'a, Input, System, Tools> {
    pub fn with_system<NewSystem>(
        self,
        key: &'static str,
        system: Option<NewSystem>,
    ) -> RequestBody<'a, Input, NewSystem, Tools> {
        RequestBody {
            fields: self.fields,
            extra: self.extra,
            input_key: self.input_key,
            input: self.input,
            system: system.map(|value| (key, value)),
            tools: self.tools,
        }
    }

    pub fn with_tools<NewTools>(
        mut self,
        tools: Option<NewTools>,
    ) -> RequestBody<'a, Input, System, NewTools> {
        if tools.is_some() {
            self.fields["tools"] = Value::Null;
        }
        RequestBody {
            fields: self.fields,
            extra: self.extra,
            input_key: self.input_key,
            input: self.input,
            system: self.system,
            tools,
        }
    }
}

impl<Input: Serialize, System: Serialize, Tools: Serialize> Serialize
    for RequestBody<'_, Input, System, Tools>
{
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let extra = self.extra.as_object();
        let overridden = |key: &str| extra.is_some_and(|extra| extra.contains_key(key));
        let mut map = serializer.serialize_map(None)?;
        for (key, value) in self.fields.as_object().expect("请求字段是对象") {
            if !overridden(key) {
                if key == "tools"
                    && let Some(tools) = &self.tools
                {
                    map.serialize_entry(key, tools)?;
                } else {
                    map.serialize_entry(key, value)?;
                }
            }
        }
        if !overridden(self.input_key) {
            map.serialize_entry(self.input_key, &self.input)?;
        }
        if let Some((key, value)) = &self.system
            && !overridden(key)
        {
            map.serialize_entry(key, value)?;
        }
        if let Some(extra) = extra {
            for (key, value) in extra {
                map.serialize_entry(key, value)?;
            }
        }
        map.end()
    }
}

pub(crate) struct WireTools<'a> {
    pub client: &'a [ToolSpec],
    pub server: Vec<Value>,
    pub protocol: Protocol,
}

impl Serialize for WireTools<'_> {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut sequence = serializer.serialize_seq(Some(self.client.len() + self.server.len()))?;
        for (index, tool) in self.client.iter().enumerate() {
            sequence.serialize_element(&WireTool {
                value: tool,
                protocol: self.protocol,
                last: index + 1 == self.client.len(),
            })?;
        }
        for tool in &self.server {
            sequence.serialize_element(tool)?;
        }
        sequence.end()
    }
}

struct WireTool<'a> {
    value: &'a Value,
    protocol: Protocol,
    last: bool,
}

impl Serialize for WireTool<'_> {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        if self.protocol == Protocol::OpenAiCompat
            || (self.protocol == Protocol::Responses && self.value.get("function").is_none())
        {
            return self.value.serialize(serializer);
        }
        let function = &self.value["function"];
        let mut map = serializer.serialize_map(None)?;
        if self.protocol == Protocol::Anthropic {
            map.serialize_entry("name", &function["name"])?;
            map.serialize_entry("input_schema", &function["parameters"])?;
            if let Some(description) = function.get("description").filter(|value| !value.is_null())
            {
                map.serialize_entry("description", description)?;
            }
            if self.last {
                #[derive(Serialize)]
                struct CacheControl {
                    #[serde(rename = "type")]
                    kind: &'static str,
                }
                map.serialize_entry("cache_control", &CacheControl { kind: "ephemeral" })?;
            }
        } else {
            map.serialize_entry("type", "function")?;
            for key in ["name", "description", "parameters", "strict"] {
                if let Some(value) = function.get(key) {
                    map.serialize_entry(key, value)?;
                }
            }
        }
        map.end()
    }
}

#[derive(Clone, Copy)]
pub(crate) struct DataUrl<'a> {
    pub media_type: &'a str,
    pub data: &'a str,
}

impl fmt::Display for DataUrl<'_> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "data:{};base64,{}", self.media_type, self.data)
    }
}

impl Serialize for DataUrl<'_> {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_str(self)
    }
}

/// OpenAI 兼容端点的 messages：开头的多条 `system` 并成一条再序列化。
/// 系统提示词按稳定性拆成两条是为了 Anthropic 的缓存断点；vLLM / Ollama 一类模板只认一条
/// `system`（第二条会覆盖前一条或直接 400），所以在共享层这里合并。
pub(crate) struct WireMessages<'a>(pub &'a [ChatMessage]);

impl Serialize for WireMessages<'_> {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeSeq as _;
        let split = self.0.iter().take_while(|message| message.role == "system").count();
        if split == 1 {
            let blank = self.0[0].text_ref().is_none_or(|text| text.trim().is_empty());
            let mut sequence = serializer.serialize_seq(Some(self.0.len() - usize::from(blank)))?;
            if !blank {
                sequence.serialize_element(&self.0[0])?;
            }
            for message in &self.0[1..] {
                sequence.serialize_element(message)?;
            }
            return sequence.end();
        }
        // 多条（或没有）时把前导 system 合成一条；全是空白就整段省掉
        let text = self.0[..split]
            .iter()
            .filter_map(ChatMessage::text_ref)
            .filter(|text| !text.trim().is_empty())
            .collect::<Vec<_>>()
            .join("\n\n");
        let mut sequence =
            serializer.serialize_seq(Some(self.0.len() - split + usize::from(!text.is_empty())))?;
        if !text.is_empty() {
            sequence.serialize_element(&serde_json::json!({"role": "system", "content": text}))?;
        }
        for message in &self.0[split..] {
            sequence.serialize_element(message)?;
        }
        sequence.end()
    }
}

pub(crate) fn system_text(messages: &[ChatMessage], skip_blank: bool) -> Option<Cow<'_, str>> {
    let mut parts: Vec<_> = messages
        .iter()
        .filter(|message| message.role == "system")
        .filter_map(ChatMessage::text_ref)
        .filter(|text| !skip_blank || !text.trim().is_empty())
        .collect();
    match parts.len() {
        0 => None,
        1 => parts.pop(),
        _ => Some(Cow::Owned(
            parts.iter().map(|part| part.as_ref()).collect::<Vec<_>>().join("\n\n"),
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn leading_system_messages_are_merged_for_openai_compatible_endpoints() {
        let messages = vec![
            ChatMessage::new("system", "稳定的前缀"),
            ChatMessage::new("system", "易变的尾巴"),
            ChatMessage::new("system", "   "),
            ChatMessage::new("user", "你好"),
        ];
        let encoded = serde_json::to_value(WireMessages(&messages)).unwrap();
        assert_eq!(
            encoded,
            json!([
                {"role": "system", "content": "稳定的前缀\n\n易变的尾巴"},
                {"role": "user", "content": "你好"},
            ])
        );
        // 单条 system 原样穿过；前导 system 全是空白就整段省掉
        let single = vec![ChatMessage::new("system", "只有一条"), ChatMessage::new("user", "你好")];
        assert_eq!(
            serde_json::to_value(WireMessages(&single)).unwrap(),
            json!([
                {"role": "system", "content": "只有一条"},
                {"role": "user", "content": "你好"},
            ])
        );
        let blank = vec![ChatMessage::new("system", "  \n "), ChatMessage::new("user", "你好")];
        assert_eq!(
            serde_json::to_value(WireMessages(&blank)).unwrap(),
            json!([{"role": "user", "content": "你好"}])
        );
    }

    #[test]
    fn extras_override_borrowed_input_system_and_regular_fields_once() {
        let input = [json!({"text": "original"})];
        let extra = json!({"model":"override", "messages":null, "system":["replacement"]});
        let body =
            RequestBody::new(json!({"model":"m", "stream":true}), &extra, "messages", &input)
                .with_system("system", Some("original"));
        let encoded = serde_json::to_string(&body).unwrap();
        let value: Value = serde_json::from_str(&encoded).unwrap();
        assert_eq!(
            value,
            json!({"model":"override", "stream":true, "messages":null, "system":["replacement"]})
        );
        assert_eq!(encoded.matches("\"messages\":").count(), 1);
        assert_eq!(encoded.matches("\"system\":").count(), 1);
    }

    #[test]
    fn borrowed_tool_formats_preserve_schema_fields_and_cache_boundaries() {
        let schema = json!({
            "type": "object", "properties": {
                "text": {"type":"string", "description":"引号\"与\\路径\n下一行"},
                "rows": {"type":"array", "items":{"anyOf":[{"type":"number"},{"type":"null"}]}}
            }, "additionalProperties": false,
        });
        let tools = vec![
            json!({"type":"function", "function":{
                "name":"first", "description":"中文说明", "parameters":schema, "strict":true
            }})
            .into(),
            json!({"type":"function", "function":{"name":"last", "description":null}}).into(),
        ];
        let server = json!({"type":"web_search_20250305", "name":"web_search"});
        let anthropic = WireTools {
            client: &tools,
            server: vec![server.clone()],
            protocol: Protocol::Anthropic,
        };
        let expected = json!([
            {"name":"first", "input_schema":schema, "description":"中文说明"},
            {"name":"last", "input_schema":null, "cache_control":{"type":"ephemeral"}},
            server,
        ]);
        assert_eq!(serde_json::to_string(&anthropic).unwrap(), expected.to_string());
        let responses =
            WireTools { client: &tools, server: Vec::new(), protocol: Protocol::Responses };
        let expected = json!([
            {"type":"function", "name":"first", "description":"中文说明", "parameters":schema, "strict":true},
            {"type":"function", "name":"last", "description":null},
        ]);
        assert_eq!(serde_json::to_string(&responses).unwrap(), expected.to_string());
        assert_eq!(tools[0]["function"]["parameters"], schema);
        assert!(tools[1]["function"].get("parameters").is_none());
        let native: Vec<_> =
            vec![json!({"type":"web_search_preview", "custom":{"enabled":true}}).into()];
        let native_wire =
            WireTools { client: &native, server: Vec::new(), protocol: Protocol::Responses };
        assert_eq!(
            serde_json::to_value(native_wire).unwrap(),
            serde_json::to_value(native).unwrap()
        );
    }

    #[test]
    fn extra_tools_bypass_the_default_serializer_and_appear_once() {
        struct Unused;
        impl Serialize for Unused {
            fn serialize<S: serde::Serializer>(&self, _: S) -> Result<S::Ok, S::Error> {
                panic!("被 extra 覆盖的工具不应序列化");
            }
        }
        for replacement in [Value::Null, json!([]), json!([{"type":"custom"}])] {
            let extra = json!({"tools": replacement});
            let body =
                RequestBody::new(json!({"model":"m"}), &extra, "messages", Vec::<Value>::new())
                    .with_tools(Some(Unused));
            let encoded = serde_json::to_string(&body).unwrap();
            assert_eq!(encoded.matches("\"tools\":").count(), 1);
            assert_eq!(serde_json::from_str::<Value>(&encoded).unwrap()["tools"], replacement);
        }
    }

    #[test]
    fn data_url_escapes_and_writes_large_data_without_a_formatted_copy() {
        let data = "abc\"\\\n".repeat(10_000);
        let url = DataUrl { media_type: "image/png", data: &data };
        let encoded = serde_json::to_string(&url).unwrap();
        let restored: String = serde_json::from_str(&encoded).unwrap();
        assert_eq!(restored, format!("data:image/png;base64,{data}"));
    }
}
