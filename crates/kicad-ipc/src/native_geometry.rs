//! Complete bounded native geometry evidence; observation does not authorize mutation.
use crate::*;
use std::collections::{BTreeMap, BTreeSet};

const ZONE: &str = "type.googleapis.com/kiapi.board.types.Zone";
const SHAPE: &str = "type.googleapis.com/kiapi.board.types.BoardGraphicShape";
const TEXT: &str = "type.googleapis.com/kiapi.common.commands.SaveDocumentToString";
const TEXT_REPLY: &str = "type.googleapis.com/kiapi.common.commands.SavedDocumentResponse";
pub const MAX_NATIVE_GEOMETRY_BYTES: usize = 32 * 1024 * 1024;
const MAX_ZONES: usize = 256;
const MAX_SHAPES: usize = 4096;
const MAX_POLYGON_NODES: usize = 500_000;

fn invalid(reason: &'static str) -> IpcError {
    IpcError::InvalidObservedBoardGraphic(reason)
}

/// Complete zone and graphic records, including unsupported geometry and unknown fields.
/// KiCad 10.0.7 does not serialize zone locks; use independently bound live board text.
#[derive(Clone, Debug, PartialEq)]
pub struct NativeBoardGeometry {
    zones: BTreeMap<String, board_types::Zone>,
    shapes: BTreeMap<String, board_types::BoardGraphicShape>,
}

fn polygon_nodes(poly: &base_types::PolySet) -> Result<usize, IpcError> {
    let mut count = 0usize;
    if poly.polygons.len() > MAX_POLYGON_NODES {
        return Err(invalid("polygon count exceeds observation bound"));
    }
    for polygon in &poly.polygons {
        if polygon.holes.len() > MAX_POLYGON_NODES {
            return Err(invalid("polygon hole count exceeds observation bound"));
        }
        for ring in polygon.outline.iter().chain(polygon.holes.iter()) {
            // Count arc control points as well as straight vertices.
            for node in &ring.nodes {
                count += match node.geometry {
                    Some(base_types::poly_line_node::Geometry::Arc(_)) => 3,
                    _ => 1,
                };
                if count > MAX_POLYGON_NODES {
                    return Err(invalid("polygon vertices exceed observation bound"));
                }
            }
        }
    }
    Ok(count)
}

impl NativeBoardGeometry {
    fn decode(items: &[Any]) -> Result<Self, IpcError> {
        if items.len() > MAX_ZONES + MAX_SHAPES {
            return Err(invalid("native geometry item count exceeds bound"));
        }
        let mut result = Self {
            zones: BTreeMap::new(),
            shapes: BTreeMap::new(),
        };
        let mut ids = BTreeSet::new();
        let mut bytes = 0usize;
        let mut nodes = 0usize;
        for item in items {
            bytes = bytes
                .checked_add(item.value.len())
                .ok_or(invalid("geometry size overflow"))?;
            if bytes > MAX_NATIVE_GEOMETRY_BYTES {
                return Err(invalid("native geometry bytes exceed bound"));
            }
            let id = match item.type_url.as_str() {
                ZONE => {
                    let zone = board_types::Zone::parse_from_bytes(&item.value)
                        .map_err(|_| IpcError::MalformedEnvelope)?;
                    let id =
                        kiid_value(zone.id.as_ref()).ok_or(invalid("zone identity missing"))?;
                    for poly in zone.outline.iter().chain(
                        zone.filled_polygons
                            .iter()
                            .filter_map(|p| p.shapes.as_ref()),
                    ) {
                        nodes += polygon_nodes(poly)?;
                        if nodes > MAX_POLYGON_NODES {
                            return Err(invalid("total zone vertices exceed observation bound"));
                        }
                    }
                    result.zones.insert(id.clone(), zone);
                    id
                }
                SHAPE => {
                    let shape = board_types::BoardGraphicShape::parse_from_bytes(&item.value)
                        .map_err(|_| IpcError::MalformedEnvelope)?;
                    let id =
                        kiid_value(shape.id.as_ref()).ok_or(invalid("shape identity missing"))?;
                    result.shapes.insert(id.clone(), shape);
                    id
                }
                _ => return Err(invalid("geometry query returned an unrequested item type")),
            };
            if id.is_empty() || id.len() > 128 || id.contains('\0') || !ids.insert(id) {
                return Err(invalid(
                    "native geometry identities must be bounded and unique",
                ));
            }
            if result.zones.len() > MAX_ZONES || result.shapes.len() > MAX_SHAPES {
                return Err(invalid("zone or shape count exceeds observation bound"));
            }
        }
        Ok(result)
    }

    pub fn zones(&self) -> &BTreeMap<String, board_types::Zone> {
        &self.zones
    }
    pub fn shapes(&self) -> &BTreeMap<String, board_types::BoardGraphicShape> {
        &self.shapes
    }

    /// Encode every observed field deterministically by kind and native identity.
    /// No parent, lock, definition or derived-fill field is discarded.
    pub fn evidence_bytes(&self) -> Result<Vec<u8>, IpcError> {
        let mut bytes = Vec::new();
        for (kind, records) in [
            (
                ZONE,
                self.zones
                    .values()
                    .map(Message::write_to_bytes)
                    .collect::<Result<Vec<_>, _>>(),
            ),
            (
                SHAPE,
                self.shapes
                    .values()
                    .map(Message::write_to_bytes)
                    .collect::<Result<Vec<_>, _>>(),
            ),
        ] {
            for record in records.map_err(|_| IpcError::MalformedEnvelope)? {
                bytes.extend_from_slice(&(kind.len() as u64).to_be_bytes());
                bytes.extend_from_slice(kind.as_bytes());
                bytes.extend_from_slice(&(record.len() as u64).to_be_bytes());
                bytes.extend_from_slice(&record);
            }
        }
        Ok(bytes)
    }
}

impl<T: KiCadTransport> KiCadClient<T> {
    /// Read complete board-level zones and graphic shapes on one stable editor/document.
    pub fn capture_board_geometry(
        &mut self,
        filename: &str,
    ) -> Result<NativeBoardGeometry, IpcError> {
        if filename.is_empty() || filename.len() > 4096 || filename.contains('\0') {
            return Err(invalid(
                "geometry observation requires a bounded board filename",
            ));
        }
        let token = self.token.clone().ok_or(IpcError::EmptyInstanceToken)?;
        let document = self.require_current_board(&token, filename)?;
        let reply: editor_commands::GetItemsResponse = self.bound_item_command(
            GET_ITEMS_TYPE_URL,
            GET_ITEMS_RESPONSE_TYPE_URL,
            editor_commands::GetItems {
                header: MessageField::some(board_item_header(filename)),
                types: [
                    enums::KiCadObjectType::KOT_PCB_ZONE,
                    enums::KiCadObjectType::KOT_PCB_SHAPE,
                ]
                .into_iter()
                .map(protobuf::EnumOrUnknown::new)
                .collect(),
                ..Default::default()
            },
        )?;
        self.validate_item_reply(reply.header, reply.status, filename)?;
        let geometry = NativeBoardGeometry::decode(&reply.items)?;
        if self.require_current_board(&token, filename)? != document {
            return Err(invalid(
                "board identity changed during geometry observation",
            ));
        }
        Ok(geometry)
    }

    /// Serialize the current live board without saving or changing its source files.
    pub fn live_board_text(&mut self, filename: &str) -> Result<String, IpcError> {
        if filename.is_empty() || filename.len() > 4096 || filename.contains('\0') {
            return Err(invalid(
                "geometry observation requires a bounded board filename",
            ));
        }
        let token = self.token.clone().ok_or(IpcError::EmptyInstanceToken)?;
        let document = self.require_current_board(&token, filename)?;
        let reply: editor_commands::SavedDocumentResponse = self.bound_item_command(
            TEXT,
            TEXT_REPLY,
            editor_commands::SaveDocumentToString {
                document: MessageField::some(document.clone()),
                ..Default::default()
            },
        )?;
        if reply.document.as_ref() != Some(&document)
            || reply.contents.is_empty()
            || reply.contents.len() > MAX_NATIVE_GEOMETRY_BYTES
        {
            return Err(invalid(
                "live board serialization has mismatched identity or size",
            ));
        }
        if self.require_current_board(&token, filename)? != document {
            return Err(invalid("board identity changed during live serialization"));
        }
        Ok(reply.contents)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn zone(id: &str) -> Any {
        let mut zone = board_types::Zone {
            id: MessageField::some(base_types::KIID {
                value: id.into(),
                ..Default::default()
            }),
            filled: true,
            ..Default::default()
        };
        zone.special_fields
            .mut_unknown_fields()
            .add_varint(1000, 42);
        Any {
            type_url: ZONE.into(),
            value: zone.write_to_bytes().unwrap(),
            ..Default::default()
        }
    }

    #[test]
    fn native_geometry_preserves_unknown_fields_and_requires_complete_bounded_sets() {
        let item = zone("zone");
        let observed = NativeBoardGeometry::decode(std::slice::from_ref(&item)).unwrap();
        assert_eq!(
            observed.zones()["zone"].write_to_bytes().unwrap(),
            item.value
        );
        assert_eq!(
            observed.zones()["zone"].locked.enum_value().unwrap(),
            base_types::LockedState::LS_UNKNOWN
        );
        assert!(NativeBoardGeometry::decode(&[item.clone(), item]).is_err());
        assert!(NativeBoardGeometry::decode(&[zone("")]).is_err());
        assert!(
            NativeBoardGeometry::decode(
                &(0..257).map(|i| zone(&i.to_string())).collect::<Vec<_>>()
            )
            .is_err()
        );
        let mut foreign = zone("different");
        foreign.type_url = PAD_TYPE_URL.into();
        assert!(NativeBoardGeometry::decode(&[foreign]).is_err());
        let mut changed = observed.clone();
        changed.zones.get_mut("zone").unwrap().filled = false;
        assert_ne!(
            observed.evidence_bytes().unwrap(),
            changed.evidence_bytes().unwrap()
        );
        changed = observed.clone();
        changed
            .zones
            .get_mut("zone")
            .unwrap()
            .special_fields
            .mut_unknown_fields()
            .add_varint(1001, 99);
        assert_ne!(
            observed.evidence_bytes().unwrap(),
            changed.evidence_bytes().unwrap()
        );
    }

    #[test]
    fn native_geometry_rejects_oversized_polygons() {
        let poly = base_types::PolySet {
            polygons: vec![base_types::PolygonWithHoles {
                outline: MessageField::some(base_types::PolyLine {
                    nodes: vec![base_types::PolyLineNode::default(); MAX_POLYGON_NODES + 1],
                    ..Default::default()
                }),
                ..Default::default()
            }],
            ..Default::default()
        };
        assert!(polygon_nodes(&poly).is_err());
    }
    struct Wire(std::collections::VecDeque<ApiResponse>);
    impl KiCadTransport for Wire {
        fn request(&mut self, _: ApiRequest) -> Result<ApiResponse, IpcError> {
            self.0.pop_front().ok_or(IpcError::MissingResponseMessage)
        }
    }
    fn response<M: Message>(kind: &str, value: M, token: &str) -> ApiResponse {
        ApiResponse {
            header: MessageField::some(envelope::ApiResponseHeader {
                kicad_token: token.into(),
                ..Default::default()
            }),
            status: MessageField::some(envelope::ApiResponseStatus {
                status: ApiStatusCode::AS_OK.into(),
                ..Default::default()
            }),
            message: MessageField::some(Any {
                type_url: kind.into(),
                value: value.write_to_bytes().unwrap(),
                ..Default::default()
            }),
            ..Default::default()
        }
    }
    fn document(name: &str) -> ApiResponse {
        response(
            "type.googleapis.com/kiapi.common.commands.GetOpenDocumentsResponse",
            editor_commands::GetOpenDocumentsResponse {
                documents: vec![board_document(name)],
                ..Default::default()
            },
            "same",
        )
    }
    fn client(replies: Vec<ApiResponse>) -> KiCadClient<Wire> {
        let mut client = KiCadClient::new(
            Wire(replies.into()),
            ClientConfig::new("ipc:///tmp/geometry-test", "geometry-test").unwrap(),
        );
        client.token = Some(InstanceToken::new("same").unwrap());
        client
    }
    #[test]
    fn live_text_requires_matching_response_document_and_stable_editor() {
        let text = |name: &str, token: &str| {
            response(
                TEXT_REPLY,
                editor_commands::SavedDocumentResponse {
                    document: MessageField::some(board_document(name)),
                    contents: "(kicad_pcb)".into(),
                    ..Default::default()
                },
                token,
            )
        };
        let good = vec![document("board"), text("board", "same"), document("board")];
        assert_eq!(
            client(good.clone()).live_board_text("board").unwrap(),
            "(kicad_pcb)"
        );
        for (index, replacement) in [
            (0, document("other")),
            (1, text("other", "same")),
            (1, text("board", "foreign")),
            (2, document("other")),
        ] {
            let mut replies = good.clone();
            replies[index] = replacement;
            assert!(client(replies).live_board_text("board").is_err());
        }
    }
    #[test]
    fn geometry_requires_exact_requested_types_and_stable_document() {
        let items = response(
            GET_ITEMS_RESPONSE_TYPE_URL,
            editor_commands::GetItemsResponse {
                header: MessageField::some(board_item_header("board")),
                status: base_types::ItemRequestStatus::IRS_OK.into(),
                items: vec![zone("zone")],
                ..Default::default()
            },
            "same",
        );
        let good = vec![document("board"), items, document("board")];
        assert_eq!(
            client(good.clone())
                .capture_board_geometry("board")
                .unwrap()
                .zones()
                .len(),
            1
        );
        let mut replies = good;
        replies[2] = document("other");
        assert!(client(replies).capture_board_geometry("board").is_err());
    }
}
