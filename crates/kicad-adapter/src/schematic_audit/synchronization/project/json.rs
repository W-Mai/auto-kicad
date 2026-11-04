//! Borrowed JSON containers retaining exact unselected tokens and delimiters.
use serde::{
    Deserialize, Deserializer,
    de::{MapAccess, Visitor},
};
use serde_json::value::RawValue;
use std::{collections::BTreeMap, fmt};

const MAX_BYTES: usize = 32 * 1024 * 1024;
const MAX_NODES: usize = 100_000;

struct Unique<'a>(BTreeMap<String, &'a RawValue>);
impl<'de> Deserialize<'de> for Unique<'de> {
    fn deserialize<D: Deserializer<'de>>(decoder: D) -> Result<Self, D::Error> {
        struct UniqueVisitor;
        impl<'de> Visitor<'de> for UniqueVisitor {
            type Value = Unique<'de>;
            fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str("an object with unique keys")
            }
            fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Self::Value, A::Error> {
                let mut rows = BTreeMap::new();
                while let Some((key, value)) = map.next_entry::<String, &'de RawValue>()? {
                    if rows.insert(key, value).is_some() || rows.len() > MAX_NODES {
                        return Err(serde::de::Error::custom(
                            "duplicate or excessive project keys",
                        ));
                    }
                }
                Ok(Unique(rows))
            }
        }
        decoder.deserialize_map(UniqueVisitor)
    }
}

pub(super) fn parse(text: &str) -> Result<&RawValue, String> {
    if text.is_empty() || text.len() > MAX_BYTES {
        return Err("project JSON exceeds input bound".into());
    }
    let raw: &RawValue = serde_json::from_str(text).map_err(|e| e.to_string())?;
    fn validate(raw: &RawValue, depth: usize, nodes: &mut usize) -> Result<(), String> {
        *nodes += 1;
        if depth > 32 || *nodes > MAX_NODES {
            return Err("project JSON exceeds depth/node bound".into());
        }
        match raw.get().as_bytes()[0] {
            b'{' => {
                let rows: Unique<'_> =
                    serde_json::from_str(raw.get()).map_err(|e| e.to_string())?;
                for value in rows.0.values() {
                    validate(value, depth + 1, nodes)?;
                }
            }
            b'[' => {
                let rows: Vec<&RawValue> =
                    serde_json::from_str(raw.get()).map_err(|e| e.to_string())?;
                if rows.len() > MAX_NODES {
                    return Err("project array exceeds node bound".into());
                }
                for value in rows {
                    validate(value, depth + 1, nodes)?;
                }
            }
            _ => {}
        }
        Ok(())
    }
    validate(raw, 0, &mut 0)?;
    Ok(raw)
}

fn offset(parent: &str, value: &str) -> Result<usize, String> {
    let start = (value.as_ptr() as usize)
        .checked_sub(parent.as_ptr() as usize)
        .ok_or("project JSON value does not borrow its parent")?;
    if parent.get(start..start + value.len()) != Some(value) {
        return Err("project JSON value span differs".into());
    }
    Ok(start)
}

struct Row<'a> {
    before_comma: &'a str,
    after_comma: &'a str,
    value: &'a RawValue,
}
struct Sequence<'a> {
    raw: &'a str,
    rows: Vec<Row<'a>>,
    tail: &'a str,
    edits: BTreeMap<usize, Option<String>>,
    insertions: BTreeMap<usize, Vec<String>>,
}
impl<'a> Sequence<'a> {
    fn new(raw: &'a RawValue, values: Vec<&'a RawValue>) -> Result<Self, String> {
        let text = raw.get();
        let mut previous = 1;
        let mut rows = Vec::new();
        for value in values {
            let start = offset(text, value.get())?;
            let head = text
                .get(previous..start)
                .ok_or("overlapping project spans")?;
            let (before_comma, after_comma) = if rows.is_empty() {
                ("", head)
            } else {
                let index = head
                    .bytes()
                    .position(|b| !b.is_ascii_whitespace())
                    .ok_or("missing JSON comma")?;
                if head.as_bytes()[index] != b',' {
                    return Err("missing project JSON separator".into());
                }
                (&head[..index], &head[index + 1..])
            };
            rows.push(Row {
                before_comma,
                after_comma,
                value,
            });
            previous = start + value.get().len();
        }
        Ok(Self {
            raw: text,
            rows,
            tail: &text[previous..],
            edits: BTreeMap::new(),
            insertions: BTreeMap::new(),
        })
    }
    fn set(&mut self, index: usize, value: Option<String>) -> Result<(), String> {
        if index >= self.rows.len() || self.edits.contains_key(&index) {
            return Err("unknown or repeated project JSON selection".into());
        }
        if let Some(value) = &value {
            parse(value)?;
        }
        self.edits.insert(index, value);
        Ok(())
    }
    fn render(&self) -> String {
        let mut result = self.raw[..1].to_owned();
        let mut count = 0;
        for index in 0..=self.rows.len() {
            if let Some(values) = self.insertions.get(&index) {
                for value in values {
                    if count > 0 {
                        result.push(',');
                    }
                    result.push_str(value);
                    count += 1;
                }
            }
            let Some(row) = self.rows.get(index) else {
                continue;
            };
            let value = match self.edits.get(&index) {
                Some(None) => continue,
                Some(Some(value)) => value.as_str(),
                None => row.value.get(),
            };
            result.push_str(row.before_comma);
            if count > 0 {
                result.push(',');
            }
            result.push_str(row.after_comma);
            result.push_str(value);
            count += 1;
        }
        result.push_str(self.tail);
        result
    }
}

pub(super) struct Object<'a> {
    keys: Vec<String>,
    sequence: Sequence<'a>,
    added: BTreeMap<String, String>,
}
impl<'a> Object<'a> {
    pub fn new(raw: &'a RawValue) -> Result<Self, String> {
        let object: Unique<'a> = serde_json::from_str(raw.get()).map_err(|e| e.to_string())?;
        let mut rows = object
            .0
            .into_iter()
            .map(|(key, value)| Ok((offset(raw.get(), value.get())?, key, value)))
            .collect::<Result<Vec<_>, String>>()?;
        rows.sort_by_key(|row| row.0);
        let keys = rows.iter().map(|row| row.1.clone()).collect();
        let sequence = Sequence::new(raw, rows.into_iter().map(|row| row.2).collect())?;
        Ok(Self {
            keys,
            sequence,
            added: BTreeMap::new(),
        })
    }
    pub fn get(&self, key: &str) -> Option<&'a RawValue> {
        self.keys
            .iter()
            .position(|k| k == key)
            .map(|index| self.sequence.rows[index].value)
    }
    pub fn entries(&self) -> impl Iterator<Item = (&str, &'a RawValue)> {
        self.keys
            .iter()
            .enumerate()
            .map(|(index, key)| (key.as_str(), self.sequence.rows[index].value))
    }
    pub fn set(&mut self, key: &str, value: Option<String>) -> Result<(), String> {
        if let Some(index) = self.keys.iter().position(|k| k == key) {
            self.sequence.set(index, value)
        } else {
            let value = value.ok_or("cannot remove an absent project property")?;
            parse(&value)?;
            if self.added.insert(key.into(), value).is_some() {
                return Err("repeated new project property".into());
            }
            Ok(())
        }
    }
    pub fn render(mut self) -> Result<String, String> {
        for (key, value) in self.added {
            let row = format!(
                "{}:{}",
                serde_json::to_string(&key).map_err(|e| e.to_string())?,
                value
            );
            self.sequence
                .insertions
                .entry(self.sequence.rows.len())
                .or_default()
                .push(row);
        }
        Ok(self.sequence.render())
    }
}

pub(super) struct Array<'a>(Sequence<'a>);
impl<'a> Array<'a> {
    pub fn new(raw: &'a RawValue) -> Result<Self, String> {
        let values: Vec<&RawValue> = serde_json::from_str(raw.get()).map_err(|e| e.to_string())?;
        Sequence::new(raw, values).map(Self)
    }
    pub fn values(&self) -> impl Iterator<Item = &'a RawValue> + '_ {
        self.0.rows.iter().map(|row| row.value)
    }
    pub fn len(&self) -> usize {
        self.0.rows.len()
    }
    pub fn set(&mut self, index: usize, value: Option<String>) -> Result<(), String> {
        self.0.set(index, value)
    }
    pub fn insert(&mut self, index: usize, value: String) -> Result<(), String> {
        if index > self.len() {
            return Err("project array insertion exceeds observed bounds".into());
        }
        parse(&value)?;
        self.0.insertions.entry(index).or_default().push(value);
        Ok(())
    }
    pub fn render(self) -> String {
        self.0.render()
    }
}

pub(super) fn replace(root: &str, old: &RawValue, value: &str) -> Result<String, String> {
    let start = offset(root, old.get())?;
    let mut result = root.to_owned();
    result.replace_range(start..start + old.get().len(), value);
    parse(&result)?;
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn unchanged_containers_retain_exact_tokens_keys_order_and_comma_trivia() {
        let text =
            "{ \"f\\u0075ture\" : [ 1e-0000002 \n , -0.0000 ], \n \"nested\": {\"q\":true} \n }";
        let object = Object::new(parse(text).unwrap()).unwrap();
        let array = Array::new(object.get("future").unwrap()).unwrap();
        assert_eq!(array.render(), "[ 1e-0000002 \n , -0.0000 ]");
        assert_eq!(object.render().unwrap(), text);
        for bad in ["{\"x\":1,\"x\":2}", "{\"a\":[{\"x\":1,\"\\u0078\":2}]}"] {
            assert!(parse(bad).is_err());
        }
    }
    #[test]
    fn removing_first_middle_last_and_all_entries_preserves_other_raw_values() {
        let source = "{ \"a\":1e1 \n , \"b\": {\"future\":-0.000}, \"c\":[true] \n }";
        for keys in [
            vec!["a"],
            vec!["b"],
            vec!["c"],
            vec!["a", "b"],
            vec!["b", "c"],
            vec!["a", "c"],
            vec!["a", "b", "c"],
        ] {
            let mut object = Object::new(parse(source).unwrap()).unwrap();
            for key in &keys {
                object.set(key, None).unwrap();
            }
            let output = object.render().unwrap();
            let remaining = Object::new(parse(&output).unwrap()).unwrap();
            let before = Object::new(parse(source).unwrap()).unwrap();
            for key in ["a", "b", "c"] {
                assert_eq!(
                    remaining.get(key).map(RawValue::get),
                    if keys.contains(&key) {
                        None
                    } else {
                        before.get(key).map(RawValue::get)
                    }
                );
            }
        }
    }
}
