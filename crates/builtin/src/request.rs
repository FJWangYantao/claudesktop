use crate::provider::ChatMessage;
use serde::Serialize;
use serde::ser::SerializeMap;
use serde_json::Value;
use std::borrow::Cow;
use std::fmt;

pub(crate) struct RequestBody<'a, Input, System = ()> {
    fields: Value,
    extra: &'a Value,
    input_key: &'static str,
    input: Input,
    system: Option<(&'static str, System)>,
}

impl<'a, Input> RequestBody<'a, Input> {
    pub fn new(fields: Value, extra: &'a Value, input_key: &'static str, input: Input) -> Self {
        Self { fields, extra, input_key, input, system: None }
    }
}

impl<'a, Input, System> RequestBody<'a, Input, System> {
    pub fn with_system<NewSystem>(
        self,
        key: &'static str,
        system: Option<NewSystem>,
    ) -> RequestBody<'a, Input, NewSystem> {
        RequestBody {
            fields: self.fields,
            extra: self.extra,
            input_key: self.input_key,
            input: self.input,
            system: system.map(|value| (key, value)),
        }
    }
}

impl<Input: Serialize, System: Serialize> Serialize for RequestBody<'_, Input, System> {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let extra = self.extra.as_object();
        let overridden = |key: &str| extra.is_some_and(|extra| extra.contains_key(key));
        let mut map = serializer.serialize_map(None)?;
        for (key, value) in self.fields.as_object().expect("请求字段是对象") {
            if !overridden(key) {
                map.serialize_entry(key, value)?;
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
    fn data_url_escapes_and_writes_large_data_without_a_formatted_copy() {
        let data = "abc\"\\\n".repeat(10_000);
        let url = DataUrl { media_type: "image/png", data: &data };
        let encoded = serde_json::to_string(&url).unwrap();
        let restored: String = serde_json::from_str(&encoded).unwrap();
        assert_eq!(restored, format!("data:image/png;base64,{data}"));
    }
}
