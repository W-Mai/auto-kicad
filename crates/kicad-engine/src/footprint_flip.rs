use super::*;

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FootprintFlipAxis {
    TopBottom,
    LeftRight,
}

pub fn expected_footprint_flip(
    snapshot: &ProjectSnapshot,
    operation: &OperationKind,
) -> Result<ProjectSnapshot, EngineError> {
    let OperationKind::FlipExistingFootprint {
        board,
        placement_id,
        source_uuid,
        target_side,
        axis,
        ..
    } = operation
    else {
        return Err(EngineError::UnsupportedOperation);
    };
    if snapshot.board.document_id.as_ref() != Some(board) {
        return Err(EngineError::ScopeMismatch);
    }
    if !connected_footprint_copper(snapshot, placement_id, FOOTPRINT_COPPER_TOLERANCE_NM)?
        .is_empty()
    {
        return Err(EngineError::InvalidPlan("connected_copper"));
    }
    let mut expected = snapshot.clone();
    let footprints = expected
        .native_footprints
        .as_mut()
        .ok_or(EngineError::InvalidPlan("missing footprints"))?;
    let target = footprints
        .placements
        .iter_mut()
        .find(|p| &p.placement_id == placement_id)
        .ok_or_else(|| EngineError::TargetNotFound(placement_id.clone()))?;
    if &target.source_uuid != source_uuid {
        return Err(EngineError::ScopeMismatch);
    }
    if target.side == *target_side {
        return Err(EngineError::InvalidPlan("footprint already on target side"));
    }
    target.side = *target_side;
    let angle = target.orientation_mdeg.rem_euclid(360_000);
    target.orientation_mdeg = match axis {
        FootprintFlipAxis::TopBottom => -angle,
        FootprintFlipAxis::LeftRight => 180_000 - angle,
    }
    .rem_euclid(360_000);
    for pad in &mut target.pads {
        if pad.stack_type != PadStackType::Normal {
            return Err(EngineError::InvalidPlan("unsupported flip pad stack"));
        }
        pad.orientation_mdeg = (-pad.orientation_mdeg.rem_euclid(360_000)).rem_euclid(360_000);
        for layer in &mut pad.layers {
            *layer = match layer {
                BoardLayer::FrontCopper => BoardLayer::BackCopper,
                BoardLayer::BackCopper => BoardLayer::FrontCopper,
                BoardLayer::FrontMask => BoardLayer::BackMask,
                BoardLayer::BackMask => BoardLayer::FrontMask,
                BoardLayer::FrontPaste => BoardLayer::BackPaste,
                BoardLayer::BackPaste => BoardLayer::FrontPaste,
                _ => return Err(EngineError::InvalidPlan("unsupported flip pad layer")),
            };
        }
        for shape in &mut pad.copper_shapes {
            shape.offset.y_nm = shape
                .offset
                .y_nm
                .checked_neg()
                .ok_or(EngineError::InvalidPlan("flip offset overflow"))?;
            shape.trapezoid_delta.y_nm = shape
                .trapezoid_delta
                .y_nm
                .checked_neg()
                .ok_or(EngineError::InvalidPlan("flip delta overflow"))?;
            std::mem::swap(&mut shape.chamfer_top_left, &mut shape.chamfer_bottom_left);
            std::mem::swap(
                &mut shape.chamfer_top_right,
                &mut shape.chamfer_bottom_right,
            );
        }
    }
    *footprints =
        kicad_model::ObservedBoardFootprints::new(board.clone(), footprints.placements.clone())
            .map_err(|e| EngineError::Serialization(e.to_string()))?;
    Ok(expected.canonical())
}
