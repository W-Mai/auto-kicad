use super::*;

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ItemPointNm {
    pub x: i64,
    pub y: i64,
}

impl ItemPointNm {
    fn apply(self, coordinate: &mut Coordinate) -> Result<(), EngineError> {
        coordinate.x = self.x;
        coordinate.y = self.y;
        validate_routing_position(*coordinate)
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum ExistingItemChange {
    Track {
        target_id: StableId,
        source_uuid: StableId,
        start_nm: Option<ItemPointNm>,
        end_nm: Option<ItemPointNm>,
        width_nm: Option<i64>,
    },
    Via {
        target_id: StableId,
        source_uuid: StableId,
        position_nm: Option<ItemPointNm>,
        diameter_nm: Option<i64>,
        drill_nm: Option<i64>,
    },
    GraphicSegment {
        target_id: StableId,
        source_uuid: StableId,
        start_nm: Option<ItemPointNm>,
        end_nm: Option<ItemPointNm>,
        width_nm: Option<i64>,
    },
}

impl ExistingItemChange {
    pub fn identities(&self) -> (&StableId, &StableId) {
        match self {
            Self::Track {
                target_id,
                source_uuid,
                ..
            }
            | Self::Via {
                target_id,
                source_uuid,
                ..
            }
            | Self::GraphicSegment {
                target_id,
                source_uuid,
                ..
            } => (target_id, source_uuid),
        }
    }

    pub fn in_board_scope(&self, board: &StableId) -> Self {
        let mut result = self.clone();
        match &mut result {
            Self::Track {
                target_id,
                source_uuid,
                ..
            } => *target_id = kicad_model::board_track_id(board, source_uuid),
            Self::Via {
                target_id,
                source_uuid,
                ..
            } => *target_id = kicad_model::board_via_id(board, source_uuid),
            Self::GraphicSegment { .. } => {}
        }
        result
    }
}

fn dimension(value: i64) -> Result<i64, EngineError> {
    if !(1..=MAX_BOARD_POSITION_NM).contains(&value) {
        return Err(EngineError::InvalidPlan(
            "existing item dimension is out of bounds",
        ));
    }
    Ok(value)
}

fn patch_segment(
    start: &mut Coordinate,
    end: &mut Coordinate,
    width: &mut i64,
    start_nm: Option<ItemPointNm>,
    end_nm: Option<ItemPointNm>,
    width_nm: Option<i64>,
) -> Result<(), EngineError> {
    if let Some(value) = start_nm {
        value.apply(start)?;
    }
    if let Some(value) = end_nm {
        value.apply(end)?;
    }
    if let Some(value) = width_nm {
        *width = dimension(value)?;
    }
    if start == end {
        return Err(EngineError::InvalidPlan("segment endpoints coincide"));
    }
    Ok(())
}

pub fn expected_existing_item_edit(
    snapshot: &ProjectSnapshot,
    operation: &OperationKind,
) -> Result<ProjectSnapshot, EngineError> {
    let OperationKind::EditExistingItems { board, changes, .. } = operation else {
        return Err(EngineError::UnsupportedOperation);
    };
    if snapshot.board.document_id.as_ref() != Some(board) {
        return Err(EngineError::ScopeMismatch);
    }
    if changes.is_empty() || changes.len() > MAX_OPERATIONS {
        return Err(EngineError::InvalidPlan(
            "existing item batch is empty or oversized",
        ));
    }
    let mut ids = BTreeSet::new();
    let mut targets = BTreeSet::new();
    let mut result = snapshot.clone();
    for change in changes {
        let (target_id, source_uuid) = change.identities();
        if !ids.insert(source_uuid) || !targets.insert(target_id) {
            return Err(EngineError::InvalidPlan(
                "existing item target is duplicated",
            ));
        }
        match change {
            ExistingItemChange::Track {
                start_nm,
                end_nm,
                width_nm,
                ..
            } => {
                let item = result
                    .native_routing
                    .as_mut()
                    .and_then(|r| r.tracks.iter_mut().find(|t| &t.track_id == target_id))
                    .ok_or_else(|| EngineError::TargetNotFound(target_id.clone()))?;
                if &item.source_uuid != source_uuid
                    || target_id != &kicad_model::board_track_id(board, source_uuid)
                {
                    return Err(EngineError::ScopeMismatch);
                }
                if item.lock_state != LockState::Unlocked {
                    return Err(EngineError::InvalidPlan("existing track is locked"));
                }
                let before = item.clone();
                let ObservedTrackGeometry::Segment { start, end } = &mut item.geometry else {
                    return Err(EngineError::InvalidPlan("arc edits are unsupported"));
                };
                patch_segment(
                    start,
                    end,
                    &mut item.width_nm,
                    *start_nm,
                    *end_nm,
                    *width_nm,
                )?;
                if item == &before {
                    return Err(EngineError::InvalidPlan("existing item edit has no effect"));
                }
            }
            ExistingItemChange::GraphicSegment {
                start_nm,
                end_nm,
                width_nm,
                ..
            } => {
                let item = result
                    .board
                    .graphics
                    .iter_mut()
                    .find(|g| &g.graphic_id == target_id)
                    .ok_or_else(|| EngineError::TargetNotFound(target_id.clone()))?;
                if target_id != source_uuid {
                    return Err(EngineError::ScopeMismatch);
                }
                if item.layer != kicad_model::BoardGraphicLayer::DwgsUser {
                    return Err(EngineError::InvalidPlan(
                        "only Dwgs.User segments can be edited",
                    ));
                }
                let before = item.clone();
                patch_segment(
                    &mut item.start,
                    &mut item.end,
                    &mut item.width_nm,
                    *start_nm,
                    *end_nm,
                    *width_nm,
                )?;
                if item == &before {
                    return Err(EngineError::InvalidPlan("existing item edit has no effect"));
                }
            }
            ExistingItemChange::Via {
                position_nm,
                diameter_nm,
                drill_nm,
                ..
            } => {
                let item = result
                    .native_routing
                    .as_mut()
                    .and_then(|r| r.vias.iter_mut().find(|v| &v.via_id == target_id))
                    .ok_or_else(|| EngineError::TargetNotFound(target_id.clone()))?;
                if &item.source_uuid != source_uuid
                    || target_id != &kicad_model::board_via_id(board, source_uuid)
                {
                    return Err(EngineError::ScopeMismatch);
                }
                if item.lock_state != LockState::Unlocked || item.via_type != ViaType::Through {
                    return Err(EngineError::InvalidPlan("via must be unlocked and through"));
                }
                let before = item.clone();
                let stack = &mut item.pad_stack;
                if !matches!(
                    stack.stack_type,
                    PadStackType::Normal | PadStackType::FrontInnerBack
                ) || stack.copper_shapes.is_empty()
                {
                    return Err(EngineError::InvalidPlan("via stack is not uniform"));
                }
                let size = stack.copper_shapes[0].size;
                if stack.copper_shapes.iter().any(|s| {
                    s.shape != PadShape::Circle
                        || s.size != size
                        || s.offset.x_nm != 0
                        || s.offset.y_nm != 0
                }) || size.x_nm != size.y_nm
                {
                    return Err(EngineError::InvalidPlan(
                        "via copper must be uniform concentric circles",
                    ));
                }
                let drill = stack
                    .primary_drill
                    .as_mut()
                    .ok_or(EngineError::InvalidPlan("missing via drill"))?;
                if drill.start_layer != BoardLayer::FrontCopper
                    || drill.end_layer != BoardLayer::BackCopper
                    || drill.shape != DrillShape::Circle
                    || drill.size.x_nm != drill.size.y_nm
                {
                    return Err(EngineError::InvalidPlan(
                        "via drill must be circular and through",
                    ));
                }
                let diameter = dimension(diameter_nm.unwrap_or(size.x_nm))?;
                let hole = dimension(drill_nm.unwrap_or(drill.size.x_nm))?;
                if hole >= diameter {
                    return Err(EngineError::InvalidPlan(
                        "via drill must be smaller than copper",
                    ));
                }
                if diameter_nm.is_some() {
                    for shape in &mut stack.copper_shapes {
                        shape.size = SizeNm {
                            x_nm: diameter,
                            y_nm: diameter,
                        };
                    }
                }
                if drill_nm.is_some() {
                    drill.size = SizeNm {
                        x_nm: hole,
                        y_nm: hole,
                    };
                }
                if let Some(value) = position_nm {
                    value.apply(&mut item.position)?;
                }
                if item == &before {
                    return Err(EngineError::InvalidPlan("existing item edit has no effect"));
                }
            }
        }
    }
    if let Some(routing) = result.native_routing.take() {
        result.native_routing = Some(
            ObservedBoardRouting::new(board.clone(), routing.tracks, routing.vias)
                .map_err(|e| EngineError::Serialization(e.to_string()))?,
        );
    }
    result
        .validate()
        .map_err(|e| EngineError::Serialization(e.to_string()))?;
    Ok(result.canonical())
}

/// Every previously connected pair of surviving pads must share an after cluster.
pub fn preserves_pad_connectivity(before: &[Vec<String>], after: &[Vec<String>]) -> bool {
    fn partition(clusters: &[Vec<String>]) -> Option<BTreeMap<&str, usize>> {
        let mut result = BTreeMap::new();
        for (index, cluster) in clusters.iter().enumerate() {
            if cluster.is_empty() {
                return None;
            }
            for pad in cluster {
                if pad.is_empty() || result.insert(pad.as_str(), index).is_some() {
                    return None;
                }
            }
        }
        Some(result)
    }
    let Some(original) = partition(before) else {
        return false;
    };
    let Some(actual) = partition(after) else {
        return false;
    };
    if actual.keys().any(|pad| !original.contains_key(pad)) {
        return false;
    }
    before.iter().all(|cluster| {
        let groups = cluster
            .iter()
            .filter_map(|pad| actual.get(pad.as_str()))
            .collect::<BTreeSet<_>>();
        groups.len() <= 1
    })
}
