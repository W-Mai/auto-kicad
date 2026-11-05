//! Exact expected electrical and diagnostic changes, without human approval or publication.
use super::*;
use kicad_checks::erc::{ErcDelta, ErcDiagnostic};
use kicad_schematic::geometry::{Coordinate, Point};
use kicad_schematic::probes::ConnectivityNet;
use kiutils_sexpr::{Atom, Node, parse_one};

const MAX_PINS: usize = 1_000_000;
const MAX_EXPECTATION_BYTES: usize = 128 * 1024 * 1024;

/// Complete changed nets, isolated electrical no-connects and explicit source-pin births/deaths. Every field defaults to empty, preserving the complete baseline.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct ConnectivityExpectation {
    pub removed_nets: Vec<ConnectivityNet>,
    pub added_nets: Vec<ConnectivityNet>,
    pub removed_isolated: Vec<ConnectivityPin>,
    pub added_isolated: Vec<ConnectivityPin>,
    pub removed_pins: Vec<ConnectivityPin>,
    pub added_pins: Vec<ConnectivityPin>,
}

/// Physical markers are independent of isolated electrical-no-connect pin definitions.
#[derive(Clone, Debug, Eq, PartialEq, Ord, PartialOrd)]
pub struct NoConnectMarker {
    pub file: String,
    pub uuid: String,
    pub position: Point,
    pub definition_digest: String,
}

/// Exact sheet-aware diagnostics and complete header transitions. Metadata changes reject.
#[derive(
    Clone, Debug, Default, Eq, PartialEq, serde::Serialize, serde::Deserialize, schemars::JsonSchema,
)]
#[serde(deny_unknown_fields)]
pub struct ErcExpectation {
    pub removed_sheet_headers: Vec<String>,
    pub added_sheet_headers: Vec<String>,
    pub introduced: Vec<ErcDiagnostic>,
    pub resolved: Vec<ErcDiagnostic>,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct SchematicEditExpectation {
    pub connectivity: ConnectivityExpectation,
    pub removed_markers: Vec<NoConnectMarker>,
    pub added_markers: Vec<NoConnectMarker>,
    pub erc: ErcExpectation,
}

/// Constructed only after exact expectation matching. This is not a user approval or a writer.
#[derive(Clone)]
pub struct MatchedSchematicPreview {
    preview: Arc<SchematicEditPreview>,
    evidence: Arc<Value>,
    digest: String,
}

impl MatchedSchematicPreview {
    pub(super) fn preview(&self) -> &Arc<SchematicEditPreview> {
        &self.preview
    }
    pub fn verify(&self) -> Result<(), String> {
        self.preview.verify()
    }
    pub fn digest(&self) -> &str {
        &self.digest
    }
    pub fn evidence(&self) -> &Value {
        &self.evidence
    }
}

impl SchematicEditPreview {
    // Physical marker state includes selected existing child files before attachment.
    fn captured_schematic_sources(&self) -> Result<BTreeMap<String, &[u8]>, String> {
        let mut sources = self
            .source
            .inner
            .parsed
            .files
            .iter()
            .map(|file| {
                let bytes = self
                    .source
                    .inner
                    .source
                    .files
                    .get(
                        &self
                            .source
                            .inner
                            .policy
                            .project
                            .join(&file.artifact.relative_path),
                    )
                    .and_then(|input| input.bytes.as_deref())
                    .ok_or("missing captured schematic source")?;
                Ok((file.artifact.relative_path.clone(), bytes))
            })
            .collect::<Result<BTreeMap<_, _>, String>>()?;
        for file in &self.additional.schematics {
            let bytes = self
                .additional
                .inputs
                .files
                .get(&self.source.inner.policy.project.join(file))
                .and_then(|input| input.bytes.as_deref())
                .ok_or("missing captured child marker source")?;
            if sources.insert(file.clone(), bytes).is_some() {
                return Err("repeated captured marker source".into());
            }
        }
        Ok(sources)
    }
    /// Require the complete candidate to equal an explicit change applied to the baseline.
    pub fn match_expectations(
        self: &Arc<Self>,
        expected: &SchematicEditExpectation,
    ) -> Result<MatchedSchematicPreview, String> {
        self.verify()?;
        let before = State::new(
            self.connectivity_before().nets(),
            self.connectivity_before().isolated_no_connect(),
        )?;
        let after = State::new(
            self.connectivity_after().nets(),
            self.connectivity_after().isolated_no_connect(),
        )?;
        let connectivity = match_connectivity(&before, &after, &expected.connectivity)?;
        let passive_only = self.candidate.changes().iter().all(|change| {
            matches!(
                change.action,
                EditAction::SetProperty { .. }
                    | EditAction::SetReference { .. }
                    | EditAction::AssignFootprint { .. }
                    | EditAction::SetFlags { .. }
            )
        });
        if passive_only
            && (before.partitions() != after.partitions() || before.isolated != after.isolated)
        {
            return Err("property/annotation/flag/assignment edits must preserve pin partitions and classes".into());
        }
        let sources = self.captured_schematic_sources()?;
        let before_markers = markers(sources.into_iter())?;
        let after_markers = markers(
            self.candidate
                .files()
                .iter()
                .map(|(file, bytes)| (file.clone(), bytes.as_slice())),
        )?;
        let marker_changes = match_markers(
            &before_markers,
            &after_markers,
            &expected.removed_markers,
            &expected.added_markers,
        )?;
        let erc = match_erc(&self.erc_delta, &expected.erc)?;
        let evidence = serde_json::json!({
            "schema_version": 1,
            "native_preview_digest": self.digest,
            "expected_connectivity": connectivity,
            "expected_markers": marker_changes,
            "expected_erc": erc,
            "user_approved": false,
            "publication_authorized": false,
        });
        let encoded = serde_json::to_vec(&evidence).map_err(|error| error.to_string())?;
        if encoded.len() > MAX_EXPECTATION_BYTES {
            return Err("expected schematic delta exceeds byte bound".into());
        }
        self.verify()?;
        Ok(MatchedSchematicPreview {
            preview: Arc::clone(self),
            digest: hash(&encoded),
            evidence: Arc::new(evidence),
        })
    }

    /// Complete physical marker observations for an explicit reviewed marker delta.
    pub fn no_connect_markers(
        &self,
    ) -> Result<(Vec<NoConnectMarker>, Vec<NoConnectMarker>), String> {
        self.verify()?;
        let before = self.captured_schematic_sources()?;
        Ok((
            markers(before.into_iter())?.into_values().collect(),
            markers(
                self.candidate
                    .files()
                    .iter()
                    .map(|(path, bytes)| (path.clone(), bytes.as_slice())),
            )?
            .into_values()
            .collect(),
        ))
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Ord, PartialOrd)]
struct Net {
    name: String,
    class: Option<String>,
    pins: Vec<ConnectivityPin>,
}

impl Net {
    fn new(net: &ConnectivityNet) -> Result<Self, String> {
        let pins = unique_pins(&net.pins)?;
        if pins.is_empty()
            || net.name.len() > 65536
            || net.class.as_ref().is_some_and(|class| class.len() > 65536)
        {
            return Err("expected net is empty or exceeds field bounds".into());
        }
        Ok(Self {
            name: net.name.clone(),
            class: net.class.clone(),
            pins: pins.into_iter().collect(),
        })
    }
    fn json(&self) -> Value {
        serde_json::json!({"name": self.name, "class": self.class, "pins": self.pins.iter().map(pin_json).collect::<Vec<_>>()})
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct State {
    nets: BTreeSet<Net>,
    isolated: BTreeSet<ConnectivityPin>,
}
impl State {
    fn new(nets: &[ConnectivityNet], isolated: &[ConnectivityPin]) -> Result<Self, String> {
        if nets.len() > MAX_PINS {
            return Err("net count exceeds bound".into());
        }
        let result = Self {
            nets: unique_nets(nets)?,
            isolated: unique_pins(isolated)?,
        };
        result.pins()?;
        Ok(result)
    }
    fn pins(&self) -> Result<BTreeSet<ConnectivityPin>, String> {
        let mut pins = self.isolated.clone();
        for net in &self.nets {
            for pin in &net.pins {
                if pins.len() >= MAX_PINS || !pins.insert(pin.clone()) {
                    return Err("pin membership overlaps or exceeds bound".into());
                }
            }
        }
        Ok(pins)
    }
    fn partitions(&self) -> BTreeSet<(&[ConnectivityPin], &Option<String>)> {
        self.nets
            .iter()
            .map(|net| (net.pins.as_slice(), &net.class))
            .collect()
    }
}

fn unique_pins(pins: &[ConnectivityPin]) -> Result<BTreeSet<ConnectivityPin>, String> {
    if pins.len() > MAX_PINS {
        return Err("pin count exceeds bound".into());
    }
    let result = pins.iter().cloned().collect::<BTreeSet<_>>();
    if result.len() != pins.len() {
        return Err("repeated source-pin identity".into());
    }
    Ok(result)
}

fn unique_nets(nets: &[ConnectivityNet]) -> Result<BTreeSet<Net>, String> {
    let mut result = BTreeSet::new();
    let mut pins = 0usize;
    for net in nets {
        pins += net.pins.len();
        if pins > MAX_PINS || !result.insert(Net::new(net)?) {
            return Err("repeated or oversized net declaration".into());
        }
    }
    Ok(result)
}

fn match_connectivity(
    before: &State,
    after: &State,
    expected: &ConnectivityExpectation,
) -> Result<Value, String> {
    let remove = unique_nets(&expected.removed_nets)?;
    let add = unique_nets(&expected.added_nets)?;
    let remove_isolated = unique_pins(&expected.removed_isolated)?;
    let add_isolated = unique_pins(&expected.added_isolated)?;
    if !remove.is_disjoint(&add) || !remove_isolated.is_disjoint(&add_isolated) {
        return Err("redundant connectivity changes are not permitted".into());
    }
    let mut predicted = before.clone();
    for net in &remove {
        if !predicted.nets.remove(net) {
            return Err("expected removed net differs from baseline".into());
        }
    }
    for net in &add {
        if !predicted.nets.insert(net.clone()) {
            return Err("expected added net already exists".into());
        }
    }
    for pin in &remove_isolated {
        if !predicted.isolated.remove(pin) {
            return Err("expected removed isolated pin is absent".into());
        }
    }
    for pin in &add_isolated {
        if !predicted.isolated.insert(pin.clone()) {
            return Err("expected isolated pin already exists".into());
        }
    }
    let baseline_pins = before.pins()?;
    let expected_pins = predicted.pins()?;
    if baseline_pins
        .difference(&expected_pins)
        .cloned()
        .collect::<BTreeSet<_>>()
        != unique_pins(&expected.removed_pins)?
        || expected_pins
            .difference(&baseline_pins)
            .cloned()
            .collect::<BTreeSet<_>>()
            != unique_pins(&expected.added_pins)?
    {
        return Err("source-pin additions/removals were not declared exactly".into());
    }
    if &predicted != after {
        return Err("native connectivity differs from explicit expected delta".into());
    }
    Ok(serde_json::json!({
        "removed_nets": remove.iter().map(Net::json).collect::<Vec<_>>(),
        "added_nets": add.iter().map(Net::json).collect::<Vec<_>>(),
        "removed_isolated": remove_isolated.iter().map(pin_json).collect::<Vec<_>>(),
        "added_isolated": add_isolated.iter().map(pin_json).collect::<Vec<_>>(),
        "removed_pins": baseline_pins.difference(&expected_pins).map(pin_json).collect::<Vec<_>>(),
        "added_pins": expected_pins.difference(&baseline_pins).map(pin_json).collect::<Vec<_>>(),
    }))
}

fn atom(node: Option<&Node>) -> Result<&str, String> {
    match node {
        Some(Node::Atom {
            atom: Atom::Quoted(value) | Atom::Symbol(value),
            ..
        }) => Ok(value),
        _ => Err("missing marker atom".into()),
    }
}
fn items(node: &Node) -> Result<&[Node], String> {
    match node {
        Node::List { items, .. } => Ok(items),
        _ => Err("marker list missing".into()),
    }
}
fn marker_field<'a>(nodes: &'a [Node], tag: &str) -> Result<&'a [Node], String> {
    let fields = nodes
        .iter()
        .filter_map(|node| items(node).ok())
        .filter(|fields| atom(fields.first()).ok() == Some(tag))
        .collect::<Vec<_>>();
    if fields.len() != 1 {
        return Err("missing or ambiguous no-connect marker field".into());
    }
    Ok(fields[0])
}
type Markers = BTreeMap<(String, String), NoConnectMarker>;
fn markers<'a>(sources: impl Iterator<Item = (String, &'a [u8])>) -> Result<Markers, String> {
    let mut markers = BTreeMap::new();
    for (file, bytes) in sources {
        let source = std::str::from_utf8(bytes).map_err(|error| error.to_string())?;
        let parsed = parse_one(source).map_err(|error| error.to_string())?;
        let root = parsed.nodes.first().ok_or("schematic root missing")?;
        for node in items(root)? {
            let Node::List {
                items: fields,
                span,
            } = node
            else {
                continue;
            };
            if atom(fields.first()).ok() != Some("no_connect") {
                continue;
            }
            let uuid = atom(marker_field(fields, "uuid")?.get(1))?.to_owned();
            let at = marker_field(fields, "at")?;
            let position = Point {
                x: Coordinate::parse_mm(atom(at.get(1))?)?,
                y: Coordinate::parse_mm(atom(at.get(2))?)?,
            };
            let marker = NoConnectMarker {
                file: file.clone(),
                uuid: uuid.clone(),
                position,
                definition_digest: hash(&bytes[span.start..span.end]),
            };
            if markers.len() >= MAX_PINS || markers.insert((file.clone(), uuid), marker).is_some() {
                return Err("repeated or oversized marker namespace".into());
            }
        }
    }
    Ok(markers)
}

fn marker_json(marker: &NoConnectMarker) -> Value {
    serde_json::json!({ "file": marker.file, "uuid": marker.uuid,
        "x_nm": marker.position.x.nm(), "y_nm": marker.position.y.nm(),
        "definition_digest": marker.definition_digest, })
}
fn declared_markers(markers: &[NoConnectMarker]) -> Result<Markers, String> {
    if markers.len() > MAX_PINS {
        return Err("marker declaration exceeds bound".into());
    }
    let result = markers
        .iter()
        .map(|marker| ((marker.file.clone(), marker.uuid.clone()), marker.clone()))
        .collect::<Markers>();
    if result.len() != markers.len() {
        return Err("repeated marker declaration".into());
    }
    Ok(result)
}
fn match_markers(
    before: &Markers,
    after: &Markers,
    removed: &[NoConnectMarker],
    added: &[NoConnectMarker],
) -> Result<Value, String> {
    let remove = declared_markers(removed)?;
    let add = declared_markers(added)?;
    let mut predicted = before.clone();
    for (key, marker) in &remove {
        if predicted.remove(key).as_ref() != Some(marker) || add.get(key) == Some(marker) {
            return Err("expected removed marker differs from baseline or is redundant".into());
        }
    }
    for (key, marker) in &add {
        if predicted.insert(key.clone(), marker.clone()).is_some() {
            return Err("expected added marker already exists".into());
        }
    }
    if &predicted != after {
        return Err("native candidate marker delta was not declared exactly".into());
    }
    Ok(serde_json::json!({
        "removed": remove.values().map(marker_json).collect::<Vec<_>>(),
        "added": add.values().map(marker_json).collect::<Vec<_>>(),
    }))
}

fn diagnostic_keys(diagnostics: &[ErcDiagnostic]) -> Result<Vec<(&str, &str, &str)>, String> {
    if diagnostics.len() > 100_000 {
        return Err("diagnostic expectation exceeds count bound".into());
    }
    let mut total = 0usize;
    let mut keys = Vec::with_capacity(diagnostics.len());
    for diagnostic in diagnostics {
        total += diagnostic.sheet_path.len()
            + diagnostic.sheet_uuid_path.len()
            + diagnostic.diagnostic_json.len();
        if total > 32 * 1024 * 1024 {
            return Err("diagnostic expectation exceeds byte bound".into());
        }
        keys.push((
            diagnostic.sheet_path.as_str(),
            diagnostic.sheet_uuid_path.as_str(),
            diagnostic.diagnostic_json.as_str(),
        ));
    }
    keys.sort_unstable();
    Ok(keys)
}
pub(crate) fn match_erc(delta: &ErcDelta, expected: &ErcExpectation) -> Result<Value, String> {
    if delta.metadata_changed {
        return Err("ERC metadata changed".into());
    }
    let headers = |values: &[String]| -> Result<BTreeSet<String>, String> {
        let unique = values.iter().cloned().collect::<BTreeSet<_>>();
        if values.len() > 4096
            || unique.len() != values.len()
            || values.iter().any(|value| value.len() > 65_536)
        {
            return Err("duplicate or excessive ERC sheet header declarations".into());
        }
        Ok(unique)
    };
    let removed = headers(&expected.removed_sheet_headers)?;
    let added = headers(&expected.added_sheet_headers)?;
    if removed != headers(&delta.removed_sheet_headers)?
        || added != headers(&delta.added_sheet_headers)?
        || delta.sheet_headers_changed != (!removed.is_empty() || !added.is_empty())
    {
        return Err("complete ERC sheet headers differ from explicit expectations".into());
    }
    let introduced = diagnostic_keys(&expected.introduced)?;
    let resolved = diagnostic_keys(&expected.resolved)?;
    if introduced != diagnostic_keys(&delta.introduced)?
        || resolved != diagnostic_keys(&delta.resolved)?
    {
        return Err("complete ERC differs from explicit diagnostic expectations".into());
    }
    Ok(
        serde_json::json!({"introduced": introduced, "resolved": resolved, "removed_sheet_headers":removed,"added_sheet_headers":added}),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use kicad_schematic::pins::{LogicalPinKey, SourceUnitKey};

    fn pin(number: &str) -> ConnectivityPin {
        ConnectivityPin::Component(LogicalPinKey {
            component_units: vec![SourceUnitKey {
                file: "root.kicad_sch".into(),
                sheet_path: "/11111111-1111-4111-8111-111111111111".into(),
                symbol_uuid: "22222222-2222-4222-8222-222222222222".into(),
            }]
            .into(),
            number: number.into(),
        })
    }
    fn net(name: &str, numbers: &[&str]) -> ConnectivityNet {
        ConnectivityNet {
            name: name.into(),
            class: Some("Default".into()),
            pins: numbers.iter().map(|number| pin(number)).collect(),
        }
    }

    #[test]
    fn electrical_declarations_preserve_unaffected_membership_and_bind_pin_lifecycles() {
        let a = net("A", &["1", "2"]);
        let b = net("B", &["3"]);
        let unaffected = net("C", &["4"]);
        let before = State::new(&[a.clone(), b.clone(), unaffected.clone()], &[pin("5")]).unwrap();
        let merged = net("Joined", &["3", "2", "1"]);
        let after = State::new(&[unaffected.clone(), merged.clone()], &[pin("5")]).unwrap();
        let expected = ConnectivityExpectation {
            removed_nets: vec![a.clone(), b.clone()],
            added_nets: vec![merged.clone()],
            ..Default::default()
        };
        let canonical = match_connectivity(&before, &after, &expected).unwrap();
        let inverse = ConnectivityExpectation {
            removed_nets: expected.added_nets.clone(),
            added_nets: expected.removed_nets.clone(),
            ..Default::default()
        };
        assert!(match_connectivity(&after, &before, &inverse).is_ok());
        let mut reordered = expected.clone();
        reordered.removed_nets.reverse();
        reordered.added_nets[0].pins.reverse();
        assert_eq!(
            canonical,
            match_connectivity(&before, &after, &reordered).unwrap()
        );
        assert!(match_connectivity(&before, &after, &Default::default()).is_err());
        for changed in [
            State::new(std::slice::from_ref(&merged), &[pin("5")]).unwrap(),
            State::new(&[merged.clone(), net("Changed", &["4"])], &[pin("5")]).unwrap(),
        ] {
            assert!(match_connectivity(&before, &changed, &expected).is_err());
        }
        let mut overlap = expected.clone();
        overlap.added_nets.push(net("Overlap", &["1"]));
        assert!(match_connectivity(&before, &after, &overlap).is_err());
        let mut missing = expected.clone();
        missing.removed_nets.pop();
        assert!(match_connectivity(&before, &after, &missing).is_err());
        let added = State::new(
            &[a.clone(), b.clone(), unaffected.clone(), net("New", &["6"])],
            &[pin("5")],
        )
        .unwrap();
        let mut birth = ConnectivityExpectation {
            added_nets: vec![net("New", &["6"])],
            ..Default::default()
        };
        assert!(match_connectivity(&before, &added, &birth).is_err());
        birth.added_pins.push(pin("6"));
        assert!(match_connectivity(&before, &added, &birth).is_ok());
        let removed = State::new(&[a.clone(), unaffected], &[pin("5")]).unwrap();
        let mut death = ConnectivityExpectation {
            removed_nets: vec![b],
            ..Default::default()
        };
        assert!(match_connectivity(&before, &removed, &death).is_err());
        death.removed_pins.push(pin("3"));
        assert!(match_connectivity(&before, &removed, &death).is_ok());
        let isolated = State::new(
            &[net("A", &["1"]), net("C", &["4"])],
            &[pin("2"), pin("3"), pin("5")],
        )
        .unwrap();
        let isolation = ConnectivityExpectation {
            removed_nets: vec![a, net("B", &["3"])],
            added_nets: vec![net("A", &["1"])],
            added_isolated: vec![pin("2"), pin("3")],
            ..Default::default()
        };
        assert!(match_connectivity(&before, &isolated, &isolation).is_ok());
        let mut wrong_class = merged;
        wrong_class.class = None;
        let wrong = State::new(&[wrong_class, net("C", &["4"])], &[pin("5")]).unwrap();
        assert!(match_connectivity(&before, &wrong, &expected).is_err());
        assert_ne!(before.partitions(), after.partitions());
    }

    #[test]
    fn erc_expectations_are_exact_sheet_aware_multisets_without_ignore_flags() {
        let diagnostic = ErcDiagnostic {
            sheet_path: "/sheet/".into(),
            sheet_uuid_path: "/root/child".into(),
            diagnostic_json:
                r#"{"severity":"warning","excluded":true,"pos":0.12345678901234567890}"#.into(),
        };
        let delta = ErcDelta {
            metadata_changed: false,
            sheet_headers_changed: false,
            removed_sheet_headers: vec![],
            added_sheet_headers: vec![],
            introduced: vec![diagnostic.clone(), diagnostic.clone()],
            resolved: vec![],
            unchanged: 12,
        };
        let expected = ErcExpectation {
            removed_sheet_headers: vec![],
            added_sheet_headers: vec![],
            introduced: delta.introduced.clone(),
            resolved: vec![],
        };
        assert!(match_erc(&delta, &expected).is_ok());
        assert!(match_erc(&delta, &Default::default()).is_err());
        let mut missing = expected.clone();
        missing.introduced.pop();
        assert!(match_erc(&delta, &missing).is_err());
        for field in 0..3 {
            let mut wrong = expected.clone();
            match field {
                0 => wrong.introduced[0].sheet_uuid_path.push_str("/other"),
                1 => {
                    wrong.introduced[0].diagnostic_json =
                        diagnostic.diagnostic_json.replace("67890", "67891")
                }
                _ => {
                    wrong.introduced[0].diagnostic_json =
                        diagnostic.diagnostic_json.replace("true", "false")
                }
            }
            assert!(match_erc(&delta, &wrong).is_err());
        }
        let mut metadata = delta.clone();
        metadata.metadata_changed = true;
        assert!(match_erc(&metadata, &expected).is_err());
        metadata.metadata_changed = false;
        metadata.sheet_headers_changed = true;
        assert!(match_erc(&metadata, &expected).is_err());
        let resolved = ErcDelta {
            introduced: vec![],
            resolved: vec![diagnostic.clone()],
            ..delta
        };
        assert!(match_erc(&resolved, &Default::default()).is_err());
        assert!(
            match_erc(
                &resolved,
                &ErcExpectation {
                    removed_sheet_headers: vec![],
                    added_sheet_headers: vec![],
                    introduced: vec![],
                    resolved: vec![diagnostic]
                }
            )
            .is_ok()
        );
    }

    #[test]
    fn no_connect_markers_keep_identity_position_and_full_definition_separate() {
        let source = b"(kicad_sch (no_connect (at 10 20) (uuid marker) (extra retained)))";
        let before = markers(std::iter::once((
            "root.kicad_sch".into(),
            source.as_slice(),
        )))
        .unwrap();
        let marker = before.values().next().unwrap().clone();
        assert!(match_markers(&before, &before, &[], &[]).is_ok());
        assert!(match_markers(&before, &BTreeMap::new(), &[], &[]).is_err());
        assert!(
            match_markers(
                &before,
                &BTreeMap::new(),
                std::slice::from_ref(&marker),
                &[]
            )
            .is_ok()
        );
        let moved_source = b"(kicad_sch (no_connect (at 10 21) (uuid marker) (extra retained)))";
        let after = markers(std::iter::once((
            "root.kicad_sch".into(),
            moved_source.as_slice(),
        )))
        .unwrap();
        let moved = after.values().next().unwrap().clone();
        assert!(
            match_markers(
                &before,
                &after,
                std::slice::from_ref(&marker),
                std::slice::from_ref(&moved)
            )
            .is_ok()
        );
        let mut altered = moved;
        altered.definition_digest = "0".repeat(64);
        assert!(match_markers(&before, &after, std::slice::from_ref(&marker), &[altered]).is_err());
        assert!(
            match_markers(
                &before,
                &before,
                std::slice::from_ref(&marker),
                std::slice::from_ref(&marker)
            )
            .is_err()
        );
        let ambiguous = b"(kicad_sch (no_connect (at 10 20) (at 1 2) (uuid marker)))";
        assert!(
            markers(std::iter::once((
                "root.kicad_sch".into(),
                ambiguous.as_slice()
            )))
            .is_err()
        );
    }

    #[test]
    fn sheet_header_changes_require_exact_bounded_declarations_without_metadata_bypass() {
        let delta = ErcDelta {
            metadata_changed: false,
            sheet_headers_changed: true,
            removed_sheet_headers: vec![
                r#"{"path":"/Old/","uuid_path":"/root/child","unknown":0.12345678901234567890}"#
                    .into(),
            ],
            added_sheet_headers: vec![
                r#"{"path":"/New/","uuid_path":"/root/child","unknown":0.12345678901234567890}"#
                    .into(),
            ],
            introduced: vec![],
            resolved: vec![],
            unchanged: 0,
        };
        assert!(match_erc(&delta, &Default::default()).is_err());
        let expected = ErcExpectation {
            removed_sheet_headers: delta.removed_sheet_headers.clone(),
            added_sheet_headers: delta.added_sheet_headers.clone(),
            ..Default::default()
        };
        match_erc(&delta, &expected).unwrap();
        let mut wrong = expected.clone();
        wrong.added_sheet_headers[0] = wrong.added_sheet_headers[0].replace("67890", "67891");
        assert!(match_erc(&delta, &wrong).is_err());
        let mut duplicate = expected.clone();
        duplicate
            .added_sheet_headers
            .push(expected.added_sheet_headers[0].clone());
        assert!(match_erc(&delta, &duplicate).is_err());
        let mut extra = expected.clone();
        extra.removed_sheet_headers.push("{}".into());
        assert!(match_erc(&delta, &extra).is_err());
        let mut metadata = delta;
        metadata.metadata_changed = true;
        assert!(match_erc(&metadata, &expected).is_err());
    }
}
