use super::*;

pub const FOOTPRINT_COPPER_TOLERANCE_NM: i64 = 250_000;

fn unsafe_copper() -> EngineError {
    EngineError::InvalidPlan("unsupported_copper_interaction")
}

fn copper_radius(shapes: &[PadCopperShape]) -> Result<f64, EngineError> {
    if shapes.is_empty() {
        return Err(unsafe_copper());
    }
    shapes.iter().try_fold(0.0_f64, |radius, shape| {
        if matches!(shape.shape, PadShape::Unknown | PadShape::Custom)
            || shape.size.x_nm < 0
            || shape.size.y_nm < 0
            || [
                shape.size.x_nm,
                shape.size.y_nm,
                shape.offset.x_nm,
                shape.offset.y_nm,
                shape.trapezoid_delta.x_nm,
                shape.trapezoid_delta.y_nm,
            ]
            .iter()
            .any(|n| n.unsigned_abs() > MAX_BOARD_POSITION_NM as u64)
        {
            return Err(unsafe_copper());
        }
        // A disk around the pad origin encloses every rotated corner and copper offset.
        let delta = shape.trapezoid_delta.x_nm.unsigned_abs() as f64
            + shape.trapezoid_delta.y_nm.unsigned_abs() as f64;
        let half_x = (shape.size.x_nm as f64 + delta) / 2.0;
        let half_y = (shape.size.y_nm as f64 + delta) / 2.0;
        let extent = if shape.shape == PadShape::Circle {
            half_x.max(half_y)
        } else {
            half_x.hypot(half_y)
        };
        Ok(radius.max(extent + (shape.offset.x_nm as f64).hypot(shape.offset.y_nm as f64)))
    })
}

fn segment_distance(p: (f64, f64), a: Coordinate, b: Coordinate) -> f64 {
    let dx = (b.x as f64) - a.x as f64;
    let dy = (b.y as f64) - a.y as f64;
    let length = dx * dx + dy * dy;
    let t = if length == 0.0 {
        0.0
    } else {
        (((p.0 - a.x as f64) * dx + (p.1 - a.y as f64) * dy) / length).clamp(0.0, 1.0)
    };
    (p.0 - (a.x as f64 + t * dx)).hypot(p.1 - (a.y as f64 + t * dy))
}

fn arc_distance(
    p: (f64, f64),
    start: Coordinate,
    mid: Coordinate,
    end: Coordinate,
) -> Result<f64, EngineError> {
    let bx = mid.x as f64 - start.x as f64;
    let by = mid.y as f64 - start.y as f64;
    let cx = end.x as f64 - start.x as f64;
    let cy = end.y as f64 - start.y as f64;
    let scale = bx.abs().max(by.abs()).max(cx.abs()).max(cy.abs());
    let determinant = 2.0 * (bx * cy - by * cx);
    if determinant.abs() <= 1e-12 * scale * scale {
        return Err(unsafe_copper());
    }
    let b2 = bx * bx + by * by;
    let c2 = cx * cx + cy * cy;
    let ox = (cy * b2 - by * c2) / determinant + start.x as f64;
    let oy = (bx * c2 - cx * b2) / determinant + start.y as f64;
    let radius = (start.x as f64 - ox).hypot(start.y as f64 - oy);
    let angle = |x: f64, y: f64| (y - oy).atan2(x - ox);
    let a = angle(start.x as f64, start.y as f64);
    let m = angle(mid.x as f64, mid.y as f64);
    let b = angle(end.x as f64, end.y as f64);
    let q = angle(p.0, p.1);
    let ccw = |from: f64, to: f64| (to - from).rem_euclid(std::f64::consts::TAU);
    let on_arc = if ccw(a, m) <= ccw(a, b) {
        ccw(a, q) <= ccw(a, b)
    } else {
        ccw(b, q) <= ccw(b, a)
    };
    let endpoints = (p.0 - start.x as f64)
        .hypot(p.1 - start.y as f64)
        .min((p.0 - end.x as f64).hypot(p.1 - end.y as f64));
    let distance = if on_arc {
        ((p.0 - ox).hypot(p.1 - oy) - radius).abs().min(endpoints)
    } else {
        endpoints
    };
    if !distance.is_finite() {
        return Err(unsafe_copper());
    }
    Ok(distance)
}

fn layer_rank(layer: &BoardLayer) -> Result<u32, EngineError> {
    match layer {
        BoardLayer::FrontCopper => Ok(0),
        BoardLayer::InnerCopper(n) => Ok(u32::from(*n)),
        BoardLayer::BackCopper => Ok(u32::MAX),
        _ => Err(unsafe_copper()),
    }
}

pub fn connected_footprint_copper(
    snapshot: &ProjectSnapshot,
    placement_id: &StableId,
    tolerance_nm: i64,
) -> Result<Vec<StableId>, EngineError> {
    if !(1..=FOOTPRINT_COPPER_TOLERANCE_NM).contains(&tolerance_nm) {
        return Err(EngineError::InvalidPlan("invalid copper tolerance"));
    }
    let footprints = snapshot
        .native_footprints
        .as_ref()
        .ok_or_else(unsafe_copper)?;
    footprints.validate().map_err(|_| unsafe_copper())?;
    let target = footprints
        .placements
        .iter()
        .find(|p| &p.placement_id == placement_id)
        .ok_or_else(|| EngineError::TargetNotFound(placement_id.clone()))?;
    match target.lock_state {
        LockState::Unlocked => {}
        LockState::Locked => return Err(EngineError::InvalidPlan("target_locked")),
        LockState::Unknown => return Err(unsafe_copper()),
    }
    let routing = snapshot.native_routing.as_ref().ok_or_else(unsafe_copper)?;
    routing.validate().map_err(|_| unsafe_copper())?;
    let mut affected = BTreeSet::new();
    for pad in &target.pads {
        let layers = pad
            .layers
            .iter()
            .filter(|l| {
                matches!(
                    l,
                    BoardLayer::FrontCopper | BoardLayer::BackCopper | BoardLayer::InnerCopper(_)
                )
            })
            .collect::<Vec<_>>();
        if layers.is_empty() {
            if pad.kind == kicad_model::PadKind::NonPlatedThroughHole {
                continue;
            }
            return Err(unsafe_copper());
        }
        if pad.stack_type == PadStackType::Unknown || pad.kind == kicad_model::PadKind::Unknown {
            return Err(unsafe_copper());
        }
        let position = pad_board_position(target, pad)?;
        let p = (position.x as f64, position.y as f64);
        let radius = copper_radius(&pad.copper_shapes)? + tolerance_nm as f64;
        for track in &routing.tracks {
            if !layers.contains(&&track.layer) {
                continue;
            }
            // Physical contact is sufficient regardless of net assignment.
            let distance = match track.geometry {
                ObservedTrackGeometry::Segment { start, end } => segment_distance(p, start, end),
                ObservedTrackGeometry::Arc { start, mid, end } => arc_distance(p, start, mid, end)?,
            };
            if distance <= radius + track.width_nm as f64 / 2.0 {
                affected.insert(track.source_uuid.clone());
            }
        }
        for via in &routing.vias {
            if via.via_type == ViaType::Unknown || via.pad_stack.stack_type == PadStackType::Unknown
            {
                return Err(unsafe_copper());
            }
            let common_layer = if via.via_type == ViaType::Through {
                true
            } else {
                let drill = via
                    .pad_stack
                    .primary_drill
                    .as_ref()
                    .ok_or_else(unsafe_copper)?;
                let start = layer_rank(&drill.start_layer)?;
                let end = layer_rank(&drill.end_layer)?;
                layers.iter().try_fold(false, |hit, layer| {
                    Ok::<_, EngineError>(
                        hit || (start.min(end)..=start.max(end)).contains(&layer_rank(layer)?),
                    )
                })?
            };
            if !common_layer {
                continue;
            }
            let mut via_radius = copper_radius(&via.pad_stack.copper_shapes)?;
            for drill in [
                &via.pad_stack.primary_drill,
                &via.pad_stack.secondary_drill,
                &via.pad_stack.tertiary_drill,
            ]
            .into_iter()
            .flatten()
            {
                via_radius =
                    via_radius.max((drill.size.x_nm as f64).hypot(drill.size.y_nm as f64) / 2.0);
            }
            if (via.position.x as f64 - p.0).hypot(via.position.y as f64 - p.1)
                <= radius + via_radius
            {
                affected.insert(via.source_uuid.clone());
            }
        }
    }
    Ok(affected.into_iter().collect())
}

pub fn expected_footprint_move(
    snapshot: &ProjectSnapshot,
    operation: &OperationKind,
) -> Result<ProjectSnapshot, EngineError> {
    let OperationKind::MoveExistingFootprint {
        board,
        placement_id,
        source_uuid,
        position,
        orientation_mdeg,
        ..
    } = operation
    else {
        return Err(EngineError::UnsupportedOperation);
    };
    if snapshot.board.document_id.as_ref() != Some(board) {
        return Err(EngineError::ScopeMismatch);
    }
    validate_routing_position(*position)?;
    if !(0..MAX_FOOTPRINT_ORIENTATION_MDEG).contains(orientation_mdeg) {
        return Err(EngineError::InvalidPlan(
            "footprint orientation is out of bounds",
        ));
    }
    let mut expected = snapshot.clone();
    let footprints = expected
        .native_footprints
        .as_mut()
        .ok_or_else(unsafe_copper)?;
    let target = footprints
        .placements
        .iter_mut()
        .find(|p| &p.placement_id == placement_id)
        .ok_or_else(|| EngineError::TargetNotFound(placement_id.clone()))?;
    if &target.source_uuid != source_uuid {
        return Err(EngineError::ScopeMismatch);
    }
    target.position = *position;
    target.orientation_mdeg = *orientation_mdeg;
    *footprints =
        kicad_model::ObservedBoardFootprints::new(board.clone(), footprints.placements.clone())
            .map_err(|e| EngineError::Serialization(e.to_string()))?;
    Ok(expected.canonical())
}
