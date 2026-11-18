use serde::de::{Deserialize, Deserializer, MapAccess, SeqAccess, Visitor};
use serde::ser::{Serialize, SerializeMap, SerializeSeq, Serializer};
use serde_json::{Value, value::RawValue};
use std::fmt;
use std::io::{self, Write};

const MIN_BYTES: usize = 4 << 20;
const MAX_PART_BYTES: usize = 1 << 20;
const MAX_FIELDS: usize = 128;
const MAX_ITEMS: usize = 64 << 10;
const SAFE_NESTING: usize = 120;

pub(super) fn format(bytes: &[u8]) -> Option<String> {
    if bytes.len() < MIN_BYTES {
        return None;
    }
    let object = serde_json::from_slice::<Object<'_>>(bytes).ok()?;
    if !object.0.iter().any(|(_, part)| matches!(part, Part::Array(_))) {
        return None;
    }
    let mut out = BoundedBuffer(Vec::with_capacity(
        bytes.len() + bytes.len() / super::REQUEST_HEADROOM_DIVISOR,
    ));
    serde_json::to_writer_pretty(&mut out, &object).ok()?;
    String::from_utf8(out.0).ok()
}

struct Object<'a>(Vec<(String, Part<'a>)>);

enum Part<'a> {
    Value(&'a RawValue),
    Array(Items<'a>),
}

impl<'de> Deserialize<'de> for Object<'de> {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct ObjectVisitor;

        impl<'de> Visitor<'de> for ObjectVisitor {
            type Value = Object<'de>;

            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("a JSON object")
            }

            fn visit_map<M: MapAccess<'de>>(self, mut map: M) -> Result<Self::Value, M::Error> {
                let mut fields: Vec<(String, Part<'de>)> = Vec::new();
                while let Some(key) = map.next_key::<String>()? {
                    if fields.len() == MAX_FIELDS
                        || key == "$serde_json::private::RawValue"
                        || fields.iter().any(|(name, _)| name == &key)
                    {
                        return Err(serde::de::Error::custom("unsupported object layout"));
                    }
                    let part = if matches!(key.as_str(), "messages" | "input" | "tools") {
                        Part::Array(map.next_value()?)
                    } else {
                        let value: &RawValue = map.next_value()?;
                        if value.get().len() > MAX_PART_BYTES {
                            return Err(serde::de::Error::custom("field budget exceeded"));
                        }
                        Part::Value(value)
                    };
                    fields.push((key, part));
                }
                Ok(Object(fields))
            }
        }

        deserializer.deserialize_map(ObjectVisitor)
    }
}

impl Serialize for Object<'_> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut map = serializer.serialize_map(Some(self.0.len()))?;
        for (key, part) in &self.0 {
            match part {
                Part::Value(raw) => map.serialize_entry(key, &parse(raw)?)?,
                Part::Array(items) => map.serialize_entry(key, items)?,
            }
        }
        map.end()
    }
}

struct Items<'a>(Vec<&'a RawValue>);

impl<'de> Deserialize<'de> for Items<'de> {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct ItemsVisitor;

        impl<'de> Visitor<'de> for ItemsVisitor {
            type Value = Items<'de>;

            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("a JSON array")
            }

            fn visit_seq<S: SeqAccess<'de>>(self, mut seq: S) -> Result<Self::Value, S::Error> {
                let mut items = Vec::new();
                while let Some(item) = seq.next_element::<&RawValue>()? {
                    if items.len() == MAX_ITEMS || item.get().len() > MAX_PART_BYTES {
                        return Err(serde::de::Error::custom("array item budget exceeded"));
                    }
                    items.push(item);
                }
                Ok(Items(items))
            }
        }

        deserializer.deserialize_seq(ItemsVisitor)
    }
}

impl Serialize for Items<'_> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut seq = serializer.serialize_seq(Some(self.0.len()))?;
        for raw in &self.0 {
            seq.serialize_element(&parse(raw)?)?;
        }
        seq.end()
    }
}

fn parse<E: serde::ser::Error>(raw: &RawValue) -> Result<Value, E> {
    let text = raw.get();
    let open_delimiters = text.bytes().filter(|&b| matches!(b, b'[' | b'{'));
    if open_delimiters.count() >= SAFE_NESTING {
        return Err(E::custom("use full parser for nesting validation"));
    }
    serde_json::from_str(text).map_err(E::custom)
}

struct BoundedBuffer(Vec<u8>);

impl Write for BoundedBuffer {
    fn write(&mut self, data: &[u8]) -> io::Result<usize> {
        if data.len() > self.0.capacity() - self.0.len() {
            return Err(io::ErrorKind::WriteZero.into());
        }
        self.0.extend_from_slice(data);
        Ok(data.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn split_array_matches_full_parser_and_rejects_ambiguous_large_values() {
        let text = "a".repeat(16 << 10);
        let item = format!(r#"{{"z":3,"text":"{text}\n\uD83D\uDE00","z":7,"x":1e-4}}"#);
        let array = std::iter::repeat_n(item.as_str(), 320).collect::<Vec<_>>().join(",");
        let input = format!(r#"{{"z":0,"messages":[{array},{{"t":true}}],"model":"大模型"}}"#);
        let expected =
            serde_json::to_string_pretty(&serde_json::from_str::<Value>(&input).unwrap()).unwrap();
        assert_eq!(format(input.as_bytes()), Some(expected));
        let duplicate = format!(r#"{{"messages":[{array}],"messages":[]}}"#);
        assert!(format(duplicate.as_bytes()).is_none());
        let invalid = format!(r#"{{"messages":[{array},1e9999]}}"#);
        assert!(format(invalid.as_bytes()).is_none());
        let marker = format!(r#"{{"$serde_json::private::RawValue":"null","input":[{array}]}}"#);
        assert!(format(marker.as_bytes()).is_none());
    }
}
