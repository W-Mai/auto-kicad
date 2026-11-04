use super::*;
use kicad_adapter::schematic_audit::synchronization::{
    SynchronizationExpectation,
    board::{SynchronizationAction, SynchronizationIntent},
};
use kicad_adapter::schematic_audit::{SchematicAudit, SchematicAuditPolicy};

#[test]
fn synchronization_completion_cannot_use_legacy_attempt_authority() {
    let value = serde_json::json!({"schema_version":1,"attempt_id":"attempt","project_dir":"/project",
        "plan_hash":"plan","approval_hash":"approval","domain":DOMAIN,"state":"in_flight",
        "evidence":null,"outcome":null,"backups":[],"post_attempt":null});
    let record: ExecutionRecord = serde_json::from_value(value).unwrap();
    assert!(!valid_execution_schema(&record));
    let attempt = ExecutionAttempt {
        path: PathBuf::from("/unused"),
        record,
    };
    assert!(
        attempt
            .finish("committed", Value::Null)
            .unwrap_err()
            .to_string()
            .contains("sealed native proof")
    );
}

#[test]
#[ignore = "requires an explicitly owned sync fixture, initialized configuration and KiCad 10.0.7"]
fn native_synchronization_publication_and_restart_recovery() {
    let root = fs::canonicalize(
        std::env::var_os("AUTO_KICAD_SYNC_TRANSACTION_FIXTURE").expect("owned root"),
    )
    .unwrap();
    let project = root.join("project");
    let config = root.join("config");
    for path in [&project, &config] {
        assert_eq!(
            fs::read_to_string(path.join(".auto-kicad-audit-owned")).unwrap(),
            "schematic-audit-acceptance\n"
        );
    }
    let control: Value =
        serde_json::from_slice(&fs::read(root.join("control.json")).unwrap()).unwrap();
    let policy = SchematicAuditPolicy::new(
        &project,
        &project.join("sync.kicad_sch"),
        &config,
        Path::new("/Applications/KiCad/KiCad.app/Contents/SharedSupport/symbols"),
        Path::new("/Applications/KiCad/KiCad.app/Contents/SharedSupport/footprints"),
        &[],
    )
    .unwrap();
    let store = ExecutionStore::open(&root.join("state")).unwrap();
    if control["mode"] == "publish" {
        let writer = store.acquire(&project).unwrap();
        let audit = SchematicAudit::prepare(&policy).unwrap();
        let source = audit.synchronization_source().unwrap();
        let observed = source
            .observe_board(&project.join("sync.kicad_pcb"))
            .unwrap();
        let intent = SynchronizationIntent {
            project_netclasses: vec![],
            schema_version: 1,
            actions: source
                .components()
                .iter()
                .filter(|c| c.eligible_for_board())
                .map(|c| SynchronizationAction::Update {
                    component_id: c.id().into(),
                    fields: vec![],
                    update_nets: true,
                })
                .collect(),
            copper_nets: serde_json::from_slice(
                &fs::read(project.join(".auto-kicad-sync-copper.json")).unwrap(),
            )
            .unwrap(),
        };
        let preview = observed.preview(&intent).unwrap();
        let expectation = SynchronizationExpectation {
            differences: preview.candidate().differences().to_vec(),
            drc: kicad_checks::CompleteDrcExpectation {
                introduced: preview.drc_delta().introduced.clone(),
                resolved: preview.drc_delta().resolved.clone(),
            },
        };
        fs::write(
            root.join("expected-candidate.kicad_pcb"),
            preview.candidate().bytes(),
        )
        .unwrap();
        let matched = preview.match_expectation(expectation).unwrap();
        let result = writer.publish_sync_internal(
            "transaction-plan",
            "transaction-approval",
            matched,
            |phase| {
                if control["boundary"].as_str() == Some(phase) {
                    fs::write(
                        root.join("checkpoint.json"),
                        serde_json::to_vec(
                            &serde_json::json!({"phase":phase,"pid":std::process::id()}),
                        )?,
                    )?;
                    if control["fault"] == true {
                        return Err(invalid("controlled synchronization failure"));
                    }
                    loop {
                        std::thread::park_timeout(std::time::Duration::from_millis(100));
                    }
                }
                Ok(())
            },
        );
        if control["fault"] == true {
            let error = result.unwrap_err();
            assert!(error.unknown);
            fs::write(root.join("publication-failure.json"),serde_json::to_vec(&serde_json::json!({"unknown":error.unknown,"attempt_id":error.attempt_id,"reason":error.reason})).unwrap()).unwrap();
        } else {
            let receipt = result.unwrap();
            fs::write(
                root.join("publication.json"),
                serde_json::to_vec(&receipt).unwrap(),
            )
            .unwrap();
            assert_eq!(
                fs::read(project.join("sync.kicad_pcb")).unwrap(),
                fs::read(root.join("expected-candidate.kicad_pcb")).unwrap()
            );
            assert!(!project.join(PENDING).exists());
            writer.ensure_ready().unwrap();
        }
        return;
    }
    let records = store.history(&project).unwrap();
    let record = records
        .iter()
        .find(|r| r.domain == DOMAIN)
        .expect("sync journal");
    assert_eq!(record.schema_version, 4);
    assert!(record.sync_publication.is_some());
    assert!(store.check_project_ready(&project).is_err());
    assert!(store.plan_recovery(&project, &record.attempt_id).is_err());
    if control["mode"] == "reject_recovery" {
        let error = store
            .plan_synchronization_recovery(&policy, &record.attempt_id)
            .unwrap_err();
        fs::write(
            root.join("recovery-rejected.json"),
            serde_json::to_vec(&serde_json::json!({"rejected":true,"reason":error.to_string()}))
                .unwrap(),
        )
        .unwrap();
        assert!(store.check_project_ready(&project).is_err());
        return;
    }
    let plan = store
        .plan_synchronization_recovery(&policy, &record.attempt_id)
        .unwrap();
    assert_eq!(plan.schema_version, 5);
    let restarted = ExecutionStore::open(&root.join("state")).unwrap();
    assert!(
        restarted
            .preview_synchronization_recovery(&policy, &plan.recovery_id, &plan.plan_hash)
            .is_err()
    );
    let approval = store
        .preview_synchronization_recovery(&policy, &plan.recovery_id, &plan.plan_hash)
        .unwrap();
    assert!(
        store
            .commit_synchronization_recovery(
                &policy,
                &plan.recovery_id,
                &plan.plan_hash,
                "wrong-approval"
            )
            .is_err()
    );
    let result = store
        .commit_recovery_with_policy(
            &project,
            &plan.recovery_id,
            &plan.plan_hash,
            &approval,
            Some(&policy),
            |index| {
                if control["recovery_boundary"].as_str() == Some(&index.to_string()) {
                    fs::write(
                        root.join("recovery-checkpoint.json"),
                        serde_json::to_vec(
                            &serde_json::json!({"phase":index,"pid":std::process::id()}),
                        )?,
                    )?;
                    loop {
                        std::thread::park_timeout(std::time::Duration::from_millis(100));
                    }
                }
                Ok(())
            },
        )
        .unwrap();
    assert_eq!(result.state, "restored");
    assert_eq!(
        result.outcome.as_ref().unwrap()["actual_native_baseline_verified"],
        true
    );
    assert!(
        store
            .commit_synchronization_recovery(&policy, &plan.recovery_id, &plan.plan_hash, &approval)
            .is_err()
    );
    fs::write(
        root.join("recovery.json"),
        serde_json::to_vec(&result).unwrap(),
    )
    .unwrap();
    assert_eq!(
        fs::read(project.join("sync.kicad_pcb")).unwrap(),
        fs::read(root.join("original.kicad_pcb")).unwrap()
    );
    assert!(!project.join(PENDING).exists());
    store.check_project_ready(&project).unwrap();
    store.acquire(&project).unwrap().ensure_ready().unwrap();
}
