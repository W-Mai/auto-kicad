//! Exercise typed candidate edits on explicitly owned copies using the installed native CLI.
use kicad_adapter::schematic_audit::{SchematicAudit, SchematicAuditPolicy};
use kicad_schematic::editing::{
    EditAction, EditScope, ElectricalShape, FootprintCompatibilityEvidence, Label, LabelKind,
    NewObject, ObjectKind, ReferenceRequest, SchematicCandidate, SchematicEdit, SymbolFlags,
    SymbolProperty, allocate_references, compile_edits,
};
use kicad_schematic::geometry::{Mirror, Point, QuarterTurn, SymbolTransform};
use kicad_schematic::probes::ConnectivityPin;
use kicad_schematic::{Limits, load_schematic_project};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    env, fs,
    path::{Path, PathBuf},
};
type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

fn digest(path: &Path) -> Result<String> {
    Ok(format!("{:x}", Sha256::digest(fs::read(path)?)))
}

fn snapshot(root: &Path) -> Result<BTreeMap<PathBuf, Vec<u8>>> {
    let mut result = BTreeMap::new();
    let mut stack = vec![root.to_owned()];
    let mut total = 0;
    while let Some(dir) = stack.pop() {
        for entry in fs::read_dir(dir)? {
            let path = entry?.path();
            let metadata = fs::symlink_metadata(&path)?;
            if metadata.file_type().is_symlink() {
                return Err("symlink in acceptance input".into());
            }
            if metadata.is_dir() {
                stack.push(path);
            } else if metadata.is_file() {
                let bytes = fs::read(&path)?;
                total += bytes.len();
                if total > 128 * 1024 * 1024 {
                    return Err("acceptance copy exceeds bound".into());
                }
                result.insert(path.strip_prefix(root)?.to_owned(), bytes);
            }
        }
    }
    Ok(result)
}
fn audit(root: &Path, source: &Path, config: &Path) -> Result<SchematicAudit> {
    let shared = Path::new("/Applications/KiCad/KiCad.app/Contents/SharedSupport");
    Ok(SchematicAudit::prepare(&SchematicAuditPolicy::new(
        root,
        source,
        config,
        &fs::canonicalize(shared.join("symbols"))?,
        &fs::canonicalize(shared.join("footprints"))?,
        &[],
    )?)?)
}
fn compile(
    root: &Path,
    source: &Path,
    audit: &SchematicAudit,
    edits: &[SchematicEdit],
) -> Result<SchematicCandidate> {
    let parsed = load_schematic_project(root, source, "10.0.7", 1, Limits::default())?;
    let files = parsed
        .files
        .iter()
        .map(|f| {
            Ok((
                f.artifact.relative_path.clone(),
                fs::read(root.join(&f.artifact.relative_path))?,
            ))
        })
        .collect::<std::io::Result<BTreeMap<_, _>>>()?;
    let sources = files
        .iter()
        .map(|(p, b)| (p.clone(), b.as_slice()))
        .collect();
    Ok(compile_edits(&parsed, audit.netlist(), &sources, edits)?)
}
fn capture(
    parent: &Path,
    source_relative: &Path,
    config: &Path,
    output: &Path,
    candidate: &SchematicCandidate,
) -> Result<SchematicAudit> {
    fs::create_dir(output)?;
    let root = output.join("project");
    fs::create_dir(&root)?;
    for (relative, mut bytes) in snapshot(parent)? {
        if let Some(replacement) = relative.to_str().and_then(|r| candidate.files().get(r)) {
            bytes = replacement.clone();
        }
        let target = root.join(relative);
        fs::create_dir_all(target.parent().ok_or("missing parent")?)?;
        fs::write(target, bytes)?;
    }
    let before = snapshot(&root)?;
    let observed = audit(&root, &root.join(source_relative), config)?;
    let connectivity = observed.observe_pin_connectivity()?;
    fs::write(
        output.join("changes.txt"),
        format!("{:#?}", candidate.changes()),
    )?;
    fs::write(
        output.join("source-replacements.txt"),
        format!("{:#?}", candidate.replacements()),
    )?;
    fs::write(
        output.join("native.txt"),
        format!("{:#?}", observed.netlist()),
    )?;
    fs::write(
        output.join("connectivity.txt"),
        format!("{:#?}", connectivity.connectivity()),
    )?;
    fs::write(output.join("erc.json"), observed.erc().raw().get())?;
    fs::write(
        output.join("evidence.json"),
        serde_json::to_vec_pretty(&observed.evidence()?)?,
    )?;
    if snapshot(&root)? != before {
        return Err("native audit changed candidate source".into());
    }
    Ok(observed)
}
fn partition(audit: &SchematicAudit) -> Result<Vec<Vec<ConnectivityPin>>> {
    Ok(audit
        .observe_pin_connectivity()?
        .connectivity()
        .partitions())
}
fn merge_selected(
    groups: &[Vec<ConnectivityPin>],
    selected: &BTreeSet<ConnectivityPin>,
) -> Vec<Vec<ConnectivityPin>> {
    let mut out = Vec::new();
    let mut merged = Vec::new();
    for group in groups {
        if group.iter().any(|p| selected.contains(p)) {
            merged.extend(group.iter().cloned());
        } else {
            out.push(group.clone());
        }
    }
    if !merged.is_empty() {
        merged.sort();
        out.push(merged);
    }
    out.sort();
    out
}
fn main() -> Result<()> {
    let args = env::args().skip(1).collect::<Vec<_>>();
    if !(3..=4).contains(&args.len()) {
        return Err(
            "usage: check-schematic-edits OWNED_SCHEMATIC OWNED_CONFIG NEW_OUTPUT [REFERENCE]"
                .into(),
        );
    }
    let reference = args.get(3).map(String::as_str).unwrap_or("R1");
    let source = fs::canonicalize(&args[0])?;
    let root = source.parent().ok_or("missing project root")?;
    let config = fs::canonicalize(&args[1])?;
    for dir in [root, &config] {
        if fs::read_to_string(dir.join(".auto-kicad-audit-owned"))?
            != "schematic-audit-acceptance\n"
        {
            return Err("missing owned-copy marker".into());
        }
    }
    let output = Path::new(&args[2]);
    if output.starts_with(root) {
        return Err("output must be outside input project".into());
    }
    fs::create_dir(output)?;
    let inputs = snapshot(root)?;
    let configuration = snapshot(&config)?;
    let before = audit(root, &source, &config)?;
    let baseline = partition(&before)?;
    fs::write(output.join("baseline-erc.json"), before.erc().raw().get())?;
    fs::write(
        output.join("baseline-connectivity.txt"),
        format!("{baseline:#?}"),
    )?;
    let unit = before
        .identities()
        .units()
        .iter()
        .find(|u| u.reference == reference)
        .ok_or("fixture needs R1")?;
    let file = unit.file.clone();
    let id = unit.source_uuid.clone();
    let target_pins = before
        .pins()
        .pins()
        .iter()
        .filter(|p| {
            p.occurrences
                .iter()
                .any(|o| o.source.file == file && o.source.symbol_uuid == id)
        })
        .collect::<Vec<_>>();
    if target_pins.iter().any(|p| p.occurrences.len() != 1) {
        return Err("acceptance target must be single-unit resistor".into());
    }
    let selected = target_pins
        .iter()
        .map(|p| ConnectivityPin::Component(p.key.clone()))
        .collect::<BTreeSet<_>>();
    let occurrences = target_pins
        .iter()
        .filter(|p| p.reference == reference)
        .collect::<Vec<_>>();
    if occurrences.len() != 2 {
        return Err("fixture R1 needs two logical pins".into());
    }
    let first = occurrences[0].occurrences[0].position;
    let second = occurrences[1].occurrences[0].position;
    let edit = |action| SchematicEdit {
        file: file.clone(),
        scope: EditScope::FileAllInstances,
        action,
    };
    let instances = before
        .identities()
        .sheets()
        .iter()
        .filter(|s| s.file == file)
        .map(|s| s.path.clone())
        .collect::<Vec<_>>();
    if instances.len() > 1 {
        let mut isolated = edit(EditAction::SetProperty {
            uuid: id.clone(),
            property: SymbolProperty::Value,
            value: "changed".into(),
        });
        isolated.scope = EditScope::SingleInstance(instances[0].clone());
        if compile(root, &source, &before, &[isolated]).is_ok() {
            return Err("accepted isolated mutation of shared physical file".into());
        }
    }
    let relative = source.strip_prefix(root)?;
    let mut private = vec![before.private_root().to_owned()];
    let parsed = load_schematic_project(root, &source, "10.0.7", 1, Limits::default())?;
    let annotation_request = ReferenceRequest {
        uuid: id.clone(),
        instance_path: (instances.len() > 1).then(|| unit.sheet_path.clone()),
        prefix: "QA".into(),
        minimum: 900,
    };
    let annotation_edits = allocate_references(
        &parsed,
        before.netlist(),
        std::slice::from_ref(&annotation_request),
    )?;
    let annotation_edits_again = allocate_references(
        &parsed,
        before.netlist(),
        std::slice::from_ref(&annotation_request),
    )?;
    if annotation_edits != annotation_edits_again {
        return Err("reference allocation is not deterministic".into());
    }
    let allocated_reference = match &annotation_edits[0].action {
        EditAction::SetReference { reference, .. } => reference.clone(),
        _ => return Err("allocator returned a non-reference edit".into()),
    };
    let annotated = compile(root, &source, &before, &annotation_edits)?;
    let after = capture(
        root,
        source.strip_prefix(root)?,
        &config,
        &output.join("annotation"),
        &annotated,
    )?;
    if partition(&after)? != baseline {
        return Err("reference annotation changed pin connectivity".into());
    }
    let annotated_unit = after
        .identities()
        .units()
        .iter()
        .find(|u| {
            u.source_uuid == id
                && annotation_request
                    .instance_path
                    .as_deref()
                    .is_none_or(|path| u.sheet_path == path)
        })
        .ok_or("annotated identity missing")?;
    if annotated_unit.reference != allocated_reference {
        return Err("native reference annotation mismatch".into());
    }
    private.push(after.private_root().to_owned());
    drop(after);
    let footprint_source = Path::new(
        "/Applications/KiCad/KiCad.app/Contents/SharedSupport/footprints/Resistor_SMD.pretty/R_0603_1608Metric.kicad_mod",
    );
    let pin_pad_map = target_pins
        .iter()
        .map(|pin| (pin.key.number.clone(), pin.key.number.clone()))
        .collect::<BTreeMap<_, _>>();
    let assignment = compile(
        root,
        &source,
        &before,
        &[edit(EditAction::AssignFootprint {
            uuid: id.clone(),
            footprint: "Resistor_SMD:R_0603_1608Metric".into(),
            evidence: FootprintCompatibilityEvidence {
                catalog_digest: before.footprint_catalog_digest().to_owned(),
                source_digest: digest(footprint_source)?,
                pad_numbers: pin_pad_map.values().cloned().collect(),
                pin_pad_map,
            },
        })],
    )?;
    let after = capture(
        root,
        relative,
        &config,
        &output.join("footprint_assignment"),
        &assignment,
    )?;
    if partition(&after)? != baseline {
        return Err("footprint assignment changed pin connectivity".into());
    }
    let component = after
        .netlist()
        .components
        .iter()
        .find(|component| component.reference == reference)
        .ok_or("assigned component missing")?;
    if component.footprint.as_deref() != Some("Resistor_SMD:R_0603_1608Metric") {
        return Err("native footprint assignment mismatch".into());
    }
    private.push(after.private_root().to_owned());
    drop(after);

    let property_edits = vec![
        edit(EditAction::SetProperty {
            uuid: id.clone(),
            property: SymbolProperty::Value,
            value: "20k 中文 \"quoted\" \\ path\nnext\tvalue".into(),
        }),
        edit(EditAction::SetProperty {
            uuid: id.clone(),
            property: SymbolProperty::Named("Acceptance".into()),
            value: "line1\nline2\t✓".into(),
        }),
        edit(EditAction::SetFlags {
            uuid: id.clone(),
            flags: SymbolFlags {
                dnp: Some(true),
                in_bom: Some(false),
                on_board: Some(false),
            },
        }),
    ];
    let properties = compile(root, &source, &before, &property_edits)?;
    let after = capture(
        root,
        relative,
        &config,
        &output.join("properties"),
        &properties,
    )?;
    if partition(&after)? != baseline {
        return Err("property/flag edits changed pin connectivity".into());
    }
    for target in &target_pins {
        let component = after
            .netlist()
            .components
            .iter()
            .find(|c| c.reference == target.reference)
            .ok_or("changed component missing")?;
        if component.value != "20k 中文 \"quoted\" \\ path\nnext\tvalue" {
            return Err("native property text mismatch".into());
        }
    }
    private.push(after.private_root().to_owned());
    drop(after);

    let target = SymbolTransform {
        origin: Point::from_nm(450_000_000, 300_000_000)?,
        rotation: QuarterTurn::Counterclockwise90,
        mirror: Mirror::X,
    };
    let moved = compile(
        root,
        &source,
        &before,
        &[edit(EditAction::TransformSymbol {
            uuid: id.clone(),
            transform: target,
        })],
    )?;
    let after = capture(root, relative, &config, &output.join("transformed"), &moved)?;
    let mut expected = baseline
        .iter()
        .map(|g| {
            g.iter()
                .filter(|p| !selected.contains(*p))
                .cloned()
                .collect::<Vec<_>>()
        })
        .filter(|g| !g.is_empty())
        .collect::<Vec<_>>();
    expected.extend(selected.iter().cloned().map(|p| vec![p]));
    expected.sort();
    if partition(&after)? != expected {
        return Err("moved resistor connectivity differs from explicit disconnection".into());
    }
    for pin in &target_pins {
        let actual = after
            .pins()
            .pins()
            .iter()
            .find(|p| p.key == pin.key)
            .ok_or("moved pin missing")?;
        if actual.occurrences[0].position != target.contact(pin.occurrences[0].library_position)? {
            return Err("native candidate contact mismatch".into());
        }
    }
    private.push(after.private_root().to_owned());
    drop(after);

    let objects = vec![
        NewObject::Wire {
            start: first,
            end: second,
        },
        NewObject::Junction(first),
        NewObject::NoConnect(Point::from_nm(500_000_000, 500_000_000)?),
        NewObject::Label(Label {
            kind: LabelKind::Local,
            text: "AK_EDIT_LOCAL".into(),
            position: Point::from_nm(600_000_000, 500_000_000)?,
            rotation: QuarterTurn::Zero,
        }),
        NewObject::Label(Label {
            kind: LabelKind::Global(ElectricalShape::Passive),
            text: "AK_EDIT_NET".into(),
            position: Point::from_nm(610_000_000, 500_000_000)?,
            rotation: QuarterTurn::Half,
        }),
        NewObject::Label(Label {
            kind: LabelKind::Hierarchical(ElectricalShape::Input),
            text: "AK_EDIT_HIER".into(),
            position: Point::from_nm(620_000_000, 500_000_000)?,
            rotation: QuarterTurn::Counterclockwise90,
        }),
    ];
    let creations = objects
        .into_iter()
        .enumerate()
        .map(|(i, object)| {
            edit(EditAction::Create {
                key: i.to_string(),
                object,
            })
        })
        .collect::<Vec<_>>();
    let created = compile(root, &source, &before, &creations)?;
    let created_dir = output.join("created");
    let created_audit = capture(root, relative, &config, &created_dir, &created)?;
    let mut merged = baseline.clone();
    for reference in target_pins
        .iter()
        .map(|p| p.reference.as_str())
        .collect::<BTreeSet<_>>()
    {
        let component_pins = target_pins
            .iter()
            .filter(|p| p.reference == reference)
            .map(|p| ConnectivityPin::Component(p.key.clone()))
            .collect();
        merged = merge_selected(&merged, &component_pins);
    }
    if partition(&created_audit)? != merged {
        return Err("created wire/labels did not produce the approved merge".into());
    }
    let remote = Point::from_nm(450_000_000, 300_000_000)?;
    let updates = created
        .changes()
        .iter()
        .map(|c| {
            edit(match c.kind {
                ObjectKind::Wire => EditAction::UpdateWire {
                    uuid: c.uuid.clone(),
                    start: first,
                    end: remote,
                },
                ObjectKind::Junction => EditAction::MoveMarker {
                    uuid: c.uuid.clone(),
                    kind: c.kind,
                    position: remote,
                },
                ObjectKind::NoConnect => EditAction::MoveMarker {
                    uuid: c.uuid.clone(),
                    kind: c.kind,
                    position: Point::from_nm(500_001_000, 500_001_000).expect("native grid"),
                },
                _ => EditAction::UpdateLabel {
                    uuid: c.uuid.clone(),
                    label: Label {
                        kind: match c.kind {
                            ObjectKind::LocalLabel => LabelKind::Local,
                            ObjectKind::GlobalLabel => {
                                LabelKind::Global(ElectricalShape::Bidirectional)
                            }
                            _ => LabelKind::Hierarchical(ElectricalShape::Output),
                        },
                        text: format!("AK_UPDATED_{:?}", c.kind),
                        position: Point::from_nm(
                            match c.kind {
                                ObjectKind::LocalLabel => 700_000_000,
                                ObjectKind::GlobalLabel => 710_000_000,
                                _ => 720_000_000,
                            },
                            600_000_000,
                        )
                        .expect("native grid"),
                        rotation: QuarterTurn::Counterclockwise270,
                    },
                },
            })
        })
        .collect::<Vec<_>>();
    let created_root = created_dir.join("project");
    let updated = compile(
        &created_root,
        &created_root.join(relative),
        &created_audit,
        &updates,
    )?;
    let updated_dir = output.join("updated");
    let updated_audit = capture(&created_root, relative, &config, &updated_dir, &updated)?;
    if partition(&updated_audit)? != baseline {
        return Err("updated wire/labels produced unexpected connectivity".into());
    }
    let deletions = created
        .changes()
        .iter()
        .map(|c| {
            edit(EditAction::Delete {
                uuid: c.uuid.clone(),
                kind: c.kind,
            })
        })
        .collect::<Vec<_>>();
    let updated_root = updated_dir.join("project");
    let deleted = compile(
        &updated_root,
        &updated_root.join(relative),
        &updated_audit,
        &deletions,
    )?;
    let deleted_audit = capture(
        &updated_root,
        relative,
        &config,
        &output.join("deleted"),
        &deleted,
    )?;
    if partition(&deleted_audit)? != baseline || !before.erc().equivalent_to(deleted_audit.erc())? {
        return Err("create/update/delete did not restore baseline connectivity/ERC".into());
    }
    for observed in [&created_audit, &updated_audit, &deleted_audit] {
        private.push(observed.private_root().to_owned());
    }
    drop((created_audit, updated_audit, deleted_audit));

    let deleted = compile(
        root,
        &source,
        &before,
        &[edit(EditAction::Delete {
            uuid: id.clone(),
            kind: ObjectKind::Symbol,
        })],
    )?;
    let after = capture(
        root,
        relative,
        &config,
        &output.join("symbol-deleted"),
        &deleted,
    )?;
    let mut expected = baseline
        .iter()
        .map(|g| {
            g.iter()
                .filter(|p| !selected.contains(*p))
                .cloned()
                .collect::<Vec<_>>()
        })
        .filter(|g| !g.is_empty())
        .collect::<Vec<_>>();
    expected.sort();
    if partition(&after)? != expected {
        return Err("symbol deletion changed unrelated connectivity".into());
    }
    private.push(after.private_root().to_owned());
    drop(after);
    before.verify()?;
    drop(before);
    if private.iter().any(|p| p.exists())
        || snapshot(root)? != inputs
        || snapshot(&config)? != configuration
    {
        return Err("acceptance input or private cleanup mismatch".into());
    }
    let summary = serde_json::json!({"passed":true,"native_version":"10.0.7","cases":["properties_flags","symbol_transform","create_wire_markers_labels","update_wire_markers_labels","delete_wire_markers_labels","delete_symbol"],"target_file":file,"reference":reference,"affected_instances":instances,"target_uuid":id,"affected_pin_count":selected.len(),"source_unchanged":true,"configuration_unchanged":true,"private_directories_removed":true,"gui_launched":false});
    fs::write(
        output.join("result.json"),
        serde_json::to_vec_pretty(&summary)?,
    )?;
    println!("{summary}");
    Ok(())
}
