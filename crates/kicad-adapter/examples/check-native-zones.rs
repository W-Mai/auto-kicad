//! Exercise native solid-zone edits and complete unedited-field preservation in one owned editor.
use kicad_adapter::lifecycle::{DisposablePcbLifecycle, OwnedPcbnewLauncher};
use kicad_adapter::live_geometry::observe_live_geometry;
use kicad_adapter::outline::outline_revision;
use kicad_adapter::zone_edits::{PreparedSolidZoneEdits, SolidZoneEdits};
use kicad_ipc::{
    ClientConfig, CopperLayer, CreateNetIdentity, IpcError, KiCadClient, KiCadTransport,
    NngTransport, SolidZonePatch, SolidZonePayload, base_types, board_types,
    envelope::{ApiRequest, ApiResponse},
};
use protobuf::{Message, MessageField};
use serde_json::json;
use std::{env, fs, path::PathBuf, time::Duration};
type Error = Box<dyn std::error::Error>;
const TEMPLATE: &str = "80000000-0000-4000-8000-000000000001";
const LOCKED: &str = "80000000-0000-4000-8000-000000000002";
struct RecordingTransport {
    inner: NngTransport,
    output: PathBuf,
    count: usize,
}
impl KiCadTransport for RecordingTransport {
    fn request(&mut self, request: ApiRequest) -> Result<ApiResponse, IpcError> {
        let mutating = request.message.as_ref().is_some_and(|m| {
            m.type_url.ends_with("CreateItems") || m.type_url.ends_with("UpdateItems")
        });
        if mutating {
            let _ = fs::write(
                self.output.join(format!("request-{}.pb", self.count)),
                request.write_to_bytes().unwrap(),
            );
        }
        let result = self.inner.request(request);
        if mutating {
            if let Ok(reply) = &result {
                let _ = fs::write(
                    self.output.join(format!("response-{}.pb", self.count)),
                    reply.write_to_bytes().unwrap(),
                );
            }
            self.count += 1;
        }
        result
    }
}
fn ring(points: &[(i64, i64)]) -> base_types::PolyLine {
    base_types::PolyLine {
        closed: true,
        nodes: points
            .iter()
            .map(|&(x_nm, y_nm)| base_types::PolyLineNode {
                geometry: Some(base_types::poly_line_node::Geometry::Point(
                    base_types::Vector2 {
                        x_nm: x_nm * 1_000_000,
                        y_nm: y_nm * 1_000_000,
                        ..Default::default()
                    },
                )),
                ..Default::default()
            })
            .collect(),
        ..Default::default()
    }
}
fn polygon() -> base_types::PolygonWithHoles {
    base_types::PolygonWithHoles {
        outline: MessageField::some(ring(&[
            (10, 30),
            (35, 30),
            (35, 60),
            (25, 60),
            (25, 50),
            (20, 50),
            (20, 60),
            (10, 60),
        ])),
        holes: vec![
            ring(&[(12, 32), (16, 32), (16, 36), (12, 36)]),
            ring(&[(27, 40), (31, 40), (31, 44), (27, 44)]),
        ],
        ..Default::default()
    }
}
fn main() -> Result<(), Error> {
    let output = PathBuf::from(
        env::args()
            .nth(1)
            .ok_or("usage: check-native-zones NEW_OUTPUT")?,
    );
    fs::create_dir(&output)?;
    let output = fs::canonicalize(output)?;
    fs::create_dir(output.join("fixture"))?;
    let board = output.join("fixture/board.kicad_pcb");
    let base = include_str!("../../../scripts/fixtures/layered-routing-rules.kicad_pcb");
    let zones = r#"
      (zone (net 1) (net_name "GND") (layer "F.Cu") (uuid "80000000-0000-4000-8000-000000000001") (name "template") (hatch edge 0.5) (connect_pads (clearance 0.2)) (min_thickness 0.2) (fill (thermal_gap 0.5) (thermal_bridge_width 0.5) (island_removal_mode 1)) (polygon (pts (xy 40 30) (xy 50 30) (xy 50 40) (xy 40 40))))
      (zone (net 1) (net_name "GND") (layer "F.Cu") (locked yes) (uuid "80000000-0000-4000-8000-000000000002") (name "locked retained") (hatch edge 0.5) (connect_pads (clearance 0.2)) (min_thickness 0.2) (fill (thermal_gap 0.5) (thermal_bridge_width 0.5) (island_removal_mode 1)) (polygon (pts (xy 60 30) (xy 70 30) (xy 70 40) (xy 60 40))))
      (zone (net 1) (net_name "GND") (layer "F.Cu") (uuid "80000000-0000-4000-8000-000000000003") (name "hatched retained") (hatch edge 0.5) (connect_pads (clearance 0.2)) (min_thickness 0.2) (fill (mode hatch) (thermal_gap 0.5) (thermal_bridge_width 0.5) (island_removal_mode 1) (hatch_thickness 0.3) (hatch_gap 0.5)) (polygon (pts (xy 75 30) (xy 85 30) (xy 85 40) (xy 75 40))))
    "#;
    fs::write(
        &board,
        format!("{}{zones})", &base[..base.rfind(')').unwrap()]),
    )?;
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
    let result: Result<(), Error> = (|| {
        let mut config = ClientConfig::new(&identity.endpoint, "owned-native-zone-acceptance")?;
        config.timeout = Duration::from_secs(60);
        let transport = RecordingTransport {
            inner: NngTransport::connect(config.clone())?,
            output: output.clone(),
            count: 0,
        };
        let mut ipc = KiCadClient::new(transport, config);
        if ipc.get_version()?.full_version != "10.0.7" {
            return Err("expected exact KiCad 10.0.7".into());
        }
        ipc.save_board(&identity.board_filename)?;
        editor.reload_owned()?;
        let baseline = observe_live_geometry(&mut ipc, &identity.board_filename)?;
        let existing = ipc.capture_existing_board_items(&identity.board_filename)?;
        let protected = fs::read(copy.project_path())?;
        let saved_before = fs::read(copy.board_path())?;
        let revision = outline_revision(&baseline)?;
        fs::write(output.join("before.kicad_pcb"), baseline.text.raw_text())?;
        let mut native = baseline.native.zones()[TEMPLATE].clone();
        native.id = MessageField::none();
        native.parent = MessageField::none();
        native.filled = false;
        native.filled_polygons.clear();
        native.name = "created concave with holes".into();
        native.priority = 3;
        native.outline.mut_or_insert_default().polygons = vec![polygon()];
        native.layers = vec![
            board_types::BoardLayer::BL_F_Cu.into(),
            board_types::BoardLayer::BL_In1_Cu.into(),
        ];
        let payload = SolidZonePayload::new(native.clone())?;
        for edits in [
            SolidZoneEdits {
                delete_ids: vec![LOCKED.into()],
                ..Default::default()
            },
            SolidZoneEdits {
                updates: vec![(
                    LOCKED.into(),
                    SolidZonePatch {
                        name: Some("bad".into()),
                        ..Default::default()
                    },
                )],
                ..Default::default()
            },
            SolidZoneEdits {
                delete_ids: vec![TEMPLATE.into(), TEMPLATE.into()],
                ..Default::default()
            },
            SolidZoneEdits {
                updates: vec![(TEMPLATE.into(), SolidZonePatch::default())],
                ..Default::default()
            },
            SolidZoneEdits {
                creates: vec![payload.clone(); 33],
                ..Default::default()
            },
        ] {
            if PreparedSolidZoneEdits::new(&baseline, &revision, edits).is_ok() {
                return Err("invalid mutation accepted".into());
            }
        }
        let changed_revision = kicad_model::RevisionDigest::new("f".repeat(64))?;
        if PreparedSolidZoneEdits::new(
            &baseline,
            &changed_revision,
            SolidZoneEdits {
                creates: vec![payload.clone()],
                ..Default::default()
            },
        )
        .is_ok()
        {
            return Err("stale revision accepted".into());
        }
        native.outline.mut_or_insert_default().polygons[0]
            .holes
            .push(polygon().holes[0].clone());
        if SolidZonePayload::new(native).is_ok() {
            return Err("overlapping holes accepted".into());
        }
        for bad in ["unknown-net", "disabled-layer"] {
            let mut native = payload.native().clone();
            if bad == "unknown-net" {
                let Some(board_types::zone::Settings::CopperSettings(c)) = &mut native.settings
                else {
                    unreachable!()
                };
                c.net.as_mut().unwrap().name = "not-an-observed-net".into();
            } else {
                native.layers = vec![board_types::BoardLayer::BL_In30_Cu.into()];
            }
            let invalid = PreparedSolidZoneEdits::new(
                &baseline,
                &revision,
                SolidZoneEdits {
                    creates: vec![SolidZonePayload::new(native)?],
                    ..Default::default()
                },
            )?;
            let commit = ipc.begin_commit()?;
            let refused = invalid.apply_in_transaction(&mut ipc, &identity.board_filename);
            ipc.drop_commit(&commit)?;
            if refused.is_ok() {
                return Err(format!("{bad} accepted").into());
            }
        }
        let before_apply = observe_live_geometry(&mut ipc, &identity.board_filename)?;
        if before_apply.native != baseline.native
            || before_apply.text.raw_text() != baseline.text.raw_text()
            || fs::read(copy.board_path())? != saved_before
        {
            return Err("refused requests changed board".into());
        }
        let create = PreparedSolidZoneEdits::new(
            &baseline,
            &revision,
            SolidZoneEdits {
                creates: vec![payload.clone()],
                ..Default::default()
            },
        )?;
        let commit = ipc.begin_commit()?;
        let receipts = match create.apply_in_transaction(&mut ipc, &identity.board_filename) {
            Ok(r) => r,
            Err(e) => {
                ipc.drop_commit(&commit)?;
                return Err(e.into());
            }
        };
        ipc.commit_changes(&commit, "Create solid copper zone")?;
        let created = receipts.created[0].source_uuid().to_owned();
        create.verify_definitions(
            &observe_live_geometry(&mut ipc, &identity.board_filename)?,
            &receipts,
        )?;
        ipc.save_board(&identity.board_filename)?;
        editor.reload_owned()?;
        let mut observed = observe_live_geometry(&mut ipc, &identity.board_filename)?;
        create.verify_definitions(&observed, &receipts)?;
        fs::copy(copy.board_path(), output.join("created.kicad_pcb"))?;
        fs::write(
            output.join("created-native.txt"),
            format!("{:#?}", observed.native.zones()[&created]),
        )?;
        for (index, patch) in [
            SolidZonePatch {
                name: Some("patched exact zone".into()),
                priority: Some(7),
                clearance_nm: Some(300_000),
                ..Default::default()
            },
            SolidZonePatch {
                net: Some(CreateNetIdentity { name: "VCC".into() }),
                layers: Some(vec![CopperLayer::FRONT, CopperLayer::BACK]),
                connection: Some(board_types::ZoneConnectionStyle::ZCS_PTH_THERMAL),
                thermal_gap_nm: Some(350_000),
                thermal_spoke_width_nm: Some(400_000),
                min_thickness_nm: Some(150_000),
                island_removal: Some(board_types::IslandRemovalMode::IRM_AREA),
                min_island_area_nm2: Some(2_000_000_000_000),
                ..Default::default()
            },
        ]
        .into_iter()
        .enumerate()
        {
            let edit = PreparedSolidZoneEdits::new(
                &observed,
                &outline_revision(&observed)?,
                SolidZoneEdits {
                    updates: vec![(created.clone(), patch)],
                    ..Default::default()
                },
            )?;
            let commit = ipc.begin_commit()?;
            let receipts = match edit.apply_in_transaction(&mut ipc, &identity.board_filename) {
                Ok(r) => r,
                Err(e) => {
                    ipc.drop_commit(&commit)?;
                    return Err(e.into());
                }
            };
            ipc.commit_changes(&commit, "Update solid copper zone")?;
            edit.verify_definitions(
                &observe_live_geometry(&mut ipc, &identity.board_filename)?,
                &receipts,
            )?;
            ipc.save_board(&identity.board_filename)?;
            editor.reload_owned()?;
            observed = observe_live_geometry(&mut ipc, &identity.board_filename)?;
            fs::write(
                output.join(format!("updated-{index}-live.kicad_pcb")),
                observed.text.raw_text(),
            )?;
            edit.verify_definitions(&observed, &receipts)?;
            fs::copy(
                copy.board_path(),
                output.join(format!("updated-{index}.kicad_pcb")),
            )?;
        }
        let mixed = PreparedSolidZoneEdits::new(
            &observed,
            &outline_revision(&observed)?,
            SolidZoneEdits {
                creates: vec![payload.clone()],
                updates: vec![(
                    TEMPLATE.into(),
                    SolidZonePatch {
                        name: Some("mixed patch".into()),
                        ..Default::default()
                    },
                )],
                delete_ids: vec![created.clone()],
            },
        )?;
        let commit = ipc.begin_commit()?;
        let mixed_receipts = match mixed.apply_in_transaction(&mut ipc, &identity.board_filename) {
            Ok(r) => r,
            Err(e) => {
                ipc.drop_commit(&commit)?;
                return Err(e.into());
            }
        };
        ipc.commit_changes(&commit, "Apply explicit zone definition batch")?;
        mixed.verify_definitions(
            &observe_live_geometry(&mut ipc, &identity.board_filename)?,
            &mixed_receipts,
        )?;
        ipc.save_board(&identity.board_filename)?;
        editor.reload_owned()?;
        observed = observe_live_geometry(&mut ipc, &identity.board_filename)?;
        mixed.verify_definitions(&observed, &mixed_receipts)?;
        let mixed_id = mixed_receipts.created[0].source_uuid().to_owned();
        let delete = PreparedSolidZoneEdits::new(
            &observed,
            &outline_revision(&observed)?,
            SolidZoneEdits {
                delete_ids: vec![mixed_id],
                updates: vec![(
                    TEMPLATE.into(),
                    SolidZonePatch {
                        name: Some("template".into()),
                        ..Default::default()
                    },
                )],
                ..Default::default()
            },
        )?;
        let commit = ipc.begin_commit()?;
        let receipts = match delete.apply_in_transaction(&mut ipc, &identity.board_filename) {
            Ok(r) => r,
            Err(e) => {
                ipc.drop_commit(&commit)?;
                return Err(e.into());
            }
        };
        ipc.commit_changes(&commit, "Delete explicit solid copper zone")?;
        delete.verify_definitions(
            &observe_live_geometry(&mut ipc, &identity.board_filename)?,
            &receipts,
        )?;
        ipc.save_board(&identity.board_filename)?;
        editor.reload_owned()?;
        let restored = observe_live_geometry(&mut ipc, &identity.board_filename)?;
        delete.verify_definitions(&restored, &receipts)?;
        if restored.text.structure_digest() != baseline.text.structure_digest()
            || !existing
                .matches_saved(&ipc.capture_existing_board_items(&identity.board_filename)?)?
            || fs::read(copy.project_path())? != protected
            || fs::read(copy.board_path())? != saved_before
        {
            return Err("deletion did not restore complete baseline".into());
        }
        let mut maximum_native = payload.native().clone();
        let exterior = (0..112)
            .map(|i| (10_000_000 + i * 250_000, 30_000_000))
            .chain((0..112).map(|i| (38_000_000, 30_000_000 + i * 250_000)))
            .chain((0..112).map(|i| (38_000_000 - i * 250_000, 58_000_000)))
            .chain((0..112).map(|i| (10_000_000, 58_000_000 - i * 250_000)))
            .map(|(x_nm, y_nm)| base_types::PolyLineNode {
                geometry: Some(base_types::poly_line_node::Geometry::Point(
                    base_types::Vector2 {
                        x_nm,
                        y_nm,
                        ..Default::default()
                    },
                )),
                ..Default::default()
            })
            .collect();
        maximum_native.outline.as_mut().unwrap().polygons = vec![base_types::PolygonWithHoles {
            outline: MessageField::some(base_types::PolyLine {
                closed: true,
                nodes: exterior,
                ..Default::default()
            }),
            holes: (0..4)
                .flat_map(|y| {
                    (0..4).map(move |x| {
                        let a = 12 + 5 * x;
                        let b = 32 + 5 * y;
                        ring(&[(a, b), (a + 1, b), (a + 1, b + 1), (a, b + 1)])
                    })
                })
                .collect(),
            ..Default::default()
        }];
        let maximum_payload = SolidZonePayload::new(maximum_native)?;
        let mut maximum_batch = vec![payload; 32];
        maximum_batch[0] = maximum_payload;
        let maximum = PreparedSolidZoneEdits::new(
            &restored,
            &outline_revision(&restored)?,
            SolidZoneEdits {
                creates: maximum_batch,
                ..Default::default()
            },
        )?;
        let commit = ipc.begin_commit()?;
        let receipts = maximum.apply_in_transaction(&mut ipc, &identity.board_filename);
        ipc.drop_commit(&commit)?;
        let receipts = receipts?;
        let cancelled = observe_live_geometry(&mut ipc, &identity.board_filename)?;
        if receipts.created.len() != 32
            || cancelled.native != restored.native
            || cancelled.text.raw_text() != restored.text.raw_text()
        {
            return Err("maximum creation cancellation changed board".into());
        }
        fs::write(
            output.join("result.json"),
            serde_json::to_vec_pretty(
                &json!({"kicad_version":"10.0.7","pid":identity.pid,"created_uuid":created,"baseline_zones":baseline.native.zones().len(),"create_with_concavity_and_two_holes":true,"updates":2,"net_and_layers_changed":true,"delete_restored_exact_saved_board":true,"locked_and_hatched_definitions_retained":true,"complete_unedited_live_fields_preserved":true,"same_process_reload":true,"maximum_creations_cancelled":32,"maximum_zone_vertices":512,"maximum_zone_holes":16,"mixed_create_update_delete":true,"unknown_net_and_disabled_layer_rejected":true,"excess_rejected":33,"source_and_project_preserved":true,"scope":"native definition editing only; refill, electrical effects, production MCP and approval remain unaccepted"}),
            )?,
        )?;
        Ok(())
    })();
    editor.close_owned(identity.pid)?;
    editor.cleanup()?;
    if fs::read(&board)? != original[0]
        || fs::read(board.with_extension("kicad_pro"))? != original[1]
    {
        return Err("source fixture changed".into());
    }
    fs::write(
        output.join("cleanup.json"),
        serde_json::to_vec_pretty(
            &json!({"owned_pid":identity.pid,"closed":true,"source_unchanged":true}),
        )?,
    )?;
    if let Err(e) = &result {
        fs::write(
            output.join("failure.json"),
            serde_json::to_vec_pretty(&json!({"error":e.to_string()}))?,
        )?;
    }
    fs::remove_dir(output.join("work"))?;
    result
}
