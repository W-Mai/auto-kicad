//! Duplicate-rejecting JSON structure preserving exact numeric tokens.
use crate::ParseError;
use serde::de;
use serde_json::value::RawValue;
use std::collections::BTreeMap;

pub(crate) fn serialize_raw<S: serde::Serializer>(
    raw: &RawValue,
    serializer: S,
) -> Result<S::Ok, S::Error> {
    // A string survives intermediate serde_json::Value and durable journal conversions exactly.
    serializer.serialize_str(raw.get())
}

pub(crate) fn json_error(error: serde_json::Error) -> ParseError {
    ParseError::Json(error.to_string())
}

// Raw numeric tokens avoid floating-point truncation and reserved-key reinterpretation.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum Node {
    Scalar(String),
    String(String),
    Array(Vec<Node>),
    Object(BTreeMap<String, Node>),
}

impl Node {
    pub(crate) fn parse(raw: &RawValue, depth: usize) -> Result<Self, ParseError> {
        if depth > 128 {
            return Err(ParseError::Schema("JSON nesting exceeds bound".into()));
        }
        let text = raw.get();
        Ok(match text.as_bytes()[0] {
            b'{' => Self::Object(
                object(raw)?
                    .into_iter()
                    .map(|(k, v)| Ok((k, Self::parse(&v, depth + 1)?)))
                    .collect::<Result<_, ParseError>>()?,
            ),
            b'[' => {
                let values: Vec<Box<RawValue>> = serde_json::from_str(text).map_err(json_error)?;
                Self::Array(
                    values
                        .iter()
                        .map(|v| Self::parse(v, depth + 1))
                        .collect::<Result<_, _>>()?,
                )
            }
            b'"' => Self::String(serde_json::from_str(text).map_err(json_error)?),
            _ => Self::Scalar(text.into()),
        })
    }
}

pub(crate) fn object(raw: &RawValue) -> Result<BTreeMap<String, Box<RawValue>>, ParseError> {
    struct Unique;
    impl<'de> de::Visitor<'de> for Unique {
        type Value = BTreeMap<String, Box<RawValue>>;
        fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.write_str("an object without duplicate keys")
        }
        fn visit_map<A: de::MapAccess<'de>>(self, mut map: A) -> Result<Self::Value, A::Error> {
            let mut values = BTreeMap::new();
            while let Some((key, value)) = map.next_entry::<String, Box<RawValue>>()? {
                if values.insert(key, value).is_some() {
                    return Err(de::Error::custom("duplicate JSON key"));
                }
            }
            Ok(values)
        }
    }
    let mut decoder = serde_json::Deserializer::from_str(raw.get());
    de::Deserializer::deserialize_map(&mut decoder, Unique).map_err(json_error)
}
