//! Verify typed route compilation, atomic native creation and saved connectivity.
use auto_kicad::{DiscoveryService, IpcDiscoveryService};
use kicad_adapter::{
    BoardGraphicsExecutor, created_routing_matches_reloaded,
    lifecycle::{DisposablePcbLifecycle, OwnedPcbnewLauncher},
    prepare_bound_native_batch, read_verified_native_routing,
};
use kicad_checks::{CheckResult, DrcPolicy, compare_drc, run_pcb_drc};
use kicad_engine::{
    ChangePlan, Engine, Operation, OperationKind, Precondition, RiskClass, compile_copper_route,
};
use kicad_ipc::{
    ClientConfig, IpcError, KiCadClient, KiCadTransport, NngTransport,
    envelope::{ApiRequest, ApiResponse},
};
use kicad_model::{
    CopperLayer, CopperRouteIntent, ObservedNetIdentity, RevisionDigest, RouteLeg, RoutePoint,
    RouteTrackEnd, RouteVia, RoutingAnchor, StableId, board_net_id,
};
use serde_json::json;
use std::{
    env, fs,
    path::{Path, PathBuf},
    time::Duration,
};

struct RecordingTransport {
    native: NngTransport,
    calls: std::sync::Arc<std::sync::Mutex<Vec<String>>>,
}
impl KiCadTransport for RecordingTransport {
    fn request(&mut self, request: ApiRequest) -> Result<ApiResponse, IpcError> {
        self.calls.lock().unwrap().push(
            request
                .message
                .as_ref()
                .map(|m| m.type_url.clone())
                .unwrap_or_default(),
        );
        self.native.request(request)
    }
}

struct FailureTransport {
    native: NngTransport,
    creates: usize,
    calls: std::sync::Arc<std::sync::Mutex<Vec<String>>>,
}
impl KiCadTransport for FailureTransport {
    fn request(&mut self, request: ApiRequest) -> Result<ApiResponse, IpcError> {
        self.calls.lock().unwrap().push(
            request
                .message
                .as_ref()
                .map(|m| m.type_url.clone())
                .unwrap_or_default(),
        );
        if request
            .message
            .as_ref()
            .is_some_and(|m| m.type_url == kicad_ipc::CREATE_ITEMS_TYPE_URL)
        {
            self.creates += 1;
            if self.creates == 2 {
                return Err(IpcError::InvalidObservedRouting(
                    "injected second native create failure",
                ));
            }
        }
        self.native.request(request)
    }
}
fn drc(board: &Path) -> Result<kicad_checks::DrcReport, Box<dyn std::error::Error>> {
    match run_pcb_drc(
        board,
        &DrcPolicy {
            allowed_root: board.parent().unwrap().to_path_buf(),
            timeout: Duration::from_secs(60),
        },
    ) {
        CheckResult::Completed(report) => Ok(report),
        CheckResult::Failed(error) => Err(format!("DRC failed: {error:?}").into()),
    }
}
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let output = PathBuf::from(
        env::args()
            .nth(1)
            .ok_or("usage: check-native-route NEW_OUTPUT_DIR")?,
    );
    fs::create_dir(&output)?;
    let output = fs::canonicalize(output)?;
    let input = output.join("fixture");
    fs::create_dir(&input)?;
    let board = input.join("board.kicad_pcb");
    let source = include_str!("../../../scripts/fixtures/layered-routing-rules.kicad_pcb");
    let index = source.rfind(')').unwrap();
    let vias = r#"
        (footprint "RouteFixture:BackPad" (layer "B.Cu") (at 50 20)
            (uuid "60000000-0000-4000-8000-000000000001")
            (property "Reference" "TP1" (at 0 2) (layer "B.SilkS") (effects (font (size 1 1) (thickness 0.15)) (justify mirror)))
            (property "Value" "GND" (at 0 -2) (layer "B.Fab") (effects (font (size 1 1) (thickness 0.15)) (justify mirror)))
            (attr smd)
            (pad "1" smd circle (at 0 0) (size 1 1) (layers "B.Cu" "B.Mask") (net 1 "GND") (uuid "60000000-0000-4000-8000-000000000002")))
        (via (at 20 20) (size 0.6) (drill 0.3) (layers "F.Cu" "B.Cu") (net 1) (uuid "50000000-0000-4000-8000-000000000001"))
        (via (at 40 20) (size 0.6) (drill 0.3) (layers "F.Cu" "B.Cu") (net 1) (uuid "50000000-0000-4000-8000-000000000002"))
    "#;
    fs::write(&board, format!("{}{vias})", &source[..index]))?;
    fs::write(
        board.with_extension("kicad_pro"),
        include_str!("../../../scripts/fixtures/layered-routing-rules.kicad_pro"),
    )?;
    let original = [
        fs::read(&board)?,
        fs::read(board.with_extension("kicad_pro"))?,
    ];
    let mut editor = OwnedPcbnewLauncher::new(&output.join("work"))?;
    let copy = editor.create_shadow(&board)?;
    let identity = editor.launch(&copy)?;
    let result: Result<_, Box<dyn std::error::Error>> = (|| {
        let mut ipc = editor.connect_owned()?;
        ipc.get_version()?;
        ipc.save_board(&identity.board_filename)?;
        editor.reload_owned()?;
        let source = IpcDiscoveryService.live_board_observation(&identity.endpoint, None)?;
        let board_id = source
            .snapshot
            .board
            .document_id
            .clone()
            .ok_or("missing board identity")?;
        let revision = source.snapshot.revision()?;
        let baseline = ipc.capture_existing_board_items(&identity.board_filename)?;
        let baseline_footprints =
            ipc.capture_footprint_instance_templates(&identity.board_filename)?;
        let protected = [fs::read(copy.board_path())?, fs::read(copy.project_path())?];
        let baseline_drc = drc(copy.board_path())?;
        let id = |value: &str| StableId::new(value).unwrap();
        let point = |x: i64, y: i64| RoutePoint {
            x_nm: x * 1_000_000,
            y_nm: y * 1_000_000,
        };
        let anchor = |suffix| RoutingAnchor::Via {
            source_uuid: id(&format!("50000000-0000-4000-8000-{suffix:012}")),
        };
        let net = ObservedNetIdentity {
            net_id: board_net_id(&board_id, "GND"),
            name: "GND".into(),
        };
        let path = CopperRouteIntent {
            route_id: id("layered-path"),
            observed_revision: revision.clone(),
            start: anchor(1),
            end: anchor(2),
            net: net.clone(),
            legs: vec![
                RouteLeg {
                    layer: CopperLayer::FRONT,
                    width_nm: 250_000,
                    points: vec![point(20, 20), point(20, 25), point(25, 25)],
                },
                RouteLeg {
                    layer: "In1.Cu".parse()?,
                    width_nm: 250_000,
                    points: vec![point(25, 25), point(35, 25)],
                },
                RouteLeg {
                    layer: CopperLayer::FRONT,
                    width_nm: 250_000,
                    points: vec![point(35, 25), point(40, 25), point(40, 20)],
                },
            ],
            vias: vec![
                RouteVia {
                    via_id: id("transition-a"),
                    position: point(25, 25),
                    pad_diameter_nm: 600_000,
                    drill_diameter_nm: 300_000,
                },
                RouteVia {
                    via_id: id("transition-b"),
                    position: point(35, 25),
                    pad_diameter_nm: 600_000,
                    drill_diameter_nm: 300_000,
                },
            ],
        };
        let join = CopperRouteIntent {
            route_id: id("join-existing-track"),
            observed_revision: revision.clone(),
            start: RoutingAnchor::TrackEnd {
                source_uuid: id("40000000-0000-4000-8000-000000000001"),
                end: RouteTrackEnd::End,
            },
            end: anchor(1),
            net: net.clone(),
            legs: vec![RouteLeg {
                layer: CopperLayer::FRONT,
                width_nm: 250_000,
                points: vec![point(15, 10), point(20, 10), point(20, 20)],
            }],
            vias: vec![],
        };
        let placement = source
            .snapshot
            .native_footprints
            .as_ref()
            .ok_or("missing footprints")?
            .placements
            .first()
            .ok_or("missing pad fixture")?;
        let pad = placement.pads.first().ok_or("missing pad")?;
        let back = CopperRouteIntent {
            route_id: id("join-back-pad"),
            observed_revision: revision.clone(),
            start: RoutingAnchor::Pad {
                pad_id: pad.pad_id.clone(),
                footprint_placement_id: placement.placement_id.clone(),
            },
            end: anchor(2),
            net,
            legs: vec![RouteLeg {
                layer: CopperLayer::BACK,
                width_nm: 250_000,
                points: vec![point(50, 20), point(45, 20), point(40, 20)],
            }],
            vias: vec![],
        };
        let routes = [path, join, back];
        let compiled = routes
            .iter()
            .map(|route| compile_copper_route(&source.snapshot, &board_id, route))
            .collect::<Result<Vec<_>, _>>()?;
        let unbound = RevisionDigest::new("0".repeat(64))?;
        let operations = routes
            .iter()
            .enumerate()
            .map(|(i, intent)| Operation {
                operation_id: id(&format!("route-{i}")),
                kind: OperationKind::AddCopperRoute {
                    board: board_id.clone(),
                    intent: intent.clone(),
                    rule_evidence: unbound.clone(),
                    native_evidence: unbound.clone(),
                },
                targets: vec![board_id.clone()],
                preconditions: vec![Precondition::Revision(revision.clone())],
                expected_effects: vec!["create explicit route".into()],
                risk_class: RiskClass::Medium,
                idempotency_key: format!("route-{i}"),
                source_run_id: "native-probe".into(),
            })
            .collect::<Vec<_>>();
        let plan = ChangePlan {
            plan_id: id("native-route-plan"),
            source_run_id: "native-probe".into(),
            base_revision: revision.clone(),
            instance_token: source.instance_token.as_str().into(),
            operations: operations.clone(),
        };
        let engine = Engine::new(source.snapshot.clone(), source.instance_token.as_str());
        let preview = engine.preview(&plan)?;
        let batch = prepare_bound_native_batch(&operations, &source.snapshot, &revision)
            .map_err(|e| format!("batch failed: {e:?}"))?;
        if batch.tracks.len() != 9 || batch.vias.len() != 2 {
            return Err("incorrect route expansion".into());
        }
        let executor = BoardGraphicsExecutor {
            max_segments: kicad_model::copper_route::MAX_BATCH_COPPER,
        };
        let failure_calls = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let config = ClientConfig::new(&identity.endpoint, "route-failure-probe")?;
        let mut fault = KiCadClient::new(
            FailureTransport {
                native: NngTransport::connect(config.clone())?,
                creates: 0,
                calls: failure_calls.clone(),
            },
            config,
        );
        fault.get_version()?;
        let failed_commit = fault.begin_commit()?;
        failure_calls.lock().unwrap().clear();
        let failed = executor.create_native_batch_in_transaction(
            &mut fault,
            &identity.board_filename,
            &batch.graphics,
            &batch.tracks,
            &batch.vias,
        );
        let failed_creation_calls = failure_calls.lock().unwrap().clone();
        if failed.is_ok()
            || failed_creation_calls != vec![kicad_ipc::CREATE_ITEMS_TYPE_URL; 2]
            || protected != [fs::read(copy.board_path())?, fs::read(copy.project_path())?]
        {
            return Err("failed creation changed transaction ownership or saved files".into());
        }
        fault.drop_commit(&failed_commit)?;
        if !baseline.matches_saved(&ipc.capture_existing_board_items(&identity.board_filename)?)?
            || protected != [fs::read(copy.board_path())?, fs::read(copy.project_path())?]
        {
            return Err("partial create rollback changed source".into());
        }
        let calls = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let config = ClientConfig::new(&identity.endpoint, "route-owned-transaction-probe")?;
        let mut creation = KiCadClient::new(
            RecordingTransport {
                native: NngTransport::connect(config.clone())?,
                calls: calls.clone(),
            },
            config,
        );
        creation.get_version()?;
        let commit = creation.begin_commit()?;
        calls.lock().unwrap().clear();
        let receipts = executor
            .create_native_batch_in_transaction(
                &mut creation,
                &identity.board_filename,
                &batch.graphics,
                &batch.tracks,
                &batch.vias,
            )
            .map_err(|e| format!("native route creation failed: {e}"))?;
        let creation_calls = calls.lock().unwrap().clone();
        if creation_calls.len() != batch.tracks.len() + batch.vias.len()
            || creation_calls
                .iter()
                .any(|call| call != kicad_ipc::CREATE_ITEMS_TYPE_URL)
            || protected != [fs::read(copy.board_path())?, fs::read(copy.project_path())?]
        {
            return Err("shared creation changed transaction ownership or saved files".into());
        }
        creation.commit_changes(&commit, "create explicit native copper routes")?;
        ipc.save_board(&identity.board_filename)?;
        kicad_adapter::verify_native_pcb_batch(&mut ipc, &identity.board_filename, &receipts)?;
        let saved = read_verified_native_routing(&mut ipc, &identity.board_filename, &receipts)?;
        editor.reload_owned()?;
        let reloaded = read_verified_native_routing(&mut ipc, &identity.board_filename, &receipts)?;
        let ids = receipts
            .tracks
            .iter()
            .map(|t| t.source_uuid.clone())
            .chain(receipts.vias.iter().map(|v| v.source_uuid.clone()))
            .collect::<Vec<_>>();
        let after = ipc.capture_existing_board_items(&identity.board_filename)?;
        if !created_routing_matches_reloaded(&saved, &reloaded)
            || !baseline.matches_after_additions(&after, &ids)?
            || fs::read(copy.project_path())? != protected[1]
            || ipc.instance_token() != Some(&source.instance_token)
        {
            fs::write(
                output.join("saved-routing-debug.txt"),
                format!("{saved:#?}"),
            )?;
            fs::write(
                output.join("reloaded-routing-debug.txt"),
                format!("{reloaded:#?}"),
            )?;
            return Err(format!(
                "saved route preservation failed: routing={} existing={} project={} token={}",
                saved == reloaded,
                baseline.matches_after_additions(&after, &ids)?,
                fs::read(copy.project_path())? == protected[1],
                ipc.instance_token() == Some(&source.instance_token)
            )
            .into());
        }
        let component = ipc.connected_copper_ids(
            &identity.board_filename,
            compiled[0].anchor_native_ids[0].as_str(),
            "GND",
        )?;
        if ids.iter().any(|id| !component.contains(id))
            || compiled
                .iter()
                .flat_map(|r| &r.anchor_native_ids)
                .any(|id| !component.contains(id.as_str()))
        {
            return Err("complete route connectivity failed".into());
        }
        let footprints = ipc.capture_footprint_instance_templates(&identity.board_filename)?;
        if footprints.len() != baseline_footprints.len() {
            return Err("footprint set changed".into());
        }
        for before in &baseline_footprints {
            let after = footprints
                .iter()
                .find(|fp| fp.metadata().source_uuid == before.metadata().source_uuid)
                .ok_or("original footprint missing")?;
            if !before.matches_saved_instance(after)? {
                return Err("original footprint payload changed".into());
            }
            fs::write(
                output.join(format!(
                    "footprint-{}-before.pb",
                    before.metadata().source_uuid
                )),
                before.persistent_evidence()?,
            )?;
            fs::write(
                output.join(format!(
                    "footprint-{}-after.pb",
                    before.metadata().source_uuid
                )),
                after.persistent_evidence()?,
            )?;
        }
        let final_drc = drc(copy.board_path())?;
        let delta = compare_drc(&baseline_drc, &final_drc);
        if !delta.introduced.is_empty()
            || final_drc.unconnected_items > baseline_drc.unconnected_items
        {
            return Err(format!("route DRC regression: {delta:?}").into());
        }
        let mapping=compiled.iter().flat_map(|r|r.tracks.iter()).zip(&receipts.tracks).map(|(planned,actual)|json!({"planned":planned.source_uuid,"native":actual.source_uuid,"layer":planned.layer})).chain(compiled.iter().flat_map(|r|r.vias.iter()).zip(&receipts.vias).map(|(planned,actual)|json!({"planned":planned.source_uuid,"native":actual.source_uuid}))).collect::<Vec<_>>();
        fs::create_dir(output.join("result"))?;
        fs::copy(copy.board_path(), output.join("result/board.kicad_pcb"))?;
        fs::copy(copy.project_path(), output.join("result/board.kicad_pro"))?;
        fs::write(output.join("plan.json"), serde_json::to_vec_pretty(&plan)?)?;
        fs::write(
            output.join("expected.json"),
            serde_json::to_vec_pretty(&preview.after)?,
        )?;
        fs::write(
            output.join("baseline-drc.json"),
            serde_json::to_vec_pretty(&baseline_drc)?,
        )?;
        fs::write(
            output.join("final-drc.json"),
            serde_json::to_vec_pretty(&final_drc)?,
        )?;
        fs::write(
            output.join("baseline-items.pb"),
            baseline.persistent_evidence()?,
        )?;
        fs::write(output.join("final-items.pb"), after.persistent_evidence()?)?;
        Ok(
            json!({"kicad_version":source.snapshot.kicad_version,"source_pid":identity.pid,"tracks":9,"vias":2,"routes":3,"anchor_kinds":["pad","track_end","via"],"layers":["F.Cu","In1.Cu","B.Cu"],"native_footprints_preserved":true,"caller_owned_transaction":true,"creation_calls":creation_calls,"failed_creation_calls":failed_creation_calls,"creation_preserved_saved_files":true,"partial_create_cancelled":true,"same_process_reload":true,"native_existing_items_preserved":true,"all_route_items_and_anchors_connected":true,"component":component,"mapping":mapping,"introduced_drc":delta.introduced.len(),"baseline_unconnected":baseline_drc.unconnected_items,"final_unconnected":final_drc.unconnected_items,"source_project_unchanged":true,"scope":"native path primitive; no MCP approval binding"}),
        )
    })();
    editor.close_owned(identity.pid)?;
    editor.cleanup()?;
    if original
        != [
            fs::read(&board)?,
            fs::read(board.with_extension("kicad_pro"))?,
        ]
    {
        return Err("original fixture changed".into());
    }
    fs::write(
        output.join("cleanup.json"),
        serde_json::to_vec_pretty(
            &json!({"owned_editor_closed":true,"shadow_cleaned":true,"original_unchanged":true}),
        )?,
    )?;
    let evidence = result?;
    fs::write(
        output.join("result.json"),
        serde_json::to_vec_pretty(&evidence)?,
    )?;
    fs::remove_dir(output.join("work"))?;
    println!("{}", serde_json::to_string(&evidence)?);
    Ok(())
}
