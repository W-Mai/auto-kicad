//! Complete native ERC diagnostics with exact, sheet-aware multiset comparison.
use crate::{
    ParseError,
    exact_json::{Node, serialize_raw},
};
use serde::Serialize;
use serde_json::value::RawValue;
use std::{
    collections::{BTreeMap, BTreeSet},
    fmt,
};

const MAX_REPORT: usize = 32 * 1024 * 1024;
const MAX_SHEETS: usize = 4096;
const MAX_DIAGNOSTICS: usize = 100_000;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ErcError(String);
impl fmt::Display for ErcError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "invalid ERC report: {}", self.0)
    }
}
impl std::error::Error for ErcError {}
impl From<ParseError> for ErcError {
    fn from(error: ParseError) -> Self {
        match error {
            ParseError::Json(message) | ParseError::Schema(message) => Self(message),
        }
    }
}
fn invalid(message: &str) -> ErcError {
    ErcError(message.into())
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct ErcSummary {
    pub kicad_version: String,
    pub source: String,
    pub sheets: usize,
    pub errors: usize,
    pub warnings: usize,
    pub exclusions: usize,
}

/// Raw JSON is retained as a string to survive intermediate JSON value serialization.
/// Parsing a report does not bind it to source files, libraries or a CLI execution.
#[derive(Clone, Debug, Serialize)]
pub struct CompleteErcReport {
    summary: ErcSummary,
    #[serde(rename = "raw_json", serialize_with = "serialize_raw")]
    raw: Box<RawValue>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct ErcDiagnostic {
    pub sheet_path: String,
    pub sheet_uuid_path: String,
    /// Structural JSON with exact numeric tokens and every diagnostic field.
    pub diagnostic_json: String,
}

/// Header changes remain explicit even when both reports contain zero diagnostics.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct ErcDelta {
    pub metadata_changed: bool,
    pub sheet_headers_changed: bool,
    pub introduced: Vec<ErcDiagnostic>,
    pub resolved: Vec<ErcDiagnostic>,
    pub unchanged: usize,
}

fn object(node: &Node) -> Result<&BTreeMap<String, Node>, ErcError> {
    match node {
        Node::Object(value) => Ok(value),
        _ => Err(invalid("expected object")),
    }
}
fn array(node: &Node) -> Result<&[Node], ErcError> {
    match node {
        Node::Array(value) => Ok(value),
        _ => Err(invalid("expected array")),
    }
}
fn field<'a>(node: &'a BTreeMap<String, Node>, key: &str) -> Result<&'a Node, ErcError> {
    node.get(key)
        .ok_or_else(|| invalid(&format!("missing {key}")))
}
fn string(node: &Node) -> Result<&str, ErcError> {
    match node {
        Node::String(value) => Ok(value),
        _ => Err(invalid("expected string")),
    }
}
fn required<'a>(node: &'a BTreeMap<String, Node>, key: &str) -> Result<&'a str, ErcError> {
    let value = string(field(node, key)?)?;
    if value.is_empty() || value.len() > 65536 {
        return Err(invalid("empty or oversized identity field"));
    }
    Ok(value)
}

fn validate(tree: &Node) -> Result<ErcSummary, ErcError> {
    let root = object(tree)?;
    if required(root, "$schema")? != "https://schemas.kicad.org/erc.v1.json"
        || required(root, "coordinate_units")? != "mm"
    {
        return Err(invalid("unsupported schema or coordinate units"));
    }
    required(root, "date")?;
    let included = array(field(root, "included_severities")?)?;
    let severities = included
        .iter()
        .map(string)
        .collect::<Result<BTreeSet<_>, _>>()?;
    if included.len() != 3 || severities != BTreeSet::from(["error", "warning", "exclusion"]) {
        return Err(invalid(
            "complete ERC requires all severities including exclusions",
        ));
    }
    for check in array(field(root, "ignored_checks")?)? {
        let check = object(check)?;
        required(check, "key")?;
        string(field(check, "description")?)?;
    }
    let sheets = array(field(root, "sheets")?)?;
    if sheets.is_empty() || sheets.len() > MAX_SHEETS {
        return Err(invalid("sheet count exceeds bounds"));
    }
    let mut summary = ErcSummary {
        kicad_version: required(root, "kicad_version")?.into(),
        source: required(root, "source")?.into(),
        sheets: sheets.len(),
        errors: 0,
        warnings: 0,
        exclusions: 0,
    };
    let mut identities = BTreeSet::new();
    let mut total = 0usize;
    for sheet in sheets {
        let sheet = object(sheet)?;
        required(sheet, "path")?;
        if !identities.insert(required(sheet, "uuid_path")?) {
            return Err(invalid("duplicate sheet identity"));
        }
        for diagnostic in array(field(sheet, "violations")?)? {
            total += 1;
            if total > MAX_DIAGNOSTICS {
                return Err(invalid("diagnostic count exceeds bound"));
            }
            let diagnostic = object(diagnostic)?;
            required(diagnostic, "type")?;
            string(field(diagnostic, "description")?)?;
            let severity = required(diagnostic, "severity")?;
            if !matches!(severity, "error" | "warning") {
                return Err(invalid("unsupported severity"));
            }
            let excluded = match diagnostic.get("excluded") {
                None => false,
                Some(Node::Scalar(value)) if value == "true" => true,
                Some(Node::Scalar(value)) if value == "false" => false,
                _ => return Err(invalid("excluded must be boolean")),
            };
            if let Some(comment) = diagnostic.get("comment") {
                string(comment)?;
            }
            // Native ERC retains the original severity when an item is excluded.
            if excluded {
                summary.exclusions += 1;
            } else if severity == "error" {
                summary.errors += 1;
            } else {
                summary.warnings += 1;
            }
            let items = array(field(diagnostic, "items")?)?;
            if items.len() > 4096 {
                return Err(invalid("diagnostic item count exceeds bound"));
            }
            for item in items {
                let item = object(item)?;
                string(field(item, "description")?)?;
                if item.contains_key("uuid") {
                    required(item, "uuid")?;
                }
                if let Some(position) = item.get("pos") {
                    let position = object(position)?;
                    for axis in ["x", "y"] {
                        let Node::Scalar(token) = field(position, axis)? else {
                            return Err(invalid("coordinate must be numeric"));
                        };
                        if !token.parse::<f64>().is_ok_and(f64::is_finite) {
                            return Err(invalid("coordinate must be finite"));
                        }
                    }
                }
            }
        }
    }
    Ok(summary)
}

// Emit exact numeric tokens directly; converting through serde_json::Value would round them.
fn node_json(node: &Node) -> String {
    fn append(node: &Node, out: &mut String) {
        match node {
            Node::Scalar(value) => out.push_str(value),
            Node::String(value) => {
                out.push_str(&serde_json::to_string(value).expect("strings serialize"))
            }
            Node::Array(values) => {
                out.push('[');
                for (i, value) in values.iter().enumerate() {
                    if i > 0 {
                        out.push(',');
                    }
                    append(value, out);
                }
                out.push(']');
            }
            Node::Object(values) => {
                out.push('{');
                for (i, (key, value)) in values.iter().enumerate() {
                    if i > 0 {
                        out.push(',');
                    }
                    out.push_str(&serde_json::to_string(key).expect("keys serialize"));
                    out.push(':');
                    append(value, out);
                }
                out.push('}');
            }
        }
    }
    let mut out = String::new();
    append(node, &mut out);
    out
}

struct Parts {
    metadata: Node,
    headers: BTreeSet<Node>,
    diagnostics: BTreeMap<(String, Node), Vec<ErcDiagnostic>>,
}
fn parts(raw: &RawValue) -> Result<Parts, ErcError> {
    let Node::Object(mut root) = Node::parse(raw, 0)? else {
        return Err(invalid("expected report object"));
    };
    root.remove("source");
    root.remove("date");
    let Node::Array(sheets) = root
        .remove("sheets")
        .ok_or_else(|| invalid("missing sheets"))?
    else {
        return Err(invalid("expected sheets"));
    };
    let mut result = Parts {
        metadata: Node::Object(root),
        headers: BTreeSet::new(),
        diagnostics: BTreeMap::new(),
    };
    for sheet in sheets {
        let Node::Object(mut header) = sheet else {
            return Err(invalid("expected sheet"));
        };
        let path = required(&header, "path")?.to_owned();
        let uuid_path = required(&header, "uuid_path")?.to_owned();
        let Node::Array(diagnostics) = header
            .remove("violations")
            .ok_or_else(|| invalid("missing violations"))?
        else {
            return Err(invalid("expected violations"));
        };
        let header = Node::Object(header);
        result.headers.insert(header);
        for mut diagnostic in diagnostics {
            if let Node::Object(diagnostic) = &mut diagnostic
                && let Some(Node::Array(items)) = diagnostic.get_mut("items")
            {
                items.sort();
            }
            let entry = ErcDiagnostic {
                sheet_path: path.clone(),
                sheet_uuid_path: uuid_path.clone(),
                diagnostic_json: node_json(&diagnostic),
            };
            result
                .diagnostics
                .entry((uuid_path.clone(), diagnostic))
                .or_default()
                .push(entry);
        }
    }
    Ok(result)
}

impl CompleteErcReport {
    pub fn parse(input: &str) -> Result<Self, ErcError> {
        if input.len() > MAX_REPORT {
            return Err(invalid("report exceeds 32 MiB"));
        }
        let raw: Box<RawValue> =
            serde_json::from_str(input).map_err(|e| invalid(&e.to_string()))?;
        let tree = Node::parse(&raw, 0)?;
        let summary = validate(&tree)?;
        Ok(Self { summary, raw })
    }
    pub fn summary(&self) -> &ErcSummary {
        &self.summary
    }
    pub fn raw(&self) -> &RawValue {
        &self.raw
    }

    /// Compare exact report structure except top-level run metadata and known diagnostic order.
    pub fn equivalent_to(&self, other: &Self) -> Result<bool, ErcError> {
        let delta = self.compare(other)?;
        Ok(!delta.metadata_changed
            && !delta.sheet_headers_changed
            && delta.introduced.is_empty()
            && delta.resolved.is_empty())
    }
    pub fn compare(&self, after: &Self) -> Result<ErcDelta, ErcError> {
        let before = parts(&self.raw)?;
        let mut after = parts(&after.raw)?;
        let mut delta = ErcDelta {
            metadata_changed: before.metadata != after.metadata,
            sheet_headers_changed: before.headers != after.headers,
            introduced: vec![],
            resolved: vec![],
            unchanged: 0,
        };
        for (key, old) in before.diagnostics {
            let new = after.diagnostics.remove(&key).unwrap_or_default();
            let same = old.len().min(new.len());
            delta.unchanged += same;
            delta.resolved.extend(old.into_iter().skip(same));
            delta.introduced.extend(new.into_iter().skip(same));
        }
        delta
            .introduced
            .extend(after.diagnostics.into_values().flatten());
        Ok(delta)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{Value, json};
    fn fixture() -> Value {
        json!({"$schema":"https://schemas.kicad.org/erc.v1.json","kicad_version":"10.0.7","coordinate_units":"mm","date":"today","source":"root.kicad_sch","included_severities":["error","warning","exclusion"],"ignored_checks":[{"key":"ignored","description":"native policy"}],"unknown":{"source":"nested-source","number":1},"sheets":[
            {"path":"/","uuid_path":"/root","unknown_sheet":"preserved","violations":[{"type":"pin_not_connected","severity":"warning","description":"unconnected","items":[{"uuid":"one","description":"A","pos":{"x":1.0,"y":2.0},"unknown_item":true},{"uuid":"two","description":"B","pos":{"x":3.0,"y":4.0}}],"unknown_diagnostic":{"value":1}}]},
            {"path":"/Child/","uuid_path":"/root/child","violations":[{"type":"excluded_check","severity":"error","excluded":true,"comment":"retained reason","description":"retained exclusion","items":[]}]}
        ]})
    }
    fn parse(value: &Value) -> CompleteErcReport {
        CompleteErcReport::parse(&value.to_string()).unwrap()
    }

    #[test]
    fn complete_structure_and_multiplicity_survive_ordering_normalization() {
        let before = fixture();
        let baseline = parse(&before);
        let mut after = before.clone();
        after["source"] = json!("copy.kicad_sch");
        after["date"] = json!("tomorrow");
        after["sheets"][0]["violations"][0]["items"]
            .as_array_mut()
            .unwrap()
            .reverse();
        after["sheets"].as_array_mut().unwrap().reverse();
        assert!(baseline.equivalent_to(&parse(&after)).unwrap());
        assert_eq!(baseline.compare(&parse(&after)).unwrap().unchanged, 2);
        let duplicate = after["sheets"][1]["violations"][0].clone();
        after["sheets"][1]["violations"]
            .as_array_mut()
            .unwrap()
            .push(duplicate);
        let delta = baseline.compare(&parse(&after)).unwrap();
        assert_eq!(delta.introduced.len(), 1);
        assert_eq!(delta.unchanged, 2);
        assert!(delta.resolved.is_empty());
        assert!(!baseline.equivalent_to(&parse(&after)).unwrap());
        assert_eq!(baseline.summary().exclusions, 1);
        assert_eq!(baseline.summary().errors, 0);
        assert_eq!(baseline.summary().warnings, 1);
        let mut included = before.clone();
        included["sheets"][1]["violations"][0]["excluded"] = json!(false);
        assert_eq!(parse(&included).summary().errors, 1);
        assert_eq!(parse(&included).summary().exclusions, 0);
        assert!(!baseline.equivalent_to(&parse(&included)).unwrap());
    }

    #[test]
    fn every_unknown_field_identity_and_policy_change_remains_significant() {
        let before = fixture();
        let baseline = parse(&before);
        for path in [
            "/unknown/source",
            "/ignored_checks/0/description",
            "/sheets/1/violations/0/comment",
            "/sheets/0/unknown_sheet",
            "/sheets/0/path",
            "/sheets/0/uuid_path",
            "/sheets/0/violations/0/unknown_diagnostic/value",
            "/sheets/0/violations/0/items/0/unknown_item",
            "/sheets/0/violations/0/items/0/uuid",
            "/sheets/0/violations/0/description",
        ] {
            let mut changed = before.clone();
            *changed.pointer_mut(path).unwrap() = json!("changed");
            assert!(!baseline.equivalent_to(&parse(&changed)).unwrap(), "{path}");
        }
        let mut changed = before.clone();
        changed["sheets"]
            .as_array_mut()
            .unwrap()
            .push(json!({"path":"/Empty/","uuid_path":"/root/empty","violations":[]}));
        let delta = baseline.compare(&parse(&changed)).unwrap();
        assert!(delta.sheet_headers_changed);
        assert!(delta.introduced.is_empty());
        assert!(!baseline.equivalent_to(&parse(&changed)).unwrap());
    }

    #[test]
    fn exact_numeric_tokens_survive_durable_serialization_and_delta() {
        let source = fixture().to_string();
        assert!(source.contains("\"x\":1.0"));
        let source = source.replace("\"x\":1.0", "\"x\":1.000000000000000001");
        let before = CompleteErcReport::parse(&source).unwrap();
        let after = CompleteErcReport::parse(
            &source.replace("1.000000000000000001", "1.000000000000000002"),
        )
        .unwrap();
        let encoded = serde_json::to_value(&before).unwrap();
        assert_eq!(encoded["raw_json"].as_str().unwrap(), source);
        let restored = CompleteErcReport::parse(encoded["raw_json"].as_str().unwrap()).unwrap();
        assert!(before.equivalent_to(&restored).unwrap());
        let delta = before.compare(&after).unwrap();
        assert_eq!(delta.introduced.len(), 1);
        assert_eq!(delta.resolved.len(), 1);
        assert!(
            delta.introduced[0]
                .diagnostic_json
                .contains("1.000000000000000002")
        );
        assert!(
            delta.resolved[0]
                .diagnostic_json
                .contains("1.000000000000000001")
        );
    }

    #[test]
    fn partial_malformed_duplicate_and_oversized_reports_reject() {
        let before = fixture();
        for field in [
            "sheets",
            "ignored_checks",
            "included_severities",
            "date",
            "source",
            "coordinate_units",
        ] {
            let mut changed = before.clone();
            changed.as_object_mut().unwrap().remove(field);
            assert!(
                CompleteErcReport::parse(&changed.to_string()).is_err(),
                "{field}"
            );
        }
        for (pointer, value) in [
            ("/included_severities", json!(["error"])),
            ("/sheets/1/uuid_path", json!("/root")),
            ("/sheets/0/violations/0/severity", json!("ignored")),
            ("/sheets/0/violations/0/items/0/pos/x", json!(true)),
            ("/coordinate_units", json!("in")),
            ("/sheets/1/violations/0/excluded", json!("true")),
            ("/sheets/1/violations/0/comment", json!(false)),
        ] {
            let mut changed = before.clone();
            *changed.pointer_mut(pointer).unwrap() = value;
            assert!(
                CompleteErcReport::parse(&changed.to_string()).is_err(),
                "{pointer}"
            );
        }
        let duplicate = before.to_string().replacen(
            "\"unknown_item\":true",
            "\"unknown_item\":true,\"unknown_item\":false",
            1,
        );
        assert!(CompleteErcReport::parse(&duplicate).is_err());
        assert!(CompleteErcReport::parse(&" ".repeat(MAX_REPORT + 1)).is_err());
    }
}
