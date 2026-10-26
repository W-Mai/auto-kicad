use super::*;

fn fixture() -> (ProjectSnapshot, StableId) {
    let (mut snapshot, _, _, _) = observed_routing_snapshot(BoardSide::Front);
    let footprints = snapshot.native_footprints.as_mut().unwrap();
    let target = &mut footprints.placements[0];
    target.position = board_coordinate(0, 0);
    target.pads[0].position = LocalCoordinateNm { x_nm: 0, y_nm: 0 };
    target.pads[0].copper_shapes[0].shape = PadShape::Rectangle;
    target.pads[0].copper_shapes[0].size = SizeNm {
        x_nm: 1000,
        y_nm: 1000,
    };
    let id = target.placement_id.clone();
    rebind(&mut snapshot);
    snapshot.native_routing =
        Some(ObservedBoardRouting::new(id_board(&snapshot), vec![], vec![]).unwrap());
    (snapshot, id)
}

fn id_board(snapshot: &ProjectSnapshot) -> StableId {
    snapshot.board.document_id.clone().unwrap()
}

fn rebind(snapshot: &mut ProjectSnapshot) {
    let footprints = snapshot.native_footprints.as_ref().unwrap();
    snapshot.native_footprints = Some(
        ObservedBoardFootprints::new(id_board(snapshot), footprints.placements.clone()).unwrap(),
    );
}

fn track(snapshot: &mut ProjectSnapshot, geometry: ObservedTrackGeometry, layer: BoardLayer) {
    let board = id_board(snapshot);
    snapshot.native_routing = Some(
        ObservedBoardRouting::new(
            board.clone(),
            vec![ObservedTrack {
                track_id: kicad_model::board_track_id(&board, &id("track")),
                source_uuid: id("track"),
                geometry,
                width_nm: 1,
                layer,
                lock_state: LockState::Unlocked,
                net: Some(kicad_model::ObservedRoutingNet {
                    net_id: Some(board_net_id(&board, "FOREIGN")),
                    code: None,
                    name: "FOREIGN".into(),
                }),
                observed_revision: digest('0'),
            }],
            vec![],
        )
        .unwrap(),
    );
}

#[test]
fn copper_guard_encloses_rotated_corners_offsets_and_foreign_nets() {
    let (mut snapshot, target) = fixture();
    track(
        &mut snapshot,
        ObservedTrackGeometry::Segment {
            start: board_coordinate(480, 480),
            end: board_coordinate(900, 900),
        },
        BoardLayer::FrontCopper,
    );
    assert_eq!(
        connected_footprint_copper(&snapshot, &target, 1).unwrap(),
        vec![id("track")]
    );
    let pad = &mut snapshot.native_footprints.as_mut().unwrap().placements[0].pads[0];
    pad.copper_shapes[0].offset.x_nm = 1000;
    pad.orientation_mdeg = 37000;
    rebind(&mut snapshot);
    track(
        &mut snapshot,
        ObservedTrackGeometry::Segment {
            start: board_coordinate(1490, 0),
            end: board_coordinate(1600, 0),
        },
        BoardLayer::FrontCopper,
    );
    assert_eq!(
        connected_footprint_copper(&snapshot, &target, 1).unwrap(),
        vec![id("track")]
    );
    track(
        &mut snapshot,
        ObservedTrackGeometry::Segment {
            start: board_coordinate(0, 0),
            end: board_coordinate(10, 0),
        },
        BoardLayer::BackCopper,
    );
    assert!(
        connected_footprint_copper(&snapshot, &target, 1)
            .unwrap()
            .is_empty()
    );
}

#[test]
fn copper_guard_uses_the_curved_arc_in_both_directions() {
    let (mut snapshot, target) = fixture();
    let footprint = &mut snapshot.native_footprints.as_mut().unwrap().placements[0];
    footprint.position = board_coordinate(7071, 7071);
    footprint.pads[0].copper_shapes[0].size = SizeNm { x_nm: 10, y_nm: 10 };
    rebind(&mut snapshot);
    for reverse in [false, true] {
        let (start, end) = if reverse {
            (-10000, 10000)
        } else {
            (10000, -10000)
        };
        track(
            &mut snapshot,
            ObservedTrackGeometry::Arc {
                start: board_coordinate(start, 0),
                mid: board_coordinate(0, 10000),
                end: board_coordinate(end, 0),
            },
            BoardLayer::FrontCopper,
        );
        assert_eq!(
            connected_footprint_copper(&snapshot, &target, 1).unwrap(),
            vec![id("track")]
        );
        snapshot.native_footprints.as_mut().unwrap().placements[0]
            .position
            .y = -7071;
        rebind(&mut snapshot);
        assert!(
            connected_footprint_copper(&snapshot, &target, 1)
                .unwrap()
                .is_empty()
        );
        snapshot.native_footprints.as_mut().unwrap().placements[0]
            .position
            .y = 7071;
        rebind(&mut snapshot);
    }
}

#[test]
fn copper_guard_rejects_unobserved_shapes_locks_and_invalid_tolerance() {
    let (snapshot, target) = fixture();
    for shape in [PadShape::Unknown, PadShape::Custom] {
        let mut bad = snapshot.clone();
        bad.native_footprints.as_mut().unwrap().placements[0].pads[0].copper_shapes[0].shape =
            shape;
        rebind(&mut bad);
        assert!(connected_footprint_copper(&bad, &target, 1).is_err());
    }
    for state in [LockState::Unknown, LockState::Locked] {
        let mut bad = snapshot.clone();
        bad.native_footprints.as_mut().unwrap().placements[0].lock_state = state;
        rebind(&mut bad);
        assert!(connected_footprint_copper(&bad, &target, 1).is_err());
    }
    for tolerance in [0, -1, FOOTPRINT_COPPER_TOLERANCE_NM + 1] {
        assert!(connected_footprint_copper(&snapshot, &target, tolerance).is_err());
    }
}

#[test]
fn copper_guard_includes_through_vias_on_inner_layers() {
    let (mut snapshot, target) = fixture();
    let pad = &mut snapshot.native_footprints.as_mut().unwrap().placements[0].pads[0];
    pad.layers = vec![BoardLayer::InnerCopper(15)];
    pad.copper_shapes[0].layer = BoardLayer::InnerCopper(15);
    rebind(&mut snapshot);
    let board = id_board(&snapshot);
    let operation = op(
        OperationKind::AddThroughVia {
            board: board.clone(),
            intent: ThroughViaIntent {
                source_uuid: id("via"),
                position: board_coordinate(0, 0),
                size_nm: 1000,
                drill_nm: 500,
                net: None,
            },
        },
        "board",
        "via",
    );
    let snapshot = apply_operations(snapshot, &[operation]).unwrap();
    assert_eq!(
        connected_footprint_copper(&snapshot, &target, 1).unwrap(),
        vec![id("via")]
    );
}
