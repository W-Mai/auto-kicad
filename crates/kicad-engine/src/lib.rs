use std::collections::{BTreeMap, BTreeSet};
use std::fmt::{Display, Formatter};

use kicad_model::{
    AllowlistedFootprintPlacement, BoardGraphicSegment, BoardLayer, Coordinate, DocumentSpace,
    DrillShape, ExactAngleDegrees, FiniteRatio, Footprint, LocalCoordinateNm, LockState,
    ObservedBoardRouting, ObservedTrack, ObservedTrackGeometry, ObservedVia, ObservedViaDrill,
    ObservedViaPadStack, PadCopperShape, PadShape, PadStackType, ProjectSnapshot, RevisionDigest,
    SizeNm, StableId, StraightTrackIntent, Symbol, ThroughViaIntent, UnconnectedLayerRemoval,
    ViaDrillCappingMode, ViaDrillFillingMode, ViaType,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

pub const MAX_OPERATIONS: usize = 100;
pub const MAX_GRAPHIC_SPAN_NM: i128 = 10_000_000;
pub const MAX_GRAPHIC_BOUNDING_AREA_NM2: i128 = 100_000_000_000_000;
pub const MAX_BOARD_POSITION_NM: i64 = 2_000_000_000;

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
    AddStraightTrack {
        board: StableId,
        intent: StraightTrackIntent,
    },
    AddThroughVia {
        board: StableId,
        intent: ThroughViaIntent,
    },
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
            OperationKind::AddBoardGraphicSegments { board, .. }
            | OperationKind::AddBoardOutline { board, .. }
            | OperationKind::AddStraightTrack { board, .. }
            | OperationKind::AddThroughVia { board, .. } => vec![board],
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
            if let OperationKind::PlaceAllowlistedFootprint { placement, .. } = &operation.kind
                && !footprint_targets.insert(placement.target_id.clone())
            {
                return Err(EngineError::TargetAlreadyExists(
                    placement.target_id.clone(),
                ));
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
                    | OperationKind::AddBoardGraphicSegments { .. }
                    | OperationKind::AddBoardOutline { .. }
                    | OperationKind::AddStraightTrack { .. }
                    | OperationKind::AddThroughVia { .. }
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
                    validate_routing_position(intent.position)?;
                    if !routing_ids.insert(intent.source_uuid.clone()) {
                        return Err(EngineError::TargetAlreadyExists(intent.source_uuid.clone()));
                    }
                }
            }
        }
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
                    layer: BoardLayer::FrontCopper,
                    lock_state: LockState::Unlocked,
                    net: None,
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
                        layers: vec![
                            BoardLayer::FrontCopper,
                            BoardLayer::InnerCopper(1),
                            BoardLayer::BackCopper,
                        ],
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
                    net: None,
                    via_type: ViaType::Through,
                    observed_revision: routing.observed_revision.clone(),
                });
                snapshot.native_routing = Some(
                    ObservedBoardRouting::new(board.clone(), routing.tracks, routing.vias)
                        .map_err(|error| EngineError::Serialization(error.to_string()))?,
                );
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
    use super::*;
    use kicad_model::{Board, Schematic, Unit};
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
    fn routing_intents_preview_into_f_cu_and_through_via_native_routing() {
        let engine = Engine::new(snapshot(), "token");
        let coordinate = |x, y| Coordinate {
            x,
            y,
            unit: Unit::Nanometres,
            space: DocumentSpace::Board,
        };
        let track =
            StraightTrackIntent::new(id("track"), coordinate(0, 0), coordinate(1_000, 2_000), 100)
                .unwrap();
        let via = ThroughViaIntent::new(id("via"), coordinate(3_000, 4_000), 600, 300).unwrap();
        let operations = vec![
            op(
                OperationKind::AddStraightTrack {
                    board: id("board"),
                    intent: track,
                },
                "board",
                "track",
            ),
            op(
                OperationKind::AddThroughVia {
                    board: id("board"),
                    intent: via,
                },
                "board",
                "via",
            ),
        ];
        let preview = engine.preview(&plan(&engine, operations)).unwrap();
        let routing = preview.after.native_routing.unwrap();
        assert_eq!(routing.board_id, id("board"));
        assert_eq!(routing.tracks.len(), 1);
        assert_eq!(routing.tracks[0].layer, BoardLayer::FrontCopper);
        assert_eq!(routing.vias.len(), 1);
        assert_eq!(routing.vias[0].via_type, ViaType::Through);
        assert_eq!(
            routing.vias[0].pad_stack.stack_type,
            PadStackType::FrontInnerBack
        );
        assert_eq!(
            routing.vias[0].pad_stack.layers,
            vec![
                BoardLayer::FrontCopper,
                BoardLayer::InnerCopper(1),
                BoardLayer::BackCopper,
            ]
        );
        assert_eq!(
            routing.vias[0]
                .pad_stack
                .copper_shapes
                .iter()
                .map(|shape| (shape.layer, shape.size.x_nm, shape.size.y_nm))
                .collect::<Vec<_>>(),
            vec![
                (BoardLayer::FrontCopper, 600, 600),
                (BoardLayer::InnerCopper(1), 0, 0),
                (BoardLayer::BackCopper, 600, 600),
            ]
        );
        let drill = routing.vias[0].pad_stack.primary_drill.as_ref().unwrap();
        assert_eq!(
            (drill.start_layer, drill.end_layer),
            (BoardLayer::FrontCopper, BoardLayer::BackCopper)
        );
        assert_eq!((drill.size.x_nm, drill.size.y_nm), (300, 300));
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
            Err(EngineError::TargetAlreadyExists(id("same")))
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
            Err(EngineError::InvalidPlan("routing position bounds exceeded"))
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
}
