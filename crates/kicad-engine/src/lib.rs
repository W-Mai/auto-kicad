use std::collections::{BTreeMap, BTreeSet};
use std::fmt::{Display, Formatter};

use kicad_model::{
    BoardGraphicSegment, Coordinate, DocumentSpace, ProjectSnapshot, RevisionDigest, StableId,
    Symbol,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

pub const MAX_OPERATIONS: usize = 100;
pub const MAX_GRAPHIC_SPAN_NM: i128 = 10_000_000;
pub const MAX_GRAPHIC_BOUNDING_AREA_NM2: i128 = 100_000_000_000_000;

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
    AddBoardGraphicSegments {
        board: StableId,
        segments: Vec<BoardGraphicSegment>,
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
            OperationKind::AddBoardGraphicSegments { board, .. } => vec![board],
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
            } else if !matches!(op.kind, OperationKind::AddBoardGraphicSegments { .. }) {
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
            OperationKind::AddBoardGraphicSegments { segments, .. } => {
                snapshot.board.graphics.extend(segments.clone());
            }
            _ => return Err(EngineError::UnsupportedOperation),
        }
    }
    Ok(snapshot.canonical())
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
}
