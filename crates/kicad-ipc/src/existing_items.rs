use super::*;
use std::collections::BTreeSet;

const FLIP: &str = "type.googleapis.com/kiapi.board.commands.FlipItems";
const FLIPPED: &str = "type.googleapis.com/kiapi.board.commands.FlipItemsResponse";
const DELETE: &str = "type.googleapis.com/kiapi.common.commands.DeleteItems";
const DELETED: &str = "type.googleapis.com/kiapi.common.commands.DeleteItemsResponse";
const CONNECTED: &str = "type.googleapis.com/kiapi.board.commands.GetConnectedItems";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FootprintFlipAxis {
    TopBottom,
    LeftRight,
}

/// Native deletion evidence is provisional until the saved board is verified.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum DeleteItemsReceipt {
    ExactItemStatuses {
        ids: Vec<String>,
    },
    /// KiCad 10.0.6 omits the item results. This is not proof of deletion.
    NeedsSavedVerification {
        ids: Vec<String>,
    },
}

fn invalid(reason: &'static str) -> IpcError {
    IpcError::InvalidObservedFootprint(reason)
}

pub(super) fn identifiers(ids: &[String], limit: usize) -> Result<BTreeSet<String>, IpcError> {
    let unique: BTreeSet<_> = ids.iter().cloned().collect();
    if ids.is_empty()
        || ids.len() > limit
        || unique.len() != ids.len()
        || ids.iter().any(|id| id.trim().is_empty() || id.len() > 128)
    {
        return Err(invalid(
            "item identities must be nonempty, unique and bounded",
        ));
    }
    Ok(unique)
}

impl<T: KiCadTransport> KiCadClient<T> {
    pub(super) fn bound_item_command<C: Message, R: Message>(
        &mut self,
        command_type: &'static str,
        response_type: &'static str,
        command: C,
    ) -> Result<R, IpcError> {
        let token = self.token.clone().ok_or(IpcError::EmptyInstanceToken)?;
        let request = command_request(&self.config, Some(&token), command_type, command)?;
        let response = self.transport.request(request)?;
        response_status(&response)?;
        if response_token(&response)? != token {
            return Err(invalid("editor identity changed during item command"));
        }
        decode_any::<R>(&response, response_type)
    }

    pub(super) fn validate_item_reply(
        &mut self,
        header: MessageField<base_types::ItemHeader>,
        status: protobuf::EnumOrUnknown<base_types::ItemRequestStatus>,
        filename: &str,
    ) -> Result<(), IpcError> {
        if status.enum_value() != Ok(base_types::ItemRequestStatus::IRS_OK) {
            return Err(invalid("item command did not return IRS_OK"));
        }
        let mut reply = editor_commands::GetItemsResponse {
            header,
            status,
            ..Default::default()
        };
        if reply.header.is_none() {
            let token = self.token.clone().ok_or(IpcError::EmptyInstanceToken)?;
            self.bind_headerless_board_items(&mut reply, &token, filename)?;
        }
        validate_get_items_document(&reply, filename)
    }

    /// Flip one footprint natively. Callers must enforce edit policy and saved postconditions.
    pub fn flip_existing_footprint(
        &mut self,
        filename: &str,
        id: &str,
        axis: FootprintFlipAxis,
    ) -> Result<FootprintInstanceTemplate, IpcError> {
        identifiers(&[id.to_owned()], 1)?;
        if filename.trim().is_empty() {
            return Err(invalid("missing board filename"));
        }
        let response: board_commands::FlipItemsResponse = self.bound_item_command(
            FLIP,
            FLIPPED,
            board_commands::FlipItems {
                header: MessageField::some(board_item_header(filename)),
                items: vec![base_types::KIID {
                    value: id.into(),
                    ..Default::default()
                }],
                direction: protobuf::EnumOrUnknown::new(match axis {
                    FootprintFlipAxis::TopBottom => {
                        board_commands::BoardFlipDirection::BFD_TOP_BOTTOM
                    }
                    FootprintFlipAxis::LeftRight => {
                        board_commands::BoardFlipDirection::BFD_LEFT_RIGHT
                    }
                }),
                ..Default::default()
            },
        )?;
        self.validate_item_reply(response.header.clone(), response.status, filename)?;
        if response.flipped_items.len() != 1 {
            return Err(invalid("flip receipt count mismatch"));
        }
        let item = &response.flipped_items[0];
        if item.status.as_ref().map(|s| s.code.enum_value())
            != Some(Ok(editor_commands::ItemStatusCode::ISC_OK))
        {
            return Err(invalid("flip item status failed"));
        }
        let value = item.item.as_ref().ok_or(IpcError::MissingResponseMessage)?;
        let reply = editor_commands::GetItemsResponse {
            status: protobuf::EnumOrUnknown::new(base_types::ItemRequestStatus::IRS_OK),
            items: vec![value.clone()],
            ..Default::default()
        };
        let template = footprint_templates_from_response(&reply)?.remove(0);
        if template.metadata().source_uuid != id {
            return Err(invalid("flip receipt identity mismatch"));
        }
        Ok(template)
    }

    /// Delete an exact ID set inside the caller's native transaction. Always verify saved removal.
    pub fn delete_board_items(
        &mut self,
        filename: &str,
        ids: &[String],
    ) -> Result<DeleteItemsReceipt, IpcError> {
        let expected = identifiers(ids, 100)?;
        if filename.trim().is_empty() {
            return Err(invalid("missing board filename"));
        }
        let version: base_commands::GetVersionResponse = self.bound_item_command(
            GET_VERSION_TYPE_URL,
            "type.googleapis.com/kiapi.common.commands.GetVersionResponse",
            base_commands::GetVersion::default(),
        )?;
        let version = version
            .version
            .into_option()
            .ok_or(IpcError::MissingResponseMessage)?;
        let response: editor_commands::DeleteItemsResponse = self.bound_item_command(
            DELETE,
            DELETED,
            editor_commands::DeleteItems {
                header: MessageField::some(board_item_header(filename)),
                item_ids: expected
                    .iter()
                    .map(|id| base_types::KIID {
                        value: id.clone(),
                        ..Default::default()
                    })
                    .collect(),
                ..Default::default()
            },
        )?;
        self.validate_item_reply(response.header, response.status, filename)?;
        let ids = expected.iter().cloned().collect();
        if response.deleted_items.is_empty() {
            if (version.major, version.minor, version.patch) == (10, 0, 6)
                && version.full_version == "10.0.6"
            {
                return Ok(DeleteItemsReceipt::NeedsSavedVerification { ids });
            }
            return Err(invalid(
                "missing native deletion results for unverified version",
            ));
        }
        let mut actual = BTreeSet::new();
        for item in response.deleted_items {
            let id =
                kiid_value(item.id.as_ref()).ok_or_else(|| invalid("missing deletion identity"))?;
            if item.status.enum_value() != Ok(editor_commands::ItemDeletionStatus::IDS_OK)
                || !actual.insert(id)
            {
                return Err(invalid("failed or duplicate native deletion result"));
            }
        }
        if actual != expected {
            return Err(invalid("native deletion identity set mismatch"));
        }
        Ok(DeleteItemsReceipt::ExactItemStatuses { ids })
    }

    /// Return a canonical partition of observed electrical pads by native copper connectivity.
    pub fn connected_pad_clusters(
        &mut self,
        filename: &str,
        pad_ids: &[String],
    ) -> Result<Vec<Vec<String>>, IpcError> {
        if filename.trim().is_empty() {
            return Err(invalid("missing board filename"));
        }
        if pad_ids.is_empty() {
            return Ok(Vec::new());
        }
        let all = identifiers(pad_ids, 4096)?;
        let mut pending = all.clone();
        let mut clusters = Vec::new();
        let deadline = std::time::Instant::now() + Duration::from_secs(30);
        while let Some(id) = pending.first().cloned() {
            if std::time::Instant::now() >= deadline {
                return Err(invalid("connectivity observation deadline exceeded"));
            }
            let response: editor_commands::GetItemsResponse = self.bound_item_command(
                CONNECTED,
                GET_ITEMS_RESPONSE_TYPE_URL,
                board_commands::GetConnectedItems {
                    header: MessageField::some(board_item_header(filename)),
                    items: vec![base_types::KIID {
                        value: id.clone(),
                        ..Default::default()
                    }],
                    types: vec![protobuf::EnumOrUnknown::new(
                        enums::KiCadObjectType::KOT_PCB_PAD,
                    )],
                    ..Default::default()
                },
            )?;
            self.validate_item_reply(response.header, response.status, filename)?;
            let mut cluster = BTreeSet::new();
            for item in response.items {
                if item.type_url != PAD_TYPE_URL {
                    return Err(invalid("connectivity returned a non-pad item"));
                }
                let pad = board_types::Pad::parse_from_bytes(&item.value)
                    .map_err(|_| IpcError::MalformedEnvelope)?;
                let member = kiid_value(pad.id.as_ref())
                    .ok_or_else(|| invalid("missing connected pad identity"))?;
                if !all.contains(&member) || !pending.contains(&member) || !cluster.insert(member) {
                    return Err(invalid("unknown, duplicate or inconsistent connected pad"));
                }
            }
            // A pad with no copper connection may have no native cluster entry.
            if cluster.is_empty() {
                cluster.insert(id.clone());
            }
            if !cluster.contains(&id) {
                return Err(invalid("connectivity cluster omitted its queried pad"));
            }
            for member in &cluster {
                pending.remove(member);
            }
            clusters.push(cluster.into_iter().collect());
        }
        clusters.sort();
        Ok(clusters)
    }
}
