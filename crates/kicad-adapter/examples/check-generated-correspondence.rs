//! Compare generated candidates and identity-bound DRC on two sequential owned editors.
use kicad_adapter::{
    geometry::observe_generated_context,
    geometry_candidate::GeometryCandidateEvidence,
    lifecycle::{DisposablePcbLifecycle, OwnedPcbnewLauncher},
    live_geometry::GeneratedZoneCorrespondence,
    teardrop_refill::{TeardropRefillObservation, VerifiedTeardropRefill},
};
use kicad_checks::{DrcPolicy, refill::refill_closed_copy};
use kicad_ipc::{ZoneFillAttempt, board_types};
use protobuf::Message;
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    env, fs,
    path::{Path, PathBuf},
    process::Command,
    time::Duration,
};
type Error = Box<dyn std::error::Error>;
struct Candidate {
    typed: GeometryCandidateEvidence,
    generated: GeneratedZoneCorrespondence,
    authored: String,
    ordinary: Vec<Vec<u8>>,
    drc: Value,
}
fn canonical_report(report: &Value, map: &BTreeMap<String, String>) -> Value {
    let mut result = report.clone();
    result.as_object_mut().unwrap().remove("date");
    result.as_object_mut().unwrap().remove("source");
    for section in ["violations", "unconnected_items", "schematic_parity"] {
        if let Some(violations) = result[section].as_array_mut() {
            for violation in violations.iter_mut() {
                if let Some(items) = violation["items"].as_array_mut() {
                    for item in items.iter_mut() {
                        if let Some(id) = item["uuid"].as_str().and_then(|id| map.get(id)) {
                            item["uuid"] = json!(id);
                        }
                    }
                    items.sort_by_key(Value::to_string);
                }
            }
            violations.sort_by_key(Value::to_string);
        }
    }
    result
}
fn cli(board: &Path, report: &Path) -> Result<Value, Error> {
    let status = Command::new("kicad-cli")
        .args(["pcb", "drc", "--format", "json", "--output"])
        .arg(report)
        .arg(board)
        .status()?;
    if !status.success() {
        return Err("native CLI DRC failed".into());
    }
    let result: Value = serde_json::from_slice(&fs::read(report)?)?;
    if result["kicad_version"] != "10.0.7" {
        return Err("wrong native DRC version".into());
    }
    Ok(result)
}
fn candidate(source: &Path, root: &Path) -> Result<Candidate, Error> {
    fs::create_dir(root)?;
    let mut editor = OwnedPcbnewLauncher::new(&root.join("work"))?;
    let copy = editor.create_shadow(source)?;
    let identity = editor.launch(&copy)?;
    fs::write(
        root.join("owned.json"),
        serde_json::to_vec_pretty(&json!({"pid":identity.pid}))?,
    )?;
    let result: Result<Candidate, Error> = (|| {
        let mut client = editor.connect_owned()?;
        client.save_board(&identity.board_filename)?;
        editor.reload_owned()?;
        let protected = fs::read(copy.project_path())?;
        let board_id = kicad_model::StableId::new("generated-candidate-board")?;
        let context = observe_generated_context(&mut client, &identity.board_filename, &board_id)?;
        fs::write(
            root.join("inspection.json"),
            serde_json::to_vec_pretty(&context.summary()?)?,
        )?;
        let request: kicad_model::geometry::GeometryRequest = serde_json::from_value(json!({
            "observed_revision":context.context().revision.to_string(),
            "outline":null,
            "zones":[{"kind":"update","source_uuid":"80000000-0000-4000-8000-000000000001",
                      "patch":{"clearance_nm":250000}}],
            "refill":"all"
        }))?;
        let mut forbidden = serde_json::to_value(&request)?;
        let generated_id = context
            .generated()
            .ok_or("complete observation missing")?
            .native()
            .zones()
            .iter()
            .find(|(_, z)| z.type_.enum_value() == Ok(board_types::ZoneType::ZT_TEARDROP))
            .ok_or("generated fixture missing")?
            .0;
        forbidden["zones"][0]["source_uuid"] = json!(generated_id);
        if context.prepare(&serde_json::from_value(forbidden)?).is_ok() {
            return Err("generated region accepted as an authored mutation target".into());
        }
        let prepared = context.prepare(&request)?;
        let transaction = client.begin_commit()?;
        let receipts = prepared.apply_in_transaction(&mut client, &identity.board_filename)?;
        client.commit_changes(&transaction, "Verify typed geometry with generated regions")?;
        let mutated = TeardropRefillObservation::capture(&mut client, &identity.board_filename)?;
        prepared.verify_generated_definitions(&mutated, &receipts)?;
        if prepared
            .verify_definitions(&mutated.authored_geometry()?, &receipts)
            .is_ok()
        {
            return Err("ordinary definition verifier accepted omitted generated content".into());
        }
        client.save_board(&identity.board_filename)?;
        editor.reload_owned()?;
        let before = TeardropRefillObservation::capture(&mut client, &identity.board_filename)?;
        if prepared
            .verify_generated_definitions(&before, &receipts)
            .is_ok()
        {
            return Err(
                "same-document mutation verification accepted reloaded generated identities".into(),
            );
        }
        prepared.verify_generated_definitions_after_reload(&before, &receipts)?;
        client = match client.request_zone_refill(&identity.board_filename) {
            ZoneFillAttempt::Returned { client, .. } => client,
            _ => return Err("native refill uncertain".into()),
        };
        let filled = TeardropRefillObservation::capture(&mut client, &identity.board_filename)?;
        client.save_board(&identity.board_filename)?;
        let saved = fs::read_to_string(copy.board_path())?;
        let independent = refill_closed_copy(
            copy.board_path(),
            &DrcPolicy {
                allowed_root: copy.root().to_owned(),
                timeout: Duration::from_secs(60),
            },
        )
        .map_err(|e| format!("{e:?}"))?;
        if independent.input_board() != saved {
            return Err("independent input differs".into());
        }
        editor.reload_owned()?;
        let after = TeardropRefillObservation::capture(&mut client, &identity.board_filename)?;
        let refill = VerifiedTeardropRefill::verify(&before, &filled, &after, &independent)?;
        let typed = GeometryCandidateEvidence::capture_generated(
            &prepared,
            &receipts,
            &before,
            &filled,
            &after,
            &independent,
        )?;
        if GeometryCandidateEvidence::capture(
            &prepared,
            &receipts,
            &before.authored_geometry()?,
            &after.authored_geometry()?,
            &independent,
        )
        .is_ok()
        {
            return Err("ordinary candidate accepted omitted generated content".into());
        }
        fs::write(
            root.join("typed-candidate.json"),
            serde_json::to_vec_pretty(&typed)?,
        )?;
        fs::write(
            root.join("typed-request.json"),
            serde_json::to_vec_pretty(&request)?,
        )?;

        if after.text().generated_count() != 10 {
            return Err("unexpected generated count".into());
        }
        let snapshot = after.drc_snapshot(&saved)?;
        let bound = root.join("bound.kicad_pcb");
        fs::write(&bound, snapshot.text())?;
        fs::write(bound.with_extension("kicad_pro"), &protected)?;
        fs::write(root.join("result.kicad_pcb"), &saved)?;
        fs::write(root.join("result.kicad_pro"), &protected)?;
        fs::write(
            root.join("snapshot.json"),
            serde_json::to_vec_pretty(&snapshot)?,
        )?;
        fs::write(
            root.join("refill.json"),
            serde_json::to_vec_pretty(&refill)?,
        )?;
        let first = cli(&bound, &root.join("drc-0.json"))?;
        let second = cli(&bound, &root.join("drc-1.json"))?;
        if canonical_report(&first, &BTreeMap::new()) != canonical_report(&second, &BTreeMap::new())
        {
            return Err("bound DRC identities changed".into());
        }
        if fs::read(copy.project_path())? != protected
            || fs::read_to_string(copy.board_path())? != saved
        {
            return Err("source snapshot or project changed".into());
        }
        let repeated = TeardropRefillObservation::capture(&mut client, &identity.board_filename)?;
        if after.native() != repeated.native()
            || after.text().raw_text() != repeated.text().raw_text()
        {
            return Err("native result changed during DRC".into());
        }
        let mut ordinary = Vec::new();
        for zone in after
            .native()
            .zones()
            .values()
            .filter(|z| z.type_.enum_value() != Ok(board_types::ZoneType::ZT_TEARDROP))
        {
            let mut zone = zone.clone();
            zone.parent = None.into();
            ordinary.push(zone.write_to_bytes()?);
        }
        for shape in after.native().shapes().values() {
            let mut shape = shape.clone();
            shape.parent = None.into();
            ordinary.push(shape.write_to_bytes()?);
        }
        let generated = after.generated_correspondence();
        fs::write(
            root.join("correspondence.json"),
            serde_json::to_vec_pretty(&generated)?,
        )?;
        Ok(Candidate {
            typed,
            generated,
            authored: after.text().authored().structure_digest().into(),
            ordinary,
            drc: first,
        })
    })();
    let failure = result.as_ref().err().map(ToString::to_string);
    editor.close_owned(identity.pid)?;
    editor.cleanup()?;
    fs::write(
        root.join("cleanup.json"),
        serde_json::to_vec_pretty(
            &json!({"pid":identity.pid,"closed":true,"passed":result.is_ok(),"failure":failure}),
        )?,
    )?;
    result
}
fn main() -> Result<(), Error> {
    let args = env::args().skip(1).collect::<Vec<_>>();
    if args.len() != 2 {
        return Err("usage: check-generated-correspondence SOURCE NEW_OUTPUT".into());
    }
    let source = fs::canonicalize(&args[0])?;
    fs::create_dir(&args[1])?;
    let root = fs::canonicalize(PathBuf::from(&args[1]))?;
    let originals = ["kicad_pcb", "kicad_pro", "kicad_dru", "kicad_sch"].map(|ext| {
        let p = source.with_extension(ext);
        let bytes = fs::read(&p).ok();
        (p, bytes)
    });
    let result: Result<(), Error> = (|| {
        let preview = candidate(&source, &root.join("preview"))?;
        let actual = candidate(&source, &root.join("source"))?;
        let map = preview.generated.equivalent_to(&actual.generated)?;
        if !preview.typed.equivalent_to(&actual.typed)?.is_empty() {
            return Err("clearance-only edit unexpectedly created authored identities".into());
        }
        if preview.typed.generated_mapping(&actual.typed)? != map {
            return Err("typed and independent generated identity mappings differ".into());
        }

        if map.len() != 10 || map.iter().any(|(a, b)| a == b) {
            return Err("expected ten differing generated identities".into());
        }
        if preview.authored != actual.authored || preview.ordinary != actual.ordinary {
            return Err("complete authored candidate content differs".into());
        }
        if canonical_report(&preview.drc, &map) != canonical_report(&actual.drc, &BTreeMap::new()) {
            return Err("full mapped raw DRC differs".into());
        }
        if canonical_report(&preview.drc, &BTreeMap::new())
            == canonical_report(&actual.drc, &BTreeMap::new())
        {
            return Err("fixture did not exercise generated DRC identity differences".into());
        }
        fs::write(
            root.join("acceptance.json"),
            serde_json::to_vec_pretty(
                &json!({"passed":true,"typed_geometry_candidate_verified":true,"generated_edit_refused":true,"omitted_generated_candidate_refused":true,"omitted_generated_definitions_refused":true,"generated_identity_change_requires_reload_verifier":true,"kicad_version":"10.0.7","generated_map":map,"authored_digest":preview.authored,"full_ordinary_native_equal":true,"full_generated_content_equal":true,"full_mapped_raw_drc_equal":true,"unmapped_drc_rejected":true}),
            )?,
        )?;
        Ok(())
    })();
    let unchanged = originals.into_iter().all(|(p, b)| fs::read(p).ok() == b);
    fs::write(
        root.join("source-check.json"),
        serde_json::to_vec_pretty(&json!({"source_unchanged":unchanged,"passed":result.is_ok()}))?,
    )?;
    if !unchanged {
        return Err("original source changed".into());
    }
    result
}
