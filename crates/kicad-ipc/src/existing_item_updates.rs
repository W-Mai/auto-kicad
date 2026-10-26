use super::*;
use std::collections::BTreeMap;

const ARC_TYPE_URL: &str = "type.googleapis.com/kiapi.board.types.Arc";
const SHAPE: &str = "type.googleapis.com/kiapi.board.types.BoardGraphicShape";

/// An explicit field patch. Omitted values retain the native field verbatim.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ExistingItemUpdate {
    Track {
        source_uuid: String,
        start_nm: Option<(i64, i64)>,
        end_nm: Option<(i64, i64)>,
        width_nm: Option<i64>,
    },
    Via {
        source_uuid: String,
        position_nm: Option<(i64, i64)>,
        diameter_nm: Option<i64>,
        drill_nm: Option<i64>,
    },
    GraphicSegment {
        source_uuid: String,
        start_nm: Option<(i64, i64)>,
        end_nm: Option<(i64, i64)>,
        width_nm: Option<i64>,
    },
}

impl ExistingItemUpdate {
    pub fn source_uuid(&self) -> &str {
        match self {
            Self::Track { source_uuid, .. }
            | Self::Via { source_uuid, .. }
            | Self::GraphicSegment { source_uuid, .. } => source_uuid,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
enum NativeItem {
    Track(board_types::Track),
    Arc(board_types::Arc),
    Via(board_types::Via),
    Graphic(board_types::BoardGraphicShape),
}

fn invalid(reason: &'static str) -> IpcError {
    IpcError::InvalidObservedRouting(reason)
}

impl NativeItem {
    fn decode(item: &Any) -> Result<Self, IpcError> {
        match item.type_url.as_str() {
            TRACK_TYPE_URL => board_types::Track::parse_from_bytes(&item.value).map(Self::Track),
            ARC_TYPE_URL => board_types::Arc::parse_from_bytes(&item.value).map(Self::Arc),
            VIA_TYPE_URL => board_types::Via::parse_from_bytes(&item.value).map(Self::Via),
            SHAPE => {
                board_types::BoardGraphicShape::parse_from_bytes(&item.value).map(Self::Graphic)
            }
            _ => return Err(invalid("unsupported native existing-item type")),
        }
        .map_err(|_| IpcError::MalformedEnvelope)
    }

    fn id(&self) -> Result<String, IpcError> {
        let id = match self {
            Self::Track(v) => &v.id,
            Self::Arc(v) => &v.id,
            Self::Via(v) => &v.id,
            Self::Graphic(v) => &v.id,
        };
        kiid_value(id.as_ref()).ok_or_else(|| invalid("native item identity is missing"))
    }

    fn encoded(&self, persistent: bool) -> Result<Any, IpcError> {
        macro_rules! encode {
            ($value:expr, $url:expr) => {{
                let mut value = $value.clone();
                if persistent {
                    value.parent = MessageField::none();
                }
                ($url, value.write_to_bytes())
            }};
        }
        let (type_url, bytes) = match self {
            Self::Track(v) => encode!(v, TRACK_TYPE_URL),
            Self::Arc(v) => encode!(v, ARC_TYPE_URL),
            Self::Via(v) => encode!(v, VIA_TYPE_URL),
            Self::Graphic(v) => encode!(v, SHAPE),
        };
        Ok(Any {
            type_url: type_url.into(),
            value: bytes.map_err(|_| IpcError::MalformedEnvelope)?,
            ..Default::default()
        })
    }

    fn patch(&mut self, update: &ExistingItemUpdate) -> Result<(), IpcError> {
        let lock = match self {
            Self::Track(v) => v.locked,
            Self::Arc(v) => v.locked,
            Self::Via(v) => v.locked,
            Self::Graphic(v) => v.locked,
        };
        if lock.enum_value() != Ok(base_types::LockedState::LS_UNLOCKED) {
            return Err(invalid("existing item is locked or lock state is unknown"));
        }
        match (self, update) {
            (
                Self::Track(v),
                ExistingItemUpdate::Track {
                    start_nm,
                    end_nm,
                    width_nm,
                    ..
                },
            ) => {
                segment(
                    &mut v.start,
                    &mut v.end,
                    &mut v.width,
                    *start_nm,
                    *end_nm,
                    *width_nm,
                )?;
            }
            (
                Self::Graphic(v),
                ExistingItemUpdate::GraphicSegment {
                    start_nm,
                    end_nm,
                    width_nm,
                    ..
                },
            ) => {
                if v.layer.enum_value() != Ok(board_types::BoardLayer::BL_Dwgs_User) {
                    return Err(invalid("only Dwgs.User segments can be edited"));
                }
                let shape = v
                    .shape
                    .as_mut()
                    .ok_or_else(|| invalid("missing graphic shape"))?;
                let Some(base_types::graphic_shape::Geometry::Segment(line)) =
                    shape.geometry.as_mut()
                else {
                    return Err(invalid("graphic is not a straight segment"));
                };
                let width = &mut shape
                    .attributes
                    .as_mut()
                    .and_then(|a| a.stroke.as_mut())
                    .ok_or_else(|| invalid("missing graphic stroke"))?
                    .width;
                segment(
                    &mut line.start,
                    &mut line.end,
                    width,
                    *start_nm,
                    *end_nm,
                    *width_nm,
                )?;
            }
            (
                Self::Via(v),
                ExistingItemUpdate::Via {
                    position_nm,
                    diameter_nm,
                    drill_nm,
                    ..
                },
            ) => {
                if v.type_.enum_value() != Ok(board_types::ViaType::VT_THROUGH) {
                    return Err(invalid("only through vias can be edited"));
                }
                let stack = v
                    .pad_stack
                    .as_mut()
                    .ok_or_else(|| invalid("missing via pad stack"))?;
                if !matches!(
                    stack.type_.enum_value(),
                    Ok(board_types::PadStackType::PST_NORMAL
                        | board_types::PadStackType::PST_FRONT_INNER_BACK)
                ) || stack.copper_layers.is_empty()
                {
                    return Err(invalid("via pad stack is not uniform"));
                }
                let mut old_size = None;
                for layer in &stack.copper_layers {
                    let size = layer
                        .size
                        .as_ref()
                        .ok_or_else(|| invalid("missing via copper size"))?;
                    if layer.shape.enum_value() != Ok(board_types::PadStackShape::PSS_CIRCLE)
                        || size.x_nm <= 0
                        || size.x_nm != size.y_nm
                        || old_size.is_some_and(|old| old != size.x_nm)
                        || layer
                            .offset
                            .as_ref()
                            .is_some_and(|o| o.x_nm != 0 || o.y_nm != 0)
                        || !layer.custom_shapes.is_empty()
                    {
                        return Err(invalid("via copper is not uniform concentric circles"));
                    }
                    old_size = Some(size.x_nm);
                }
                let drill = stack
                    .drill
                    .as_mut()
                    .ok_or_else(|| invalid("missing via drill"))?;
                if drill.start_layer.enum_value() != Ok(board_types::BoardLayer::BL_F_Cu)
                    || drill.end_layer.enum_value() != Ok(board_types::BoardLayer::BL_B_Cu)
                    || drill.shape.enum_value() != Ok(board_types::DrillShape::DS_CIRCLE)
                {
                    return Err(invalid("via drill is not circular and through"));
                }
                let size = drill
                    .diameter
                    .as_mut()
                    .ok_or_else(|| invalid("missing drill size"))?;
                if size.x_nm <= 0 || size.x_nm != size.y_nm {
                    return Err(invalid("via drill diameter is not circular"));
                }
                let diameter = diameter_nm.unwrap_or(old_size.unwrap());
                let hole = drill_nm.unwrap_or(size.x_nm);
                positive(diameter)?;
                positive(hole)?;
                if hole >= diameter {
                    return Err(invalid("via drill must be smaller than copper diameter"));
                }
                if diameter_nm.is_some() {
                    for layer in &mut stack.copper_layers {
                        let size = layer.size.as_mut().unwrap();
                        size.x_nm = diameter;
                        size.y_nm = diameter;
                    }
                }
                if drill_nm.is_some() {
                    size.x_nm = hole;
                    size.y_nm = hole;
                }
                point(&mut v.position, *position_nm)?;
            }
            _ => {
                return Err(invalid(
                    "existing-item kind does not match its native identity",
                ));
            }
        }
        Ok(())
    }
}

fn positive(value: i64) -> Result<(), IpcError> {
    if !(1..=2_000_000_000).contains(&value) {
        return Err(invalid("item dimension is outside the supported range"));
    }
    Ok(())
}

fn point(
    value: &mut MessageField<base_types::Vector2>,
    patch: Option<(i64, i64)>,
) -> Result<(), IpcError> {
    let value = value
        .as_mut()
        .ok_or_else(|| invalid("missing item position"))?;
    if let Some((x, y)) = patch {
        if !(-2_000_000_000..=2_000_000_000).contains(&x)
            || !(-2_000_000_000..=2_000_000_000).contains(&y)
        {
            return Err(invalid("item position is outside the supported range"));
        }
        value.x_nm = x;
        value.y_nm = y;
    }
    Ok(())
}

fn segment(
    start: &mut MessageField<base_types::Vector2>,
    end: &mut MessageField<base_types::Vector2>,
    width: &mut MessageField<base_types::Distance>,
    start_nm: Option<(i64, i64)>,
    end_nm: Option<(i64, i64)>,
    width_nm: Option<i64>,
) -> Result<(), IpcError> {
    point(start, start_nm)?;
    point(end, end_nm)?;
    if start.as_ref().map(|p| (p.x_nm, p.y_nm)) == end.as_ref().map(|p| (p.x_nm, p.y_nm)) {
        return Err(invalid("segment cannot have coincident endpoints"));
    }
    let width = width
        .as_mut()
        .ok_or_else(|| invalid("missing segment width"))?;
    if let Some(value) = width_nm {
        positive(value)?;
        width.value_nm = value;
    }
    Ok(())
}

/// Complete routing and graphic-shape payloads, including fields absent from normalized observations.
#[derive(Clone, Debug, PartialEq)]
pub struct ExistingBoardItemSet {
    items: BTreeMap<String, NativeItem>,
}

impl ExistingBoardItemSet {
    fn decode(items: &[Any]) -> Result<Self, IpcError> {
        let mut records = BTreeMap::new();
        for item in items {
            let native = NativeItem::decode(item)?;
            if records.insert(native.id()?, native).is_some() {
                return Err(invalid("duplicate native item identity"));
            }
        }
        Ok(Self { items: records })
    }

    pub fn persistent_evidence(&self) -> Result<Vec<u8>, IpcError> {
        let mut evidence = Vec::new();
        for item in self.items.values() {
            let bytes = item
                .encoded(true)?
                .write_to_bytes()
                .map_err(|_| IpcError::MalformedEnvelope)?;
            evidence.extend_from_slice(&(bytes.len() as u64).to_be_bytes());
            evidence.extend_from_slice(&bytes);
        }
        Ok(evidence)
    }

    pub fn matches_saved(&self, actual: &Self) -> Result<bool, IpcError> {
        Ok(self.persistent_evidence()? == actual.persistent_evidence()?)
    }

    /// Derive an expected result solely from the observed payloads and explicit intent.
    pub fn updated(&self, changes: &[ExistingItemUpdate]) -> Result<Self, IpcError> {
        let ids = changes
            .iter()
            .map(|c| c.source_uuid().to_owned())
            .collect::<Vec<_>>();
        super::existing_items::identifiers(&ids, 100)?;
        let mut result = self.clone();
        for change in changes {
            let before = self
                .items
                .get(change.source_uuid())
                .ok_or_else(|| invalid("existing item was not found"))?;
            let item = result.items.get_mut(change.source_uuid()).unwrap();
            item.patch(change)?;
            if item == before {
                return Err(invalid("existing-item update has no effect"));
            }
        }
        Ok(result)
    }
}

impl<T: KiCadTransport> KiCadClient<T> {
    pub fn capture_existing_board_items(
        &mut self,
        filename: &str,
    ) -> Result<ExistingBoardItemSet, IpcError> {
        if filename.trim().is_empty() {
            return Err(invalid("missing board filename"));
        }
        let response: editor_commands::GetItemsResponse = self.bound_item_command(
            GET_ITEMS_TYPE_URL,
            GET_ITEMS_RESPONSE_TYPE_URL,
            editor_commands::GetItems {
                header: MessageField::some(board_item_header(filename)),
                types: [
                    enums::KiCadObjectType::KOT_PCB_TRACE,
                    enums::KiCadObjectType::KOT_PCB_ARC,
                    enums::KiCadObjectType::KOT_PCB_VIA,
                    enums::KiCadObjectType::KOT_PCB_SHAPE,
                ]
                .into_iter()
                .map(protobuf::EnumOrUnknown::new)
                .collect(),
                ..Default::default()
            },
        )?;
        self.validate_item_reply(response.header, response.status, filename)?;
        ExistingBoardItemSet::decode(&response.items)
    }

    /// Apply patches within the caller's transaction; verify saved state separately.
    pub fn update_existing_board_items(
        &mut self,
        filename: &str,
        before: &ExistingBoardItemSet,
        changes: &[ExistingItemUpdate],
    ) -> Result<ExistingBoardItemSet, IpcError> {
        let expected = before.updated(changes)?;
        if self.capture_existing_board_items(filename)? != *before {
            return Err(invalid("native existing items changed before update"));
        }
        let items = changes
            .iter()
            .map(|change| expected.items[change.source_uuid()].encoded(false))
            .collect::<Result<Vec<_>, _>>()?;
        let response: editor_commands::UpdateItemsResponse = self.bound_item_command(
            UPDATE_ITEMS_TYPE_URL,
            UPDATE_ITEMS_RESPONSE_TYPE_URL,
            editor_commands::UpdateItems {
                header: MessageField::some(board_item_header(filename)),
                items,
                ..Default::default()
            },
        )?;
        self.validate_item_reply(response.header, response.status, filename)?;
        let mut actual = BTreeMap::new();
        for item in response.updated_items {
            if item.status.as_ref().map(|s| s.code.enum_value())
                != Some(Ok(editor_commands::ItemStatusCode::ISC_OK))
            {
                return Err(invalid("existing item update status failed"));
            }
            let native =
                NativeItem::decode(item.item.as_ref().ok_or(IpcError::MissingResponseMessage)?)?;
            if actual.insert(native.id()?, native).is_some() {
                return Err(invalid("duplicate native update receipt"));
            }
        }
        if actual.len() != changes.len()
            || changes
                .iter()
                .any(|c| actual.get(c.source_uuid()) != expected.items.get(c.source_uuid()))
        {
            return Err(invalid(
                "native update receipt differs from complete expected payload",
            ));
        }
        Ok(expected)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    pub(super) fn fixture() -> ExistingBoardItemSet {
        let id = |s: &str| {
            MessageField::some(base_types::KIID {
                value: s.into(),
                ..Default::default()
            })
        };
        let mut track = NativeItem::decode(
            &create_track_item(&CreateTrack {
                start_x_nm: 1_000_000,
                start_y_nm: 2_000_000,
                end_x_nm: 3_000_000,
                end_y_nm: 2_000_000,
                width_nm: 200_000,
                net: Some(CreateNetIdentity {
                    name: "SIGNAL".into(),
                }),
            })
            .unwrap(),
        )
        .unwrap();
        let NativeItem::Track(v) = &mut track else {
            unreachable!()
        };
        v.id = id("track");
        v.parent = id("board");
        v.locked = protobuf::EnumOrUnknown::new(base_types::LockedState::LS_UNLOCKED);
        v.special_fields.mut_unknown_fields().add_varint(1000, 42);
        let mut via = NativeItem::decode(
            &create_through_via_item(&CreateThroughVia {
                position_x_nm: 2_000_000,
                position_y_nm: 2_000_000,
                pad_diameter_nm: 600_000,
                drill_diameter_nm: 300_000,
                net: Some(CreateNetIdentity {
                    name: "SIGNAL".into(),
                }),
            })
            .unwrap(),
        )
        .unwrap();
        let NativeItem::Via(v) = &mut via else {
            unreachable!()
        };
        v.id = id("via");
        v.parent = id("board");
        v.locked = protobuf::EnumOrUnknown::new(base_types::LockedState::LS_UNLOCKED);
        v.special_fields.mut_unknown_fields().add_varint(1000, 42);
        let stack = v.pad_stack.as_mut().unwrap();
        stack.secondary_drill = stack.drill.clone();
        stack.drill.as_mut().unwrap().capped =
            protobuf::EnumOrUnknown::new(board_types::ViaDrillCappingMode::VDCM_CAPPED);
        let mut graphic = NativeItem::decode(
            &board_graphic_segment_item(&BoardGraphicSegment {
                start_x_nm: 1_000_000,
                start_y_nm: 5_000_000,
                end_x_nm: 3_000_000,
                end_y_nm: 5_000_000,
                width_nm: 100_000,
                layer: BoardGraphicLayer::DwgsUser,
            })
            .unwrap(),
        )
        .unwrap();
        let NativeItem::Graphic(v) = &mut graphic else {
            unreachable!()
        };
        v.id = id("graphic");
        v.parent = id("board");
        v.locked = protobuf::EnumOrUnknown::new(base_types::LockedState::LS_UNLOCKED);
        v.special_fields.mut_unknown_fields().add_varint(1000, 42);
        ExistingBoardItemSet {
            items: [track, via, graphic]
                .into_iter()
                .map(|v| (v.id().unwrap(), v))
                .collect(),
        }
    }

    pub(super) fn changes() -> Vec<ExistingItemUpdate> {
        vec![
            ExistingItemUpdate::Track {
                source_uuid: "track".into(),
                start_nm: None,
                end_nm: None,
                width_nm: Some(250_000),
            },
            ExistingItemUpdate::Via {
                source_uuid: "via".into(),
                position_nm: None,
                diameter_nm: Some(700_000),
                drill_nm: Some(350_000),
            },
            ExistingItemUpdate::GraphicSegment {
                source_uuid: "graphic".into(),
                start_nm: None,
                end_nm: None,
                width_nm: Some(150_000),
            },
        ]
    }

    #[test]
    fn patches_preserve_all_non_requested_native_fields() {
        let before = fixture();
        let mut after = before.updated(&changes()).unwrap();
        let NativeItem::Track(v) = after.items.get_mut("track").unwrap() else {
            unreachable!()
        };
        assert_eq!(v.width.as_ref().unwrap().value_nm, 250_000);
        v.width.as_mut().unwrap().value_nm = 200_000;
        let NativeItem::Via(v) = after.items.get_mut("via").unwrap() else {
            unreachable!()
        };
        let stack = v.pad_stack.as_mut().unwrap();
        for layer in &mut stack.copper_layers {
            let size = layer.size.as_mut().unwrap();
            assert_eq!((size.x_nm, size.y_nm), (700_000, 700_000));
            size.x_nm = 600_000;
            size.y_nm = 600_000;
        }
        let drill = stack.drill.as_mut().unwrap().diameter.as_mut().unwrap();
        assert_eq!((drill.x_nm, drill.y_nm), (350_000, 350_000));
        drill.x_nm = 300_000;
        drill.y_nm = 300_000;
        let NativeItem::Graphic(v) = after.items.get_mut("graphic").unwrap() else {
            unreachable!()
        };
        let width = v
            .shape
            .as_mut()
            .unwrap()
            .attributes
            .as_mut()
            .unwrap()
            .stroke
            .as_mut()
            .unwrap()
            .width
            .as_mut()
            .unwrap();
        assert_eq!(width.value_nm, 150_000);
        width.value_nm = 100_000;
        assert_eq!(before, after);
        assert_eq!(
            before.persistent_evidence().unwrap(),
            after.persistent_evidence().unwrap()
        );
    }

    #[test]
    fn unsupported_or_ambiguous_patches_leave_the_original_unchanged() {
        for fault in 0..12 {
            let mut before = fixture();
            let mut updates = changes();
            match fault {
                0 => updates.push(updates[0].clone()),
                1 => updates.clear(),
                2 => {
                    if let ExistingItemUpdate::Track { source_uuid, .. } = &mut updates[0] {
                        *source_uuid = "missing".into();
                    }
                }
                3 => {
                    if let ExistingItemUpdate::Track { width_nm, .. } = &mut updates[0] {
                        *width_nm = Some(200_000);
                    }
                }
                4 => {
                    if let ExistingItemUpdate::Track { width_nm, .. } = &mut updates[0] {
                        *width_nm = Some(0);
                    }
                }
                5 => {
                    if let NativeItem::Track(v) = before.items.get_mut("track").unwrap() {
                        v.locked = protobuf::EnumOrUnknown::new(base_types::LockedState::LS_LOCKED);
                    }
                }
                6 => {
                    if let NativeItem::Graphic(v) = before.items.get_mut("graphic").unwrap() {
                        v.layer =
                            protobuf::EnumOrUnknown::new(board_types::BoardLayer::BL_Edge_Cuts);
                    }
                }
                7 => {
                    if let NativeItem::Via(v) = before.items.get_mut("via").unwrap() {
                        v.type_ = protobuf::EnumOrUnknown::new(board_types::ViaType::VT_MICRO);
                    }
                }
                8 => {
                    if let NativeItem::Via(v) = before.items.get_mut("via").unwrap() {
                        v.pad_stack.as_mut().unwrap().copper_layers[0]
                            .size
                            .as_mut()
                            .unwrap()
                            .x_nm = 500_000;
                    }
                }
                9 => {
                    if let ExistingItemUpdate::Via { drill_nm, .. } = &mut updates[1] {
                        *drill_nm = Some(700_000);
                    }
                }
                10 => {
                    if let ExistingItemUpdate::Track { start_nm, .. } = &mut updates[0] {
                        *start_nm = Some((3_000_000, 2_000_000));
                    }
                }
                11 => {
                    if let ExistingItemUpdate::Via { position_nm, .. } = &mut updates[1] {
                        *position_nm = Some((i64::MAX, 0));
                    }
                }
                _ => unreachable!(),
            }
            let saved = before.clone();
            assert!(before.updated(&updates).is_err(), "{fault}");
            assert_eq!(saved, before);
        }
    }
}

#[cfg(test)]
mod receipt_tests {
    use super::*;

    struct Replies(Vec<ApiResponse>);
    impl KiCadTransport for Replies {
        fn request(&mut self, _: ApiRequest) -> Result<ApiResponse, IpcError> {
            Ok(self.0.remove(0))
        }
    }

    fn reply<M: Message>(url: &str, value: M) -> ApiResponse {
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
                type_url: url.into(),
                value: value.write_to_bytes().unwrap(),
                ..Default::default()
            }),
            ..Default::default()
        }
    }

    #[test]
    fn update_receipts_require_exact_full_payloads_and_instance() {
        for fault in 0..9 {
            let before = super::tests::fixture();
            let changes = super::tests::changes();
            let expected = before.updated(&changes).unwrap();
            let mut captured = editor_commands::GetItemsResponse {
                header: MessageField::some(board_item_header("main.kicad_pcb")),
                status: protobuf::EnumOrUnknown::new(base_types::ItemRequestStatus::IRS_OK),
                items: before
                    .items
                    .values()
                    .map(|v| v.encoded(false).unwrap())
                    .collect(),
                ..Default::default()
            };
            if fault == 8 {
                captured.items.pop();
            }
            let mut updated = editor_commands::UpdateItemsResponse {
                header: MessageField::some(board_item_header("main.kicad_pcb")),
                status: protobuf::EnumOrUnknown::new(base_types::ItemRequestStatus::IRS_OK),
                updated_items: expected
                    .items
                    .values()
                    .map(|v| editor_commands::ItemUpdateResult {
                        status: MessageField::some(editor_commands::ItemStatus {
                            code: protobuf::EnumOrUnknown::new(
                                editor_commands::ItemStatusCode::ISC_OK,
                            ),
                            ..Default::default()
                        }),
                        item: MessageField::some(v.encoded(false).unwrap()),
                        ..Default::default()
                    })
                    .collect(),
                ..Default::default()
            };
            match fault {
                1 => {
                    updated.updated_items.pop();
                }
                2 => updated.updated_items[1] = updated.updated_items[0].clone(),
                3 => {
                    updated.updated_items[1].status.as_mut().unwrap().code =
                        protobuf::EnumOrUnknown::new(
                            editor_commands::ItemStatusCode::ISC_INVALID_DATA,
                        )
                }
                4 => updated.updated_items[1].item.as_mut().unwrap().type_url = "wrong".into(),
                5 => updated.header = MessageField::some(board_item_header("foreign.kicad_pcb")),
                6 => {
                    let item = updated
                        .updated_items
                        .iter_mut()
                        .find(|v| v.item.as_ref().unwrap().type_url == TRACK_TYPE_URL)
                        .unwrap();
                    let mut track =
                        board_types::Track::parse_from_bytes(&item.item.as_ref().unwrap().value)
                            .unwrap();
                    track.net.as_mut().unwrap().name = "FOREIGN".into();
                    item.item.as_mut().unwrap().value = track.write_to_bytes().unwrap();
                }
                _ => {}
            }
            let mut response = reply(UPDATE_ITEMS_RESPONSE_TYPE_URL, updated);
            if fault == 7 {
                response.header.as_mut().unwrap().kicad_token = "foreign".into();
            }
            let mut client = KiCadClient::new(
                Replies(vec![reply(GET_ITEMS_RESPONSE_TYPE_URL, captured), response]),
                ClientConfig::new("ipc:///tmp/fixture.sock", "fixture").unwrap(),
            );
            client.token = Some(InstanceToken::new("token").unwrap());
            let result = client.update_existing_board_items("main.kicad_pcb", &before, &changes);
            assert_eq!(result.is_ok(), fault == 0, "{fault}: {result:?}");
            assert_eq!(client.instance_token().unwrap().as_str(), "token");
            if fault == 8 {
                assert_eq!(client.transport.0.len(), 1);
            }
        }
    }
}
