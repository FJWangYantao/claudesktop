use serde::Deserialize;
use serde::de::{DeserializeSeed, MapAccess, SeqAccess, Visitor};
use std::fmt;

#[derive(Debug, Default, PartialEq, Eq)]
pub(super) struct RequestStats {
    pub model: Option<String>,
    pub user_id: Option<String>,
    pub messages: Option<usize>,
    pub tools: Option<usize>,
}

pub(super) fn parse(body: &[u8]) -> serde_json::Result<RequestStats> {
    let mut deserializer = serde_json::Deserializer::from_slice(body);
    let summary = Selection::Request.deserialize(&mut deserializer)?;
    deserializer.end()?;
    Ok(summary.fields())
}

#[derive(Clone, Copy)]
enum Selection {
    Request,
    Metadata,
    Text,
    Count,
    Ignore,
}

enum Summary {
    Other,
    Text(String),
    Count(usize),
    Fields(RequestStats),
}

impl Summary {
    fn text(self) -> Option<String> {
        if let Self::Text(text) = self { Some(text) } else { None }
    }

    fn count(self) -> Option<usize> {
        if let Self::Count(count) = self { Some(count) } else { None }
    }

    fn fields(self) -> RequestStats {
        if let Self::Fields(fields) = self { fields } else { RequestStats::default() }
    }
}

#[derive(Deserialize)]
#[serde(field_identifier, rename_all = "snake_case")]
enum Field {
    Model,
    Metadata,
    UserId,
    Messages,
    Input,
    Tools,
    #[serde(other)]
    Other,
}

impl<'de> DeserializeSeed<'de> for Selection {
    type Value = Summary;

    fn deserialize<D: serde::Deserializer<'de>>(
        self,
        deserializer: D,
    ) -> Result<Summary, D::Error> {
        deserializer.deserialize_any(self)
    }
}

impl<'de> Visitor<'de> for Selection {
    type Value = Summary;

    fn expecting(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
        formatter.write_str("a JSON value")
    }

    fn visit_unit<E: serde::de::Error>(self) -> Result<Summary, E> {
        Ok(Summary::Other)
    }

    fn visit_bool<E: serde::de::Error>(self, _: bool) -> Result<Summary, E> {
        Ok(Summary::Other)
    }

    fn visit_i64<E: serde::de::Error>(self, _: i64) -> Result<Summary, E> {
        Ok(Summary::Other)
    }

    fn visit_u64<E: serde::de::Error>(self, _: u64) -> Result<Summary, E> {
        Ok(Summary::Other)
    }

    fn visit_f64<E: serde::de::Error>(self, _: f64) -> Result<Summary, E> {
        Ok(Summary::Other)
    }

    fn visit_str<E: serde::de::Error>(self, value: &str) -> Result<Summary, E> {
        Ok(if matches!(self, Self::Text) {
            Summary::Text(value.to_string())
        } else {
            Summary::Other
        })
    }

    fn visit_seq<A: SeqAccess<'de>>(self, mut sequence: A) -> Result<Summary, A::Error> {
        let mut count = 0;
        while sequence.next_element_seed(Self::Ignore)?.is_some() {
            count += 1;
        }
        Ok(if matches!(self, Self::Count) { Summary::Count(count) } else { Summary::Other })
    }

    fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Summary, A::Error> {
        let mut stats = RequestStats::default();
        let mut has_messages = false;
        let mut input = None;
        while let Some(field) = map.next_key::<Field>()? {
            match (self, field) {
                (Self::Request, Field::Model) => {
                    stats.model = map.next_value_seed(Self::Text)?.text();
                }
                (Self::Request, Field::Metadata) => {
                    stats.user_id = map.next_value_seed(Self::Metadata)?.fields().user_id;
                }
                (Self::Metadata, Field::UserId) => {
                    stats.user_id = map.next_value_seed(Self::Text)?.text();
                }
                (Self::Request, Field::Messages) => {
                    has_messages = true;
                    stats.messages = map.next_value_seed(Self::Count)?.count();
                }
                (Self::Request, Field::Input) => {
                    input = map.next_value_seed(Self::Count)?.count();
                }
                (Self::Request, Field::Tools) => {
                    stats.tools = map.next_value_seed(Self::Count)?.count();
                }
                _ => {
                    map.next_value_seed(Self::Ignore)?;
                }
            }
        }
        if matches!(self, Self::Request) && !has_messages {
            stats.messages = input;
        }
        Ok(if matches!(self, Self::Request | Self::Metadata) {
            Summary::Fields(stats)
        } else {
            Summary::Other
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{Value, json};

    fn previous(body: &[u8]) -> serde_json::Result<RequestStats> {
        let value: Value = serde_json::from_slice(body)?;
        Ok(RequestStats {
            model: value.get("model").and_then(Value::as_str).map(str::to_string),
            user_id: value.pointer("/metadata/user_id").and_then(Value::as_str).map(str::to_string),
            messages: value
                .get("messages")
                .or_else(|| value.get("input"))
                .and_then(Value::as_array)
                .map(Vec::len),
            tools: value.get("tools").and_then(Value::as_array).map(Vec::len),
        })
    }

    fn agrees(body: &[u8]) {
        assert_eq!(parse(body).ok(), previous(body).ok(), "{}", String::from_utf8_lossy(body));
    }

    #[test]
    fn extracts_stats_for_all_protocols_and_tolerates_other_shapes() {
        let values = [
            json!(null),
            json!(false),
            json!(12),
            json!(-0.25),
            json!("escaped \" text \\ 中文 😀"),
            json!([]),
            json!([null, {}, {"content": [{"type": "image", "data": "QUJD"}]}]),
            json!({}),
            json!({"user_id": "user_abc_account__session_acp-s1"}),
        ];
        for messages in &values {
            for input in &values {
                agrees(
                    &serde_json::to_vec(&json!({
                        "model": "fixture-model", "messages": messages,
                        "input": input, "tools": [null, {}],
                        "metadata": {"user_id": "user_a_session_s1"},
                    }))
                    .unwrap(),
                );
            }
            agrees(
                &serde_json::to_vec(&json!({
                    "model": messages, "input": input_fixture(),
                    "tools": messages, "metadata": messages,
                }))
                .unwrap(),
            );
            agrees(&serde_json::to_vec(messages).unwrap());
        }
        for body in [
            r#"{"model":"m","messages":[{},{}],"tools":[]}"#,
            r#"{"model":"m","input":[{}, {}, {}]}"#,
            r#"{"model":"m","input":"plain Responses input","metadata":null}"#,
            r#"{"\u006dodel":"m\u006fdel","metadata":{"\u0075ser_id":"s"}}"#,
        ] {
            agrees(body.as_bytes());
        }
    }

    fn input_fixture() -> Value {
        json!([{"type": "function_call_output", "output": "tool text"}])
    }

    #[test]
    fn duplicate_fields_use_the_last_value_and_messages_shadow_input() {
        for body in [
            r#"{"model":"a","model":null,"tools":[1],"tools":{}}"#,
            r#"{"model":{},"model":"b","messages":null,"input":[1,2]}"#,
            r#"{"input":[1,2],"messages":[],"messages":false}"#,
            r#"{"messages":{},"messages":[1],"input":[],"input":[1,2]}"#,
            r#"{"metadata":{"user_id":"old","user_id":"new"}}"#,
            r#"{"metadata":{"user_id":"old"},"metadata":false}"#,
            r#"{"metadata":{"user_id":false,"user_id":"new"},"model":"m"}"#,
        ] {
            agrees(body.as_bytes());
        }
    }

    #[test]
    fn invalid_ignored_values_do_not_leave_partial_stats() {
        for body in [
            &b""[..],
            br#"{"model":"m","other":1e999}"#,
            br#"{"model":"m","other":"\uD800"}"#,
            br#"{"model":"m","other":{"\uDFFF":0}}"#,
            b"{\"model\":\"m\",\"other\":\"\xff\"}",
            br#"{"model":"m","other":[1,]}"#,
            br#"{"model":"m","other":01}"#,
            br#"{"model":"m","messages":[]} trailing"#,
            br#"{"model":"m","metadata":{"user_id":"s"}"#,
        ] {
            assert!(parse(body).is_err(), "{}", String::from_utf8_lossy(body));
            agrees(body);
        }
        for depth in [124, 125, 126, 127, 128, 129, 256] {
            agrees(
                format!(r#"{{"model":"m","other":{}0{}}}"#, "[".repeat(depth), "]".repeat(depth))
                    .as_bytes(),
            );
        }
    }

    #[test]
    #[ignore]
    fn compare_parse_latency() {
        let input = std::env::var_os("CLAUDESKTOP_GATEWAY_PARSE_BENCH").unwrap();
        let output = std::env::var_os("CLAUDESKTOP_GATEWAY_PARSE_REPORT").unwrap();
        let mut body = Vec::new();
        std::io::Read::read_to_end(
            &mut flate2::read::GzDecoder::new(std::fs::File::open(input).unwrap()),
            &mut body,
        )
        .unwrap();
        assert_eq!(parse(&body).unwrap(), previous(&body).unwrap());
        let mut samples = Vec::new();
        for round in 0..120 {
            for candidate in if round % 2 == 0 { [false, true] } else { [true, false] } {
                let started = std::time::Instant::now();
                let stats = if candidate { parse(&body) } else { previous(&body) }.unwrap();
                std::hint::black_box(stats);
                let elapsed_ms = started.elapsed().as_secs_f64() * 1000.;
                if round >= 20 {
                    samples.push(json!({
                        "round": round - 20,
                        "candidate": candidate,
                        "elapsed_ms": elapsed_ms,
                    }));
                }
            }
        }
        std::fs::write(
            output,
            serde_json::to_vec_pretty(&json!({ "bytes": body.len(), "samples": samples })).unwrap(),
        )
        .unwrap();
    }

    #[test]
    fn large_text_images_and_signatures_are_only_counted_as_array_entries() {
        let value = json!({
            "model": "fixture-model",
            "metadata": {"user_id": "user_a_session_s1"},
            "messages": [{"content": [
                {"type":"text", "text": "escaped \" text \\ 中文\n".repeat(20_000)},
                {"type":"image", "source":{"data":"QUJD".repeat(150_000)}},
                {"type":"thinking", "thinking":"thought".repeat(20_000), "signature":"signed"},
            ]}],
            "tools": [{"input_schema":{"properties":{"payload":{"enum":["large".repeat(100_000)]}}}}],
        });
        let body = serde_json::to_vec(&value).unwrap();
        agrees(&body);
        assert_eq!(
            parse(&body).unwrap(),
            RequestStats {
                model: Some("fixture-model".into()),
                user_id: Some("user_a_session_s1".into()),
                messages: Some(1),
                tools: Some(1),
            }
        );
    }
}
