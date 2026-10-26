//! Resolve complete route anchors against a single immutable board observation.
use crate::{
    EngineError, observed_net_matches, pad_board_position, resolve_routing_endpoint,
    validate_routing_net,
};
use kicad_model::{
    BoardLayer, Coordinate, CopperLayer, CopperRouteIntent, ObservedTrackGeometry, PadKind,
    ProjectSnapshot, RouteTrackEnd, RoutingAnchor, RoutingEndpointRef, StableId,
    StraightTrackIntent, ThroughViaIntent, ViaType,
};
use std::collections::BTreeSet;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CompiledCopperRoute {
    pub anchor_native_ids: [StableId; 2],
    pub tracks: Vec<StraightTrackIntent>,
    pub vias: Vec<ThroughViaIntent>,
}

fn invalid(reason: &'static str) -> EngineError {
    EngineError::InvalidPlan(reason)
}

pub fn pad_accepts_layer(pad: &kicad_model::ObservedPad, layer: CopperLayer) -> bool {
    pad.layers.contains(&layer.board_layer())
        || (pad.kind == PadKind::PlatedThroughHole
            && pad.layers.contains(&BoardLayer::FrontCopper)
            && pad.layers.contains(&BoardLayer::BackCopper)
            && pad
                .drill
                .as_ref()
                .is_some_and(|drill| drill.size.x_nm > 0 && drill.size.y_nm > 0))
}

fn anchor(
    snapshot: &ProjectSnapshot,
    intent: &CopperRouteIntent,
    value: &RoutingAnchor,
    layer: CopperLayer,
) -> Result<(Coordinate, StableId), EngineError> {
    match value {
        RoutingAnchor::Pad {
            pad_id,
            footprint_placement_id,
        } => {
            let endpoint = RoutingEndpointRef {
                pad_id: pad_id.clone(),
                footprint_placement_id: footprint_placement_id.clone(),
            };
            let (placement, pad) = resolve_routing_endpoint(snapshot, &endpoint)?;
            if pad.net.as_ref() != Some(&intent.net) || !pad_accepts_layer(pad, layer) {
                return Err(invalid(
                    "route pad anchor net or copper layer does not match",
                ));
            }
            Ok((pad_board_position(placement, pad)?, pad.source_uuid.clone()))
        }
        RoutingAnchor::TrackEnd { source_uuid, end } => {
            let routing = snapshot
                .native_routing
                .as_ref()
                .ok_or(invalid("route requires native routing observation"))?;
            let track = routing
                .tracks
                .iter()
                .find(|track| track.source_uuid == *source_uuid)
                .ok_or(invalid("route track anchor is missing"))?;
            let ObservedTrackGeometry::Segment { start, end: finish } = track.geometry else {
                return Err(invalid("route track anchor must be a straight segment"));
            };
            if track.layer != layer.board_layer()
                || !track
                    .net
                    .as_ref()
                    .is_some_and(|net| observed_net_matches(&intent.net, net))
            {
                return Err(invalid(
                    "route track anchor net or copper layer does not match",
                ));
            }
            Ok((
                match end {
                    RouteTrackEnd::Start => start,
                    RouteTrackEnd::End => finish,
                },
                source_uuid.clone(),
            ))
        }
        RoutingAnchor::Via { source_uuid } => {
            let routing = snapshot
                .native_routing
                .as_ref()
                .ok_or(invalid("route requires native routing observation"))?;
            let via = routing
                .vias
                .iter()
                .find(|via| via.source_uuid == *source_uuid)
                .ok_or(invalid("route via anchor is missing"))?;
            let drill = via
                .pad_stack
                .primary_drill
                .as_ref()
                .ok_or(invalid("route via anchor has no primary drill"))?;
            if via.via_type != ViaType::Through
                || !matches!(
                    (drill.start_layer, drill.end_layer),
                    (BoardLayer::FrontCopper, BoardLayer::BackCopper)
                        | (BoardLayer::BackCopper, BoardLayer::FrontCopper)
                )
                || !via.pad_stack.layers.contains(&BoardLayer::FrontCopper)
                || !via.pad_stack.layers.contains(&BoardLayer::BackCopper)
                || !via
                    .net
                    .as_ref()
                    .is_some_and(|net| observed_net_matches(&intent.net, net))
            {
                return Err(invalid(
                    "route via anchor must be a matching full-span through via",
                ));
            }
            Ok((via.position, source_uuid.clone()))
        }
    }
}

pub fn compile_copper_route(
    snapshot: &ProjectSnapshot,
    board: &StableId,
    intent: &CopperRouteIntent,
) -> Result<CompiledCopperRoute, EngineError> {
    snapshot
        .validate()
        .map_err(|e| EngineError::Serialization(e.to_string()))?;
    intent
        .validate()
        .map_err(|e| EngineError::Serialization(e.to_string()))?;
    if snapshot.board.document_id.as_ref() != Some(board) {
        return Err(EngineError::ScopeMismatch);
    }
    if snapshot
        .revision()
        .map_err(|e| EngineError::Serialization(e.to_string()))?
        != intent.observed_revision
    {
        return Err(invalid("route observation revision is stale"));
    }
    validate_routing_net(Some(&intent.net), board)?;
    let stackup = snapshot
        .native_stackup
        .as_ref()
        .ok_or(invalid("route requires observed enabled copper layers"))?;
    for leg in &intent.legs {
        leg.layer
            .require_enabled_on(stackup)
            .map_err(|e| EngineError::Serialization(e.to_string()))?;
    }
    let first = &intent.legs[0];
    let last = intent.legs.last().expect("validated nonempty route");
    let (start, start_id) = anchor(snapshot, intent, &intent.start, first.layer)?;
    let (end, end_id) = anchor(snapshot, intent, &intent.end, last.layer)?;
    if start != first.points[0].coordinate()
        || end
            != last
                .points
                .last()
                .expect("validated leg points")
                .coordinate()
    {
        return Err(invalid(
            "route path endpoints do not match observed anchors",
        ));
    }
    let (tracks, vias) = intent
        .primitives()
        .map_err(|e| EngineError::Serialization(e.to_string()))?;
    let mut ids = existing_board_ids(snapshot);
    for id in tracks
        .iter()
        .map(|t| &t.source_uuid)
        .chain(vias.iter().map(|v| &v.source_uuid))
    {
        if !ids.insert(id.clone()) {
            return Err(EngineError::TargetAlreadyExists(id.clone()));
        }
    }
    Ok(CompiledCopperRoute {
        anchor_native_ids: [start_id, end_id],
        tracks,
        vias,
    })
}

fn existing_board_ids(snapshot: &ProjectSnapshot) -> BTreeSet<StableId> {
    snapshot
        .board
        .graphics
        .iter()
        .map(|g| g.graphic_id.clone())
        .chain(snapshot.native_routing.iter().flat_map(|r| {
            r.tracks
                .iter()
                .map(|t| t.source_uuid.clone())
                .chain(r.vias.iter().map(|v| v.source_uuid.clone()))
        }))
        .chain(
            snapshot
                .native_footprints
                .iter()
                .flat_map(|f| f.placements.iter())
                .flat_map(|f| {
                    std::iter::once(f.source_uuid.clone())
                        .chain(f.pads.iter().map(|p| p.source_uuid.clone()))
                }),
        )
        .collect::<BTreeSet<_>>()
}

/// Require unique board object identities across additive operation families.
pub fn validate_additive_board_identities(
    snapshot: &ProjectSnapshot,
    operations: &[crate::Operation],
) -> Result<(), EngineError> {
    use crate::OperationKind;
    let mut ids = existing_board_ids(snapshot);
    for operation in operations {
        let added = match &operation.kind {
            OperationKind::AddCopperRoute { intent, .. } => {
                let (tracks, vias) = intent
                    .primitives()
                    .map_err(|e| EngineError::Serialization(e.to_string()))?;
                tracks
                    .into_iter()
                    .map(|t| t.source_uuid)
                    .chain(vias.into_iter().map(|v| v.source_uuid))
                    .collect::<Vec<_>>()
            }
            OperationKind::AddStraightTrack { intent, .. } => vec![intent.source_uuid.clone()],
            OperationKind::AddThroughVia { intent, .. } => vec![intent.source_uuid.clone()],
            OperationKind::AddBoardGraphicSegments { segments, .. } => {
                segments.iter().map(|s| s.graphic_id.clone()).collect()
            }
            OperationKind::AddBoardOutline {
                top_left,
                bottom_right,
                width_nm,
                ..
            } => crate::expand_board_outline(*top_left, *bottom_right, *width_nm)
                .into_iter()
                .map(|s| s.graphic_id)
                .collect(),
            _ => Vec::new(),
        };
        for id in added {
            if !ids.insert(id.clone()) {
                return Err(EngineError::TargetAlreadyExists(id));
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use kicad_model::*;
    fn fixture() -> (ProjectSnapshot, CopperRouteIntent) {
        let (mut snapshot, a, b, net) = crate::tests::observed_routing_snapshot(BoardSide::Front);
        let board = snapshot.board.document_id.clone().unwrap();
        let layers = [
            ObservedStackupLayerIdentity::FrontCopper,
            ObservedStackupLayerIdentity::InnerCopper(1),
            ObservedStackupLayerIdentity::InnerCopper(2),
            ObservedStackupLayerIdentity::BackCopper,
        ]
        .into_iter()
        .map(|layer| ObservedStackupLayer {
            layer,
            enabled: true,
            layer_type: ObservedStackupLayerType::Copper,
            thickness_nm: 35_000,
            material_name: String::new(),
            user_name: String::new(),
            color: None,
            dielectric: None,
            dielectric_type: None,
            soldermask: None,
            silkscreen_material: None,
        })
        .collect();
        snapshot.native_stackup =
            Some(ObservedBoardStackup::new(board, None, None, None, layers).unwrap());
        let point = |endpoint: &RoutingEndpointRef| {
            let (fp, pad) = resolve_routing_endpoint(&snapshot, endpoint).unwrap();
            let p = pad_board_position(fp, pad).unwrap();
            RoutePoint {
                x_nm: p.x,
                y_nm: p.y,
            }
        };
        let start = point(&a);
        let end = point(&b);
        let v1 = RoutePoint {
            x_nm: start.x_nm,
            y_nm: start.y_nm + 1_000_000,
        };
        let v2 = RoutePoint {
            x_nm: end.x_nm,
            y_nm: end.y_nm + 1_000_000,
        };
        let route = CopperRouteIntent {
            route_id: StableId::new("path").unwrap(),
            observed_revision: snapshot.revision().unwrap(),
            start: RoutingAnchor::Pad {
                pad_id: a.pad_id,
                footprint_placement_id: a.footprint_placement_id,
            },
            end: RoutingAnchor::Pad {
                pad_id: b.pad_id,
                footprint_placement_id: b.footprint_placement_id,
            },
            net,
            legs: vec![
                RouteLeg {
                    layer: CopperLayer::FRONT,
                    width_nm: 200_000,
                    points: vec![start, v1],
                },
                RouteLeg {
                    layer: "In1.Cu".parse().unwrap(),
                    width_nm: 200_000,
                    points: vec![v1, v2],
                },
                RouteLeg {
                    layer: CopperLayer::FRONT,
                    width_nm: 200_000,
                    points: vec![v2, end],
                },
            ],
            vias: vec![
                RouteVia {
                    via_id: StableId::new("via-1").unwrap(),
                    position: v1,
                    pad_diameter_nm: 600_000,
                    drill_diameter_nm: 300_000,
                },
                RouteVia {
                    via_id: StableId::new("via-2").unwrap(),
                    position: v2,
                    pad_diameter_nm: 600_000,
                    drill_diameter_nm: 300_000,
                },
            ],
        };
        (snapshot, route)
    }
    #[test]
    fn additive_identity_namespace_is_shared_by_routes_and_graphics() {
        use crate::{Operation, OperationKind, RiskClass};
        let (snapshot, route) = fixture();
        let board = snapshot.board.document_id.clone().unwrap();
        let digest = snapshot.revision().unwrap();
        let operation = Operation {
            operation_id: StableId::new("route-op").unwrap(),
            kind: OperationKind::AddCopperRoute {
                board: board.clone(),
                intent: route.clone(),
                rule_evidence: digest.clone(),
                native_evidence: digest,
            },
            targets: vec![board.clone()],
            preconditions: vec![],
            expected_effects: vec![],
            risk_class: RiskClass::Medium,
            idempotency_key: "route-op".into(),
            source_run_id: "test".into(),
        };
        assert!(
            validate_additive_board_identities(&snapshot, std::slice::from_ref(&operation)).is_ok()
        );
        assert!(
            validate_additive_board_identities(&snapshot, &[operation.clone(), operation.clone()])
                .is_err()
        );
        let mut graphic = operation.clone();
        graphic.kind = OperationKind::AddBoardGraphicSegments {
            board,
            segments: vec![BoardGraphicSegment {
                graphic_id: route.primitives().unwrap().0[0].source_uuid.clone(),
                start: route.legs[0].points[0].coordinate(),
                end: route.legs[0].points[1].coordinate(),
                width_nm: 100_000,
                layer: BoardGraphicLayer::DwgsUser,
            }],
        };
        for operations in [[operation.clone(), graphic.clone()], [graphic, operation]] {
            assert!(validate_additive_board_identities(&snapshot, &operations).is_err());
        }
    }

    #[test]
    fn route_binding_requires_current_matching_anchors_nets_and_enabled_layers() {
        let (snapshot, route) = fixture();
        let board = snapshot.board.document_id.clone().unwrap();
        let before = snapshot.clone();
        let compiled = compile_copper_route(&snapshot, &board, &route).unwrap();
        assert_eq!(snapshot, before);
        assert_eq!(compiled.tracks.len(), 3);
        assert_eq!(compiled.vias.len(), 2);
        let cases: [fn(&mut CopperRouteIntent); 6] = [
            |r| r.observed_revision = RevisionDigest::new("f".repeat(64)).unwrap(),
            |r| r.legs[1].layer = "In3.Cu".parse().unwrap(),
            |r| r.net.net_id = board_net_id(&StableId::new("other").unwrap(), "GND"),
            |r| r.legs[0].points[0].x_nm += 1,
            |r| {
                r.start = RoutingAnchor::TrackEnd {
                    source_uuid: StableId::new("missing").unwrap(),
                    end: RouteTrackEnd::End,
                }
            },
            |r| {
                r.start = RoutingAnchor::Via {
                    source_uuid: StableId::new("missing").unwrap(),
                }
            },
        ];
        for change in cases {
            let mut invalid = route.clone();
            change(&mut invalid);
            assert!(compile_copper_route(&snapshot, &board, &invalid).is_err());
        }
        let mut wrong_layer = route;
        wrong_layer.legs = vec![RouteLeg {
            layer: CopperLayer::BACK,
            width_nm: 200_000,
            points: vec![
                wrong_layer.legs[0].points[0],
                *wrong_layer.legs[2].points.last().unwrap(),
            ],
        }];
        wrong_layer.vias.clear();
        assert!(compile_copper_route(&snapshot, &board, &wrong_layer).is_err());
    }
}
