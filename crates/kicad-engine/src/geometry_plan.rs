//! Validated operation shape for native geometry and additive whole-board refill.
use crate::{EngineError, MAX_OPERATIONS, Operation, OperationKind, geometry::GeometryContext};
use kicad_model::{
    RevisionDigest, StableId,
    geometry::{GeometryRequest, RefillMode},
};

/// A geometry tail can compose only with unchanged additive operation families on one board.
pub struct GeometryPlan<'a> {
    pub board: &'a StableId,
    pub request: &'a GeometryRequest,
    pub context: &'a GeometryContext,
    pub source_evidence: &'a RevisionDigest,
    pub additions: &'a [Operation],
}

pub fn additive_board(kind: &OperationKind) -> Option<&StableId> {
    match kind {
        OperationKind::AddCopperRoute { board, .. }
        | OperationKind::AddStraightTrack { board, .. }
        | OperationKind::AddThroughVia { board, .. }
        | OperationKind::AddBoardGraphicSegments { board, .. }
        | OperationKind::AddBoardOutline { board, .. } => Some(board),
        _ => None,
    }
}

pub fn geometry_plan(operations: &[Operation]) -> Result<GeometryPlan<'_>, EngineError> {
    let invalid = || {
        EngineError::InvalidPlan(
            "geometry requires an exclusive operation or additive operations followed by refill-only",
        )
    };
    let (tail, additions) = operations.split_last().ok_or_else(invalid)?;
    let OperationKind::EditGeometry {
        board,
        request,
        context,
        source_evidence,
    } = &tail.kind
    else {
        return Err(invalid());
    };
    if &context.board_id != board || request.observed_revision != context.revision {
        return Err(EngineError::ScopeMismatch);
    }
    if !additions.is_empty()
        && (additions.len() > MAX_OPERATIONS
            || request.refill != RefillMode::All
            || request.outline.is_some()
            || !request.zones.is_empty()
            || additions
                .iter()
                .any(|operation| additive_board(&operation.kind) != Some(board)))
    {
        return Err(invalid());
    }
    Ok(GeometryPlan {
        board,
        request,
        context,
        source_evidence,
        additions,
    })
}
