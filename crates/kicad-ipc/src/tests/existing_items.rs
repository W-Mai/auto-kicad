use super::*;

fn reply<M: Message>(name: &str, message: M) -> ApiResponse {
    ApiResponse {
        header: MessageField::some(envelope::ApiResponseHeader {
            kicad_token: "token".into(),
            ..Default::default()
        }),
        status: MessageField::some(envelope::ApiResponseStatus {
            status: protobuf::EnumOrUnknown::new(ApiStatusCode::AS_OK),
            ..Default::default()
        }),
        message: MessageField::some(Any {
            type_url: format!("type.googleapis.com/{name}"),
            value: message.write_to_bytes().unwrap(),
            ..Default::default()
        }),
        ..Default::default()
    }
}

fn client(responses: Vec<ApiResponse>) -> KiCadClient<FakeTransport> {
    let mut client = footprint_client(responses[0].clone());
    client.transport.responses = responses;
    client.token = Some(InstanceToken::new("token").unwrap());
    client
}

#[test]
fn deletion_receipt_never_fabricates_item_success() {
    for fault in 0..10 {
        let mut version = reply(
            "kiapi.common.commands.GetVersionResponse",
            base_commands::GetVersionResponse {
                version: MessageField::some(base_types::KiCadVersion {
                    major: 10,
                    minor: 0,
                    patch: if fault == 2 { 7 } else { 6 },
                    full_version: if fault == 2 { "10.0.7" } else { "10.0.6" }.into(),
                    ..Default::default()
                }),
                ..Default::default()
            },
        );
        let item = editor_commands::ItemDeletionResult {
            id: MessageField::some(base_types::KIID {
                value: "item".into(),
                ..Default::default()
            }),
            status: protobuf::EnumOrUnknown::new(editor_commands::ItemDeletionStatus::IDS_OK),
            ..Default::default()
        };
        let mut message = editor_commands::DeleteItemsResponse {
            header: MessageField::some(board_item_header("main.kicad_pcb")),
            status: protobuf::EnumOrUnknown::new(base_types::ItemRequestStatus::IRS_OK),
            deleted_items: if fault == 1 || fault == 2 {
                vec![]
            } else {
                vec![item.clone()]
            },
            ..Default::default()
        };
        match fault {
            3 => message.deleted_items.push(item),
            4 => message.deleted_items[0].id.as_mut().unwrap().value = "foreign".into(),
            5 => {
                message.deleted_items[0].status = protobuf::EnumOrUnknown::new(
                    editor_commands::ItemDeletionStatus::IDS_NONEXISTENT,
                )
            }
            6 => {
                message.status =
                    protobuf::EnumOrUnknown::new(base_types::ItemRequestStatus::IRS_UNKNOWN)
            }
            7 => message.header = MessageField::some(board_item_header("foreign.kicad_pcb")),
            _ => {}
        }
        let mut response = reply("kiapi.common.commands.DeleteItemsResponse", message);
        if fault == 8 {
            response.header.as_mut().unwrap().kicad_token = "foreign".into();
        }
        if fault == 9 {
            version.header.as_mut().unwrap().kicad_token = "foreign".into();
        }
        let mut ipc = client(vec![version, response]);
        let result = ipc.delete_board_items("main.kicad_pcb", &["item".into()]);
        match fault {
            0 => assert!(matches!(
                result,
                Ok(DeleteItemsReceipt::ExactItemStatuses { .. })
            )),
            1 => assert!(matches!(
                result,
                Ok(DeleteItemsReceipt::NeedsSavedVerification { .. })
            )),
            _ => assert!(result.is_err(), "{fault}: {result:?}"),
        }
        assert_eq!(ipc.instance_token().unwrap().as_str(), "token");
    }
}

#[test]
fn connectivity_requires_disjoint_complete_known_pad_partition() {
    for fault in 0..6 {
        let mut first = vec![pad_item("a", "1"), pad_item("b", "2")];
        match fault {
            1 => first.push(pad_item("foreign", "3")),
            2 => first.push(pad_item("b", "2")),
            3 => first = vec![pad_item("b", "2")],
            4 => first[0].type_url = "wrong".into(),
            _ => {}
        }
        let first = get_items_response("token", base_types::ItemRequestStatus::IRS_OK, first);
        let second = get_items_response(
            "token",
            base_types::ItemRequestStatus::IRS_OK,
            if fault == 5 {
                vec![pad_item("a", "1"), pad_item("c", "3")]
            } else {
                vec![]
            },
        );
        let mut ipc = client(vec![first, second]);
        let result =
            ipc.connected_pad_clusters("main.kicad_pcb", &["a".into(), "b".into(), "c".into()]);
        if fault == 0 {
            assert_eq!(
                result.unwrap(),
                vec![vec!["a".to_owned(), "b".to_owned()], vec!["c".to_owned()]]
            );
        } else {
            assert!(result.is_err(), "fault {fault}");
        }
    }
}

#[test]
fn flip_checks_identity_status_type_document_and_token() {
    for fault in 0..7 {
        let mut item = footprint_item("fp");
        if fault == 2 {
            item = footprint_item("foreign");
        }
        if fault == 3 {
            item.type_url = "wrong".into();
        }
        let mut response = board_commands::FlipItemsResponse {
            header: MessageField::some(board_item_header(if fault == 4 {
                "foreign.kicad_pcb"
            } else {
                "main.kicad_pcb"
            })),
            status: protobuf::EnumOrUnknown::new(base_types::ItemRequestStatus::IRS_OK),
            flipped_items: vec![board_commands::ItemFlipResult {
                status: MessageField::some(editor_commands::ItemStatus {
                    code: protobuf::EnumOrUnknown::new(if fault == 1 {
                        editor_commands::ItemStatusCode::ISC_NONEXISTENT
                    } else {
                        editor_commands::ItemStatusCode::ISC_OK
                    }),
                    ..Default::default()
                }),
                item: MessageField::some(item),
                ..Default::default()
            }],
            ..Default::default()
        };
        if fault == 5 {
            response.flipped_items.clear();
        }
        let mut response = reply("kiapi.board.commands.FlipItemsResponse", response);
        if fault == 6 {
            response.header.as_mut().unwrap().kicad_token = "changed".into();
        }
        let mut ipc = client(vec![response]);
        let result =
            ipc.flip_existing_footprint("main.kicad_pcb", "fp", FootprintFlipAxis::LeftRight);
        assert_eq!(result.is_ok(), fault == 0, "{fault}: {result:?}");
    }
}

#[test]
fn invalid_item_batches_reject_before_native_requests() {
    let mut ipc = client(vec![version_response("token", 10)]);
    for ids in [
        vec![],
        vec!["".into()],
        vec!["a".into(), "a".into()],
        (0..101).map(|i| i.to_string()).collect(),
    ] {
        assert!(ipc.delete_board_items("main.kicad_pcb", &ids).is_err());
    }
    assert!(ipc.transport.requests.is_empty());
}
