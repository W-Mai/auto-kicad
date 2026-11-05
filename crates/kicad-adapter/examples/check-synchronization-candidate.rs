//! Qualify synchronization candidates on explicitly owned schematic and PCB copies.
use kicad_adapter::schematic_audit::synchronization::board::{
    SynchronizationAction, SynchronizationCopperNet, SynchronizationIntent,
    SynchronizationPlacement, SynchronizationSide,
};
use kicad_adapter::schematic_audit::{SchematicAudit, SchematicAuditPolicy};
use kicad_schematic::{
    Limits, SchematicPatchOutcome, create_two_resistor_circuit, load_schematic_project,
};
use serde_json::json;
use std::{env, fs, path::PathBuf};
type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

fn main() -> Result<()> {
    let args = env::args().collect::<Vec<_>>();
    if args.len() != 5 {
        return Err("MODE OWNED_PROJECT OWNED_CONFIG NEW_OUTPUT required".into());
    }
    let project = fs::canonicalize(&args[2])?;
    let config = fs::canonicalize(&args[3])?;
    let output = PathBuf::from(&args[4]);
    fs::create_dir(&output)?;
    if fs::read_to_string(project.join(".auto-kicad-audit-owned"))?
        != "schematic-audit-acceptance\n"
    {
        return Err("owned fixture marker missing".into());
    }
    let schematic = project.join("sync.kicad_sch");
    if args[1] == "initialize" {
        let parsed = load_schematic_project(&project, &schematic, "10.0.7", 1, Limits::default())?;
        let before = parsed
            .files
            .iter()
            .find(|f| f.artifact.relative_path == "sync.kicad_sch")
            .ok_or("missing root")?;
        let outcome = create_two_resistor_circuit(&project, &schematic, &before.artifact);
        if !matches!(outcome, SchematicPatchOutcome::Committed { .. }) {
            return Err(format!("fixed source creation failed: {outcome:?}").into());
        }
        let raw = fs::read_to_string(&schematic)?;
        fs::write(
            &schematic,
            raw.replace(r#"(project "" "#, r#"(project "sync" "#),
        )?;
    } else if args[1] != "preview"
        && args[1] != "source"
        && args[1] != "addition"
        && args[1] != "replacement"
        && args[1] != "intent"
        && args[1] != "publication-proof"
    {
        return Err("unknown qualification mode".into());
    }
    let policy = SchematicAuditPolicy::new(
        &project,
        &schematic,
        &config,
        &PathBuf::from("/Applications/KiCad/KiCad.app/Contents/SharedSupport/symbols"),
        &PathBuf::from("/Applications/KiCad/KiCad.app/Contents/SharedSupport/footprints"),
        &[],
    )?;
    let audit = SchematicAudit::prepare(&policy)?;
    let source = audit.synchronization_source()?;
    fs::write(
        output.join("source.json"),
        serde_json::to_vec_pretty(&source.evidence()?)?,
    )?;
    if args[1] != "preview"
        && args[1] != "addition"
        && args[1] != "replacement"
        && args[1] != "intent"
        && args[1] != "publication-proof"
    {
        println!("{} authoritative components", source.components().len());
        return Ok(());
    }
    let board = project.join("sync.kicad_pcb");
    let original = fs::read(&board)?;
    let observation = source.observe_board(&board)?;
    let mut intent = SynchronizationIntent {
        project_netclasses: vec![],
        zone_nets: Vec::new(),
        schema_version: 1,
        actions: source
            .components()
            .iter()
            .filter(|c| c.eligible_for_board())
            .map(|c| {
                if args[1] == "addition" {
                    let placements = project.join(".auto-kicad-sync-placements.json");
                    let placement = if placements.exists() {
                        serde_json::from_slice::<
                            std::collections::BTreeMap<String, SynchronizationPlacement>,
                        >(&fs::read(placements).expect("owned placements"))
                        .expect("typed placements")
                        .get(c.reference())
                        .expect("component placement")
                        .clone()
                    } else {
                        SynchronizationPlacement {
                            position_nm: [
                                if c.reference() == "R1" {
                                    30_000_000
                                } else {
                                    40_000_000
                                },
                                25_000_000,
                            ],
                            orientation_mdeg: 0,
                            side: SynchronizationSide::Front,
                        }
                    };
                    SynchronizationAction::Add {
                        component_id: c.id().into(),
                        placement,
                        fields: vec!["Datasheet".into()],
                    }
                } else if args[1] == "replacement"
                    && observation.board().footprints().values().any(|fp| {
                        fp.path.as_deref().is_some_and(|path| {
                            c.aliases()
                                .iter()
                                .any(|alias| alias == path.trim_end_matches('/'))
                        }) && Some(fp.library.as_str()) != c.footprint()
                    })
                {
                    let target = observation
                        .board()
                        .footprints()
                        .values()
                        .find(|fp| {
                            fp.path.as_deref().is_some_and(|path| {
                                c.aliases()
                                    .iter()
                                    .any(|alias| alias == path.trim_end_matches('/'))
                            })
                        })
                        .expect("owned replacement link");
                    SynchronizationAction::Replace {
                        component_id: c.id().into(),
                        footprint_uuid: target.uuid.clone(),
                        fields: vec!["Datasheet".into()],
                    }
                } else {
                    SynchronizationAction::Update {
                        component_id: c.id().into(),
                        fields: vec![],
                        update_nets: true,
                    }
                }
            })
            .collect(),
        copper_nets: vec![],
    };
    if args[1] == "intent" {
        intent = serde_json::from_slice(&fs::read(project.join(".auto-kicad-sync-intent.json"))?)?;
    }
    let copper = project.join(".auto-kicad-sync-copper.json");
    let mut copper_conflict = false;
    if copper.exists() {
        let rejected = observation.preview(&intent);
        assert!(
            rejected.is_err(),
            "connected pad-only net changes must reject"
        );
        copper_conflict = true;
        intent.copper_nets =
            serde_json::from_slice::<Vec<SynchronizationCopperNet>>(&fs::read(copper)?)?;
    }
    fs::write(
        output.join("intent.json"),
        serde_json::to_vec_pretty(&intent)?,
    )?;
    let preview_result = observation.preview(&intent);
    let expected_error = project.join(".auto-kicad-sync-expected-error.json");
    if expected_error.exists() {
        let expected: String = serde_json::from_slice(&fs::read(expected_error)?)?;
        let message = match preview_result {
            Err(message) => message,
            Ok(_) => return Err("expected native constraint conflict was accepted".into()),
        };
        if !message.contains(&expected) {
            return Err(format!("wrong native constraint conflict: {message}").into());
        }
        assert_eq!(fs::read(&board)?, original);
        fs::write(
            output.join("result.json"),
            serde_json::to_vec_pretty(
                &json!({"expected_native_conflict":true,"message":message,"source_unchanged":true,"public_mcp_accepted":false,"product_complete":false}),
            )?,
        )?;
        return Ok(());
    }
    let preview = preview_result?;
    preview.verify()?;
    let delta = preview.drc_delta();
    let expected = kicad_checks::CompleteDrcExpectation {
        introduced: delta.introduced.clone(),
        resolved: delta.resolved.clone(),
    };
    preview.match_drc_expectation(&expected)?;
    let undeclared_rejected = preview
        .match_drc_expectation(&kicad_checks::CompleteDrcExpectation::default())
        .is_err();
    if !delta.introduced.is_empty() || !delta.resolved.is_empty() {
        assert!(
            undeclared_rejected,
            "native DRC changes must be declared completely"
        );
        let mut wrong = expected.clone();
        if let Some(row) = wrong.introduced.first_mut().or(wrong.resolved.first_mut()) {
            let mut diagnostic: serde_json::Value = serde_json::from_str(&row.diagnostic_json)?;
            diagnostic["description"] = json!("same-count corruption");
            row.diagnostic_json = serde_json::to_string(&diagnostic)?;
        }
        assert!(
            preview.match_drc_expectation(&wrong).is_err(),
            "same counts must not grant diagnostic equivalence"
        );
    }
    fs::write(
        output.join("drc-expectation.json"),
        serde_json::to_vec_pretty(&expected)?,
    )?;
    assert_eq!(fs::read(&board)?, original, "preview changed actual source");
    fs::write(
        output.join("candidate.kicad_pcb"),
        preview.candidate().bytes(),
    )?;
    fs::copy(
        board.with_extension("kicad_pro"),
        output.join("candidate.kicad_pro"),
    )?;
    fs::write(
        output.join("preview.json"),
        serde_json::to_vec_pretty(preview.evidence())?,
    )?;
    let artifacts = output.join("templates");
    for (id, name, bytes) in preview.native_template_artifacts() {
        fs::create_dir_all(artifacts.join(id))?;
        fs::write(artifacts.join(id).join(name), bytes)?;
    }
    let mut actual_publication_proof = false;
    if args[1] == "publication-proof" {
        use kicad_adapter::schematic_audit::synchronization::SynchronizationExpectation;
        let complete = SynchronizationExpectation {
            regions: None,
            differences: preview.candidate().differences().to_vec(),
            drc: expected.clone(),
        };
        let mut wrong = complete.clone();
        wrong
            .differences
            .first_mut()
            .ok_or("publication probe needs a changed candidate")?
            .after = Some("same-count unapproved change".into());
        assert!(
            preview.match_expectation(wrong).is_err(),
            "inexact complete differences must reject"
        );
        let mut duplicate = complete.clone();
        duplicate.differences.push(duplicate.differences[0].clone());
        assert!(
            preview.match_expectation(duplicate).is_err(),
            "duplicate differences must reject"
        );
        let matched = preview.match_expectation(complete)?;
        fs::write(
            output.join("matching.json"),
            serde_json::to_vec_pretty(
                &json!({"matched_digest":matched.digest(),"expectation":matched.expectation()}),
            )?,
        )?;
        let prepared = matched.prepare_publication()?;
        prepared.verify_before_write()?;
        assert!(
            prepared.verify_published().is_err(),
            "original PCB cannot supply a published-candidate proof"
        );
        fs::write(&board, prepared.candidate_bytes())?;
        fs::File::open(&board)?.sync_all()?;
        let verified = prepared.verify_published().and_then(|proof| {
            proof.verify()?;
            fs::write(
                output.join("actual-publication-proof.json"),
                serde_json::to_vec_pretty(proof.evidence()).map_err(|e| e.to_string())?,
            )
            .map_err(|e| e.to_string())?;
            Ok(proof)
        });
        // Restore only the exact owned candidate, preserving any unexpected later bytes.
        if fs::read(&board)? != prepared.candidate_bytes() {
            return Err(
                "owned publication probe source changed; manual inspection required".into(),
            );
        }
        fs::write(&board, &original)?;
        fs::File::open(&board)?.sync_all()?;
        let proof = verified?;
        assert!(
            proof.verify().is_err(),
            "restored original bytes must invalidate the actual-candidate proof"
        );
        actual_publication_proof = true;
    }
    let resource_marker = project.join(".auto-kicad-sync-resource.json");
    let resource_drift = resource_marker.exists();
    let drift_path = if resource_drift {
        let relative: String = serde_json::from_slice(&fs::read(resource_marker)?)?;
        let path = project.join(relative);
        if fs::canonicalize(&path)? != path || !path.starts_with(&project) {
            return Err("resource control escapes owned project".into());
        }
        path
    } else {
        board.clone()
    };
    let drift_before = fs::read(&drift_path)?;
    let mut changed = drift_before.clone();
    changed.extend_from_slice(b"\n ");
    fs::write(&drift_path, &changed)?;
    let rejected = preview.verify().is_err();
    fs::write(&drift_path, &drift_before)?;
    assert!(rejected, "private native context drift must reject");
    assert!(
        preview.verify().is_err(),
        "restored bytes must not revive invalidated observation"
    );
    let result = json!({"native_candidate_passed":true,"source_unchanged":fs::read(&board)?==original,
        "permanent_board_drift_rejection":!resource_drift,"permanent_manual_resource_drift_rejection":resource_drift,"undeclared_drc_rejected":undeclared_rejected,"exact_drc_expectation_passed":true,"pad_only_connected_net_change_rejected":copper_conflict,"components":source.components().len(),
        "has_changes":preview.candidate().has_changes(),"ineligible_units":source.ineligible_units().len(),"differences":preview.candidate().differences().len(),"digest":preview.digest(),
        "actual_pcb_publication_probe":actual_publication_proof,"durable_synchronization_publication_accepted":false,"public_mcp_accepted":false,"synchronization_product_complete":false});
    fs::write(
        output.join("result.json"),
        serde_json::to_vec_pretty(&result)?,
    )?;
    println!("{result}");
    Ok(())
}
