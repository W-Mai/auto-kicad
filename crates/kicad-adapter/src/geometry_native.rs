//! Exact conversion from typed solid-zone intent to the pinned native representation.
use kicad_ipc::{
    CopperLayer, CreateNetIdentity, IpcError, SolidZonePatch, SolidZonePayload, base_types,
    board_types,
};
use kicad_model::geometry as model;
use protobuf::MessageField;

fn distance(value_nm: i64) -> MessageField<base_types::Distance> {
    MessageField::some(base_types::Distance {
        value_nm,
        ..Default::default()
    })
}
fn polygon(value: &model::ZonePolygon) -> base_types::PolygonWithHoles {
    let ring = |r: &model::ZoneRing| base_types::PolyLine {
        closed: true,
        nodes: r
            .points
            .iter()
            .map(|p| base_types::PolyLineNode {
                geometry: Some(base_types::poly_line_node::Geometry::Point(
                    base_types::Vector2 {
                        x_nm: p.x_nm,
                        y_nm: p.y_nm,
                        ..Default::default()
                    },
                )),
                ..Default::default()
            })
            .collect(),
        ..Default::default()
    };
    base_types::PolygonWithHoles {
        outline: MessageField::some(ring(&value.exterior)),
        holes: value.holes.iter().map(ring).collect(),
        ..Default::default()
    }
}
fn connection(value: model::ZoneConnection) -> board_types::ZoneConnectionStyle {
    match value {
        model::ZoneConnection::Solid => board_types::ZoneConnectionStyle::ZCS_FULL,
        model::ZoneConnection::Thermal => board_types::ZoneConnectionStyle::ZCS_THERMAL,
        model::ZoneConnection::PthThermal => board_types::ZoneConnectionStyle::ZCS_PTH_THERMAL,
    }
}
fn islands(value: model::IslandRemoval) -> board_types::IslandRemovalMode {
    match value {
        model::IslandRemoval::Always => board_types::IslandRemovalMode::IRM_ALWAYS,
        model::IslandRemoval::Never => board_types::IslandRemovalMode::IRM_NEVER,
        model::IslandRemoval::BelowArea => board_types::IslandRemovalMode::IRM_AREA,
    }
}
fn layers(values: &[kicad_model::CopperLayer]) -> Vec<CopperLayer> {
    let mut layers = values
        .iter()
        .map(|l| crate::native_copper_layer(*l))
        .collect::<Vec<_>>();
    layers.sort_by_key(|l| l.native() as i32);
    layers
}
/// Construct an ordinary solid zone without borrowing another zone's settings. Inactive hatch fields use the exact 10.0.7 saved-reload defaults.
pub fn payload(d: &model::SolidZoneDefinition) -> Result<SolidZonePayload, IpcError> {
    let value = SolidZonePayload::new(board_types::Zone {
        name: d.name.clone(),
        priority: d.priority,
        type_: board_types::ZoneType::ZT_COPPER.into(),
        layers: layers(&d.layers)
            .iter()
            .map(|l| l.native().into())
            .collect(),
        outline: MessageField::some(base_types::PolySet {
            polygons: vec![polygon(&d.polygon)],
            ..Default::default()
        }),
        border: MessageField::some(board_types::ZoneBorderSettings {
            style: board_types::ZoneBorderStyle::ZBS_DIAGONAL_EDGE.into(),
            pitch: distance(500_000),
            ..Default::default()
        }),
        settings: Some(board_types::zone::Settings::CopperSettings(
            board_types::CopperZoneSettings {
                connection: MessageField::some(board_types::ZoneConnectionSettings {
                    zone_connection: connection(d.connection).into(),
                    thermal_spokes: MessageField::some(board_types::ThermalSpokeSettings {
                        width: distance(d.thermal_spoke_width_nm),
                        gap: distance(d.thermal_gap_nm),
                        ..Default::default()
                    }),
                    ..Default::default()
                }),
                clearance: distance(d.clearance_nm),
                min_thickness: distance(d.min_thickness_nm),
                island_mode: islands(d.island_removal).into(),
                min_island_area: d.min_island_area_nm2,
                fill_mode: board_types::ZoneFillMode::ZFM_SOLID.into(),
                corner_smoothing: board_types::ZoneCornerSmoothingMode::ZCSM_NONE.into(),
                hatch_settings: MessageField::some(board_types::HatchFillSettings {
                    thickness: distance(1_000_000),
                    gap: distance(1_500_000),
                    orientation: MessageField::some(base_types::Angle::default()),
                    hatch_smoothing_ratio: 0.1,
                    hatch_hole_min_area_ratio: 0.15,
                    border_mode: board_types::ZoneHatchFillBorderMode::ZHFBM_USE_HATCH_THICKNESS
                        .into(),
                    ..Default::default()
                }),
                net: MessageField::some(board_types::Net {
                    name: d.net.name.clone(),
                    ..Default::default()
                }),
                teardrop: MessageField::some(board_types::ZoneTeardropSettings {
                    type_: board_types::ZoneTeardropType::ZTDT_NONE.into(),
                    ..Default::default()
                }),
                ..Default::default()
            },
        )),
        ..Default::default()
    })?;
    require_saved_area(&value)?;
    Ok(value)
}
pub fn patch(p: &model::SolidZonePatch) -> Result<SolidZonePatch, IpcError> {
    Ok(SolidZonePatch {
        name: p.name.clone(),
        net: p.net.as_ref().map(|n| CreateNetIdentity {
            name: n.name.clone(),
        }),
        layers: p.layers.as_ref().map(|v| layers(v)),
        polygon: p.polygon.as_ref().map(polygon),
        priority: p.priority,
        clearance_nm: p.clearance_nm,
        min_thickness_nm: p.min_thickness_nm,
        connection: p.connection.map(connection),
        thermal_gap_nm: p.thermal_gap_nm,
        thermal_spoke_width_nm: p.thermal_spoke_width_nm,
        island_removal: p.island_removal.map(islands),
        min_island_area_nm2: p.min_island_area_nm2,
    })
}

// Native files omit inactive island thresholds; exact persistence requires the saved default.
pub fn require_saved_area(value: &SolidZonePayload) -> Result<(), IpcError> {
    let Some(board_types::zone::Settings::CopperSettings(c)) = &value.native().settings else {
        unreachable!("validated solid copper payload");
    };
    if c.island_mode.enum_value() != Ok(board_types::IslandRemovalMode::IRM_AREA)
        && c.min_island_area != 10_000_000_000_000
    {
        return Err(IpcError::InvalidObservedBoardGraphic(
            "inactive island area must explicitly equal the KiCad saved default of 10000000000000 nm2",
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn typed_native_definitions_require_exact_persistent_island_settings() {
        let mut stages: serde_json::Value = serde_json::from_str(include_str!(
            "../../../scripts/fixtures/geometry-zone-requests.json"
        ))
        .unwrap();
        stages[0]["observed_revision"] = serde_json::json!("1".repeat(64));
        let request: model::GeometryRequest = serde_json::from_value(stages[0].clone()).unwrap();
        let model::ZoneEdit::Create { mut definition, .. } = request.zones[0].clone() else {
            unreachable!()
        };
        let value = payload(&definition).unwrap();
        for mode in [model::IslandRemoval::Always, model::IslandRemoval::Never] {
            definition.island_removal = mode;
            assert!(payload(&definition).is_ok());
            definition.min_island_area_nm2 = 2_000_000_000_000;
            assert!(payload(&definition).is_err());
            definition.min_island_area_nm2 = 10_000_000_000_000;
        }
        definition.island_removal = model::IslandRemoval::BelowArea;
        definition.min_island_area_nm2 = 2_000_000_000_000;
        let mut native = payload(&definition).unwrap().native().clone();
        native.id = MessageField::some(base_types::KIID {
            value: "zone".into(),
            ..Default::default()
        });
        let mut fields = model::SolidZonePatch {
            island_removal: Some(model::IslandRemoval::Always),
            ..Default::default()
        };
        let resolved = SolidZonePayload::patched(&native, &patch(&fields).unwrap()).unwrap();
        assert!(require_saved_area(&resolved).is_err());
        fields.min_island_area_nm2 = Some(10_000_000_000_000);
        let resolved = SolidZonePayload::patched(&native, &patch(&fields).unwrap()).unwrap();
        assert!(require_saved_area(&resolved).is_ok());
        assert_eq!(resolved.native().outline, value.native().outline);
        assert_eq!(resolved.native().layers, value.native().layers);
    }
}
