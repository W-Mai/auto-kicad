mod connectivity;
mod delta;
use crate::exact_json::{Node, json_error, object, serialize_raw};
use crate::{DrcReport, ParseError, RawViolation, normalize_violation};
pub use delta::{CompleteDrcDelta, CompleteDrcDiagnostic, CompleteDrcExpectation};
use serde::Serialize;
use serde_json::value::RawValue;
use std::collections::{BTreeMap, BTreeSet};

const SECTIONS: [&str; 3] = ["violations", "unconnected_items", "schematic_parity"];

/// Complete CLI evidence. Construction validates the normalized view and every raw field.
#[derive(Clone, Debug, Serialize)]
pub struct CompleteDrcReport {
    normalized: DrcReport,
    #[serde(rename = "raw_json", serialize_with = "serialize_raw")]
    raw: Box<RawValue>,
}

impl CompleteDrcReport {
    pub fn parse(input: &str) -> Result<Self, ParseError> {
        if input.len() > 32 * 1024 * 1024 {
            return Err(ParseError::Schema("report exceeds 32 MiB bound".into()));
        }
        let raw: Box<RawValue> = serde_json::from_str(input).map_err(json_error)?;
        let tree = Node::parse(&raw, 0)?;
        let Node::Object(root) = tree else {
            return Err(ParseError::Schema("report must be an object".into()));
        };
        // Decode known diagnostics independently of the summary's intentionally narrow scope.
        let fields = object(&raw)?;
        for section in SECTIONS {
            if let Some(value) = fields.get(section) {
                let diagnostics: Vec<RawViolation> =
                    serde_json::from_str(value.get()).map_err(json_error)?;
                for diagnostic in diagnostics {
                    normalize_violation(diagnostic)?;
                }
            }
        }
        if root
            .get("date")
            .is_some_and(|value| !matches!(value, Node::String(_)))
        {
            return Err(ParseError::Schema("date must be a string".into()));
        }
        if !root.contains_key("violations") {
            return Err(ParseError::Schema("violations are required".into()));
        }
        Ok(Self {
            normalized: DrcReport::parse(input)?,
            raw,
        })
    }

    pub fn normalized(&self) -> &DrcReport {
        &self.normalized
    }

    pub fn raw(&self) -> &RawValue {
        &self.raw
    }

    /// Maps only identities already proven by the caller's native creation receipts.
    /// All other fields remain significant except source/date and diagnostic/item order.
    pub fn equivalent_to(
        &self,
        other: &Self,
        identities: &BTreeMap<String, String>,
    ) -> Result<bool, ParseError> {
        let mut left = Node::parse(&self.raw, 0)?;
        let mut right = Node::parse(&other.raw, 0)?;
        let targets: BTreeSet<_> = identities.values().collect();
        if identities.len() > 4096
            || targets.len() != identities.len()
            || identities.iter().any(|(a, b)| {
                [a, b]
                    .iter()
                    .any(|id| id.trim().is_empty() || id.len() > 128)
            })
        {
            return Err(ParseError::Schema("invalid bijective identity map".into()));
        }
        for id in left.item_ids() {
            if !identities.contains_key(&id) && targets.contains(&id) {
                return Err(ParseError::Schema(
                    "identity map collides with an original item".into(),
                ));
            }
        }
        left.canonicalize(identities);
        right.canonicalize(&BTreeMap::new());
        Ok(left == right)
    }
}

impl Node {
    fn item_ids(&self) -> BTreeSet<String> {
        let mut result = BTreeSet::new();
        if let Self::Object(root) = self {
            for section in SECTIONS {
                if let Some(Self::Array(diagnostics)) = root.get(section) {
                    for diagnostic in diagnostics {
                        if let Self::Object(diagnostic) = diagnostic
                            && let Some(Self::Array(items)) = diagnostic.get("items")
                        {
                            for item in items {
                                if let Self::Object(item) = item
                                    && let Some(Self::String(id)) = item.get("uuid")
                                {
                                    result.insert(id.clone());
                                }
                            }
                        }
                    }
                }
            }
        }
        result
    }

    fn canonicalize(&mut self, identities: &BTreeMap<String, String>) {
        let Self::Object(root) = self else { return };
        root.remove("source");
        root.remove("date");
        for section in SECTIONS {
            if let Some(Self::Array(diagnostics)) = root.get_mut(section) {
                for diagnostic in diagnostics.iter_mut() {
                    if let Self::Object(diagnostic) = diagnostic
                        && let Some(Self::Array(items)) = diagnostic.get_mut("items")
                    {
                        for item in items.iter_mut() {
                            if let Self::Object(item) = item
                                && let Some(Self::String(id)) = item.get_mut("uuid")
                                && let Some(mapped) = identities.get(id)
                            {
                                *id = mapped.clone();
                            }
                        }
                        items.sort();
                    }
                }
                diagnostics.sort();
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{Value, json};

    fn fixture() -> Value {
        let diagnostic = json!({"type":"clearance","severity":"error","description":"same",
            "items":[{"uuid":"created","description":"new","pos":{"x":1,"y":2}},
                     {"uuid":"original","description":"old","pos":{"x":3,"y":4}}]});
        json!({"$schema":"https://schemas.kicad.org/drc.v1.json","kicad_version":"10.0.7",
            "source":"preview","date":"today","violations":[diagnostic.clone()],
            "unconnected_items":[diagnostic.clone()],"schematic_parity":[diagnostic],
            "ignored_checks":["a"],"unknown":{"uuid":"created","value":1}})
    }
    fn parse(value: &Value) -> CompleteDrcReport {
        CompleteDrcReport::parse(&value.to_string()).unwrap()
    }

    #[test]
    fn complete_correspondence_preserves_all_sections_and_limits_identity_projection() {
        let original = fixture();
        let baseline = parse(&original);
        let map = BTreeMap::from([("created".into(), "source-created".into())]);
        let mut target = original.clone();
        target["source"] = json!("source");
        target["date"] = json!("tomorrow");
        for section in SECTIONS {
            target[section][0]["items"][0]["uuid"] = json!("source-created");
            target[section][0]["items"]
                .as_array_mut()
                .unwrap()
                .reverse();
        }
        assert!(baseline.equivalent_to(&parse(&target), &map).unwrap());
        assert!(
            !baseline
                .equivalent_to(&parse(&target), &BTreeMap::new())
                .unwrap()
        );
        for path in [
            "/unconnected_items/0/items/0/uuid",
            "/schematic_parity/0/description",
            "/violations/0/severity",
            "/ignored_checks/0",
            "/unknown/uuid",
        ] {
            let mut changed = target.clone();
            *changed.pointer_mut(path).unwrap() = json!("changed");
            assert!(
                !baseline.equivalent_to(&parse(&changed), &map).unwrap(),
                "{path}"
            );
        }
        let mut changed = target.clone();
        changed["unconnected_items"][0]["items"][0]["pos"]["x"] = json!(5);
        assert!(!baseline.equivalent_to(&parse(&changed), &map).unwrap());
        let mut collision = map.clone();
        collision.insert("created".into(), "original".into());
        assert!(baseline.equivalent_to(&parse(&target), &collision).is_err());
        collision.insert("other".into(), "original".into());
        assert!(baseline.equivalent_to(&parse(&target), &collision).is_err());
        assert_eq!(baseline.normalized().unconnected_items, 1);
        assert_eq!(
            serde_json::to_value(&baseline).unwrap()["raw_json"],
            original.to_string()
        );
    }

    #[test]
    fn complete_parser_rejects_ambiguous_json_and_retains_exact_unknown_numbers() {
        let input = fixture().to_string();
        assert!(
            CompleteDrcReport::parse(&input.replace("\"value\":1", "\"value\":1,\"value\":2"))
                .is_err()
        );
        assert!(
            CompleteDrcReport::parse(
                &input.replace("\"value\":1", "\"value\":1,\"val\\u0075e\":2")
            )
            .is_err()
        );
        for section in SECTIONS {
            let mut malformed = fixture();
            malformed[section] = json!([{"type":"bad"}]);
            assert!(CompleteDrcReport::parse(&malformed.to_string()).is_err());
        }
        let huge = input.replace(
            "\"value\":1",
            "\"value\":123456789012345678901234567890.123456789",
        );
        let other = huge.replace("890.123456789", "890.123456788");
        let a = CompleteDrcReport::parse(&huge).unwrap();
        let b = CompleteDrcReport::parse(&other).unwrap();
        let durable = serde_json::to_value(&a).unwrap();
        let restored = CompleteDrcReport::parse(durable["raw_json"].as_str().unwrap()).unwrap();
        assert!(a.equivalent_to(&restored, &BTreeMap::new()).unwrap());
        assert!(
            durable["raw_json"]
                .as_str()
                .unwrap()
                .contains("123456789012345678901234567890.123456789")
        );
        assert!(!a.equivalent_to(&b, &BTreeMap::new()).unwrap());
        assert!(
            a.raw()
                .get()
                .contains("123456789012345678901234567890.123456789")
        );
        let reserved = input.replace(
            "\"value\":1",
            "\"value\":{\"$serde_json::private::Number\":\"123\"}",
        );
        assert!(
            CompleteDrcReport::parse(&reserved)
                .unwrap()
                .raw()
                .get()
                .contains("$serde_json::private::Number")
        );
        let nested = input.replace(
            "\"value\":1",
            &format!("\"value\":{}0{}", "[".repeat(130), "]".repeat(130)),
        );
        assert!(CompleteDrcReport::parse(&nested).is_err());
    }
}
