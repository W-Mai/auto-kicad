pub mod copper_route;
pub mod outline;
pub use copper_route::{CompiledCopperRoute, compile_copper_route};
mod item_deletion;
pub use item_deletion::{ExistingItemKind, ExistingItemTarget, expected_item_deletion};
mod footprint_flip;
pub use footprint_flip::{FootprintFlipAxis, expected_footprint_flip};
mod existing_item_edit;
pub use existing_item_edit::{
    ExistingItemChange, ItemPointNm, expected_existing_item_edit, preserves_pad_connectivity,
};
mod footprint_edit;
pub use footprint_edit::{
    FOOTPRINT_COPPER_TOLERANCE_NM, connected_footprint_copper, expected_footprint_move,
};

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::{Display, Formatter};

use kicad_model::{
    AllowlistedFootprintPlacement, BoardGraphicSegment, BoardLayer, Coordinate, DocumentSpace,
    DrillShape, ExactAngleDegrees, FiniteRatio, Footprint, LocalCoordinateNm, LockState,
    ObservedBoardRouting, ObservedFootprintPlacement, ObservedPad, ObservedTrack,
    ObservedTrackGeometry, ObservedVia, ObservedViaDrill, ObservedViaPadStack, PadCopperShape,
    PadShape, PadStackType, ProjectSnapshot, RevisionDigest, RoutingEndpointRef, SizeNm, StableId,
    StraightTrackIntent, Symbol, ThroughViaIntent, UnconnectedLayerRemoval, ViaDrillCappingMode,
    ViaDrillFillingMode, ViaType, board_net_id,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

pub const MAX_OPERATIONS: usize = 100;
pub const MAX_GRAPHIC_SPAN_NM: i128 = 10_000_000;
pub const MAX_GRAPHIC_BOUNDING_AREA_NM2: i128 = 100_000_000_000_000;
pub const MAX_BOARD_POSITION_NM: i64 = 2_000_000_000;
pub const MAX_FOOTPRINT_ORIENTATION_MDEG: i64 = 360_000;

/// A logical pad in an assembly, resolved to a native pad only after creation.
#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
pub struct PlannedPadRef {
    pub target_id: StableId,
    pub pad_number: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct PlannedPadConnection {
    pub track_id: StableId,
    pub start: PlannedPadRef,
    pub end: PlannedPadRef,
    pub width_nm: i64,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ConnectedFootprintAssembly {
    pub placements: Vec<AllowlistedFootprintPlacement>,
    pub connections: Vec<PlannedPadConnection>,
}

impl ConnectedFootprintAssembly {
    pub fn validate(&self) -> Result<(), EngineError> {
        let invalid = EngineError::InvalidPlan;
        if self.placements.is_empty()
            || self.connections.is_empty()
            || self.placements.len() + self.connections.len() > MAX_OPERATIONS
        {
            return Err(invalid("assembly is empty or oversized"));
        }
        let mut targets = BTreeMap::new();
        let mut references = BTreeSet::new();
        let mut symbols = BTreeSet::new();
        let mut revision = None;
        for placement in &self.placements {
            placement
                .validate()
                .map_err(|_| invalid("invalid assembly placement"))?;
            validate_routing_position(placement.position)?;
            let source = placement
                .source
                .as_ref()
                .ok_or(invalid("assembly source is missing"))?;
            if targets.insert(&placement.target_id, placement).is_some()
                || !references.insert(&placement.reference)
                || !symbols.insert(&source.symbol_path)
            {
                return Err(invalid("assembly placement identity is duplicated"));
            }
            if revision
                .replace(&source.schematic)
                .is_some_and(|previous| previous != &source.schematic)
            {
                return Err(invalid("assembly schematic sources differ"));
            }
        }
        let net = |endpoint: &PlannedPadRef| -> Result<&str, EngineError> {
            targets
                .get(&endpoint.target_id)
                .and_then(|p| p.source.as_ref())
                .and_then(|source| source.pad_nets.get(&endpoint.pad_number))
                .and_then(|net| net.as_deref())
                .filter(|net| !net.is_empty())
                .ok_or(invalid("assembly endpoint is missing or has no net"))
        };
        let mut tracks = BTreeSet::new();
        let mut connections = BTreeSet::new();
        for connection in &self.connections {
            let mut endpoints = [&connection.start, &connection.end];
            endpoints.sort();
            if connection.start == connection.end
                || connection.width_nm <= 0
                || connection.width_nm > MAX_BOARD_POSITION_NM
                || targets.contains_key(&connection.track_id)
                || !tracks.insert(&connection.track_id)
                || !connections.insert(endpoints)
                || net(&connection.start)? != net(&connection.end)?
            {
                return Err(invalid(
                    "assembly connection identity, width or net is invalid",
                ));
            }
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "arguments")]
pub enum OperationKind {
    AddSymbol {
        symbol: Symbol,
    },
    SetProperty {
        target: StableId,
        property: String,
        value: String,
    },
    ConnectPins {
        source: StableId,
        target: StableId,
    },
    AssignFootprint {
        symbol: StableId,
        footprint: String,
    },
    PlaceFootprint {
        target: StableId,
        position: Coordinate,
    },
    PlaceAllowlistedFootprint {
        board: StableId,
        placement: AllowlistedFootprintPlacement,
    },
    PlaceConnectedFootprints {
        board: StableId,
        assembly: ConnectedFootprintAssembly,
    },
    AddBoardGraphicSegments {
        board: StableId,
        segments: Vec<BoardGraphicSegment>,
    },
    AddBoardOutline {
        board: StableId,
        top_left: Coordinate,
        bottom_right: Coordinate,
        width_nm: i64,
    },
    AddCopperRoute {
        board: StableId,
        intent: kicad_model::CopperRouteIntent,
        rule_evidence: RevisionDigest,
        native_evidence: RevisionDigest,
    },
    AddStraightTrack {
        board: StableId,
        intent: StraightTrackIntent,
    },
    AddThroughVia {
        board: StableId,
        intent: ThroughViaIntent,
    },
    DeleteExistingItems {
        board: StableId,
        targets: Vec<ExistingItemTarget>,
        native_evidence: RevisionDigest,
    },
    EditExistingItems {
        board: StableId,
        changes: Vec<ExistingItemChange>,
        native_evidence: RevisionDigest,
    },
    FlipExistingFootprint {
        board: StableId,
        placement_id: StableId,
        source_uuid: StableId,
        target_side: kicad_model::BoardSide,
        axis: FootprintFlipAxis,
        native_evidence: RevisionDigest,
    },
    MoveExistingFootprint {
        board: StableId,
        placement_id: StableId,
        source_uuid: StableId,
        position: Coordinate,
        orientation_mdeg: i64,
        #[serde(default = "default_footprint_copper_tolerance")]
        copper_tolerance_nm: i64,
        native_evidence: RevisionDigest,
    },
}

pub fn default_footprint_copper_tolerance() -> i64 {
    FOOTPRINT_COPPER_TOLERANCE_NM
}

impl OperationKind {
    pub fn is_existing_item_mutation(&self) -> bool {
        matches!(
            self,
            Self::EditExistingItems { .. }
                | Self::FlipExistingFootprint { .. }
                | Self::DeleteExistingItems { .. }
        )
    }

    pub fn footprint_placements(&self) -> &[AllowlistedFootprintPlacement] {
        match self {
            Self::PlaceAllowlistedFootprint { placement, .. } => std::slice::from_ref(placement),
            Self::PlaceConnectedFootprints { assembly, .. } => &assembly.placements,
            _ => &[],
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum RiskClass {
    Low,
    Medium,
    High,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum Precondition {
    TargetExists(StableId),
    TargetAbsent(StableId),
    Revision(RevisionDigest),
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct Operation {
    pub operation_id: StableId,
    pub kind: OperationKind,
    pub targets: Vec<StableId>,
    pub preconditions: Vec<Precondition>,
    pub expected_effects: Vec<String>,
    pub risk_class: RiskClass,
    pub idempotency_key: String,
    pub source_run_id: String,
}

impl Operation {
    fn refs(&self) -> Vec<&StableId> {
        match &self.kind {
            OperationKind::AddSymbol { symbol } => vec![&symbol.source_uuid],
            OperationKind::SetProperty { target, .. } => vec![target],
            OperationKind::ConnectPins { source, target } => vec![source, target],
            OperationKind::AssignFootprint { symbol, .. } => vec![symbol],
            OperationKind::PlaceFootprint { target, .. } => vec![target],
            OperationKind::PlaceAllowlistedFootprint { board, placement } => {
                vec![board, &placement.target_id]
            }
            OperationKind::PlaceConnectedFootprints { board, assembly } => std::iter::once(board)
                .chain(assembly.placements.iter().map(|p| &p.target_id))
                .collect(),
            OperationKind::AddBoardGraphicSegments { board, .. }
            | OperationKind::AddBoardOutline { board, .. }
            | OperationKind::AddCopperRoute { board, .. }
            | OperationKind::AddStraightTrack { board, .. }
            | OperationKind::AddThroughVia { board, .. } => vec![board],
            OperationKind::DeleteExistingItems { board, targets, .. } => std::iter::once(board)
                .chain(targets.iter().flat_map(|t| [&t.target_id, &t.source_uuid]))
                .collect(),
            OperationKind::EditExistingItems { board, changes, .. } => std::iter::once(board)
                .chain(changes.iter().flat_map(|c| {
                    let (target, source) = c.identities();
                    [target, source]
                }))
                .collect(),
            OperationKind::FlipExistingFootprint {
                board,
                placement_id,
                source_uuid,
                ..
            }
            | OperationKind::MoveExistingFootprint {
                board,
                placement_id,
                source_uuid,
                ..
            } => vec![board, placement_id, source_uuid],
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ChangePlan {
    pub plan_id: StableId,
    pub source_run_id: String,
    pub base_revision: RevisionDigest,
    pub instance_token: String,
    pub operations: Vec<Operation>,
}

impl ChangePlan {
    pub fn hash(&self) -> Result<String, EngineError> {
        let bytes =
            serde_json::to_vec(self).map_err(|e| EngineError::Serialization(e.to_string()))?;
        let mut h = Sha256::new();
        h.update(bytes);
        Ok(hex::encode(h.finalize()))
    }

    pub fn validate(&self, snapshot: &ProjectSnapshot) -> Result<(), EngineError> {
        if self.base_revision
            != snapshot
                .revision()
                .map_err(|error| EngineError::Serialization(error.to_string()))?
        {
            return Err(EngineError::RevisionStale);
        }
        if self.operations.is_empty() {
            return Err(EngineError::InvalidPlan("operation list must not be empty"));
        }
        if self.operations.len() > MAX_OPERATIONS {
            return Err(EngineError::OperationLimitExceeded);
        }
        if self.source_run_id.is_empty() || self.instance_token.is_empty() {
            return Err(EngineError::InvalidPlan("identity is required"));
        }
        let mut ids = BTreeSet::new();
        let mut keys = BTreeSet::new();
        for operation in &self.operations {
            if !ids.insert(operation.operation_id.clone()) {
                return Err(EngineError::DuplicateOperationId);
            }
            if operation.idempotency_key.is_empty()
                || !keys.insert(operation.idempotency_key.clone())
            {
                return Err(EngineError::DuplicateIdempotencyKey);
            }
        }
        let has_route = self
            .operations
            .iter()
            .any(|op| matches!(op.kind, OperationKind::AddCopperRoute { .. }));
        if has_route
            && self.operations.iter().any(|op| {
                !matches!(
                    op.kind,
                    OperationKind::AddCopperRoute { .. }
                        | OperationKind::AddStraightTrack { .. }
                        | OperationKind::AddThroughVia { .. }
                        | OperationKind::AddBoardGraphicSegments { .. }
                        | OperationKind::AddBoardOutline { .. }
                )
            })
        {
            return Err(EngineError::InvalidPlan(
                "route batches may only add board copper and graphics",
            ));
        }
        let mut copper_count = 0;
        for op in &self.operations {
            copper_count += match &op.kind {
                OperationKind::AddCopperRoute { intent, .. } => {
                    intent
                        .validate()
                        .map_err(|e| EngineError::Serialization(e.to_string()))?;
                    intent
                        .legs
                        .iter()
                        .map(|leg| leg.points.len() - 1)
                        .sum::<usize>()
                        + intent.vias.len()
                }
                OperationKind::AddStraightTrack { .. } | OperationKind::AddThroughVia { .. } => 1,
                _ => 0,
            };
            if copper_count > kicad_model::copper_route::MAX_BATCH_COPPER {
                return Err(EngineError::InvalidPlan("batch copper count exceeds 256"));
            }
        }
        let existing: BTreeSet<StableId> = snapshot
            .schematic
            .symbols
            .iter()
            .map(|s| s.source_uuid.clone())
            .collect();
        let existing_graphics: BTreeSet<StableId> = snapshot
            .board
            .graphics
            .iter()
            .map(|graphic| graphic.graphic_id.clone())
            .collect();
        let mut routing_ids: BTreeSet<StableId> = snapshot
            .native_routing
            .as_ref()
            .into_iter()
            .flat_map(|routing| {
                routing
                    .tracks
                    .iter()
                    .map(|track| track.source_uuid.clone())
                    .chain(routing.vias.iter().map(|via| via.source_uuid.clone()))
            })
            .collect();
        let mut footprint_targets: BTreeSet<StableId> = snapshot
            .board
            .footprints
            .iter()
            .map(|footprint| footprint.source_uuid.clone())
            .collect();
        if let Some(native_footprints) = &snapshot.native_footprints {
            footprint_targets.extend(native_footprints.placements.iter().flat_map(|placement| {
                [
                    placement.source_uuid.clone(),
                    placement.placement_id.clone(),
                ]
            }));
        }
        for operation in &self.operations {
            for placement in operation.kind.footprint_placements() {
                if !footprint_targets.insert(placement.target_id.clone()) {
                    return Err(EngineError::TargetAlreadyExists(
                        placement.target_id.clone(),
                    ));
                }
            }
        }
        let mut planned_adds = BTreeMap::new();
        for (index, operation) in self.operations.iter().enumerate() {
            if let OperationKind::AddSymbol { symbol } = &operation.kind
                && (existing.contains(&symbol.source_uuid)
                    || planned_adds
                        .insert(symbol.source_uuid.clone(), index)
                        .is_some())
            {
                return Err(EngineError::TargetAlreadyExists(symbol.source_uuid.clone()));
            }
        }
        let mut known = existing.clone();
        if let Some(board) = &snapshot.board.document_id {
            known.insert(board.clone());
        }
        for (index, op) in self.operations.iter().enumerate() {
            if op.source_run_id != self.source_run_id {
                return Err(EngineError::InvalidPlan("operation run identity mismatch"));
            }
            if op.targets.is_empty() || op.refs().iter().any(|target| !op.targets.contains(target))
            {
                return Err(EngineError::ScopeMismatch);
            }
            for precondition in &op.preconditions {
                match precondition {
                    Precondition::TargetExists(target) if !known.contains(target) => {
                        return Err(EngineError::PreconditionFailed);
                    }
                    Precondition::TargetAbsent(target) if known.contains(target) => {
                        return Err(EngineError::PreconditionFailed);
                    }
                    Precondition::Revision(revision) if revision != &self.base_revision => {
                        return Err(EngineError::PreconditionFailed);
                    }
                    _ => {}
                }
            }
            if let OperationKind::AddSymbol { symbol } = &op.kind {
                known.insert(symbol.source_uuid.clone());
            } else if !matches!(
                op.kind,
                OperationKind::PlaceAllowlistedFootprint { .. }
                    | OperationKind::PlaceConnectedFootprints { .. }
                    | OperationKind::AddBoardGraphicSegments { .. }
                    | OperationKind::AddBoardOutline { .. }
                    | OperationKind::AddCopperRoute { .. }
                    | OperationKind::AddStraightTrack { .. }
                    | OperationKind::AddThroughVia { .. }
                    | OperationKind::MoveExistingFootprint { .. }
                    | OperationKind::EditExistingItems { .. }
                    | OperationKind::DeleteExistingItems { .. }
                    | OperationKind::FlipExistingFootprint { .. }
            ) {
                for target in op.refs() {
                    if !known.contains(target) {
                        if planned_adds
                            .get(target)
                            .is_some_and(|add_index| *add_index > index)
                        {
                            return Err(EngineError::InvalidOrder);
                        }
                        return Err(EngineError::TargetNotFound(target.clone()));
                    }
                }
            }
            match &op.kind {
                OperationKind::AddSymbol { .. } => {}
                OperationKind::SetProperty { property, .. } => {
                    if property.is_empty() {
                        return Err(EngineError::InvalidPlan("property is empty"));
                    }
                }
                OperationKind::ConnectPins { .. } => {}
                OperationKind::AssignFootprint { footprint, .. } => {
                    if footprint.is_empty() {
                        return Err(EngineError::InvalidPlan("footprint is empty"));
                    }
                }
                OperationKind::PlaceFootprint { position, target } => {
                    if position.space != DocumentSpace::Board {
                        return Err(EngineError::InvalidPlan(
                            "footprint position must use board space",
                        ));
                    }
                    if self.operations.iter().enumerate().any(|(candidate_index, candidate)| {
                        candidate_index > index
                            && matches!(&candidate.kind, OperationKind::AssignFootprint { symbol, .. } if symbol == target)
                    }) {
                        return Err(EngineError::InvalidOrder);
                    }
                }
                OperationKind::PlaceAllowlistedFootprint { board, placement } => {
                    if snapshot.board.document_id.as_ref() != Some(board) {
                        return Err(EngineError::ScopeMismatch);
                    }
                    placement.validate().map_err(|_| {
                        EngineError::InvalidPlan("invalid allowlisted footprint placement")
                    })?;
                    if placement.position.x.unsigned_abs() > MAX_BOARD_POSITION_NM as u64
                        || placement.position.y.unsigned_abs() > MAX_BOARD_POSITION_NM as u64
                    {
                        return Err(EngineError::InvalidPlan(
                            "footprint position bounds exceeded",
                        ));
                    }
                }
                OperationKind::PlaceConnectedFootprints { board, assembly } => {
                    if snapshot.board.document_id.as_ref() != Some(board) {
                        return Err(EngineError::ScopeMismatch);
                    }
                    assembly.validate()?;
                    for placement in &assembly.placements {
                        if snapshot
                            .board
                            .footprints
                            .iter()
                            .any(|p| p.reference.as_ref() == Some(&placement.reference))
                            || snapshot
                                .native_footprints
                                .iter()
                                .flat_map(|f| &f.placements)
                                .any(|p| {
                                    p.reference == placement.reference
                                        || placement.source.as_ref().is_some_and(|source| {
                                            p.symbol_sheet_path == source.symbol_path
                                        })
                                })
                        {
                            return Err(EngineError::TargetAlreadyExists(
                                placement.target_id.clone(),
                            ));
                        }
                    }
                    for connection in &assembly.connections {
                        if !routing_ids.insert(connection.track_id.clone()) {
                            return Err(EngineError::TargetAlreadyExists(
                                connection.track_id.clone(),
                            ));
                        }
                    }
                }
                OperationKind::AddBoardGraphicSegments { board, segments } => {
                    if snapshot.board.document_id.as_ref() != Some(board) {
                        return Err(EngineError::ScopeMismatch);
                    }
                    let mut ids = BTreeSet::new();
                    for segment in segments {
                        if existing_graphics.contains(&segment.graphic_id)
                            || !ids.insert(segment.graphic_id.clone())
                        {
                            return Err(EngineError::TargetAlreadyExists(
                                segment.graphic_id.clone(),
                            ));
                        }
                    }
                    validate_graphics(segments)?;
                }
                OperationKind::AddBoardOutline {
                    board,
                    top_left,
                    bottom_right,
                    width_nm,
                } => {
                    if snapshot.board.document_id.as_ref() != Some(board) {
                        return Err(EngineError::ScopeMismatch);
                    }
                    validate_outline(*top_left, *bottom_right, *width_nm)?;
                    if let Some(segment) = expand_board_outline(*top_left, *bottom_right, *width_nm)
                        .iter()
                        .find(|segment| existing_graphics.contains(&segment.graphic_id))
                    {
                        return Err(EngineError::TargetAlreadyExists(segment.graphic_id.clone()));
                    }
                }
                OperationKind::AddCopperRoute { board, intent, .. } => {
                    let route = compile_copper_route(snapshot, board, intent)?;
                    for id in route
                        .tracks
                        .iter()
                        .map(|t| &t.source_uuid)
                        .chain(route.vias.iter().map(|v| &v.source_uuid))
                    {
                        if !routing_ids.insert(id.clone()) {
                            return Err(EngineError::TargetAlreadyExists(id.clone()));
                        }
                    }
                }
                OperationKind::AddStraightTrack { board, intent } => {
                    if snapshot.board.document_id.as_ref() != Some(board) {
                        return Err(EngineError::ScopeMismatch);
                    }
                    if let Some(routing) = &snapshot.native_routing
                        && routing.board_id != *board
                    {
                        return Err(EngineError::ScopeMismatch);
                    }
                    intent
                        .validate()
                        .map_err(|_| EngineError::InvalidPlan("invalid straight track intent"))?;
                    validate_routing_track_binding(snapshot, board, intent)?;
                    validate_routing_position(intent.start)?;
                    validate_routing_position(intent.end)?;
                    if !routing_ids.insert(intent.source_uuid.clone()) {
                        return Err(EngineError::TargetAlreadyExists(intent.source_uuid.clone()));
                    }
                }
                OperationKind::AddThroughVia { board, intent } => {
                    if snapshot.board.document_id.as_ref() != Some(board) {
                        return Err(EngineError::ScopeMismatch);
                    }
                    if let Some(routing) = &snapshot.native_routing
                        && routing.board_id != *board
                    {
                        return Err(EngineError::ScopeMismatch);
                    }
                    intent
                        .validate()
                        .map_err(|_| EngineError::InvalidPlan("invalid through via intent"))?;
                    if intent.net.is_none() {
                        return Err(EngineError::InvalidPlan(
                            "through via net binding is required",
                        ));
                    }
                    validate_observed_routing_net(snapshot, intent.net.as_ref())?;
                    validate_routing_position(intent.position)?;
                    if !routing_ids.insert(intent.source_uuid.clone()) {
                        return Err(EngineError::TargetAlreadyExists(intent.source_uuid.clone()));
                    }
                }
                OperationKind::FlipExistingFootprint { .. } => {
                    expected_footprint_flip(snapshot, &op.kind)?;
                }
                OperationKind::DeleteExistingItems { .. } => {
                    expected_item_deletion(snapshot, &op.kind)?;
                    if op.risk_class != RiskClass::High {
                        return Err(EngineError::InvalidPlan(
                            "deletion requires high risk classification",
                        ));
                    }
                }
                OperationKind::EditExistingItems { .. } => {
                    expected_existing_item_edit(snapshot, &op.kind)?;
                }
                OperationKind::MoveExistingFootprint {
                    board,
                    placement_id,
                    source_uuid,
                    position,
                    orientation_mdeg,
                    copper_tolerance_nm,
                    ..
                } => {
                    if !(1..=FOOTPRINT_COPPER_TOLERANCE_NM).contains(copper_tolerance_nm) {
                        return Err(EngineError::InvalidPlan("invalid copper tolerance"));
                    }
                    if snapshot.board.document_id.as_ref() != Some(board) {
                        return Err(EngineError::ScopeMismatch);
                    }
                    let footprints =
                        snapshot
                            .native_footprints
                            .as_ref()
                            .ok_or(EngineError::InvalidPlan(
                                "native footprint observation is required",
                            ))?;
                    let target = footprints
                        .placements
                        .iter()
                        .find(|placement| placement.placement_id == *placement_id)
                        .ok_or_else(|| EngineError::TargetNotFound(placement_id.clone()))?;
                    if target.source_uuid != *source_uuid {
                        return Err(EngineError::ScopeMismatch);
                    }
                    if target.lock_state == LockState::Locked {
                        return Err(EngineError::InvalidPlan("target footprint is locked"));
                    }
                    validate_routing_position(*position)?;
                    if orientation_mdeg.is_negative()
                        || *orientation_mdeg >= MAX_FOOTPRINT_ORIENTATION_MDEG
                    {
                        return Err(EngineError::InvalidPlan(
                            "footprint orientation is out of bounds",
                        ));
                    }
                }
            }
        }
        copper_route::validate_additive_board_identities(snapshot, &self.operations)?;
        Ok(())
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum SessionState {
    Planned,
    Previewed,
    Validated,
    AwaitingApproval,
    Committed,
    Rejected,
    RolledBack,
}
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct AuditEvent {
    pub event: String,
    pub detail: String,
}
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ChangeSession {
    pub plan: ChangePlan,
    pub plan_hash: String,
    pub state: SessionState,
    pub approval_hash: Option<String>,
    pub snapshot: ProjectSnapshot,
    pub audit: Vec<AuditEvent>,
    pub committed_idempotency: BTreeMap<String, String>,
}
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Preview {
    pub plan_hash: String,
    pub before: ProjectSnapshot,
    pub after: ProjectSnapshot,
}

#[derive(Debug, Eq, PartialEq)]
pub enum EngineError {
    InvalidPlan(&'static str),
    OperationLimitExceeded,
    DuplicateOperationId,
    DuplicateIdempotencyKey,
    InvalidOrder,
    ScopeMismatch,
    TargetNotFound(StableId),
    TargetAlreadyExists(StableId),
    PreconditionFailed,
    RevisionStale,
    InstanceStale,
    InvalidState,
    ApprovalMismatch,
    Serialization(String),
    UnsupportedOperation,
}
impl Display for EngineError {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidPlan(s) => write!(f, "invalid_plan:{s}"),
            Self::OperationLimitExceeded => f.write_str("operation_limit_exceeded"),
            Self::DuplicateOperationId => f.write_str("duplicate_operation_id"),
            Self::DuplicateIdempotencyKey => f.write_str("duplicate_idempotency_key"),
            Self::InvalidOrder => f.write_str("invalid_order"),
            Self::ScopeMismatch => f.write_str("scope_mismatch"),
            Self::TargetNotFound(id) => write!(f, "target_not_found:{id}"),
            Self::TargetAlreadyExists(id) => write!(f, "target_already_exists:{id}"),
            Self::PreconditionFailed => f.write_str("precondition_failed"),
            Self::RevisionStale => f.write_str("revision_stale"),
            Self::InstanceStale => f.write_str("instance_stale"),
            Self::InvalidState => f.write_str("invalid_state"),
            Self::ApprovalMismatch => f.write_str("approval_mismatch"),
            Self::Serialization(s) => write!(f, "serialization:{s}"),
            Self::UnsupportedOperation => f.write_str("unsupported_operation"),
        }
    }
}
impl std::error::Error for EngineError {}

pub struct Engine {
    snapshot: ProjectSnapshot,
    instance_token: String,
    committed: BTreeMap<String, String>,
}
impl Engine {
    pub fn new(snapshot: ProjectSnapshot, instance_token: impl Into<String>) -> Self {
        Self {
            snapshot,
            instance_token: instance_token.into(),
            committed: BTreeMap::new(),
        }
    }
    pub fn snapshot(&self) -> &ProjectSnapshot {
        &self.snapshot
    }
    fn check_identity(&self, plan: &ChangePlan) -> Result<(), EngineError> {
        if plan.instance_token != self.instance_token {
            return Err(EngineError::InstanceStale);
        }
        if plan.base_revision
            != self
                .snapshot
                .revision()
                .map_err(|e| EngineError::Serialization(e.to_string()))?
        {
            return Err(EngineError::RevisionStale);
        }
        Ok(())
    }
    pub fn begin_session(&self, plan: ChangePlan) -> Result<ChangeSession, EngineError> {
        self.check_identity(&plan)?;
        plan.validate(&self.snapshot)?;
        let hash = plan.hash()?;
        Ok(ChangeSession {
            plan,
            plan_hash: hash,
            state: SessionState::Planned,
            approval_hash: None,
            snapshot: self.snapshot.clone(),
            audit: vec![AuditEvent {
                event: "planned".into(),
                detail: "no committed mutation".into(),
            }],
            committed_idempotency: BTreeMap::new(),
        })
    }
    pub fn preview(&self, plan: &ChangePlan) -> Result<Preview, EngineError> {
        self.check_identity(plan)?;
        plan.validate(&self.snapshot)?;
        let before = self.snapshot.clone();
        let after = apply_operations(before.clone(), &plan.operations)?;
        Ok(Preview {
            plan_hash: plan.hash()?,
            before,
            after,
        })
    }
    pub fn preview_session(&self, session: &mut ChangeSession) -> Result<Preview, EngineError> {
        if session.state != SessionState::Planned {
            return Err(EngineError::InvalidState);
        }
        let preview = self.preview(&session.plan)?;
        session.state = SessionState::Previewed;
        session.audit.push(AuditEvent {
            event: "previewed".into(),
            detail: preview.plan_hash.clone(),
        });
        Ok(preview)
    }
    pub fn validate(&self, session: &mut ChangeSession) -> Result<(), EngineError> {
        if session.state != SessionState::Previewed {
            return Err(EngineError::InvalidState);
        }
        self.check_identity(&session.plan)?;
        session.plan.validate(&self.snapshot)?;
        session.state = SessionState::Validated;
        session.audit.push(AuditEvent {
            event: "validated".into(),
            detail: "preconditions hold".into(),
        });
        Ok(())
    }
    pub fn request_approval(&self, session: &mut ChangeSession) -> Result<(), EngineError> {
        if session.state != SessionState::Validated {
            return Err(EngineError::InvalidState);
        }
        session.state = SessionState::AwaitingApproval;
        session.audit.push(AuditEvent {
            event: "awaiting_approval".into(),
            detail: session.plan_hash.clone(),
        });
        Ok(())
    }
    pub fn approve(&self, session: &mut ChangeSession, plan_hash: &str) -> Result<(), EngineError> {
        let current_hash = session.plan.hash()?;
        if session.state != SessionState::AwaitingApproval
            || session.plan_hash != current_hash
            || current_hash != plan_hash
        {
            return Err(EngineError::ApprovalMismatch);
        }
        session.approval_hash = Some(plan_hash.into());
        session.audit.push(AuditEvent {
            event: "approved".into(),
            detail: plan_hash.into(),
        });
        Ok(())
    }
    pub fn commit(&mut self, session: &mut ChangeSession) -> Result<(), EngineError> {
        let current_hash = session.plan.hash()?;
        if session.state != SessionState::AwaitingApproval
            || session.plan_hash != current_hash
            || session.approval_hash.as_deref() != Some(current_hash.as_str())
        {
            return Err(EngineError::InvalidState);
        }
        if session
            .plan
            .operations
            .iter()
            .all(|op| self.committed.get(&op.idempotency_key) == Some(&current_hash))
        {
            session.state = SessionState::Committed;
            session.audit.push(AuditEvent {
                event: "commit_replayed".into(),
                detail: current_hash,
            });
            return Ok(());
        }
        self.check_identity(&session.plan)?;
        session.plan.validate(&self.snapshot)?;
        let updated = apply_operations(self.snapshot.clone(), &session.plan.operations)?;
        self.snapshot = updated;
        for op in &session.plan.operations {
            self.committed
                .insert(op.idempotency_key.clone(), session.plan_hash.clone());
            session
                .committed_idempotency
                .insert(op.idempotency_key.clone(), session.plan_hash.clone());
        }
        session.state = SessionState::Committed;
        session.audit.push(AuditEvent {
            event: "committed".into(),
            detail: session.plan_hash.clone(),
        });
        Ok(())
    }
    pub fn reject(&self, session: &mut ChangeSession) -> Result<(), EngineError> {
        if session.state != SessionState::AwaitingApproval {
            return Err(EngineError::InvalidState);
        }
        session.state = SessionState::Rejected;
        session.audit.push(AuditEvent {
            event: "rejected".into(),
            detail: session.plan_hash.clone(),
        });
        Ok(())
    }
    pub fn rollback(&mut self, session: &mut ChangeSession) -> Result<(), EngineError> {
        if session.state != SessionState::Committed {
            return Err(EngineError::InvalidState);
        }
        self.snapshot = session.snapshot.clone();
        for op in &session.plan.operations {
            if self.committed.get(&op.idempotency_key) == Some(&session.plan_hash) {
                self.committed.remove(&op.idempotency_key);
            }
        }
        session.committed_idempotency.clear();
        session.state = SessionState::RolledBack;
        session.audit.push(AuditEvent {
            event: "rolled_back".into(),
            detail: session.plan_hash.clone(),
        });
        Ok(())
    }
}

fn apply_operations(
    mut snapshot: ProjectSnapshot,
    operations: &[Operation],
) -> Result<ProjectSnapshot, EngineError> {
    let base_revision = snapshot
        .revision()
        .map_err(|error| EngineError::Serialization(error.to_string()))?;
    for op in operations {
        match &op.kind {
            OperationKind::AddSymbol { symbol } => snapshot.schematic.symbols.push(symbol.clone()),
            OperationKind::SetProperty {
                target,
                property,
                value,
            } if property == "value" => {
                let symbol = snapshot
                    .schematic
                    .symbols
                    .iter_mut()
                    .find(|s| s.source_uuid == *target)
                    .ok_or_else(|| EngineError::TargetNotFound(target.clone()))?;
                symbol.value = Some(value.clone());
            }
            OperationKind::SetProperty { .. } => return Err(EngineError::UnsupportedOperation),
            OperationKind::PlaceAllowlistedFootprint { placement, .. } => {
                snapshot.board.footprints.push(Footprint {
                    source_uuid: placement.target_id.clone(),
                    observed_revision: base_revision.clone(),
                    reference: Some(placement.reference.clone()),
                    value: Some(placement.value.clone()),
                    position: placement.position,
                    library_identifier: Some(placement.library_identifier.clone()),
                });
            }
            OperationKind::PlaceConnectedFootprints { assembly, .. } => {
                // Native pad geometry is bound by disposable validation, not invented here.
                for placement in &assembly.placements {
                    snapshot.board.footprints.push(Footprint {
                        source_uuid: placement.target_id.clone(),
                        observed_revision: base_revision.clone(),
                        reference: Some(placement.reference.clone()),
                        value: Some(placement.value.clone()),
                        position: placement.position,
                        library_identifier: Some(placement.library_identifier.clone()),
                    });
                }
            }
            OperationKind::AddBoardGraphicSegments { segments, .. } => {
                snapshot.board.graphics.extend(segments.clone());
            }
            OperationKind::AddBoardOutline {
                top_left,
                bottom_right,
                width_nm,
                ..
            } => {
                snapshot.board.graphics.extend(expand_board_outline(
                    *top_left,
                    *bottom_right,
                    *width_nm,
                ));
            }
            OperationKind::AddCopperRoute { board, intent, .. } => {
                let (tracks, vias) = intent
                    .primitives()
                    .map_err(|e| EngineError::Serialization(e.to_string()))?;
                let expanded = tracks
                    .into_iter()
                    .map(|intent| OperationKind::AddStraightTrack {
                        board: board.clone(),
                        intent,
                    })
                    .chain(vias.into_iter().map(|intent| OperationKind::AddThroughVia {
                        board: board.clone(),
                        intent,
                    }))
                    .map(|kind| {
                        let mut operation = op.clone();
                        operation.kind = kind;
                        operation
                    })
                    .collect::<Vec<_>>();
                snapshot = apply_operations(snapshot, &expanded)?;
            }
            OperationKind::AddStraightTrack { board, intent } => {
                let mut routing = snapshot.native_routing.take().unwrap_or_else(|| {
                    ObservedBoardRouting::new(board.clone(), vec![], vec![])
                        .expect("empty routing observation is valid")
                });
                routing.tracks.push(ObservedTrack {
                    track_id: kicad_model::board_track_id(board, &intent.source_uuid),
                    source_uuid: intent.source_uuid.clone(),
                    geometry: ObservedTrackGeometry::Segment {
                        start: intent.start,
                        end: intent.end,
                    },
                    width_nm: intent.width_nm,
                    layer: intent.layer.board_layer(),
                    lock_state: LockState::Unlocked,
                    net: intent
                        .net
                        .as_ref()
                        .map(|net| kicad_model::ObservedRoutingNet {
                            net_id: Some(net.net_id.clone()),
                            code: None,
                            name: net.name.clone(),
                        }),
                    observed_revision: routing.observed_revision.clone(),
                });
                snapshot.native_routing = Some(
                    ObservedBoardRouting::new(board.clone(), routing.tracks, routing.vias)
                        .map_err(|error| EngineError::Serialization(error.to_string()))?,
                );
            }
            OperationKind::AddThroughVia { board, intent } => {
                let mut routing = snapshot.native_routing.take().unwrap_or_else(|| {
                    ObservedBoardRouting::new(board.clone(), vec![], vec![])
                        .expect("empty routing observation is valid")
                });
                let ratio = FiniteRatio::new(0.0)
                    .map_err(|error| EngineError::Serialization(error.to_string()))?;
                let pad = |layer| PadCopperShape {
                    layer,
                    shape: PadShape::Circle,
                    size: SizeNm {
                        x_nm: intent.size_nm,
                        y_nm: intent.size_nm,
                    },
                    offset: LocalCoordinateNm { x_nm: 0, y_nm: 0 },
                    corner_rounding_ratio: ratio,
                    chamfer_ratio: ratio,
                    chamfer_top_left: false,
                    chamfer_top_right: false,
                    chamfer_bottom_left: false,
                    chamfer_bottom_right: false,
                    trapezoid_delta: LocalCoordinateNm { x_nm: 0, y_nm: 0 },
                    custom_anchor_shape: None,
                };
                routing.vias.push(ObservedVia {
                    via_id: kicad_model::board_via_id(board, &intent.source_uuid),
                    source_uuid: intent.source_uuid.clone(),
                    position: intent.position,
                    pad_stack: ObservedViaPadStack {
                        stack_type: PadStackType::FrontInnerBack,
                        layers: match &snapshot.native_stackup {
                            Some(stackup) => kicad_model::enabled_copper_layers(stackup)
                                .map_err(|e| EngineError::Serialization(e.to_string()))?
                                .into_iter()
                                .map(|layer| layer.board_layer())
                                .collect(),
                            None => vec![BoardLayer::FrontCopper, BoardLayer::BackCopper],
                        },
                        primary_drill: Some(ObservedViaDrill {
                            start_layer: BoardLayer::FrontCopper,
                            end_layer: BoardLayer::BackCopper,
                            size: SizeNm {
                                x_nm: intent.drill_nm,
                                y_nm: intent.drill_nm,
                            },
                            shape: DrillShape::Circle,
                            capping: ViaDrillCappingMode::Unknown,
                            filling: ViaDrillFillingMode::Unknown,
                        }),
                        unconnected_layer_removal: UnconnectedLayerRemoval::Unknown,
                        copper_shapes: vec![
                            pad(BoardLayer::FrontCopper),
                            PadCopperShape {
                                layer: BoardLayer::InnerCopper(1),
                                shape: PadShape::Circle,
                                size: SizeNm { x_nm: 0, y_nm: 0 },
                                offset: LocalCoordinateNm { x_nm: 0, y_nm: 0 },
                                corner_rounding_ratio: ratio,
                                chamfer_ratio: ratio,
                                chamfer_top_left: false,
                                chamfer_top_right: false,
                                chamfer_bottom_left: false,
                                chamfer_bottom_right: false,
                                trapezoid_delta: LocalCoordinateNm { x_nm: 0, y_nm: 0 },
                                custom_anchor_shape: None,
                            },
                            pad(BoardLayer::BackCopper),
                        ],
                        angle_degrees: ExactAngleDegrees::new(0.0)
                            .map_err(|error| EngineError::Serialization(error.to_string()))?,
                        front_outer_layers: None,
                        back_outer_layers: None,
                        zone_settings: None,
                        secondary_drill: None,
                        tertiary_drill: None,
                        front_post_machining: None,
                        back_post_machining: None,
                    },
                    lock_state: LockState::Unlocked,
                    net: intent
                        .net
                        .as_ref()
                        .map(|net| kicad_model::ObservedRoutingNet {
                            net_id: Some(net.net_id.clone()),
                            code: None,
                            name: net.name.clone(),
                        }),
                    via_type: ViaType::Through,
                    observed_revision: routing.observed_revision.clone(),
                });
                snapshot.native_routing = Some(
                    ObservedBoardRouting::new(board.clone(), routing.tracks, routing.vias)
                        .map_err(|error| EngineError::Serialization(error.to_string()))?,
                );
            }
            OperationKind::FlipExistingFootprint { .. } => {
                snapshot = expected_footprint_flip(&snapshot, &op.kind)?;
            }
            OperationKind::DeleteExistingItems { .. } => {
                snapshot = expected_item_deletion(&snapshot, &op.kind)?;
            }
            OperationKind::EditExistingItems { .. } => {
                snapshot = expected_existing_item_edit(&snapshot, &op.kind)?;
            }
            OperationKind::MoveExistingFootprint { .. } => {
                snapshot = expected_footprint_move(&snapshot, &op.kind)?;
            }
            _ => return Err(EngineError::UnsupportedOperation),
        }
    }
    Ok(snapshot.canonical())
}

fn validate_routing_position(position: Coordinate) -> Result<(), EngineError> {
    if position.x.unsigned_abs() > MAX_BOARD_POSITION_NM as u64
        || position.y.unsigned_abs() > MAX_BOARD_POSITION_NM as u64
    {
        return Err(EngineError::InvalidPlan("routing position bounds exceeded"));
    }
    Ok(())
}

pub fn validate_routing_net(
    net: Option<&kicad_model::ObservedNetIdentity>,
    board_id: &StableId,
) -> Result<(), EngineError> {
    let Some(net) = net else {
        return Err(EngineError::InvalidPlan("routing net binding is required"));
    };
    if net.name.is_empty() || net.net_id != board_net_id(board_id, &net.name) {
        return Err(EngineError::InvalidPlan("routing net binding is invalid"));
    }
    Ok(())
}

pub fn validate_observed_routing_net(
    snapshot: &ProjectSnapshot,
    net: Option<&kicad_model::ObservedNetIdentity>,
) -> Result<(), EngineError> {
    let Some(net) = net else {
        return Err(EngineError::InvalidPlan("routing net binding is required"));
    };
    validate_routing_net(
        Some(net),
        snapshot
            .board
            .document_id
            .as_ref()
            .ok_or(EngineError::InvalidPlan(
                "board identity is required for routing",
            ))?,
    )?;
    if !requested_net_is_observed(snapshot, net) {
        return Err(EngineError::InvalidPlan(
            "routing net binding was not observed on the board",
        ));
    }
    Ok(())
}

fn observed_net_matches(
    requested: &kicad_model::ObservedNetIdentity,
    observed: &kicad_model::ObservedRoutingNet,
) -> bool {
    !observed.name.is_empty()
        && observed.name == requested.name
        && observed.net_id.as_ref() == Some(&requested.net_id)
}

fn requested_net_is_observed(
    snapshot: &ProjectSnapshot,
    requested: &kicad_model::ObservedNetIdentity,
) -> bool {
    snapshot
        .native_footprints
        .as_ref()
        .into_iter()
        .flat_map(|footprints| footprints.placements.iter())
        .flat_map(|placement| placement.pads.iter())
        .filter_map(|pad| pad.net.as_ref())
        .any(|net| net == requested)
        || snapshot
            .native_routing
            .as_ref()
            .into_iter()
            .flat_map(|routing| {
                routing
                    .tracks
                    .iter()
                    .map(|track| &track.net)
                    .chain(routing.vias.iter().map(|via| &via.net))
            })
            .filter_map(Option::as_ref)
            .any(|net| observed_net_matches(requested, net))
}

pub fn resolve_routing_endpoint<'a>(
    snapshot: &'a ProjectSnapshot,
    endpoint: &RoutingEndpointRef,
) -> Result<(&'a ObservedFootprintPlacement, &'a ObservedPad), EngineError> {
    let Some(observed) = &snapshot.native_footprints else {
        return Err(EngineError::InvalidPlan(
            "native pad observation is required",
        ));
    };
    let mut matches = observed.placements.iter().flat_map(|placement| {
        placement
            .pads
            .iter()
            .filter_map(move |pad| (pad.pad_id == endpoint.pad_id).then_some((placement, pad)))
    });
    let Some((placement, pad)) = matches.next() else {
        return Err(EngineError::InvalidPlan(
            "routing endpoint pad was not observed",
        ));
    };
    if matches.next().is_some() {
        return Err(EngineError::InvalidPlan(
            "routing endpoint pad is ambiguous",
        ));
    }
    if placement.placement_id != endpoint.footprint_placement_id
        || pad.footprint_placement_id != endpoint.footprint_placement_id
    {
        return Err(EngineError::InvalidPlan("routing endpoint parent mismatch"));
    }
    if pad.pad_id != kicad_model::footprint_pad_id(&placement.placement_id, &pad.source_uuid) {
        return Err(EngineError::InvalidPlan(
            "routing endpoint pad identity is invalid",
        ));
    }
    Ok((placement, pad))
}

pub fn pad_board_position(
    placement: &ObservedFootprintPlacement,
    pad: &ObservedPad,
) -> Result<Coordinate, EngineError> {
    if placement.position.space != DocumentSpace::Board
        || placement.position.unit != kicad_model::Unit::Nanometres
    {
        return Err(EngineError::InvalidPlan(
            "footprint position is not board nanometres",
        ));
    }
    let x = i128::from(pad.position.x_nm);
    let mut y = i128::from(pad.position.y_nm);
    if placement.side == kicad_model::BoardSide::Back {
        y = -y;
    }
    let angle = (placement.orientation_mdeg.rem_euclid(360_000)) as f64 / 1_000.0;
    let radians = -angle.to_radians();
    let rotated_x = (x as f64 * radians.cos() - y as f64 * radians.sin()).round() as i128;
    let rotated_y = (x as f64 * radians.sin() + y as f64 * radians.cos()).round() as i128;
    let x = i64::try_from(i128::from(placement.position.x) + rotated_x)
        .map_err(|_| EngineError::InvalidPlan("resolved pad position overflows"))?;
    let y = i64::try_from(i128::from(placement.position.y) + rotated_y)
        .map_err(|_| EngineError::InvalidPlan("resolved pad position overflows"))?;
    Ok(Coordinate {
        x,
        y,
        unit: kicad_model::Unit::Nanometres,
        space: DocumentSpace::Board,
    })
}

pub fn validate_routing_track_binding(
    snapshot: &ProjectSnapshot,
    board: &StableId,
    intent: &StraightTrackIntent,
) -> Result<(), EngineError> {
    let Some(start_ref) = &intent.start_pad else {
        return Err(EngineError::InvalidPlan(
            "track endpoint binding is required",
        ));
    };
    let Some(end_ref) = &intent.end_pad else {
        return Err(EngineError::InvalidPlan(
            "track endpoint binding is required",
        ));
    };
    let Some(requested_net) = &intent.net else {
        return Err(EngineError::InvalidPlan("track net binding is required"));
    };
    if requested_net.name.is_empty()
        || requested_net.net_id != board_net_id(board, &requested_net.name)
    {
        return Err(EngineError::InvalidPlan("track net binding is invalid"));
    }
    let (start_placement, start_pad) = resolve_routing_endpoint(snapshot, start_ref)?;
    let (end_placement, end_pad) = resolve_routing_endpoint(snapshot, end_ref)?;
    let Some(start_net) = &start_pad.net else {
        return Err(EngineError::InvalidPlan("start pad net is missing"));
    };
    let Some(end_net) = &end_pad.net else {
        return Err(EngineError::InvalidPlan("end pad net is missing"));
    };
    if start_net.name.is_empty()
        || end_net.name.is_empty()
        || start_net.net_id != kicad_model::board_net_id(board, &start_net.name)
        || end_net.net_id != kicad_model::board_net_id(board, &end_net.name)
        || start_net != end_net
        || requested_net != start_net
    {
        return Err(EngineError::InvalidPlan(
            "routing pads and net do not match",
        ));
    }
    if let Some(stackup) = &snapshot.native_stackup {
        intent
            .layer
            .require_enabled_on(stackup)
            .map_err(|e| EngineError::Serialization(e.to_string()))?;
    } else if !intent.layer.is_front() {
        return Err(EngineError::InvalidPlan(
            "selected copper layer requires a stackup observation",
        ));
    }
    if !copper_route::pad_accepts_layer(start_pad, intent.layer)
        || !copper_route::pad_accepts_layer(end_pad, intent.layer)
    {
        return Err(EngineError::InvalidPlan(
            "track layer is not available on both pads",
        ));
    }
    let start = pad_board_position(start_placement, start_pad)?;
    let end = pad_board_position(end_placement, end_pad)?;
    if intent.start != start || intent.end != end {
        return Err(EngineError::InvalidPlan(
            "track geometry does not match observed pads",
        ));
    }
    Ok(())
}

fn validate_outline(
    top_left: Coordinate,
    bottom_right: Coordinate,
    width_nm: i64,
) -> Result<(), EngineError> {
    if top_left.unit != kicad_model::Unit::Nanometres
        || bottom_right.unit != kicad_model::Unit::Nanometres
        || top_left.space != DocumentSpace::Board
        || bottom_right.space != DocumentSpace::Board
    {
        return Err(EngineError::InvalidPlan(
            "outline coordinates must use board nanometres",
        ));
    }
    if width_nm <= 0 || bottom_right.x <= top_left.x || bottom_right.y <= top_left.y {
        return Err(EngineError::InvalidPlan(
            "outline dimensions must be positive",
        ));
    }
    let span_x = i128::from(bottom_right.x) - i128::from(top_left.x);
    let span_y = i128::from(bottom_right.y) - i128::from(top_left.y);
    if span_x > MAX_GRAPHIC_SPAN_NM
        || span_y > MAX_GRAPHIC_SPAN_NM
        || span_x
            .checked_mul(span_y)
            .is_none_or(|area| area > MAX_GRAPHIC_BOUNDING_AREA_NM2)
    {
        return Err(EngineError::InvalidPlan("outline bounds exceeded"));
    }
    Ok(())
}

pub fn expand_board_outline(
    top_left: Coordinate,
    bottom_right: Coordinate,
    width_nm: i64,
) -> Vec<BoardGraphicSegment> {
    let points = [
        top_left,
        Coordinate {
            x: bottom_right.x,
            y: top_left.y,
            ..top_left
        },
        bottom_right,
        Coordinate {
            x: top_left.x,
            y: bottom_right.y,
            ..top_left
        },
    ];
    points
        .iter()
        .enumerate()
        .map(|(index, start)| BoardGraphicSegment {
            graphic_id: StableId::new(format!(
                "outline:{}:{}:{}:{}:edge:{index}",
                top_left.x, top_left.y, bottom_right.x, bottom_right.y
            ))
            .expect("outline identity is valid"),
            start: *start,
            end: points[(index + 1) % points.len()],
            width_nm,
            layer: kicad_model::BoardGraphicLayer::EdgeCuts,
        })
        .collect()
}

fn validate_graphics(segments: &[BoardGraphicSegment]) -> Result<(), EngineError> {
    if segments.is_empty() {
        return Err(EngineError::InvalidPlan(
            "graphic segment list must not be empty",
        ));
    }
    if segments.len() > MAX_OPERATIONS {
        return Err(EngineError::OperationLimitExceeded);
    }
    for segment in segments {
        segment
            .validate()
            .map_err(|_| EngineError::InvalidPlan("invalid board graphic"))?;
    }
    let min_x = segments
        .iter()
        .map(|segment| segment.start.x.min(segment.end.x))
        .min()
        .unwrap();
    let max_x = segments
        .iter()
        .map(|segment| segment.start.x.max(segment.end.x))
        .max()
        .unwrap();
    let min_y = segments
        .iter()
        .map(|segment| segment.start.y.min(segment.end.y))
        .min()
        .unwrap();
    let max_y = segments
        .iter()
        .map(|segment| segment.start.y.max(segment.end.y))
        .max()
        .unwrap();
    let width = i128::from(max_x) - i128::from(min_x);
    let height = i128::from(max_y) - i128::from(min_y);
    if width > MAX_GRAPHIC_SPAN_NM || height > MAX_GRAPHIC_SPAN_NM {
        return Err(EngineError::InvalidPlan("graphic bounding span exceeded"));
    }
    let area = width
        .checked_mul(height)
        .ok_or(EngineError::InvalidPlan("graphic bounding area overflowed"))?;
    if area > MAX_GRAPHIC_BOUNDING_AREA_NM2 {
        return Err(EngineError::InvalidPlan("graphic bounding area exceeded"));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    mod footprint_edit;
    use super::*;
    use kicad_model::{
        Board, BoardSide, FootprintLibraryIdentifier, ObservedBoardFootprints, ObservedNetIdentity,
        PadKind, Schematic, Unit, footprint_definition_id, footprint_pad_id,
        footprint_placement_id,
    };
    #[test]
    fn native_item_edit_expected_state_is_bounded_and_preserves_other_objects() {
        let (mut before, _, _, _) = observed_routing_snapshot(BoardSide::Front);
        let board = id("board");
        let source = id("existing-track");
        let target = kicad_model::board_track_id(&board, &source);
        before.native_routing = Some(
            ObservedBoardRouting::new(
                board.clone(),
                vec![ObservedTrack {
                    track_id: target.clone(),
                    source_uuid: source.clone(),
                    geometry: ObservedTrackGeometry::Segment {
                        start: board_coordinate(100, 250),
                        end: board_coordinate(300, 250),
                    },
                    width_nm: 100,
                    layer: BoardLayer::FrontCopper,
                    lock_state: LockState::Unlocked,
                    net: None,
                    observed_revision: digest('0'),
                }],
                vec![],
            )
            .unwrap(),
        );
        let change = ExistingItemChange::Track {
            target_id: target,
            source_uuid: source,
            start_nm: None,
            end_nm: None,
            width_nm: Some(150),
        };
        let kind = OperationKind::EditExistingItems {
            board: board.clone(),
            changes: vec![change.clone()],
            native_evidence: digest('a'),
        };
        let after = expected_existing_item_edit(&before, &kind).unwrap();
        assert_eq!(after.native_footprints, before.native_footprints);
        assert_eq!(after.board, before.board);
        assert_eq!(
            after.native_routing.as_ref().unwrap().tracks[0].width_nm,
            150
        );
        assert_eq!(
            before.native_routing.as_ref().unwrap().tracks[0].width_nm,
            100
        );
        let rebound = change.in_board_scope(&id("shadow"));
        assert_ne!(rebound.identities().0, change.identities().0);
        assert_eq!(rebound.identities().1, change.identities().1);
        for fault in 0..5 {
            let mut kind = kind.clone();
            let OperationKind::EditExistingItems { board, changes, .. } = &mut kind else {
                unreachable!()
            };
            match fault {
                0 => changes.push(changes[0].clone()),
                1 => *board = id("wrong"),
                2 => {
                    if let ExistingItemChange::Track { source_uuid, .. } = &mut changes[0] {
                        *source_uuid = id("foreign");
                    }
                }
                3 => {
                    if let ExistingItemChange::Track { width_nm, .. } = &mut changes[0] {
                        *width_nm = Some(100);
                    }
                }
                4 => {
                    if let ExistingItemChange::Track { end_nm, .. } = &mut changes[0] {
                        *end_nm = Some(ItemPointNm { x: 100, y: 250 });
                    }
                }
                _ => unreachable!(),
            }
            assert!(
                expected_existing_item_edit(&before, &kind).is_err(),
                "{fault}"
            );
        }
    }

    #[test]
    fn pad_partition_contract_detects_equal_count_rewiring() {
        let clusters = |sets: &[&[&str]]| {
            sets.iter()
                .map(|s| s.iter().map(|v| v.to_string()).collect())
                .collect::<Vec<Vec<String>>>()
        };
        let before = clusters(&[&["a", "b"], &["c", "d"]]);
        assert!(preserves_pad_connectivity(&before, &before));
        assert!(preserves_pad_connectivity(
            &before,
            &clusters(&[&["d", "c", "b", "a"]])
        ));
        assert!(!preserves_pad_connectivity(
            &before,
            &clusters(&[&["a", "c"], &["b", "d"]])
        ));
        assert!(!preserves_pad_connectivity(
            &before,
            &clusters(&[&["a", "b"], &["c"], &["d"]])
        ));
        assert!(!preserves_pad_connectivity(
            &before,
            &clusters(&[&["a", "b", "foreign"], &["c", "d"]])
        ));
        assert!(!preserves_pad_connectivity(
            &before,
            &clusters(&[&["a", "b"], &["a", "c", "d"]])
        ));
        assert!(preserves_pad_connectivity(
            &before,
            &clusters(&[&["a"], &["c", "d"]])
        ));
    }

    fn id(v: &str) -> StableId {
        StableId::new(v).unwrap()
    }
    fn digest(c: char) -> RevisionDigest {
        RevisionDigest::new(c.to_string().repeat(64)).unwrap()
    }
    fn snapshot() -> ProjectSnapshot {
        ProjectSnapshot {
            project_id: id("project"),
            kicad_version: "10.0.5".into(),
            capabilities: vec!["schematic.write".into()],
            schematic: Schematic::default(),
            board: Board {
                document_id: Some(id("board")),
                ..Board::default()
            },
            native_footprints: None,
            native_routing: None,
            native_stackup: None,
        }
    }
    fn board_coordinate(x: i64, y: i64) -> Coordinate {
        Coordinate {
            x,
            y,
            unit: Unit::Nanometres,
            space: DocumentSpace::Board,
        }
    }
    pub(super) fn observed_routing_snapshot(
        side: BoardSide,
    ) -> (
        ProjectSnapshot,
        RoutingEndpointRef,
        RoutingEndpointRef,
        ObservedNetIdentity,
    ) {
        let board_id = id("board");
        let net = ObservedNetIdentity {
            net_id: board_net_id(&board_id, "GND"),
            name: "GND".into(),
        };
        let library_identifier = FootprintLibraryIdentifier {
            nickname: "Resistor_SMD".into(),
            entry_name: "R_0603_1608Metric".into(),
        };
        let placements = [("footprint-a", 100_i64), ("footprint-b", 300_i64)]
            .into_iter()
            .map(|(source, x)| {
                let source_uuid = id(source);
                let placement_id = footprint_placement_id(&board_id, &source_uuid);
                let pad_source_uuid = id(&format!("pad-{source}"));
                let pad = ObservedPad {
                    pad_id: footprint_pad_id(&placement_id, &pad_source_uuid),
                    source_uuid: pad_source_uuid,
                    footprint_placement_id: placement_id.clone(),
                    number: "1".into(),
                    kind: PadKind::SurfaceMount,
                    lock_state: LockState::Unlocked,
                    position: LocalCoordinateNm { x_nm: 0, y_nm: 50 },
                    stack_type: PadStackType::Normal,
                    orientation_mdeg: 0,
                    layers: vec![BoardLayer::FrontCopper],
                    copper_shapes: vec![PadCopperShape {
                        layer: BoardLayer::FrontCopper,
                        shape: PadShape::Circle,
                        size: SizeNm {
                            x_nm: 100,
                            y_nm: 100,
                        },
                        offset: LocalCoordinateNm { x_nm: 0, y_nm: 0 },
                        corner_rounding_ratio: FiniteRatio::new(0.0).unwrap(),
                        chamfer_ratio: FiniteRatio::new(0.0).unwrap(),
                        chamfer_top_left: false,
                        chamfer_top_right: false,
                        chamfer_bottom_left: false,
                        chamfer_bottom_right: false,
                        trapezoid_delta: LocalCoordinateNm { x_nm: 0, y_nm: 0 },
                        custom_anchor_shape: None,
                    }],
                    drill: None,
                    net: Some(net.clone()),
                    observed_revision: digest('0'),
                };
                ObservedFootprintPlacement {
                    placement_id,
                    source_uuid,
                    definition_id: footprint_definition_id(&library_identifier),
                    library_identifier: library_identifier.clone(),
                    reference: source.into(),
                    value: "10k".into(),
                    position: board_coordinate(x, 200),
                    orientation_mdeg: 0,
                    side,
                    lock_state: LockState::Unlocked,
                    symbol_sheet_path: Vec::new(),
                    pads: vec![pad],
                    observed_revision: digest('0'),
                }
            })
            .collect::<Vec<_>>();
        let native = ObservedBoardFootprints::new(board_id.clone(), placements).unwrap();
        let start_pad = native.placements[0].pads[0].pad_id.clone();
        let end_pad = native.placements[1].pads[0].pad_id.clone();
        let start_placement = footprint_placement_id(&board_id, &id("footprint-a"));
        let end_placement = footprint_placement_id(&board_id, &id("footprint-b"));
        let mut snapshot = snapshot();
        snapshot.board.document_id = Some(board_id.clone());
        snapshot.native_footprints = Some(native);
        (
            snapshot,
            RoutingEndpointRef {
                pad_id: start_pad,
                footprint_placement_id: start_placement,
            },
            RoutingEndpointRef {
                pad_id: end_pad,
                footprint_placement_id: end_placement,
            },
            net,
        )
    }
    fn symbol(uuid: &str) -> Symbol {
        Symbol {
            source_uuid: id(uuid),
            observed_revision: digest('a'),
            sheet_path: "/".into(),
            reference: Some("R1".into()),
            value: Some("10k".into()),
            position: Coordinate {
                x: 0,
                y: 0,
                unit: Unit::Nanometres,
                space: kicad_model::DocumentSpace::Schematic,
            },
            pins: vec![],
        }
    }
    fn graphic(
        graphic_id: &str,
        start_x: i64,
        start_y: i64,
        end_x: i64,
        end_y: i64,
    ) -> BoardGraphicSegment {
        BoardGraphicSegment {
            graphic_id: id(graphic_id),
            start: Coordinate {
                x: start_x,
                y: start_y,
                unit: Unit::Nanometres,
                space: DocumentSpace::Board,
            },
            end: Coordinate {
                x: end_x,
                y: end_y,
                unit: Unit::Nanometres,
                space: DocumentSpace::Board,
            },
            width_nm: 100_000,
            layer: kicad_model::BoardGraphicLayer::DwgsUser,
        }
    }
    fn op(kind: OperationKind, target: &str, key: &str) -> Operation {
        Operation {
            operation_id: id(key),
            kind,
            targets: vec![id(target)],
            preconditions: vec![],
            expected_effects: vec![],
            risk_class: RiskClass::Low,
            idempotency_key: key.into(),
            source_run_id: "run".into(),
        }
    }
    fn allowlisted_footprint_op(target: &str, x: i64, y: i64, key: &str) -> Operation {
        Operation {
            operation_id: id(key),
            kind: OperationKind::PlaceAllowlistedFootprint {
                board: id("board"),
                placement: AllowlistedFootprintPlacement::new(
                    id(target),
                    kicad_model::FootprintLibraryIdentifier {
                        nickname: "Connector_USB".into(),
                        entry_name: "USB_C_Receptacle".into(),
                    },
                    Coordinate {
                        x,
                        y,
                        unit: Unit::Nanometres,
                        space: DocumentSpace::Board,
                    },
                    target,
                    "USB-C",
                )
                .unwrap(),
            },
            targets: vec![id("board"), id(target)],
            preconditions: vec![],
            expected_effects: vec![],
            risk_class: RiskClass::Low,
            idempotency_key: key.into(),
            source_run_id: "run".into(),
        }
    }
    fn plan(engine: &Engine, ops: Vec<Operation>) -> ChangePlan {
        ChangePlan {
            plan_id: id("plan"),
            source_run_id: "run".into(),
            base_revision: engine.snapshot().revision().unwrap(),
            instance_token: "token".into(),
            operations: ops,
        }
    }
    #[test]
    fn invalid_order_duplicate_scope_and_count_rejected() {
        let engine = Engine::new(snapshot(), "token");
        let add = op(
            OperationKind::AddSymbol {
                symbol: symbol("s"),
            },
            "s",
            "add",
        );
        let set = op(
            OperationKind::SetProperty {
                target: id("s"),
                property: "value".into(),
                value: "22k".into(),
            },
            "s",
            "set",
        );
        assert_eq!(
            engine.preview(&plan(&engine, vec![set.clone(), add.clone()])),
            Err(EngineError::InvalidOrder)
        );
        let duplicate = Operation {
            operation_id: id("other"),
            idempotency_key: add.idempotency_key.clone(),
            ..set
        };
        assert_eq!(
            engine.preview(&plan(&engine, vec![add.clone(), duplicate])),
            Err(EngineError::DuplicateIdempotencyKey)
        );
        let out = (0..=MAX_OPERATIONS)
            .map(|i| {
                op(
                    OperationKind::AddSymbol {
                        symbol: symbol(&format!("s{i}")),
                    },
                    &format!("s{i}"),
                    &format!("k{i}"),
                )
            })
            .collect();
        assert_eq!(
            engine.preview(&plan(&engine, out)),
            Err(EngineError::OperationLimitExceeded)
        );
        let mut bad_scope = add;
        bad_scope.targets.clear();
        assert_eq!(
            engine.preview(&plan(&engine, vec![bad_scope])),
            Err(EngineError::ScopeMismatch)
        );
    }
    #[test]
    fn full_state_order_exact_approval_and_stale_identity() {
        let mut engine = Engine::new(snapshot(), "token");
        let mut session = engine
            .begin_session(plan(
                &engine,
                vec![op(
                    OperationKind::AddSymbol {
                        symbol: symbol("s"),
                    },
                    "s",
                    "add",
                )],
            ))
            .unwrap();
        assert_eq!(session.state, SessionState::Planned);
        engine.preview_session(&mut session).unwrap();
        assert_eq!(session.state, SessionState::Previewed);
        engine.validate(&mut session).unwrap();
        assert_eq!(session.state, SessionState::Validated);
        engine.request_approval(&mut session).unwrap();
        assert_eq!(session.state, SessionState::AwaitingApproval);
        assert_eq!(
            engine.approve(&mut session, "wrong"),
            Err(EngineError::ApprovalMismatch)
        );
        let hash = session.plan_hash.clone();
        engine.approve(&mut session, &hash).unwrap();
        engine.commit(&mut session).unwrap();
        assert_eq!(session.state, SessionState::Committed);
        assert_eq!(engine.snapshot().schematic.symbols.len(), 1);
        engine.rollback(&mut session).unwrap();
        assert_eq!(session.state, SessionState::RolledBack);
        assert!(engine.snapshot().schematic.symbols.is_empty());
        assert_eq!(
            engine.rollback(&mut session),
            Err(EngineError::InvalidState)
        );
        let mut stale = plan(
            &engine,
            vec![op(
                OperationKind::AddSymbol {
                    symbol: symbol("new"),
                },
                "new",
                "new",
            )],
        );
        stale.instance_token = "old".into();
        assert_eq!(engine.preview(&stale), Err(EngineError::InstanceStale));
    }
    #[test]
    fn commit_applies_every_operation_in_plan_order() {
        let mut engine = Engine::new(snapshot(), "token");
        let operations = vec![
            op(
                OperationKind::AddSymbol {
                    symbol: symbol("first"),
                },
                "first",
                "add-first",
            ),
            op(
                OperationKind::AddSymbol {
                    symbol: symbol("second"),
                },
                "second",
                "add-second",
            ),
        ];
        let mut session = engine.begin_session(plan(&engine, operations)).unwrap();
        engine.preview_session(&mut session).unwrap();
        engine.validate(&mut session).unwrap();
        engine.request_approval(&mut session).unwrap();
        let hash = session.plan_hash.clone();
        engine.approve(&mut session, &hash).unwrap();

        engine.commit(&mut session).unwrap();

        assert_eq!(
            engine
                .snapshot()
                .schematic
                .symbols
                .iter()
                .map(|symbol| symbol.source_uuid.clone())
                .collect::<Vec<_>>(),
            [id("first"), id("second")]
        );
        assert_eq!(
            session
                .committed_idempotency
                .keys()
                .cloned()
                .collect::<Vec<_>>(),
            ["add-first", "add-second"]
        );
    }

    #[test]
    fn unsupported_commit_does_not_mutate_snapshot() {
        let mut engine = Engine::new(snapshot(), "token");
        let add_source = op(
            OperationKind::AddSymbol {
                symbol: symbol("source"),
            },
            "source",
            "add-source",
        );
        let add_target = op(
            OperationKind::AddSymbol {
                symbol: symbol("target"),
            },
            "target",
            "add-target",
        );
        let connect = Operation {
            operation_id: id("connect"),
            kind: OperationKind::ConnectPins {
                source: id("source"),
                target: id("target"),
            },
            targets: vec![id("source"), id("target")],
            preconditions: vec![],
            expected_effects: vec![],
            risk_class: RiskClass::Low,
            idempotency_key: "connect".into(),
            source_run_id: "run".into(),
        };
        let change_plan = plan(&engine, vec![add_source, add_target, connect]);
        let plan_hash = change_plan.hash().unwrap();
        let mut session = ChangeSession {
            plan: change_plan,
            plan_hash: plan_hash.clone(),
            state: SessionState::AwaitingApproval,
            approval_hash: Some(plan_hash),
            snapshot: engine.snapshot().clone(),
            audit: vec![],
            committed_idempotency: BTreeMap::new(),
        };
        assert_eq!(
            engine.commit(&mut session),
            Err(EngineError::UnsupportedOperation)
        );
        assert!(engine.snapshot().schematic.symbols.is_empty());
    }

    #[test]
    fn preconditions_are_evaluated_before_each_operation() {
        let engine = Engine::new(snapshot(), "token");
        let mut add = op(
            OperationKind::AddSymbol {
                symbol: symbol("s"),
            },
            "s",
            "add",
        );
        add.preconditions = vec![Precondition::TargetAbsent(id("s"))];
        assert!(engine.preview(&plan(&engine, vec![add.clone()])).is_ok());

        add.preconditions = vec![Precondition::TargetExists(id("missing"))];
        assert_eq!(
            engine.preview(&plan(&engine, vec![add.clone()])),
            Err(EngineError::PreconditionFailed)
        );
        add.preconditions = vec![Precondition::Revision(digest('b'))];
        assert_eq!(
            engine.preview(&plan(&engine, vec![add])),
            Err(EngineError::PreconditionFailed)
        );
    }

    #[test]
    fn approval_rejects_plan_mutation() {
        let mut engine = Engine::new(snapshot(), "token");
        let mut session = engine
            .begin_session(plan(
                &engine,
                vec![op(
                    OperationKind::AddSymbol {
                        symbol: symbol("s"),
                    },
                    "s",
                    "add",
                )],
            ))
            .unwrap();
        engine.preview_session(&mut session).unwrap();
        engine.validate(&mut session).unwrap();
        engine.request_approval(&mut session).unwrap();
        let hash = session.plan_hash.clone();
        engine.approve(&mut session, &hash).unwrap();
        session.plan.operations[0]
            .expected_effects
            .push("tampered".into());
        assert_eq!(engine.commit(&mut session), Err(EngineError::InvalidState));
        assert!(engine.snapshot().schematic.symbols.is_empty());
    }

    #[test]
    fn committed_plan_replay_is_idempotent() {
        let mut engine = Engine::new(snapshot(), "token");
        let mut session = engine
            .begin_session(plan(
                &engine,
                vec![op(
                    OperationKind::AddSymbol {
                        symbol: symbol("s"),
                    },
                    "s",
                    "add",
                )],
            ))
            .unwrap();
        engine.preview_session(&mut session).unwrap();
        engine.validate(&mut session).unwrap();
        engine.request_approval(&mut session).unwrap();
        let hash = session.plan_hash.clone();
        engine.approve(&mut session, &hash).unwrap();
        let mut replay = session.clone();
        engine.commit(&mut session).unwrap();
        engine.commit(&mut replay).unwrap();
        assert_eq!(engine.snapshot().schematic.symbols.len(), 1);
        assert_eq!(replay.audit.last().unwrap().event, "commit_replayed");
    }

    #[test]
    fn operation_kind_uses_stable_tagged_json() {
        let value = serde_json::to_value(op(
            OperationKind::SetProperty {
                target: id("s"),
                property: "value".into(),
                value: "22k".into(),
            },
            "s",
            "set",
        ))
        .unwrap();
        assert_eq!(value["kind"]["kind"], "SetProperty");
        assert_eq!(value["kind"]["arguments"]["property"], "value");
    }

    #[test]
    fn revision_stale_is_rejected() {
        let engine = Engine::new(snapshot(), "token");
        let mut stale = plan(
            &engine,
            vec![op(
                OperationKind::AddSymbol {
                    symbol: symbol("s"),
                },
                "s",
                "add",
            )],
        );
        stale.base_revision = digest('b');
        assert_eq!(engine.preview(&stale), Err(EngineError::RevisionStale));
    }

    #[test]
    fn allowlisted_footprint_preview_is_deterministic_and_additive() {
        let engine = Engine::new(snapshot(), "token");
        let first = engine
            .preview(&plan(
                &engine,
                vec![
                    allowlisted_footprint_op("J2", 2_000_000, 0, "place-j2"),
                    allowlisted_footprint_op("J1", 1_000_000, 0, "place-j1"),
                ],
            ))
            .unwrap();
        let second = engine
            .preview(&plan(
                &engine,
                vec![
                    allowlisted_footprint_op("J1", 1_000_000, 0, "place-j1"),
                    allowlisted_footprint_op("J2", 2_000_000, 0, "place-j2"),
                ],
            ))
            .unwrap();
        assert_eq!(first.after, second.after);
        assert_eq!(first.after.board.footprints[0].source_uuid, id("J1"));
        assert_eq!(
            first.after.board.footprints[0].reference.as_deref(),
            Some("J1")
        );

        let legacy = serde_json::to_value(OperationKind::PlaceFootprint {
            target: id("existing"),
            position: Coordinate {
                x: 1,
                y: 2,
                unit: Unit::Nanometres,
                space: DocumentSpace::Board,
            },
        })
        .unwrap();
        assert_eq!(legacy["kind"], "PlaceFootprint");
        assert!(legacy["arguments"].get("placement").is_none());
    }

    #[test]
    fn existing_footprint_move_validates_identity_and_updates_only_transform() {
        let (mut snapshot, _, _, _) = observed_routing_snapshot(BoardSide::Front);
        let board = snapshot.board.document_id.clone().unwrap();
        let target = snapshot
            .native_footprints
            .as_ref()
            .unwrap()
            .placements
            .first()
            .unwrap()
            .clone();
        let original = snapshot.clone();
        let operation = Operation {
            operation_id: id("move-existing"),
            kind: OperationKind::MoveExistingFootprint {
                board: board.clone(),
                placement_id: target.placement_id.clone(),
                source_uuid: target.source_uuid.clone(),
                position: board_coordinate(500, 600),
                orientation_mdeg: 90_000,
                copper_tolerance_nm: FOOTPRINT_COPPER_TOLERANCE_NM,
                native_evidence: digest('a'),
            },
            targets: vec![
                board,
                target.placement_id.clone(),
                target.source_uuid.clone(),
            ],
            preconditions: Vec::new(),
            expected_effects: vec!["move or rotate existing footprint".into()],
            risk_class: RiskClass::Medium,
            idempotency_key: "move-existing".into(),
            source_run_id: "m1".into(),
        };
        let plan = ChangePlan {
            plan_id: id("move-plan"),
            base_revision: original.revision().unwrap(),
            source_run_id: "m1".into(),
            operations: vec![operation],
            instance_token: "token".into(),
        };
        plan.validate(&original).unwrap();
        let mut changed_evidence = plan.clone();
        if let OperationKind::MoveExistingFootprint {
            native_evidence, ..
        } = &mut changed_evidence.operations[0].kind
        {
            *native_evidence = digest('b');
        }
        assert_ne!(plan.hash().unwrap(), changed_evidence.hash().unwrap());
        let expected = apply_operations(original.clone(), &plan.operations).unwrap();
        expected
            .native_footprints
            .as_ref()
            .unwrap()
            .validate()
            .unwrap();
        assert_ne!(expected.revision().unwrap(), original.revision().unwrap());
        let moved = expected.native_footprints.unwrap().placements[0].clone();
        assert_eq!(moved.position, board_coordinate(500, 600));
        assert_eq!(moved.orientation_mdeg, 90_000);
        assert_eq!(moved.source_uuid, target.source_uuid);
        snapshot = original;
        assert_eq!(snapshot.native_footprints.unwrap().placements[0], target);
    }

    #[test]
    fn allowlisted_footprint_commit_replay_and_rollback_follow_session_contract() {
        let mut engine = Engine::new(snapshot(), "token");
        let operation = allowlisted_footprint_op("J1", 1_000_000, 2_000_000, "place-j1");
        let mut session = engine
            .begin_session(plan(&engine, vec![operation]))
            .unwrap();
        let preview = engine.preview_session(&mut session).unwrap();
        assert_eq!(preview.after.board.footprints.len(), 1);
        engine.validate(&mut session).unwrap();
        engine.request_approval(&mut session).unwrap();
        let hash = session.plan_hash.clone();
        engine.approve(&mut session, &hash).unwrap();
        let mut replay = session.clone();

        engine.commit(&mut session).unwrap();
        assert_eq!(engine.snapshot().board.footprints.len(), 1);
        assert_eq!(
            engine.snapshot().board.footprints[0].position,
            Coordinate {
                x: 1_000_000,
                y: 2_000_000,
                unit: Unit::Nanometres,
                space: DocumentSpace::Board,
            }
        );
        engine.commit(&mut replay).unwrap();
        assert_eq!(replay.audit.last().unwrap().event, "commit_replayed");
        assert_eq!(engine.snapshot().board.footprints.len(), 1);
        engine.rollback(&mut session).unwrap();
        assert!(engine.snapshot().board.footprints.is_empty());
    }

    fn sourced_assembly() -> ConnectedFootprintAssembly {
        use kicad_model::{
            FootprintSourceBinding, ObservedSchematicPin, ObservedSchematicSymbol,
            ReferenceResolution, SchematicArtifactFile, SchematicArtifactRevision,
        };
        let schematic = SchematicArtifactRevision::new(
            vec![
                SchematicArtifactFile::new(
                    "board.kicad_sch",
                    42,
                    RevisionDigest::new("a".repeat(64)).unwrap(),
                )
                .unwrap(),
            ],
            "10.0.6",
            1,
            false,
        )
        .unwrap();
        let placements = ["R1", "R2"]
            .into_iter()
            .map(|reference| {
                let OperationKind::PlaceAllowlistedFootprint { mut placement, .. } =
                    allowlisted_footprint_op(reference, 5_000_000, 5_000_000, reference).kind
                else {
                    unreachable!()
                };
                placement.source = Some(Box::new(FootprintSourceBinding {
                    resolution_digest: None,
                    schematic: schematic.clone(),
                    symbol: ObservedSchematicSymbol {
                        symbol_id: id(reference),
                        source_uuid: id(reference),
                        sheet_instance_path: "/root".into(),
                        reference: reference.into(),
                        value: placement.value.clone(),
                        lib_identifier: "Device:R".into(),
                        footprint_field: Some("Connector_USB:USB_C_Receptacle".into()),
                        unit: 1,
                        pins: vec![ObservedSchematicPin {
                            pin_id: id("pin"),
                            source_uuid: id("pin"),
                            number: "1".into(),
                            name: None,
                            electrical_type: None,
                            net: ReferenceResolution::Resolved(id("GND")),
                        }],
                        observed_revision: schematic.digest.clone(),
                    },
                    symbol_path: vec![id("root"), id(reference)],
                    pad_nets: BTreeMap::from([("1".into(), Some("GND".into()))]),
                    library_digest: RevisionDigest::new("b".repeat(64)).unwrap(),
                    library_size: 100,
                }));
                placement
            })
            .collect();
        ConnectedFootprintAssembly {
            placements,
            connections: vec![PlannedPadConnection {
                track_id: id("connection"),
                start: PlannedPadRef {
                    target_id: id("R1"),
                    pad_number: "1".into(),
                },
                end: PlannedPadRef {
                    target_id: id("R2"),
                    pad_number: "1".into(),
                },
                width_nm: 250_000,
            }],
        }
    }

    #[test]
    fn assembly_contract_rejects_ambiguous_identity_and_unbound_pins() {
        let assembly = sourced_assembly();
        assembly.validate().unwrap();
        let mut invalid = Vec::new();
        let mut case = assembly.clone();
        case.placements.push(case.placements[0].clone());
        invalid.push(case);
        let mut case = assembly.clone();
        case.placements[0].source = None;
        invalid.push(case);
        let mut case = assembly.clone();
        case.connections[0].end.pad_number = "missing".into();
        invalid.push(case);
        let mut case = assembly.clone();
        case.connections[0].end.target_id = id("external");
        invalid.push(case);
        let mut case = assembly.clone();
        case.connections[0].width_nm = 0;
        invalid.push(case);
        let mut case = assembly.clone();
        case.connections[0].track_id = id("R1");
        invalid.push(case);
        let mut case = assembly.clone();
        case.connections.push(case.connections[0].clone());
        invalid.push(case);
        let mut case = assembly.clone();
        case.placements[0]
            .source
            .as_mut()
            .unwrap()
            .pad_nets
            .insert("1".into(), Some("OTHER".into()));
        invalid.push(case);
        for case in invalid {
            assert!(case.validate().is_err(), "{case:?}");
        }

        let engine = Engine::new(snapshot(), "token");
        let mut operation = op(
            OperationKind::PlaceConnectedFootprints {
                board: id("board"),
                assembly,
            },
            "board",
            "assembly",
        );
        operation.targets.extend([id("R1"), id("R2")]);
        let planned = plan(&engine, vec![operation.clone()]);
        engine.preview(&planned).unwrap();
        let mut changed = planned.clone();
        let OperationKind::PlaceConnectedFootprints { assembly, .. } =
            &mut changed.operations[0].kind
        else {
            unreachable!()
        };
        assembly.connections[0].width_nm += 1;
        assert_ne!(planned.hash().unwrap(), changed.hash().unwrap());
        operation.targets.pop();
        assert_eq!(
            engine.preview(&plan(&engine, vec![operation])),
            Err(EngineError::ScopeMismatch)
        );
    }

    #[test]
    fn allowlisted_footprint_serializes_as_a_new_tagged_operation() {
        let value = serde_json::to_value(OperationKind::PlaceAllowlistedFootprint {
            board: id("board"),
            placement: AllowlistedFootprintPlacement::new(
                id("J1"),
                kicad_model::FootprintLibraryIdentifier {
                    nickname: "Connector_USB".into(),
                    entry_name: "USB_C_Receptacle".into(),
                },
                Coordinate {
                    x: 1,
                    y: 2,
                    unit: Unit::Nanometres,
                    space: DocumentSpace::Board,
                },
                "J1",
                "USB-C",
            )
            .unwrap(),
        })
        .unwrap();
        assert_eq!(value["kind"], "PlaceAllowlistedFootprint");
        assert_eq!(
            value["arguments"]["placement"]["library_identifier"]["entry_name"],
            "USB_C_Receptacle"
        );
        assert_eq!(
            serde_json::from_value::<OperationKind>(value.clone()).unwrap(),
            serde_json::from_value(value).unwrap()
        );
    }

    #[test]
    fn allowlisted_footprint_rejects_invalid_context_bounds_and_identity() {
        let engine = Engine::new(snapshot(), "token");
        let mut invalid = allowlisted_footprint_op("J1", 0, 0, "invalid");
        let OperationKind::PlaceAllowlistedFootprint { placement, .. } = &mut invalid.kind else {
            unreachable!()
        };
        placement.library_identifier.nickname.clear();
        assert_eq!(
            engine.preview(&plan(&engine, vec![invalid])),
            Err(EngineError::InvalidPlan(
                "invalid allowlisted footprint placement"
            ))
        );

        let mut invalid = allowlisted_footprint_op("J1", 0, 0, "invalid-space");
        let OperationKind::PlaceAllowlistedFootprint { placement, .. } = &mut invalid.kind else {
            unreachable!()
        };
        placement.position.space = DocumentSpace::Schematic;
        assert!(matches!(
            engine.preview(&plan(&engine, vec![invalid])),
            Err(EngineError::InvalidPlan(_))
        ));

        let oversized = allowlisted_footprint_op("J1", MAX_BOARD_POSITION_NM + 1, 0, "oversized");
        assert_eq!(
            engine.preview(&plan(&engine, vec![oversized])),
            Err(EngineError::InvalidPlan(
                "footprint position bounds exceeded"
            ))
        );

        let duplicate = vec![
            allowlisted_footprint_op("J1", 0, 0, "place-1"),
            allowlisted_footprint_op("J1", 1, 1, "place-2"),
        ];
        assert_eq!(
            engine.preview(&plan(&engine, duplicate)),
            Err(EngineError::TargetAlreadyExists(id("J1")))
        );

        let mut existing = snapshot();
        existing.board.footprints.push(Footprint {
            source_uuid: id("J1"),
            observed_revision: digest('a'),
            reference: Some("J1".into()),
            value: Some("existing".into()),
            position: Coordinate {
                x: 0,
                y: 0,
                unit: Unit::Nanometres,
                space: DocumentSpace::Board,
            },
            library_identifier: None,
        });
        let engine = Engine::new(existing, "token");
        assert_eq!(
            engine.preview(&plan(
                &engine,
                vec![allowlisted_footprint_op("J1", 1, 1, "place")]
            )),
            Err(EngineError::TargetAlreadyExists(id("J1")))
        );
    }

    #[test]
    fn board_graphics_require_exact_scope_and_unique_identity() {
        let engine = Engine::new(snapshot(), "token");
        let segment = graphic("g1", 0, 0, 1_000_000, 1_000_000);
        let wrong_scope = op(
            OperationKind::AddBoardGraphicSegments {
                board: id("other-board"),
                segments: vec![segment.clone()],
            },
            "other-board",
            "graphic",
        );
        assert_eq!(
            engine.preview(&plan(&engine, vec![wrong_scope])),
            Err(EngineError::ScopeMismatch)
        );

        let duplicate = op(
            OperationKind::AddBoardGraphicSegments {
                board: id("board"),
                segments: vec![segment.clone(), segment],
            },
            "board",
            "graphic",
        );
        assert_eq!(
            engine.preview(&plan(&engine, vec![duplicate])),
            Err(EngineError::TargetAlreadyExists(id("g1")))
        );
    }

    #[test]
    fn board_outline_preview_expands_to_four_closed_edge_cuts_segments() {
        let engine = Engine::new(snapshot(), "token");
        let top_left = Coordinate {
            x: 0,
            y: 0,
            unit: kicad_model::Unit::Nanometres,
            space: DocumentSpace::Board,
        };
        let bottom_right = Coordinate {
            x: 1_000_000,
            y: 500_000,
            unit: kicad_model::Unit::Nanometres,
            space: DocumentSpace::Board,
        };
        let operation = op(
            OperationKind::AddBoardOutline {
                board: id("board"),
                top_left,
                bottom_right,
                width_nm: 100_000,
            },
            "board",
            "outline",
        );
        let preview = engine.preview(&plan(&engine, vec![operation])).unwrap();
        assert_eq!(preview.after.board.graphics.len(), 4);
        assert!(
            preview
                .after
                .board
                .graphics
                .iter()
                .all(|segment| segment.layer == kicad_model::BoardGraphicLayer::EdgeCuts)
        );
        let starts = preview
            .after
            .board
            .graphics
            .iter()
            .map(|segment| (segment.start.x, segment.start.y))
            .collect::<BTreeSet<_>>();
        let ends = preview
            .after
            .board
            .graphics
            .iter()
            .map(|segment| (segment.end.x, segment.end.y))
            .collect::<BTreeSet<_>>();
        assert_eq!(starts, ends);
        let invalid = op(
            OperationKind::AddBoardOutline {
                board: id("board"),
                top_left,
                bottom_right: Coordinate {
                    x: 0,
                    ..bottom_right
                },
                width_nm: 100_000,
            },
            "board",
            "invalid-outline",
        );
        assert!(matches!(
            engine.preview(&plan(&engine, vec![invalid])),
            Err(EngineError::InvalidPlan(_))
        ));
    }

    #[test]
    fn board_graphics_preview_commit_and_replay_are_deterministic() {
        let mut engine = Engine::new(snapshot(), "token");
        let operation = op(
            OperationKind::AddBoardGraphicSegments {
                board: id("board"),
                segments: vec![
                    graphic("g2", 1_000_000, 0, 0, 1_000_000),
                    graphic("g1", 0, 0, 1_000_000, 0),
                ],
            },
            "board",
            "graphics",
        );
        let mut session = engine
            .begin_session(plan(&engine, vec![operation]))
            .unwrap();
        let preview = engine.preview_session(&mut session).unwrap();
        assert_eq!(preview.after.board.graphics[0].graphic_id, id("g1"));
        assert_ne!(
            preview.before.revision().unwrap(),
            preview.after.revision().unwrap()
        );
        engine.validate(&mut session).unwrap();
        engine.request_approval(&mut session).unwrap();
        let hash = session.plan_hash.clone();
        engine.approve(&mut session, &hash).unwrap();
        let mut replay = session.clone();
        engine.commit(&mut session).unwrap();
        engine.commit(&mut replay).unwrap();
        assert_eq!(engine.snapshot().board.graphics.len(), 2);
        assert_eq!(replay.audit.last().unwrap().event, "commit_replayed");
    }

    #[test]
    fn board_graphics_reject_missing_scope_existing_identity_and_large_area() {
        let mut missing = snapshot();
        missing.board.document_id = None;
        let engine = Engine::new(missing, "token");
        let operation = op(
            OperationKind::AddBoardGraphicSegments {
                board: id("board"),
                segments: vec![graphic("g1", 0, 0, 1, 1)],
            },
            "board",
            "graphic",
        );
        assert_eq!(
            engine.preview(&plan(&engine, vec![operation])),
            Err(EngineError::ScopeMismatch)
        );

        let mut existing = snapshot();
        existing.board.graphics.push(graphic("g1", 0, 0, 1, 1));
        let engine = Engine::new(existing, "token");
        let duplicate = op(
            OperationKind::AddBoardGraphicSegments {
                board: id("board"),
                segments: vec![graphic("g1", 2, 2, 3, 3)],
            },
            "board",
            "graphic",
        );
        assert_eq!(
            engine.preview(&plan(&engine, vec![duplicate])),
            Err(EngineError::TargetAlreadyExists(id("g1")))
        );

        let engine = Engine::new(snapshot(), "token");
        let oversized = op(
            OperationKind::AddBoardGraphicSegments {
                board: id("board"),
                segments: vec![graphic("large", 0, 0, 20_000_000, 20_000_000)],
            },
            "board",
            "large",
        );
        assert_eq!(
            engine.preview(&plan(&engine, vec![oversized])),
            Err(EngineError::InvalidPlan("graphic bounding span exceeded"))
        );
    }

    #[test]
    fn endpoint_bound_same_net_track_passes_change_plan_validation() {
        let (snapshot, start_pad, end_pad, net) = observed_routing_snapshot(BoardSide::Front);
        let intent = StraightTrackIntent::new_endpoint_bound(
            id("track"),
            board_coordinate(100, 250),
            board_coordinate(300, 250),
            100,
            start_pad,
            end_pad,
            net,
        )
        .unwrap();
        let engine = Engine::new(snapshot, "token");
        let operation = op(
            OperationKind::AddStraightTrack {
                board: id("board"),
                intent,
            },
            "board",
            "track",
        );
        assert!(engine.preview(&plan(&engine, vec![operation])).is_ok());
    }

    #[test]
    fn back_side_pad_position_mirrors_local_y_axis() {
        let (snapshot, start_pad, _, _) = observed_routing_snapshot(BoardSide::Back);
        let (placement, pad) = resolve_routing_endpoint(&snapshot, &start_pad).unwrap();
        assert_eq!(
            pad_board_position(placement, pad).unwrap(),
            board_coordinate(100, 150)
        );
    }

    #[test]
    fn endpoint_bound_track_rejects_geometry_drift_before_preview() {
        let (snapshot, start_pad, end_pad, net) = observed_routing_snapshot(BoardSide::Front);
        let intent = StraightTrackIntent::new_endpoint_bound(
            id("track"),
            board_coordinate(100, 251),
            board_coordinate(300, 250),
            100,
            start_pad,
            end_pad,
            net,
        )
        .unwrap();
        let engine = Engine::new(snapshot, "token");
        let operation = op(
            OperationKind::AddStraightTrack {
                board: id("board"),
                intent,
            },
            "board",
            "track",
        );
        assert_eq!(
            engine.preview(&plan(&engine, vec![operation])),
            Err(EngineError::InvalidPlan(
                "track geometry does not match observed pads",
            ))
        );
    }

    #[test]
    fn coordinate_only_routing_is_rejected_before_preview() {
        let engine = Engine::new(snapshot(), "token");
        let coordinate = |x, y| Coordinate {
            x,
            y,
            unit: Unit::Nanometres,
            space: DocumentSpace::Board,
        };
        let intent =
            StraightTrackIntent::new(id("track"), coordinate(0, 0), coordinate(1_000, 2_000), 100)
                .unwrap();
        let operation = op(
            OperationKind::AddStraightTrack {
                board: id("board"),
                intent,
            },
            "board",
            "track",
        );
        assert_eq!(
            engine.preview(&plan(&engine, vec![operation])),
            Err(EngineError::InvalidPlan(
                "track endpoint binding is required"
            ))
        );
    }

    #[test]
    fn routing_intents_reject_wrong_board_bounds_and_duplicate_source_ids() {
        let engine = Engine::new(snapshot(), "token");
        let coordinate = |x, y| Coordinate {
            x,
            y,
            unit: Unit::Nanometres,
            space: DocumentSpace::Board,
        };
        let make_track = |source, x| {
            op(
                OperationKind::AddStraightTrack {
                    board: id("board"),
                    intent: StraightTrackIntent::new(
                        id(source),
                        coordinate(x, 0),
                        coordinate(x + 1, 1),
                        100,
                    )
                    .unwrap(),
                },
                "board",
                source,
            )
        };
        let mut second = make_track("same", 2);
        second.operation_id = id("same-second-operation");
        second.idempotency_key = "same-second-key".into();
        assert_eq!(
            engine.preview(&plan(&engine, vec![make_track("same", 0), second])),
            Err(EngineError::InvalidPlan(
                "track endpoint binding is required"
            ))
        );
        let oversized = op(
            OperationKind::AddThroughVia {
                board: id("board"),
                intent: ThroughViaIntent::new(
                    id("oversized"),
                    coordinate(MAX_BOARD_POSITION_NM + 1, 0),
                    600,
                    300,
                )
                .unwrap(),
            },
            "board",
            "oversized",
        );
        assert_eq!(
            engine.preview(&plan(&engine, vec![oversized])),
            Err(EngineError::InvalidPlan(
                "through via net binding is required"
            ))
        );
        let wrong_board = op(
            OperationKind::AddStraightTrack {
                board: id("other-board"),
                intent: StraightTrackIntent::new(
                    id("wrong-board"),
                    coordinate(0, 0),
                    coordinate(1, 1),
                    100,
                )
                .unwrap(),
            },
            "other-board",
            "wrong-board",
        );
        assert_eq!(
            engine.preview(&plan(&engine, vec![wrong_board])),
            Err(EngineError::ScopeMismatch)
        );
    }

    #[test]
    fn explicit_side_flip_preserves_scope_and_is_not_a_replay_toggle() {
        let (mut before, _, _, _) = observed_routing_snapshot(BoardSide::Front);
        let board = before.board.document_id.clone().unwrap();
        before.native_routing =
            Some(ObservedBoardRouting::new(board.clone(), vec![], vec![]).unwrap());
        let original = before.native_footprints.as_ref().unwrap().placements[0].clone();
        for axis in [FootprintFlipAxis::TopBottom, FootprintFlipAxis::LeftRight] {
            let mut kind = OperationKind::FlipExistingFootprint {
                board: board.clone(),
                placement_id: original.placement_id.clone(),
                source_uuid: original.source_uuid.clone(),
                target_side: BoardSide::Back,
                axis,
                native_evidence: digest('a'),
            };
            let after = expected_footprint_flip(&before, &kind).unwrap();
            let mut cyclic = before.clone();
            let footprints = cyclic.native_footprints.as_mut().unwrap();
            footprints.placements[0].orientation_mdeg -= 360_000;
            footprints.placements[0].pads[0].orientation_mdeg += 720_000;
            *footprints =
                ObservedBoardFootprints::new(board.clone(), footprints.placements.clone()).unwrap();
            assert_eq!(cyclic.canonical(), before.canonical());
            assert_eq!(cyclic.revision().unwrap(), before.revision().unwrap());
            assert_eq!(after.native_routing, before.native_routing);
            assert_eq!(after.board, before.board);
            let target = &after.native_footprints.as_ref().unwrap().placements[0];
            assert_eq!(target.position, original.position);
            assert_eq!(target.pads[0].position, original.pads[0].position);
            assert_eq!(target.pads[0].net, original.pads[0].net);
            assert!(expected_footprint_flip(&after, &kind).is_err());
            if let OperationKind::FlipExistingFootprint { target_side, .. } = &mut kind {
                *target_side = BoardSide::Front;
            }
            assert_eq!(
                expected_footprint_flip(&after, &kind).unwrap(),
                before.canonical()
            );
            if let OperationKind::FlipExistingFootprint {
                board, target_side, ..
            } = &mut kind
            {
                *board = id("foreign");
                *target_side = BoardSide::Back;
            }
            assert_eq!(
                expected_footprint_flip(&before, &kind),
                Err(EngineError::ScopeMismatch)
            );
        }
    }

    #[test]
    fn deletion_is_exact_and_cannot_bypass_original_connections() {
        let (mut before, _, _, _) = observed_routing_snapshot(BoardSide::Front);
        let board = before.board.document_id.clone().unwrap();
        before.native_routing =
            Some(ObservedBoardRouting::new(board.clone(), vec![], vec![]).unwrap());
        let fp = before.native_footprints.as_ref().unwrap().placements[0].clone();
        let target = ExistingItemTarget {
            kind: ExistingItemKind::Footprint,
            target_id: fp.placement_id.clone(),
            source_uuid: fp.source_uuid.clone(),
        };
        let operation = OperationKind::DeleteExistingItems {
            board: board.clone(),
            targets: vec![target.clone()],
            native_evidence: digest('a'),
        };
        let engine = Engine::new(before.clone(), "instance");
        let mut deletion = Operation {
            operation_id: id("delete"),
            kind: operation.clone(),
            targets: vec![
                board.clone(),
                target.target_id.clone(),
                target.source_uuid.clone(),
            ],
            preconditions: vec![],
            expected_effects: vec!["delete exact set".into()],
            risk_class: RiskClass::Medium,
            idempotency_key: "delete".into(),
            source_run_id: "run".into(),
        };
        assert!(matches!(
            plan(&engine, vec![deletion.clone()]).validate(&before),
            Err(EngineError::InvalidPlan(
                "deletion requires high risk classification"
            ))
        ));
        deletion.risk_class = RiskClass::High;
        plan(&engine, vec![deletion]).validate(&before).unwrap();
        let after = expected_item_deletion(&before, &operation).unwrap();
        assert_eq!(after.board, before.board);
        assert_eq!(after.native_routing, before.native_routing);
        assert_eq!(
            after.native_footprints.as_ref().unwrap().placements.len(),
            1
        );
        let remaining = &after.native_footprints.as_ref().unwrap().placements[0];
        assert_eq!(
            remaining.source_uuid,
            before.native_footprints.as_ref().unwrap().placements[1].source_uuid
        );
        assert!(expected_item_deletion(&after, &operation).is_err());
        for targets in [
            vec![],
            vec![target.clone(), target.clone()],
            vec![target.in_board_scope(&id("foreign"))],
        ] {
            assert!(
                expected_item_deletion(
                    &before,
                    &OperationKind::DeleteExistingItems {
                        board: board.clone(),
                        targets,
                        native_evidence: digest('a')
                    }
                )
                .is_err()
            );
        }
        let source = id("connected-track");
        let track_id = kicad_model::board_track_id(&board, &source);
        before.native_routing = Some(
            ObservedBoardRouting::new(
                board.clone(),
                vec![ObservedTrack {
                    track_id: track_id.clone(),
                    source_uuid: source.clone(),
                    geometry: ObservedTrackGeometry::Segment {
                        start: board_coordinate(100, 250),
                        end: board_coordinate(300, 250),
                    },
                    width_nm: 100,
                    layer: BoardLayer::FrontCopper,
                    lock_state: LockState::Unlocked,
                    net: None,
                    observed_revision: digest('0'),
                }],
                vec![],
            )
            .unwrap(),
        );
        let batch = OperationKind::DeleteExistingItems {
            board,
            targets: vec![
                ExistingItemTarget {
                    kind: ExistingItemKind::Track,
                    target_id: track_id,
                    source_uuid: source,
                },
                target,
            ],
            native_evidence: digest('a'),
        };
        assert!(matches!(
            expected_item_deletion(&before, &batch),
            Err(EngineError::InvalidPlan("connected_copper"))
        ));
    }
}
