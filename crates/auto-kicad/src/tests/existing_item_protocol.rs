use super::*;
use kicad_ipc::{DeleteItemsReceipt, ExistingItemUpdate, FootprintFlipAxis};

#[test]
#[ignore = "requires KiCad PCB Editor; native protocol probe, not MCP acceptance"]
fn live_existing_item_protocol_uses_one_editor() {
    let fixture = TempBoard::new();
    let input = fixture.root.join("board.kicad_pcb");
    let board = routing_fixture_board()
        .replace("(segment (start 7.5 5) (end 7.5 8) (width 0.25) (layer \"B.Cu\")", "(segment (start 5 5) (end 10 5) (width 0.25) (layer \"F.Cu\")")
        .replace("(pad \"1\" smd rect (at 0 0) (size 1 1) (layers \"B.Cu\"", "(pad \"1\" smd rect (at 0.4 0.2 30) (size 1 0.5) (layers \"B.Cu\"")
        .replace("(gr_line (start 0 0)", "(gr_line (start 2 12) (end 4 12) (stroke (width 0.15) (type default)) (layer \"Dwgs.User\") (uuid \"30000000-0000-4000-8000-000000000005\")) (gr_line (start 0 0)");
    let board = format!(
        "{}\n(via (at 12 12) (size 0.6) (drill 0.3) (layers \"F.Cu\" \"B.Cu\") (net 1) (uuid \"50000000-0000-4000-8000-000000000001\"))\n)",
        board.trim_end().strip_suffix(')').unwrap()
    );
    std::fs::write(&input, &board).unwrap();
    std::fs::write(input.with_extension("kicad_pro"), "{}").unwrap();
    let mut editor = OwnedPcbnewLauncher::new(&fixture.root.join("owned")).unwrap();
    let copy = editor.create_shadow(&input).unwrap();
    let identity = editor.launch(&copy).unwrap();
    let mut client = editor.connect_owned().unwrap();
    let version = client.get_version().unwrap();
    client.save_board(&identity.board_filename).unwrap();
    editor.reload_owned().unwrap();
    let before = observe_owned_shadow(&mut editor).unwrap();
    let pads = before
        .snapshot
        .native_footprints
        .as_ref()
        .unwrap()
        .placements
        .iter()
        .flat_map(|fp| fp.pads.iter().map(|p| p.source_uuid.to_string()))
        .collect::<Vec<_>>();
    let clusters = client
        .connected_pad_clusters(&identity.board_filename, &pads)
        .unwrap();
    assert_eq!(clusters.iter().map(Vec::len).collect::<Vec<_>>(), [2, 1]);
    let original_templates = client
        .capture_footprint_instance_templates(&identity.board_filename)
        .unwrap();
    let native_before = client
        .capture_existing_board_items(&identity.board_filename)
        .unwrap();
    for restore in [false, true] {
        let changes = vec![
            ExistingItemUpdate::Track {
                source_uuid: "40000000-0000-4000-8000-000000000001".into(),
                start_nm: None,
                end_nm: None,
                width_nm: Some(if restore { 250_000 } else { 300_000 }),
            },
            ExistingItemUpdate::Via {
                source_uuid: "50000000-0000-4000-8000-000000000001".into(),
                position_nm: Some((if restore { 12_000_000 } else { 12_200_000 }, 12_000_000)),
                diameter_nm: Some(if restore { 600_000 } else { 700_000 }),
                drill_nm: Some(if restore { 300_000 } else { 350_000 }),
            },
            ExistingItemUpdate::GraphicSegment {
                source_uuid: "30000000-0000-4000-8000-000000000005".into(),
                start_nm: None,
                end_nm: Some((if restore { 4_000_000 } else { 4_500_000 }, 12_000_000)),
                width_nm: Some(if restore { 150_000 } else { 200_000 }),
            },
        ];
        let current = client
            .capture_existing_board_items(&identity.board_filename)
            .unwrap();
        let commit = client.begin_commit().unwrap();
        let expected = client
            .update_existing_board_items(&identity.board_filename, &current, &changes)
            .unwrap();
        client
            .commit_changes(&commit, "native existing item update verification")
            .unwrap();
        client.save_board(&identity.board_filename).unwrap();
        editor.reload_owned().unwrap();
        let actual = client
            .capture_existing_board_items(&identity.board_filename)
            .unwrap();
        assert!(expected.matches_saved(&actual).unwrap());
        assert_eq!(
            client
                .connected_pad_clusters(&identity.board_filename, &pads)
                .unwrap(),
            clusters
        );
    }
    assert!(
        native_before
            .matches_saved(
                &client
                    .capture_existing_board_items(&identity.board_filename)
                    .unwrap()
            )
            .unwrap()
    );
    assert_eq!(
        observe_owned_shadow(&mut editor).unwrap().snapshot,
        before.snapshot
    );
    let target = "10000000-0000-4000-8000-000000000003";
    let mut flips = Vec::new();
    for axis in [FootprintFlipAxis::TopBottom, FootprintFlipAxis::LeftRight] {
        for step in 0..2 {
            let observed = observe_owned_shadow(&mut editor).unwrap();
            let fp = observed
                .snapshot
                .native_footprints
                .as_ref()
                .unwrap()
                .placements
                .iter()
                .find(|p| p.source_uuid.as_str() == target)
                .unwrap();
            let commit = client.begin_commit().unwrap();
            let receipt = client
                .flip_existing_footprint(&identity.board_filename, target, axis)
                .unwrap();
            assert_eq!(receipt.metadata().source_uuid, target);
            client
                .commit_changes(&commit, "native flip protocol verification")
                .unwrap();
            client.save_board(&identity.board_filename).unwrap();
            editor.reload_owned().unwrap();
            let after = observe_owned_shadow(&mut editor).unwrap();
            let actual = after
                .snapshot
                .native_footprints
                .as_ref()
                .unwrap()
                .placements
                .iter()
                .find(|p| p.source_uuid.as_str() == target)
                .unwrap();
            assert_eq!(actual.position, fp.position);
            assert_ne!(actual.side, fp.side);
            let angle = match axis {
                FootprintFlipAxis::TopBottom => -fp.orientation_mdeg,
                FootprintFlipAxis::LeftRight => 180_000 - fp.orientation_mdeg,
            }
            .rem_euclid(360_000);
            assert_eq!(actual.orientation_mdeg, angle);
            assert_eq!(actual.source_uuid, fp.source_uuid);
            assert_eq!(
                actual
                    .pads
                    .iter()
                    .map(|p| (&p.source_uuid, &p.number, &p.net))
                    .collect::<Vec<_>>(),
                fp.pads
                    .iter()
                    .map(|p| (&p.source_uuid, &p.number, &p.net))
                    .collect::<Vec<_>>()
            );
            assert_eq!(
                client
                    .connected_pad_clusters(&identity.board_filename, &pads)
                    .unwrap(),
                clusters
            );
            flips.push(json!({"axis":format!("{axis:?}"),"step":step,"snapshot":after.snapshot}));
        }
        let templates = client
            .capture_footprint_instance_templates(&identity.board_filename)
            .unwrap();
        assert_eq!(templates.len(), original_templates.len());
        for original in &original_templates {
            let actual = templates
                .iter()
                .find(|t| t.metadata().source_uuid == original.metadata().source_uuid)
                .unwrap();
            assert!(
                original.matches_saved_instance(actual).unwrap(),
                "flip round trip changed native fields for {}",
                original.metadata().source_uuid
            );
        }
    }
    let track = "40000000-0000-4000-8000-000000000001";
    let commit = client.begin_commit().unwrap();
    let dropped = client
        .delete_board_items(&identity.board_filename, &[track.into()])
        .unwrap();
    assert!(
        matches!(dropped, DeleteItemsReceipt::NeedsSavedVerification { .. }),
        "{dropped:?}"
    );
    client.drop_commit(&commit).unwrap();
    client.save_board(&identity.board_filename).unwrap();
    editor.reload_owned().unwrap();
    assert_eq!(
        observe_owned_shadow(&mut editor).unwrap().snapshot,
        before.snapshot
    );
    assert_eq!(
        client
            .connected_pad_clusters(&identity.board_filename, &pads)
            .unwrap(),
        clusters
    );
    let graphic = "30000000-0000-4000-8000-000000000005";
    let commit = client.begin_commit().unwrap();
    let deleted = client
        .delete_board_items(&identity.board_filename, &[graphic.into()])
        .unwrap();
    client
        .commit_changes(&commit, "native exact deletion protocol verification")
        .unwrap();
    client.save_board(&identity.board_filename).unwrap();
    editor.reload_owned().unwrap();
    let after = observe_owned_shadow(&mut editor).unwrap();
    let mut expected = before.snapshot.clone();
    expected
        .board
        .graphics
        .retain(|g| g.layer != kicad_model::BoardGraphicLayer::DwgsUser);
    assert_eq!(after.snapshot, expected);
    assert_eq!(
        client
            .connected_pad_clusters(&identity.board_filename, &pads)
            .unwrap(),
        clusters
    );
    assert_eq!(std::fs::read_to_string(&input).unwrap(), board);
    drop(client);
    editor.close_owned(identity.pid).unwrap();
    editor.cleanup().unwrap();
    assert!(!copy.root().exists());
    if let Some(path) = std::env::var_os("AUTO_KICAD_EXISTING_PROTOCOL_EVIDENCE") {
        std::fs::write(path,serde_json::to_vec_pretty(&json!({"passed":true,"transport":"native IPC, not MCP","kicad_version":version.full_version,"source_pid":identity.pid,"source_reused":true,"owned_editor_closed":true,"fixture_cleaned":true,"clusters":clusters,"flip_results":flips,"item_updates_round_trip":true,"delete_dropped":format!("{dropped:?}"),"delete_committed":format!("{deleted:?}"),"before":before.snapshot,"after":after.snapshot})).unwrap()).unwrap();
    }
}
