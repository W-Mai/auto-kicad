//! Publish matched native candidates on explicitly owned copies and verify complete durable results.
use auto_kicad::recovery::ExecutionStore;
use kicad_adapter::schematic_audit::{
    ConnectivityExpectation, ErcExpectation, SchematicAudit, SchematicAuditPolicy,
    SchematicEditExpectation,
};
use kicad_schematic::editing::{
    EditAction, EditScope, NewObject, SchematicEdit, SymbolCreation, SymbolFlags,
    SymbolInstancePath, SymbolProperty,
};
use kicad_schematic::geometry::{Mirror, Point, QuarterTurn, SymbolTransform};
use kicad_schematic::probes::{ConnectivityNet, ConnectivityPin};
use serde_json::json;
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    env, fs,
    path::{Path, PathBuf},
};
type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

fn digest(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}
fn snapshot(root: &Path) -> Result<BTreeMap<PathBuf, String>> {
    let mut stack = vec![root.to_owned()];
    let mut files = BTreeMap::new();
    let mut total = 0;
    while let Some(path) = stack.pop() {
        let metadata = fs::symlink_metadata(&path)?;
        if metadata.file_type().is_symlink() {
            return Err("symlink in owned input".into());
        }
        if metadata.is_dir() {
            for entry in fs::read_dir(path)? {
                stack.push(entry?.path());
            }
        } else if metadata.is_file() {
            if metadata.len() > 32 * 1024 * 1024 {
                return Err("input file exceeds bound".into());
            }
            let bytes = fs::read(&path)?;
            total += bytes.len();
            if total > 128 * 1024 * 1024 || files.len() >= 8192 {
                return Err("input snapshot exceeds bound".into());
            }
            files.insert(path.strip_prefix(root)?.to_owned(), digest(&bytes));
        } else {
            return Err("special file in owned input".into());
        }
    }
    Ok(files)
}

fn property_edits(audit: &SchematicAudit) -> Result<Vec<SchematicEdit>> {
    let mut targets = BTreeMap::new();
    for unit in audit.identities().units() {
        if unit.reference.starts_with('R') {
            targets.entry(unit.file.clone()).or_insert(unit);
        }
    }
    if targets.is_empty() {
        return Err("owned fixture needs resistors".into());
    }
    Ok(targets
        .values()
        .enumerate()
        .map(|(index, unit)| SchematicEdit {
            file: unit.file.clone(),
            scope: EditScope::FileAllInstances,
            action: EditAction::SetProperty {
                uuid: unit.source_uuid.clone(),
                property: SymbolProperty::Value,
                value: format!("AK_NATIVE_{index}"),
            },
        })
        .collect())
}

fn wire_edit(audit: &SchematicAudit) -> Result<(Vec<SchematicEdit>, SchematicEditExpectation)> {
    let unit = audit
        .identities()
        .units()
        .iter()
        .find(|unit| unit.reference == "R2")
        .ok_or("wire fixture needs R2")?;
    let pins = audit
        .pins()
        .pins()
        .iter()
        .filter(|pin| {
            pin.occurrences.iter().any(|occurrence| {
                occurrence.source.file == unit.file
                    && occurrence.source.symbol_uuid == unit.source_uuid
            })
        })
        .collect::<Vec<_>>();
    if pins.len() != 2 || pins.iter().any(|pin| pin.occurrences.len() != 1) {
        return Err("wire fixture needs single-instance resistor".into());
    }
    let selected = pins
        .iter()
        .map(|pin| ConnectivityPin::Component(pin.key.clone()))
        .collect::<Vec<_>>();
    let baseline = audit.observe_pin_connectivity()?;
    let removed = baseline
        .connectivity()
        .nets()
        .iter()
        .filter(|net| net.pins.iter().any(|pin| selected.contains(pin)))
        .cloned()
        .collect::<Vec<_>>();
    if removed.len() != 2
        || removed.iter().filter(|net| net.name == "GND").count() != 1
        || removed
            .iter()
            .any(|net| net.class != Some("Default".into()))
    {
        return Err("wire fixture needs separate GND and component nets".into());
    }
    let mut joined = ConnectivityNet {
        name: "GND".into(),
        class: Some("Default".into()),
        pins: removed.iter().flat_map(|net| net.pins.clone()).collect(),
    };
    joined.pins.sort();
    Ok((
        vec![SchematicEdit {
            file: unit.file.clone(),
            scope: EditScope::FileAllInstances,
            action: EditAction::Create {
                key: "native-wire".into(),
                object: NewObject::Wire {
                    start: pins[0].occurrences[0].position,
                    end: pins[1].occurrences[0].position,
                },
            },
        }],
        SchematicEditExpectation {
            connectivity: ConnectivityExpectation {
                removed_nets: removed,
                added_nets: vec![joined],
                ..Default::default()
            },
            ..Default::default()
        },
    ))
}

fn main() -> Result<()> {
    let args = env::args_os().skip(1).collect::<Vec<_>>();
    if args.len() != 4 {
        return Err("usage: check-native-schematic-publication OWNED_SCHEMATIC OWNED_CONFIG NEW_OUTPUT properties|wire|symbol".into());
    }
    let source = fs::canonicalize(&args[0])?;
    let project = source.parent().ok_or("missing schematic parent")?;
    let config = fs::canonicalize(&args[1])?;
    let output = PathBuf::from(&args[2]);
    let mode = args[3].to_str().ok_or("invalid mode")?;
    if !matches!(mode, "properties" | "wire" | "symbol") {
        return Err("invalid publication mode".into());
    }
    for root in [project, &config] {
        if fs::read_to_string(root.join(".auto-kicad-audit-owned"))?
            != "schematic-audit-acceptance\n"
        {
            return Err("owned-copy marker missing".into());
        }
    }
    if output.starts_with(project) || output.starts_with(&config) {
        return Err("output overlaps input".into());
    }
    fs::create_dir(&output)?;
    let output = fs::canonicalize(output)?;
    let store = ExecutionStore::open(&output.join("state"))?;
    let writer = store.acquire(project)?;
    let before = snapshot(project)?;
    let configuration = snapshot(&config)?;
    let shared = Path::new("/Applications/KiCad/KiCad.app/Contents/SharedSupport");
    let policy = SchematicAuditPolicy::new(
        project,
        &source,
        &config,
        &shared.join("symbols"),
        &shared.join("footprints"),
        &[],
    )?;
    let audit = SchematicAudit::prepare(&policy)?;
    let mut controls = Vec::new();
    if mode == "properties" {
        let edits = property_edits(&audit)?;
        let preview = audit.preview_edits(&edits)?;
        let matched = preview.match_expectations(&SchematicEditExpectation::default())?;
        let wrong_project = output.join("wrong-project");
        fs::create_dir(&wrong_project)?;
        let other = store.acquire(&wrong_project)?;
        let error = other
            .publish_matched_schematic("wrong-plan", "wrong-approval", &matched)
            .unwrap_err();
        if error.unknown || error.attempt_id.is_some() || !store.history(&wrong_project)?.is_empty()
        {
            return Err("wrong project created durable uncertainty".into());
        }
        drop(other);
        drop(matched);
        drop(preview);
        controls.push("wrong_project_rejected".to_owned());
        let mut paths = vec![source.clone(), config.join("10.0/kicad_common.json")];
        for entry in fs::read_dir(project)? {
            let path = entry?.path();
            if path.extension().and_then(|value| value.to_str()) == Some("kicad_sym") {
                paths.push(path);
            }
        }
        for path in paths {
            let preview = audit.preview_edits(&edits)?;
            let matched = preview.match_expectations(&SchematicEditExpectation::default())?;
            let original = fs::read(&path)?;
            let mut changed = original.clone();
            changed.extend_from_slice(b"\n");
            fs::write(&path, changed)?;
            let error = writer
                .publish_matched_schematic("stale-plan", "stale-approval", &matched)
                .unwrap_err();
            fs::write(&path, original)?;
            if error.unknown
                || error.attempt_id.is_some()
                || matched.verify().is_ok()
                || !store.history(project)?.is_empty()
            {
                return Err("stale input did not reject before durable preparation".into());
            }
            controls.push(format!("stale_input_rejected:{}", path.display()));
        }
    }
    let root = source
        .file_name()
        .and_then(|value| value.to_str())
        .ok_or("invalid basename")?
        .to_owned();
    let (edits, mut expected) = match mode {
        "wire" => wire_edit(&audit)?,
        "symbol" => (
            vec![SchematicEdit {
                file: root.clone(),
                scope: EditScope::FileAllInstances,
                action: EditAction::CreateSymbol {
                    key: "native-published-symbol".into(),
                    symbol: SymbolCreation {
                        evidence: audit.symbol_definition("Device:R")?,
                        transform: SymbolTransform {
                            origin: Point::from_nm(609_600_000, 508_000_000)?,
                            rotation: QuarterTurn::Zero,
                            mirror: Mirror::None,
                        },
                        unit: 1,
                        body_style: None,
                        reference: "R900".into(),
                        value: "1k".into(),
                        footprint: None,
                        flags: SymbolFlags::default(),
                        instance_paths: audit
                            .identities()
                            .sheets()
                            .iter()
                            .filter(|sheet| sheet.file == root)
                            .map(|sheet| SymbolInstancePath {
                                path: sheet.path.clone(),
                                reference: None,
                            })
                            .collect(),
                    },
                },
            }],
            SchematicEditExpectation::default(),
        ),
        _ => (property_edits(&audit)?, SchematicEditExpectation::default()),
    };
    let preview = audit.preview_edits(&edits)?;
    fs::write(
        output.join("preview.json"),
        serde_json::to_vec_pretty(preview.evidence())?,
    )?;
    if mode == "symbol" {
        let uuid = &preview.candidate().changes()[0].uuid;
        let pins = preview
            .observed()
            .pins()
            .pins()
            .iter()
            .filter(|pin| {
                pin.key
                    .component_units
                    .iter()
                    .any(|unit| unit.symbol_uuid == *uuid)
            })
            .map(|pin| ConnectivityPin::Component(pin.key.clone()))
            .collect::<Vec<_>>();
        if pins.len() != 2 {
            return Err("created resistor needs exactly two logical pins".into());
        }
        let nets = pins
            .iter()
            .map(|pin| {
                let ConnectivityPin::Component(key) = pin else {
                    unreachable!()
                };
                ConnectivityNet {
                    name: format!("unconnected-(R900-Pad{})", key.number),
                    class: Some("Default".into()),
                    pins: vec![pin.clone()],
                }
            })
            .collect();
        expected.connectivity.added_pins = pins;
        expected.connectivity.added_nets = nets;
        if preview.erc_delta().introduced.len() != 2
            || preview.erc_delta().introduced.iter().any(|diagnostic| {
                !diagnostic.diagnostic_json.contains("pin_not_connected")
                    || !diagnostic.diagnostic_json.contains("R900")
            })
            || !preview.erc_delta().resolved.is_empty()
        {
            return Err(
                "created resistor needs exactly its two explicit unconnected-pin diagnostics"
                    .into(),
            );
        }
        expected.erc = ErcExpectation {
            introduced: preview.erc_delta().introduced.clone(),
            resolved: vec![],
        };
    }
    if mode != "properties"
        && preview
            .match_expectations(&SchematicEditExpectation::default())
            .is_ok()
    {
        return Err("electrical changes accepted without explicit expectations".into());
    }
    let matched = preview.match_expectations(&expected)?;
    fs::write(
        output.join("matched.json"),
        serde_json::to_vec_pretty(matched.evidence())?,
    )?;
    let mut expected_project = before.clone();
    for relative in preview.candidate().replacements().keys() {
        expected_project.insert(
            PathBuf::from(relative),
            digest(&preview.candidate().files()[relative]),
        );
    }
    let receipt = writer.publish_matched_schematic(
        "native-matched-plan",
        "native-matched-approval",
        &matched,
    )?;
    if matched.verify().is_ok() || matched.prepare_publication().is_ok() {
        return Err("publication did not consume all preview handles".into());
    }
    writer.ensure_ready()?;
    if snapshot(project)? != expected_project || snapshot(&config)? != configuration {
        return Err("published source or protected configuration differs".into());
    }
    let records = store.history(project)?;
    if records.len() != 1
        || records[0].state != "committed"
        || records[0].attempt_id != receipt.attempt_id
        || records[0].post_attempt.is_none()
    {
        return Err("native publication did not persist exact completion".into());
    }
    let artifacts = records[0].evidence["artifacts"]
        .as_array()
        .ok_or("artifact references missing")?;
    for artifact in artifacts {
        let bytes = fs::read(artifact["path"].as_str().ok_or("artifact path missing")?)?;
        if artifact["sha256"].as_str() != Some(digest(&bytes).as_str())
            || artifact["bytes"].as_u64() != Some(bytes.len() as u64)
        {
            return Err("retained native evidence digest differs".into());
        }
    }
    if artifacts.len() != preview.candidate().replacements().len() + 3 {
        return Err("complete native evidence/candidate artifacts were not retained".into());
    }
    let verification = artifacts
        .iter()
        .find(|entry| entry["name"] == "verification.json")
        .ok_or("native verification artifact missing")?;
    let proof: serde_json::Value =
        serde_json::from_slice(&fs::read(verification["path"].as_str().unwrap())?)?;
    if proof["published_context"]["private_trees"]
        .as_object()
        .ok_or("private-tree evidence missing")?
        .keys()
        .any(|path| Path::new(path).exists())
    {
        return Err("final native audit snapshots leaked".into());
    }
    let replay = writer
        .publish_matched_schematic("native-matched-plan", "native-matched-approval", &matched)
        .unwrap_err();
    if replay.unknown || replay.attempt_id.is_some() || snapshot(project)? != expected_project {
        return Err("consumed preview replay changed source".into());
    }
    let final_audit = SchematicAudit::prepare(&policy)?;
    if final_audit.netlist() != preview.observed().netlist()
        || !final_audit.erc().equivalent_to(preview.observed().erc())?
        || final_audit.observe_pin_connectivity()?.connectivity() != preview.connectivity_after()
    {
        return Err("independent final native audit differs from preview".into());
    }
    fs::write(
        output.join("final-native.json"),
        serde_json::to_vec_pretty(&final_audit.evidence()?)?,
    )?;
    fs::write(
        output.join("receipt.json"),
        serde_json::to_vec_pretty(&receipt)?,
    )?;
    let changed_files = preview.candidate().replacements().len();
    let affected_instances = preview
        .candidate()
        .changes()
        .iter()
        .map(|change| change.instance_paths.len())
        .sum::<usize>();
    let private = [
        audit.private_root().to_owned(),
        preview.observed().private_root().to_owned(),
        final_audit.private_root().to_owned(),
    ];
    drop(final_audit);
    drop(matched);
    drop(preview);
    drop(audit);
    drop(writer);
    if private.iter().any(|path| path.exists()) {
        return Err("native audit snapshots were not removed".into());
    }
    let result = json!({"passed":true,"mode":mode,"native_version":"10.0.7","changed_files":changed_files,"affected_instances":affected_instances,
        "controls":controls,"exact_candidate_published":true,"complete_actual_native_result_matched":true,"durable_native_evidence_and_candidates_retained":true,
        "consumed_preview_and_replay_rejected":true,"protected_inputs_unchanged":true,"private_snapshots_removed":true,"gui_launched":false,"public_general_edit_mcp":false});
    fs::write(
        output.join("result.json"),
        serde_json::to_vec_pretty(&result)?,
    )?;
    println!("{}", serde_json::to_string(&result)?);
    Ok(())
}
