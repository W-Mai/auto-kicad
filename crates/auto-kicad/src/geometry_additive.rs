//! Source-bound additive intent for complete whole-board geometry verification.
use crate::geometry::{GeometryPlanningEvidence, prepare_with_library_context};
use kicad_adapter::{LiveBoardObservation, NativePcbBatch};
use kicad_engine::{Operation, OperationKind, Precondition, RiskClass};
use kicad_model::{
    RevisionDigest, StableId,
    geometry::{GeometryRequest, RefillMode},
};
use serde::Serialize;
use std::collections::BTreeSet;

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct AdditiveEvidence {
    pub(super) kinds: Vec<OperationKind>,
    #[serde(serialize_with = "serialize_batch")]
    pub(super) batch: NativePcbBatch,
    pub(super) identities: kicad_adapter::additive_identities::AdditiveCreationIdentities,
}

fn serialize_batch<S: serde::Serializer>(
    batch: &NativePcbBatch,
    serializer: S,
) -> Result<S::Ok, S::Error> {
    kicad_adapter::geometry_additions::batch_intent(batch).serialize(serializer)
}

impl AdditiveEvidence {
    pub(super) fn bind(&mut self, native: &RevisionDigest, rules: &RevisionDigest) {
        for kind in &mut self.kinds {
            if let OperationKind::AddCopperRoute {
                native_evidence,
                rule_evidence,
                ..
            } = kind
            {
                *native_evidence = native.clone();
                *rule_evidence = rules.clone();
            }
        }
    }

    pub(super) fn net_names(&self) -> BTreeSet<String> {
        self.batch
            .tracks
            .iter()
            .filter_map(|t| t.net.as_ref())
            .chain(self.batch.vias.iter().filter_map(|v| v.net.as_ref()))
            .map(|n| n.name.clone())
            .collect()
    }

    pub(super) fn changes_outline(&self) -> bool {
        self.batch
            .graphics
            .iter()
            .any(|g| g.layer == kicad_ipc::BoardGraphicLayer::EdgeCuts)
    }
}

pub(super) fn compile(
    source: &LiveBoardObservation,
    kinds: &[OperationKind],
) -> Result<AdditiveEvidence, String> {
    let board = source
        .snapshot
        .board
        .document_id
        .as_ref()
        .ok_or("missing additive board identity")?;
    if kinds.is_empty()
        || kinds.len() > kicad_engine::MAX_OPERATIONS
        || kinds
            .iter()
            .any(|kind| kicad_engine::geometry_plan::additive_board(kind) != Some(board))
    {
        return Err("geometry additions require bounded additive operations on one board".into());
    }
    let revision = source.snapshot.revision().map_err(|e| e.to_string())?;
    let operations = operations(source, kinds)?;
    let batch = kicad_adapter::prepare_bound_native_batch(&operations, &source.snapshot, &revision)
        .map_err(|e| format!("invalid additive native binding: {e:?}"))?;
    if batch
        .tracks
        .iter()
        .any(|t| t.net.as_ref().is_none_or(|n| n.name.is_empty()))
        || batch
            .vias
            .iter()
            .any(|v| v.net.as_ref().is_none_or(|n| n.name.is_empty()))
    {
        return Err("zoned additive copper requires explicit named nets".into());
    }
    let scope = serde_json::to_vec(&(board, &revision)).map_err(|e| e.to_string())?;
    let identities =
        kicad_adapter::additive_identities::AdditiveCreationIdentities::derive(&scope, &batch)?;
    Ok(AdditiveEvidence {
        kinds: kinds.to_vec(),
        batch,
        identities,
    })
}

/// Only zoned additive boards select complete refill; existing-item guards remain independent.
pub(super) fn prepare(
    endpoint: &str,
    source: &LiveBoardObservation,
    kinds: &[OperationKind],
) -> Result<Option<GeometryPlanningEvidence>, String> {
    let board = source
        .snapshot
        .board
        .document_id
        .as_ref()
        .ok_or("missing additive board identity")?;
    if kinds.is_empty()
        || !kinds
            .iter()
            .all(|kind| kicad_engine::geometry_plan::additive_board(kind) == Some(board))
    {
        return Ok(None);
    }
    let mut client = super::geometry::connected_client(endpoint, source)?;
    let observed = kicad_adapter::geometry::observe_generated_context(
        &mut client,
        &source.board_filename,
        board,
    )?;
    if observed.context().zones.is_empty()
        && observed
            .generated()
            .is_none_or(|g| g.generated_count() == 0)
    {
        return Ok(None);
    }
    let request = GeometryRequest {
        observed_revision: observed.context().revision.clone(),
        outline: None,
        zones: Vec::new(),
        refill: RefillMode::All,
    };
    prepare_with_library_context(endpoint, source, &request, None, kinds, None).map(Some)
}

pub(super) fn operations(
    source: &LiveBoardObservation,
    kinds: &[OperationKind],
) -> Result<Vec<Operation>, String> {
    let board = source
        .snapshot
        .board
        .document_id
        .as_ref()
        .ok_or("missing additive board identity")?;
    let revision = source.snapshot.revision().map_err(|e| e.to_string())?;
    let operations = kinds
        .iter()
        .enumerate()
        .map(|(index, kind)| Operation {
            operation_id: StableId::new(format!("additive:{index}"))
                .expect("fixed valid operation identity"),
            kind: kind.clone(),
            targets: vec![board.clone()],
            preconditions: vec![Precondition::Revision(revision.clone())],
            expected_effects: vec!["Create explicitly bound native items".into()],
            risk_class: RiskClass::Medium,
            idempotency_key: format!("additive:{index}"),
            source_run_id: "additive-preparation".into(),
        })
        .collect::<Vec<_>>();
    Ok(operations)
}
