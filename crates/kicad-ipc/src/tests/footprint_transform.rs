use super::*;

fn fixture() -> board_types::FootprintInstance {
    let item = definition_pad_footprint_item("fp");
    let mut instance = board_types::FootprintInstance::parse_from_bytes(&item.value).unwrap();
    instance
        .definition
        .as_mut()
        .unwrap()
        .id
        .as_mut()
        .unwrap()
        .library_nickname = "Test".into();
    instance.orientation.as_mut().unwrap().value_degrees = 0.0;
    instance.locked = protobuf::EnumOrUnknown::new(base_types::LockedState::LS_UNLOCKED);
    for field in [&mut instance.reference_field, &mut instance.value_field] {
        field.as_mut().unwrap().text.as_mut().unwrap().layer =
            protobuf::EnumOrUnknown::new(board_types::BoardLayer::BL_F_SilkS);
        let text = field
            .as_mut()
            .unwrap()
            .text
            .as_mut()
            .unwrap()
            .text
            .as_mut()
            .unwrap();
        text.position = MessageField::some(base_types::Vector2 {
            x_nm: 100,
            y_nm: 200,
            ..Default::default()
        });
    }
    let item = &mut instance.definition.as_mut().unwrap().items[0];
    let mut pad = board_types::Pad::parse_from_bytes(&item.value).unwrap();
    pad.position = MessageField::some(base_types::Vector2 {
        x_nm: 510,
        y_nm: 180,
        ..Default::default()
    });
    pad.pad_stack
        .as_mut()
        .unwrap()
        .angle
        .mut_or_insert_default()
        .value_degrees = 30.0;
    item.value = pad.write_to_bytes().unwrap();
    instance
}

fn any(instance: &board_types::FootprintInstance) -> Any {
    Any {
        type_url: FOOTPRINT_INSTANCE_TYPE_URL.into(),
        value: instance.write_to_bytes().unwrap(),
        ..Default::default()
    }
}

#[test]
fn rigid_transform_preserves_native_identity_and_moves_children() {
    let before = fixture();
    let mut after = before.clone();
    crate::footprint_transform::transform(&mut after, 1010, 1980, 90000).unwrap();
    let pad =
        board_types::Pad::parse_from_bytes(&after.definition.as_ref().unwrap().items[0].value)
            .unwrap();
    assert_eq!(
        (
            pad.position.as_ref().unwrap().x_nm,
            pad.position.as_ref().unwrap().y_nm
        ),
        (1210, 1480)
    );
    assert_eq!(
        pad.pad_stack
            .as_ref()
            .unwrap()
            .angle
            .as_ref()
            .unwrap()
            .value_degrees,
        120.0
    );
    let text = after
        .reference_field
        .as_ref()
        .unwrap()
        .text
        .as_ref()
        .unwrap()
        .text
        .as_ref()
        .unwrap();
    assert_eq!(
        (
            text.position.as_ref().unwrap().x_nm,
            text.position.as_ref().unwrap().y_nm
        ),
        (1230, 1890)
    );
    assert_eq!(after.id, before.id);
    assert_eq!(after.symbol_path, before.symbol_path);
    crate::footprint_transform::transform(&mut after, 10, -20, 0).unwrap();
    let mut canonical = before;
    crate::footprint_transform::transform(&mut canonical, 10, -20, 0).unwrap();
    assert_eq!(after, canonical);
}

#[test]
fn update_receipts_validate_all_children_statuses_and_token() {
    let before = fixture();
    let mut expected = before.clone();
    crate::footprint_transform::transform(&mut expected, 1010, 1980, 90000).unwrap();
    for fault in 0..9 {
        let mut response = editor_commands::UpdateItemsResponse {
            status: protobuf::EnumOrUnknown::new(base_types::ItemRequestStatus::IRS_OK),
            updated_items: vec![editor_commands::ItemUpdateResult {
                status: MessageField::some(editor_commands::ItemStatus {
                    code: protobuf::EnumOrUnknown::new(editor_commands::ItemStatusCode::ISC_OK),
                    ..Default::default()
                }),
                item: MessageField::some(any(&expected)),
                ..Default::default()
            }],
            ..Default::default()
        };
        match fault {
            1 => response.updated_items.clear(),
            2 => {
                response.updated_items[0].status.as_mut().unwrap().code =
                    protobuf::EnumOrUnknown::new(editor_commands::ItemStatusCode::ISC_IMMUTABLE)
            }
            7 | 8 => {
                response.updated_items[0].status.as_mut().unwrap().code =
                    protobuf::EnumOrUnknown::new(if fault == 7 {
                        editor_commands::ItemStatusCode::ISC_INVALID_DATA
                    } else {
                        editor_commands::ItemStatusCode::ISC_NONEXISTENT
                    });
            }
            3 => response.updated_items[0].item.as_mut().unwrap().type_url = "wrong".into(),
            4 => {
                let mut changed = expected.clone();
                changed.definition.as_mut().unwrap().items =
                    before.definition.as_ref().unwrap().items.clone();
                response.updated_items[0].item = MessageField::some(any(&changed));
            }
            5 => {
                let mut changed = expected.clone();
                changed.symbol_path = MessageField::none();
                response.updated_items[0].item = MessageField::some(any(&changed));
            }
            6 => {
                response.status =
                    protobuf::EnumOrUnknown::new(base_types::ItemRequestStatus::IRS_UNKNOWN)
            }
            _ => {}
        }
        let native = ApiResponse {
            header: MessageField::some(envelope::ApiResponseHeader {
                kicad_token: "updated".into(),
                ..Default::default()
            }),
            status: MessageField::some(envelope::ApiResponseStatus {
                status: protobuf::EnumOrUnknown::new(ApiStatusCode::AS_OK),
                ..Default::default()
            }),
            message: MessageField::some(Any {
                type_url: UPDATE_ITEMS_RESPONSE_TYPE_URL.into(),
                value: response.write_to_bytes().unwrap(),
                ..Default::default()
            }),
            ..Default::default()
        };
        let mut client = footprint_client(get_items_response(
            "observed",
            base_types::ItemRequestStatus::IRS_OK,
            vec![any(&before)],
        ));
        client.transport.responses.push(native);
        let result = client.update_footprint_instance("main.kicad_pcb", "fp", 1010, 1980, 90000);
        assert_eq!(result.is_ok(), fault == 0, "fault={fault}: {result:?}");
        assert_eq!(
            client.token.as_ref().unwrap().as_str(),
            if fault == 0 { "updated" } else { "observed" }
        );
        let request = editor_commands::UpdateItems::parse_from_bytes(
            &client.transport.requests[1].message.as_ref().unwrap().value,
        )
        .unwrap();
        assert_eq!(request.items, vec![any(&expected)]);
    }
}

#[test]
fn unsupported_child_geometry_rejects_before_update_request() {
    let mut before = fixture();
    before.definition.as_mut().unwrap().items.push(Any {
        type_url: "type.googleapis.com/kiapi.board.types.Zone".into(),
        ..Default::default()
    });
    let mut client = footprint_client(get_items_response(
        "observed",
        base_types::ItemRequestStatus::IRS_OK,
        vec![any(&before)],
    ));
    assert!(
        client
            .update_footprint_instance("main.kicad_pcb", "fp", 1010, 1980, 90000)
            .is_err()
    );
    assert_eq!(client.transport.requests.len(), 1);
}

#[test]
fn saved_template_comparison_excludes_only_board_container_identity() {
    let before = fixture();
    let mut client = footprint_client(get_items_response(
        "observed",
        base_types::ItemRequestStatus::IRS_OK,
        vec![any(&before)],
    ));
    let baseline = client
        .capture_footprint_instance_templates("main.kicad_pcb")
        .unwrap()
        .remove(0);
    let mut changed = before.clone();
    changed.parent = MessageField::some(base_types::KIID {
        value: "reloaded-board".into(),
        ..Default::default()
    });
    let mut after = baseline.clone();
    after.bytes = changed.write_to_bytes().unwrap();
    assert!(baseline.matches_saved_instance(&after).unwrap());
    assert_eq!(
        baseline.persistent_evidence().unwrap(),
        after.persistent_evidence().unwrap()
    );
    let item = &mut changed.definition.as_mut().unwrap().items[0];
    let mut pad = board_types::Pad::parse_from_bytes(&item.value).unwrap();
    pad.number = "changed".into();
    item.value = pad.write_to_bytes().unwrap();
    after.bytes = changed.write_to_bytes().unwrap();
    assert!(!baseline.matches_saved_instance(&after).unwrap());
    assert_ne!(
        baseline.persistent_evidence().unwrap(),
        after.persistent_evidence().unwrap()
    );
}

#[test]
fn copper_graphic_probe_rejects_unknown_layers_and_reports_copper_ids() {
    for layer in [
        board_types::BoardLayer::BL_F_Cu,
        board_types::BoardLayer::BL_In15_Cu,
        board_types::BoardLayer::BL_B_Cu,
        board_types::BoardLayer::BL_Dwgs_User,
        board_types::BoardLayer::BL_UNKNOWN,
    ] {
        let mut item = observed_segment_item(board_types::BoardLayer::BL_Dwgs_User);
        let mut graphic = board_types::BoardGraphicShape::parse_from_bytes(&item.value).unwrap();
        graphic.layer = protobuf::EnumOrUnknown::new(layer);
        let id = graphic.id.as_ref().unwrap().value.clone();
        item.value = graphic.write_to_bytes().unwrap();
        let mut client = footprint_client(get_items_response(
            "observed",
            base_types::ItemRequestStatus::IRS_OK,
            vec![],
        ));
        client.transport.responses.push(get_items_response(
            "updated",
            base_types::ItemRequestStatus::IRS_OK,
            vec![item],
        ));
        let result = client.get_unsupported_move_copper("main.kicad_pcb");
        match layer {
            board_types::BoardLayer::BL_UNKNOWN => assert!(result.is_err()),
            board_types::BoardLayer::BL_Dwgs_User => assert!(result.unwrap().is_empty()),
            _ => assert_eq!(result.unwrap(), vec![id]),
        }
    }
}

fn flip_fixture() -> board_types::FootprintInstance {
    let mut value = fixture();
    for f in [&mut value.reference_field, &mut value.value_field] {
        f.as_mut()
            .unwrap()
            .text
            .as_mut()
            .unwrap()
            .text
            .as_mut()
            .unwrap()
            .attributes
            .mut_or_insert_default()
            .angle
            .mut_or_insert_default();
    }
    let item = &mut value.definition.as_mut().unwrap().items[0];
    let mut pad = board_types::Pad::parse_from_bytes(&item.value).unwrap();
    let stack = pad.pad_stack.as_mut().unwrap();
    stack
        .layers
        .retain(|l| l.enum_value() != Ok(board_types::BoardLayer::BL_In2_Cu));
    stack
        .copper_layers
        .retain(|l| l.layer.enum_value() == Ok(board_types::BoardLayer::BL_F_Cu));
    stack.drill.as_mut().unwrap().start_layer =
        protobuf::EnumOrUnknown::new(board_types::BoardLayer::BL_F_Cu);
    stack.drill.as_mut().unwrap().end_layer =
        protobuf::EnumOrUnknown::new(board_types::BoardLayer::BL_B_Cu);
    item.value = pad.write_to_bytes().unwrap();
    value
}

#[test]
fn flip_comparison_preserves_geometry_metadata_and_unknown_fields() {
    let before = flip_fixture();
    let mut client = footprint_client(get_items_response(
        "observed",
        base_types::ItemRequestStatus::IRS_OK,
        vec![any(&before)],
    ));
    let baseline = client
        .capture_footprint_instance_templates("main.kicad_pcb")
        .unwrap()
        .remove(0);
    for axis in [FootprintFlipAxis::TopBottom, FootprintFlipAxis::LeftRight] {
        let expected = baseline.expected_flipped(axis).unwrap();
        let mut value = board_types::FootprintInstance::parse_from_bytes(&expected.bytes).unwrap();
        value.orientation.as_mut().unwrap().value_degrees += 360.0;
        value.parent = MessageField::some(base_types::KIID {
            value: "reloaded".into(),
            ..Default::default()
        });
        let item = &mut value.definition.as_mut().unwrap().items[0];
        let mut pad = board_types::Pad::parse_from_bytes(&item.value).unwrap();
        pad.pad_stack.as_mut().unwrap().layers.reverse();
        item.value = pad.write_to_bytes().unwrap();
        let mut actual = expected.clone();
        actual.bytes = value.write_to_bytes().unwrap();
        assert!(expected.matches_flip_result(&actual).unwrap());
        for fault in 0..5 {
            let mut corrupted = value.clone();
            match fault {
                0 => corrupted.orientation.as_mut().unwrap().value_degrees += 0.0001,
                1 => corrupted.position.as_mut().unwrap().x_nm += 1,
                2 => corrupted
                    .special_fields
                    .mut_unknown_fields()
                    .add_varint(998, 4),
                3 => corrupted.id.as_mut().unwrap().value.push('x'),
                _ => {
                    let item = &mut corrupted.definition.as_mut().unwrap().items[0];
                    let mut pad = board_types::Pad::parse_from_bytes(&item.value).unwrap();
                    pad.number.push('x');
                    item.value = pad.write_to_bytes().unwrap();
                }
            }
            actual.bytes = corrupted.write_to_bytes().unwrap();
            assert!(
                !expected.matches_flip_result(&actual).unwrap(),
                "fault {fault:?}"
            );
        }
    }
}

#[test]
fn flip_rejects_unmodelled_children_and_preserves_input() {
    let before = flip_fixture();
    let mut client = footprint_client(get_items_response(
        "observed",
        base_types::ItemRequestStatus::IRS_OK,
        vec![any(&before)],
    ));
    let mut baseline = client
        .capture_footprint_instance_templates("main.kicad_pcb")
        .unwrap()
        .remove(0);
    let mut bad = before;
    bad.definition.as_mut().unwrap().items.push(Any {
        type_url: "type.googleapis.com/unsupported".into(),
        value: vec![0],
        ..Default::default()
    });
    baseline.bytes = bad.write_to_bytes().unwrap();
    let bytes = baseline.bytes.clone();
    assert!(
        baseline
            .expected_flipped(FootprintFlipAxis::TopBottom)
            .is_err()
    );
    assert_eq!(baseline.bytes, bytes);
}

#[test]
fn zero_size_pad_drills_accept_only_empty_full_span_metadata() {
    for (a, b) in [
        (
            board_types::BoardLayer::BL_F_Cu,
            board_types::BoardLayer::BL_B_Cu,
        ),
        (
            board_types::BoardLayer::BL_B_Cu,
            board_types::BoardLayer::BL_F_Cu,
        ),
    ] {
        let mut drill = board_types::DrillProperties {
            start_layer: protobuf::EnumOrUnknown::new(a),
            end_layer: protobuf::EnumOrUnknown::new(b),
            shape: protobuf::EnumOrUnknown::new(board_types::DrillShape::DS_CIRCLE),
            diameter: MessageField::some(base_types::Vector2::default()),
            ..Default::default()
        };
        assert_eq!(observed_pad_drill(&drill).unwrap(), None);
        drill.start_layer = protobuf::EnumOrUnknown::new(board_types::BoardLayer::BL_In1_Cu);
        assert!(observed_pad_drill(&drill).is_err());
        drill.start_layer = protobuf::EnumOrUnknown::new(a);
        drill.diameter.as_mut().unwrap().x_nm = 10;
        assert!(observed_pad_drill(&drill).is_err());
    }
}
