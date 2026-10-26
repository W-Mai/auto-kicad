//! Bounded solid-zone definitions and exact native mutation receipts.
use crate::*;
use std::collections::{BTreeMap, BTreeSet};

const ZONE: &str = "type.googleapis.com/kiapi.board.types.Zone";
pub const MAX_ZONE_MUTATIONS: usize = 32;
type Point = kicad_geometry::Point;

fn invalid(reason: &'static str) -> IpcError {
    IpcError::InvalidObservedBoardGraphic(reason)
}
fn id(zone: &board_types::Zone) -> Result<String, IpcError> {
    let id = kiid_value(zone.id.as_ref()).ok_or(invalid("zone identity missing"))?;
    if id.trim().is_empty() || id.len() > 128 || id.contains('\0') {
        return Err(invalid("zone identity must be bounded and nonempty"));
    }
    Ok(id)
}
fn distance(value: &MessageField<base_types::Distance>, positive: bool) -> Result<(), IpcError> {
    if !value.as_ref().is_some_and(|v| {
        (i64::from(positive)..=kicad_geometry::MAX_COORDINATE_NM).contains(&v.value_nm)
    }) {
        return Err(invalid("zone distance is missing or outside bounds"));
    }
    Ok(())
}
fn set_distance(value: &mut MessageField<base_types::Distance>, patch: Option<i64>) {
    if let Some(nm) = patch {
        value.mut_or_insert_default().value_nm = nm;
    }
}
fn points(line: &base_types::PolyLine) -> Result<Vec<Point>, IpcError> {
    if !line.closed || line.nodes.len() > kicad_geometry::MAX_POLYGON_VERTICES {
        return Err(invalid("zone boundary must be closed and bounded"));
    }
    line.nodes
        .iter()
        .map(|node| match &node.geometry {
            Some(base_types::poly_line_node::Geometry::Point(p)) => Ok((p.x_nm, p.y_nm)),
            _ => Err(invalid("zone authoring requires straight polygon nodes")),
        })
        .collect()
}
fn polygon(zone: &board_types::Zone) -> Result<(), IpcError> {
    let polygons = &zone
        .outline
        .as_ref()
        .ok_or(invalid("zone outline missing"))?
        .polygons;
    if polygons.len() != 1 || polygons[0].holes.len() > kicad_geometry::MAX_POLYGON_HOLES {
        return Err(invalid("zone requires one exterior and at most 16 holes"));
    }
    let exterior = points(
        polygons[0]
            .outline
            .as_ref()
            .ok_or(invalid("zone exterior missing"))?,
    )?;
    let holes = polygons[0]
        .holes
        .iter()
        .map(points)
        .collect::<Result<Vec<_>, _>>()?;
    kicad_geometry::polygon_bounds(&exterior, &holes).map_err(|e| invalid(e.message()))?;
    Ok(())
}
fn copper(zone: &board_types::Zone) -> Result<&board_types::CopperZoneSettings, IpcError> {
    match &zone.settings {
        Some(board_types::zone::Settings::CopperSettings(c)) => Ok(c),
        _ => Err(invalid("solid zone requires copper settings")),
    }
}
fn definition(zone: &board_types::Zone) -> board_types::Zone {
    let mut zone = zone.clone();
    zone.filled = false;
    zone.filled_polygons.clear();
    zone.parent = MessageField::none();
    zone
}

/// Explicit fields only; all other native fields, including unknown fields, remain unchanged.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct SolidZonePatch {
    pub name: Option<String>,
    pub net: Option<CreateNetIdentity>,
    pub layers: Option<Vec<CopperLayer>>,
    pub polygon: Option<base_types::PolygonWithHoles>,
    pub priority: Option<u32>,
    pub clearance_nm: Option<i64>,
    pub min_thickness_nm: Option<i64>,
    pub connection: Option<board_types::ZoneConnectionStyle>,
    pub thermal_gap_nm: Option<i64>,
    pub thermal_spoke_width_nm: Option<i64>,
    pub island_removal: Option<board_types::IslandRemovalMode>,
    pub min_island_area_nm2: Option<u64>,
}

/// Validated complete unfilled definition. This type does not authorize locked-object mutation.
#[derive(Clone, Debug, PartialEq)]
pub struct SolidZonePayload(board_types::Zone);
impl SolidZonePayload {
    /// Preserve the complete supplied definition. Derived copper must be absent explicitly.
    pub fn new(zone: board_types::Zone) -> Result<Self, IpcError> {
        if zone.compute_size() > MAX_NATIVE_GEOMETRY_BYTES as u64
            || zone.type_.enum_value() != Ok(board_types::ZoneType::ZT_COPPER)
            || zone.name.len() > 512
            || zone.name.contains('\0')
            || zone.filled
            || !zone.filled_polygons.is_empty()
            || zone.layers.is_empty()
            || zone.layers.len() > 32
        {
            return Err(invalid(
                "solid zone definition type, size, layers or fill is invalid",
            ));
        }
        if zone.id.is_some() {
            id(&zone)?;
        }
        // This protocol version omits locks; caller policy must use independently bound live text.
        if zone.locked.enum_value() != Ok(base_types::LockedState::LS_UNKNOWN) {
            return Err(invalid(
                "zone lock writes are unsupported by the pinned protocol",
            ));
        }
        let mut layers = BTreeSet::new();
        for layer in &zone.layers {
            CopperLayer::try_from(routing_copper_layer(layer)?)?;
            if !layers.insert(layer.value()) {
                return Err(invalid("zone layers repeat"));
            }
        }
        let mut properties = BTreeSet::new();
        for entry in &zone.layer_properties {
            if !layers.contains(&entry.layer.value()) || !properties.insert(entry.layer.value()) {
                return Err(invalid(
                    "zone layer properties must belong to unique selected layers",
                ));
            }
        }
        polygon(&zone)?;
        let settings = copper(&zone)?;
        if settings.fill_mode.enum_value() != Ok(board_types::ZoneFillMode::ZFM_SOLID)
            || settings.teardrop.as_ref().map(|t| t.type_.enum_value())
                != Some(Ok(board_types::ZoneTeardropType::ZTDT_NONE))
            || !matches!(
                settings.island_mode.enum_value(),
                Ok(board_types::IslandRemovalMode::IRM_ALWAYS
                    | board_types::IslandRemovalMode::IRM_NEVER
                    | board_types::IslandRemovalMode::IRM_AREA)
            )
            || settings.min_island_area > i64::MAX as u64
        {
            return Err(invalid(
                "zone must be solid ordinary copper with known island settings",
            ));
        }
        distance(&settings.clearance, false)?;
        distance(&settings.min_thickness, true)?;
        let connection = settings
            .connection
            .as_ref()
            .ok_or(invalid("zone connection missing"))?;
        if !matches!(
            connection.zone_connection.enum_value(),
            Ok(board_types::ZoneConnectionStyle::ZCS_FULL
                | board_types::ZoneConnectionStyle::ZCS_THERMAL
                | board_types::ZoneConnectionStyle::ZCS_PTH_THERMAL)
        ) {
            return Err(invalid("zone connection style is unsupported"));
        }
        let thermal = connection
            .thermal_spokes
            .as_ref()
            .ok_or(invalid("zone thermal settings missing"))?;
        distance(&thermal.gap, false)?;
        distance(&thermal.width, true)?;
        let net = settings.net.as_ref().ok_or(invalid("zone net missing"))?;
        if net.name.is_empty()
            || net.name.len() > 4096
            || net.name.contains('\0')
            || net.code.is_some()
        {
            return Err(invalid(
                "zone net requires a bounded name without deprecated numeric code",
            ));
        }
        Ok(Self(zone))
    }
    pub fn native(&self) -> &board_types::Zone {
        &self.0
    }
    /// Clone a complete source and apply only explicit fields. Refill is required after commit.
    pub fn patched(source: &board_types::Zone, patch: &SolidZonePatch) -> Result<Self, IpcError> {
        id(source)?;
        let mut zone = definition(source);
        zone.parent = source.parent.clone();
        // Reject unsupported source kinds before applying any field.
        Self::new(zone.clone())?;
        if let Some(name) = &patch.name {
            zone.name = name.clone();
        }
        if let Some(priority) = patch.priority {
            zone.priority = priority;
        }
        if let Some(layers) = &patch.layers {
            zone.layers = layers.iter().map(|l| l.native().into()).collect();
        }
        if let Some(polygon) = &patch.polygon {
            zone.outline.mut_or_insert_default().polygons = vec![polygon.clone()];
        }
        let Some(board_types::zone::Settings::CopperSettings(c)) = &mut zone.settings else {
            unreachable!()
        };
        if let Some(net) = &patch.net {
            let n = c.net.mut_or_insert_default();
            n.name = net.name.clone();
        }
        set_distance(&mut c.clearance, patch.clearance_nm);
        set_distance(&mut c.min_thickness, patch.min_thickness_nm);
        if let Some(mode) = patch.island_removal {
            c.island_mode = mode.into();
        }
        if let Some(area) = patch.min_island_area_nm2 {
            c.min_island_area = area;
        }
        let connection = c.connection.mut_or_insert_default();
        if let Some(style) = patch.connection {
            connection.zone_connection = style.into();
        }
        let thermal = connection.thermal_spokes.mut_or_insert_default();
        set_distance(&mut thermal.gap, patch.thermal_gap_nm);
        set_distance(&mut thermal.width, patch.thermal_spoke_width_nm);
        if definition(&zone) == definition(source) {
            return Err(invalid("zone patch is empty or has no effect"));
        }
        Self::new(zone)
    }
}

/// Complete accepted native record. Saved equality omits only the transient board parent.
#[derive(Clone, Debug, PartialEq)]
pub struct SolidZoneReceipt {
    native: board_types::Zone,
}
impl SolidZoneReceipt {
    pub fn native(&self) -> &board_types::Zone {
        &self.native
    }
    pub fn source_uuid(&self) -> &str {
        &self.native.id.as_ref().expect("validated identity").value
    }
    /// Compare this receipt with the complete requested definition, including all unknown fields.
    pub fn matches_definition(&self, expected: &SolidZonePayload) -> bool {
        let Ok(value) = self.native.write_to_bytes() else {
            return false;
        };
        receipt(
            &Any {
                type_url: ZONE.into(),
                value,
                ..Default::default()
            },
            expected,
            expected.native().id.is_none(),
        )
        .is_ok()
    }
    pub fn matches_saved(&self, actual: &board_types::Zone) -> bool {
        let mut expected = self.native.clone();
        let mut actual = actual.clone();
        expected.parent = MessageField::none();
        actual.parent = MessageField::none();
        expected == actual
    }
}

fn receipt(
    item: &Any,
    expected: &SolidZonePayload,
    creating: bool,
) -> Result<SolidZoneReceipt, IpcError> {
    if item.type_url != ZONE {
        return Err(invalid("zone receipt type differs"));
    }
    let actual = board_types::Zone::parse_from_bytes(&item.value)
        .map_err(|_| IpcError::MalformedEnvelope)?;
    id(&actual)?;
    if actual.filled {
        return Err(invalid("zone receipt unexpectedly contains filled copper"));
    }
    // KiCad retains empty per-layer containers after UnFill. Classify each complete record.
    let mut empty_layers = BTreeSet::new();
    for fill in &actual.filled_polygons {
        let empty = board_types::ZoneFilledPolygons {
            layer: fill.layer,
            shapes: MessageField::some(base_types::PolySet::default()),
            ..Default::default()
        };
        if fill != &empty
            || !actual.layers.contains(&fill.layer)
            || !empty_layers.insert(fill.layer.value())
        {
            return Err(invalid(
                "zone receipt has nonempty or unclassified fill data",
            ));
        }
    }
    let mut compared = actual.clone();
    compared.filled_polygons.clear();
    if creating {
        compared.id = MessageField::none();
        compared.parent = MessageField::none();
    }
    if compared != expected.0 {
        return Err(invalid(
            "zone receipt differs from complete expected definition",
        ));
    }
    Ok(SolidZoneReceipt { native: actual })
}

impl<T: KiCadTransport> KiCadClient<T> {
    fn zone_context(
        &mut self,
        filename: &str,
        payloads: &[SolidZonePayload],
    ) -> Result<(), IpcError> {
        if payloads.is_empty() || payloads.len() > MAX_ZONE_MUTATIONS {
            return Err(invalid("zone batch requires 1..32 definitions"));
        }
        if payloads
            .iter()
            .try_fold(0u64, |size, p| size.checked_add(p.0.compute_size()))
            .is_none_or(|size| size > MAX_NATIVE_GEOMETRY_BYTES as u64)
        {
            return Err(invalid("aggregate zone definition bytes exceed bound"));
        }
        let token = self.token.clone().ok_or(IpcError::EmptyInstanceToken)?;
        let version: base_commands::GetVersionResponse = self.bound_item_command(
            GET_VERSION_TYPE_URL,
            "type.googleapis.com/kiapi.common.commands.GetVersionResponse",
            base_commands::GetVersion::default(),
        )?;
        if version
            .version
            .as_ref()
            .is_none_or(|v| v.full_version != "10.0.7" || (v.major, v.minor, v.patch) != (10, 0, 7))
        {
            return Err(invalid("zone writes require exact KiCad 10.0.7"));
        }
        self.require_current_board(&token, filename)?;
        let nets = self.get_board_nets(filename)?;
        self.require_current_board(&token, filename)?;
        let stackup = self.get_board_stackup(filename)?;
        for payload in payloads {
            let net = copper(&payload.0)?.net.as_ref().unwrap();
            if nets.iter().filter(|n| n.name == net.name).count() != 1 {
                return Err(invalid(
                    "zone net name is missing or ambiguous in the current observation",
                ));
            }
            for layer in &payload.0.layers {
                let identity = match routing_copper_layer(layer)? {
                    ObservedBoardLayer::FrontCopper => ObservedStackupLayerIdentity::FrontCopper,
                    ObservedBoardLayer::BackCopper => ObservedStackupLayerIdentity::BackCopper,
                    ObservedBoardLayer::InnerCopper(i) => {
                        ObservedStackupLayerIdentity::InnerCopper(i)
                    }
                    _ => return Err(invalid("zone layer is not copper")),
                };
                if !stackup
                    .layers
                    .iter()
                    .any(|l| l.layer == identity && l.enabled)
                {
                    return Err(invalid("zone layer is not enabled in observed stackup"));
                }
            }
        }
        self.require_current_board(&token, filename)?;
        Ok(())
    }
    /// Create definitions inside the caller's transaction; fill and saved preservation remain separate gates.
    pub fn create_solid_zones(
        &mut self,
        filename: &str,
        before: &NativeBoardGeometry,
        payloads: &[SolidZonePayload],
    ) -> Result<Vec<SolidZoneReceipt>, IpcError> {
        if payloads.is_empty()
            || payloads.len() > MAX_ZONE_MUTATIONS
            || before.zones().len() + payloads.len() > 256
            || payloads
                .iter()
                .any(|p| p.0.id.is_some() || p.0.parent.is_some())
        {
            return Err(invalid(
                "zone creation requires bounded new definitions without existing identities",
            ));
        }
        self.zone_context(filename, payloads)?;
        if self.capture_board_geometry(filename)? != *before {
            return Err(invalid("zone creation observation changed"));
        }
        let items = payloads
            .iter()
            .map(|p| {
                Ok(Any {
                    type_url: ZONE.into(),
                    value: p
                        .0
                        .write_to_bytes()
                        .map_err(|_| IpcError::MalformedEnvelope)?,
                    ..Default::default()
                })
            })
            .collect::<Result<Vec<_>, IpcError>>()?;
        let reply: editor_commands::CreateItemsResponse = self.bound_item_command(
            CREATE_ITEMS_TYPE_URL,
            CREATE_ITEMS_RESPONSE_TYPE_URL,
            editor_commands::CreateItems {
                header: MessageField::some(board_item_header(filename)),
                items,
                ..Default::default()
            },
        )?;
        self.validate_item_reply(reply.header, reply.status, filename)?;
        if reply.created_items.len() != payloads.len() {
            return Err(invalid("zone creation receipt count differs"));
        }
        let mut ids = BTreeSet::new();
        reply
            .created_items
            .iter()
            .zip(payloads)
            .map(|(r, p)| {
                if r.status.as_ref().map(|s| s.code.enum_value())
                    != Some(Ok(editor_commands::ItemStatusCode::ISC_OK))
                {
                    return Err(invalid("zone creation item failed"));
                }
                let receipt = receipt(
                    r.item.as_ref().ok_or(IpcError::MissingResponseMessage)?,
                    p,
                    true,
                )?;
                if before.zones().contains_key(receipt.source_uuid())
                    || before.shapes().contains_key(receipt.source_uuid())
                    || !ids.insert(receipt.source_uuid().to_owned())
                {
                    return Err(invalid("zone creation identities collide"));
                }
                Ok(receipt)
            })
            .collect()
    }
    /// Apply complete typed patches inside the caller's transaction. Callers must verify live-text locks.
    pub fn update_solid_zones(
        &mut self,
        filename: &str,
        before: &NativeBoardGeometry,
        changes: &[(String, SolidZonePatch)],
    ) -> Result<Vec<SolidZoneReceipt>, IpcError> {
        super::existing_items::identifiers(
            &changes.iter().map(|(id, _)| id.clone()).collect::<Vec<_>>(),
            MAX_ZONE_MUTATIONS,
        )?;
        let payloads = changes
            .iter()
            .map(|(id, patch)| {
                SolidZonePayload::patched(
                    before
                        .zones()
                        .get(id)
                        .ok_or(invalid("zone update target missing"))?,
                    patch,
                )
            })
            .collect::<Result<Vec<_>, _>>()?;
        self.zone_context(filename, &payloads)?;
        if self.capture_board_geometry(filename)? != *before {
            return Err(invalid("zone update observation changed"));
        }
        let items = payloads
            .iter()
            .map(|p| {
                Ok(Any {
                    type_url: ZONE.into(),
                    value: p
                        .0
                        .write_to_bytes()
                        .map_err(|_| IpcError::MalformedEnvelope)?,
                    ..Default::default()
                })
            })
            .collect::<Result<Vec<_>, IpcError>>()?;
        let reply: editor_commands::UpdateItemsResponse = self.bound_item_command(
            UPDATE_ITEMS_TYPE_URL,
            UPDATE_ITEMS_RESPONSE_TYPE_URL,
            editor_commands::UpdateItems {
                header: MessageField::some(board_item_header(filename)),
                items,
                ..Default::default()
            },
        )?;
        self.validate_item_reply(reply.header, reply.status, filename)?;
        let expected = payloads
            .iter()
            .map(|p| Ok((id(&p.0)?, p)))
            .collect::<Result<BTreeMap<_, _>, IpcError>>()?;
        let mut result = BTreeMap::new();
        for r in reply.updated_items {
            if r.status.as_ref().map(|s| s.code.enum_value())
                != Some(Ok(editor_commands::ItemStatusCode::ISC_OK))
            {
                return Err(invalid("zone update item failed"));
            }
            let item = r.item.as_ref().ok_or(IpcError::MissingResponseMessage)?;
            let native = board_types::Zone::parse_from_bytes(&item.value)
                .map_err(|_| IpcError::MalformedEnvelope)?;
            let id = id(&native)?;
            let wanted = expected
                .get(&id)
                .ok_or(invalid("unrequested zone update receipt"))?;
            if result.insert(id, receipt(item, wanted, false)?).is_some() {
                return Err(invalid("duplicate zone update receipt"));
            }
        }
        if result.len() != payloads.len() {
            return Err(invalid("zone update receipt count differs"));
        }
        Ok(result.into_values().collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn fixture() -> board_types::Zone {
        let d = |nm| {
            MessageField::some(base_types::Distance {
                value_nm: nm,
                ..Default::default()
            })
        };
        board_types::Zone {
            type_: board_types::ZoneType::ZT_COPPER.into(),
            layers: vec![board_types::BoardLayer::BL_F_Cu.into()],
            outline: MessageField::some(base_types::PolySet {
                polygons: vec![base_types::PolygonWithHoles {
                    outline: MessageField::some(base_types::PolyLine {
                        closed: true,
                        nodes: [
                            (0, 0),
                            (10_000_000, 0),
                            (10_000_000, 10_000_000),
                            (0, 10_000_000),
                        ]
                        .into_iter()
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
                        .collect(),
                        ..Default::default()
                    }),
                    ..Default::default()
                }],
                ..Default::default()
            }),
            settings: Some(board_types::zone::Settings::CopperSettings(
                board_types::CopperZoneSettings {
                    net: MessageField::some(board_types::Net {
                        name: "GND".into(),
                        ..Default::default()
                    }),
                    connection: MessageField::some(board_types::ZoneConnectionSettings {
                        zone_connection: board_types::ZoneConnectionStyle::ZCS_THERMAL.into(),
                        thermal_spokes: MessageField::some(board_types::ThermalSpokeSettings {
                            width: d(500_000),
                            gap: d(500_000),
                            ..Default::default()
                        }),
                        ..Default::default()
                    }),
                    clearance: d(200_000),
                    min_thickness: d(200_000),
                    island_mode: board_types::IslandRemovalMode::IRM_NEVER.into(),
                    fill_mode: board_types::ZoneFillMode::ZFM_SOLID.into(),
                    teardrop: MessageField::some(board_types::ZoneTeardropSettings {
                        type_: board_types::ZoneTeardropType::ZTDT_NONE.into(),
                        ..Default::default()
                    }),
                    ..Default::default()
                },
            )),
            ..Default::default()
        }
    }
    fn any(zone: &board_types::Zone) -> Any {
        Any {
            type_url: ZONE.into(),
            value: zone.write_to_bytes().unwrap(),
            ..Default::default()
        }
    }
    fn with_id(mut zone: board_types::Zone) -> board_types::Zone {
        zone.id = MessageField::some(base_types::KIID {
            value: "zone".into(),
            ..Default::default()
        });
        zone
    }
    #[test]
    fn typed_patch_preserves_unknown_native_fields_and_refuses_noop() {
        let mut before = with_id(fixture());
        before
            .special_fields
            .mut_unknown_fields()
            .add_varint(900, 123);
        let Some(board_types::zone::Settings::CopperSettings(c)) = &mut before.settings else {
            unreachable!()
        };
        c.special_fields.mut_unknown_fields().add_varint(901, 456);
        c.clearance
            .as_mut()
            .unwrap()
            .special_fields
            .mut_unknown_fields()
            .add_varint(902, 789);
        before.filled = true;
        let patch = SolidZonePatch {
            clearance_nm: Some(300_000),
            ..Default::default()
        };
        let payload = SolidZonePayload::patched(&before, &patch).unwrap();
        let mut expected = before.clone();
        expected.filled = false;
        let Some(board_types::zone::Settings::CopperSettings(c)) = &mut expected.settings else {
            unreachable!()
        };
        c.clearance.as_mut().unwrap().value_nm = 300_000;
        assert_eq!(payload.native(), &expected);
        assert!(SolidZonePayload::patched(&before, &SolidZonePatch::default()).is_err());
        assert!(
            SolidZonePayload::patched(
                &before,
                &SolidZonePatch {
                    clearance_nm: Some(200_000),
                    ..Default::default()
                }
            )
            .is_err()
        );
        assert!(
            SolidZonePayload::patched(
                &before,
                &SolidZonePatch {
                    clearance_nm: Some(-1),
                    ..Default::default()
                }
            )
            .is_err()
        );
    }
    #[test]
    fn receipts_require_all_definition_fields_and_classify_only_empty_fill_containers() {
        let request = SolidZonePayload::new(fixture()).unwrap();
        let mut actual = with_id(fixture());
        actual
            .filled_polygons
            .push(board_types::ZoneFilledPolygons {
                layer: actual.layers[0],
                shapes: MessageField::some(base_types::PolySet::default()),
                ..Default::default()
            });
        let accepted = receipt(&any(&actual), &request, true).unwrap();
        assert!(accepted.matches_saved(&actual));
        actual.priority += 1;
        assert!(receipt(&any(&actual), &request, true).is_err());
        actual.priority -= 1;
        actual.filled_polygons[0]
            .special_fields
            .mut_unknown_fields()
            .add_varint(900, 1);
        assert!(receipt(&any(&actual), &request, true).is_err());
        actual.filled_polygons.clear();
        actual
            .special_fields
            .mut_unknown_fields()
            .add_varint(900, 1);
        assert!(receipt(&any(&actual), &request, true).is_err());
        actual = with_id(fixture());
        actual.filled = true;
        assert!(receipt(&any(&actual), &request, true).is_err());
        actual.filled = false;
        actual.filled_polygons = vec![board_types::ZoneFilledPolygons {
            layer: board_types::BoardLayer::BL_B_Cu.into(),
            shapes: MessageField::some(base_types::PolySet::default()),
            ..Default::default()
        }];
        assert!(receipt(&any(&actual), &request, true).is_err());
    }
    #[test]
    fn payload_rejects_non_solid_authoring_and_oversized_geometry() {
        for kind in [
            board_types::ZoneType::ZT_UNKNOWN,
            board_types::ZoneType::ZT_RULE_AREA,
            board_types::ZoneType::ZT_TEARDROP,
        ] {
            let mut zone = fixture();
            zone.type_ = kind.into();
            assert!(SolidZonePayload::new(zone).is_err());
        }
        let mut zone = fixture();
        zone.layers.push(zone.layers[0]);
        assert!(SolidZonePayload::new(zone).is_err());
        let mut zone = fixture();
        zone.layers[0] = board_types::BoardLayer::BL_Edge_Cuts.into();
        assert!(SolidZonePayload::new(zone).is_err());
        let mut zone = fixture();
        zone.outline.as_mut().unwrap().polygons[0]
            .outline
            .as_mut()
            .unwrap()
            .nodes[0]
            .geometry = Some(base_types::poly_line_node::Geometry::Point(
            base_types::Vector2 {
                x_nm: i64::MIN,
                y_nm: 0,
                ..Default::default()
            },
        ));
        assert!(SolidZonePayload::new(zone).is_err());
        let mut zone = fixture();
        zone.locked = base_types::LockedState::LS_UNLOCKED.into();
        assert!(SolidZonePayload::new(zone).is_err());
    }
}
