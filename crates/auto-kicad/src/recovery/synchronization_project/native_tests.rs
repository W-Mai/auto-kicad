//! Actual native publication/restart entry point for explicitly owned acceptance copies.
use super::*;
use kicad_adapter::schematic_audit::{
    SchematicAudit,
    synchronization::{
        SynchronizationProjectBoardExpectation,
        board::{SynchronizationAction, SynchronizationCopperNet, SynchronizationIntent},
        project::ProjectNetclassChange,
    },
};
use std::collections::BTreeSet;

#[test]
#[ignore = "requires an explicitly owned project-sync fixture, configuration and KiCad 10.0.7"]
fn native_project_publication_and_restart_recovery() {
    let root = fs::canonicalize(
        std::env::var_os("AUTO_KICAD_SYNC_PROJECT_TRANSACTION_FIXTURE").expect("owned root"),
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
        let baseline = serde_json::to_value(observed.recovery_baseline().unwrap()).unwrap();
        let project_only = control["project_only"] == true;
        let mut intent = SynchronizationIntent {
            project_netclasses: vec![],
            schema_version: 1,
            actions: source
                .components()
                .iter()
                .filter(|component| component.eligible_for_board())
                .map(|component| {
                    let target = observed
                        .board()
                        .footprints()
                        .values()
                        .find(|fp| {
                            fp.path.as_deref().is_some_and(|path| {
                                component
                                    .aliases()
                                    .iter()
                                    .any(|alias| alias == path.trim_end_matches('/'))
                            })
                        })
                        .expect("complete owned source link");
                    if component.footprint() != Some(target.library.as_str()) {
                        SynchronizationAction::Replace {
                            component_id: component.id().into(),
                            footprint_uuid: target.uuid.clone(),
                            fields: vec![],
                        }
                    } else {
                        SynchronizationAction::Update {
                            component_id: component.id().into(),
                            fields: vec![],
                            update_nets: true,
                        }
                    }
                })
                .collect(),
            copper_nets: vec![],
        };
        for item in baseline["native"]["copper"].as_array().unwrap() {
            if let Some(net) = item["net"].as_str().unwrap().strip_prefix("OLD_") {
                intent.copper_nets.push(SynchronizationCopperNet {
                    uuid: item["uuid"].as_str().unwrap().into(),
                    net: net.into(),
                });
            }
        }
        if project_only {
            intent.actions = observed
                .board()
                .footprints()
                .values()
                .map(|fp| SynchronizationAction::Keep {
                    footprint_uuid: fp.uuid.clone(),
                })
                .collect();
            intent.copper_nets.clear();
        }
        let targets = baseline["native"]["netclasses"]
            .as_object()
            .unwrap()
            .iter()
            .filter(|(_, class)| {
                class["constituents"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|member| member["name"] == "Power")
            })
            .filter_map(|(name, _)| name.strip_prefix("OLD_"))
            .filter(|name| audit.netlist().nets.iter().any(|net| net.name == *name))
            .map(str::to_owned)
            .collect::<BTreeSet<_>>();
        assert!(project_only || !targets.is_empty());
        let actions = if project_only {
            vec![ProjectNetclassChange::Define {
                name: "DurableUnused".into(),
                priority: Some(5),
                dimensions: vec![],
            }]
        } else {
            targets
                .iter()
                .map(|net| ProjectNetclassChange::Assign {
                    net: net.clone(),
                    classes: vec!["Power".into()],
                })
                .collect()
        };
        let preview = observed
            .preview_board_and_project(&intent, &actions)
            .unwrap();
        let expected = SynchronizationProjectBoardExpectation {
            differences: preview.board_candidate().differences().to_vec(),
            project_differences: preview.project_candidate().differences().to_vec(),
            netclasses: preview.netclass_delta().clone(),
            drc: kicad_checks::CompleteDrcExpectation {
                introduced: preview.drc_delta().introduced.clone(),
                resolved: preview.drc_delta().resolved.clone(),
            },
        };
        fs::write(
            root.join("expected.kicad_pcb"),
            preview.board_candidate().bytes(),
        )
        .unwrap();
        fs::write(
            root.join("expected.kicad_pro"),
            preview.project_candidate().bytes(),
        )
        .unwrap();
        fs::write(
            root.join("expected-preview.json"),
            serde_json::to_vec(preview.evidence()).unwrap(),
        )
        .unwrap();
        let result = writer.publish_sync_project_internal(
            "project-transaction-plan",
            "project-transaction-approval",
            preview.match_expectation(expected).unwrap(),
            |phase| {
                if control["boundary"].as_str() == Some(phase) {
                    fs::write(
                        root.join("checkpoint.json"),
                        serde_json::to_vec(
                            &serde_json::json!({"phase":phase,"pid":std::process::id()}),
                        )?,
                    )?;
                    if control["fault"] == true {
                        return Err(invalid("controlled project synchronization failure"));
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
            fs::write(
                root.join("publication-failure.json"),
                serde_json::to_vec(
                    &serde_json::json!({"unknown":error.unknown,"reason":error.reason}),
                )
                .unwrap(),
            )
            .unwrap();
        } else {
            let receipt = result.unwrap();
            fs::write(
                root.join("publication.json"),
                serde_json::to_vec(&receipt).unwrap(),
            )
            .unwrap();
            assert!(writer.ensure_ready().is_ok());
        }
    } else {
        let executions = store.history(&project).unwrap();
        assert_eq!(executions.len(), 1);
        let execution = &executions[0];
        assert_eq!(execution.schema_version, 5);
        if control["mode"] == "expired" {
            let plan: RecoveryRecord =
                serde_json::from_slice(&fs::read(root.join("approved-recovery.json")).unwrap())
                    .unwrap();
            let error = store
                .commit_synchronization_recovery(
                    &policy,
                    &plan.recovery_id,
                    &plan.plan_hash,
                    plan.approval_hash.as_ref().unwrap(),
                )
                .unwrap_err();
            assert!(!error.unknown);
            fs::write(
                root.join("expired-approval.json"),
                serde_json::to_vec(
                    &serde_json::json!({"rejected":true,"reason":error.to_string()}),
                )
                .unwrap(),
            )
            .unwrap();
        } else {
            let planned = store.plan_synchronization_recovery(&policy, &execution.attempt_id);
            if control["expect_rejection"] == true {
                let error = planned.unwrap_err();
                fs::write(
                    root.join("recovery-rejection.json"),
                    serde_json::to_vec(
                        &serde_json::json!({"rejected":true,"reason":error.to_string()}),
                    )
                    .unwrap(),
                )
                .unwrap();
                assert!(store.acquire(&project).unwrap().ensure_ready().is_err());
            } else {
                let plan = planned.unwrap();
                assert_eq!(plan.schema_version, 6);
                let approval = store
                    .preview_synchronization_recovery(&policy, &plan.recovery_id, &plan.plan_hash)
                    .unwrap();
                if control["mode"] == "plan" {
                    let mut approved = plan;
                    approved.approval_hash = Some(approval);
                    fs::write(
                        root.join("approved-recovery.json"),
                        serde_json::to_vec(&approved).unwrap(),
                    )
                    .unwrap();
                } else {
                    let receipt = store
                        .commit_synchronization_recovery(
                            &policy,
                            &plan.recovery_id,
                            &plan.plan_hash,
                            &approval,
                        )
                        .unwrap();
                    assert_eq!(receipt.state, "restored");
                    assert!(
                        receipt.outcome.as_ref().unwrap()["actual_native_baseline_verified"]
                            == true
                    );
                    fs::write(
                        root.join("restoration.json"),
                        serde_json::to_vec(&receipt).unwrap(),
                    )
                    .unwrap();
                    assert!(store.acquire(&project).unwrap().ensure_ready().is_ok());
                }
            }
        }
    }
}
