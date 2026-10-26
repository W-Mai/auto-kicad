use super::*;

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExistingItemKind {
    Track,
    Via,
    GraphicSegment,
    Footprint,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExistingItemTarget {
    pub kind: ExistingItemKind,
    pub target_id: StableId,
    pub source_uuid: StableId,
}

impl ExistingItemTarget {
    pub fn in_board_scope(&self, board: &StableId) -> Self {
        let mut result = self.clone();
        result.target_id = match self.kind {
            ExistingItemKind::Track => kicad_model::board_track_id(board, &self.source_uuid),
            ExistingItemKind::Via => kicad_model::board_via_id(board, &self.source_uuid),
            ExistingItemKind::Footprint => {
                kicad_model::footprint_placement_id(board, &self.source_uuid)
            }
            ExistingItemKind::GraphicSegment => self.source_uuid.clone(),
        };
        result
    }
}

pub fn expected_item_deletion(
    snapshot: &ProjectSnapshot,
    operation: &OperationKind,
) -> Result<ProjectSnapshot, EngineError> {
    let OperationKind::DeleteExistingItems { board, targets, .. } = operation else {
        return Err(EngineError::UnsupportedOperation);
    };
    if snapshot.board.document_id.as_ref() != Some(board) {
        return Err(EngineError::ScopeMismatch);
    }
    if targets.is_empty() || targets.len() > MAX_OPERATIONS {
        return Err(EngineError::InvalidPlan(
            "deletion requires 1 to 100 explicit targets",
        ));
    }
    let mut ids = BTreeSet::new();
    let mut observed_ids = BTreeSet::new();
    let mut result = snapshot.clone();
    for target in targets {
        if !ids.insert(&target.source_uuid) || !observed_ids.insert(&target.target_id) {
            return Err(EngineError::InvalidPlan("duplicate deletion target"));
        }
        if target.in_board_scope(board) != *target {
            return Err(EngineError::ScopeMismatch);
        }
        let missing = || EngineError::TargetNotFound(target.target_id.clone());
        match target.kind {
            ExistingItemKind::Track => {
                let routing = result.native_routing.as_mut().ok_or_else(missing)?;
                let index = routing
                    .tracks
                    .iter()
                    .position(|v| {
                        v.track_id == target.target_id && v.source_uuid == target.source_uuid
                    })
                    .ok_or_else(missing)?;
                if routing.tracks[index].lock_state != LockState::Unlocked {
                    return Err(EngineError::InvalidPlan("deletion target is locked"));
                }
                routing.tracks.remove(index);
            }
            ExistingItemKind::Via => {
                let routing = result.native_routing.as_mut().ok_or_else(missing)?;
                let index = routing
                    .vias
                    .iter()
                    .position(|v| {
                        v.via_id == target.target_id && v.source_uuid == target.source_uuid
                    })
                    .ok_or_else(missing)?;
                let via = &routing.vias[index];
                if via.lock_state != LockState::Unlocked || via.via_type != ViaType::Through {
                    return Err(EngineError::InvalidPlan(
                        "deletion via must be unlocked and through",
                    ));
                }
                routing.vias.remove(index);
            }
            ExistingItemKind::GraphicSegment => {
                let index = result
                    .board
                    .graphics
                    .iter()
                    .position(|v| v.graphic_id == target.target_id)
                    .ok_or_else(missing)?;
                if result.board.graphics[index].layer != kicad_model::BoardGraphicLayer::DwgsUser {
                    return Err(EngineError::InvalidPlan(
                        "only Dwgs.User segment deletion is supported",
                    ));
                }
                result.board.graphics.remove(index);
            }
            ExistingItemKind::Footprint => {
                // Check the original board: deleting a connecting track in the same batch must not bypass policy.
                if !connected_footprint_copper(
                    snapshot,
                    &target.target_id,
                    FOOTPRINT_COPPER_TOLERANCE_NM,
                )?
                .is_empty()
                {
                    return Err(EngineError::InvalidPlan("connected_copper"));
                }
                let footprints = result.native_footprints.as_mut().ok_or_else(missing)?;
                let index = footprints
                    .placements
                    .iter()
                    .position(|v| {
                        v.placement_id == target.target_id && v.source_uuid == target.source_uuid
                    })
                    .ok_or_else(missing)?;
                if !footprints.placements[index].symbol_sheet_path.is_empty() {
                    return Err(EngineError::InvalidPlan(
                        "schematic-associated footprint deletion is unsupported",
                    ));
                }
                footprints.placements.remove(index);
            }
        }
    }
    if let Some(routing) = result.native_routing.as_mut() {
        *routing =
            ObservedBoardRouting::new(board.clone(), routing.tracks.clone(), routing.vias.clone())
                .map_err(|e| EngineError::Serialization(e.to_string()))?;
    }
    if let Some(footprints) = result.native_footprints.as_mut() {
        *footprints =
            kicad_model::ObservedBoardFootprints::new(board.clone(), footprints.placements.clone())
                .map_err(|e| EngineError::Serialization(e.to_string()))?;
    }
    result
        .validate()
        .map_err(|e| EngineError::Serialization(e.to_string()))?;
    Ok(result.canonical())
}
