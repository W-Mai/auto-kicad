//! Source-bound closed native templates; no caller-provided geometry or allocation proof.
use super::*;
mod frame;
use crate::library_catalog::FootprintResolutionEvidence;
use board::{
    SavedSynchronizationBoard, SynchronizationPlacement, SynchronizationSide, atom, items,
};
use kiutils_sexpr::{Atom, Node};
use serde::Deserialize;

const WORKER: &str = include_str!("../../../helpers/synchronization_template.py");

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct TemplatePad {
    number: String,
    position_nm: [i64; 2],
    orientation_microdegrees: i64,
    layers: Vec<String>,
    property: i32,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct TemplateItem {
    uuid: String,
    family: String,
    index: usize,
    kind: String,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct TemplateReport {
    native_version: String,
    request_sha256: String,
    source_sha256: String,
    before_sha256: String,
    after_sha256: String,
    receipt: Vec<TemplateItem>,
    before_pads: BTreeMap<String, TemplatePad>,
    placed_pads: BTreeMap<String, TemplatePad>,
    position_nm: [i64; 2],
    orientation_degrees: f64,
    side: SynchronizationSide,
}

pub(super) struct SavedFootprintTemplate {
    pub root_id: String,
    pub footprint: String,
    pub identities: BTreeSet<String>,
    pub evidence: Value,
    pub replacement: Option<board::replacement::ReplacementBinding>,
    pub native_footprint: Option<String>,
    source: FootprintResolutionEvidence,
    original: Inputs,
    private: Inputs,
    configuration: PathBuf,
    configuration_names: Vec<PathBuf>,
    tree: BTreeSet<PathBuf>,
    runner: NetlistRunner,
    worker: native::NativeWorkerReceipt,
    owner: tempfile::TempDir,
}
impl SavedFootprintTemplate {
    pub fn artifacts(&self) -> impl Iterator<Item = (&'static str, &[u8])> {
        ["template-before.kicad_pcb", "template-after.kicad_pcb"]
            .into_iter()
            .filter_map(|name| {
                self.private
                    .files
                    .get(&self.owner.path().join(name))
                    .and_then(|file| file.bytes.as_deref())
                    .map(|bytes| (name, bytes))
            })
    }
    pub fn verify_composed(
        &self,
        candidate: &SavedSynchronizationBoard,
        changes: &[board::SynchronizationDifference],
    ) -> Result<(), String> {
        verify_composed_definition(
            &self.footprint,
            &self.root_id,
            &self.identities,
            candidate,
            changes,
        )
    }
    pub fn verify_native_composed(
        &self,
        candidate: &SavedSynchronizationBoard,
        changes: &[board::SynchronizationDifference],
    ) -> Result<(), String> {
        verify_composed_definition(
            self.native_footprint.as_deref().unwrap_or(&self.footprint),
            &self.root_id,
            &self.identities,
            candidate,
            changes,
        )
    }
    pub fn verify(&self, source: &SynchronizationSource) -> Result<(), String> {
        source.verify()?;
        self.original.verify()?;
        self.private.verify()?;
        if tree_names(self.owner.path())? != self.tree
            || config_names(&self.configuration)? != self.configuration_names
            || source
                .audit
                .inner
                .footprints
                .resolve(&self.source.identifier)?
                != self.source
        {
            return Err("synchronization template source/configuration/namespace changed".into());
        }
        self.worker.verify(
            &source.audit,
            self.runner
                .execution_context()
                .ok_or("missing template execution context")?,
        )
    }
}

fn verify_composed_definition(
    footprint: &str,
    root_id: &str,
    identities: &BTreeSet<String>,
    candidate: &SavedSynchronizationBoard,
    changes: &[board::SynchronizationDifference],
) -> Result<(), String> {
    let wrap = |footprint: &str| format!("(kicad_pcb (version 20260206) {footprint})");
    let expected = SavedSynchronizationBoard::parse(&wrap(footprint))?;
    let actual = SavedSynchronizationBoard::parse(&wrap(candidate.footprint_text(root_id)?))?;
    let approved = changes
        .iter()
        .filter(|change| change.field != "footprint" && identities.contains(&change.uuid))
        .cloned()
        .collect::<Vec<_>>();
    if expected.protected_template_digest(&approved)?
        != actual.protected_template_digest(&approved)?
    {
        return Err("complete source-derived template changed during candidate composition or native reload".into());
    }
    Ok(())
}

pub(super) fn derived(seed: &str, family: &str, index: usize) -> String {
    let mut bytes = Sha256::digest(format!(
        "auto-kicad-sync-template-v1:{seed}:{family}:{index}"
    ));
    bytes[6] = (bytes[6] & 15) | 128;
    bytes[8] = (bytes[8] & 63) | 128;
    let text = hex::encode(&bytes[..16]);
    format!(
        "{}-{}-{}-{}-{}",
        &text[..8],
        &text[8..12],
        &text[12..16],
        &text[16..20],
        &text[20..]
    )
}

fn layer_flip(name: &str) -> String {
    if let Some(rest) = name.strip_prefix("F.") {
        format!("B.{rest}")
    } else if let Some(rest) = name.strip_prefix("B.") {
        format!("F.{rest}")
    } else {
        name.into()
    }
}
fn angle(value: f64) -> Result<i64, String> {
    if !value.is_finite() || value.abs() > 36000.0 {
        return Err("invalid native template angle".into());
    }
    let microdegrees = value * 1_000_000.0;
    if (microdegrees - microdegrees.round()).abs() > 0.000001 {
        return Err("native template angle lost precision".into());
    }
    Ok((microdegrees.round() as i64).rem_euclid(360_000_000))
}
fn verify_placement(
    report: &TemplateReport,
    placement: &SynchronizationPlacement,
) -> Result<(), String> {
    if report.position_nm != placement.position_nm
        || report.side != placement.side
        || angle(report.orientation_degrees)? != placement.orientation_mdeg as i64 * 1000
        || report.before_pads.keys().collect::<Vec<_>>()
            != report.placed_pads.keys().collect::<Vec<_>>()
    {
        return Err("native template placement or complete pad identity differs".into());
    }
    let radians = (placement.orientation_mdeg as f64 / 1000.0).to_radians();
    let (sin, cos) = radians.sin_cos();
    for (id, before) in &report.before_pads {
        let after = &report.placed_pads[id];
        if !(0..360_000_000).contains(&before.orientation_microdegrees)
            || !(0..360_000_000).contains(&after.orientation_microdegrees)
        {
            return Err("invalid exact native pad angle".into());
        }
        let x = before.position_nm[0] as f64;
        let y = before.position_nm[1] as f64
            * if placement.side == SynchronizationSide::Back {
                -1.0
            } else {
                1.0
            };
        let position = [
            (x * cos + y * sin).round() as i64 + placement.position_nm[0],
            (-x * sin + y * cos).round() as i64 + placement.position_nm[1],
        ];
        let rotation = if placement.side == SynchronizationSide::Back {
            -before.orientation_microdegrees
        } else {
            before.orientation_microdegrees
        };
        let mut layers = before
            .layers
            .iter()
            .map(|layer| {
                if placement.side == SynchronizationSide::Back {
                    layer_flip(layer)
                } else {
                    layer.clone()
                }
            })
            .collect::<Vec<_>>();
        layers.sort();
        let mut observed = after.layers.clone();
        observed.sort();
        if before.number != after.number
            || before.property != after.property
            || position != after.position_nm
            || position.iter().any(|v| v.unsigned_abs() > 2_000_000_000)
            || layers != observed
            || (rotation + placement.orientation_mdeg as i64 * 1000).rem_euclid(360_000_000)
                != after.orientation_microdegrees
        {
            return Err(format!(
                "native template pad transform or fabrication property differs: {id}"
            ));
        }
    }
    Ok(())
}

#[derive(Clone, Debug, Eq, PartialEq, Ord, PartialOrd)]
enum Projection {
    Atom(bool, String),
    List(Vec<Projection>),
}
fn fields<'a>(node: &'a Node, name: &str) -> Result<Vec<&'a Node>, String> {
    Ok(items(node)?
        .iter()
        .filter(|n| {
            matches!(n, Node::List { .. })
                && items(n)
                    .ok()
                    .and_then(|v| v.first())
                    .and_then(|v| atom(v).ok())
                    == Some(name)
        })
        .collect())
}
fn decimal(value: &str) -> String {
    let (negative, text) = value
        .strip_prefix('-')
        .map_or((false, value), |v| (true, v));
    if text.is_empty()
        || !text.bytes().any(|byte| byte.is_ascii_digit())
        || !text.bytes().all(|b| b.is_ascii_digit() || b == b'.')
        || text.bytes().filter(|b| *b == b'.').count() > 1
    {
        return value.into();
    }
    let (integer, fraction) = text.split_once('.').unwrap_or((text, ""));
    let integer = integer.trim_start_matches('0');
    let fraction = fraction.trim_end_matches('0');
    let number = format!(
        "{}{}{}",
        if integer.is_empty() { "0" } else { integer },
        if fraction.is_empty() { "" } else { "." },
        fraction
    );
    if negative && number != "0" {
        format!("-{number}")
    } else {
        number
    }
}
fn project(
    node: &Node,
    root: bool,
    missing_standard: &BTreeSet<String>,
) -> Result<Projection, String> {
    match node {
        Node::Atom {
            atom: Atom::Quoted(value),
            ..
        } => Ok(Projection::Atom(true, value.clone())),
        Node::Atom {
            atom: Atom::Symbol(value),
            ..
        } => Ok(Projection::Atom(false, decimal(value))),
        Node::List { items: values, .. } => {
            let name = values
                .first()
                .and_then(|n| atom(n).ok())
                .ok_or("empty template form")?;
            let mut atoms = Vec::new();
            let mut lists = Vec::new();
            for (index, child) in values.iter().enumerate() {
                if root && index == 1 {
                    atoms.push(Projection::Atom(true, "<qualified-footprint>".into()));
                    continue;
                }
                if let Node::List {
                    items: children, ..
                } = child
                {
                    let tag = children
                        .first()
                        .and_then(|n| atom(n).ok())
                        .ok_or("empty template child form")?;
                    if tag == "uuid" {
                        if children.len() != 2 || !board::uuid(atom(&children[1])?) {
                            return Err("invalid template source UUID".into());
                        }
                        continue;
                    }
                    if root && ["version", "generator", "generator_version"].contains(&tag) {
                        continue;
                    }
                    if root
                        && tag == "at"
                        && children
                            .iter()
                            .skip(1)
                            .all(|n| atom(n).is_ok_and(|v| decimal(v) == "0"))
                    {
                        continue;
                    }
                    if root
                        && tag == "embedded_fonts"
                        && children.len() == 2
                        && atom(&children[1])? == "no"
                    {
                        continue;
                    }
                    if root
                        && tag == "property"
                        && children.len() >= 3
                        && missing_standard.contains(atom(&children[1])?)
                        && atom(&children[2])?.is_empty()
                    {
                        continue;
                    }
                    lists.push(project(child, false, missing_standard)?);
                } else {
                    atoms.push(project(child, false, missing_standard)?);
                }
            }
            if name == "layers" && atoms.len() > 1 {
                atoms[1..].sort();
            }
            if !["pts", "primitives", "pins", "members"].contains(&name) {
                lists.sort();
            }
            atoms.extend(lists);
            Ok(Projection::List(atoms))
        }
    }
}

pub(super) fn physical_pad_digest(node: &Node) -> Result<String, String> {
    let mut value = project(node, false, &BTreeSet::new())?;
    if let Projection::List(values) = &mut value {
        values.retain(|value| !matches!(value, Projection::List(parts) if parts.first().and_then(|n| match n { Projection::Atom(_, tag) => Some(tag.as_str()), _ => None }).is_some_and(|tag| ["net", "pinfunction", "pintype"].contains(&tag))));
    }
    Ok(hash(format!("{value:?}").as_bytes()))
}

fn source_projection(
    source: &str,
    native: &SavedSynchronizationBoard,
    id: &str,
) -> Result<(), String> {
    let parsed = kicad_schematic::cst::parse_one(source).map_err(|e| e.to_string())?;
    let root = parsed
        .nodes
        .first()
        .ok_or("missing library footprint root")?;
    if items(root)?.first().map(atom).transpose()? != Some("footprint")
        || fields(root, "version")?.len() != 1
        || atom(
            items(fields(root, "version")?[0])?
                .get(1)
                .ok_or("missing template source version")?,
        )? != "20260206"
        || !fields(root, "group")?.is_empty()
        || !fields(root, "zone")?.is_empty()
        || !fields(root, "net_tie_pad_groups")?.is_empty()
        || fields(root, "duplicate_pad_numbers_are_jumpers")?
            .iter()
            .any(|n| items(n).is_ok_and(|v| v.len() != 2 || !atom(&v[1]).is_ok_and(|v| v == "no")))
    {
        return Err(
            "unsupported source template version or group/net-tie/jumper relationships".into(),
        );
    }
    for header in ["version", "generator", "generator_version"] {
        for field in fields(root, header)? {
            let values = items(field)?;
            if values.len() != 2 || atom(&values[1]).is_err() {
                return Err("invalid source template header grammar".into());
            }
        }
    }
    let mut property_names = BTreeSet::new();
    for property in fields(root, "property")? {
        let values = items(property)?;
        if values.len() < 3 || !property_names.insert(atom(&values[1])?.to_owned()) {
            return Err("ambiguous source template property".into());
        }
    }
    let missing = ["Datasheet", "Description"]
        .into_iter()
        .filter(|name| !property_names.contains(*name))
        .map(str::to_owned)
        .collect::<BTreeSet<_>>();
    if project(root, true, &missing)? != project(native.node(id)?, true, &missing)? {
        return Err(
            "native template serialization did not preserve the complete source definition".into(),
        );
    }
    Ok(())
}

pub(super) fn capture(
    source: &SynchronizationSource,
    component: &SynchronizationComponent,
    placement: &SynchronizationPlacement,
    seed: &str,
    root_id: &str,
) -> Result<SavedFootprintTemplate, String> {
    source.verify()?;
    placement.validate()?;
    let assigned = component
        .footprint
        .as_deref()
        .filter(|v| !v.is_empty())
        .ok_or("missing source footprint assignment")?;
    let (nickname, entry_name) = assigned
        .split_once(':')
        .ok_or("unqualified source footprint assignment")?;
    let identifier = FootprintLibraryIdentifier {
        nickname: nickname.into(),
        entry_name: entry_name.into(),
    };
    let resolved = source.audit.inner.footprints.resolve(&identifier)?;
    if resolved.source_size > 8 * 1024 * 1024 {
        return Err("source template exceeds 8 MiB bound".into());
    }
    let owner = tempfile::Builder::new()
        .prefix("auto-kicad-sync-template-")
        .tempdir()
        .map_err(|e| e.to_string())?;
    let owned = fs::canonicalize(owner.path()).map_err(|e| e.to_string())?;
    let configuration = owned.join("config/10.0");
    fs::create_dir_all(&configuration).map_err(|e| e.to_string())?;
    let mut original = Inputs::default();
    for path in &source.audit.inner.private_config_names {
        original.copy(
            path,
            &configuration.join(
                path.file_name()
                    .ok_or("missing native configuration name")?,
            ),
        )?;
    }
    let library = owned
        .join("source.pretty")
        .join(format!("{entry_name}.kicad_mod"));
    original.copy_expected(
        &resolved.canonical_source_path,
        &library,
        resolved.source_size,
        Some(&resolved.source_digest),
    )?;
    let source_bytes = fs::read(&library).map_err(|e| e.to_string())?;
    crate::library_catalog::symbols::validate_raw_nesting(&source_bytes)?;
    let request = serde_json::json!({"identifier":identifier,"source_sha256":resolved.source_digest,"source_size":resolved.source_size,"seed":seed,"footprint_uuid":root_id,"position_nm":placement.position_nm,"orientation_mdeg":placement.orientation_mdeg,"side":placement.side});
    let request_bytes = serde_json::to_vec(&request).map_err(|e| e.to_string())?;
    let request_path = owned.join("request.json");
    write_new(&request_path, &request_bytes)?;
    let runner = NetlistRunner::production()
        .map_err(|e| e.to_string())?
        .freeze_context(Some(&owned.join("config")))
        .map_err(|e| e.to_string())?;
    let context = runner
        .execution_context()
        .ok_or("missing native template context")?;
    let (first, _) =
        native::run_fixed_worker(&source.audit, context, &owned, WORKER, &[&request_path])?;
    let before_path = owned.join("template-before.kicad_pcb");
    let after_path = owned.join("template-after.kicad_pcb");
    let first_before = native::bounded_read(&before_path, 128 * 1024 * 1024)?;
    let first_after = native::bounded_read(&after_path, 128 * 1024 * 1024)?;
    fs::remove_file(&before_path).map_err(|e| e.to_string())?;
    fs::remove_file(&after_path).map_err(|e| e.to_string())?;
    let configuration_names = config_names(&configuration)?;
    let mut settled = Inputs::default();
    for path in &configuration_names {
        settled.track(path)?;
    }
    let (report_bytes, worker) =
        native::run_fixed_worker(&source.audit, context, &owned, WORKER, &[&request_path])?;
    settled.verify()?;
    if config_names(&configuration)? != configuration_names || report_bytes != first {
        return Err(
            "native template output/configuration changed across fixed repeated runs".into(),
        );
    }
    let before = native::bounded_read(&before_path, 128 * 1024 * 1024)?;
    let after = native::bounded_read(&after_path, 128 * 1024 * 1024)?;
    if before != first_before || after != first_after {
        return Err("complete native template bytes are nondeterministic".into());
    }
    let report: TemplateReport =
        serde_json::from_slice(&report_bytes).map_err(|e| e.to_string())?;
    if report.native_version != "10.0.7"
        || report.source_sha256 != resolved.source_digest
        || report.request_sha256 != hash(&request_bytes)
        || report.before_sha256 != hash(&before)
        || report.after_sha256 != hash(&after)
    {
        return Err("native template request/source/output binding differs".into());
    }
    verify_placement(&report, placement)?;
    let before =
        SavedSynchronizationBoard::parse(std::str::from_utf8(&before).map_err(|e| e.to_string())?)?;
    let after =
        SavedSynchronizationBoard::parse(std::str::from_utf8(&after).map_err(|e| e.to_string())?)?;
    if before.footprints().len() != 1
        || after.footprints().len() != 1
        || !after.footprints().contains_key(root_id)
    {
        return Err("native template footprint inventory differs".into());
    }
    frame::verify(&before, &after, root_id, placement)?;
    source_projection(
        std::str::from_utf8(&source_bytes).map_err(|e| e.to_string())?,
        &before,
        root_id,
    )?;
    let mut identities = BTreeSet::new();
    let mut slots = BTreeSet::new();
    for entry in &report.receipt {
        let expected = if entry.family == "root" && entry.index == 0 {
            root_id.to_owned()
        } else if ["field", "graphic", "pad"].contains(&entry.family.as_str()) {
            derived(seed, &entry.family, entry.index)
        } else {
            return Err("unknown native template allocation family".into());
        };
        if entry.uuid != expected
            || !identities.insert(entry.uuid.clone())
            || !slots.insert((entry.family.as_str(), entry.index))
            || entry.kind.is_empty()
        {
            return Err("native template allocation receipt differs".into());
        }
    }
    if &identities != before.identities() || &identities != after.identities() {
        return Err("native template allocation does not cover complete child identities".into());
    }
    let expected = component.pins.keys().cloned().collect::<BTreeSet<_>>();
    let pads = &after.footprints()[root_id].pads;
    if pads
        .iter()
        .filter(|pad| !pad.number.is_empty())
        .map(|pad| pad.number.clone())
        .collect::<BTreeSet<_>>()
        != expected
        || pads.iter().any(|pad| !pad.net.is_empty())
    {
        return Err("complete source/template pad universe differs".into());
    }
    let footprint = after.footprint_text(root_id)?.to_owned();
    let tree = tree_names(&owned)?;
    let mut private = Inputs::default();
    for path in &tree {
        if path.is_file() {
            private.track(path)?;
        }
    }
    let evidence = serde_json::json!({"source":resolved,"request":request,"native":report,"worker":worker,"original_inputs":original,"private_inputs":private,"template_sha256":hash(footprint.as_bytes())});
    let template = SavedFootprintTemplate {
        root_id: root_id.into(),
        footprint,
        identities,
        evidence,
        replacement: None,
        native_footprint: None,
        source: resolved,
        original,
        private,
        configuration,
        configuration_names,
        tree,
        runner,
        worker,
        owner,
    };
    template.verify(source)?;
    Ok(template)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn composed_template_checks_cannot_hide_geometry_behind_the_creation_difference() {
        let id = "11111111-1111-4111-8111-111111111111";
        let pad = "22222222-2222-4222-8222-222222222222";
        let definition = format!(
            r#"(footprint "Test:Part" (uuid "{id}") (property "Reference" "REF**" (at 0 1)) (property "Value" "Part") (unknown "retained") (pad "1" smd rect (uuid "{pad}") (at 0 0)))"#
        );
        let candidate = definition.replace("REF**", "R1");
        let board = SavedSynchronizationBoard::parse(&format!(
            "(kicad_pcb (version 20260206) {candidate})"
        ))
        .unwrap();
        let ids = BTreeSet::from([id.to_owned(), pad.to_owned()]);
        let changes = vec![
            board::SynchronizationDifference {
                uuid: id.into(),
                field: "footprint".into(),
                before: None,
                after: Some("R1".into()),
            },
            board::SynchronizationDifference {
                uuid: id.into(),
                field: "property:Reference".into(),
                before: Some("REF**".into()),
                after: Some("R1".into()),
            },
        ];
        verify_composed_definition(&definition, id, &ids, &board, &changes).unwrap();
        let reordered = candidate.replace("(unknown \"retained\")", "").replace(
            "(property \"Value\" \"Part\")",
            "(unknown \"retained\") (property \"Value\" \"Part\")",
        );
        let reordered = SavedSynchronizationBoard::parse(&format!(
            "(kicad_pcb (version 20260206) {reordered})"
        ))
        .unwrap();
        verify_composed_definition(&definition, id, &ids, &reordered, &changes).unwrap();
        for corrupted in [
            candidate.replace("(at 0 0)", "(at 1 0)"),
            candidate.replace("(at 0 1)", "(at 0 2)"),
            candidate.replace("retained", "changed"),
        ] {
            let board = SavedSynchronizationBoard::parse(&format!(
                "(kicad_pcb (version 20260206) {corrupted})"
            ))
            .unwrap();
            assert!(verify_composed_definition(&definition, id, &ids, &board, &changes).is_err());
        }
    }

    #[test]
    fn complete_source_projection_preserves_unknown_fields_geometry_text_and_exact_numbers() {
        let id = "11111111-1111-4111-8111-111111111111";
        let source = r#"(footprint "Part" (version 20260206) (generator source) (layer "F.Cu")
            (property "Reference" "REF**") (property "Value" "10k")
            (fp_poly (pts (xy 0.00 0) (xy 1.0 0) (xy 0 1)) (layer "F.Fab"))
            (pad "1" smd rect (at 0.12345678901234567890 0) (size 1 1) (layers "F.Cu" "F.Paste" "F.Mask")))"#;
        let native = format!(
            r#"(kicad_pcb (version 20260206) (footprint "Test:Part" (layer "F.Cu") (uuid "{id}") (at 0 0)
            (property "Reference" "REF**") (property "Value" "10k") (property "Datasheet" "") (property "Description" "")
            (fp_poly (pts (xy 0 0) (xy 1 0) (xy 0 1)) (layer "F.Fab"))
            (pad "1" smd rect (uuid "22222222-2222-4222-8222-222222222222") (at 0.12345678901234567890 0) (size 1 1) (layers "F.Cu" "F.Mask" "F.Paste")) (embedded_fonts no)))"#
        );
        let board = SavedSynchronizationBoard::parse(&native).unwrap();
        source_projection(source, &board, id).unwrap();
        for changed in [
            source.replace("10k", "100k"),
            source.replace("67890", "67891"),
            source.replace("(xy 1.0 0) (xy 0 1)", "(xy 0 1) (xy 1.0 0)"),
            source.replace("(layer \"F.Fab\")", "(layer \"B.Fab\")"),
            source.replace(
                "(generator source)",
                "(generator source) (future (value 1))",
            ),
        ] {
            assert!(source_projection(&changed, &board, id).is_err());
        }
        assert_ne!(derived("source", "pad", 0), derived("source", "field", 0));
        assert_ne!(derived("source", "pad", 0), derived("different", "pad", 0));
        assert_ne!(derived("source", "pad", 0), derived("source", "pad", 1));
    }
}
