use super::*;

async fn rpc(
    client: &rmcp::service::RunningService<rmcp::RoleClient, TestClient>,
    name: &'static str,
    arguments: Value,
) -> Value {
    let response = client
        .call_tool(
            CallToolRequestParams::new(name).with_arguments(arguments.as_object().unwrap().clone()),
        )
        .await
        .unwrap();
    assert_ne!(response.is_error, Some(true), "{response:?}");
    response.structured_content.unwrap()
}

#[tokio::test]
async fn recovery_mcp_contract_and_session_expiry() {
    let fixture = TempBoard::new();
    let project = std::fs::canonicalize(&fixture.root).unwrap();
    let board = project.join("board.kicad_pcb");
    let state = project.join("state");
    let server = Server::with_discovery(Arc::new(FakeDiscovery {
        write_capability: true,
    }));
    let client = connected(server.clone()).await;
    assert!(
        !client
            .list_all_tools()
            .await
            .unwrap()
            .iter()
            .any(|t| t.name == "kicad_plan_recovery")
    );
    let caps = rpc(&client, "kicad_capabilities", json!({})).await;
    assert_eq!(
        caps["data"]["capability_matrix"]["capabilities"]
            .as_array()
            .unwrap()
            .iter()
            .find(|c| c["id"] == "project.guarded_recovery")
            .unwrap()["status"],
        "unavailable"
    );
    client.cancel().await.unwrap();
    let server = server.with_state_dir(&state).unwrap();
    let store = server.execution_store.as_ref().unwrap();
    let writer = store.acquire(&project).unwrap();
    let attempt = writer
        .begin(
            "pcb",
            "test-plan",
            "test-approval",
            Value::Null,
            std::slice::from_ref(&board),
        )
        .unwrap();
    let id = attempt.attempt_id().to_owned();
    let before = std::fs::read(&board).unwrap();
    std::fs::write(&board, b"uncertain saved change").unwrap();
    attempt.finish("unknown", Value::Null).unwrap();
    drop(writer);
    let client = connected(server.clone()).await;
    assert!(
        client
            .list_all_tools()
            .await
            .unwrap()
            .iter()
            .any(|t| t.name == "kicad_plan_recovery")
    );
    let plan = rpc(
        &client,
        "kicad_plan_recovery",
        json!({"project_dir": project, "attempt_id": id, "action": "restore_backups"}),
    )
    .await;
    assert_eq!(plan["ok"], true, "{plan}");
    let args = json!({"project_dir": project, "recovery_id": plan["data"]["recovery_id"], "recovery_plan_hash": plan["data"]["recovery_plan_hash"]});
    let preview = rpc(&client, "kicad_preview_recovery", args.clone()).await;
    assert_eq!(preview["ok"], true, "{preview}");
    let mut commit = args.clone();
    commit["approval_hash"] = json!("wrong");
    assert_eq!(
        rpc(&client, "kicad_commit_recovery", commit.clone()).await["data"]["state"],
        "rejected"
    );
    commit["approval_hash"] = preview["data"]["approval_hash"].clone();
    let writer = store.acquire(&project).unwrap();
    assert_eq!(
        rpc(&client, "kicad_commit_recovery", commit.clone()).await["data"]["state"],
        "rejected"
    );
    drop(writer);
    let restarted = connected(
        Server::with_discovery(Arc::new(FakeDiscovery {
            write_capability: true,
        }))
        .with_state_dir(&state)
        .unwrap(),
    )
    .await;
    assert_eq!(
        rpc(&restarted, "kicad_commit_recovery", commit.clone()).await["data"]["state"],
        "rejected"
    );
    restarted.cancel().await.unwrap();
    let restored = rpc(&client, "kicad_commit_recovery", commit.clone()).await;
    assert_eq!(restored["ok"], true, "{restored}");
    assert_eq!(restored["data"]["state"], "restored");
    assert_eq!(std::fs::read(&board).unwrap(), before);
    assert_eq!(
        rpc(&client, "kicad_commit_recovery", commit).await["data"]["state"],
        "rejected"
    );
    let inspected = rpc(
        &client,
        "kicad_inspect_project",
        json!({"endpoint": "", "project_dir": project}),
    )
    .await;
    assert_eq!(inspected["ok"], true);
    assert_eq!(inspected["data"]["observation"], "durable_history_only");
    assert!(inspected["project_revision"].is_null());
    assert_eq!(
        inspected["data"]["execution_history"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    assert_eq!(
        inspected["data"]["recovery_history"][0]["state"],
        "restored"
    );
    assert_eq!(store.history(&project).unwrap().len(), 1);
    client.cancel().await.unwrap();
}

struct UncertainSavedReceipt(std::sync::atomic::AtomicBool);
impl BoardGraphicsExecutionService for UncertainSavedReceipt {
    fn execute(
        &self,
        endpoint: &str,
        filename: &str,
        path: &std::path::Path,
        project: &std::path::Path,
        token: &str,
        revision: &RevisionDigest,
        operations: &[Operation],
    ) -> BoardGraphicsCommitOutcome {
        let outcome = IpcBoardGraphicsExecutionService::default().execute(
            endpoint, filename, path, project, token, revision, operations,
        );
        if outcome == BoardGraphicsCommitOutcome::CommittedAndSaved
            && self.0.swap(false, AtomicOrdering::SeqCst)
        {
            BoardGraphicsCommitOutcome::Unknown {
                reason: "test-only lost receipt after native commit and save".into(),
            }
        } else {
            outcome
        }
    }
}

async fn change(
    client: &rmcp::service::RunningService<rmcp::RoleClient, TestClient>,
    endpoint: &str,
    request: &str,
    y: i64,
) -> Value {
    let mut addition = graphic(request);
    addition.start.y = y;
    addition.end.y = y;
    let plan = rpc(client, "kicad_plan_change", json!({"endpoint": endpoint, "source_run_id": "recovery-live", "request_id": request, "segments": [addition]})).await;
    assert_eq!(plan["ok"], true, "{plan}");
    let preview = rpc(
        client,
        "kicad_preview_change",
        json!({"plan_id": plan["data"]["plan_id"], "plan_hash": plan["data"]["plan_hash"]}),
    )
    .await;
    assert_eq!(preview["ok"], true, "{preview}");
    assert_eq!(
        preview["data"]["shadow_validation"]["normalized_snapshot_matches"],
        true
    );
    rpc(client, "kicad_commit_change", json!({"plan_id": plan["data"]["plan_id"], "plan_hash": plan["data"]["plan_hash"], "approval_hash": preview["data"]["approval_hash"], "validation_evidence_hash": preview["data"]["validation_evidence_hash"]})).await
}

#[tokio::test]
#[ignore = "requires KiCad PCB Editor and CLI; closes and reopens only its disposable copy"]
async fn live_recovery_restores_native_board_and_allows_next_mcp_commit() {
    let fixture = TempBoard::new();
    let input = fixture.root.join("board.kicad_pcb");
    let external = std::env::var_os("AUTO_KICAD_RECOVERY_SOURCE").map(PathBuf::from);
    let original = if let Some(path) = &external {
        let bytes = std::fs::read(path).unwrap();
        let project = std::fs::read(path.with_extension("kicad_pro")).unwrap();
        std::fs::write(&input, &bytes).unwrap();
        std::fs::write(input.with_extension("kicad_pro"), &project).unwrap();
        Some((bytes, project))
    } else {
        std::fs::write(&input, routing_fixture_board()).unwrap();
        std::fs::write(input.with_extension("kicad_pro"), "{}").unwrap();
        None
    };
    let mut editor = OwnedPcbnewLauncher::new(&fixture.root.join("source")).unwrap();
    let copy = editor.create_shadow(&input).unwrap();
    let identity = editor.launch(&copy).unwrap();
    // Normalize the saved fixture through KiCad before capturing recovery evidence.
    editor
        .connect_owned()
        .unwrap()
        .save_board(&identity.board_filename)
        .unwrap();
    editor.reload_owned().unwrap();
    let before = observe_owned_shadow(&mut editor).unwrap();
    let before_bytes = std::fs::read(copy.board_path()).unwrap();
    let project_bytes = std::fs::read(copy.project_path()).unwrap();
    let state = fixture.root.join("state");
    let server = Server::with_discovery_and_execution_and_shadow(
        Arc::new(IpcDiscoveryService),
        Arc::new(UncertainSavedReceipt(std::sync::atomic::AtomicBool::new(
            true,
        ))),
        Arc::new(IpcShadowValidationService::new(
            fixture.root.join("preview"),
        )),
    )
    .with_state_dir(&state)
    .unwrap();
    let client = connected(server.clone()).await;
    let baseline = rpc(
        &client,
        "kicad_validate_project",
        json!({"endpoint": identity.endpoint}),
    )
    .await;
    assert_eq!(baseline["ok"], true, "{baseline}");
    let unknown = change(&client, &identity.endpoint, "uncertain-graphic", 2_000_000).await;
    assert_eq!(unknown["ok"], false, "{unknown}");
    assert_eq!(unknown["data"]["state"], "unknown");
    assert_ne!(std::fs::read(copy.board_path()).unwrap(), before_bytes);
    let changed = observe_owned_shadow(&mut editor).unwrap();
    assert_eq!(
        changed.snapshot.board.graphics.len(),
        before.snapshot.board.graphics.len() + 1
    );
    let history = rpc(
        &client,
        "kicad_inspect_project",
        json!({"endpoint": identity.endpoint, "project_dir": copy.root()}),
    )
    .await;
    assert_eq!(history["ok"], true, "{history}");
    let execution = &history["data"]["execution_history"][0];
    let attempt_id = execution["attempt_id"].as_str().unwrap();
    let plan_args =
        json!({"project_dir": copy.root(), "attempt_id": attempt_id, "action": "restore_backups"});
    let locked = rpc(&client, "kicad_plan_recovery", plan_args.clone()).await;
    assert_eq!(
        locked["ok"], false,
        "open editor must reject restoration: {locked}"
    );
    editor.close_owned(identity.pid).unwrap();
    // The launcher kills and waits for its child. Remove only that child's disposable lock.
    for entry in std::fs::read_dir(copy.root()).unwrap() {
        let entry = entry.unwrap();
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if name.starts_with('~') && name.ends_with(".lck") {
            eprintln!("Removing stale lock from the exited owned fixture editor: {name}");
            std::fs::remove_file(entry.path()).unwrap();
        }
    }
    let (restored, stdio_evidence) = if let Some(binary) =
        std::env::var_os("AUTO_KICAD_RECOVERY_SERVER")
    {
        let evidence_path = fixture.root.join("stdio-recovery.json");
        let status = std::process::Command::new("python3")
            .env("PYTHONDONTWRITEBYTECODE", "1")
            .arg(
                std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                    .join("../../scripts/check-recovery.py"),
            )
            .arg(binary)
            .arg(&state)
            .arg(copy.root())
            .arg(attempt_id)
            .arg(&evidence_path)
            .status()
            .unwrap();
        assert!(status.success(), "stdio recovery failed");
        let evidence: Value =
            serde_json::from_slice(&std::fs::read(evidence_path).unwrap()).unwrap();
        (json!({"ok": true, "data": evidence["restored"]}), evidence)
    } else {
        let plan = rpc(&client, "kicad_plan_recovery", plan_args).await;
        assert_eq!(plan["ok"], true, "{plan}");
        let mut args = json!({"project_dir": copy.root(), "recovery_id": plan["data"]["recovery_id"], "recovery_plan_hash": plan["data"]["recovery_plan_hash"]});
        let preview = rpc(&client, "kicad_preview_recovery", args.clone()).await;
        assert_eq!(preview["ok"], true, "{preview}");
        args["approval_hash"] = preview["data"]["approval_hash"].clone();
        let restored = rpc(&client, "kicad_commit_recovery", args.clone()).await;
        assert_eq!(restored["ok"], true, "{restored}");
        assert_eq!(restored["data"]["state"], "restored");
        assert_eq!(std::fs::read(copy.board_path()).unwrap(), before_bytes);
        assert_eq!(std::fs::read(copy.project_path()).unwrap(), project_bytes);
        assert_eq!(
            rpc(&client, "kicad_commit_recovery", args).await["data"]["state"],
            "rejected"
        );
        (restored, Value::Null)
    };
    assert_eq!(std::fs::read(copy.board_path()).unwrap(), before_bytes);
    assert_eq!(std::fs::read(copy.project_path()).unwrap(), project_bytes);
    let reopened_identity = editor.reopen().unwrap();
    let reopened = observe_owned_shadow(&mut editor).unwrap();
    assert_eq!(before.snapshot.board, reopened.snapshot.board);
    assert_eq!(
        before.snapshot.native_footprints,
        reopened.snapshot.native_footprints
    );
    assert_eq!(
        before.snapshot.native_routing,
        reopened.snapshot.native_routing
    );
    assert_eq!(
        before.snapshot.native_stackup,
        reopened.snapshot.native_stackup
    );
    let restored_drc = rpc(
        &client,
        "kicad_validate_project",
        json!({"endpoint": reopened_identity.endpoint}),
    )
    .await;
    assert_eq!(restored_drc["ok"], true, "{restored_drc}");
    assert_eq!(restored_drc["data"]["report"], baseline["data"]["report"]);
    let next = change(
        &client,
        &reopened_identity.endpoint,
        "normal-after-recovery",
        3_000_000,
    )
    .await;
    assert_eq!(next["ok"], true, "{next}");
    assert_eq!(next["data"]["state"], "committed");
    editor.reload_owned().unwrap();
    let after = observe_owned_shadow(&mut editor).unwrap();
    assert_eq!(
        after.snapshot.board.graphics.len(),
        before.snapshot.board.graphics.len() + 1
    );
    let final_drc = rpc(&client, "kicad_validate_project", json!({"endpoint": reopened_identity.endpoint, "baseline_id": restored_drc["data"]["baseline_id"]})).await;
    assert_eq!(final_drc["ok"], true, "{final_drc}");
    assert_eq!(final_drc["data"]["delta"]["introduced"], json!([]));
    let final_history = rpc(
        &client,
        "kicad_inspect_project",
        json!({"endpoint": reopened_identity.endpoint, "project_dir": copy.root()}),
    )
    .await;
    assert_eq!(final_history["ok"], true, "{final_history}");
    assert_eq!(
        final_history["data"]["execution_history"]
            .as_array()
            .unwrap()
            .len(),
        2
    );
    assert_eq!(
        final_history["data"]["recovery_history"][0]["state"],
        "restored"
    );
    let unchanged_execution = final_history["data"]["execution_history"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["attempt_id"] == attempt_id)
        .unwrap();
    assert_eq!(unchanged_execution, execution);
    if let (Some(path), Some((board, project))) = (&external, &original) {
        assert_eq!(&std::fs::read(path).unwrap(), board);
        assert_eq!(
            &std::fs::read(path.with_extension("kicad_pro")).unwrap(),
            project
        );
    }
    client.cancel().await.unwrap();
    editor.close_owned(reopened_identity.pid).unwrap();
    editor.cleanup().unwrap();
    assert!(!copy.root().exists());
    let evidence = json!({"passed": true, "fixture": external, "kicad_version": before.snapshot.kicad_version,
            "fault": "test-only unknown receipt after real native save", "source_pid": identity.pid, "reopened_pid": reopened_identity.pid,
            "stdio_recovery": stdio_evidence, "open_editor_rejected": locked, "unknown": unknown, "restored": restored, "subsequent_commit": next,
            "restored_objects_equal": true, "restored_bytes_equal": true, "original_project_unchanged": true,
            "baseline_drc": baseline["data"]["report"], "restored_drc": restored_drc["data"]["report"], "final_delta": final_drc["data"]["delta"],
            "execution_history": final_history["data"]["execution_history"], "recovery_history": final_history["data"]["recovery_history"],
            "owned_editors_closed": true, "fixture_cleaned": true});
    if let Some(path) = std::env::var_os("AUTO_KICAD_RECOVERY_EVIDENCE") {
        std::fs::write(path, serde_json::to_vec_pretty(&evidence).unwrap()).unwrap();
    }
    eprintln!(
        "Native MCP recovery, exact byte restoration, reopen, DRC, subsequent commit and owned-editor cleanup passed ({})",
        before.snapshot.kicad_version
    );
}
