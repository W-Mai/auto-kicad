//! Exact diagnostic deltas for explicitly reviewed saved-board changes.
use super::*;
use crate::exact_json::node_json;
use serde::Deserialize;

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CompleteDrcDiagnostic {
    pub section: String,
    /// JSON text retains every field and the original numeric tokens.
    pub diagnostic_json: String,
}

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CompleteDrcExpectation {
    #[serde(default)]
    pub introduced: Vec<CompleteDrcDiagnostic>,
    #[serde(default)]
    pub resolved: Vec<CompleteDrcDiagnostic>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct CompleteDrcDelta {
    pub metadata_changed: bool,
    pub introduced: Vec<CompleteDrcDiagnostic>,
    pub resolved: Vec<CompleteDrcDiagnostic>,
    pub unchanged: usize,
}

type Key = (String, Node);
type Multiset = BTreeMap<Key, usize>;

fn diagnostic_node(raw: &RawValue) -> Result<Node, ParseError> {
    let known: RawViolation = serde_json::from_str(raw.get()).map_err(json_error)?;
    normalize_violation(known)?;
    let mut node = Node::parse(raw, 0)?;
    if let Node::Object(fields) = &mut node
        && let Some(Node::Array(items)) = fields.get_mut("items")
    {
        items.sort();
    }
    Ok(node)
}

fn parts(report: &CompleteDrcReport) -> Result<(Node, Multiset), ParseError> {
    let mut root = object(report.raw())?;
    root.remove("source");
    root.remove("date");
    let mut diagnostics = Multiset::new();
    let mut present = BTreeMap::new();
    for section in SECTIONS {
        let entries = root.remove(section);
        present.insert(
            section.to_owned(),
            Node::Scalar(entries.is_some().to_string()),
        );
        if let Some(entries) = entries {
            let entries: Vec<Box<RawValue>> =
                serde_json::from_str(entries.get()).map_err(json_error)?;
            for entry in entries {
                *diagnostics
                    .entry((section.into(), diagnostic_node(&entry)?))
                    .or_default() += 1;
            }
        }
    }
    let header = Node::Object(
        root.into_iter()
            .map(|(k, v)| Ok((k, Node::parse(&v, 0)?)))
            .collect::<Result<_, ParseError>>()?,
    );
    Ok((
        Node::Array(vec![header, Node::Object(present)]),
        diagnostics,
    ))
}

fn expected(rows: &[CompleteDrcDiagnostic]) -> Result<Multiset, ParseError> {
    let mut total = 0usize;
    if rows.len() > 4096 {
        return Err(ParseError::Schema(
            "DRC expectation exceeds diagnostic count bound".into(),
        ));
    }
    let mut result = Multiset::new();
    for row in rows {
        total = total
            .checked_add(row.diagnostic_json.len())
            .ok_or_else(|| ParseError::Schema("DRC expectation byte overflow".into()))?;
        if total > 2 * 1024 * 1024 || !SECTIONS.contains(&row.section.as_str()) {
            return Err(ParseError::Schema(
                "DRC expectation exceeds bounds or has an unknown section".into(),
            ));
        }
        let raw: Box<RawValue> = serde_json::from_str(&row.diagnostic_json).map_err(json_error)?;
        *result
            .entry((row.section.clone(), diagnostic_node(&raw)?))
            .or_default() += 1;
    }
    Ok(result)
}

impl CompleteDrcReport {
    /// Validate the complete fixed native capture contract independently of report counts.
    pub fn require_complete_capture(&self, version: &str, source: &str) -> Result<(), ParseError> {
        let fields = object(self.raw())?;
        for (key, value) in [
            ("$schema", "https://schemas.kicad.org/drc.v1.json"),
            ("coordinate_units", "mm"),
            ("kicad_version", version),
            ("source", source),
        ] {
            if fields.get(key).map(|raw| Node::parse(raw, 0)).transpose()?
                != Some(Node::String(value.into()))
            {
                return Err(ParseError::Schema(format!(
                    "complete DRC capture differs: {key}"
                )));
            }
        }
        let severities = fields
            .get("included_severities")
            .ok_or_else(|| ParseError::Schema("missing complete DRC severity evidence".into()))?;
        let mut severities = Node::parse(severities, 0)?;
        if let Node::Array(values) = &mut severities {
            values.sort();
        }
        if severities
            != Node::Array(vec![
                Node::String("error".into()),
                Node::String("exclusion".into()),
                Node::String("warning".into()),
            ])
        {
            return Err(ParseError::Schema(
                "complete DRC requires all severities including exclusions".into(),
            ));
        }
        if !fields
            .get("ignored_checks")
            .map(|raw| Node::parse(raw, 0))
            .transpose()?
            .is_some_and(|node| matches!(node, Node::Array(_)))
        {
            return Err(ParseError::Schema(
                "complete DRC requires ignored-check metadata".into(),
            ));
        }
        for section in SECTIONS {
            if !fields
                .get(section)
                .map(|raw| Node::parse(raw, 0))
                .transpose()?
                .is_some_and(|node| matches!(node, Node::Array(_)))
            {
                return Err(ParseError::Schema(format!(
                    "missing complete DRC section: {section}"
                )));
            }
        }
        Ok(())
    }

    /// Preserve unknown metadata, complete diagnostic fields, exact numbers and multiplicity.
    pub fn compare_complete(&self, after: &Self) -> Result<CompleteDrcDelta, ParseError> {
        let (before_header, before) = parts(self)?;
        let (after_header, after) = parts(after)?;
        let mut delta = CompleteDrcDelta {
            metadata_changed: before_header != after_header,
            introduced: vec![],
            resolved: vec![],
            unchanged: 0,
        };
        for key in before.keys().chain(after.keys()).collect::<BTreeSet<_>>() {
            let old = before.get(key).copied().unwrap_or(0);
            let new = after.get(key).copied().unwrap_or(0);
            delta.unchanged += old.min(new);
            let row = CompleteDrcDiagnostic {
                section: key.0.clone(),
                diagnostic_json: node_json(&key.1),
            };
            delta
                .resolved
                .extend(std::iter::repeat_n(row.clone(), old.saturating_sub(new)));
            delta
                .introduced
                .extend(std::iter::repeat_n(row, new.saturating_sub(old)));
        }
        Ok(delta)
    }
}

impl CompleteDrcDelta {
    /// Exact expectation matching is separate from user approval and write authority.
    pub fn match_expectation(
        &self,
        expectation: &CompleteDrcExpectation,
    ) -> Result<(), ParseError> {
        let introduced = expected(&expectation.introduced)?;
        let resolved = expected(&expectation.resolved)?;
        if self.metadata_changed
            || introduced != expected(&self.introduced)?
            || resolved != expected(&self.resolved)?
        {
            return Err(ParseError::Schema(
                "complete DRC differs from the explicit diagnostic expectation".into(),
            ));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn fixture() -> String {
        r#"{"$schema":"https://schemas.kicad.org/drc.v1.json","coordinate_units":"mm","date":"today","source":"sync.kicad_pcb","kicad_version":"10.0.7","included_severities":["error","warning","exclusion"],"ignored_checks":[],"violations":[],"unconnected_items":[],"schematic_parity":[{"type":"footprint_symbol_mismatch","severity":"warning","description":"full","items":[{"uuid":"pad","description":"pin","pos":{"x":0.12345678901234567890,"y":0}}],"unknown":1e-20}]}"#.into()
    }
    #[test]
    fn exact_delta_retains_sections_unknowns_multiplicity_and_numeric_tokens() {
        let raw = fixture();
        let before = CompleteDrcReport::parse(&raw).unwrap();
        before
            .require_complete_capture("10.0.7", "sync.kicad_pcb")
            .unwrap();
        before
            .compare_complete(&before)
            .unwrap()
            .match_expectation(&CompleteDrcExpectation::default())
            .unwrap();
        for changed in [
            raw.replace("full", "changed"),
            raw.replace("67890", "67891"),
            raw.replace("1e-20", "1.0e-20"),
            raw.replace("schematic_parity", "unrecognized_section"),
        ] {
            let after = CompleteDrcReport::parse(&changed).unwrap();
            let delta = before.compare_complete(&after).unwrap();
            assert!(
                delta
                    .match_expectation(&CompleteDrcExpectation::default())
                    .is_err()
            );
            let expectation = CompleteDrcExpectation {
                introduced: delta.introduced.clone(),
                resolved: delta.resolved.clone(),
            };
            assert_eq!(
                delta.match_expectation(&expectation).is_ok(),
                !delta.metadata_changed
            );
            if !expectation.resolved.is_empty() {
                assert!(
                    expectation.resolved[0]
                        .diagnostic_json
                        .contains("0.12345678901234567890")
                );
            }
        }
        let mut fields = object(before.raw()).unwrap();
        let rows: Vec<Box<RawValue>> =
            serde_json::from_str(fields["schematic_parity"].get()).unwrap();
        fields.insert(
            "schematic_parity".into(),
            RawValue::from_string(format!("[{},{}]", rows[0], rows[0])).unwrap(),
        );
        let twice = CompleteDrcReport::parse(&serde_json::to_string(&fields).unwrap()).unwrap();
        let delta = before.compare_complete(&twice).unwrap();
        assert_eq!(delta.unchanged, 1);
        assert_eq!(delta.introduced.len(), 1);
        assert!(
            delta
                .match_expectation(&CompleteDrcExpectation::default())
                .is_err()
        );
        let expected = CompleteDrcExpectation {
            introduced: delta.introduced.clone(),
            resolved: vec![],
        };
        delta.match_expectation(&expected).unwrap();
        for changed in [
            raw.replace("10.0.7", "11.0.0"),
            raw.replace("\"warning\",\"exclusion\"", "\"warning\""),
            raw.replace("\"schematic_parity\":", "\"unknown\":"),
            raw.replace("\"mm\"", "\"mils\""),
        ] {
            assert!(
                CompleteDrcReport::parse(&changed)
                    .unwrap()
                    .require_complete_capture("10.0.7", "sync.kicad_pcb")
                    .is_err()
            );
        }
    }
}
