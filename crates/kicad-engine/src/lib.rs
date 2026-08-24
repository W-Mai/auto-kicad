use std::collections::{BTreeMap, BTreeSet};
use std::fmt::{Display, Formatter};

use kicad_model::{Coordinate, DocumentSpace, ProjectSnapshot, RevisionDigest, StableId, Symbol};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

pub const MAX_OPERATIONS: usize = 100;

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
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
    fn rank(&self) -> u8 {
        match self.kind {
            OperationKind::AddSymbol { .. } => 0,
            OperationKind::SetProperty { .. } => 1,
            OperationKind::ConnectPins { .. } => 2,
            OperationKind::AssignFootprint { .. } => 3,
            OperationKind::PlaceFootprint { .. } => 4,
        }
    }
    fn refs(&self) -> Vec<&StableId> {
        match &self.kind {
            OperationKind::AddSymbol { symbol } => vec![&symbol.source_uuid],
            OperationKind::SetProperty { target, .. } => vec![target],
            OperationKind::ConnectPins { source, target } => vec![source, target],
            OperationKind::AssignFootprint { symbol, .. } => vec![symbol],
            OperationKind::PlaceFootprint { target, .. } => vec![target],
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
        let mut planned_adds = BTreeSet::new();
        let mut previous_rank = 0;
        for (index, operation) in self.operations.iter().enumerate() {
            let rank = operation.rank();
            if index > 0 && rank < previous_rank {
                return Err(EngineError::InvalidOrder);
            }
            previous_rank = rank;
            if let OperationKind::AddSymbol { symbol } = &operation.kind
                && (existing.contains(&symbol.source_uuid)
                    || !planned_adds.insert(symbol.source_uuid.clone()))
            {
                return Err(EngineError::TargetAlreadyExists(symbol.source_uuid.clone()));
            }
        }
        let mut known = existing.clone();
        for op in &self.operations {
            if op.source_run_id != self.source_run_id {
                return Err(EngineError::InvalidPlan("operation run identity mismatch"));
            }
            if op.targets.is_empty() || op.refs().iter().any(|target| !op.targets.contains(target))
            {
                return Err(EngineError::ScopeMismatch);
            }
            match &op.kind {
                OperationKind::AddSymbol { symbol } => {
                    if !known.insert(symbol.source_uuid.clone()) {
                        return Err(EngineError::TargetAlreadyExists(symbol.source_uuid.clone()));
                    }
                }
                OperationKind::SetProperty {
                    target, property, ..
                } => {
                    if property.is_empty() {
                        return Err(EngineError::InvalidPlan("property is empty"));
                    }
                    if !known.contains(target) {
                        return Err(EngineError::TargetNotFound(target.clone()));
                    }
                }
                OperationKind::ConnectPins { source, target } => {
                    if !known.contains(source) {
                        return Err(EngineError::TargetNotFound(source.clone()));
                    }
                    if !known.contains(target) {
                        return Err(EngineError::TargetNotFound(target.clone()));
                    }
                }
                OperationKind::AssignFootprint { symbol, footprint } => {
                    if footprint.is_empty() {
                        return Err(EngineError::InvalidPlan("footprint is empty"));
                    }
                    if !known.contains(symbol) {
                        return Err(EngineError::TargetNotFound(symbol.clone()));
                    }
                }
                OperationKind::PlaceFootprint { target, position } => {
                    if position.space != DocumentSpace::Board {
                        return Err(EngineError::InvalidPlan(
                            "footprint position must use board space",
                        ));
                    }
                    if !known.contains(target) {
                        return Err(EngineError::TargetNotFound(target.clone()));
                    }
                }
            }
            for precondition in &op.preconditions {
                if let Precondition::TargetAbsent(target) = precondition
                    && known.contains(target)
                {
                    return Err(EngineError::PreconditionFailed);
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
}
impl Engine {
    pub fn new(snapshot: ProjectSnapshot, instance_token: impl Into<String>) -> Self {
        Self {
            snapshot,
            instance_token: instance_token.into(),
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
        if session.state != SessionState::AwaitingApproval || session.plan_hash != plan_hash {
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
        if session.state != SessionState::AwaitingApproval
            || session.approval_hash.as_deref() != Some(session.plan_hash.as_str())
        {
            return Err(EngineError::InvalidState);
        }
        self.check_identity(&session.plan)?;
        session.plan.validate(&self.snapshot)?;
        self.snapshot = apply_operations(self.snapshot.clone(), &session.plan.operations)?;
        for op in &session.plan.operations {
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
            _ => return Err(EngineError::UnsupportedOperation),
        }
    }
    Ok(snapshot.canonical())
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
            board: Board::default(),
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
        let mut session = engine
            .begin_session(plan(
                &engine,
                vec![op(
                    OperationKind::ConnectPins {
                        source: id("source"),
                        target: id("target"),
                    },
                    "source",
                    "connect",
                )],
            ))
            .unwrap();
        engine.preview_session(&mut session).unwrap_err();
        assert!(engine.snapshot().schematic.symbols.is_empty());
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
}
