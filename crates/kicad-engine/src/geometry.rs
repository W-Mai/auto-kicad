//! Compile explicit geometry changes against complete observed identity, locks, nets and layers.
use crate::{
    EngineError,
    outline::{CompiledOutlineEdit, ObservedOutlineTarget, compile_outline_edit},
};
use kicad_model::{CopperLayer, LockState, RevisionDigest, StableId, board_net_id, geometry::*};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ObservedZoneTarget {
    pub source_uuid: StableId,
    pub lock_state: LockState,
    pub editable_solid: bool,
    pub refillable: bool,
    pub net: Option<ZoneNetIdentity>,
}
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct GeometryContext {
    pub board_id: StableId,
    pub revision: RevisionDigest,
    pub nets: Vec<ZoneNetIdentity>,
    pub enabled_layers: Vec<CopperLayer>,
    pub zones: Vec<ObservedZoneTarget>,
    pub outline_targets: Vec<ObservedOutlineTarget>,
}
/// Immutable structural compilation. Native intent, topology, refill, electrical checks and approval are mandatory separate gates.
#[derive(Clone, Debug)]
pub struct CompiledGeometryChange {
    request: GeometryRequest,
    outline: Option<CompiledOutlineEdit>,
    refill_zone_ids: Vec<StableId>,
    affected_nets: Vec<ZoneNetIdentity>,
}
impl CompiledGeometryChange {
    pub fn request(&self) -> &GeometryRequest {
        &self.request
    }
    pub fn outline(&self) -> Option<&CompiledOutlineEdit> {
        self.outline.as_ref()
    }
    pub fn refill_zone_ids(&self) -> &[StableId] {
        &self.refill_zone_ids
    }
    pub fn affected_nets(&self) -> &[ZoneNetIdentity] {
        &self.affected_nets
    }
}
fn invalid(reason: &'static str) -> EngineError {
    EngineError::InvalidPlan(reason)
}
fn identifier(id: &StableId) -> Result<(), EngineError> {
    if id.as_str().trim().is_empty() || id.as_str().len() > 128 || id.as_str().contains('\0') {
        return Err(invalid("geometry identity must be nonempty and bounded"));
    }
    Ok(())
}
fn name(value: &str) -> Result<(), EngineError> {
    if value.len() > 512 || value.contains('\0') {
        return Err(invalid("zone name exceeds bound"));
    }
    Ok(())
}
fn distance(value: i64, positive: bool) -> Result<(), EngineError> {
    if !(i64::from(positive)..=kicad_geometry::MAX_COORDINATE_NM).contains(&value) {
        return Err(invalid("zone dimension is outside bounds"));
    }
    Ok(())
}
fn polygon(p: &ZonePolygon) -> Result<(), EngineError> {
    let ring = |r: &ZoneRing| {
        r.points
            .iter()
            .map(|p| (p.x_nm, p.y_nm))
            .collect::<Vec<_>>()
    };
    kicad_geometry::polygon_bounds(
        &ring(&p.exterior),
        &p.holes.iter().map(ring).collect::<Vec<_>>(),
    )
    .map_err(|e| invalid(e.message()))?;
    Ok(())
}
fn layers(values: &[CopperLayer], enabled: &BTreeSet<CopperLayer>) -> Result<(), EngineError> {
    if values.is_empty()
        || values.len() > 32
        || values.iter().collect::<BTreeSet<_>>().len() != values.len()
        || values.iter().any(|v| !enabled.contains(v))
    {
        return Err(invalid("zone layers must be unique enabled copper layers"));
    }
    Ok(())
}
fn area(value: u64) -> Result<(), EngineError> {
    if value > i64::MAX as u64 {
        return Err(invalid("zone island area exceeds native bound"));
    }
    Ok(())
}
fn net(value: &ZoneNetIdentity, context: &GeometryContext) -> Result<(), EngineError> {
    if value.name.is_empty()
        || value.name.len() > 4096
        || value.name.contains('\0')
        || value.net_id != board_net_id(&context.board_id, &value.name)
        || context.nets.iter().filter(|n| *n == value).count() != 1
    {
        return Err(invalid(
            "zone net must match one observed board-scoped identity and name",
        ));
    }
    Ok(())
}
fn definition(
    d: &SolidZoneDefinition,
    context: &GeometryContext,
    enabled: &BTreeSet<CopperLayer>,
) -> Result<(), EngineError> {
    name(&d.name)?;
    net(&d.net, context)?;
    layers(&d.layers, enabled)?;
    polygon(&d.polygon)?;
    distance(d.clearance_nm, false)?;
    distance(d.min_thickness_nm, true)?;
    distance(d.thermal_gap_nm, true)?;
    distance(d.thermal_spoke_width_nm, true)?;
    area(d.min_island_area_nm2)
}
fn patch(
    p: &SolidZonePatch,
    context: &GeometryContext,
    enabled: &BTreeSet<CopperLayer>,
) -> Result<(), EngineError> {
    if p == &SolidZonePatch::default() {
        return Err(invalid("zone patch is empty"));
    }
    if let Some(v) = &p.name {
        name(v)?;
    }
    if let Some(v) = &p.net {
        net(v, context)?;
    }
    if let Some(v) = &p.layers {
        layers(v, enabled)?;
    }
    if let Some(v) = &p.polygon {
        polygon(v)?;
    }
    if let Some(v) = p.clearance_nm {
        distance(v, false)?;
    }
    for v in [
        p.min_thickness_nm,
        p.thermal_gap_nm,
        p.thermal_spoke_width_nm,
    ]
    .into_iter()
    .flatten()
    {
        distance(v, true)?;
    }
    if let Some(v) = p.min_island_area_nm2 {
        area(v)?;
    }
    Ok(())
}

pub fn compile_geometry_change(
    request: &GeometryRequest,
    context: &GeometryContext,
) -> Result<CompiledGeometryChange, EngineError> {
    if request.observed_revision != context.revision {
        return Err(invalid("geometry observation revision changed"));
    }
    if request.zones.len() > 32 || context.zones.len() > 256 || context.nets.len() > 100_000 {
        return Err(invalid("geometry request or observation exceeds bounds"));
    }
    let enabled = context
        .enabled_layers
        .iter()
        .copied()
        .collect::<BTreeSet<_>>();
    if enabled.len() < 2 || enabled.len() > 32 || enabled.len() != context.enabled_layers.len() {
        return Err(invalid(
            "enabled copper layer observation is incomplete or ambiguous",
        ));
    }
    let mut observed_nets = BTreeSet::new();
    for n in &context.nets {
        if n.name.is_empty()
            || n.name.len() > 4096
            || n.name.contains('\0')
            || n.net_id != board_net_id(&context.board_id, &n.name)
        {
            return Err(invalid("observed net identity is invalid"));
        }
        if !observed_nets.insert(&n.name) {
            return Err(invalid("observed net names repeat"));
        }
    }
    let mut sources = BTreeMap::new();
    let mut affected = BTreeMap::new();
    for z in &context.zones {
        identifier(&z.source_uuid)?;
        if sources.insert(&z.source_uuid, z).is_some() {
            return Err(invalid("observed zone identities repeat"));
        }
        if let Some(n) = &z.net {
            net(n, context)?;
            affected.insert(n.net_id.clone(), n.clone());
        }
    }
    if request.refill == RefillMode::None
        && (!request.zones.is_empty()
            || request.outline.is_none()
            || context.zones.iter().any(|z| z.refillable))
    {
        return Err(invalid(
            "zone edits and zoned outline edits require whole-board refill",
        ));
    }
    let outline = request
        .outline
        .as_ref()
        .map(|o| {
            compile_outline_edit(
                o,
                &request.observed_revision,
                &context.revision,
                &context.outline_targets,
            )
        })
        .transpose()?;
    let mut targets = BTreeSet::new();
    let mut created = BTreeSet::new();
    let mut deleted = BTreeSet::new();
    for change in &request.zones {
        match change {
            ZoneEdit::Create {
                zone_id,
                definition: d,
            } => {
                identifier(zone_id)?;
                if !created.insert(zone_id) || sources.contains_key(zone_id) {
                    return Err(invalid("zone creation identity repeats or collides"));
                }
                definition(d, context, &enabled)?;
                affected.insert(d.net.net_id.clone(), d.net.clone());
            }
            ZoneEdit::Update {
                source_uuid,
                patch: p,
            } => {
                identifier(source_uuid)?;
                if !targets.insert(source_uuid) {
                    return Err(invalid("zone mutation targets repeat"));
                }
                let source = sources
                    .get(source_uuid)
                    .ok_or(invalid("zone update target is missing"))?;
                if source.lock_state != LockState::Unlocked || !source.editable_solid {
                    return Err(invalid(
                        "zone update requires observed unlocked ordinary solid copper",
                    ));
                }
                patch(p, context, &enabled)?;
                if let Some(n) = &p.net {
                    affected.insert(n.net_id.clone(), n.clone());
                }
            }
            ZoneEdit::Delete { source_uuid } => {
                identifier(source_uuid)?;
                if !targets.insert(source_uuid) {
                    return Err(invalid("zone mutation targets repeat"));
                }
                let source = sources
                    .get(source_uuid)
                    .ok_or(invalid("zone deletion target is missing"))?;
                if source.lock_state != LockState::Unlocked || !source.editable_solid {
                    return Err(invalid(
                        "zone deletion requires observed unlocked ordinary solid copper",
                    ));
                }
                deleted.insert(source_uuid);
            }
        }
    }
    if context.zones.len() + created.len() - deleted.len() > 256 {
        return Err(invalid("resulting zones exceed observation bound"));
    }
    let refill_zone_ids = if request.refill == RefillMode::All {
        context
            .zones
            .iter()
            .filter(|z| z.refillable && !deleted.contains(&z.source_uuid))
            .map(|z| z.source_uuid.clone())
            .collect()
    } else {
        vec![]
    };
    Ok(CompiledGeometryChange {
        request: request.clone(),
        outline,
        refill_zone_ids,
        affected_nets: affected.into_values().collect(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use kicad_model::{OutlineEdge, OutlineEdit, OutlineLoop, OutlinePoint};
    fn context() -> GeometryContext {
        let board_id = StableId::new("board").unwrap();
        let net = ZoneNetIdentity {
            net_id: board_net_id(&board_id, "GND"),
            name: "GND".into(),
        };
        GeometryContext {
            board_id,
            revision: RevisionDigest::new("1".repeat(64)).unwrap(),
            nets: vec![net.clone()],
            enabled_layers: vec![CopperLayer::FRONT, CopperLayer::BACK],
            zones: vec![ObservedZoneTarget {
                source_uuid: StableId::new("source").unwrap(),
                lock_state: LockState::Unlocked,
                editable_solid: true,
                refillable: true,
                net: Some(net),
            }],
            outline_targets: vec![],
        }
    }
    fn request(c: &GeometryContext) -> GeometryRequest {
        let points = [
            (0, 0),
            (10_000_000, 0),
            (10_000_000, 10_000_000),
            (0, 10_000_000),
        ]
        .into_iter()
        .map(|(x_nm, y_nm)| OutlinePoint { x_nm, y_nm })
        .collect();
        GeometryRequest {
            observed_revision: c.revision.clone(),
            outline: None,
            refill: RefillMode::All,
            zones: vec![ZoneEdit::Create {
                zone_id: StableId::new("new").unwrap(),
                definition: SolidZoneDefinition {
                    name: "solid".into(),
                    net: c.nets[0].clone(),
                    layers: vec![CopperLayer::FRONT],
                    polygon: ZonePolygon {
                        exterior: ZoneRing { points },
                        holes: vec![],
                    },
                    priority: 1,
                    clearance_nm: 0,
                    min_thickness_nm: 200_000,
                    connection: ZoneConnection::Thermal,
                    thermal_gap_nm: 500_000,
                    thermal_spoke_width_nm: 500_000,
                    island_removal: IslandRemoval::BelowArea,
                    min_island_area_nm2: 10_000_000_000_000,
                },
            }],
        }
    }
    #[test]
    fn geometry_compiler_binds_revision_identity_layers_targets_and_refill_scope() {
        let c = context();
        let r = request(&c);
        let compiled = compile_geometry_change(&r, &c).unwrap();
        assert_eq!(
            compiled.refill_zone_ids(),
            &[c.zones[0].source_uuid.clone()]
        );
        assert_eq!(compiled.affected_nets(), &c.nets);
        for mode in 0..8 {
            let mut bad = r.clone();
            let ZoneEdit::Create { definition: d, .. } = &mut bad.zones[0] else {
                unreachable!()
            };
            match mode {
                0 => bad.observed_revision = RevisionDigest::new("2".repeat(64)).unwrap(),
                1 => d.net.net_id = board_net_id(&StableId::new("other-board").unwrap(), "GND"),
                2 => d.net.name = "not GND".into(),
                3 => d.layers = vec!["In1.Cu".parse().unwrap()],
                4 => d.layers.push(CopperLayer::FRONT),
                5 => d.polygon.holes.push(d.polygon.exterior.clone()),
                6 => d.thermal_gap_nm = 0,
                _ => bad.refill = RefillMode::None,
            }
            assert!(compile_geometry_change(&bad, &c).is_err(), "{mode}");
        }
        let mut edit = r.clone();
        edit.zones = vec![ZoneEdit::Update {
            source_uuid: c.zones[0].source_uuid.clone(),
            patch: SolidZonePatch {
                name: Some("changed".into()),
                ..Default::default()
            },
        }];
        assert!(compile_geometry_change(&edit, &c).is_ok());
        for lock in [LockState::Unknown, LockState::Locked] {
            let mut c = c.clone();
            c.zones[0].lock_state = lock;
            assert!(compile_geometry_change(&edit, &c).is_err());
        }
        let mut unsupported = c.clone();
        unsupported.zones[0].editable_solid = false;
        assert!(compile_geometry_change(&edit, &unsupported).is_err());
        edit.zones.push(ZoneEdit::Delete {
            source_uuid: c.zones[0].source_uuid.clone(),
        });
        assert!(compile_geometry_change(&edit, &c).is_err());
        edit.zones = vec![ZoneEdit::Delete {
            source_uuid: c.zones[0].source_uuid.clone(),
        }];
        assert!(
            compile_geometry_change(&edit, &c)
                .unwrap()
                .refill_zone_ids()
                .is_empty()
        );
        edit.zones.clear();
        assert!(compile_geometry_change(&edit, &c).is_ok());
        edit.refill = RefillMode::None;
        assert!(compile_geometry_change(&edit, &c).is_err());
    }
    #[test]
    fn geometry_json_bounds_and_unknown_fields_cannot_bypass_compilation() {
        let c = context();
        let r = request(&c);
        let json = serde_json::to_string(&r).unwrap();
        assert_eq!(GeometryRequest::from_json(&json).unwrap(), r);
        let v = serde_json::to_value(&r).unwrap();
        for mode in 0..5 {
            let mut v = v.clone();
            match mode {
                0 => v["zones"] = serde_json::json!(vec![v["zones"][0].clone(); 33]),
                1 => v["zones"][0]["definition"]["layers"] = serde_json::json!(vec!["F.Cu"; 33]),
                2 => {
                    v["zones"][0]["definition"]["polygon"]["exterior"]["points"] =
                        serde_json::json!(vec![serde_json::json!({"x_nm":0,"y_nm":0}); 513])
                }
                3 => v["zones"][0]["definition"]["net"]["numeric_code"] = serde_json::json!(1),
                _ => v["arbitrary_action"] = serde_json::json!("fill"),
            }
            assert!(
                serde_json::from_value::<GeometryRequest>(v).is_err(),
                "{mode}"
            );
        }
        let mut many = r.clone();
        many.zones = (0..32)
            .map(|i| {
                let mut z = r.zones[0].clone();
                let ZoneEdit::Create { zone_id, .. } = &mut z else {
                    unreachable!()
                };
                *zone_id = StableId::new(format!("new-{i}")).unwrap();
                z
            })
            .collect();
        assert!(compile_geometry_change(&many, &c).is_ok());
        many.zones.push(r.zones[0].clone());
        assert!(compile_geometry_change(&many, &c).is_err());
        let mut outline = request(&c);
        outline.zones.clear();
        outline.refill = RefillMode::None;
        let p = |x, y| OutlinePoint { x_nm: x, y_nm: y };
        outline.outline = Some(OutlineEdit {
            remove_ids: vec![],
            loops: vec![OutlineLoop {
                loop_id: StableId::new("edge").unwrap(),
                width_nm: 50_000,
                edges: vec![
                    OutlineEdge::Segment {
                        start: p(0, 0),
                        end: p(10, 0),
                    },
                    OutlineEdge::Segment {
                        start: p(10, 0),
                        end: p(10, 10),
                    },
                    OutlineEdge::Segment {
                        start: p(10, 10),
                        end: p(0, 10),
                    },
                    OutlineEdge::Segment {
                        start: p(0, 10),
                        end: p(0, 0),
                    },
                ],
            }],
        });
        assert!(compile_geometry_change(&outline, &c).is_err());
        let mut empty = c.clone();
        empty.zones.clear();
        assert!(
            compile_geometry_change(&outline, &empty)
                .unwrap()
                .outline()
                .is_some()
        );
    }
}
