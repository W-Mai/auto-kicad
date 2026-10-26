//! Bounded authored geometry summaries; complete native evidence remains in the observation.
use crate::geometry::GeometryObservation;
use crate::live_geometry::RefillDefinitionEvidence;
use kicad_ipc::{SolidZonePayload, base_types as base, board_types as native};
use kicad_model::{OutlinePoint, geometry::*};
use protobuf::Message;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

fn digest(message: &impl Message) -> Result<String, String> {
    Ok(format!(
        "sha256:{}",
        hex::encode(Sha256::digest(
            message.write_to_bytes().map_err(|e| e.to_string())?
        ))
    ))
}
fn point(p: Option<&base::Vector2>) -> Result<OutlinePoint, String> {
    let p = p.ok_or("native point is missing")?;
    Ok(OutlinePoint {
        x_nm: p.x_nm,
        y_nm: p.y_nm,
    })
}
fn ring(r: &base::PolyLine) -> Result<ZoneRing, String> {
    if !r.closed || r.nodes.len() > 512 {
        return Err("polygon ring is open or exceeds authoring bound".into());
    }
    Ok(ZoneRing {
        points: r
            .nodes
            .iter()
            .map(|n| match &n.geometry {
                Some(base::poly_line_node::Geometry::Point(p)) => point(Some(p)),
                _ => Err("curved or unknown polygon nodes are not editable".into()),
            })
            .collect::<Result<_, _>>()?,
    })
}
fn polygon(p: &base::PolySet) -> Result<ZonePolygon, String> {
    if p.polygons.len() != 1 {
        return Err("multiple native polygons are not editable".into());
    }
    let p = &p.polygons[0];
    if p.holes.len() > 16 {
        return Err("polygon holes exceed authoring bound".into());
    }
    Ok(ZonePolygon {
        exterior: ring(p.outline.as_ref().ok_or("missing exterior")?)?,
        holes: p.holes.iter().map(ring).collect::<Result<_, _>>()?,
    })
}
fn layer(
    value: &protobuf::EnumOrUnknown<native::BoardLayer>,
) -> Result<kicad_model::CopperLayer, String> {
    let native = value
        .enum_value()
        .map_err(|_| "unknown native copper layer")?;
    let candidates = [
        kicad_model::CopperLayer::FRONT,
        kicad_model::CopperLayer::BACK,
    ]
    .into_iter()
    .chain((1..=30).map(|i| {
        kicad_model::CopperLayer::try_from(kicad_model::BoardLayer::InnerCopper(i))
            .expect("bounded inner layer")
    }));
    candidates
        .into_iter()
        .find(|l| crate::native_copper_layer(*l).native() == native)
        .ok_or_else(|| "unsupported copper layer".into())
}
fn definition(
    zone: &native::Zone,
    net: Option<&ZoneNetIdentity>,
) -> Result<SolidZoneDefinition, String> {
    let mut unfilled = zone.clone();
    unfilled.filled = false;
    unfilled.filled_polygons.clear();
    SolidZonePayload::new(unfilled).map_err(|e| e.to_string())?;
    let Some(native::zone::Settings::CopperSettings(c)) = &zone.settings else {
        return Err("zone is not ordinary copper".into());
    };
    let connection = c.connection.as_ref().ok_or("missing connection")?;
    let thermal = connection
        .thermal_spokes
        .as_ref()
        .ok_or("missing thermal settings")?;
    let distance = |v: &protobuf::MessageField<base::Distance>| {
        v.as_ref()
            .map(|d| d.value_nm)
            .ok_or_else(|| "missing distance".to_string())
    };
    Ok(SolidZoneDefinition {
        name: zone.name.clone(),
        net: net.cloned().ok_or("missing zone net")?,
        layers: zone.layers.iter().map(layer).collect::<Result<_, _>>()?,
        polygon: polygon(zone.outline.as_ref().ok_or("missing zone polygon")?)?,
        priority: zone.priority,
        clearance_nm: distance(&c.clearance)?,
        min_thickness_nm: distance(&c.min_thickness)?,
        connection: match connection.zone_connection.enum_value() {
            Ok(native::ZoneConnectionStyle::ZCS_FULL) => ZoneConnection::Solid,
            Ok(native::ZoneConnectionStyle::ZCS_THERMAL) => ZoneConnection::Thermal,
            Ok(native::ZoneConnectionStyle::ZCS_PTH_THERMAL) => ZoneConnection::PthThermal,
            _ => return Err("unsupported connection style".into()),
        },
        thermal_gap_nm: distance(&thermal.gap)?,
        thermal_spoke_width_nm: distance(&thermal.width)?,
        island_removal: match c.island_mode.enum_value() {
            Ok(native::IslandRemovalMode::IRM_ALWAYS) => IslandRemoval::Always,
            Ok(native::IslandRemovalMode::IRM_NEVER) => IslandRemoval::Never,
            Ok(native::IslandRemovalMode::IRM_AREA) => IslandRemoval::BelowArea,
            _ => return Err("unsupported island mode".into()),
        },
        min_island_area_nm2: c.min_island_area,
    })
}
fn outline(shape: &native::BoardGraphicShape) -> Result<Value, String> {
    use base::graphic_shape::Geometry;
    let s = shape.shape.as_ref().ok_or("missing native shape")?;
    Ok(match s.geometry.as_ref().ok_or("missing geometry")? {
        Geometry::Segment(g) => {
            json!({"kind":"segment","start":point(g.start.as_ref())?,"end":point(g.end.as_ref())?})
        }
        Geometry::Arc(g) => {
            json!({"kind":"arc","start":point(g.start.as_ref())?,"mid":point(g.mid.as_ref())?,"end":point(g.end.as_ref())?})
        }
        Geometry::Circle(g) => {
            json!({"kind":"circle","center":point(g.center.as_ref())?,"radius_point":point(g.radius_point.as_ref())?})
        }
        Geometry::Rectangle(g) => {
            json!({"kind":"rectangle","top_left":point(g.top_left.as_ref())?,"bottom_right":point(g.bottom_right.as_ref())?,"corner_radius_nm":g.corner_radius.as_ref().map(|r|r.value_nm)})
        }
        Geometry::Polygon(g) => json!({"kind":"polygon","polygon":polygon(g)?}),
        Geometry::Bezier(_) => return Err("Bezier outline authoring is unsupported".into()),
        _ => return Err("unknown outline geometry".into()),
    })
}

impl GeometryObservation {
    /// Describe all observed zones and board-level Edge.Cuts. Unsupported forms retain identity and full-definition digests; no fill vertices are exposed.
    pub fn summary(&self) -> Result<Value, String> {
        let mut zones = Vec::new();
        for target in &self.context().zones {
            let id = target.source_uuid.as_str();
            let zone = &self.live().native.zones()[id];
            let (definition, reason) = match definition(zone, target.net.as_ref()) {
                Ok(value) => (Some(value), None),
                Err(reason) => (None, Some(reason)),
            };
            let mut native_definition = zone.clone();
            native_definition.filled = false;
            native_definition.filled_polygons.clear();
            native_definition.parent = None.into();
            zones.push(json!({"source_uuid":id,"name":zone.name,"native_type":zone.type_.value(),"lock_state":target.lock_state,"editable_solid":target.editable_solid,"refillable":target.refillable,"net":target.net,"copper_layers":zone.layers.iter().map(layer).collect::<Result<Vec<_>,_>>().ok(),"native_layers":zone.layers.iter().map(|l|l.value()).collect::<Vec<_>>(),"definition":definition,"unsupported_reason":reason,"definition_digest":digest(&native_definition)?,"filled":zone.filled,"fill_polygon_count":zone.filled_polygons.iter().filter_map(|p|p.shapes.as_ref()).map(|p|p.polygons.len()).sum::<usize>()}));
        }
        let mut outlines = Vec::new();
        for target in &self.context().outline_targets {
            let shape = &self.live().native.shapes()[target.source_uuid.as_str()];
            let (geometry, reason) = match outline(shape) {
                Ok(g) => (Some(g), None),
                Err(e) => (None, Some(e)),
            };
            let mut persistent = shape.clone();
            persistent.parent = None.into();
            let width = shape
                .shape
                .as_ref()
                .and_then(|s| s.attributes.as_ref())
                .and_then(|a| a.stroke.as_ref())
                .and_then(|s| s.width.as_ref())
                .map(|w| w.value_nm);
            outlines.push(json!({"source_uuid":target.source_uuid,"lock_state":target.lock_state,"geometry":geometry,"unsupported_reason":reason,"width_nm":width,"definition_digest":digest(&persistent)?}));
        }
        let summary = json!({"board_id":self.context().board_id,"observed_revision":self.context().revision.to_string(),"enabled_copper_layers":self.context().enabled_layers,"nets":self.context().nets,"zones":zones,"outlines":outlines,"definitions":RefillDefinitionEvidence::from_observation(self.live())?,"copper":self.live().text.filled_copper()?});
        if serde_json::to_vec(&summary)
            .map_err(|e| e.to_string())?
            .len()
            > 2 * 1024 * 1024
        {
            return Err("geometry summary exceeds 2 MiB; no partial observation returned".into());
        }
        Ok(summary)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn summary_retains_authored_intent_and_fingerprints_unknown_fields() {
        let stages: Value = serde_json::from_str(include_str!(
            "../../../scripts/fixtures/geometry-zone-requests.json"
        ))
        .unwrap();
        let intent: SolidZoneDefinition =
            serde_json::from_value(stages[0]["zones"][0]["definition"].clone()).unwrap();
        let payload = crate::geometry_native::payload(&intent).unwrap();
        let native = payload.native();
        assert_eq!(definition(native, Some(&intent.net)).unwrap(), intent);
        let original = digest(native).unwrap();
        let mut extra = native.clone();
        extra
            .special_fields
            .mut_unknown_fields()
            .add_varint(900, 123);
        assert_eq!(definition(&extra, Some(&intent.net)).unwrap(), intent);
        assert_ne!(digest(&extra).unwrap(), original);
        if let Some(native::zone::Settings::CopperSettings(c)) = &mut extra.settings {
            c.fill_mode = native::ZoneFillMode::ZFM_HATCHED.into();
        }
        assert!(definition(&extra, Some(&intent.net)).is_err());
        assert!(definition(native, None).is_err());
        for i in 1..=30 {
            let native = kicad_ipc::CopperLayer::inner(i).unwrap().native().into();
            assert_eq!(layer(&native).unwrap().to_string(), format!("In{i}.Cu"));
        }
        assert!(layer(&native::BoardLayer::BL_Edge_Cuts.into()).is_err());
    }
}
