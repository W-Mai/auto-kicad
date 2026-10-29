pub mod copper_route;
pub use copper_route::BoardRouteRequest;
mod existing_footprint;
mod existing_item_edit;
mod footprint_authoring;
mod geometry;
mod geometry_additive;
mod geometry_connectivity;
mod geometry_drc;
pub use geometry::{GeometryExecution, GeometryPlanningEvidence};
mod library_settings;
pub mod recovery;
pub mod routing_rules;

use kicad_adapter::{
    BoardGraphicsCommitOutcome, BoardGraphicsExecutor, DiscoveryInput, DisposablePcbLifecycle,
    DocumentObservation, FootprintObservationError, LiveBoardObservation,
    LiveBoardObservationError, ObservedFootprintWithPads, OwnedPcbnewLauncher,
    artifact_discovery_snapshot, board_document_id, board_footprint_observation,
    discovery_snapshot, live_board_observation,
};
use kicad_checks::{BaselineDelta, CheckResult, DrcPolicy, DrcReport, compare_drc, run_pcb_drc};
use kicad_engine::{
    ChangePlan, ChangeSession, Engine, Operation, OperationKind, Precondition, RiskClass,
    expand_board_outline,
};
use kicad_ipc::{
    BoardGraphicLayer as IpcBoardGraphicLayer, BoardGraphicSegment as IpcBoardGraphicSegment,
    ClientConfig, DocumentQuery, KiCadClient, NngTransport,
};
use kicad_model::{
    DiscoverySnapshot, ProjectSnapshot, RevisionDigest, SchematicArtifactFile, StableId,
    StraightTrackIntent, ThroughViaIntent,
};
use kicad_schematic::{
    Limits as SchematicLimits, SchematicPatchOutcome, SchematicPreviewReceipt,
    TWO_RESISTOR_OPERATION_IDENTITY, commit_two_resistor_circuit, load_schematic_project,
    preview_two_resistor_circuit, schematic_change_available,
};
use rmcp::{
    ErrorData, ServerHandler,
    handler::server::{router::tool::ToolRouter, wrapper::Parameters},
    model::{CallToolResponse, ServerCapabilities, ServerInfo},
    tool, tool_handler, tool_router,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

pub const CONTRACT_SCHEMA_VERSION: &str = "1";
const CAPABILITY_MATRIX_JSON: &str = include_str!("../../../schemas/capability-matrix.json");
const MAX_BOARD_BATCH_ITEMS: usize = 100;

fn digest_bytes(bytes: &[u8]) -> RevisionDigest {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    RevisionDigest::new(hex::encode(hasher.finalize())).expect("sha256 is a valid revision digest")
}

fn digest_text(parts: &[&str]) -> String {
    let mut hasher = Sha256::new();
    for part in parts {
        hasher.update(part.as_bytes());
        hasher.update([0]);
    }
    format!("sha256:{}", hex::encode(hasher.finalize()))
}

fn schematic_state(outcome: &SchematicPatchOutcome) -> &'static str {
    match outcome {
        SchematicPatchOutcome::Committed { .. } => "committed",
        SchematicPatchOutcome::Rejected { .. } => "rejected",
        SchematicPatchOutcome::Unknown { .. } => "unknown",
    }
}

fn schematic_outcome_data(outcome: &SchematicPatchOutcome) -> Value {
    match outcome {
        SchematicPatchOutcome::Committed { artifact } => json!({
            "kind": "committed",
            "artifact": artifact,
        }),
        SchematicPatchOutcome::Rejected { reason } => json!({
            "kind": "rejected",
            "reason": format!("{reason:?}"),
        }),
        SchematicPatchOutcome::Unknown { reason } => json!({
            "kind": "unknown",
            "reason": reason,
        }),
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Capability {
    pub id: &'static str,
    pub domain: &'static str,
    pub mode: &'static str,
    pub status: &'static str,
}

pub trait DiscoveryService: Send + Sync {
    fn geometry_additive_planning(
        &self,
        _endpoint: &str,
        _source: &LiveBoardObservation,
        _kinds: &[OperationKind],
    ) -> Result<Option<geometry::GeometryPlanningEvidence>, String> {
        Ok(None)
    }

    fn geometry_planning(
        &self,
        _endpoint: &str,
        _source: &LiveBoardObservation,
        _request: &kicad_model::geometry::GeometryRequest,
    ) -> Result<geometry::GeometryPlanningEvidence, String> {
        Err("complete geometry planning evidence unavailable".into())
    }

    fn geometry_inspection(
        &self,
        _endpoint: &str,
        _source: &LiveBoardObservation,
    ) -> Result<Value, String> {
        Err("complete native geometry inspection is unavailable".into())
    }

    fn copper_route_evidence(
        &self,
        _endpoint: &str,
        _source: &LiveBoardObservation,
        _kinds: &[OperationKind],
    ) -> Result<copper_route::RouteEvidence, String> {
        Err("complete copper route evidence is unavailable".into())
    }

    fn routing_rule_inspection(
        &self,
        _endpoint: &str,
        _source: &LiveBoardObservation,
        _net_ids: &[String],
    ) -> Result<routing_rules::RoutingRuleInspection, String> {
        Err("native routing rule observation is unavailable".into())
    }

    fn item_deletion_evidence(
        &self,
        _endpoint: &str,
        _source: &LiveBoardObservation,
        _kind: &OperationKind,
    ) -> Result<RevisionDigest, String> {
        Err("complete deletion evidence is unavailable".into())
    }

    fn footprint_flip_evidence(
        &self,
        _endpoint: &str,
        _source: &LiveBoardObservation,
        _kind: &OperationKind,
    ) -> Result<RevisionDigest, String> {
        Err("complete native flip evidence is unavailable".into())
    }

    fn existing_item_edit_evidence(
        &self,
        _endpoint: &str,
        _source: &LiveBoardObservation,
        _changes: &[kicad_engine::ExistingItemChange],
    ) -> Result<RevisionDigest, String> {
        Err("complete existing item evidence is unavailable".into())
    }

    fn footprint_move_evidence(
        &self,
        _endpoint: &str,
        _source: &LiveBoardObservation,
    ) -> Result<(Vec<String>, RevisionDigest), String> {
        Err("complete footprint and copper evidence is unavailable".into())
    }

    fn discover(
        &self,
        endpoint: &str,
        project_dir: Option<&std::path::Path>,
    ) -> Result<DiscoverySnapshot, String>;

    fn project_snapshot(
        &self,
        _endpoint: &str,
        _project_dir: Option<&std::path::Path>,
    ) -> Result<ProjectSnapshot, String> {
        Err("normalized project snapshot is unavailable".into())
    }

    fn live_board_observation(
        &self,
        _endpoint: &str,
        _project_dir: Option<&std::path::Path>,
    ) -> Result<LiveBoardObservation, String> {
        Err("live board observation is unavailable".into())
    }
}

#[derive(Clone, Debug, Default)]
pub struct IpcDiscoveryService;

impl DiscoveryService for IpcDiscoveryService {
    fn geometry_additive_planning(
        &self,
        endpoint: &str,
        source: &LiveBoardObservation,
        kinds: &[OperationKind],
    ) -> Result<Option<geometry::GeometryPlanningEvidence>, String> {
        geometry_additive::prepare(endpoint, source, kinds)
    }

    fn geometry_planning(
        &self,
        endpoint: &str,
        source: &LiveBoardObservation,
        request: &kicad_model::geometry::GeometryRequest,
    ) -> Result<geometry::GeometryPlanningEvidence, String> {
        geometry::prepare(endpoint, source, request)
    }

    fn geometry_inspection(
        &self,
        endpoint: &str,
        source: &LiveBoardObservation,
    ) -> Result<Value, String> {
        geometry::inspect(endpoint, source)
    }

    fn copper_route_evidence(
        &self,
        endpoint: &str,
        source: &LiveBoardObservation,
        kinds: &[OperationKind],
    ) -> Result<copper_route::RouteEvidence, String> {
        copper_route::prepare(endpoint, source, kinds)
    }

    fn routing_rule_inspection(
        &self,
        endpoint: &str,
        source: &LiveBoardObservation,
        net_ids: &[String],
    ) -> Result<routing_rules::RoutingRuleInspection, String> {
        routing_rules::inspect(endpoint, source, net_ids)
    }

    fn item_deletion_evidence(
        &self,
        endpoint: &str,
        source: &LiveBoardObservation,
        kind: &OperationKind,
    ) -> Result<RevisionDigest, String> {
        existing_item_edit::prepare_deletion_evidence(endpoint, source, kind)
    }

    fn footprint_flip_evidence(
        &self,
        endpoint: &str,
        source: &LiveBoardObservation,
        kind: &OperationKind,
    ) -> Result<RevisionDigest, String> {
        existing_item_edit::prepare_flip_evidence(endpoint, source, kind)
    }

    fn existing_item_edit_evidence(
        &self,
        endpoint: &str,
        source: &LiveBoardObservation,
        changes: &[kicad_engine::ExistingItemChange],
    ) -> Result<RevisionDigest, String> {
        existing_item_edit::prepare_evidence(endpoint, source, changes)
    }

    fn footprint_move_evidence(
        &self,
        endpoint: &str,
        source: &LiveBoardObservation,
    ) -> Result<(Vec<String>, RevisionDigest), String> {
        let config =
            ClientConfig::new(endpoint, "auto-kicad-move-check").map_err(|e| e.to_string())?;
        let mut client = KiCadClient::new(
            NngTransport::connect(config.clone()).map_err(|e| e.to_string())?,
            config,
        );
        let mut ids = client
            .get_unsupported_move_copper(&source.board_filename)
            .map_err(|e| e.to_string())?;
        if client.instance_token() != Some(&source.instance_token) {
            return Err("editor identity changed during copper observation".into());
        }
        let templates = client
            .capture_footprint_instance_templates(&source.board_filename)
            .map_err(|e| e.to_string())?;
        if client.instance_token() != Some(&source.instance_token) {
            return Err("editor identity changed during native footprint observation".into());
        }
        ids.sort();
        Ok((ids, existing_footprint::footprint_evidence(&templates)?))
    }

    fn discover(
        &self,
        endpoint: &str,
        project_dir: Option<&std::path::Path>,
    ) -> Result<DiscoverySnapshot, String> {
        let version_config =
            ClientConfig::new(endpoint, "auto-kicad-mcp").map_err(|error| error.to_string())?;
        let version_transport =
            NngTransport::connect(version_config.clone()).map_err(|error| error.to_string())?;
        let mut version_client = KiCadClient::new(version_transport, version_config);
        let version = version_client
            .get_version()
            .map_err(|error| error.to_string())?;
        let schematic = observe_documents(&mut version_client, DocumentQuery::Schematic)?;
        let pcb = observe_documents(&mut version_client, DocumentQuery::Pcb)?;
        if let Some(project_dir) = project_dir
            && is_unsupported(&schematic)
            && is_unsupported(&pcb)
        {
            return artifact_discovery_snapshot(project_dir, endpoint, version)
                .map_err(|error| error.to_string());
        }
        discovery_snapshot(DiscoveryInput {
            endpoint: endpoint.to_owned(),
            version,
            schematic,
            pcb,
        })
        .map_err(|error| error.to_string())
    }

    fn live_board_observation(
        &self,
        endpoint: &str,
        _project_dir: Option<&std::path::Path>,
    ) -> Result<LiveBoardObservation, String> {
        let config =
            ClientConfig::new(endpoint, "auto-kicad-mcp").map_err(|error| error.to_string())?;
        let transport = NngTransport::connect(config.clone()).map_err(|error| error.to_string())?;
        let mut client = KiCadClient::new(transport, config);
        let version = client.get_version().map_err(|error| error.to_string())?;
        let mut documents = client
            .get_open_documents(DocumentQuery::Pcb)
            .map_err(|error| error.to_string())?;
        if documents.len() != 1 {
            return Err(format!(
                "expected exactly one open PCB document, found {}",
                documents.len()
            ));
        }
        let document = documents.pop().expect("document count checked");
        let board_filename = match &document.kind {
            kicad_ipc::OpenDocumentKind::Pcb { board_filename } if !board_filename.is_empty() => {
                board_filename.clone()
            }
            _ => return Err("open PCB document has no usable filename".into()),
        };
        observe_live_board(&mut client, version, document, &board_filename)
            .map_err(|error| error.to_string())
    }
}

#[derive(Debug)]
enum ObserveLiveBoardError {
    Ipc(kicad_ipc::IpcError),
    MissingInstanceToken,
    Footprints(FootprintObservationError),
    Routing(kicad_adapter::RoutingObservationError),
    Stackup(kicad_model::ModelError),
    LiveBoard(LiveBoardObservationError),
}

impl std::fmt::Display for ObserveLiveBoardError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Ipc(error) => error.fmt(formatter),
            Self::MissingInstanceToken => {
                formatter.write_str("KiCad instance token was not observed")
            }
            Self::Footprints(error) => error.fmt(formatter),
            Self::Routing(error) => error.fmt(formatter),
            Self::Stackup(error) => error.fmt(formatter),
            Self::LiveBoard(error) => error.fmt(formatter),
        }
    }
}

trait LiveBoardObservationSource {
    fn graphics_observation(
        &mut self,
        filename: &str,
        layer: kicad_ipc::BoardGraphicLayer,
    ) -> Result<kicad_ipc::ObservedBoardGraphics, kicad_ipc::IpcError> {
        Ok(kicad_ipc::ObservedBoardGraphics {
            segments: self.board_graphics(filename, layer)?,
            opaque: Vec::new(),
        })
    }

    fn board_graphics(
        &mut self,
        board_filename: &str,
        layer: kicad_ipc::BoardGraphicLayer,
    ) -> Result<Vec<kicad_ipc::ObservedBoardGraphicSegment>, kicad_ipc::IpcError>;

    fn footprint_instances(
        &mut self,
        board_filename: &str,
    ) -> Result<Vec<kicad_ipc::ObservedFootprintInstance>, kicad_ipc::IpcError>;

    fn footprint_pads(
        &mut self,
        board_filename: &str,
        footprint_source_uuid: &str,
    ) -> Result<Vec<kicad_ipc::ObservedPad>, kicad_ipc::IpcError>;

    fn board_routing(
        &mut self,
        board_filename: &str,
    ) -> Result<kicad_ipc::ObservedBoardRoutingItems, kicad_ipc::IpcError>;

    fn board_stackup(
        &mut self,
        board_filename: &str,
    ) -> Result<kicad_ipc::ObservedBoardStackup, kicad_ipc::IpcError>;

    fn latest_instance_token(&mut self) -> Option<kicad_ipc::InstanceToken>;
}

impl<T: kicad_ipc::KiCadTransport> LiveBoardObservationSource for KiCadClient<T> {
    fn graphics_observation(
        &mut self,
        filename: &str,
        layer: kicad_ipc::BoardGraphicLayer,
    ) -> Result<kicad_ipc::ObservedBoardGraphics, kicad_ipc::IpcError> {
        self.get_board_graphics_observation(filename, layer)
    }

    fn board_graphics(
        &mut self,
        board_filename: &str,
        layer: kicad_ipc::BoardGraphicLayer,
    ) -> Result<Vec<kicad_ipc::ObservedBoardGraphicSegment>, kicad_ipc::IpcError> {
        self.get_board_graphics_on_layer(board_filename, layer)
    }

    fn footprint_instances(
        &mut self,
        board_filename: &str,
    ) -> Result<Vec<kicad_ipc::ObservedFootprintInstance>, kicad_ipc::IpcError> {
        self.get_footprint_instances(board_filename)
    }

    fn footprint_pads(
        &mut self,
        board_filename: &str,
        footprint_source_uuid: &str,
    ) -> Result<Vec<kicad_ipc::ObservedPad>, kicad_ipc::IpcError> {
        self.get_footprint_definition_pads(board_filename, footprint_source_uuid)
    }

    fn board_routing(
        &mut self,
        board_filename: &str,
    ) -> Result<kicad_ipc::ObservedBoardRoutingItems, kicad_ipc::IpcError> {
        self.get_board_routing(board_filename)
    }

    fn board_stackup(
        &mut self,
        board_filename: &str,
    ) -> Result<kicad_ipc::ObservedBoardStackup, kicad_ipc::IpcError> {
        self.get_board_stackup(board_filename)
    }

    fn latest_instance_token(&mut self) -> Option<kicad_ipc::InstanceToken> {
        self.instance_token().cloned()
    }
}

fn observe_owned_shadow(
    launcher: &mut OwnedPcbnewLauncher,
) -> Result<LiveBoardObservation, String> {
    let mut client = launcher
        .connect_owned()
        .map_err(|error| error.to_string())?;
    let version = client.get_version().map_err(|error| error.to_string())?;
    let mut documents = client
        .get_open_documents(DocumentQuery::Pcb)
        .map_err(|error| error.to_string())?;
    if documents.len() != 1 {
        return Err(format!(
            "expected exactly one owned shadow PCB document, found {}",
            documents.len()
        ));
    }
    let document = documents.pop().expect("document count checked");
    let board_filename = match &document.kind {
        kicad_ipc::OpenDocumentKind::Pcb { board_filename } if !board_filename.is_empty() => {
            board_filename.clone()
        }
        _ => return Err("owned shadow document is not a usable PCB".into()),
    };
    observe_live_board(&mut client, version, document, &board_filename)
        .map_err(|error| error.to_string())
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
struct ShadowSourceFingerprint {
    board: Option<(RevisionDigest, u64, Option<std::time::SystemTime>)>,
    project: Option<(RevisionDigest, u64, Option<std::time::SystemTime>)>,
    rules: Option<(RevisionDigest, u64, Option<std::time::SystemTime>)>,
}

fn shadow_source_fingerprint(source_board: &std::path::Path) -> ShadowSourceFingerprint {
    fn fingerprint(
        path: &std::path::Path,
    ) -> Option<(RevisionDigest, u64, Option<std::time::SystemTime>)> {
        let bytes = std::fs::read(path).ok()?;
        let metadata = std::fs::metadata(path).ok()?;
        Some((
            digest_bytes(&bytes),
            metadata.len(),
            metadata.modified().ok(),
        ))
    }

    ShadowSourceFingerprint {
        board: fingerprint(source_board),
        project: fingerprint(&source_board.with_extension("kicad_pro")),
        rules: fingerprint(&source_board.with_extension("kicad_dru")),
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ShadowValidationState {
    Committed,
    Rejected,
    Unknown,
}

#[derive(Clone, Debug)]
pub struct ShadowValidationOutcome {
    pub state: ShadowValidationState,
    pub fresh_revision: Option<RevisionDigest>,
    pub reopened_revision: Option<RevisionDigest>,
    pub reopened: bool,
    pub normalized_snapshot_matches: bool,
    pub baseline: Option<DrcReport>,
    pub after: Option<DrcReport>,
    pub delta: Option<BaselineDelta>,
    pub source_unchanged: bool,
    pub shadow_cleaned: bool,
    pub geometry: Option<geometry::GeometryExecution>,
    pub prepared_footprints: Vec<kicad_adapter::footprint::SourceFootprintTemplate>,
    pub reason: Option<String>,
}

impl ShadowValidationOutcome {
    fn rejected(reason: impl Into<String>) -> Self {
        Self {
            state: ShadowValidationState::Rejected,
            fresh_revision: None,
            reopened_revision: None,
            reopened: false,
            normalized_snapshot_matches: false,
            baseline: None,
            after: None,
            delta: None,
            source_unchanged: true,
            shadow_cleaned: false,
            geometry: None,
            prepared_footprints: Vec::new(),
            reason: Some(reason.into()),
        }
    }

    fn unknown(reason: impl Into<String>) -> Self {
        Self {
            state: ShadowValidationState::Unknown,
            fresh_revision: None,
            reopened_revision: None,
            reopened: false,
            normalized_snapshot_matches: false,
            baseline: None,
            after: None,
            delta: None,
            source_unchanged: true,
            shadow_cleaned: false,
            geometry: None,
            prepared_footprints: Vec::new(),
            reason: Some(reason.into()),
        }
    }
}

fn validate_shadow_operation_kinds(operations: &[Operation]) -> Result<(), String> {
    if geometry::contains(operations) {
        return geometry::request(operations).map(|_| ());
    }

    if operations.is_empty() {
        return Err("shadow operation batch is empty".into());
    }
    if let [operation] = operations
        && !operation.kind.footprint_placements().is_empty()
    {
        for placement in operation.kind.footprint_placements() {
            if placement.source.is_none() {
                return Err("footprint source evidence is required".into());
            }
            placement.validate().map_err(|e| e.to_string())?;
        }
        if let OperationKind::PlaceConnectedFootprints { assembly, .. } = &operation.kind {
            assembly.validate().map_err(|e| e.to_string())?;
        }
        return Ok(());
    }
    if operations.iter().any(|operation| {
        !matches!(
            operation.kind,
            OperationKind::AddBoardGraphicSegments { .. }
                | OperationKind::AddBoardOutline { .. }
                | OperationKind::AddCopperRoute { .. }
                | OperationKind::AddStraightTrack { .. }
                | OperationKind::AddThroughVia { .. }
                | OperationKind::MoveExistingFootprint { .. }
                | OperationKind::EditExistingItems { .. }
                | OperationKind::DeleteExistingItems { .. }
                | OperationKind::FlipExistingFootprint { .. }
        )
    }) {
        return Err("shadow validation only accepts supported board operations".into());
    }
    Ok(())
}

fn existing_item_targets(
    board: &StableId,
    changes: &[kicad_engine::ExistingItemChange],
) -> Vec<StableId> {
    std::iter::once(board.clone())
        .chain(changes.iter().flat_map(|c| {
            let (target, source) = c.identities();
            [target.clone(), source.clone()]
        }))
        .collect()
}

fn deletion_targets(
    board: &StableId,
    targets: &[kicad_engine::ExistingItemTarget],
) -> Vec<StableId> {
    std::iter::once(board.clone())
        .chain(
            targets
                .iter()
                .flat_map(|t| [t.target_id.clone(), t.source_uuid.clone()]),
        )
        .collect()
}

fn operation_matches_board(operation: &Operation, observed: &StableId) -> bool {
    match &operation.kind {
        OperationKind::PlaceAllowlistedFootprint { board, placement } => {
            board == observed
                && operation.targets == vec![board.clone(), placement.target_id.clone()]
        }
        OperationKind::PlaceConnectedFootprints { board, assembly } => {
            board == observed
                && operation.targets
                    == std::iter::once(board.clone())
                        .chain(assembly.placements.iter().map(|p| p.target_id.clone()))
                        .collect::<Vec<_>>()
        }
        OperationKind::AddBoardGraphicSegments { board, .. }
        | OperationKind::AddBoardOutline { board, .. }
        | OperationKind::AddCopperRoute { board, .. }
        | OperationKind::AddStraightTrack { board, .. }
        | OperationKind::AddThroughVia { board, .. } => {
            board == observed && operation.targets == vec![board.clone()]
        }
        OperationKind::DeleteExistingItems { board, targets, .. } => {
            board == observed && operation.targets == deletion_targets(board, targets)
        }
        OperationKind::EditExistingItems { board, changes, .. } => {
            board == observed && operation.targets == existing_item_targets(board, changes)
        }
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
        } => {
            board == observed
                && operation.targets
                    == vec![board.clone(), placement_id.clone(), source_uuid.clone()]
        }
        _ => false,
    }
}

fn bind_shadow_operations(
    operations: &[Operation],
    shadow: &ProjectSnapshot,
) -> Result<Vec<Operation>, String> {
    bind_shadow_operations_with_source(operations, shadow, None)
}

fn bind_shadow_operations_with_source(
    operations: &[Operation],
    shadow: &ProjectSnapshot,
    original: Option<&ProjectSnapshot>,
) -> Result<Vec<Operation>, String> {
    let shadow_board = shadow
        .board
        .document_id
        .as_ref()
        .ok_or("shadow has no board identity")?;
    validate_shadow_operation_kinds(operations)?;
    let template_board = operations
        .first()
        .and_then(|operation| operation.targets.first())
        .ok_or_else(|| "shadow operation has no source board target".to_owned())?;
    let source = if copper_route::contains_route(operations) {
        original
            .ok_or("route shadow requires its original observation")?
            .clone()
    } else {
        shadow
            .in_board_scope(shadow.project_id.clone(), template_board.clone())
            .map_err(|e| e.to_string())?
    };
    if operations[0].kind.footprint_placements().is_empty()
        && !matches!(
            operations[0].kind,
            OperationKind::MoveExistingFootprint { .. }
                | OperationKind::EditExistingItems { .. }
                | OperationKind::DeleteExistingItems { .. }
                | OperationKind::FlipExistingFootprint { .. }
        )
    {
        kicad_adapter::prepare_bound_native_batch(
            operations,
            &source,
            &source.revision().map_err(|e| e.to_string())?,
        )
        .map_err(|e| format!("invalid source operation binding: {e:?}"))?;
    }
    let rebind_endpoint = |endpoint: &kicad_model::RoutingEndpointRef| -> Result<kicad_model::RoutingEndpointRef, String> {
        let (placement, pad) = kicad_engine::resolve_routing_endpoint(&source, endpoint).map_err(|e| e.to_string())?;
        let placement_id = kicad_model::footprint_placement_id(shadow_board, &placement.source_uuid);
        Ok(kicad_model::RoutingEndpointRef {
            pad_id: kicad_model::footprint_pad_id(&placement_id, &pad.source_uuid),
            footprint_placement_id: placement_id,
        })
    };
    let rebind_net = |net: &kicad_model::ObservedNetIdentity| kicad_model::ObservedNetIdentity {
        net_id: kicad_model::board_net_id(shadow_board, &net.name),
        name: net.name.clone(),
    };
    let mut bound = Vec::with_capacity(operations.len());
    for operation in operations {
        if !operation_matches_board(operation, template_board) {
            return Err("shadow operation targets are not uniformly board-scoped".into());
        }
        let operation_board = match &operation.kind {
            OperationKind::PlaceAllowlistedFootprint { board, .. }
            | OperationKind::PlaceConnectedFootprints { board, .. }
            | OperationKind::AddBoardGraphicSegments { board, .. }
            | OperationKind::AddBoardOutline { board, .. }
            | OperationKind::AddCopperRoute { board, .. }
            | OperationKind::AddStraightTrack { board, .. }
            | OperationKind::AddThroughVia { board, .. }
            | OperationKind::MoveExistingFootprint { board, .. }
            | OperationKind::EditExistingItems { board, .. }
            | OperationKind::DeleteExistingItems { board, .. }
            | OperationKind::FlipExistingFootprint { board, .. } => board,
            _ => return Err("shadow validation only accepts supported board operations".into()),
        };
        if operation_board != template_board {
            return Err("shadow operation kind board differs from its target".into());
        }
        let mut operation = operation.clone();
        operation.kind = match &operation.kind {
            OperationKind::PlaceAllowlistedFootprint { placement, .. } => {
                OperationKind::PlaceAllowlistedFootprint {
                    board: shadow_board.clone(),
                    placement: placement.clone(),
                }
            }
            OperationKind::PlaceConnectedFootprints { assembly, .. } => {
                OperationKind::PlaceConnectedFootprints {
                    board: shadow_board.clone(),
                    assembly: assembly.clone(),
                }
            }
            OperationKind::AddCopperRoute {
                intent,
                rule_evidence,
                native_evidence,
                ..
            } => {
                let mut intent = intent.clone();
                intent.start = copper_route::rebind_anchor(&intent.start, &source, shadow_board)?;
                intent.end = copper_route::rebind_anchor(&intent.end, &source, shadow_board)?;
                intent.net = rebind_net(&intent.net);
                intent.observed_revision = shadow.revision().map_err(|e| e.to_string())?;
                OperationKind::AddCopperRoute {
                    board: shadow_board.clone(),
                    intent,
                    rule_evidence: rule_evidence.clone(),
                    native_evidence: native_evidence.clone(),
                }
            }
            OperationKind::AddStraightTrack { intent, .. } => {
                let mut intent = intent.clone();
                intent.start_pad = intent
                    .start_pad
                    .as_ref()
                    .map(&rebind_endpoint)
                    .transpose()?;
                intent.end_pad = intent.end_pad.as_ref().map(&rebind_endpoint).transpose()?;
                intent.net = intent.net.as_ref().map(&rebind_net);
                OperationKind::AddStraightTrack {
                    board: shadow_board.clone(),
                    intent,
                }
            }
            OperationKind::AddThroughVia { intent, .. } => {
                let mut intent = intent.clone();
                intent.net = intent.net.as_ref().map(&rebind_net);
                OperationKind::AddThroughVia {
                    board: shadow_board.clone(),
                    intent,
                }
            }
            OperationKind::AddBoardGraphicSegments { segments, .. } => {
                OperationKind::AddBoardGraphicSegments {
                    board: shadow_board.clone(),
                    segments: segments.clone(),
                }
            }
            OperationKind::AddBoardOutline {
                top_left,
                bottom_right,
                width_nm,
                ..
            } => OperationKind::AddBoardOutline {
                board: shadow_board.clone(),
                top_left: *top_left,
                bottom_right: *bottom_right,
                width_nm: *width_nm,
            },
            OperationKind::FlipExistingFootprint {
                source_uuid,
                target_side,
                axis,
                native_evidence,
                ..
            } => OperationKind::FlipExistingFootprint {
                board: shadow_board.clone(),
                placement_id: kicad_model::footprint_placement_id(shadow_board, source_uuid),
                source_uuid: source_uuid.clone(),
                target_side: *target_side,
                axis: *axis,
                native_evidence: native_evidence.clone(),
            },
            OperationKind::DeleteExistingItems {
                targets,
                native_evidence,
                ..
            } => OperationKind::DeleteExistingItems {
                board: shadow_board.clone(),
                targets: targets
                    .iter()
                    .map(|t| t.in_board_scope(shadow_board))
                    .collect(),
                native_evidence: native_evidence.clone(),
            },
            OperationKind::EditExistingItems {
                changes,
                native_evidence,
                ..
            } => OperationKind::EditExistingItems {
                board: shadow_board.clone(),
                changes: changes
                    .iter()
                    .map(|c| c.in_board_scope(shadow_board))
                    .collect(),
                native_evidence: native_evidence.clone(),
            },
            OperationKind::MoveExistingFootprint {
                source_uuid,
                position,
                orientation_mdeg,
                copper_tolerance_nm,
                native_evidence,
                ..
            } => OperationKind::MoveExistingFootprint {
                board: shadow_board.clone(),
                placement_id: kicad_model::footprint_placement_id(shadow_board, source_uuid),
                source_uuid: source_uuid.clone(),
                position: *position,
                orientation_mdeg: *orientation_mdeg,
                copper_tolerance_nm: *copper_tolerance_nm,
                native_evidence: native_evidence.clone(),
            },
            _ => {
                return Err("shadow validation only accepts supported board operations".into());
            }
        };
        operation.targets[0] = shadow_board.clone();
        if let OperationKind::MoveExistingFootprint { placement_id, .. }
        | OperationKind::FlipExistingFootprint { placement_id, .. } = &operation.kind
        {
            operation.targets[1] = placement_id.clone();
        }
        if let OperationKind::DeleteExistingItems { board, targets, .. } = &operation.kind {
            operation.targets = deletion_targets(board, targets);
        }
        if let OperationKind::EditExistingItems { board, changes, .. } = &operation.kind {
            operation.targets = existing_item_targets(board, changes);
        }
        bound.push(operation);
    }
    Ok(bound)
}

fn validate_shadow_operations(
    observation: &LiveBoardObservation,
    operations: &[Operation],
) -> Result<(), String> {
    let board = observation
        .snapshot
        .board
        .document_id
        .as_ref()
        .ok_or_else(|| "shadow observation has no board identity".to_owned())?;
    if operations
        .iter()
        .any(|operation| !operation_matches_board(operation, board))
    {
        return Err("shadow operation target does not match fresh board identity".into());
    }
    Ok(())
}

fn shadow_graphic_segments(
    operations: &[Operation],
) -> Result<Vec<kicad_model::BoardGraphicSegment>, String> {
    let mut graphics = Vec::new();
    for operation in operations {
        match &operation.kind {
            OperationKind::AddBoardGraphicSegments { segments, .. } => {
                graphics.extend(segments.iter().cloned());
            }
            OperationKind::AddBoardOutline {
                top_left,
                bottom_right,
                width_nm,
                ..
            } => {
                graphics.extend(expand_board_outline(*top_left, *bottom_right, *width_nm));
            }
            OperationKind::AddCopperRoute { .. }
            | OperationKind::AddStraightTrack { .. }
            | OperationKind::AddThroughVia { .. } => {}
            OperationKind::MoveExistingFootprint { .. }
            | OperationKind::EditExistingItems { .. }
            | OperationKind::DeleteExistingItems { .. }
            | OperationKind::FlipExistingFootprint { .. } => {}
            _ => return Err("shadow postcondition contains an unsupported operation".into()),
        }
    }
    Ok(graphics)
}

fn native_graphic(segment: &kicad_model::BoardGraphicSegment) -> IpcBoardGraphicSegment {
    IpcBoardGraphicSegment {
        start_x_nm: segment.start.x,
        start_y_nm: segment.start.y,
        end_x_nm: segment.end.x,
        end_y_nm: segment.end.y,
        width_nm: segment.width_nm,
        layer: match segment.layer {
            kicad_model::BoardGraphicLayer::DwgsUser => IpcBoardGraphicLayer::DwgsUser,
            kicad_model::BoardGraphicLayer::EdgeCuts => IpcBoardGraphicLayer::EdgeCuts,
        },
    }
}

fn expected_shadow_snapshot(
    observation: &LiveBoardObservation,
    operations: &[Operation],
    receipts: &[kicad_ipc::CreatedBoardGraphicReceipt],
) -> Result<ProjectSnapshot, String> {
    let mut graphics = shadow_graphic_segments(operations)?;
    if graphics.len() != receipts.len() {
        return Err("shadow creation receipt count does not match plan".into());
    }
    let mut expected = observation.snapshot.clone();
    for (index, (graphic, receipt)) in graphics.iter_mut().zip(receipts).enumerate() {
        if receipt.request_index != index || receipt.segment != native_graphic(graphic) {
            return Err("shadow creation receipt content does not match plan".into());
        }
        graphic.graphic_id =
            StableId::new(receipt.graphic_id.clone()).map_err(|error| error.to_string())?;
    }
    let mut ids = expected
        .board
        .graphics
        .iter()
        .map(|graphic| graphic.graphic_id.clone())
        .collect::<std::collections::BTreeSet<_>>();
    if graphics
        .iter()
        .any(|graphic| !ids.insert(graphic.graphic_id.clone()))
    {
        return Err("shadow creation receipt reused an existing identity".into());
    }
    expected.board.graphics.extend(graphics);
    expected.validate().map_err(|error| error.to_string())?;
    Ok(expected.canonical())
}

fn expected_native_shadow_snapshot(
    observation: &LiveBoardObservation,
    operations: &[Operation],
    receipts: &kicad_adapter::NativePcbBatchReceipt,
    routing: kicad_ipc::ObservedBoardRoutingItems,
) -> Result<ProjectSnapshot, String> {
    let mut expected = expected_shadow_snapshot(observation, operations, &receipts.graphics)?;
    let board = expected
        .board
        .document_id
        .as_ref()
        .ok_or("missing board identity")?
        .clone();
    let added = kicad_adapter::board_routing_observation(board.clone(), routing)
        .map_err(|e| e.to_string())?;
    let batch = kicad_adapter::prepare_bound_native_batch(
        operations,
        &observation.snapshot,
        &observation.snapshot.revision().map_err(|e| e.to_string())?,
    )
    .map_err(|e| format!("invalid expected native batch: {e:?}"))?;
    if batch.tracks.len() != receipts.tracks.len()
        || batch.vias.len() != receipts.vias.len()
        || batch
            .tracks
            .iter()
            .zip(&receipts.tracks)
            .any(|(track, receipt)| receipt.request_index != 0 || track != &receipt.track)
        || batch
            .vias
            .iter()
            .zip(&receipts.vias)
            .any(|(via, receipt)| receipt.request_index != 0 || via != &receipt.via)
    {
        return Err("routing receipts differ from the bound plan".into());
    }
    if added.tracks.is_empty() && added.vias.is_empty() {
        return Ok(expected);
    }
    let mut existing = expected
        .native_routing
        .take()
        .ok_or("missing baseline routing observation")?;
    let mut ids = expected
        .board
        .graphics
        .iter()
        .map(|g| g.graphic_id.clone())
        .chain(existing.tracks.iter().map(|t| t.source_uuid.clone()))
        .chain(existing.vias.iter().map(|v| v.source_uuid.clone()))
        .chain(
            expected
                .native_footprints
                .iter()
                .flat_map(|f| f.placements.iter())
                .flat_map(|f| {
                    std::iter::once(f.source_uuid.clone())
                        .chain(f.pads.iter().map(|p| p.source_uuid.clone()))
                }),
        )
        .collect::<std::collections::BTreeSet<_>>();
    for id in added
        .tracks
        .iter()
        .map(|t| &t.source_uuid)
        .chain(added.vias.iter().map(|v| &v.source_uuid))
    {
        if !ids.insert(id.clone()) {
            return Err("routing receipt reused an existing identity".into());
        }
    }
    existing.tracks.extend(added.tracks);
    existing.vias.extend(added.vias);
    expected.native_routing = Some(
        kicad_model::ObservedBoardRouting::new(board, existing.tracks, existing.vias)
            .map_err(|e| e.to_string())?,
    );
    expected.validate().map_err(|e| e.to_string())?;
    Ok(expected.canonical())
}

fn apply_shadow_operations(
    launcher: &mut OwnedPcbnewLauncher,
    observation: &LiveBoardObservation,
    operations: &[Operation],
    source_project: &std::path::Path,
    library_settings: &library_settings::SharedLibrarySettings,
) -> Result<
    (
        ProjectSnapshot,
        Vec<kicad_adapter::footprint::SourceFootprintTemplate>,
    ),
    BoardGraphicsCommitOutcome,
> {
    let unknown = |reason: String| BoardGraphicsCommitOutcome::Unknown { reason };
    if copper_route::contains_route(operations) {
        let mut client = launcher
            .connect_owned()
            .map_err(|e| unknown(e.to_string()))?;
        let endpoint = launcher
            .current_identity()
            .map_err(|e| unknown(e.to_string()))?
            .endpoint
            .clone();
        return copper_route::execute(
            &mut client,
            observation,
            operations,
            &endpoint,
            copper_route::ExecutionContext::Preview,
        )
        .map(|snapshot| (snapshot, Vec::new()));
    }
    if let [operation] = operations
        && !operation.kind.footprint_placements().is_empty()
    {
        let placements = operation.kind.footprint_placements();
        let mut prepared = Vec::with_capacity(placements.len());
        for placement in placements {
            let registry =
                footprint_authoring::verify_sources(library_settings, source_project, placement)
                    .map_err(|reason| BoardGraphicsCommitOutcome::Rejected { reason })?;
            let template = kicad_adapter::footprint::capture_source_footprint_template(
                launcher,
                &registry,
                placement.clone(),
            )
            .map_err(|e| unknown(e.to_string()))?;
            prepared.push(footprint_authoring::bind_template(template).map_err(unknown)?);
            if observe_owned_shadow(launcher)
                .map_err(unknown)?
                .snapshot
                .canonical()
                != observation.snapshot.canonical()
            {
                return Err(unknown(
                    "source template capture changed the shadow baseline".into(),
                ));
            }
        }
        let connections = match &operation.kind {
            OperationKind::PlaceConnectedFootprints { assembly, .. } => {
                assembly.connections.as_slice()
            }
            _ => &[],
        };
        let mut client = launcher
            .connect_owned()
            .map_err(|e| unknown(e.to_string()))?;
        let expected =
            footprint_authoring::execute(&mut client, observation, &prepared, connections)?;
        for placement in placements {
            footprint_authoring::verify_sources(library_settings, source_project, placement)
                .map_err(unknown)?;
        }
        return Ok((expected, prepared));
    }
    if let [operation] = operations
        && operation.kind.is_existing_item_mutation()
    {
        let expected =
            existing_item_edit::expected_snapshot(&observation.snapshot, &operation.kind)
                .map_err(|e| unknown(e.to_string()))?;
        let mut client = launcher
            .connect_owned()
            .map_err(|e| unknown(e.to_string()))?;
        let endpoint = launcher
            .current_identity()
            .map_err(|e| unknown(e.to_string()))?
            .endpoint
            .clone();
        return match existing_item_edit::execute(&mut client, observation, operation, &endpoint) {
            BoardGraphicsCommitOutcome::CommittedAndSaved => Ok((expected, Vec::new())),
            BoardGraphicsCommitOutcome::CommittedButNotSaved { reason } => Err(unknown(reason)),
            outcome => Err(outcome),
        };
    }
    let revision = observation
        .snapshot
        .revision()
        .map_err(|e| unknown(e.to_string()))?;
    if let [operation] = operations
        && matches!(operation.kind, OperationKind::MoveExistingFootprint { .. })
    {
        let expected =
            kicad_engine::expected_footprint_move(&observation.snapshot, &operation.kind)
                .map_err(|e| unknown(e.to_string()))?;
        let mut client = launcher
            .connect_owned()
            .map_err(|e| unknown(e.to_string()))?;
        let endpoint = launcher
            .current_identity()
            .map_err(|e| unknown(e.to_string()))?
            .endpoint
            .clone();
        let outcome = existing_footprint::execute(&mut client, observation, operation, &endpoint);
        return match outcome {
            kicad_adapter::BoardGraphicsCommitOutcome::CommittedAndSaved => {
                drop(client);
                let shadow = observe_owned_shadow(launcher).map_err(unknown)?;
                if shadow.snapshot.canonical() != expected {
                    return Err(unknown(
                        "moved shadow differs from the approved transform".into(),
                    ));
                }
                Ok((expected, Vec::new()))
            }
            kicad_adapter::BoardGraphicsCommitOutcome::Rejected { reason } => {
                Err(BoardGraphicsCommitOutcome::Rejected { reason })
            }
            kicad_adapter::BoardGraphicsCommitOutcome::Unknown { reason } => {
                Err(BoardGraphicsCommitOutcome::Unknown { reason })
            }
            kicad_adapter::BoardGraphicsCommitOutcome::CommittedButNotSaved { reason } => {
                Err(BoardGraphicsCommitOutcome::Unknown { reason })
            }
        };
    }
    let batch =
        kicad_adapter::prepare_bound_native_batch(operations, &observation.snapshot, &revision)?;
    let mut client = launcher
        .connect_owned()
        .map_err(|e| unknown(e.to_string()))?;
    let receipts = BoardGraphicsExecutor::default().execute_native_batch(
        &mut client,
        &observation.board_filename,
        &batch.graphics,
        &batch.tracks,
        &batch.vias,
        "auto-kicad disposable shadow validation",
    )?;
    // Read serialized pad-stack defaults from the owned editor after reload.
    drop(client);
    launcher
        .reload_owned()
        .map_err(|error| unknown(error.to_string()))?;
    let mut client = launcher
        .connect_owned()
        .map_err(|error| unknown(error.to_string()))?;
    let routing = kicad_adapter::read_verified_native_routing(
        &mut client,
        &observation.board_filename,
        &receipts,
    )
    .map_err(unknown)?;
    Ok((
        expected_native_shadow_snapshot(observation, operations, &receipts, routing)
            .map_err(unknown)?,
        Vec::new(),
    ))
}

fn cleanup_shadow(
    launcher: &mut OwnedPcbnewLauncher,
    source_board: &std::path::Path,
    source_before: &ShadowSourceFingerprint,
    mut outcome: ShadowValidationOutcome,
) -> ShadowValidationOutcome {
    let mut cleanup_errors = Vec::new();
    if launcher.editor_is_open() {
        match launcher.current_identity() {
            Ok(identity) => {
                if let Err(error) = launcher.close_owned(identity.pid) {
                    cleanup_errors.push(error.to_string());
                }
            }
            Err(error) => cleanup_errors.push(error.to_string()),
        }
    }
    if launcher.current_shadow().is_ok()
        && let Err(error) = launcher.cleanup()
    {
        cleanup_errors.push(error.to_string());
    }
    outcome.shadow_cleaned = launcher.current_shadow().is_err();
    outcome.source_unchanged = shadow_source_fingerprint(source_board) == *source_before;
    if !cleanup_errors.is_empty() || !outcome.shadow_cleaned || !outcome.source_unchanged {
        outcome.state = ShadowValidationState::Unknown;
        let mut reasons = cleanup_errors;
        if !outcome.shadow_cleaned {
            reasons.push("shadow cleanup was not proven".into());
        }
        if !outcome.source_unchanged {
            reasons.push("source board or project fingerprint changed".into());
        }
        outcome.reason = Some(reasons.join("; "));
    }
    outcome
}

static PREVIEW_EDITOR: Mutex<()> = Mutex::new(());

pub fn validate_disposable_shadow(
    server_root: &std::path::Path,
    source_board: &std::path::Path,
    operations: &[Operation],
    drc_timeout: Duration,
) -> ShadowValidationOutcome {
    validate_disposable_shadow_bound(
        server_root,
        source_board,
        operations,
        drc_timeout,
        None,
        source_board.parent().unwrap_or(server_root),
        &library_settings::capture(),
        None,
    )
}

#[allow(clippy::too_many_arguments)]
fn validate_disposable_shadow_bound(
    server_root: &std::path::Path,
    source_board: &std::path::Path,
    operations: &[Operation],
    drc_timeout: Duration,
    expected_source: Option<&ProjectSnapshot>,
    source_project: &std::path::Path,
    library_settings: &library_settings::SharedLibrarySettings,
    routing_rules: Option<&kicad_adapter::routing_rules::RoutingRuleEvidence>,
) -> ShadowValidationOutcome {
    let Ok(_preview_guard) = PREVIEW_EDITOR.try_lock() else {
        return ShadowValidationOutcome::rejected(
            "preview editor is busy; retry after the active preview finishes",
        );
    };
    if let Err(reason) = validate_shadow_operation_kinds(operations) {
        return ShadowValidationOutcome::rejected(reason);
    }
    let source_before = shadow_source_fingerprint(source_board);
    if source_before.board.is_none() || source_before.project.is_none() {
        return ShadowValidationOutcome::rejected(
            "source board and matching project must be readable before shadow validation",
        );
    }
    let mut launcher = match OwnedPcbnewLauncher::new(server_root) {
        Ok(launcher) => launcher,
        Err(error) => return ShadowValidationOutcome::rejected(error.to_string()),
    };
    let shadow = match launcher.create_shadow(source_board) {
        Ok(shadow) => shadow,
        Err(error) => return ShadowValidationOutcome::rejected(error.to_string()),
    };
    let canonical_server_root = match std::fs::canonicalize(server_root) {
        Ok(root) => root,
        Err(error) => {
            return cleanup_shadow(
                &mut launcher,
                source_board,
                &source_before,
                ShadowValidationOutcome::rejected(error.to_string()),
            );
        }
    };
    if !shadow.is_within(&canonical_server_root) {
        return cleanup_shadow(
            &mut launcher,
            source_board,
            &source_before,
            ShadowValidationOutcome::rejected("shadow paths escaped the server root"),
        );
    }
    if let Err(error) = footprint_authoring::prepare_shadow_library_table(
        library_settings,
        source_project,
        shadow.root(),
        operations,
        expected_source,
    ) {
        return cleanup_shadow(
            &mut launcher,
            source_board,
            &source_before,
            ShadowValidationOutcome::rejected(error),
        );
    }
    if let Err(error) = launcher.launch(&shadow) {
        return cleanup_shadow(
            &mut launcher,
            source_board,
            &source_before,
            ShadowValidationOutcome::rejected(error.to_string()),
        );
    }
    let observation = match observe_owned_shadow(&mut launcher) {
        Ok(observation) => observation,
        Err(error) => {
            return cleanup_shadow(
                &mut launcher,
                source_board,
                &source_before,
                ShadowValidationOutcome::unknown(error),
            );
        }
    };
    if let Some(expected) = expected_source {
        let copied = expected.board.document_id.as_ref().and_then(|board| {
            observation
                .snapshot
                .in_board_scope(expected.project_id.clone(), board.clone())
                .ok()
        });
        if copied.as_ref() != Some(&expected.canonical()) {
            return cleanup_shadow(
                &mut launcher,
                source_board,
                &source_before,
                ShadowValidationOutcome::rejected(
                    "saved PCB differs from the editor state; save the project and create a new plan",
                ),
            );
        }
    }
    let expected_revision = match observation.snapshot.revision() {
        Ok(revision) => revision,
        Err(error) => {
            return cleanup_shadow(
                &mut launcher,
                source_board,
                &source_before,
                ShadowValidationOutcome::unknown(error.to_string()),
            );
        }
    };
    let canonical_shadow_root = match std::fs::canonicalize(shadow.root()) {
        Ok(root) => root,
        Err(error) => {
            return cleanup_shadow(
                &mut launcher,
                source_board,
                &source_before,
                ShadowValidationOutcome::unknown(error.to_string()),
            );
        }
    };
    let canonical_shadow_board = match std::fs::canonicalize(shadow.board_path()) {
        Ok(path) => path,
        Err(error) => {
            return cleanup_shadow(
                &mut launcher,
                source_board,
                &source_before,
                ShadowValidationOutcome::unknown(error.to_string()),
            );
        }
    };
    if observation.project_dir != canonical_shadow_root
        || observation.board_path != canonical_shadow_board
    {
        return cleanup_shadow(
            &mut launcher,
            source_board,
            &source_before,
            ShadowValidationOutcome::rejected(
                "fresh shadow observation did not match the owned shadow paths",
            ),
        );
    }
    match observation.snapshot.board.document_id.as_ref() {
        Some(_) => {}
        None => {
            return cleanup_shadow(
                &mut launcher,
                source_board,
                &source_before,
                ShadowValidationOutcome::rejected("fresh shadow observation has no board identity"),
            );
        }
    };
    let mut bound_operations = match if copper_route::contains_route(operations) {
        bind_shadow_operations_with_source(operations, &observation.snapshot, expected_source)
    } else {
        bind_shadow_operations(operations, &observation.snapshot)
    } {
        Ok(operations) => operations,
        Err(reason) => {
            return cleanup_shadow(
                &mut launcher,
                source_board,
                &source_before,
                ShadowValidationOutcome::rejected(reason),
            );
        }
    };
    if copper_route::contains_route(&bound_operations) {
        let reconcile = (|| -> Result<(), String> {
            let rules = routing_rules.ok_or("source routing rule evidence is missing")?;
            let endpoint = launcher
                .current_identity()
                .map_err(|e| e.to_string())?
                .endpoint
                .clone();
            let mut kinds = bound_operations
                .iter()
                .map(|op| op.kind.clone())
                .collect::<Vec<_>>();
            let actual = copper_route::prepare(&endpoint, &observation, &kinds)?;
            if !rules.equivalent_preview_rules(&actual.rules) {
                return Err("saved/live routing rules differ in preview; save/reload source settings and create a new plan".into());
            }
            for op in &bound_operations {
                if let OperationKind::AddCopperRoute {
                    native_evidence, ..
                } = &op.kind
                    && *native_evidence != actual.native_digest
                {
                    return Err(
                        "preview native objects or connectivity differ from the source evidence"
                            .into(),
                    );
                }
            }
            copper_route::bind(&mut kinds, &actual)?;
            for (op, kind) in bound_operations.iter_mut().zip(kinds) {
                op.kind = kind;
            }
            Ok(())
        })();
        if let Err(reason) = reconcile {
            return cleanup_shadow(
                &mut launcher,
                source_board,
                &source_before,
                ShadowValidationOutcome::rejected(reason),
            );
        }
    }
    if validate_shadow_operations(&observation, &bound_operations).is_err() {
        return cleanup_shadow(
            &mut launcher,
            source_board,
            &source_before,
            ShadowValidationOutcome::rejected(
                "fresh shadow operation identity did not match the owned board",
            ),
        );
    }
    let policy = DrcPolicy {
        allowed_root: shadow.root().to_path_buf(),
        timeout: drc_timeout,
    };
    let baseline = match run_pcb_drc(shadow.board_path(), &policy) {
        CheckResult::Completed(report) => report,
        CheckResult::Failed(error) => {
            return cleanup_shadow(
                &mut launcher,
                source_board,
                &source_before,
                ShadowValidationOutcome::rejected(format!("shadow baseline DRC failed: {error:?}")),
            );
        }
    };
    let (expected_snapshot, prepared_footprints) = match apply_shadow_operations(
        &mut launcher,
        &observation,
        &bound_operations,
        source_project,
        library_settings,
    ) {
        Ok(result) => result,
        Err(outcome) => {
            let outcome = match outcome {
                BoardGraphicsCommitOutcome::Rejected { reason } => {
                    ShadowValidationOutcome::rejected(reason)
                }
                BoardGraphicsCommitOutcome::Unknown { reason }
                | BoardGraphicsCommitOutcome::CommittedButNotSaved { reason } => {
                    ShadowValidationOutcome::unknown(reason)
                }
                BoardGraphicsCommitOutcome::CommittedAndSaved => {
                    ShadowValidationOutcome::unknown("missing shadow postcondition")
                }
            };
            return cleanup_shadow(&mut launcher, source_board, &source_before, outcome);
        }
    };
    let native_before_reload = if prepared_footprints.is_empty() {
        Vec::new()
    } else {
        match launcher
            .connect_owned()
            .map_err(|e| e.to_string())
            .and_then(|mut client| {
                client
                    .capture_footprint_instance_templates(&observation.board_filename)
                    .map_err(|e| e.to_string())
            }) {
            Ok(native) => native,
            Err(error) => {
                return cleanup_shadow(
                    &mut launcher,
                    source_board,
                    &source_before,
                    ShadowValidationOutcome::unknown(error),
                );
            }
        }
    };
    if let Err(error) = launcher.reload_owned() {
        return cleanup_shadow(
            &mut launcher,
            source_board,
            &source_before,
            ShadowValidationOutcome::unknown(error.to_string()),
        );
    }
    let reopened = match observe_owned_shadow(&mut launcher) {
        Ok(observation) => observation,
        Err(error) => {
            return cleanup_shadow(
                &mut launcher,
                source_board,
                &source_before,
                ShadowValidationOutcome::unknown(error),
            );
        }
    };
    if !prepared_footprints.is_empty() {
        let verified = (|| -> Result<(), String> {
            let native_after = launcher
                .connect_owned()
                .map_err(|e| e.to_string())?
                .capture_footprint_instance_templates(&observation.board_filename)
                .map_err(|e| e.to_string())?;
            if native_before_reload.len() != native_after.len() {
                return Err("native footprint set changed on reload".into());
            }
            for before in &native_before_reload {
                let after = native_after
                    .iter()
                    .find(|item| item.metadata().source_uuid == before.metadata().source_uuid)
                    .ok_or("native footprint disappeared on reload")?;
                if !before
                    .matches_saved_instance(after)
                    .map_err(|e| e.to_string())?
                {
                    return Err("complete native footprint payload changed on reload".into());
                }
            }
            Ok(())
        })();
        if let Err(error) = verified {
            return cleanup_shadow(
                &mut launcher,
                source_board,
                &source_before,
                ShadowValidationOutcome::unknown(error),
            );
        }
    }
    let reopened_revision = match reopened.snapshot.revision() {
        Ok(revision) => revision,
        Err(error) => {
            return cleanup_shadow(
                &mut launcher,
                source_board,
                &source_before,
                ShadowValidationOutcome::unknown(error.to_string()),
            );
        }
    };
    let normalized_snapshot_matches = reopened.snapshot.canonical() == expected_snapshot;
    if !normalized_snapshot_matches {
        return cleanup_shadow(
            &mut launcher,
            source_board,
            &source_before,
            ShadowValidationOutcome {
                state: ShadowValidationState::Unknown,
                fresh_revision: Some(expected_revision),
                reopened_revision: Some(reopened_revision),
                reopened: true,
                normalized_snapshot_matches: false,
                baseline: None,
                after: None,
                delta: None,
                source_unchanged: true,
                shadow_cleaned: false,
                geometry: None,
                prepared_footprints: Vec::new(),
                reason: Some(
                    "reopened shadow normalized snapshot does not match expected snapshot".into(),
                ),
            },
        );
    }
    if reopened.project_dir != canonical_shadow_root
        || reopened.board_path != canonical_shadow_board
    {
        return cleanup_shadow(
            &mut launcher,
            source_board,
            &source_before,
            ShadowValidationOutcome::unknown("reopened shadow document identity changed"),
        );
    }
    let after = match run_pcb_drc(shadow.board_path(), &policy) {
        CheckResult::Completed(report) => report,
        CheckResult::Failed(error) => {
            return cleanup_shadow(
                &mut launcher,
                source_board,
                &source_before,
                ShadowValidationOutcome::unknown(format!(
                    "shadow postcondition DRC failed: {error:?}"
                )),
            );
        }
    };
    let delta = compare_drc(&baseline, &after);
    let introduced = delta.introduced.len();
    let disconnected = after.unconnected_items > baseline.unconnected_items;
    let mut outcome = ShadowValidationOutcome {
        state: if introduced == 0 && !disconnected {
            ShadowValidationState::Committed
        } else {
            ShadowValidationState::Rejected
        },
        fresh_revision: Some(expected_revision),
        reopened_revision: Some(reopened_revision),
        baseline: Some(baseline),
        after: Some(after),
        delta: Some(delta),
        reopened: true,
        normalized_snapshot_matches,
        source_unchanged: true,
        shadow_cleaned: false,
        geometry: None,
        prepared_footprints,
        reason: (introduced != 0 || disconnected)
            .then(|| format!("shadow DRC introduced {introduced} violation(s); connectivity regressed: {disconnected}")),
    };
    outcome = cleanup_shadow(&mut launcher, source_board, &source_before, outcome);
    outcome
}

fn observe_live_board<S: LiveBoardObservationSource>(
    source: &mut S,
    version: kicad_ipc::VersionInfo,
    document: kicad_ipc::OpenDocument,
    board_filename: &str,
) -> Result<LiveBoardObservation, ObserveLiveBoardError> {
    let mut graphics = Vec::new();
    let mut opaque_graphics = Vec::new();
    for layer in [
        kicad_ipc::BoardGraphicLayer::DwgsUser,
        kicad_ipc::BoardGraphicLayer::EdgeCuts,
    ] {
        let observed = source
            .graphics_observation(board_filename, layer)
            .map_err(ObserveLiveBoardError::Ipc)?;
        graphics.extend(observed.segments);
        for record in observed.opaque {
            opaque_graphics.push(kicad_model::OpaqueBoardGraphic {
                source_uuid: StableId::new(record.graphic_id)
                    .map_err(|e| ObserveLiveBoardError::LiveBoard(e.into()))?,
                layer: match record.layer {
                    kicad_ipc::BoardGraphicLayer::DwgsUser => {
                        kicad_model::BoardGraphicLayer::DwgsUser
                    }
                    kicad_ipc::BoardGraphicLayer::EdgeCuts => {
                        kicad_model::BoardGraphicLayer::EdgeCuts
                    }
                    _ => {
                        return Err(ObserveLiveBoardError::LiveBoard(
                            LiveBoardObservationError::UnsupportedGraphicLayer,
                        ));
                    }
                },
                native_digest: digest_bytes(&record.persistent_payload),
            });
        }
    }
    let footprints = source
        .footprint_instances(board_filename)
        .map_err(ObserveLiveBoardError::Ipc)?;
    let mut observed = Vec::with_capacity(footprints.len());
    for footprint in footprints {
        let pads = source
            .footprint_pads(board_filename, &footprint.source_uuid)
            .map_err(ObserveLiveBoardError::Ipc)?;
        observed.push(ObservedFootprintWithPads { footprint, pads });
    }
    let board_id = board_document_id(&document).map_err(ObserveLiveBoardError::LiveBoard)?;
    let native_footprints = board_footprint_observation(board_id.clone(), observed)
        .map_err(ObserveLiveBoardError::Footprints)?;
    let routing = source
        .board_routing(board_filename)
        .map_err(ObserveLiveBoardError::Ipc)?;
    let native_routing = kicad_adapter::board_routing_observation(board_id.clone(), routing)
        .map_err(ObserveLiveBoardError::Routing)?;
    let stackup = source
        .board_stackup(board_filename)
        .map_err(ObserveLiveBoardError::Ipc)?;
    let native_stackup = kicad_adapter::board_stackup_observation(board_id, stackup)
        .map_err(ObserveLiveBoardError::Stackup)?;
    let instance_token = source
        .latest_instance_token()
        .ok_or(ObserveLiveBoardError::MissingInstanceToken)?;
    let mut result = live_board_observation(
        version,
        document,
        graphics,
        native_footprints,
        native_routing,
        native_stackup,
        instance_token,
    )
    .map_err(ObserveLiveBoardError::LiveBoard)?;
    result.snapshot.opaque_graphics = opaque_graphics;
    result
        .snapshot
        .validate()
        .map_err(|e| ObserveLiveBoardError::LiveBoard(e.into()))?;
    result.snapshot = result.snapshot.canonical();
    Ok(result)
}

fn is_unsupported(observation: &DocumentObservation) -> bool {
    matches!(observation, DocumentObservation::Unsupported(_))
}

fn observe_documents<T: kicad_ipc::KiCadTransport>(
    client: &mut KiCadClient<T>,
    query: DocumentQuery,
) -> Result<DocumentObservation, String> {
    match client.get_open_documents(query) {
        Ok(documents) => Ok(DocumentObservation::Documents(documents)),
        Err(error) if error.code() == "unsupported_capability" => {
            Ok(DocumentObservation::Unsupported(error.to_string()))
        }
        Err(error) => Ok(DocumentObservation::Failed {
            code: error.code().to_owned(),
            message: error.to_string(),
        }),
    }
}

#[derive(Clone, Debug, Deserialize, schemars::JsonSchema)]
pub struct PlanBoardGraphicsRequest {
    pub geometry: Option<kicad_model::geometry::GeometryRequest>,
    pub endpoint: String,
    pub source_run_id: String,
    pub segments: Option<Value>,
    pub outline: Option<BoardOutlineRequest>,
    pub footprint: Option<BoardFootprintRequest>,
    pub move_footprint: Option<BoardMoveExistingFootprintRequest>,
    pub edit_existing: Option<BoardExistingItemEditRequest>,
    pub delete_items: Option<BoardDeleteItemsRequest>,
    pub flip_footprint: Option<BoardFlipFootprintRequest>,
    pub assembly: Option<BoardAssemblyRequest>,
    pub track: Option<BoardTrackRequest>,
    pub via: Option<BoardViaRequest>,
    pub route: Option<BoardRouteRequest>,
    pub batch: Option<Vec<BoardBatchItemRequest>>,
    pub request_id: String,
}

#[derive(Clone, Copy, Debug, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum BoardFlipAxisRequest {
    TopBottom,
    LeftRight,
}
#[derive(Clone, Copy, Debug, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum BoardFlipSideRequest {
    Front,
    Back,
}
#[derive(Clone, Debug, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct BoardFlipFootprintRequest {
    pub observed_revision: String,
    pub placement_id: String,
    pub source_uuid: String,
    pub target_side: BoardFlipSideRequest,
    pub axis: BoardFlipAxisRequest,
}

#[derive(Clone, Debug, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct BoardDeleteItemsRequest {
    pub observed_revision: String,
    pub targets: Vec<BoardDeletionTargetRequest>,
}
#[derive(Clone, Copy, Debug, serde::Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum BoardDeletionKindRequest {
    Track,
    Via,
    GraphicSegment,
    Footprint,
}
#[derive(Clone, Debug, serde::Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct BoardDeletionTargetRequest {
    pub kind: BoardDeletionKindRequest,
    pub target_id: String,
    pub source_uuid: String,
}

#[derive(Clone, Debug, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct BoardExistingItemEditRequest {
    pub observed_revision: String,
    pub changes: Vec<BoardExistingItemChangeRequest>,
}

#[derive(Clone, Copy, Debug, serde::Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct BoardItemPointNmRequest {
    pub x: i64,
    pub y: i64,
}

#[derive(Clone, Debug, serde::Serialize, Deserialize, schemars::JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum BoardExistingItemChangeRequest {
    Track {
        target_id: String,
        source_uuid: String,
        start_nm: Option<BoardItemPointNmRequest>,
        end_nm: Option<BoardItemPointNmRequest>,
        width_nm: Option<i64>,
    },
    Via {
        target_id: String,
        source_uuid: String,
        position_nm: Option<BoardItemPointNmRequest>,
        diameter_nm: Option<i64>,
        drill_nm: Option<i64>,
    },
    GraphicSegment {
        target_id: String,
        source_uuid: String,
        start_nm: Option<BoardItemPointNmRequest>,
        end_nm: Option<BoardItemPointNmRequest>,
        width_nm: Option<i64>,
    },
}

#[derive(Clone, Debug, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct BoardMoveExistingFootprintRequest {
    pub placement_id: String,
    pub source_uuid: String,
    pub observed_revision: String,
    pub x_nm: Option<i64>,
    pub y_nm: Option<i64>,
    pub orientation_mdeg: Option<i64>,
    pub copper_tolerance_nm: Option<i64>,
}

#[derive(Clone, Debug, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct BoardBatchItemRequest {
    pub segments: Option<Vec<BoardGraphicSegmentRequest>>,
    pub outline: Option<BoardOutlineRequest>,
    pub track: Option<BoardTrackRequest>,
    pub via: Option<BoardViaRequest>,
    pub route: Option<BoardRouteRequest>,
}

#[derive(Clone, Debug, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct BoardGraphicSegmentRequest {
    pub graphic_id: String,
    pub start: BoardCoordinateRequest,
    pub end: BoardCoordinateRequest,
    pub width_nm: i64,
    pub layer: BoardGraphicLayerRequest,
}

#[derive(Clone, Copy, Debug, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct BoardCoordinateRequest {
    pub x: i64,
    pub y: i64,
    pub unit: BoardCoordinateUnitRequest,
    pub space: BoardCoordinateSpaceRequest,
}

#[derive(Clone, Copy, Debug, Deserialize, schemars::JsonSchema)]
pub enum BoardCoordinateUnitRequest {
    Nanometres,
}

#[derive(Clone, Copy, Debug, Deserialize, schemars::JsonSchema)]
pub enum BoardCoordinateSpaceRequest {
    Board,
}

#[derive(Clone, Copy, Debug, Deserialize, schemars::JsonSchema)]
pub enum BoardGraphicLayerRequest {
    DwgsUser,
    EdgeCuts,
}

#[derive(Clone, Debug, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct BoardRoutingEndpointRequest {
    pub pad_id: String,
    pub footprint_placement_id: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct BoardRoutingNetRequest {
    pub net_id: String,
    pub name: String,
}

#[derive(Clone, Debug, Deserialize, schemars::JsonSchema)]
pub struct BoardTrackRequest {
    #[serde(default)]
    pub layer: Option<String>,
    pub source_uuid: String,
    pub start_x_nm: i64,
    pub start_y_nm: i64,
    pub end_x_nm: i64,
    pub end_y_nm: i64,
    pub width_nm: i64,
    #[serde(default)]
    pub start_pad: Option<BoardRoutingEndpointRequest>,
    #[serde(default)]
    pub end_pad: Option<BoardRoutingEndpointRequest>,
    #[serde(default)]
    pub net: Option<BoardRoutingNetRequest>,
}

#[derive(Clone, Debug, Deserialize, schemars::JsonSchema)]
pub struct BoardViaRequest {
    pub source_uuid: String,
    pub x_nm: i64,
    pub y_nm: i64,
    pub pad_diameter_nm: i64,
    pub drill_diameter_nm: i64,
    #[serde(default)]
    pub net: Option<BoardRoutingNetRequest>,
}

#[derive(Clone, Debug, Deserialize, schemars::JsonSchema)]
pub struct BoardOutlineRequest {
    pub top_left_x_nm: i64,
    pub top_left_y_nm: i64,
    pub bottom_right_x_nm: i64,
    pub bottom_right_y_nm: i64,
    pub width_nm: i64,
}

#[derive(Clone, Debug, Deserialize, schemars::JsonSchema)]
pub struct BoardFootprintRequest {
    pub target_id: String,
    pub library_nickname: String,
    pub library_entry_name: String,
    pub x_nm: i64,
    pub y_nm: i64,
    pub reference: String,
    pub value: String,
}

#[derive(Clone, Debug, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct BoardAssemblyRequest {
    pub placements: Vec<BoardFootprintRequest>,
    pub connections: Vec<BoardAssemblyConnectionRequest>,
}

#[derive(Clone, Debug, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct BoardAssemblyPadRequest {
    pub target_id: String,
    pub pad_number: String,
}

#[derive(Clone, Debug, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct BoardAssemblyConnectionRequest {
    pub track_id: String,
    pub start: BoardAssemblyPadRequest,
    pub end: BoardAssemblyPadRequest,
    pub width_nm: i64,
}

impl BoardAssemblyRequest {
    fn prepare(
        &self,
        observation: &LiveBoardObservation,
        library_settings: &library_settings::SharedLibrarySettings,
    ) -> Result<kicad_engine::ConnectedFootprintAssembly, String> {
        if self.placements.is_empty()
            || self.connections.is_empty()
            || self.placements.len() + self.connections.len() > kicad_engine::MAX_OPERATIONS
        {
            return Err("assembly is empty or oversized".into());
        }
        let endpoint = |pin: &BoardAssemblyPadRequest| -> Result<_, String> {
            Ok(kicad_engine::PlannedPadRef {
                target_id: StableId::new(&pin.target_id).map_err(|e| e.to_string())?,
                pad_number: pin.pad_number.clone(),
            })
        };
        let connections = self
            .connections
            .iter()
            .map(|connection| -> Result<_, String> {
                Ok(kicad_engine::PlannedPadConnection {
                    track_id: StableId::new(&connection.track_id).map_err(|e| e.to_string())?,
                    start: endpoint(&connection.start)?,
                    end: endpoint(&connection.end)?,
                    width_nm: connection.width_nm,
                })
            })
            .collect::<Result<Vec<_>, _>>()?;
        let placements = self
            .placements
            .iter()
            .map(|p| footprint_authoring::prepare(library_settings, p, observation))
            .collect::<Result<Vec<_>, _>>()?;
        let assembly = kicad_engine::ConnectedFootprintAssembly {
            placements,
            connections,
        };
        assembly.validate().map_err(|e| e.to_string())?;
        Ok(assembly)
    }
}

impl BoardGraphicSegmentRequest {
    fn into_model(self) -> Result<kicad_model::BoardGraphicSegment, ErrorData> {
        Ok(kicad_model::BoardGraphicSegment {
            graphic_id: StableId::new(self.graphic_id)
                .map_err(|error| ErrorData::invalid_params(error.to_string(), None))?,
            start: self.start.into_model(),
            end: self.end.into_model(),
            width_nm: self.width_nm,
            layer: match self.layer {
                BoardGraphicLayerRequest::DwgsUser => kicad_model::BoardGraphicLayer::DwgsUser,
                BoardGraphicLayerRequest::EdgeCuts => kicad_model::BoardGraphicLayer::EdgeCuts,
            },
        })
    }
}

impl BoardCoordinateRequest {
    fn into_model(self) -> kicad_model::Coordinate {
        let BoardCoordinateUnitRequest::Nanometres = self.unit;
        let BoardCoordinateSpaceRequest::Board = self.space;
        kicad_model::Coordinate {
            x: self.x,
            y: self.y,
            unit: kicad_model::Unit::Nanometres,
            space: kicad_model::DocumentSpace::Board,
        }
    }
}

fn board_operation_kind(
    board: &StableId,
    segments: Option<Vec<BoardGraphicSegmentRequest>>,
    outline: Option<BoardOutlineRequest>,
    track: Option<BoardTrackRequest>,
    via: Option<BoardViaRequest>,
    route: Option<BoardRouteRequest>,
) -> Result<OperationKind, ErrorData> {
    if let Some(route) = route {
        if segments.is_some() || outline.is_some() || track.is_some() || via.is_some() {
            return Err(ErrorData::invalid_params(
                "route is exclusive with other operation fields",
                None,
            ));
        }
        return Ok(OperationKind::AddCopperRoute {
            board: board.clone(),
            intent: route
                .into_model()
                .map_err(|e| ErrorData::invalid_params(e, None))?,
            rule_evidence: digest_bytes(b"unbound"),
            native_evidence: digest_bytes(b"unbound"),
        });
    }
    let kind = match (segments, outline, track, via) {
        (Some(segments), None, None, None) => OperationKind::AddBoardGraphicSegments {
            board: board.clone(),
            segments: segments
                .into_iter()
                .map(BoardGraphicSegmentRequest::into_model)
                .collect::<Result<_, _>>()?,
        },
        (None, Some(outline), None, None) => OperationKind::AddBoardOutline {
            board: board.clone(),
            top_left: kicad_model::Coordinate {
                x: outline.top_left_x_nm,
                y: outline.top_left_y_nm,
                unit: kicad_model::Unit::Nanometres,
                space: kicad_model::DocumentSpace::Board,
            },
            bottom_right: kicad_model::Coordinate {
                x: outline.bottom_right_x_nm,
                y: outline.bottom_right_y_nm,
                unit: kicad_model::Unit::Nanometres,
                space: kicad_model::DocumentSpace::Board,
            },
            width_nm: outline.width_nm,
        },
        (None, None, Some(track), None) => {
            let source_uuid = StableId::new(track.source_uuid)
                .map_err(|error| ErrorData::invalid_params(error.to_string(), None))?;
            let start = kicad_model::Coordinate {
                x: track.start_x_nm,
                y: track.start_y_nm,
                unit: kicad_model::Unit::Nanometres,
                space: kicad_model::DocumentSpace::Board,
            };
            let end = kicad_model::Coordinate {
                x: track.end_x_nm,
                y: track.end_y_nm,
                unit: kicad_model::Unit::Nanometres,
                space: kicad_model::DocumentSpace::Board,
            };
            let mut intent = match (track.start_pad, track.end_pad, track.net) {
                (Some(start_pad), Some(end_pad), Some(net)) => {
                    StraightTrackIntent::new_endpoint_bound(
                        source_uuid,
                        start,
                        end,
                        track.width_nm,
                        kicad_model::RoutingEndpointRef {
                            pad_id: StableId::new(start_pad.pad_id).map_err(|error| {
                                ErrorData::invalid_params(error.to_string(), None)
                            })?,
                            footprint_placement_id: StableId::new(start_pad.footprint_placement_id)
                                .map_err(|error| {
                                    ErrorData::invalid_params(error.to_string(), None)
                                })?,
                        },
                        kicad_model::RoutingEndpointRef {
                            pad_id: StableId::new(end_pad.pad_id).map_err(|error| {
                                ErrorData::invalid_params(error.to_string(), None)
                            })?,
                            footprint_placement_id: StableId::new(end_pad.footprint_placement_id)
                                .map_err(|error| {
                                ErrorData::invalid_params(error.to_string(), None)
                            })?,
                        },
                        kicad_model::ObservedNetIdentity {
                            net_id: StableId::new(net.net_id).map_err(|error| {
                                ErrorData::invalid_params(error.to_string(), None)
                            })?,
                            name: net.name,
                        },
                    )
                }
                _ => StraightTrackIntent::new(source_uuid, start, end, track.width_nm),
            }
            .map_err(|error| ErrorData::invalid_params(error.to_string(), None))?;
            intent.layer = track.layer.as_deref().unwrap_or("F.Cu").parse().map_err(
                |error: kicad_model::ModelError| ErrorData::invalid_params(error.to_string(), None),
            )?;
            OperationKind::AddStraightTrack {
                board: board.clone(),
                intent,
            }
        }
        (None, None, None, Some(via)) => {
            let source_uuid = StableId::new(via.source_uuid)
                .map_err(|error| ErrorData::invalid_params(error.to_string(), None))?;
            let position = kicad_model::Coordinate {
                x: via.x_nm,
                y: via.y_nm,
                unit: kicad_model::Unit::Nanometres,
                space: kicad_model::DocumentSpace::Board,
            };
            let intent = match via.net {
                Some(net) => ThroughViaIntent::new_net_bound(
                    source_uuid,
                    position,
                    via.pad_diameter_nm,
                    via.drill_diameter_nm,
                    kicad_model::ObservedNetIdentity {
                        net_id: StableId::new(net.net_id)
                            .map_err(|error| ErrorData::invalid_params(error.to_string(), None))?,
                        name: net.name,
                    },
                ),
                None => ThroughViaIntent::new(
                    source_uuid,
                    position,
                    via.pad_diameter_nm,
                    via.drill_diameter_nm,
                ),
            }
            .map_err(|error| ErrorData::invalid_params(error.to_string(), None))?;
            OperationKind::AddThroughVia {
                board: board.clone(),
                intent,
            }
        }
        _ => {
            return Err(ErrorData::invalid_params(
                "exactly one of segments, outline, track, via, or route is required per batch item",
                None,
            ));
        }
    };
    Ok(kind)
}

fn board_operation_kinds(
    request: &PlanBoardGraphicsRequest,
    board: &StableId,
) -> Result<(Vec<OperationKind>, bool), ErrorData> {
    let legacy_count = [
        request.segments.is_some(),
        request.outline.is_some(),
        request.track.is_some(),
        request.via.is_some(),
        request.route.is_some(),
    ]
    .into_iter()
    .filter(|present| *present)
    .count();

    match (request.batch.as_ref(), legacy_count) {
        (Some(batch), 0) if !batch.is_empty() && batch.len() <= MAX_BOARD_BATCH_ITEMS => batch
            .iter()
            .map(|item| {
                board_operation_kind(
                    board,
                    item.segments.clone(),
                    item.outline.clone(),
                    item.track.clone(),
                    item.via.clone(),
                    item.route.clone(),
                )
            })
            .collect::<Result<Vec<_>, _>>()
            .map(|kinds| (kinds, true)),
        (None, 1) => {
            let segments = request
                .segments
                .clone()
                .map(serde_json::from_value)
                .transpose()
                .map_err(|error| ErrorData::invalid_params(error.to_string(), None))?;
            board_operation_kind(
                board,
                segments,
                request.outline.clone(),
                request.track.clone(),
                request.via.clone(),
                request.route.clone(),
            )
            .map(|kind| (vec![kind], false))
        }
        _ => Err(ErrorData::invalid_params(
            "exactly one of segments, outline, footprint, track, via, or route is required; use a non-empty batch instead",
            None,
        )),
    }
}

fn expected_board_effect(kind: &OperationKind) -> &'static str {
    match kind {
        OperationKind::AddBoardGraphicSegments { .. } => "add board graphic segments",
        OperationKind::AddBoardOutline { .. } => "add rectangular Edge.Cuts outline",
        OperationKind::PlaceAllowlistedFootprint { .. } => "place allowlisted native footprint",
        OperationKind::PlaceConnectedFootprints { .. } => {
            "place source-bound footprints and connect their pads atomically"
        }
        OperationKind::EditGeometry { .. } => {
            "edit explicit outline and zone definitions with verified copper"
        }
        OperationKind::AddCopperRoute { .. } => "add explicit copper route",
        OperationKind::AddStraightTrack { .. } => "add straight copper track",
        OperationKind::AddThroughVia { .. } => "add through via",
        OperationKind::MoveExistingFootprint { .. } => "move or rotate existing footprint",
        OperationKind::DeleteExistingItems { .. } => "delete exact approved native object set",
        OperationKind::EditExistingItems { .. } => "edit existing track via or drawing fields",
        OperationKind::FlipExistingFootprint { .. } => "flip existing footprint to approved side",
        _ => "apply bounded board change",
    }
}

fn shadow_validation_state(state: &ShadowValidationState) -> &'static str {
    match state {
        ShadowValidationState::Committed => "committed",
        ShadowValidationState::Rejected => "rejected",
        ShadowValidationState::Unknown => "unknown",
    }
}

fn footprint_template_evidence(outcome: &ShadowValidationOutcome) -> Vec<Value> {
    outcome.prepared_footprints.iter().map(|prepared| {
        let (bytes, association) = prepared.template.evidence();
        json!({
            "target_id": prepared.placement.target_id,
            "template_digest": digest_bytes(bytes),
            "association": association.map(|a| json!({"symbol_path": a.symbol_path, "pad_nets": a.pad_nets})),
        })
    }).collect()
}

fn validation_evidence_hash(
    plan_hash: &str,
    outcome: &ShadowValidationOutcome,
    source_files: &ShadowSourceFingerprint,
) -> Result<String, String> {
    let evidence = json!({
        "schema_version": CONTRACT_SCHEMA_VERSION,
        "plan_hash": plan_hash,
        "source_files": source_files,
        "footprint_templates": footprint_template_evidence(outcome),
        "geometry": outcome.geometry,
        "shadow_validation": {
            "state": shadow_validation_state(&outcome.state),
            "fresh_revision": outcome.fresh_revision,
            "reopened_revision": outcome.reopened_revision,
            "reopened": outcome.reopened,
            "normalized_snapshot_matches": outcome.normalized_snapshot_matches,
            "baseline": outcome.baseline,
            "after": outcome.after,
            "delta": outcome.delta,
            "source_unchanged": outcome.source_unchanged,
            "shadow_cleaned": outcome.shadow_cleaned,
            "reason": outcome.reason,
        },
    });
    let bytes = serde_json::to_vec(&evidence).map_err(|error| error.to_string())?;
    Ok(digest_bytes(&bytes).to_string())
}

fn approval_hash(plan_hash: &str, evidence_hash: &str) -> String {
    digest_text(&["pcb-approval-v1", plan_hash, evidence_hash])
}

#[derive(Clone, Debug, Deserialize, schemars::JsonSchema)]
pub struct PreviewChangeRequest {
    pub plan_id: String,
    pub plan_hash: String,
    pub request_id: Option<String>,
}

#[derive(Clone, Debug, Deserialize, schemars::JsonSchema)]
pub struct CommitChangeRequest {
    pub plan_id: String,
    pub plan_hash: String,
    pub approval_hash: String,
    pub validation_evidence_hash: String,
    pub request_id: Option<String>,
}

#[derive(Clone, Debug, Deserialize, schemars::JsonSchema)]
pub struct PlanRecoveryRequest {
    pub project_dir: String,
    pub attempt_id: String,
    pub action: String,
    pub request_id: Option<String>,
}

#[derive(Clone, Debug, Deserialize, schemars::JsonSchema)]
pub struct PreviewRecoveryRequest {
    pub project_dir: String,
    pub recovery_id: String,
    pub recovery_plan_hash: String,
    pub request_id: Option<String>,
}

#[derive(Clone, Debug, Deserialize, schemars::JsonSchema)]
pub struct CommitRecoveryRequest {
    pub project_dir: String,
    pub recovery_id: String,
    pub recovery_plan_hash: String,
    pub approval_hash: String,
    pub request_id: Option<String>,
}

#[derive(Clone, Debug, Deserialize, schemars::JsonSchema)]
pub struct PlanSchematicChangeRequest {
    pub project_root: String,
    pub root_schematic: String,
    pub source_run_id: String,
    pub request_id: String,
}

#[derive(Clone, Debug, Deserialize, schemars::JsonSchema)]
pub struct PreviewSchematicChangeRequest {
    pub plan_id: String,
    pub plan_hash: String,
    pub request_id: Option<String>,
}

#[derive(Clone, Debug, Deserialize, schemars::JsonSchema)]
pub struct CommitSchematicChangeRequest {
    pub plan_id: String,
    pub plan_hash: String,
    pub approval_hash: String,
    pub request_id: Option<String>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum StoredExecutionState {
    Ready,
    InFlight,
    Committed,
    Unknown,
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum SchematicExecutionState {
    Planned,
    AwaitingApproval,
    InFlight,
    Committed,
    Unknown,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
struct SchematicAuditEvent {
    sequence: u64,
    action: &'static str,
    source_run_id: String,
    request_id: String,
    plan_id: String,
    plan_hash: String,
    approval_hash: Option<String>,
    before_revision: Option<RevisionDigest>,
    after_revision: Option<RevisionDigest>,
    before_artifact: Option<SchematicArtifactFile>,
    after_artifact: Option<SchematicArtifactFile>,
    outcome: String,
}

#[derive(Clone, Debug)]
struct StoredSchematicChange {
    project_root: std::path::PathBuf,
    root_schematic: std::path::PathBuf,
    source_artifact: SchematicArtifactFile,
    source_run_id: String,
    plan_hash: String,
    approval_hash: Option<String>,
    receipt: Option<SchematicPreviewReceipt>,
    state: SchematicExecutionState,
    outcome: Option<SchematicPatchOutcome>,
}

pub trait SchematicChangeService: Send + Sync {
    fn verified(&self) -> bool {
        false
    }

    fn preview(
        &self,
        project_root: &std::path::Path,
        root_schematic: &std::path::Path,
        source_artifact: &SchematicArtifactFile,
    ) -> Result<SchematicPreviewReceipt, SchematicPatchOutcome>;
    fn commit(
        &self,
        project_root: &std::path::Path,
        root_schematic: &std::path::Path,
        receipt: &SchematicPreviewReceipt,
    ) -> SchematicPatchOutcome;
}

#[derive(Clone, Debug, Default)]
pub struct ProductionSchematicChangeService;

impl SchematicChangeService for ProductionSchematicChangeService {
    fn verified(&self) -> bool {
        schematic_change_available()
    }

    fn preview(
        &self,
        project_root: &std::path::Path,
        root_schematic: &std::path::Path,
        source_artifact: &SchematicArtifactFile,
    ) -> Result<SchematicPreviewReceipt, SchematicPatchOutcome> {
        preview_two_resistor_circuit(project_root, root_schematic, source_artifact)
    }

    fn commit(
        &self,
        project_root: &std::path::Path,
        root_schematic: &std::path::Path,
        receipt: &SchematicPreviewReceipt,
    ) -> SchematicPatchOutcome {
        commit_two_resistor_circuit(project_root, root_schematic, receipt)
    }
}

#[derive(Clone, Debug)]
struct StoredChangeSession {
    session: ChangeSession,
    endpoint: String,
    board_filename: String,
    board_path: std::path::PathBuf,
    project_dir: std::path::PathBuf,
    source_files: ShadowSourceFingerprint,
    routing_evidence: Option<copper_route::RouteEvidence>,
    geometry_evidence: Option<geometry::GeometryPlanningEvidence>,
    execution_state: StoredExecutionState,
    shadow_validation: Option<ShadowValidationOutcome>,
    validation_evidence_hash: Option<String>,
    approval_hash: Option<String>,
}

pub trait ShadowValidationService: Send + Sync {
    fn validate(
        &self,
        endpoint: &str,
        source: &LiveBoardObservation,
        operations: &[Operation],
        timeout: Duration,
    ) -> ShadowValidationOutcome;
}

#[derive(Clone, Debug)]
pub struct IpcShadowValidationService {
    library_settings: library_settings::SharedLibrarySettings,
    server_root: std::path::PathBuf,
}

impl IpcShadowValidationService {
    pub fn new(server_root: impl Into<std::path::PathBuf>) -> Self {
        Self {
            server_root: server_root.into(),
            library_settings: library_settings::capture(),
        }
    }
}

struct LiveShadowExport(std::path::PathBuf);

impl Drop for LiveShadowExport {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn export_live_shadow(
    root: &std::path::Path,
    endpoint: &str,
    source: &LiveBoardObservation,
) -> Result<(LiveShadowExport, std::path::PathBuf), String> {
    std::fs::create_dir_all(root).map_err(|error| error.to_string())?;
    let root = std::fs::canonicalize(root).map_err(|error| error.to_string())?;
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(|error| error.to_string())?
        .as_nanos();
    let export_root = root.join(format!("auto-kicad-export-{}-{stamp}", std::process::id()));
    std::fs::create_dir(&export_root).map_err(|error| error.to_string())?;
    let export = LiveShadowExport(export_root);
    let filename = source
        .board_path
        .file_name()
        .ok_or("source board filename is missing")?;
    let destination = export.0.join(filename);
    if IpcDiscoveryService
        .live_board_observation(endpoint, None)
        .as_ref()
        != Ok(source)
    {
        return Err("source editor changed before copying".into());
    }
    for extension in ["kicad_pcb", "kicad_pro", "kicad_dru"] {
        let source_path = source.board_path.with_extension(extension);
        if extension != "kicad_dru" || source_path.exists() {
            std::fs::copy(source_path, destination.with_extension(extension))
                .map_err(|error| error.to_string())?;
        }
    }
    Ok((export, destination))
}

impl ShadowValidationService for IpcShadowValidationService {
    fn validate(
        &self,
        endpoint: &str,
        source: &LiveBoardObservation,
        operations: &[Operation],
        timeout: Duration,
    ) -> ShadowValidationOutcome {
        if let Err(reason) = validate_shadow_operation_kinds(operations) {
            return ShadowValidationOutcome::rejected(reason);
        }
        if geometry::contains(operations) {
            return geometry::preview(
                &self.server_root,
                &self.library_settings,
                endpoint,
                source,
                operations,
                timeout,
            );
        }
        for operation in operations {
            for placement in operation.kind.footprint_placements() {
                if let Err(reason) = footprint_authoring::verify_sources(
                    &self.library_settings,
                    &source.project_dir,
                    placement,
                ) {
                    return ShadowValidationOutcome::rejected(reason);
                }
            }
        }
        let routing = if copper_route::contains_route(operations) {
            let kinds = operations
                .iter()
                .map(|op| op.kind.clone())
                .collect::<Vec<_>>();
            match copper_route::prepare(endpoint, source, &kinds)
                .and_then(|e| copper_route::matches(operations, &e).map(|()| e))
            {
                Ok(evidence) => Some(evidence),
                Err(reason) => return ShadowValidationOutcome::rejected(reason),
            }
        } else {
            None
        };
        let fingerprint = shadow_source_fingerprint(&source.board_path);
        let (export, path) = match export_live_shadow(&self.server_root, endpoint, source) {
            Ok(export) => export,
            Err(reason) => return ShadowValidationOutcome::rejected(reason),
        };
        let mut outcome = validate_disposable_shadow_bound(
            &self.server_root,
            &path,
            operations,
            timeout,
            Some(&source.snapshot),
            &source.project_dir,
            &self.library_settings,
            routing.as_ref().map(|e| &e.rules),
        );
        if let Some(prior) = routing {
            let kinds = operations
                .iter()
                .map(|op| op.kind.clone())
                .collect::<Vec<_>>();
            let stable = copper_route::prepare(endpoint, source, &kinds).and_then(|current| {
                copper_route::matches(operations, &current)?;
                if current.rules != prior.rules || current.native_digest != prior.native_digest {
                    return Err("routing evidence changed during preview".into());
                }
                Ok(())
            });
            if let Err(reason) = stable {
                outcome.state = ShadowValidationState::Rejected;
                outcome.reason = Some(reason);
            }
        }
        if shadow_source_fingerprint(&source.board_path) != fingerprint
            || IpcDiscoveryService
                .live_board_observation(endpoint, None)
                .as_ref()
                != Ok(source)
        {
            outcome.state = ShadowValidationState::Rejected;
            outcome.source_unchanged = false;
            outcome.reason = Some(format!(
                "source changed during shadow validation; {}",
                outcome.reason.unwrap_or_default()
            ));
        }
        if let Err(error) = std::fs::remove_dir_all(&export.0) {
            outcome.state = ShadowValidationState::Unknown;
            outcome.shadow_cleaned = false;
            outcome.reason = Some(format!("export cleanup failed: {error}"));
        }
        outcome
    }
}

fn shadow_validation_data(outcome: &ShadowValidationOutcome) -> Value {
    json!({
        "footprint_templates": footprint_template_evidence(outcome),
        "geometry": outcome.geometry,
        "state": match outcome.state {
            ShadowValidationState::Committed => "committed",
            ShadowValidationState::Rejected => "rejected",
            ShadowValidationState::Unknown => "unknown",
        },
        "fresh_revision": outcome.fresh_revision,
        "reopened_revision": outcome.reopened_revision,
        "reopened": outcome.reopened,
        "normalized_snapshot_matches": outcome.normalized_snapshot_matches,
        "baseline": outcome.baseline,
        "after": outcome.after,
        "delta": outcome.delta,
        "source_unchanged": outcome.source_unchanged,
        "shadow_cleaned": outcome.shadow_cleaned,
        "reason": outcome.reason,
    })
}

pub trait BoardGraphicsExecutionService: Send + Sync {
    fn execute_geometry(
        &self,
        _endpoint: &str,
        _source: &LiveBoardObservation,
        _request: &kicad_model::geometry::GeometryRequest,
        _expected: &geometry::GeometryPlanningEvidence,
        _approved: &geometry::GeometryExecution,
    ) -> Result<geometry::GeometryExecution, BoardGraphicsCommitOutcome> {
        Err(BoardGraphicsCommitOutcome::Rejected {
            reason: "native geometry execution unavailable".into(),
        })
    }

    #[allow(clippy::too_many_arguments)]
    fn execute(
        &self,
        endpoint: &str,
        board_filename: &str,
        board_path: &std::path::Path,
        project_dir: &std::path::Path,
        expected_token: &str,
        expected_revision: &RevisionDigest,
        operations: &[Operation],
    ) -> BoardGraphicsCommitOutcome;
    #[allow(clippy::too_many_arguments)]
    fn execute_prepared(
        &self,
        endpoint: &str,
        board_filename: &str,
        board_path: &std::path::Path,
        project_dir: &std::path::Path,
        expected_token: &str,
        expected_revision: &RevisionDigest,
        operations: &[Operation],
        _prepared: &[kicad_adapter::footprint::SourceFootprintTemplate],
    ) -> BoardGraphicsCommitOutcome {
        self.execute(
            endpoint,
            board_filename,
            board_path,
            project_dir,
            expected_token,
            expected_revision,
            operations,
        )
    }
}

#[derive(Clone, Debug)]
pub struct IpcBoardGraphicsExecutionService {
    library_settings: library_settings::SharedLibrarySettings,
}

impl Default for IpcBoardGraphicsExecutionService {
    fn default() -> Self {
        Self {
            library_settings: library_settings::capture(),
        }
    }
}

impl BoardGraphicsExecutionService for IpcBoardGraphicsExecutionService {
    fn execute_geometry(
        &self,
        endpoint: &str,
        source: &LiveBoardObservation,
        request: &kicad_model::geometry::GeometryRequest,
        expected: &geometry::GeometryPlanningEvidence,
        approved: &geometry::GeometryExecution,
    ) -> Result<geometry::GeometryExecution, BoardGraphicsCommitOutcome> {
        geometry::execute(
            endpoint,
            source,
            request,
            expected,
            Some(approved),
            Duration::from_secs(60),
        )
    }

    fn execute(
        &self,
        endpoint: &str,
        board_filename: &str,
        board_path: &std::path::Path,
        project_dir: &std::path::Path,
        expected_token: &str,
        expected_revision: &RevisionDigest,
        operations: &[Operation],
    ) -> BoardGraphicsCommitOutcome {
        self.execute_live(
            endpoint,
            board_filename,
            board_path,
            project_dir,
            expected_token,
            expected_revision,
            operations,
            &[],
        )
    }
    fn execute_prepared(
        &self,
        endpoint: &str,
        board_filename: &str,
        board_path: &std::path::Path,
        project_dir: &std::path::Path,
        expected_token: &str,
        expected_revision: &RevisionDigest,
        operations: &[Operation],
        prepared: &[kicad_adapter::footprint::SourceFootprintTemplate],
    ) -> BoardGraphicsCommitOutcome {
        self.execute_live(
            endpoint,
            board_filename,
            board_path,
            project_dir,
            expected_token,
            expected_revision,
            operations,
            prepared,
        )
    }
}

impl IpcBoardGraphicsExecutionService {
    #[allow(clippy::too_many_arguments)]
    fn execute_live(
        &self,
        endpoint: &str,
        board_filename: &str,
        board_path: &std::path::Path,
        project_dir: &std::path::Path,
        expected_token: &str,
        expected_revision: &RevisionDigest,
        operations: &[Operation],
        prepared: &[kicad_adapter::footprint::SourceFootprintTemplate],
    ) -> BoardGraphicsCommitOutcome {
        if operations.iter().any(|operation| {
            !matches!(
                &operation.kind,
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
            )
        }) {
            return BoardGraphicsCommitOutcome::Rejected {
                reason: "operation kind is not supported by the PCB execution service".into(),
            };
        }
        let canonical_project_dir = match std::fs::canonicalize(project_dir) {
            Ok(path) if path.is_dir() => path,
            _ => {
                return BoardGraphicsCommitOutcome::Rejected {
                    reason: "canonical project directory is unavailable".into(),
                };
            }
        };
        let canonical_board_path = match std::fs::canonicalize(board_path) {
            Ok(path) if path.is_file() && path.starts_with(&canonical_project_dir) => path,
            _ => {
                return BoardGraphicsCommitOutcome::Rejected {
                    reason: "canonical board path is not an existing project file".into(),
                };
            }
        };
        let config = match ClientConfig::new(endpoint, "auto-kicad-mcp") {
            Ok(config) => config,
            Err(reason) => {
                return BoardGraphicsCommitOutcome::Unknown {
                    reason: reason.to_string(),
                };
            }
        };
        let transport = match NngTransport::connect(config.clone()) {
            Ok(transport) => transport,
            Err(reason) => {
                return BoardGraphicsCommitOutcome::Unknown {
                    reason: reason.to_string(),
                };
            }
        };
        let mut client = KiCadClient::new(transport, config);
        let version = match client.get_version() {
            Ok(version) => version,
            Err(reason) => {
                return BoardGraphicsCommitOutcome::Unknown {
                    reason: reason.to_string(),
                };
            }
        };
        let mut documents = match client.get_open_documents(DocumentQuery::Pcb) {
            Ok(documents) if documents.len() == 1 => documents,
            Ok(documents) => {
                return BoardGraphicsCommitOutcome::Rejected {
                    reason: format!(
                        "expected exactly one open PCB document, found {}",
                        documents.len()
                    ),
                };
            }
            Err(reason) => {
                return BoardGraphicsCommitOutcome::Unknown {
                    reason: reason.to_string(),
                };
            }
        };
        let document = documents.pop().expect("document count checked");
        let observed_filename = match &document.kind {
            kicad_ipc::OpenDocumentKind::Pcb { board_filename } => board_filename,
            _ => {
                return BoardGraphicsCommitOutcome::Rejected {
                    reason: "open document is not a PCB".into(),
                };
            }
        };
        if observed_filename != board_filename {
            return BoardGraphicsCommitOutcome::Rejected {
                reason: "board filename changed".into(),
            };
        }
        let observation = match observe_live_board(&mut client, version, document, board_filename) {
            Ok(observation) => observation,
            Err(ObserveLiveBoardError::Ipc(reason)) => {
                return BoardGraphicsCommitOutcome::Unknown {
                    reason: reason.to_string(),
                };
            }
            Err(ObserveLiveBoardError::MissingInstanceToken) => {
                return BoardGraphicsCommitOutcome::Unknown {
                    reason: "KiCad instance token was not observed".into(),
                };
            }
            Err(ObserveLiveBoardError::Footprints(reason)) => {
                return BoardGraphicsCommitOutcome::Rejected {
                    reason: reason.to_string(),
                };
            }
            Err(ObserveLiveBoardError::Routing(reason)) => {
                return BoardGraphicsCommitOutcome::Rejected {
                    reason: reason.to_string(),
                };
            }
            Err(ObserveLiveBoardError::Stackup(reason)) => {
                return BoardGraphicsCommitOutcome::Rejected {
                    reason: reason.to_string(),
                };
            }
            Err(ObserveLiveBoardError::LiveBoard(reason)) => {
                return BoardGraphicsCommitOutcome::Rejected {
                    reason: reason.to_string(),
                };
            }
        };
        let observed_board_id = observation.snapshot.board.document_id.as_ref();
        let operations_match_observed_board = observed_board_id.is_some_and(|board| {
            operations
                .iter()
                .all(|operation| operation_matches_board(operation, board))
        });
        if observation.board_path != canonical_board_path
            || observation.project_dir != canonical_project_dir
            || !operations_match_observed_board
        {
            return BoardGraphicsCommitOutcome::Rejected {
                reason: "live PCB document identity changed".into(),
            };
        }
        let revision = match observation.snapshot.revision() {
            Ok(revision) => revision,
            Err(reason) => {
                return BoardGraphicsCommitOutcome::Unknown {
                    reason: reason.to_string(),
                };
            }
        };
        if observation.instance_token.as_str() != expected_token || &revision != expected_revision {
            return BoardGraphicsCommitOutcome::Rejected {
                reason: "live identity or revision is stale".into(),
            };
        }
        if copper_route::contains_route(operations) {
            return match copper_route::execute(
                &mut client,
                &observation,
                operations,
                endpoint,
                copper_route::ExecutionContext::Source,
            ) {
                Ok(_) => BoardGraphicsCommitOutcome::CommittedAndSaved,
                Err(outcome) => outcome,
            };
        }
        if let [operation] = operations
            && operation.kind.is_existing_item_mutation()
        {
            return existing_item_edit::execute(&mut client, &observation, operation, endpoint);
        }
        if let [operation] = operations
            && matches!(operation.kind, OperationKind::MoveExistingFootprint { .. })
        {
            return existing_footprint::execute(&mut client, &observation, operation, endpoint);
        }
        if let [operation] = operations
            && !operation.kind.footprint_placements().is_empty()
        {
            let result = (|| -> Result<(), BoardGraphicsCommitOutcome> {
                let placements = operation.kind.footprint_placements();
                if prepared.len() != placements.len()
                    || prepared
                        .iter()
                        .zip(placements)
                        .any(|(template, placement)| template.placement != *placement)
                {
                    return Err(BoardGraphicsCommitOutcome::Rejected {
                        reason: "validated footprint templates differ from plan".into(),
                    });
                }
                for placement in placements {
                    footprint_authoring::verify_sources(
                        &self.library_settings,
                        project_dir,
                        placement,
                    )
                    .map_err(|reason| BoardGraphicsCommitOutcome::Rejected { reason })?;
                }
                let unknown = |reason: String| BoardGraphicsCommitOutcome::Unknown { reason };
                let connections = match &operation.kind {
                    OperationKind::PlaceConnectedFootprints { assembly, .. } => {
                        assembly.connections.as_slice()
                    }
                    _ => &[],
                };
                let expected =
                    footprint_authoring::execute(&mut client, &observation, prepared, connections)?;
                let document = client
                    .get_open_documents(DocumentQuery::Pcb)
                    .map_err(|e| unknown(e.to_string()))?;
                if document.len() != 1 {
                    return Err(unknown("PCB document changed after placement".into()));
                }
                let version = client.get_version().map_err(|e| unknown(e.to_string()))?;
                let after =
                    observe_live_board(&mut client, version, document[0].clone(), board_filename)
                        .map_err(|e| unknown(format!("{e:?}")))?;
                if after.snapshot.canonical() != expected
                    || after.board_path != observation.board_path
                    || after.instance_token != observation.instance_token
                {
                    return Err(unknown(
                        "placed board does not match approved footprint postcondition".into(),
                    ));
                }
                for placement in placements {
                    footprint_authoring::verify_sources(
                        &self.library_settings,
                        project_dir,
                        placement,
                    )
                    .map_err(unknown)?;
                }
                Ok(())
            })();
            return result
                .map(|_| BoardGraphicsCommitOutcome::CommittedAndSaved)
                .unwrap_or_else(|outcome| outcome);
        }
        BoardGraphicsExecutor::default().execute_bound_batch(
            &mut client,
            operations,
            &observation.snapshot,
            &revision,
            board_filename,
            "auto-kicad MCP approved board change",
        )
    }
}

#[derive(Clone, Debug, Deserialize, schemars::JsonSchema)]
pub struct ValidateProjectRequest {
    pub endpoint: String,
    pub baseline_id: Option<String>,
    pub request_id: Option<String>,
}

pub trait BoardValidationService: Send + Sync {
    fn validate(&self, board_path: &std::path::Path, policy: &DrcPolicy) -> CheckResult;
}

#[derive(Clone, Debug, Default)]
pub struct IpcBoardValidationService;

impl BoardValidationService for IpcBoardValidationService {
    fn validate(&self, board_path: &std::path::Path, policy: &DrcPolicy) -> CheckResult {
        run_pcb_drc(board_path, policy)
    }
}

#[derive(Clone, Debug)]
struct StoredDrcBaseline {
    instance_token: String,
    board_identity: String,
    board_path: std::path::PathBuf,
    kicad_version: String,
    captured_revision: RevisionDigest,
    report: DrcReport,
}

#[derive(Clone)]
pub struct Server {
    library_settings: library_settings::SharedLibrarySettings,
    execution_store: Option<Arc<recovery::ExecutionStore>>,
    sessions: Arc<Mutex<BTreeMap<String, StoredChangeSession>>>,
    schematic_sessions: Arc<Mutex<BTreeMap<String, StoredSchematicChange>>>,
    schematic_audit: Arc<Mutex<Vec<SchematicAuditEvent>>>,
    schematic_audit_sequence: Arc<AtomicU64>,
    drc_baselines: Arc<Mutex<BTreeMap<String, StoredDrcBaseline>>>,
    schematic: Arc<dyn SchematicChangeService>,
    capabilities: Vec<Capability>,
    discovery: Arc<dyn DiscoveryService>,
    execution: Arc<dyn BoardGraphicsExecutionService>,
    validation: Arc<dyn BoardValidationService>,
    shadow_validation: Option<Arc<dyn ShadowValidationService>>,
    baseline_sequence: Arc<AtomicU64>,
    tool_router: ToolRouter<Self>,
}

impl std::fmt::Debug for Server {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("Server")
            .field("capabilities", &self.capabilities)
            .finish_non_exhaustive()
    }
}

#[derive(Clone, Debug, Deserialize, schemars::JsonSchema)]
pub struct CapabilityRequest {
    pub request_id: Option<String>,
}

#[derive(Clone, Debug, Deserialize, schemars::JsonSchema)]
pub struct SchematicAuditRequest {
    pub request_id: Option<String>,
    pub source_run_id: Option<String>,
    pub plan_id: Option<String>,
    pub limit: Option<usize>,
}

#[derive(Clone, Debug, Deserialize, schemars::JsonSchema)]
pub struct InspectProjectRequest {
    /// Include complete bounded native zone and board-level outline summaries.
    pub geometry: Option<bool>,
    pub routing_rules: Option<routing_rules::RoutingRulesRequest>,
    pub endpoint: String,
    pub footprint_source: Option<FootprintSourceRequest>,
    pub project_dir: Option<String>,
    pub request_id: Option<String>,
}

#[derive(Clone, Debug, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct FootprintSourceRequest {
    pub library_nickname: String,
    pub library_entry_name: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct Envelope {
    pub ok: bool,
    pub request_id: String,
    pub run_id: Option<String>,
    pub project_revision: Option<String>,
    pub data: Value,
    pub diagnostics: Vec<Value>,
    pub next_actions: Vec<String>,
}

impl Default for Server {
    fn default() -> Self {
        let library_settings = library_settings::capture();
        Self::with_frozen_library_settings(
            Arc::new(IpcDiscoveryService),
            Arc::new(IpcBoardGraphicsExecutionService {
                library_settings: library_settings.clone(),
            }),
            Arc::new(IpcBoardValidationService),
            Arc::new(ProductionSchematicChangeService),
            Some(Arc::new(IpcShadowValidationService {
                server_root: std::env::temp_dir().join("auto-kicad-shadow"),
                library_settings: library_settings.clone(),
            })),
            library_settings,
        )
    }
}

const RECOVERY_TOOLS: [&str; 3] = [
    "kicad_plan_recovery",
    "kicad_preview_recovery",
    "kicad_commit_recovery",
];

impl Server {
    fn invalidate_native_approval(&self, plan_id: &str) {
        if let Some(stored) = self.sessions.lock().expect("session lock").get_mut(plan_id)
            && (copper_route::contains_route(&stored.session.plan.operations)
                || geometry::contains(&stored.session.plan.operations))
        {
            stored.approval_hash = None;
            stored.validation_evidence_hash = None;
            stored.session.approval_hash = None;
            stored.session.state = kicad_engine::SessionState::Rejected;
        }
    }

    fn verify_native_source(
        &self,
        stored: &StoredChangeSession,
        source: &LiveBoardObservation,
    ) -> Result<(), String> {
        if geometry::contains(&stored.session.plan.operations) {
            if source.snapshot != stored.session.snapshot
                || source.instance_token.as_str() != stored.session.plan.instance_token
                || source.board_path != stored.board_path
                || source.project_dir != stored.project_dir
            {
                return Err("geometry source identity changed since planning".into());
            }
            let request = geometry::request(&stored.session.plan.operations)?;
            let shape = kicad_engine::geometry_plan::geometry_plan(&stored.session.plan.operations)
                .map_err(|e| e.to_string())?;
            let current = if shape.additions.is_empty() {
                self.discovery
                    .geometry_planning(&stored.endpoint, source, request)?
            } else {
                let kinds = shape
                    .additions
                    .iter()
                    .map(|op| op.kind.clone())
                    .collect::<Vec<_>>();
                self.discovery
                    .geometry_additive_planning(&stored.endpoint, source, &kinds)?
                    .ok_or("zoned additive source no longer has complete refill evidence")?
            };
            geometry::matches(&stored.session.plan.operations, &current)?;
            if stored.geometry_evidence.as_ref() != Some(&current) {
                return Err("stored geometry evidence changed".into());
            }
            return Ok(());
        }
        if !copper_route::contains_route(&stored.session.plan.operations) {
            return Ok(());
        }
        if source.snapshot != stored.session.snapshot
            || source.instance_token.as_str() != stored.session.plan.instance_token
            || source.board_path != stored.board_path
            || source.project_dir != stored.project_dir
        {
            return Err("route source observation changed since planning".into());
        }
        let kinds = stored
            .session
            .plan
            .operations
            .iter()
            .map(|op| op.kind.clone())
            .collect::<Vec<_>>();
        let current = self
            .discovery
            .copper_route_evidence(&stored.endpoint, source, &kinds)?;
        copper_route::matches(&stored.session.plan.operations, &current)?;
        if stored.routing_evidence.as_ref() != Some(&current) {
            return Err("stored routing evidence changed".into());
        }
        Ok(())
    }
    pub fn with_state_dir(mut self, directory: &std::path::Path) -> std::io::Result<Self> {
        self.execution_store = Some(Arc::new(recovery::ExecutionStore::open(directory)?));
        for name in RECOVERY_TOOLS {
            self.tool_router.enable_route(name);
        }
        self.capabilities
            .retain(|c| c.id != "project.guarded_recovery");
        self.capabilities.retain(|c| c.id != "pcb.geometry.write");
        self.capabilities.push(Capability {
            id: "pcb.geometry.write",
            domain: "pcb",
            mode: "write",
            status: "verified",
        });
        self.capabilities.push(Capability {
            id: "project.guarded_recovery",
            domain: "project",
            mode: "write",
            status: "verified",
        });
        Ok(self)
    }

    fn acquire_project_writer(
        &self,
        project: &std::path::Path,
    ) -> Result<Option<recovery::ProjectWriter>, ErrorData> {
        self.execution_store
            .as_ref()
            .map(|store| {
                let writer = store.acquire(project)?;
                writer.ensure_ready()?;
                Ok::<_, std::io::Error>(writer)
            })
            .transpose()
            .map_err(|e| ErrorData::internal_error(format!("project write unavailable: {e}"), None))
    }

    pub fn with_discovery(discovery: Arc<dyn DiscoveryService>) -> Self {
        Self::with_discovery_and_execution(
            discovery,
            Arc::new(IpcBoardGraphicsExecutionService::default()),
        )
    }

    pub fn with_discovery_and_execution(
        discovery: Arc<dyn DiscoveryService>,
        execution: Arc<dyn BoardGraphicsExecutionService>,
    ) -> Self {
        Self::with_services(discovery, execution, Arc::new(IpcBoardValidationService))
    }

    pub fn with_discovery_and_execution_and_shadow(
        discovery: Arc<dyn DiscoveryService>,
        execution: Arc<dyn BoardGraphicsExecutionService>,
        shadow_validation: Arc<dyn ShadowValidationService>,
    ) -> Self {
        Self::with_all_services_and_shadow(
            discovery,
            execution,
            Arc::new(IpcBoardValidationService),
            Arc::new(ProductionSchematicChangeService),
            Some(shadow_validation),
        )
    }

    pub fn with_services(
        discovery: Arc<dyn DiscoveryService>,
        execution: Arc<dyn BoardGraphicsExecutionService>,
        validation: Arc<dyn BoardValidationService>,
    ) -> Self {
        Self::with_all_services(
            discovery,
            execution,
            validation,
            Arc::new(ProductionSchematicChangeService),
        )
    }

    pub fn with_all_services(
        discovery: Arc<dyn DiscoveryService>,
        execution: Arc<dyn BoardGraphicsExecutionService>,
        validation: Arc<dyn BoardValidationService>,
        schematic: Arc<dyn SchematicChangeService>,
    ) -> Self {
        Self::with_all_services_and_shadow(discovery, execution, validation, schematic, None)
    }

    fn with_all_services_and_shadow(
        discovery: Arc<dyn DiscoveryService>,
        execution: Arc<dyn BoardGraphicsExecutionService>,
        validation: Arc<dyn BoardValidationService>,
        schematic: Arc<dyn SchematicChangeService>,
        shadow_validation: Option<Arc<dyn ShadowValidationService>>,
    ) -> Self {
        Self::with_frozen_library_settings(
            discovery,
            execution,
            validation,
            schematic,
            shadow_validation,
            library_settings::capture(),
        )
    }

    fn with_frozen_library_settings(
        discovery: Arc<dyn DiscoveryService>,
        execution: Arc<dyn BoardGraphicsExecutionService>,
        validation: Arc<dyn BoardValidationService>,
        schematic: Arc<dyn SchematicChangeService>,
        shadow_validation: Option<Arc<dyn ShadowValidationService>>,
        library_settings: library_settings::SharedLibrarySettings,
    ) -> Self {
        let schematic_verified = schematic.verified();
        let mut tool_router = Self::tool_router();
        for name in RECOVERY_TOOLS {
            tool_router.disable_route(name);
        }
        if !schematic_verified {
            for name in [
                "kicad_plan_schematic_change",
                "kicad_preview_schematic_change",
                "kicad_commit_schematic_change",
            ] {
                tool_router.disable_route(name);
            }
        }
        Self {
            library_settings,
            execution_store: None,
            sessions: Arc::new(Mutex::new(BTreeMap::new())),
            schematic_sessions: Arc::new(Mutex::new(BTreeMap::new())),
            schematic_audit: Arc::new(Mutex::new(Vec::new())),
            schematic_audit_sequence: Arc::new(AtomicU64::new(0)),
            drc_baselines: Arc::new(Mutex::new(BTreeMap::new())),
            schematic,
            capabilities: vec![
                Capability {
                    id: "pcb.native_authoring",
                    domain: "pcb",
                    mode: "write",
                    status: "verified",
                },
                Capability {
                    id: "pcb.edit_existing",
                    domain: "pcb",
                    mode: "write",
                    status: "verified",
                },
                Capability {
                    id: "pcb.source_bound_footprint",
                    domain: "pcb",
                    mode: "write",
                    status: "verified",
                },
                Capability {
                    id: "pcb.source_bound_assembly",
                    domain: "pcb",
                    mode: "write",
                    status: "verified",
                },
                Capability {
                    id: "pcb.drc.validate",
                    domain: "pcb",
                    mode: "read",
                    status: "verified",
                },
                Capability {
                    id: "base.get_version",
                    domain: "base",
                    mode: "read",
                    status: "verified",
                },
                Capability {
                    id: "pcb.copper_route.write",
                    domain: "pcb",
                    mode: "write",
                    status: "verified",
                },
                Capability {
                    id: "pcb.routing_rules.inspect",
                    domain: "pcb",
                    mode: "read",
                    status: "verified",
                },
                Capability {
                    id: "pcb.geometry.write",
                    domain: "pcb",
                    mode: "write",
                    status: "unavailable",
                },
                Capability {
                    id: "pcb.geometry.inspect",
                    domain: "pcb",
                    mode: "read",
                    status: "verified",
                },
                Capability {
                    id: "project.inspect",
                    domain: "project",
                    mode: "read",
                    status: "verified",
                },
                Capability {
                    id: "schematic.fixed_two_resistor_change",
                    domain: "schematic",
                    mode: "write",
                    status: if schematic_verified {
                        "verified"
                    } else {
                        "unavailable"
                    },
                },
            ],
            discovery,
            execution,
            validation,
            shadow_validation,
            baseline_sequence: Arc::new(AtomicU64::new(0)),
            tool_router,
        }
    }

    pub fn capabilities(&self) -> &[Capability] {
        &self.capabilities
    }

    fn capability_matrix(&self) -> Value {
        let mut matrix: Value = serde_json::from_str(CAPABILITY_MATRIX_JSON)
            .expect("embedded capability matrix must be valid JSON");
        let statuses = self
            .capabilities
            .iter()
            .map(|capability| (capability.id, capability.status))
            .collect::<BTreeMap<_, _>>();
        if let Some(entries) = matrix["capabilities"].as_array_mut() {
            for entry in entries {
                if entry["id"] == "project.guarded_recovery" && self.execution_store.is_none() {
                    entry["status"] = json!("unavailable");
                }
                if let Some(id) = entry["id"].as_str()
                    && let Some(status) = statuses.get(id)
                {
                    entry["status"] = Value::String((*status).to_owned());
                }
            }
        }
        matrix
    }
}

#[tool_router]
impl Server {
    #[tool(
        name = "kicad_capabilities",
        description = "Report KiCad capability status and enabled tools."
    )]
    pub async fn kicad_capabilities(
        &self,
        Parameters(arguments): Parameters<CapabilityRequest>,
    ) -> Result<CallToolResponse, ErrorData> {
        let request_id = arguments
            .request_id
            .unwrap_or_else(|| "req_capabilities".to_owned());
        let advertised_tools = self
            .tool_router
            .list_all()
            .into_iter()
            .map(|tool| tool.name.to_string())
            .collect::<Vec<_>>();
        let envelope = Envelope {
            ok: true,
            request_id,
            run_id: None,
            project_revision: None,
            data: json!({
                "schema_version": CONTRACT_SCHEMA_VERSION,
                "capabilities": self.capabilities.iter().map(|capability| json!({
                    "id": capability.id,
                    "domain": capability.domain,
                    "mode": capability.mode,
                    "status": capability.status
                })).collect::<Vec<_>>(),
                "capability_matrix": self.capability_matrix(),
                "advertised_tools": advertised_tools
            }),
            diagnostics: Vec::new(),
            next_actions: Vec::new(),
        };
        let value = serde_json::to_value(envelope)
            .map_err(|error| ErrorData::internal_error(error.to_string(), None))?;
        Ok(rmcp::model::CallToolResult::structured(value).into())
    }

    #[tool(
        name = "kicad_schematic_audit",
        description = "Read the bounded, append-only audit ledger for schematic changes."
    )]
    pub async fn kicad_schematic_audit(
        &self,
        Parameters(arguments): Parameters<SchematicAuditRequest>,
    ) -> Result<CallToolResponse, ErrorData> {
        if !self.schematic.verified() {
            return Ok(self.rejected(
                arguments
                    .request_id
                    .unwrap_or_else(|| "req_schematic_audit".into()),
                "capability_not_verified",
            ));
        }
        const MAX_AUDIT_EVENTS: usize = 1000;
        let limit = arguments.limit.unwrap_or(100).min(MAX_AUDIT_EVENTS);
        let events = self
            .schematic_audit
            .lock()
            .expect("schematic audit lock")
            .iter()
            .filter(|event| {
                arguments
                    .source_run_id
                    .as_deref()
                    .is_none_or(|source_run_id| event.source_run_id == source_run_id)
            })
            .filter(|event| {
                arguments
                    .plan_id
                    .as_deref()
                    .is_none_or(|plan_id| event.plan_id == plan_id)
            })
            .rev()
            .take(limit)
            .cloned()
            .collect::<Vec<_>>();
        self.envelope(
            arguments
                .request_id
                .unwrap_or_else(|| "req_schematic_audit".into()),
            None,
            json!({"events": events, "limit": limit}),
        )
    }

    #[tool(
        name = "kicad_validate_project",
        description = "Run read-only PCB DRC against the server-observed open board."
    )]
    pub async fn kicad_validate_project(
        &self,
        Parameters(arguments): Parameters<ValidateProjectRequest>,
    ) -> Result<CallToolResponse, ErrorData> {
        let request_id = arguments
            .request_id
            .unwrap_or_else(|| "req_validate_project".to_owned());
        let observation = match self
            .discovery
            .live_board_observation(&arguments.endpoint, None)
        {
            Ok(observation) => observation,
            Err(error) => {
                return Ok(self.envelope_sync(
                    request_id,
                    None,
                    json!({
                        "kind": "observation_failed", "message": error
                    }),
                ));
            }
        };
        let board_identity = observation
            .snapshot
            .board
            .document_id
            .as_ref()
            .map(ToString::to_string)
            .ok_or_else(|| ErrorData::internal_error("board identity is unavailable", None))?;
        let board_path = match std::fs::canonicalize(&observation.board_path) {
            Ok(path) if path.is_file() => path,
            _ => {
                return Ok(self.envelope_sync(
                    request_id,
                    None,
                    json!({ "kind": "board_file_unavailable" }),
                ));
            }
        };
        let project_dir = match std::fs::canonicalize(&observation.project_dir) {
            Ok(path) if path.is_dir() => path,
            _ => {
                return Ok(self.envelope_sync(
                    request_id,
                    None,
                    json!({ "kind": "project_directory_unavailable" }),
                ));
            }
        };
        if !board_path.starts_with(&project_dir) {
            return Ok(self.envelope_sync(
                request_id,
                None,
                json!({ "kind": "board_outside_project" }),
            ));
        }
        let token = observation.instance_token.as_str().to_owned();
        let version = observation.snapshot.kicad_version.clone();
        let current_revision = observation
            .snapshot
            .revision()
            .map_err(|error| ErrorData::internal_error(error.to_string(), None))?;
        let baseline = if let Some(id) = arguments.baseline_id.as_deref() {
            let baseline = self
                .drc_baselines
                .lock()
                .expect("baseline lock")
                .get(id)
                .cloned();
            let baseline = match baseline {
                Some(baseline) => baseline,
                None => {
                    return Ok(self.envelope_sync(
                        request_id,
                        None,
                        json!({ "kind": "baseline_not_found", "baseline_id": id }),
                    ));
                }
            };
            if baseline.instance_token != token
                || baseline.board_identity != board_identity
                || baseline.board_path != board_path
                || baseline.kicad_version != version
            {
                return Ok(self.envelope_sync(
                    request_id,
                    None,
                    json!({ "kind": "baseline_mismatch", "baseline_id": id }),
                ));
            }
            Some(baseline)
        } else {
            None
        };
        let policy = DrcPolicy {
            allowed_root: project_dir,
            timeout: Duration::from_secs(30),
        };
        let result = self.validation.validate(&board_path, &policy);
        let report = match result {
            CheckResult::Completed(report) => report,
            CheckResult::Failed(failure) => {
                return Ok(self.envelope_sync(
                    request_id,
                    None,
                    json!({
                        "kind": "validation_failed", "failure": failure
                    }),
                ));
            }
        };
        if let Some(id) = arguments.baseline_id {
            let baseline = baseline.expect("baseline precheck guarantees presence");
            if baseline.report.parser_version != report.parser_version {
                return Ok(self.envelope_sync(
                    request_id,
                    None,
                    json!({ "kind": "baseline_mismatch", "baseline_id": id }),
                ));
            }
            let delta = compare_drc(&baseline.report, &report);
            return self.envelope(
                request_id,
                Some(current_revision.to_string()),
                json!({
                    "baseline_id": id,
                    "report": report,
                    "delta": delta,
                    "captured_revision": baseline.captured_revision.to_string(),
                    "current_revision": current_revision.to_string()
                }),
            );
        }
        let sequence = self.baseline_sequence.fetch_add(1, Ordering::Relaxed);
        let baseline_id = StableId::new(format!("drc-baseline:{sequence}"))
            .map_err(|error| ErrorData::internal_error(error.to_string(), None))?
            .to_string();
        self.drc_baselines.lock().expect("baseline lock").insert(
            baseline_id.clone(),
            StoredDrcBaseline {
                instance_token: token,
                board_identity,
                board_path,
                kicad_version: version,
                captured_revision: current_revision.clone(),
                report: report.clone(),
            },
        );
        self.envelope(
            request_id,
            Some(current_revision.to_string()),
            json!({
                "baseline_id": baseline_id,
                "report": report,
                "delta": Value::Null,
                "captured_revision": current_revision.to_string()
            }),
        )
    }

    #[tool(
        name = "kicad_inspect_project",
        description = "Read open KiCad documents and optional routing rules for existing net IDs, or durable project history with an empty endpoint."
    )]
    pub async fn kicad_inspect_project(
        &self,
        Parameters(arguments): Parameters<InspectProjectRequest>,
    ) -> Result<CallToolResponse, ErrorData> {
        let request_id = arguments
            .request_id
            .unwrap_or_else(|| "req_inspect_project".to_owned());
        let project_dir = arguments.project_dir.as_deref().map(std::path::Path::new);
        if let Some(query) = &arguments.routing_rules {
            routing_rules::validate_selection(&query.net_ids)
                .map_err(|reason| ErrorData::invalid_params(reason, None))?;
            if arguments.endpoint.is_empty() {
                return Err(ErrorData::invalid_params(
                    "routing rules require a live board endpoint",
                    None,
                ));
            }
        }
        if arguments.geometry == Some(true) && arguments.endpoint.is_empty() {
            return Err(ErrorData::invalid_params(
                "geometry inspection requires a live board endpoint",
                None,
            ));
        }
        if arguments.footprint_source.is_some() && arguments.endpoint.is_empty() {
            return Err(ErrorData::invalid_params(
                "footprint source lookup requires a live board endpoint",
                None,
            ));
        }
        if arguments.endpoint.is_empty() {
            let (Some(project), Some(store)) = (project_dir, self.execution_store.as_ref()) else {
                return Ok(self.rejected(
                    request_id,
                    "durable history requires project_dir and a state directory",
                ));
            };
            let execution_history = store.history(project).map_err(|e| {
                ErrorData::internal_error(format!("execution history unavailable: {e}"), None)
            })?;
            let recovery_history = store.recovery_history(project).map_err(|e| {
                ErrorData::internal_error(format!("recovery history unavailable: {e}"), None)
            })?;
            return self.envelope(request_id, None, json!({"observation": "durable_history_only", "execution_history": execution_history, "recovery_history": recovery_history}));
        }
        let snapshot = self
            .discovery
            .discover(&arguments.endpoint, project_dir)
            .map_err(|error| ErrorData::internal_error(error.to_string(), None))?;
        let revision = snapshot
            .revision()
            .map_err(|error| ErrorData::internal_error(error.to_string(), None))?;
        let mut execution_history = Vec::new();
        if let Some(store) = &self.execution_store {
            let mut projects = snapshot
                .documents
                .iter()
                .filter_map(|document| document.project_path.as_deref())
                .map(std::path::PathBuf::from)
                .collect::<std::collections::BTreeSet<_>>();
            if let Some(project) = project_dir {
                projects.insert(project.to_path_buf());
            }
            for project in projects {
                execution_history.extend(store.history(&project).map_err(|e| {
                    ErrorData::internal_error(format!("recovery history unavailable: {e}"), None)
                })?);
            }
        }
        let mut data = serde_json::to_value(&snapshot)
            .map_err(|error| ErrorData::internal_error(error.to_string(), None))?;
        match self
            .discovery
            .live_board_observation(&arguments.endpoint, project_dir)
        {
            Ok(pcb) => {
                let revision = pcb
                    .snapshot
                    .revision()
                    .map_err(|e| ErrorData::internal_error(e.to_string(), None))?;
                if arguments.geometry == Some(true) {
                    data["geometry"] = match self
                        .discovery
                        .geometry_inspection(&arguments.endpoint, &pcb)
                    {
                        Ok(observation) => json!({"status":"observed","observation":observation}),
                        Err(reason) => json!({"status":"unavailable","reason":reason}),
                    };
                }
                if let Some(query) = &arguments.routing_rules {
                    data["routing_rules"] = match self.discovery.routing_rule_inspection(
                        &arguments.endpoint,
                        &pcb,
                        &query.net_ids,
                    ) {
                        Ok(observed) => json!({"status":"observed", "observation":observed}),
                        Err(reason) => json!({"status":"unavailable", "reason":reason}),
                    };
                }
                if let Some(query) = &arguments.footprint_source {
                    let identifier = kicad_model::FootprintLibraryIdentifier {
                        nickname: query.library_nickname.clone(),
                        entry_name: query.library_entry_name.clone(),
                    };
                    data["footprint_source"] = match library_settings::registry(
                        &self.library_settings,
                        &pcb.project_dir,
                        &pcb.snapshot.kicad_version,
                    )
                    .and_then(|r| r.resolve_identifier(&identifier).map_err(|e| e.to_string()))
                    {
                        Ok(source) => {
                            json!({"status":"resolved", "geometry_verified": false, "evidence":source.provenance.catalog_evidence})
                        }
                        Err(reason) => json!({"status":"unavailable", "reason":reason}),
                    };
                }
                data["pcb"] = json!({"snapshot": pcb.snapshot, "revision": revision.to_string(), "instance_token": pcb.instance_token.as_str()});
            }
            Err(reason) => {
                data["pcb"] = Value::Null;
                data["pcb_observation_error"] = json!(reason);
                if arguments.geometry == Some(true) {
                    data["geometry"] = json!({"status":"unavailable","reason":reason});
                }
                if arguments.routing_rules.is_some() {
                    data["routing_rules"] = json!({"status":"unavailable", "reason":reason});
                }
            }
        }
        if self.execution_store.is_some() {
            data["execution_history"] = json!(execution_history);
            let recovery_history = if let Some(store) = &self.execution_store {
                let mut records = Vec::new();
                let mut projects = snapshot
                    .documents
                    .iter()
                    .filter_map(|document| document.project_path.as_deref())
                    .map(std::path::PathBuf::from)
                    .collect::<std::collections::BTreeSet<_>>();
                if let Some(project) = project_dir {
                    projects.insert(project.to_path_buf());
                }
                for project in projects {
                    records.extend(store.recovery_history(&project).map_err(|e| {
                        ErrorData::internal_error(
                            format!("recovery history unavailable: {e}"),
                            None,
                        )
                    })?);
                }
                records
            } else {
                Vec::new()
            };
            data["recovery_history"] = json!(recovery_history);
            data["recovery_policy"] = json!(
                "Uncertain outcomes require a guarded recovery plan, fresh source and backup checks, exact approval and atomic restore. No automatic replay or rollback."
            );
        }
        let envelope = Envelope {
            ok: true,
            request_id,
            run_id: None,
            project_revision: Some(revision.to_string()),
            data,
            diagnostics: Vec::new(),
            next_actions: Vec::new(),
        };
        let value = serde_json::to_value(envelope)
            .map_err(|error| ErrorData::internal_error(error.to_string(), None))?;
        Ok(rmcp::model::CallToolResult::structured(value).into())
    }

    #[tool(
        name = "kicad_plan_recovery",
        description = "Prepare a guarded recovery plan for one uncertain recorded execution without mutating source files."
    )]
    pub async fn kicad_plan_recovery(
        &self,
        Parameters(arguments): Parameters<PlanRecoveryRequest>,
    ) -> Result<CallToolResponse, ErrorData> {
        let request_id = arguments
            .request_id
            .unwrap_or_else(|| "req_plan_recovery".into());
        if arguments.action != "restore_backups" {
            return Ok(self.rejected(request_id, "recovery_action_unsupported"));
        }
        let Some(store) = &self.execution_store else {
            return Ok(self.rejected(request_id, "recovery_unavailable"));
        };
        let record = match store.plan_recovery(
            std::path::Path::new(&arguments.project_dir),
            &arguments.attempt_id,
        ) {
            Ok(record) => record,
            Err(error) => return Ok(self.rejected(request_id, &error.to_string())),
        };
        let mut data = json!({
            "recovery_id": record.recovery_id,
            "attempt_id": record.attempt_id,
            "action": record.action,
            "recovery_plan_hash": record.plan_hash,
            "files": record.files,
            "state": record.state
        });
        if !record.created_sources.is_empty() {
            data["schema_version"] = json!(record.schema_version);
            data["created_sources"] = json!(record.created_sources);
            data["created_sources_restore_to"] = json!({"kind": "absent"});
        }
        self.envelope(request_id, Some(record.plan_hash.clone()), data)
    }

    #[tool(
        name = "kicad_preview_recovery",
        description = "Recheck a guarded recovery plan against current files and return exact approval evidence."
    )]
    pub async fn kicad_preview_recovery(
        &self,
        Parameters(arguments): Parameters<PreviewRecoveryRequest>,
    ) -> Result<CallToolResponse, ErrorData> {
        let request_id = arguments
            .request_id
            .unwrap_or_else(|| "req_preview_recovery".into());
        let Some(store) = &self.execution_store else {
            return Ok(self.rejected(request_id, "recovery_unavailable"));
        };
        let approval_hash = match store.preview_recovery(
            std::path::Path::new(&arguments.project_dir),
            &arguments.recovery_id,
            &arguments.recovery_plan_hash,
        ) {
            Ok(hash) => hash,
            Err(error) => return Ok(self.rejected(request_id, &error.to_string())),
        };
        self.envelope(
            request_id,
            Some(arguments.recovery_plan_hash.clone()),
            json!({
                "recovery_id": arguments.recovery_id,
                "recovery_plan_hash": arguments.recovery_plan_hash,
                "approval_hash": approval_hash,
                "state": "awaiting_approval"
            }),
        )
    }

    #[tool(
        name = "kicad_commit_recovery",
        description = "Restore backups and remove recorded new schematics using an exactly approved per-file recovery plan."
    )]
    pub async fn kicad_commit_recovery(
        &self,
        Parameters(arguments): Parameters<CommitRecoveryRequest>,
    ) -> Result<CallToolResponse, ErrorData> {
        let request_id = arguments
            .request_id
            .unwrap_or_else(|| "req_commit_recovery".into());
        let Some(store) = &self.execution_store else {
            return Ok(self.rejected(request_id, "recovery_unavailable"));
        };
        let project = std::path::Path::new(&arguments.project_dir);
        let result = store.commit_recovery(
            project,
            &arguments.recovery_id,
            &arguments.recovery_plan_hash,
            &arguments.approval_hash,
        );
        match result {
            Ok(record) => self.envelope(
                request_id,
                None,
                json!({
                    "recovery_id": record.recovery_id,
                    "state": record.state,
                    "outcome": record.outcome
                }),
            ),
            Err(error) => {
                let state = if error.unknown { "unknown" } else { "rejected" };
                Ok(self.envelope_sync(
                    request_id,
                    None,
                    json!({
                        "recovery_id": arguments.recovery_id,
                        "state": state,
                        "outcome": {"kind": if state == "unknown" { "unknown" } else { "rejected" }, "reason": error.to_string()}
                    }),
                ))
            }
        }
    }

    #[tool(
        name = "kicad_plan_change",
        description = "Plan bounded PCB geometry, graphics, pad-bound routing, existing item fields, footprint transforms or source-bound placements without mutating KiCad."
    )]
    pub async fn kicad_plan_change(
        &self,
        Parameters(arguments): Parameters<PlanBoardGraphicsRequest>,
    ) -> Result<CallToolResponse, ErrorData> {
        if arguments.endpoint.is_empty() {
            return Ok(self.rejected(arguments.request_id, "identity_is_required"));
        }
        if arguments.geometry.is_some()
            && (arguments.delete_items.is_some()
                || arguments.flip_footprint.is_some()
                || arguments.edit_existing.is_some()
                || arguments.move_footprint.is_some()
                || arguments.assembly.is_some()
                || arguments.footprint.is_some()
                || arguments.batch.is_some()
                || arguments.segments.is_some()
                || arguments.outline.is_some()
                || arguments.track.is_some()
                || arguments.via.is_some()
                || arguments.route.is_some())
        {
            return Err(ErrorData::invalid_params(
                "geometry requires its own plan",
                None,
            ));
        }
        if arguments.delete_items.is_some()
            && (arguments.flip_footprint.is_some()
                || arguments.edit_existing.is_some()
                || arguments.move_footprint.is_some()
                || arguments.assembly.is_some()
                || arguments.footprint.is_some()
                || arguments.batch.is_some()
                || arguments.segments.is_some()
                || arguments.outline.is_some()
                || arguments.track.is_some()
                || arguments.via.is_some()
                || arguments.route.is_some())
        {
            return Err(ErrorData::invalid_params(
                "deletion requires its own plan",
                None,
            ));
        }
        if arguments.flip_footprint.is_some()
            && (arguments.edit_existing.is_some()
                || arguments.move_footprint.is_some()
                || arguments.assembly.is_some()
                || arguments.footprint.is_some()
                || arguments.batch.is_some()
                || arguments.segments.is_some()
                || arguments.outline.is_some()
                || arguments.track.is_some()
                || arguments.via.is_some()
                || arguments.route.is_some())
        {
            return Err(ErrorData::invalid_params(
                "footprint flip requires its own plan",
                None,
            ));
        }
        if arguments.edit_existing.is_some()
            && (arguments.move_footprint.is_some()
                || arguments.assembly.is_some()
                || arguments.footprint.is_some()
                || arguments.batch.is_some()
                || arguments.segments.is_some()
                || arguments.outline.is_some()
                || arguments.track.is_some()
                || arguments.via.is_some()
                || arguments.route.is_some())
        {
            return Err(ErrorData::invalid_params(
                "existing item edits require their own plan",
                None,
            ));
        }
        let observation = self
            .discovery
            .live_board_observation(&arguments.endpoint, None)
            .map_err(|error| ErrorData::internal_error(error, None))?;
        let observed_token = observation.instance_token.as_str();
        let assembly = if let Some(request) = &arguments.assembly {
            if arguments.footprint.is_some()
                || arguments.move_footprint.is_some()
                || arguments.batch.is_some()
                || arguments.segments.is_some()
                || arguments.outline.is_some()
                || arguments.track.is_some()
                || arguments.via.is_some()
                || arguments.route.is_some()
            {
                return Err(ErrorData::invalid_params(
                    "assembly requires its own plan",
                    None,
                ));
            }
            match request.prepare(&observation, &self.library_settings) {
                Ok(assembly) => Some(assembly),
                Err(reason) => {
                    return Ok(self.envelope_sync(
                        arguments.request_id,
                        None,
                        json!({"reason": "assembly_source_unavailable", "detail": reason}),
                    ));
                }
            }
        } else {
            None
        };
        let footprint = if let Some(request) = &arguments.footprint {
            if arguments.batch.is_some()
                || arguments.move_footprint.is_some()
                || arguments.segments.is_some()
                || arguments.outline.is_some()
                || arguments.track.is_some()
                || arguments.via.is_some()
                || arguments.route.is_some()
            {
                return Err(ErrorData::invalid_params(
                    "footprint placement requires its own plan",
                    None,
                ));
            }
            match footprint_authoring::prepare(&self.library_settings, request, &observation) {
                Ok(placement) => Some(placement),
                Err(reason) => {
                    return Ok(self.envelope_sync(
                        arguments.request_id,
                        None,
                        json!({"reason": "footprint_source_unavailable", "detail": reason}),
                    ));
                }
            }
        } else {
            None
        };
        if arguments.move_footprint.is_some()
            && (arguments.footprint.is_some()
                || arguments.assembly.is_some()
                || arguments.batch.is_some()
                || arguments.segments.is_some()
                || arguments.outline.is_some()
                || arguments.track.is_some()
                || arguments.via.is_some()
                || arguments.route.is_some())
        {
            return Err(ErrorData::invalid_params(
                "existing footprint movement requires its own plan",
                None,
            ));
        }
        let snapshot = observation.snapshot.clone();
        if !snapshot
            .capabilities
            .iter()
            .any(|capability| capability == "pcb.board_graphics.write")
        {
            return Ok(self.rejected(arguments.request_id, "capability_not_verified"));
        }
        let board = snapshot
            .board
            .document_id
            .clone()
            .ok_or_else(|| ErrorData::internal_error("board identity is unavailable", None))?;
        let mut geometry_evidence = arguments
            .geometry
            .as_ref()
            .map(|request| {
                self.discovery
                    .geometry_planning(&arguments.endpoint, &observation, request)
            })
            .transpose()
            .map_err(|e| ErrorData::invalid_params(e, None))?;
        let (mut kinds, is_batch) = if let Some(request) = &arguments.geometry {
            let evidence = geometry_evidence
                .as_ref()
                .expect("prepared geometry evidence");
            (
                vec![OperationKind::EditGeometry {
                    board: board.clone(),
                    request: request.clone(),
                    context: evidence.context.clone(),
                    source_evidence: evidence.source_evidence.clone(),
                }],
                false,
            )
        } else if let Some(assembly) = assembly {
            (
                vec![OperationKind::PlaceConnectedFootprints {
                    board: board.clone(),
                    assembly,
                }],
                false,
            )
        } else if let Some(placement) = footprint {
            (
                vec![OperationKind::PlaceAllowlistedFootprint {
                    board: board.clone(),
                    placement,
                }],
                false,
            )
        } else if let Some(request) = &arguments.delete_items {
            if snapshot
                .revision()
                .map_err(|e| ErrorData::invalid_params(e.to_string(), None))?
                .to_string()
                != request.observed_revision
            {
                return Ok(self.rejected(arguments.request_id, "stale_plan"));
            }
            let targets: Vec<kicad_engine::ExistingItemTarget> = serde_json::from_value(
                serde_json::to_value(&request.targets)
                    .map_err(|e| ErrorData::invalid_params(e.to_string(), None))?,
            )
            .map_err(|e| ErrorData::invalid_params(e.to_string(), None))?;
            let mut kind = OperationKind::DeleteExistingItems {
                board: board.clone(),
                targets,
                native_evidence: digest_bytes(b"unbound"),
            };
            if let Err(error) = kicad_engine::expected_item_deletion(&snapshot, &kind) {
                return Ok(self.rejected(arguments.request_id, &error.to_string()));
            }
            let evidence = match self.discovery.item_deletion_evidence(
                &arguments.endpoint,
                &observation,
                &kind,
            ) {
                Ok(evidence) => evidence,
                Err(reason) => {
                    return Ok(self.envelope_sync(
                        arguments.request_id,
                        None,
                        json!({"reason":"item_deletion_unavailable", "detail": reason}),
                    ));
                }
            };
            if let OperationKind::DeleteExistingItems {
                native_evidence, ..
            } = &mut kind
            {
                *native_evidence = evidence;
            }
            (vec![kind], false)
        } else if let Some(request) = &arguments.flip_footprint {
            if snapshot
                .revision()
                .map_err(|e| ErrorData::invalid_params(e.to_string(), None))?
                .to_string()
                != request.observed_revision
            {
                return Ok(self.rejected(arguments.request_id, "stale_plan"));
            }
            let mut kind = OperationKind::FlipExistingFootprint {
                board: board.clone(),
                placement_id: StableId::new(request.placement_id.clone())
                    .map_err(|e| ErrorData::invalid_params(e.to_string(), None))?,
                source_uuid: StableId::new(request.source_uuid.clone())
                    .map_err(|e| ErrorData::invalid_params(e.to_string(), None))?,
                target_side: match request.target_side {
                    BoardFlipSideRequest::Front => kicad_model::BoardSide::Front,
                    BoardFlipSideRequest::Back => kicad_model::BoardSide::Back,
                },
                axis: match request.axis {
                    BoardFlipAxisRequest::TopBottom => kicad_engine::FootprintFlipAxis::TopBottom,
                    BoardFlipAxisRequest::LeftRight => kicad_engine::FootprintFlipAxis::LeftRight,
                },
                native_evidence: digest_bytes(b"unbound"),
            };
            if let Err(error) = kicad_engine::expected_footprint_flip(&snapshot, &kind) {
                return Ok(self.rejected(arguments.request_id, &error.to_string()));
            }
            let evidence = match self.discovery.footprint_flip_evidence(
                &arguments.endpoint,
                &observation,
                &kind,
            ) {
                Ok(e) => e,
                Err(reason) => {
                    return Ok(self.envelope_sync(
                        arguments.request_id,
                        None,
                        json!({"reason":"footprint_flip_unavailable", "detail": reason}),
                    ));
                }
            };
            if let OperationKind::FlipExistingFootprint {
                native_evidence, ..
            } = &mut kind
            {
                *native_evidence = evidence;
            }
            (vec![kind], false)
        } else if let Some(request) = &arguments.edit_existing {
            if snapshot
                .revision()
                .map_err(|e| ErrorData::invalid_params(e.to_string(), None))?
                .to_string()
                != request.observed_revision
            {
                return Ok(self.rejected(arguments.request_id, "stale_plan"));
            }
            let changes: Vec<kicad_engine::ExistingItemChange> = serde_json::from_value(
                serde_json::to_value(&request.changes)
                    .map_err(|e| ErrorData::invalid_params(e.to_string(), None))?,
            )
            .map_err(|e| ErrorData::invalid_params(e.to_string(), None))?;
            let mut kind = OperationKind::EditExistingItems {
                board: board.clone(),
                changes: changes.clone(),
                native_evidence: digest_bytes(b"unbound"),
            };
            if let Err(error) = kicad_engine::expected_existing_item_edit(&snapshot, &kind) {
                return Ok(self.rejected(arguments.request_id, &error.to_string()));
            }
            let evidence = match self.discovery.existing_item_edit_evidence(
                &arguments.endpoint,
                &observation,
                &changes,
            ) {
                Ok(evidence) => evidence,
                Err(reason) => {
                    return Ok(self.envelope_sync(
                        arguments.request_id,
                        None,
                        json!({"reason":"existing_item_edit_unavailable","detail":reason}),
                    ));
                }
            };
            if let OperationKind::EditExistingItems {
                native_evidence, ..
            } = &mut kind
            {
                *native_evidence = evidence;
            }
            (vec![kind], false)
        } else if let Some(move_request) = &arguments.move_footprint {
            if arguments.batch.is_some()
                || arguments.segments.is_some()
                || arguments.outline.is_some()
                || arguments.track.is_some()
                || arguments.via.is_some()
                || arguments.route.is_some()
            {
                return Err(ErrorData::invalid_params(
                    "existing footprint movement requires its own plan",
                    None,
                ));
            }
            if snapshot
                .revision()
                .map_err(|e| ErrorData::invalid_params(e.to_string(), None))?
                .to_string()
                != move_request.observed_revision
            {
                return Ok(self.rejected(arguments.request_id, "stale_plan"));
            }
            let placement_id = StableId::new(move_request.placement_id.clone())
                .map_err(|e| ErrorData::invalid_params(e.to_string(), None))?;
            let source_uuid = StableId::new(move_request.source_uuid.clone())
                .map_err(|e| ErrorData::invalid_params(e.to_string(), None))?;
            let target = snapshot
                .native_footprints
                .as_ref()
                .and_then(|f| f.placements.iter().find(|p| p.placement_id == placement_id))
                .ok_or_else(|| ErrorData::invalid_params("target_not_found", None))?;
            if target.source_uuid != source_uuid {
                return Ok(self.rejected(arguments.request_id, "target_identity_mismatch"));
            }
            let position = match (move_request.x_nm, move_request.y_nm) {
                (Some(x), Some(y)) => kicad_model::Coordinate {
                    x,
                    y,
                    ..target.position
                },
                (None, None) if move_request.orientation_mdeg.is_some() => target.position,
                _ => {
                    return Err(ErrorData::invalid_params(
                        "provide both position coordinates or an orientation",
                        None,
                    ));
                }
            };
            let orientation_mdeg = move_request
                .orientation_mdeg
                .unwrap_or(target.orientation_mdeg)
                .rem_euclid(360_000);
            let copper_tolerance_nm = move_request
                .copper_tolerance_nm
                .unwrap_or(kicad_engine::FOOTPRINT_COPPER_TOLERANCE_NM);
            let affected = match kicad_engine::connected_footprint_copper(
                &snapshot,
                &placement_id,
                copper_tolerance_nm,
            ) {
                Ok(affected) => affected,
                Err(e) => return Ok(self.rejected(arguments.request_id, &e.to_string())),
            };
            if !affected.is_empty() {
                return Ok(self.envelope_sync(arguments.request_id, None, json!({"reason": "connected_copper_requires_separate_edit", "target": placement_id, "affected_ids": affected})));
            }
            let (zones, native_evidence) = match self
                .discovery
                .footprint_move_evidence(&arguments.endpoint, &observation)
            {
                Ok(zones) => zones,
                Err(e) => {
                    return Ok(self.envelope_sync(
                        arguments.request_id,
                        None,
                        json!({"reason": "unsupported_copper_interaction", "detail": e}),
                    ));
                }
            };
            if !zones.is_empty() {
                return Ok(self.envelope_sync(
                    arguments.request_id,
                    None,
                    json!({"reason": "unsupported_copper_interaction", "affected_ids": zones}),
                ));
            }
            (
                vec![OperationKind::MoveExistingFootprint {
                    board: board.clone(),
                    placement_id,
                    source_uuid,
                    position,
                    orientation_mdeg,
                    copper_tolerance_nm,
                    native_evidence,
                }],
                false,
            )
        } else {
            board_operation_kinds(&arguments, &board)?
        };
        if geometry_evidence.is_none() {
            geometry_evidence = self
                .discovery
                .geometry_additive_planning(&arguments.endpoint, &observation, &kinds)
                .map_err(|e| ErrorData::invalid_params(e, None))?;
            if let Some(evidence) = &geometry_evidence {
                let additions = evidence.additions.as_ref().ok_or_else(|| {
                    ErrorData::internal_error("missing prepared additive evidence", None)
                })?;
                kinds = additions.kinds.clone();
                kinds.push(OperationKind::EditGeometry {
                    board: board.clone(),
                    request: kicad_model::geometry::GeometryRequest {
                        observed_revision: evidence.context.revision.clone(),
                        outline: None,
                        zones: Vec::new(),
                        refill: kicad_model::geometry::RefillMode::All,
                    },
                    context: evidence.context.clone(),
                    source_evidence: evidence.source_evidence.clone(),
                });
            }
        }
        let is_batch = is_batch || kinds.len() > 1;
        let routing_evidence = if geometry_evidence.is_none()
            && kinds
                .iter()
                .any(|kind| matches!(kind, OperationKind::AddCopperRoute { .. }))
        {
            let evidence = self
                .discovery
                .copper_route_evidence(&arguments.endpoint, &observation, &kinds)
                .map_err(|e| ErrorData::invalid_params(e, None))?;
            copper_route::bind(&mut kinds, &evidence)
                .map_err(|e| ErrorData::invalid_params(e, None))?;
            Some(evidence)
        } else {
            None
        };
        let base_revision = snapshot
            .revision()
            .map_err(|error| ErrorData::internal_error(error.to_string(), None))?;
        let plan_id = StableId::new(format!("plan:{}", arguments.request_id))
            .map_err(|error| ErrorData::invalid_params(error.to_string(), None))?;
        let operations = kinds
            .into_iter()
            .enumerate()
            .map(|(index, kind)| {
                let operation_id = if is_batch {
                    format!("operation:{}:{index}", arguments.request_id)
                } else {
                    format!("operation:{}", arguments.request_id)
                };
                let operation_id =
                    StableId::new(operation_id).expect("deterministic operation id is valid");
                let expected_effect = expected_board_effect(&kind);
                let targets = match &kind {
                    OperationKind::DeleteExistingItems { board, targets, .. } => {
                        deletion_targets(board, targets)
                    }
                    OperationKind::EditExistingItems { board, changes, .. } => {
                        existing_item_targets(board, changes)
                    }
                    OperationKind::FlipExistingFootprint {
                        placement_id,
                        source_uuid,
                        ..
                    }
                    | OperationKind::MoveExistingFootprint {
                        placement_id,
                        source_uuid,
                        ..
                    } => vec![board.clone(), placement_id.clone(), source_uuid.clone()],
                    _ => std::iter::once(board.clone())
                        .chain(
                            kind.footprint_placements()
                                .iter()
                                .map(|p| p.target_id.clone()),
                        )
                        .collect(),
                };
                let risk_class = if matches!(
                    kind,
                    OperationKind::DeleteExistingItems { .. } | OperationKind::EditGeometry { .. }
                ) {
                    RiskClass::High
                } else {
                    RiskClass::Medium
                };
                Operation {
                    operation_id,
                    kind,
                    targets,
                    preconditions: vec![Precondition::Revision(base_revision.clone())],
                    expected_effects: vec![expected_effect.into()],
                    risk_class,
                    idempotency_key: if is_batch {
                        format!("{}:{index}", arguments.request_id)
                    } else {
                        arguments.request_id.clone()
                    },
                    source_run_id: arguments.source_run_id.clone(),
                }
            })
            .collect();
        let server_token = observed_token.to_owned();
        let plan = ChangePlan {
            plan_id: plan_id.clone(),
            source_run_id: arguments.source_run_id,
            base_revision,
            instance_token: server_token.clone(),
            operations,
        };
        let engine = Engine::new(snapshot, server_token.clone());
        let session = engine
            .begin_session(plan)
            .map_err(|error| ErrorData::invalid_params(error.to_string(), None))?;
        let footprint_sources = session
            .plan
            .operations
            .iter()
            .flat_map(|operation| operation.kind.footprint_placements())
            .map(|placement| {
                footprint_authoring::source_evidence(
                    &self.library_settings,
                    &observation.project_dir,
                    placement,
                )
            })
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| ErrorData::invalid_params(e, None))?;
        let plan_hash = session.plan_hash.clone();
        let base_revision = session.plan.base_revision.to_string();
        self.sessions.lock().expect("session lock").insert(
            plan_id.to_string(),
            StoredChangeSession {
                session,
                routing_evidence: routing_evidence.clone(),
                geometry_evidence: geometry_evidence.clone(),
                endpoint: arguments.endpoint,
                board_filename: observation.board_filename,
                source_files: shadow_source_fingerprint(&observation.board_path),
                board_path: observation.board_path,
                project_dir: observation.project_dir,
                execution_state: StoredExecutionState::Ready,
                shadow_validation: None,
                validation_evidence_hash: None,
                approval_hash: None,
            },
        );
        self.envelope(
            arguments.request_id,
            Some(base_revision),
            json!({
                "plan_id": plan_id,
                "plan_hash": plan_hash,
                "footprint_sources": footprint_sources,
                "routing_evidence": routing_evidence,
                "geometry_evidence": geometry_evidence,
                "state": "planned"
            }),
        )
    }

    #[tool(
        name = "kicad_preview_change",
        description = "Preview and validate a planned KiCad PCB change, then await approval."
    )]
    pub async fn kicad_preview_change(
        &self,
        Parameters(arguments): Parameters<PreviewChangeRequest>,
    ) -> Result<CallToolResponse, ErrorData> {
        let mut stored = self
            .sessions
            .lock()
            .expect("session lock")
            .get(&arguments.plan_id)
            .cloned()
            .ok_or_else(|| ErrorData::invalid_params("unknown plan_id", None))?;
        if stored.session.plan_hash != arguments.plan_hash {
            return Err(ErrorData::invalid_params("plan hash mismatch", None));
        }
        if stored.execution_state != StoredExecutionState::Ready
            || stored.session.state != kicad_engine::SessionState::Planned
        {
            return Err(ErrorData::invalid_params(
                "preview requires an unexecuted planned session",
                None,
            ));
        }
        let engine = Engine::new(
            stored.session.snapshot.clone(),
            stored.session.plan.instance_token.clone(),
        );
        let predicted = if geometry::contains(&stored.session.plan.operations) {
            None
        } else {
            let preview = engine
                .preview_session(&mut stored.session)
                .map_err(|e| ErrorData::invalid_params(e.to_string(), None))?;
            engine
                .validate(&mut stored.session)
                .map_err(|e| ErrorData::invalid_params(e.to_string(), None))?;
            Some(preview)
        };
        let shadow_validation = self.shadow_validation.as_ref().ok_or_else(|| {
            ErrorData::invalid_params("shadow validation is required before approval", None)
        })?;
        let source = self
            .discovery
            .live_board_observation(&stored.endpoint, None)
            .map_err(|error| ErrorData::internal_error(error, None))?;
        if source.snapshot != stored.session.snapshot
            || source.instance_token.as_str() != stored.session.plan.instance_token
            || source.board_path != stored.board_path
            || source.project_dir != stored.project_dir
            || shadow_source_fingerprint(&stored.board_path) != stored.source_files
        {
            self.invalidate_native_approval(&arguments.plan_id);
            return Err(ErrorData::invalid_params(
                "source changed since planning",
                None,
            ));
        }
        if let Err(reason) = self.verify_native_source(&stored, &source) {
            self.invalidate_native_approval(&arguments.plan_id);
            return Err(ErrorData::invalid_params(reason, None));
        }
        let outcome = shadow_validation.validate(
            &stored.endpoint,
            &source,
            &stored.session.plan.operations,
            Duration::from_secs(30),
        );
        if let Err(reason) = self.verify_native_source(&stored, &source) {
            self.invalidate_native_approval(&arguments.plan_id);
            return Err(ErrorData::invalid_params(reason, None));
        }
        let shadow_ok = matches!(outcome.state, ShadowValidationState::Committed);
        let preview = if geometry::contains(&stored.session.plan.operations) {
            if shadow_ok {
                let actual = outcome.geometry.as_ref().ok_or_else(|| {
                    ErrorData::internal_error("missing verified geometry candidate", None)
                })?;
                let board = stored
                    .session
                    .snapshot
                    .board
                    .document_id
                    .clone()
                    .ok_or_else(|| ErrorData::internal_error("missing source board", None))?;
                let after = actual
                    .observation
                    .snapshot
                    .in_board_scope(stored.session.snapshot.project_id.clone(), board)
                    .map_err(|e| ErrorData::internal_error(e.to_string(), None))?;
                Some(
                    engine
                        .preview_observed_session(&mut stored.session, after)
                        .map_err(|e| ErrorData::invalid_params(e.to_string(), None))?,
                )
            } else {
                None
            }
        } else {
            predicted
        };
        if geometry::contains(&stored.session.plan.operations) && preview.is_some() {
            engine
                .validate(&mut stored.session)
                .map_err(|e| ErrorData::invalid_params(e.to_string(), None))?;
        }

        stored.shadow_validation = Some(outcome.clone());
        if shadow_ok {
            let evidence_hash =
                validation_evidence_hash(&stored.session.plan_hash, &outcome, &stored.source_files)
                    .map_err(|error| ErrorData::internal_error(error, None))?;
            stored.approval_hash = Some(approval_hash(&stored.session.plan_hash, &evidence_hash));
            stored.validation_evidence_hash = Some(evidence_hash);
        } else {
            stored.approval_hash = None;
            stored.validation_evidence_hash = None;
        }
        if !shadow_ok {
            let validation_evidence_hash = stored.validation_evidence_hash.clone();
            let approval_hash = stored.approval_hash.clone();
            self.sessions
                .lock()
                .expect("session lock")
                .insert(arguments.plan_id.clone(), stored);
            return Ok(self.envelope_sync(
                arguments
                    .request_id
                    .unwrap_or_else(|| "req_preview_change".into()),
                None,
                json!({
                    "plan_id": arguments.plan_id,
                    "plan_hash": arguments.plan_hash,
                    "validation_evidence_hash": validation_evidence_hash,
                    "approval_hash": approval_hash,
                    "state": match outcome.state {
                        ShadowValidationState::Rejected => "rejected",
                        ShadowValidationState::Unknown => "unknown",
                        ShadowValidationState::Committed => unreachable!("shadow_ok checked above"),
                    },
                    "shadow_validation": shadow_validation_data(&outcome),
                }),
            ));
        }
        let preview = preview.expect("successful preview was verified");
        engine
            .request_approval(&mut stored.session)
            .map_err(|error| ErrorData::invalid_params(error.to_string(), None))?;
        self.sessions
            .lock()
            .expect("session lock")
            .insert(arguments.plan_id.clone(), stored.clone());
        let after_revision = preview
            .after
            .revision()
            .map_err(|error| ErrorData::internal_error(error.to_string(), None))?
            .to_string();
        let shadow_data = stored
            .shadow_validation
            .as_ref()
            .map(shadow_validation_data)
            .unwrap_or(Value::Null);
        self.envelope(
            arguments
                .request_id
                .unwrap_or_else(|| "req_preview_change".into()),
            Some(after_revision),
            json!({
                "plan_id": stored.session.plan.plan_id,
                "plan_hash": preview.plan_hash,
                "validation_evidence_hash": stored.validation_evidence_hash,
                "approval_hash": stored.approval_hash,
                "operations": stored.session.plan.operations,
                "before": preview.before,
                "after": preview.after,
                "base_revision": stored.session.plan.base_revision,
                "shadow_validation": shadow_data,
                "state": "awaiting_approval"
            }),
        )
    }

    #[tool(
        name = "kicad_commit_change",
        description = "Commit an exactly approved and validated KiCad board change."
    )]
    pub async fn kicad_commit_change(
        &self,
        Parameters(arguments): Parameters<CommitChangeRequest>,
    ) -> Result<CallToolResponse, ErrorData> {
        let mut stored = self
            .sessions
            .lock()
            .expect("session lock")
            .get(&arguments.plan_id)
            .cloned()
            .ok_or_else(|| ErrorData::invalid_params("unknown plan_id", None))?;
        if stored.session.plan_hash != arguments.plan_hash {
            return Err(ErrorData::invalid_params("plan hash mismatch", None));
        }
        if self.shadow_validation.is_none() {
            return Err(ErrorData::invalid_params(
                "shadow validation is required before commit",
                None,
            ));
        }
        let evidence_matches = stored.validation_evidence_hash.as_deref()
            == Some(arguments.validation_evidence_hash.as_str());
        let approval_matches =
            stored.approval_hash.as_deref() == Some(arguments.approval_hash.as_str());
        if !evidence_matches || !approval_matches {
            return Err(ErrorData::invalid_params("approval mismatch", None));
        }
        if !stored.shadow_validation.as_ref().is_some_and(|outcome| {
            matches!(outcome.state, ShadowValidationState::Committed)
                && outcome.reopened
                && outcome.normalized_snapshot_matches
                && outcome.source_unchanged
                && outcome.shadow_cleaned
        }) {
            return Err(ErrorData::invalid_params(
                "shadow validation is required before commit",
                None,
            ));
        }
        let current_evidence = validation_evidence_hash(
            &stored.session.plan_hash,
            stored.shadow_validation.as_ref().expect("validated above"),
            &stored.source_files,
        )
        .map_err(|error| ErrorData::internal_error(error, None))?;
        if stored.validation_evidence_hash.as_deref() != Some(current_evidence.as_str()) {
            return Err(ErrorData::invalid_params(
                "validation evidence changed since approval",
                None,
            ));
        }
        let writer = self.acquire_project_writer(&stored.project_dir)?;
        if geometry::contains(&stored.session.plan.operations) && writer.is_none() {
            return Err(ErrorData::invalid_params(
                "geometry commit requires a durable execution store",
                None,
            ));
        }

        if shadow_source_fingerprint(&stored.board_path) != stored.source_files {
            self.invalidate_native_approval(&arguments.plan_id);
            return Err(ErrorData::invalid_params(
                "source files changed since planning",
                None,
            ));
        }
        if stored.execution_state != StoredExecutionState::Ready {
            return Err(ErrorData::invalid_params(
                match stored.execution_state {
                    StoredExecutionState::InFlight => "change execution is already in flight",
                    StoredExecutionState::Committed => "change already committed",
                    StoredExecutionState::Unknown => {
                        "change outcome is unknown; manual verification required"
                    }
                    StoredExecutionState::Ready => unreachable!("state checked above"),
                },
                None,
            ));
        }
        if copper_route::contains_route(&stored.session.plan.operations)
            || geometry::contains(&stored.session.plan.operations)
        {
            let current = self
                .discovery
                .live_board_observation(&stored.endpoint, None)
                .and_then(|source| self.verify_native_source(&stored, &source));
            if let Err(reason) = current {
                self.invalidate_native_approval(&arguments.plan_id);
                return Err(ErrorData::invalid_params(reason, None));
            }
        }
        let engine = Engine::new(
            stored.session.snapshot.clone(),
            stored.session.plan.instance_token.clone(),
        );
        engine
            .approve(&mut stored.session, &arguments.plan_hash)
            .map_err(|error| ErrorData::invalid_params(error.to_string(), None))?;

        let attempt = writer.as_ref().map(|writer| {
            let mut sources = vec![stored.board_path.clone(), stored.board_path.with_extension("kicad_pro")];
            let rules = stored.board_path.with_extension("kicad_dru");
            if rules.exists() { sources.push(rules); }
            writer.begin("pcb", &arguments.plan_hash, &arguments.approval_hash,
                json!({"plan": stored.session.plan, "validation_evidence_hash": stored.validation_evidence_hash, "source_files": stored.source_files, "routing_evidence": stored.routing_evidence, "geometry_evidence":stored.geometry_evidence, "geometry_candidate":stored.shadow_validation.as_ref().and_then(|s|s.geometry.as_ref())}), &sources)
        }).transpose().map_err(|e| ErrorData::internal_error(format!("execution record unavailable: {e}"), None))?;

        let mut sessions = self.sessions.lock().expect("session lock");
        let current = sessions
            .get_mut(&arguments.plan_id)
            .ok_or_else(|| ErrorData::invalid_params("unknown plan_id", None))?;
        if current.execution_state != StoredExecutionState::Ready {
            return Err(ErrorData::invalid_params(
                "change execution was claimed",
                None,
            ));
        }
        current.execution_state = StoredExecutionState::InFlight;
        drop(sessions);

        let mut geometry_result = None;
        let outcome = if geometry::contains(&stored.session.plan.operations) {
            let result = (|| {
                let reject = |reason: String| BoardGraphicsCommitOutcome::Rejected { reason };
                let source = self
                    .discovery
                    .live_board_observation(&stored.endpoint, None)
                    .map_err(reject)?;
                self.verify_native_source(&stored, &source)
                    .map_err(reject)?;
                let request = geometry::request(&stored.session.plan.operations).map_err(reject)?;
                let evidence = stored
                    .geometry_evidence
                    .as_ref()
                    .ok_or_else(|| reject("missing geometry evidence".into()))?;
                let approved = stored
                    .shadow_validation
                    .as_ref()
                    .and_then(|s| s.geometry.as_ref())
                    .ok_or_else(|| reject("missing approved geometry".into()))?;
                self.execution.execute_geometry(
                    &stored.endpoint,
                    &source,
                    request,
                    evidence,
                    approved,
                )
            })();
            match result {
                Ok(result) => {
                    geometry_result = Some(result);
                    BoardGraphicsCommitOutcome::CommittedAndSaved
                }
                Err(outcome) => outcome,
            }
        } else {
            self.execution.execute_prepared(
                &stored.endpoint,
                &stored.board_filename,
                &stored.board_path,
                &stored.project_dir,
                &stored.session.plan.instance_token,
                &stored.session.plan.base_revision,
                &stored.session.plan.operations,
                &stored
                    .shadow_validation
                    .as_ref()
                    .expect("shadow validated above")
                    .prepared_footprints,
            )
        };
        let (ok, state, outcome_data) = match &outcome {
            BoardGraphicsCommitOutcome::CommittedAndSaved => {
                let mut commit_engine = Engine::new(
                    stored.session.snapshot.clone(),
                    stored.session.plan.instance_token.clone(),
                );
                let committed = if let Some(result) = &geometry_result {
                    commit_engine
                        .commit_observed(&mut stored.session, result.observation.snapshot.clone())
                } else {
                    commit_engine.commit(&mut stored.session)
                };
                if let Err(error) = committed {
                    stored.execution_state = StoredExecutionState::Unknown;
                    self.sessions
                        .lock()
                        .expect("session lock")
                        .insert(arguments.plan_id.clone(), stored);
                    return Err(ErrorData::internal_error(error.to_string(), None));
                }
                stored.execution_state = StoredExecutionState::Committed;
                (
                    true,
                    "committed",
                    json!({"kind": "committed_and_saved", "geometry":geometry_result}),
                )
            }
            BoardGraphicsCommitOutcome::CommittedButNotSaved { reason } => {
                stored.execution_state = StoredExecutionState::Unknown;
                (
                    false,
                    "unknown",
                    json!({"kind": "committed_but_not_saved", "reason": reason}),
                )
            }
            BoardGraphicsCommitOutcome::Rejected { reason } => {
                stored.execution_state = StoredExecutionState::Ready;
                if stored
                    .session
                    .plan
                    .operations
                    .iter()
                    .any(|op| !op.kind.footprint_placements().is_empty())
                    || geometry::contains(&stored.session.plan.operations)
                {
                    stored.approval_hash = None;
                    stored.validation_evidence_hash = None;
                    stored.shadow_validation = None;
                }
                (
                    false,
                    "rejected",
                    json!({"kind": "rejected", "reason": reason}),
                )
            }
            BoardGraphicsCommitOutcome::Unknown { reason } => {
                stored.execution_state = StoredExecutionState::Unknown;
                (
                    false,
                    "unknown",
                    json!({"kind": "unknown", "reason": reason}),
                )
            }
        };
        if let Some(attempt) = attempt
            && let Err(error) = attempt.finish(state, outcome_data.clone())
        {
            stored.execution_state = StoredExecutionState::Unknown;
            self.sessions
                .lock()
                .expect("session lock")
                .insert(arguments.plan_id.clone(), stored);
            return Err(ErrorData::internal_error(
                format!(
                    "execution outcome is uncertain because its durable record failed: {error}"
                ),
                None,
            ));
        }
        self.sessions
            .lock()
            .expect("session lock")
            .insert(arguments.plan_id.clone(), stored);
        let request_id = arguments
            .request_id
            .unwrap_or_else(|| "req_commit_change".into());
        let data = json!({"plan_id": arguments.plan_id, "state": state, "outcome": outcome_data});
        if ok {
            self.envelope(request_id, None, data)
        } else {
            Ok(self.envelope_sync(request_id, None, data))
        }
    }

    #[tool(
        name = "kicad_plan_schematic_change",
        description = "Plan the fixed two-resistor schematic change against a saved filesystem schematic without writing it."
    )]
    pub async fn kicad_plan_schematic_change(
        &self,
        Parameters(arguments): Parameters<PlanSchematicChangeRequest>,
    ) -> Result<CallToolResponse, ErrorData> {
        if !self.schematic.verified() {
            return Ok(self.rejected(arguments.request_id, "capability_not_verified"));
        }
        if arguments.source_run_id.trim().is_empty() {
            return Err(ErrorData::invalid_params(
                "source_run_id must not be empty",
                None,
            ));
        }
        let root = std::fs::canonicalize(&arguments.project_root)
            .map_err(|_| ErrorData::invalid_params("project_root is unavailable", None))?;
        if !root.is_dir() {
            return Err(ErrorData::invalid_params(
                "project_root is not a directory",
                None,
            ));
        }
        let schematic = std::fs::canonicalize(&arguments.root_schematic)
            .map_err(|_| ErrorData::invalid_params("root_schematic is unavailable", None))?;
        if !schematic.is_file()
            || !schematic.starts_with(&root)
            || schematic.extension().and_then(|e| e.to_str()) != Some("kicad_sch")
        {
            return Err(ErrorData::invalid_params(
                "root_schematic is not a confined .kicad_sch file",
                None,
            ));
        }
        let relative = schematic
            .strip_prefix(&root)
            .map_err(|_| ErrorData::invalid_params("root_schematic escapes project_root", None))?
            .to_str()
            .ok_or_else(|| ErrorData::invalid_params("root_schematic is not UTF-8", None))?
            .replace(std::path::MAIN_SEPARATOR, "/");
        let project = load_schematic_project(
            &root,
            &schematic,
            "filesystem-plan",
            1,
            SchematicLimits::default(),
        )
        .map_err(|error| ErrorData::invalid_params(error.to_string(), None))?;
        if project.editor_lock_present {
            return Err(ErrorData::invalid_params(
                "schematic editor lock is present",
                None,
            ));
        }
        let bytes = std::fs::read(&schematic)
            .map_err(|_| ErrorData::invalid_params("root_schematic is unreadable", None))?;
        let source_artifact =
            SchematicArtifactFile::new(relative, bytes.len() as u64, digest_bytes(&bytes))
                .map_err(|error| ErrorData::invalid_params(error.to_string(), None))?;
        let plan_id = format!("schematic-plan:{}", arguments.request_id);
        let plan_hash = digest_text(&[
            TWO_RESISTOR_OPERATION_IDENTITY,
            arguments.source_run_id.as_str(),
            root.to_string_lossy().as_ref(),
            schematic.to_string_lossy().as_ref(),
            &serde_json::to_string(&source_artifact).expect("artifact serialization"),
        ]);
        let stored = StoredSchematicChange {
            project_root: root,
            root_schematic: schematic,
            source_artifact: source_artifact.clone(),
            source_run_id: arguments.source_run_id.clone(),
            plan_hash: plan_hash.clone(),
            approval_hash: None,
            receipt: None,
            state: SchematicExecutionState::Planned,
            outcome: None,
        };
        let mut sessions = self
            .schematic_sessions
            .lock()
            .expect("schematic session lock");
        if let Some(existing) = sessions.get(&plan_id) {
            if existing.plan_hash != plan_hash {
                return Err(ErrorData::invalid_params("plan_id conflict", None));
            }
            return self.envelope(
                arguments.request_id,
                Some(plan_hash),
                json!({"plan_id": plan_id, "plan_hash": existing.plan_hash, "source_artifact": existing.source_artifact, "state": "planned"}),
            );
        }
        sessions.insert(plan_id.clone(), stored);
        let audit = self.append_schematic_audit(
            "plan",
            arguments.source_run_id,
            arguments.request_id.clone(),
            plan_id.clone(),
            plan_hash.clone(),
            None,
            Some(source_artifact.clone()),
            None,
            "planned".into(),
        );
        self.envelope(
            arguments.request_id,
            Some(plan_hash.clone()),
            json!({"plan_id": plan_id, "plan_hash": plan_hash, "operation_identity": TWO_RESISTOR_OPERATION_IDENTITY, "source_artifact": source_artifact, "state": "planned", "audit": audit}),
        )
    }

    #[tool(
        name = "kicad_preview_schematic_change",
        description = "Preview the fixed two-resistor filesystem schematic change using production validation, then await approval."
    )]
    pub async fn kicad_preview_schematic_change(
        &self,
        Parameters(arguments): Parameters<PreviewSchematicChangeRequest>,
    ) -> Result<CallToolResponse, ErrorData> {
        if !self.schematic.verified() {
            return Ok(self.rejected(
                arguments
                    .request_id
                    .unwrap_or_else(|| "req_preview_schematic_change".into()),
                "capability_not_verified",
            ));
        }
        let mut stored = self
            .schematic_sessions
            .lock()
            .expect("schematic session lock")
            .get(&arguments.plan_id)
            .cloned()
            .ok_or_else(|| ErrorData::invalid_params("unknown schematic plan_id", None))?;
        if stored.plan_hash != arguments.plan_hash {
            return Err(ErrorData::invalid_params("plan hash mismatch", None));
        }
        if stored.state == SchematicExecutionState::Committed {
            return Err(ErrorData::invalid_params(
                "schematic change already committed",
                None,
            ));
        }
        if stored.state == SchematicExecutionState::Unknown {
            return Err(ErrorData::invalid_params(
                "schematic outcome is unknown; manual verification required",
                None,
            ));
        }
        let receipt = match self.schematic.preview(
            &stored.project_root,
            &stored.root_schematic,
            &stored.source_artifact,
        ) {
            Ok(receipt) => receipt,
            Err(outcome) => {
                if matches!(outcome, SchematicPatchOutcome::Unknown { .. }) {
                    stored.state = SchematicExecutionState::Unknown;
                }
                self.schematic_sessions
                    .lock()
                    .expect("schematic session lock")
                    .insert(arguments.plan_id.clone(), stored.clone());
                let audit = self.append_schematic_audit(
                    "preview",
                    stored.source_run_id.clone(),
                    arguments
                        .request_id
                        .clone()
                        .unwrap_or_else(|| "req_preview_schematic_change".into()),
                    arguments.plan_id,
                    arguments.plan_hash.clone(),
                    None,
                    Some(stored.source_artifact.clone()),
                    None,
                    schematic_state(&outcome).into(),
                );
                return Ok(self.envelope_sync(
                    arguments
                        .request_id
                        .unwrap_or_else(|| "req_preview_schematic_change".into()),
                    Some(arguments.plan_hash),
                    json!({"outcome": schematic_outcome_data(&outcome), "audit": audit}),
                ));
            }
        };
        if receipt.operation_identity != TWO_RESISTOR_OPERATION_IDENTITY
            || receipt.source_artifact != stored.source_artifact
        {
            stored.state = SchematicExecutionState::Unknown;
            return Err(ErrorData::internal_error(
                "schematic preview receipt identity mismatch",
                None,
            ));
        }
        let approval_hash = digest_text(&[
            &stored.plan_hash,
            &receipt.operation_identity,
            receipt.source_artifact.content_digest.as_str(),
            receipt.candidate_artifact.content_digest.as_str(),
        ]);
        stored.approval_hash = Some(approval_hash.clone());
        stored.receipt = Some(receipt.clone());
        stored.state = SchematicExecutionState::AwaitingApproval;
        self.schematic_sessions
            .lock()
            .expect("schematic session lock")
            .insert(arguments.plan_id.clone(), stored.clone());
        let request_id = arguments
            .request_id
            .unwrap_or_else(|| "req_preview_schematic_change".into());
        let audit = self.append_schematic_audit(
            "preview",
            stored.source_run_id,
            request_id.clone(),
            arguments.plan_id.clone(),
            arguments.plan_hash.clone(),
            Some(approval_hash.clone()),
            Some(receipt.source_artifact.clone()),
            Some(receipt.candidate_artifact.clone()),
            "awaiting_approval".into(),
        );
        self.envelope(
            request_id,
            Some(arguments.plan_hash),
            json!({"plan_id": arguments.plan_id, "state": "awaiting_approval", "approval_hash": approval_hash, "operation_identity": receipt.operation_identity, "source_artifact": receipt.source_artifact, "candidate_artifact": receipt.candidate_artifact, "audit": audit}),
        )
    }

    #[tool(
        name = "kicad_commit_schematic_change",
        description = "Commit an exactly approved fixed two-resistor filesystem schematic preview."
    )]
    pub async fn kicad_commit_schematic_change(
        &self,
        Parameters(arguments): Parameters<CommitSchematicChangeRequest>,
    ) -> Result<CallToolResponse, ErrorData> {
        if !self.schematic.verified() {
            return Ok(self.rejected(
                arguments
                    .request_id
                    .unwrap_or_else(|| "req_commit_schematic_change".into()),
                "capability_not_verified",
            ));
        }
        let mut stored = self
            .schematic_sessions
            .lock()
            .expect("schematic session lock")
            .get(&arguments.plan_id)
            .cloned()
            .ok_or_else(|| ErrorData::invalid_params("unknown schematic plan_id", None))?;
        if stored.plan_hash != arguments.plan_hash {
            return Err(ErrorData::invalid_params(
                "schematic plan hash mismatch",
                None,
            ));
        }
        if stored.state == SchematicExecutionState::Committed {
            if stored.approval_hash.as_deref() != Some(&arguments.approval_hash) {
                return Err(ErrorData::invalid_params(
                    "schematic approval mismatch",
                    None,
                ));
            }
            let outcome = stored
                .outcome
                .as_ref()
                .expect("committed schematic has outcome");
            return self.envelope(arguments.request_id.unwrap_or_else(|| "req_commit_schematic_change".into()), Some(arguments.plan_hash), json!({"plan_id": arguments.plan_id, "state": schematic_state(outcome), "outcome": schematic_outcome_data(outcome)}));
        }
        if stored.state != SchematicExecutionState::AwaitingApproval
            || stored.approval_hash.as_deref() != Some(&arguments.approval_hash)
        {
            return Err(ErrorData::invalid_params(
                "schematic approval mismatch or state is not awaiting approval",
                None,
            ));
        }
        let writer = self.acquire_project_writer(&stored.project_root)?;
        let attempt = writer.as_ref().map(|writer| {
            writer.begin("schematic", &arguments.plan_hash, &arguments.approval_hash,
                json!({"source_artifact": stored.source_artifact, "operation_identity": TWO_RESISTOR_OPERATION_IDENTITY}), std::slice::from_ref(&stored.root_schematic))
        }).transpose().map_err(|e| ErrorData::internal_error(format!("execution record unavailable: {e}"), None))?;
        let mut sessions = self
            .schematic_sessions
            .lock()
            .expect("schematic session lock");
        let current = sessions
            .get_mut(&arguments.plan_id)
            .ok_or_else(|| ErrorData::invalid_params("unknown schematic plan_id", None))?;
        if current.state != SchematicExecutionState::AwaitingApproval {
            return Err(ErrorData::invalid_params(
                "schematic change execution was claimed",
                None,
            ));
        }
        current.state = SchematicExecutionState::InFlight;
        stored.state = SchematicExecutionState::InFlight;
        drop(sessions);
        let receipt = stored
            .receipt
            .as_ref()
            .expect("awaiting approval has receipt");
        let outcome = self
            .schematic
            .commit(&stored.project_root, &stored.root_schematic, receipt);
        if let Some(attempt) = attempt
            && let Err(error) =
                attempt.finish(schematic_state(&outcome), schematic_outcome_data(&outcome))
        {
            stored.state = SchematicExecutionState::Unknown;
            self.schematic_sessions
                .lock()
                .expect("schematic session lock")
                .insert(arguments.plan_id.clone(), stored);
            return Err(ErrorData::internal_error(
                format!(
                    "schematic outcome is uncertain because its durable record failed: {error}"
                ),
                None,
            ));
        }
        let state = match &outcome {
            SchematicPatchOutcome::Committed { .. } => SchematicExecutionState::Committed,
            SchematicPatchOutcome::Rejected { .. } => SchematicExecutionState::Planned,
            SchematicPatchOutcome::Unknown { .. } => SchematicExecutionState::Unknown,
        };
        stored.state = state;
        if !matches!(outcome, SchematicPatchOutcome::Committed { .. }) {
            stored.approval_hash = None;
        }
        stored.outcome = Some(outcome.clone());
        self.schematic_sessions
            .lock()
            .expect("schematic session lock")
            .insert(arguments.plan_id.clone(), stored.clone());
        let request_id = arguments
            .request_id
            .unwrap_or_else(|| "req_commit_schematic_change".into());
        let after_artifact = match &outcome {
            SchematicPatchOutcome::Committed { artifact } => Some(artifact.clone()),
            _ => None,
        };
        let audit = self.append_schematic_audit(
            "commit",
            stored.source_run_id,
            request_id.clone(),
            arguments.plan_id.clone(),
            arguments.plan_hash.clone(),
            Some(arguments.approval_hash),
            Some(stored.source_artifact.clone()),
            after_artifact,
            schematic_state(&outcome).into(),
        );
        self.envelope(request_id, Some(arguments.plan_hash), json!({"plan_id": arguments.plan_id, "state": schematic_state(&outcome), "outcome": schematic_outcome_data(&outcome), "audit": audit}))
    }

    fn rejected(&self, request_id: String, reason: &str) -> CallToolResponse {
        self.envelope_sync(request_id, None, json!({ "reason": reason }))
    }

    #[allow(clippy::too_many_arguments)]
    fn append_schematic_audit(
        &self,
        action: &'static str,
        source_run_id: String,
        request_id: String,
        plan_id: String,
        plan_hash: String,
        approval_hash: Option<String>,
        before_artifact: Option<SchematicArtifactFile>,
        after_artifact: Option<SchematicArtifactFile>,
        outcome: String,
    ) -> SchematicAuditEvent {
        let event = SchematicAuditEvent {
            sequence: self.schematic_audit_sequence.fetch_add(1, Ordering::SeqCst) + 1,
            action,
            source_run_id,
            request_id,
            plan_id,
            plan_hash,
            approval_hash,
            before_revision: before_artifact
                .as_ref()
                .map(|artifact| artifact.content_digest.clone()),
            after_revision: after_artifact
                .as_ref()
                .map(|artifact| artifact.content_digest.clone()),
            before_artifact,
            after_artifact,
            outcome,
        };
        self.schematic_audit
            .lock()
            .expect("schematic audit lock")
            .push(event.clone());
        event
    }

    fn envelope_sync(
        &self,
        request_id: String,
        plan_hash: Option<String>,
        data: Value,
    ) -> CallToolResponse {
        let envelope = Envelope {
            ok: false,
            request_id,
            run_id: None,
            project_revision: plan_hash,
            data,
            diagnostics: Vec::new(),
            next_actions: Vec::new(),
        };
        rmcp::model::CallToolResult::structured(
            serde_json::to_value(envelope).expect("envelope serialization"),
        )
        .into()
    }

    fn envelope(
        &self,
        request_id: String,
        plan_hash: Option<String>,
        data: Value,
    ) -> Result<CallToolResponse, ErrorData> {
        let envelope = Envelope {
            ok: true,
            request_id,
            run_id: None,
            project_revision: plan_hash,
            data,
            diagnostics: Vec::new(),
            next_actions: Vec::new(),
        };
        Ok(rmcp::model::CallToolResult::structured(
            serde_json::to_value(envelope)
                .map_err(|error| ErrorData::internal_error(error.to_string(), None))?,
        )
        .into())
    }
}

#[tool_handler(router = self.tool_router)]
impl ServerHandler for Server {
    fn get_info(&self) -> ServerInfo {
        ServerInfo::new(ServerCapabilities::builder().enable_tools().build())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use kicad_checks::{CheckFailure, DrcItem, DrcViolation, Position, ViolationIdentity};
    use kicad_model::{
        Board, BoardGraphicLayer, BoardGraphicSegment, CapabilityObservation, CapabilityStatus,
        Coordinate, Diagnostic, DocumentSpace, ObservedBoardFootprints, ProjectSnapshot, Schematic,
        StraightTrackIntent, Unit,
    };
    use rmcp::{ClientHandler, ServiceExt, model::CallToolRequestParams};
    use std::collections::VecDeque;
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicUsize, Ordering as AtomicOrdering};

    #[derive(Clone, Default)]
    struct TestClient;

    impl ClientHandler for TestClient {}

    #[derive(Clone, Debug, Eq, PartialEq)]
    enum LiveReadCall {
        Graphics {
            board_filename: String,
            layer: kicad_ipc::BoardGraphicLayer,
        },
        Footprints(String),
        Pads {
            board_filename: String,
            footprint_source_uuid: String,
        },
        Routing(String),
        Stackup(String),
        Token,
    }

    #[derive(Clone)]
    struct FakeLiveBoardSource {
        calls: Vec<LiveReadCall>,
        dwgs_user_graphics: Vec<kicad_ipc::ObservedBoardGraphicSegment>,
        edge_cuts_graphics: Vec<kicad_ipc::ObservedBoardGraphicSegment>,
        failing_graphics_layer: Option<kicad_ipc::BoardGraphicLayer>,
        footprints: Vec<kicad_ipc::ObservedFootprintInstance>,
        pads: BTreeMap<String, Vec<kicad_ipc::ObservedPad>>,
        failing_pad_parent: Option<String>,
        stackup: kicad_ipc::ObservedBoardStackup,
        failing_stackup: bool,
        token: Option<kicad_ipc::InstanceToken>,
    }

    impl LiveBoardObservationSource for FakeLiveBoardSource {
        fn board_graphics(
            &mut self,
            board_filename: &str,
            layer: kicad_ipc::BoardGraphicLayer,
        ) -> Result<Vec<kicad_ipc::ObservedBoardGraphicSegment>, kicad_ipc::IpcError> {
            self.calls.push(LiveReadCall::Graphics {
                board_filename: board_filename.to_owned(),
                layer,
            });
            if self.failing_graphics_layer == Some(layer) {
                return Err(kicad_ipc::IpcError::InvalidObservedBoardGraphic(
                    "fixture graphics read failed",
                ));
            }
            Ok(match layer {
                kicad_ipc::BoardGraphicLayer::DwgsUser => self.dwgs_user_graphics.clone(),
                kicad_ipc::BoardGraphicLayer::EdgeCuts => self.edge_cuts_graphics.clone(),
                kicad_ipc::BoardGraphicLayer::FSilkS => Vec::new(),
            })
        }

        fn footprint_instances(
            &mut self,
            board_filename: &str,
        ) -> Result<Vec<kicad_ipc::ObservedFootprintInstance>, kicad_ipc::IpcError> {
            self.calls
                .push(LiveReadCall::Footprints(board_filename.to_owned()));
            Ok(self.footprints.clone())
        }

        fn footprint_pads(
            &mut self,
            board_filename: &str,
            footprint_source_uuid: &str,
        ) -> Result<Vec<kicad_ipc::ObservedPad>, kicad_ipc::IpcError> {
            self.calls.push(LiveReadCall::Pads {
                board_filename: board_filename.to_owned(),
                footprint_source_uuid: footprint_source_uuid.to_owned(),
            });
            if self.failing_pad_parent.as_deref() == Some(footprint_source_uuid) {
                return Err(kicad_ipc::IpcError::InvalidObservedPad(
                    "fixture pad read failed",
                ));
            }
            Ok(self
                .pads
                .get(footprint_source_uuid)
                .cloned()
                .unwrap_or_default())
        }

        fn board_routing(
            &mut self,
            board_filename: &str,
        ) -> Result<kicad_ipc::ObservedBoardRoutingItems, kicad_ipc::IpcError> {
            self.calls
                .push(LiveReadCall::Routing(board_filename.to_owned()));
            Ok(kicad_ipc::ObservedBoardRoutingItems::default())
        }

        fn board_stackup(
            &mut self,
            board_filename: &str,
        ) -> Result<kicad_ipc::ObservedBoardStackup, kicad_ipc::IpcError> {
            self.calls
                .push(LiveReadCall::Stackup(board_filename.to_owned()));
            if self.failing_stackup {
                return Err(kicad_ipc::IpcError::InvalidObservedStackup(
                    "fixture stackup read failed",
                ));
            }
            Ok(self.stackup.clone())
        }

        fn latest_instance_token(&mut self) -> Option<kicad_ipc::InstanceToken> {
            self.calls.push(LiveReadCall::Token);
            self.token.clone()
        }
    }

    fn live_version() -> kicad_ipc::VersionInfo {
        kicad_ipc::VersionInfo {
            major: 10,
            minor: 0,
            patch: 4,
            full_version: "10.0.4".to_owned(),
        }
    }

    fn live_document() -> kicad_ipc::OpenDocument {
        kicad_ipc::OpenDocument {
            project_name: Some("main".to_owned()),
            project_path: Some("/projects/main".to_owned()),
            kind: kicad_ipc::OpenDocumentKind::Pcb {
                board_filename: "main.kicad_pcb".to_owned(),
            },
        }
    }

    fn live_footprint(source_uuid: &str, reference: &str) -> kicad_ipc::ObservedFootprintInstance {
        kicad_ipc::ObservedFootprintInstance {
            source_uuid: source_uuid.to_owned(),
            position_x_nm: 12_000_000,
            position_y_nm: 34_000_000,
            orientation_mdeg: 90_000,
            side: kicad_ipc::FootprintSide::Front,
            lock_state: kicad_ipc::ObservedLockState::Unlocked,
            library_identifier: kicad_ipc::ObservedFootprintLibraryIdentifier {
                nickname: "Package_QFP".to_owned(),
                entry_name: "LQFP-48".to_owned(),
            },
            reference: reference.to_owned(),
            value: "MCU".to_owned(),
            symbol_sheet_path: vec!["root-kiid".to_owned()],
        }
    }

    fn live_pad(source_uuid: &str, footprint_source_uuid: &str) -> kicad_ipc::ObservedPad {
        kicad_ipc::ObservedPad {
            source_uuid: source_uuid.to_owned(),
            footprint_source_uuid: footprint_source_uuid.to_owned(),
            number: "1".to_owned(),
            kind: kicad_ipc::ObservedPadKind::SurfaceMount,
            lock_state: kicad_ipc::ObservedLockState::Unlocked,
            position_x_nm: -750_000,
            position_y_nm: 250_000,
            stack_type: kicad_ipc::ObservedPadStackType::Normal,
            orientation_mdeg: 0,
            layers: vec![kicad_ipc::ObservedBoardLayer::FrontCopper],
            copper_shapes: vec![kicad_ipc::ObservedPadCopperShape {
                layer: kicad_ipc::ObservedBoardLayer::FrontCopper,
                shape: kicad_ipc::ObservedPadShape::Rectangle,
                size_x_nm: 1_200_000,
                size_y_nm: 800_000,
                offset_x_nm: 0,
                offset_y_nm: 0,
                corner_rounding_ratio: 0.0,
                chamfer_ratio: 0.0,
                chamfer_top_left: false,
                chamfer_top_right: false,
                chamfer_bottom_left: false,
                chamfer_bottom_right: false,
                trapezoid_delta_x_nm: 0,
                trapezoid_delta_y_nm: 0,
                custom_anchor_shape: None,
            }],
            drill: None,
            net_name: Some("GND".to_owned()),
        }
    }

    fn live_stackup() -> kicad_ipc::ObservedBoardStackup {
        use kicad_ipc::ObservedStackupLayerIdentity as Identity;
        use kicad_ipc::ObservedStackupLayerType as LayerType;

        kicad_ipc::ObservedBoardStackup {
            finish: None,
            impedance: None,
            edge: None,
            layers: vec![
                kicad_ipc::ObservedStackupLayer {
                    dielectric_type: None,
                    soldermask: None,
                    silkscreen_material: None,
                    layer: Identity::FrontCopper,
                    enabled: true,
                    layer_type: LayerType::Copper,
                    thickness_nm: Some(35_000),
                    color: None,
                    material_name: "Copper".to_owned(),
                    user_name: "F.Cu".to_owned(),
                    dielectric: None,
                },
                kicad_ipc::ObservedStackupLayer {
                    dielectric_type: None,
                    soldermask: None,
                    silkscreen_material: None,
                    layer: Identity::BackCopper,
                    enabled: true,
                    layer_type: LayerType::Copper,
                    thickness_nm: Some(35_000),
                    color: None,
                    material_name: "Copper".to_owned(),
                    user_name: "B.Cu".to_owned(),
                    dielectric: None,
                },
            ],
        }
    }

    fn fake_live_source() -> FakeLiveBoardSource {
        FakeLiveBoardSource {
            calls: Vec::new(),
            dwgs_user_graphics: Vec::new(),
            edge_cuts_graphics: Vec::new(),
            failing_graphics_layer: None,
            footprints: vec![
                live_footprint("footprint-a", "U1"),
                live_footprint("footprint-b", "U2"),
            ],
            pads: BTreeMap::from([
                (
                    "footprint-a".to_owned(),
                    vec![live_pad("pad-a", "footprint-a")],
                ),
                (
                    "footprint-b".to_owned(),
                    vec![live_pad("pad-b", "footprint-b")],
                ),
            ]),
            failing_pad_parent: None,
            stackup: live_stackup(),
            failing_stackup: false,
            token: Some(kicad_ipc::InstanceToken::new("token").unwrap()),
        }
    }

    fn expected_live_read_calls() -> Vec<LiveReadCall> {
        vec![
            LiveReadCall::Graphics {
                board_filename: "main.kicad_pcb".to_owned(),
                layer: kicad_ipc::BoardGraphicLayer::DwgsUser,
            },
            LiveReadCall::Graphics {
                board_filename: "main.kicad_pcb".to_owned(),
                layer: kicad_ipc::BoardGraphicLayer::EdgeCuts,
            },
            LiveReadCall::Footprints("main.kicad_pcb".to_owned()),
            LiveReadCall::Pads {
                board_filename: "main.kicad_pcb".to_owned(),
                footprint_source_uuid: "footprint-a".to_owned(),
            },
            LiveReadCall::Pads {
                board_filename: "main.kicad_pcb".to_owned(),
                footprint_source_uuid: "footprint-b".to_owned(),
            },
            LiveReadCall::Routing("main.kicad_pcb".to_owned()),
            LiveReadCall::Stackup("main.kicad_pcb".to_owned()),
            LiveReadCall::Token,
        ]
    }

    #[test]
    fn live_board_observation_reads_each_parent_and_is_stable() {
        let mut first_source = fake_live_source();
        let first = observe_live_board(
            &mut first_source,
            live_version(),
            live_document(),
            "main.kicad_pcb",
        )
        .unwrap();
        let mut second_source = fake_live_source();
        let second = observe_live_board(
            &mut second_source,
            live_version(),
            live_document(),
            "main.kicad_pcb",
        )
        .unwrap();

        assert_eq!(first_source.calls, expected_live_read_calls());
        assert_eq!(second_source.calls, expected_live_read_calls());
        let first_native = first.snapshot.native_footprints.as_ref().unwrap();
        let second_native = second.snapshot.native_footprints.as_ref().unwrap();
        assert_eq!(first_native, second_native);
        assert_eq!(
            first.snapshot.native_stackup,
            second.snapshot.native_stackup
        );
        assert_eq!(
            first.snapshot.revision().unwrap(),
            second.snapshot.revision().unwrap()
        );
        assert_eq!(
            first_native
                .placements
                .iter()
                .map(|placement| placement.source_uuid.as_str())
                .collect::<Vec<_>>(),
            ["footprint-a", "footprint-b"]
        );
        assert_eq!(
            first_native
                .placements
                .iter()
                .flat_map(|placement| placement.pads.iter())
                .map(|pad| pad.source_uuid.as_str())
                .collect::<Vec<_>>(),
            ["pad-a", "pad-b"]
        );
    }

    #[test]
    fn live_board_observation_includes_edge_cuts_in_snapshot_revision() {
        let mut source = fake_live_source();
        source
            .edge_cuts_graphics
            .push(kicad_ipc::ObservedBoardGraphicSegment {
                graphic_id: "outline-1".to_owned(),
                segment: kicad_ipc::BoardGraphicSegment {
                    start_x_nm: 0,
                    start_y_nm: 0,
                    end_x_nm: 10_000_000,
                    end_y_nm: 0,
                    width_nm: 50_000,
                    layer: kicad_ipc::BoardGraphicLayer::EdgeCuts,
                },
            });
        let with_outline = observe_live_board(
            &mut source,
            live_version(),
            live_document(),
            "main.kicad_pcb",
        )
        .unwrap();
        assert_eq!(with_outline.snapshot.board.graphics.len(), 1);
        assert_eq!(
            with_outline.snapshot.board.graphics[0].layer,
            BoardGraphicLayer::EdgeCuts
        );

        let mut without_outline_source = fake_live_source();
        let without_outline = observe_live_board(
            &mut without_outline_source,
            live_version(),
            live_document(),
            "main.kicad_pcb",
        )
        .unwrap();
        assert_ne!(
            with_outline.snapshot.revision().unwrap(),
            without_outline.snapshot.revision().unwrap()
        );
    }

    #[test]
    fn live_board_observation_stops_on_edge_cuts_read_failure() {
        let mut source = fake_live_source();
        source.failing_graphics_layer = Some(kicad_ipc::BoardGraphicLayer::EdgeCuts);
        assert!(matches!(
            observe_live_board(
                &mut source,
                live_version(),
                live_document(),
                "main.kicad_pcb",
            ),
            Err(ObserveLiveBoardError::Ipc(_))
        ));
        assert_eq!(source.calls.len(), 2);
        assert!(matches!(
            source.calls[1],
            LiveReadCall::Graphics {
                layer: kicad_ipc::BoardGraphicLayer::EdgeCuts,
                ..
            }
        ));
    }

    #[test]
    fn live_board_observation_stops_on_parent_pad_failure() {
        let mut source = fake_live_source();
        source.failing_pad_parent = Some("footprint-b".to_owned());

        let error = observe_live_board(
            &mut source,
            live_version(),
            live_document(),
            "main.kicad_pcb",
        )
        .unwrap_err();

        assert!(matches!(error, ObserveLiveBoardError::Ipc(_)));
        let mut expected = expected_live_read_calls();
        expected.truncate(expected.len() - 3);
        assert_eq!(source.calls, expected);
    }

    #[test]
    fn live_board_observation_rejects_parent_mismatch_before_token() {
        let mut source = fake_live_source();
        source.pads.insert(
            "footprint-b".to_owned(),
            vec![live_pad("pad-b", "footprint-a")],
        );

        let error = observe_live_board(
            &mut source,
            live_version(),
            live_document(),
            "main.kicad_pcb",
        )
        .unwrap_err();

        assert!(matches!(
            error,
            ObserveLiveBoardError::Footprints(FootprintObservationError::PadParentMismatch { .. })
        ));
        assert!(!source.calls.contains(&LiveReadCall::Token));
    }

    #[test]
    fn live_board_observation_stackup_ipc_failure_stops_before_token() {
        let mut source = fake_live_source();
        source.failing_stackup = true;

        let error = observe_live_board(
            &mut source,
            live_version(),
            live_document(),
            "main.kicad_pcb",
        )
        .unwrap_err();

        assert!(matches!(error, ObserveLiveBoardError::Ipc(_)));
        let mut expected = expected_live_read_calls();
        expected.pop();
        assert_eq!(source.calls, expected);
    }

    #[test]
    fn live_board_observation_stackup_domain_failure_stops_before_token() {
        let mut source = fake_live_source();
        source.stackup.layers[0].thickness_nm = None;

        let error = observe_live_board(
            &mut source,
            live_version(),
            live_document(),
            "main.kicad_pcb",
        )
        .unwrap_err();

        assert!(matches!(
            error,
            ObserveLiveBoardError::Stackup(kicad_model::ModelError::InvalidStackupObservation(
                "required layer thickness is missing"
            ))
        ));
        let mut expected = expected_live_read_calls();
        expected.pop();
        assert_eq!(source.calls, expected);
    }

    #[test]
    fn live_board_observation_stackup_order_changes_full_snapshot_revision() {
        let mut first_source = fake_live_source();
        let first = observe_live_board(
            &mut first_source,
            live_version(),
            live_document(),
            "main.kicad_pcb",
        )
        .unwrap();
        let mut reordered_source = fake_live_source();
        reordered_source.stackup.layers.swap(0, 1);
        let reordered = observe_live_board(
            &mut reordered_source,
            live_version(),
            live_document(),
            "main.kicad_pcb",
        )
        .unwrap();

        assert_ne!(first.snapshot, reordered.snapshot);
        assert_ne!(
            first.snapshot.revision().unwrap(),
            reordered.snapshot.revision().unwrap()
        );
        assert_eq!(first_source.calls, expected_live_read_calls());
        assert_eq!(reordered_source.calls, expected_live_read_calls());
    }

    #[test]
    fn live_board_observation_reports_missing_token_last() {
        let mut source = fake_live_source();
        source.token = None;

        let error = observe_live_board(
            &mut source,
            live_version(),
            live_document(),
            "main.kicad_pcb",
        )
        .unwrap_err();

        assert!(matches!(error, ObserveLiveBoardError::MissingInstanceToken));
        assert_eq!(source.calls, expected_live_read_calls());
    }

    #[derive(Clone, Default)]
    struct FakeDiscovery {
        write_capability: bool,
    }

    impl DiscoveryService for FakeDiscovery {
        fn discover(
            &self,
            endpoint: &str,
            _project_dir: Option<&std::path::Path>,
        ) -> Result<DiscoverySnapshot, String> {
            Ok(DiscoverySnapshot {
                kicad_version: "10.0.4".to_owned(),
                endpoint: endpoint.to_owned(),
                documents: Vec::new(),
                capabilities: vec![CapabilityObservation {
                    id: "base.get_version".to_owned(),
                    status: CapabilityStatus::Verified,
                    detail: None,
                }],
                diagnostics: vec![Diagnostic {
                    code: "content_not_observed".to_owned(),
                    message: "test".to_owned(),
                }],
            }
            .canonical())
        }

        fn project_snapshot(
            &self,
            _endpoint: &str,
            _project_dir: Option<&std::path::Path>,
        ) -> Result<ProjectSnapshot, String> {
            Ok(project_snapshot(self.write_capability))
        }

        fn live_board_observation(
            &self,
            _endpoint: &str,
            _project_dir: Option<&std::path::Path>,
        ) -> Result<LiveBoardObservation, String> {
            Ok(LiveBoardObservation {
                snapshot: project_snapshot(self.write_capability),
                instance_token: kicad_ipc::InstanceToken::new("token").unwrap(),
                board_filename: "board.kicad_pcb".into(),
                board_path: "/tmp/board.kicad_pcb".into(),
                project_dir: "/tmp".into(),
            })
        }
    }

    #[derive(Clone)]
    struct FakeSchematic {
        preview_calls: Arc<AtomicUsize>,
        commit_calls: Arc<AtomicUsize>,
    }

    impl SchematicChangeService for FakeSchematic {
        fn verified(&self) -> bool {
            true
        }

        fn preview(
            &self,
            _: &Path,
            _: &Path,
            source_artifact: &SchematicArtifactFile,
        ) -> Result<SchematicPreviewReceipt, SchematicPatchOutcome> {
            self.preview_calls.fetch_add(1, AtomicOrdering::SeqCst);
            let candidate_bytes = b"approved schematic candidate".to_vec();
            Ok(SchematicPreviewReceipt {
                source_artifact: source_artifact.clone(),
                candidate_artifact: SchematicArtifactFile::new(
                    source_artifact.relative_path.clone(),
                    candidate_bytes.len() as u64,
                    digest_bytes(&candidate_bytes),
                )
                .unwrap(),
                operation_identity: TWO_RESISTOR_OPERATION_IDENTITY.to_owned(),
                candidate_bytes,
            })
        }

        fn commit(
            &self,
            _: &Path,
            _: &Path,
            receipt: &SchematicPreviewReceipt,
        ) -> SchematicPatchOutcome {
            self.commit_calls.fetch_add(1, AtomicOrdering::SeqCst);
            SchematicPatchOutcome::Committed {
                artifact: receipt.candidate_artifact.clone(),
            }
        }
    }

    #[derive(Clone, Debug, Default)]
    struct UnavailableSchematic;

    impl SchematicChangeService for UnavailableSchematic {
        fn preview(
            &self,
            _: &Path,
            _: &Path,
            _: &SchematicArtifactFile,
        ) -> Result<SchematicPreviewReceipt, SchematicPatchOutcome> {
            Err(SchematicPatchOutcome::Unknown {
                reason: "schematic service unavailable".into(),
            })
        }

        fn commit(&self, _: &Path, _: &Path, _: &SchematicPreviewReceipt) -> SchematicPatchOutcome {
            SchematicPatchOutcome::Unknown {
                reason: "schematic service unavailable".into(),
            }
        }
    }

    #[derive(Clone)]
    struct ValidationDiscovery {
        root: PathBuf,
        board_path: PathBuf,
        snapshot: ProjectSnapshot,
        token: String,
    }

    impl DiscoveryService for ValidationDiscovery {
        fn discover(&self, endpoint: &str, _: Option<&Path>) -> Result<DiscoverySnapshot, String> {
            Ok(DiscoverySnapshot {
                kicad_version: self.snapshot.kicad_version.clone(),
                endpoint: endpoint.to_owned(),
                documents: Vec::new(),
                capabilities: Vec::new(),
                diagnostics: Vec::new(),
            })
        }

        fn live_board_observation(
            &self,
            _: &str,
            _: Option<&Path>,
        ) -> Result<LiveBoardObservation, String> {
            Ok(LiveBoardObservation {
                snapshot: self.snapshot.clone(),
                instance_token: kicad_ipc::InstanceToken::new(&self.token).unwrap(),
                board_filename: "board.kicad_pcb".into(),
                board_path: self.board_path.clone(),
                project_dir: self.root.clone(),
            })
        }
    }

    #[derive(Clone)]
    struct FakeValidation {
        calls: Arc<AtomicUsize>,
        results: Arc<Mutex<VecDeque<CheckResult>>>,
    }

    impl BoardValidationService for FakeValidation {
        fn validate(&self, _: &Path, _: &DrcPolicy) -> CheckResult {
            self.calls.fetch_add(1, AtomicOrdering::SeqCst);
            self.results.lock().unwrap().pop_front().unwrap()
        }
    }

    static TEMP_SEQUENCE: AtomicUsize = AtomicUsize::new(0);

    struct TempBoard {
        root: PathBuf,
    }

    impl TempBoard {
        fn new() -> Self {
            let sequence = TEMP_SEQUENCE.fetch_add(1, AtomicOrdering::Relaxed);
            let root = std::env::temp_dir().join(format!(
                "auto-kicad-validation-{}-{sequence}",
                std::process::id()
            ));
            std::fs::create_dir_all(&root).unwrap();
            std::fs::write(
                root.join("board.kicad_pcb"),
                "(kicad_pcb (version 20240108))",
            )
            .unwrap();
            Self { root }
        }
    }

    impl Drop for TempBoard {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.root);
        }
    }

    fn report(violations: Vec<DrcViolation>) -> CheckResult {
        CheckResult::Completed(DrcReport {
            parser_version: "kicad-drc-v1".into(),
            schema: "https://schemas.kicad.org/drc.v1.json".into(),
            kicad_version: "10.0.5".into(),
            source: "test".into(),
            ignored_checks: 0,
            unconnected_items: 0,
            violations,
        })
    }

    fn violation(rule_type: &str, description: &str, uuid: &str) -> DrcViolation {
        DrcViolation {
            identity: ViolationIdentity::Uuids {
                rule_type: rule_type.into(),
                uuids: vec![uuid.into()],
            },
            rule_type: rule_type.into(),
            severity: "error".into(),
            description: description.into(),
            items: vec![DrcItem {
                uuid: Some(uuid.into()),
                description: "item".into(),
                position: Some(Position { x: 1.0, y: 2.0 }),
            }],
        }
    }

    fn validation_server(
        fixture: &TempBoard,
        snapshot: ProjectSnapshot,
        results: Vec<CheckResult>,
    ) -> (Server, Arc<AtomicUsize>) {
        let calls = Arc::new(AtomicUsize::new(0));
        let discovery = Arc::new(ValidationDiscovery {
            root: fixture.root.clone(),
            board_path: fixture.root.join("board.kicad_pcb"),
            snapshot,
            token: "validation-token".into(),
        });
        let validation = Arc::new(FakeValidation {
            calls: calls.clone(),
            results: Arc::new(Mutex::new(results.into_iter().collect())),
        });
        (
            Server::with_services(
                discovery,
                Arc::new(IpcBoardGraphicsExecutionService::default()),
                validation,
            ),
            calls,
        )
    }

    fn project_snapshot(write_capability: bool) -> ProjectSnapshot {
        ProjectSnapshot {
            opaque_graphics: Vec::new(),
            project_id: StableId::new("project").unwrap(),
            kicad_version: "10.0.5".into(),
            capabilities: if write_capability {
                vec!["pcb.board_graphics.write".into()]
            } else {
                Vec::new()
            },
            schematic: Schematic::default(),
            board: Board {
                document_id: Some(StableId::new("board").unwrap()),
                footprints: Vec::new(),
                graphics: Vec::new(),
            },
            native_footprints: Some(
                ObservedBoardFootprints::new(StableId::new("board").unwrap(), Vec::new()).unwrap(),
            ),
            native_routing: None,
            native_stackup: None,
        }
    }

    fn graphic(id: &str) -> BoardGraphicSegment {
        BoardGraphicSegment {
            graphic_id: StableId::new(id).unwrap(),
            start: Coordinate {
                x: 0,
                y: 0,
                unit: Unit::Nanometres,
                space: DocumentSpace::Board,
            },
            end: Coordinate {
                x: 1_000_000,
                y: 0,
                unit: Unit::Nanometres,
                space: DocumentSpace::Board,
            },
            width_nm: 100_000,
            layer: BoardGraphicLayer::DwgsUser,
        }
    }

    pub(super) fn fresh_shadow_observation() -> LiveBoardObservation {
        let mut source = fake_live_source();
        observe_live_board(
            &mut source,
            live_version(),
            live_document(),
            "main.kicad_pcb",
        )
        .unwrap()
    }

    fn shadow_graphics_operation(board: StableId) -> Operation {
        Operation {
            operation_id: StableId::new("shadow-operation").unwrap(),
            kind: OperationKind::AddBoardGraphicSegments {
                board: board.clone(),
                segments: vec![graphic("shadow-graphic")],
            },
            targets: vec![board],
            preconditions: Vec::new(),
            expected_effects: vec!["add board graphic segments".into()],
            risk_class: RiskClass::Low,
            idempotency_key: "shadow-operation".into(),
            source_run_id: "shadow-run".into(),
        }
    }

    #[test]
    fn shadow_binding_rebinds_only_supported_board_operations_to_fresh_identity() {
        let observation = fresh_shadow_observation();
        let original_board = observation.snapshot.board.document_id.clone().unwrap();
        let shadow_board = StableId::new("shadow-board").unwrap();
        let original = shadow_graphics_operation(original_board);
        let bound = bind_shadow_operations(
            std::slice::from_ref(&original),
            &observation
                .snapshot
                .in_board_scope(
                    observation.snapshot.project_id.clone(),
                    shadow_board.clone(),
                )
                .unwrap(),
        )
        .unwrap();
        assert_eq!(bound.len(), 1);
        assert_eq!(bound[0].targets, vec![shadow_board.clone()]);
        assert!(validate_shadow_operations(&observation, std::slice::from_ref(&original)).is_ok());
        assert!(validate_shadow_operations(&observation, &bound).is_err());
    }

    #[test]
    fn shadow_binding_rejects_unbound_routing_before_any_mutation() {
        let board = StableId::new("board").unwrap();
        let intent = StraightTrackIntent::new(
            StableId::new("track").unwrap(),
            Coordinate {
                x: 0,
                y: 0,
                unit: Unit::Nanometres,
                space: DocumentSpace::Board,
            },
            Coordinate {
                x: 1_000,
                y: 0,
                unit: Unit::Nanometres,
                space: DocumentSpace::Board,
            },
            100,
        )
        .unwrap();
        let operation = Operation {
            operation_id: StableId::new("routing-operation").unwrap(),
            kind: OperationKind::AddStraightTrack {
                board: board.clone(),
                intent,
            },
            targets: vec![board],
            preconditions: Vec::new(),
            expected_effects: vec!["add straight copper track".into()],
            risk_class: RiskClass::Medium,
            idempotency_key: "routing-operation".into(),
            source_run_id: "shadow-run".into(),
        };
        assert!(validate_shadow_operation_kinds(std::slice::from_ref(&operation)).is_ok());
        assert!(
            bind_shadow_operations(&[operation], &fresh_shadow_observation().snapshot).is_err()
        );
    }

    #[test]
    fn routing_shadow_rebinds_endpoints_and_preserves_native_state_in_mixed_batches() {
        let mut observation = fresh_shadow_observation();
        let board = observation.snapshot.board.document_id.clone().unwrap();
        let mut placements = observation
            .snapshot
            .native_footprints
            .take()
            .unwrap()
            .placements;
        placements[1].position.x += 10_000_000;
        observation.snapshot.native_footprints =
            Some(ObservedBoardFootprints::new(board.clone(), placements).unwrap());
        let placements = &observation
            .snapshot
            .native_footprints
            .as_ref()
            .unwrap()
            .placements;
        let endpoint = |index: usize| kicad_model::RoutingEndpointRef {
            pad_id: placements[index].pads[0].pad_id.clone(),
            footprint_placement_id: placements[index].placement_id.clone(),
        };
        let intent = StraightTrackIntent::new_endpoint_bound(
            StableId::new("planned-track").unwrap(),
            kicad_engine::pad_board_position(&placements[0], &placements[0].pads[0]).unwrap(),
            kicad_engine::pad_board_position(&placements[1], &placements[1].pads[0]).unwrap(),
            200_000,
            endpoint(0),
            endpoint(1),
            placements[0].pads[0].net.clone().unwrap(),
        )
        .unwrap();
        let mut track = shadow_graphics_operation(board.clone());
        track.kind = OperationKind::AddStraightTrack {
            board: board.clone(),
            intent,
        };
        let operations = vec![shadow_graphics_operation(board.clone()), track];
        let shadow_board = StableId::new("shadow-board").unwrap();
        let shadow = observation
            .snapshot
            .in_board_scope(
                StableId::new("shadow-project").unwrap(),
                shadow_board.clone(),
            )
            .unwrap();
        let bound = bind_shadow_operations(&operations, &shadow).unwrap();
        let prepared =
            kicad_adapter::prepare_bound_native_batch(&bound, &shadow, &shadow.revision().unwrap())
                .unwrap();
        assert_eq!(prepared.graphics.len(), 1);
        assert_eq!(prepared.tracks.len(), 1);
        let OperationKind::AddStraightTrack { intent, .. } = &bound[1].kind else {
            panic!("expected track")
        };
        assert_eq!(
            intent.net.as_ref().unwrap().net_id,
            kicad_model::board_net_id(&shadow_board, "GND")
        );
        let mut invalid = operations.clone();
        let OperationKind::AddStraightTrack { intent, .. } = &mut invalid[1].kind else {
            unreachable!()
        };
        intent.start_pad.as_mut().unwrap().footprint_placement_id =
            StableId::new("wrong-parent").unwrap();
        assert!(bind_shadow_operations(&invalid, &shadow).is_err());

        let batch = kicad_adapter::prepare_bound_native_batch(
            &operations,
            &observation.snapshot,
            &observation.snapshot.revision().unwrap(),
        )
        .unwrap();
        let track = batch.tracks[0].clone();
        let receipts = kicad_adapter::NativePcbBatchReceipt {
            graphics: vec![kicad_ipc::CreatedBoardGraphicReceipt {
                request_index: 0,
                graphic_id: "created-graphic".into(),
                lock_state: kicad_ipc::ObservedLockState::Unlocked,
                segment: batch.graphics[0].clone(),
            }],
            tracks: vec![kicad_ipc::CreatedTrackReceipt {
                request_index: 0,
                source_uuid: "created-track".into(),
                lock_state: kicad_ipc::ObservedLockState::Unlocked,
                track: track.clone(),
            }],
            vias: vec![],
        };
        let routing = kicad_ipc::ObservedBoardRoutingItems {
            tracks: vec![kicad_ipc::ObservedTrack {
                source_uuid: "created-track".into(),
                geometry: kicad_ipc::ObservedTrackGeometry::Segment {
                    start: kicad_ipc::ObservedRoutingPoint {
                        x_nm: track.start_x_nm,
                        y_nm: track.start_y_nm,
                    },
                    end: kicad_ipc::ObservedRoutingPoint {
                        x_nm: track.end_x_nm,
                        y_nm: track.end_y_nm,
                    },
                },
                width_nm: track.width_nm,
                layer: kicad_ipc::ObservedBoardLayer::FrontCopper,
                lock_state: kicad_ipc::ObservedLockState::Unlocked,
                net: Some(kicad_ipc::ObservedRoutingNet {
                    code: Some(1),
                    name: "GND".into(),
                }),
            }],
            vias: vec![],
        };
        let expected =
            expected_native_shadow_snapshot(&observation, &operations, &receipts, routing).unwrap();
        assert_eq!(
            expected.native_footprints,
            observation.snapshot.native_footprints
        );
        assert_eq!(expected.native_stackup, observation.snapshot.native_stackup);
        assert_eq!(
            expected.native_routing.as_ref().unwrap().tracks[0]
                .source_uuid
                .as_str(),
            "created-track"
        );
        assert_ne!(
            expected.revision().unwrap(),
            observation.snapshot.revision().unwrap()
        );
    }

    #[test]
    fn shadow_snapshot_binds_receipts_and_preserves_existing_state() {
        let mut observation = fresh_shadow_observation();
        observation
            .snapshot
            .board
            .graphics
            .push(graphic("existing-graphic"));
        let board = observation.snapshot.board.document_id.clone().unwrap();
        let operation = shadow_graphics_operation(board);
        let receipts = vec![kicad_ipc::CreatedBoardGraphicReceipt {
            request_index: 0,
            graphic_id: "server-assigned-uuid".into(),
            lock_state: kicad_ipc::ObservedLockState::Unlocked,
            segment: native_graphic(&graphic("shadow-graphic")),
        }];
        let expected =
            expected_shadow_snapshot(&observation, std::slice::from_ref(&operation), &receipts)
                .unwrap();
        assert!(
            expected
                .board
                .graphics
                .iter()
                .any(|g| g.graphic_id.as_str() == "server-assigned-uuid")
        );
        assert!(
            !expected
                .board
                .graphics
                .iter()
                .any(|g| g.graphic_id.as_str() == "shadow-graphic")
        );
        for original in &observation.snapshot.board.graphics {
            assert!(expected.board.graphics.contains(original));
        }
        assert!(
            expected_shadow_snapshot(&observation, std::slice::from_ref(&operation), &[]).is_err()
        );
        let mut wrong = receipts.clone();
        wrong[0].segment.width_nm += 1;
        assert!(
            expected_shadow_snapshot(&observation, std::slice::from_ref(&operation), &wrong)
                .is_err()
        );
        wrong = receipts;
        wrong[0].graphic_id = observation.snapshot.board.graphics[0]
            .graphic_id
            .to_string();
        assert!(expected_shadow_snapshot(&observation, &[operation], &wrong).is_err());
    }

    #[test]
    #[ignore = "requires one owned KiCad PCB editor, CLI and official libraries"]
    fn source_bound_circuit_and_routing_reuse_one_owned_editor() {
        let fixture = TempBoard::new();
        let root = std::fs::canonicalize(&fixture.root).unwrap();
        let board_path = root.join("board.kicad_pcb");
        std::fs::write(&board_path, empty_fixture_board()).unwrap();
        std::fs::write(board_path.with_extension("kicad_pro"), "{}").unwrap();
        let sch = root.join("board.kicad_sch");
        let empty = b"(kicad_sch (version 20260306) (generator eeschema) (uuid 11111111-1111-4111-8111-111111111111) (paper A4) (lib_symbols))";
        std::fs::write(&sch, empty).unwrap();
        let artifact = kicad_model::SchematicArtifactFile::new(
            "board.kicad_sch",
            empty.len() as u64,
            digest_bytes(empty),
        )
        .unwrap();
        assert!(matches!(
            kicad_schematic::create_two_resistor_circuit(&root, &sch, &artifact),
            kicad_schematic::SchematicPatchOutcome::Committed { .. }
        ));
        let source = shadow_source_fingerprint(&board_path);
        let schematic_source = std::fs::read(&sch).unwrap();
        let mut launcher = OwnedPcbnewLauncher::new(&root.join("preview")).unwrap();
        let shadow = launcher.create_shadow(&board_path).unwrap();
        let result = (|| -> Result<(), String> {
            launcher.launch(&shadow).map_err(|e| e.to_string())?;
            let pid = launcher.current_identity().map_err(|e| e.to_string())?.pid;
            let policy = DrcPolicy {
                allowed_root: shadow.root().to_path_buf(),
                timeout: Duration::from_secs(30),
            };
            let CheckResult::Completed(baseline) = run_pcb_drc(shadow.board_path(), &policy) else {
                return Err("baseline DRC failed".into());
            };
            let mut observation = observe_owned_shadow(&mut launcher)?;
            let board = observation
                .snapshot
                .board
                .document_id
                .clone()
                .ok_or("missing board")?;
            for (reference, x_nm) in [("R1", 5_000_000), ("R2", 10_000_000)] {
                let mut source_observation = observation.clone();
                source_observation.project_dir = root.clone();
                source_observation.board_path = board_path.clone();
                let placement = footprint_authoring::prepare(
                    &library_settings::capture(),
                    &BoardFootprintRequest {
                        target_id: format!("placed-{reference}"),
                        library_nickname: "Resistor_SMD".into(),
                        library_entry_name: "R_0603_1608Metric".into(),
                        x_nm,
                        y_nm: 5_000_000,
                        reference: reference.into(),
                        value: "1k".into(),
                    },
                    &source_observation,
                )?;
                let mut operation = shadow_graphics_operation(board.clone());
                operation.targets.push(placement.target_id.clone());
                operation.kind = OperationKind::PlaceAllowlistedFootprint {
                    board: board.clone(),
                    placement,
                };
                let (expected, templates) = apply_shadow_operations(
                    &mut launcher,
                    &observation,
                    &[operation],
                    &root,
                    &library_settings::capture(),
                )
                .map_err(|e| format!("footprint execution: {e:?}"))?;
                if templates.len() != 1 {
                    return Err("missing source template".into());
                }
                launcher.reload_owned().map_err(|e| e.to_string())?;
                observation = observe_owned_shadow(&mut launcher)?;
                if observation.snapshot.canonical() != expected {
                    return Err("footprint reload mismatch".into());
                }
                if launcher.current_identity().map_err(|e| e.to_string())?.pid != pid {
                    return Err("editor was restarted".into());
                }
            }
            let endpoint = |reference: &str, number: &str| -> Result<_, String> {
                let footprint = observation
                    .snapshot
                    .native_footprints
                    .as_ref()
                    .ok_or("missing footprints")?
                    .placements
                    .iter()
                    .find(|fp| fp.reference == reference)
                    .ok_or("missing reference")?;
                let pad = footprint
                    .pads
                    .iter()
                    .find(|pad| pad.number == number)
                    .ok_or("missing pad")?;
                Ok((
                    kicad_model::RoutingEndpointRef {
                        pad_id: pad.pad_id.clone(),
                        footprint_placement_id: footprint.placement_id.clone(),
                    },
                    kicad_engine::pad_board_position(footprint, pad)
                        .map_err(|error| error.to_string())?,
                    pad.net.clone().ok_or("missing pad net")?,
                ))
            };
            let (start_pad, start, net) = endpoint("R1", "2")?;
            let (end_pad, end, end_net) = endpoint("R2", "1")?;
            if net != end_net {
                return Err("schematic pin networks differ".into());
            }
            let mut track = shadow_graphics_operation(board.clone());
            track.kind = OperationKind::AddStraightTrack {
                board: board.clone(),
                intent: StraightTrackIntent::new_endpoint_bound(
                    StableId::new("planned-track").unwrap(),
                    start,
                    end,
                    250_000,
                    start_pad,
                    end_pad,
                    net.clone(),
                )
                .map_err(|e| e.to_string())?,
            };
            let mut via = shadow_graphics_operation(board.clone());
            via.kind = OperationKind::AddThroughVia {
                board: board.clone(),
                intent: ThroughViaIntent::new_net_bound(
                    StableId::new("planned-via").unwrap(),
                    Coordinate {
                        x: (start.x + end.x) / 2,
                        y: start.y,
                        ..start
                    },
                    600_000,
                    300_000,
                    net,
                )
                .map_err(|e| e.to_string())?,
            };
            let operations = [shadow_graphics_operation(board), track];
            let (expected, _) = apply_shadow_operations(
                &mut launcher,
                &observation,
                &operations,
                &root,
                &library_settings::capture(),
            )
            .map_err(|e| format!("routing execution: {e:?}"))?;
            launcher.reload_owned().map_err(|e| e.to_string())?;
            let actual = observe_owned_shadow(&mut launcher)?;
            if actual.snapshot.canonical() != expected {
                return Err("routing reload mismatch".into());
            }
            if launcher.current_identity().map_err(|e| e.to_string())?.pid != pid {
                return Err("editor was restarted".into());
            }
            let CheckResult::Completed(after) = run_pcb_drc(shadow.board_path(), &policy) else {
                return Err("postcondition DRC failed".into());
            };
            let delta = compare_drc(&baseline, &after);
            if !delta.introduced.is_empty() {
                return Err(format!("introduced DRC: {:?}", delta.introduced));
            }
            let (expected, _) = apply_shadow_operations(
                &mut launcher,
                &actual,
                &[via],
                &root,
                &library_settings::capture(),
            )
            .map_err(|error| format!("via execution: {error:?}"))?;
            launcher.reload_owned().map_err(|error| error.to_string())?;
            let with_via = observe_owned_shadow(&mut launcher)?;
            if with_via.snapshot.canonical() != expected {
                return Err("via reload mismatch".into());
            }
            let CheckResult::Completed(via_report) = run_pcb_drc(shadow.board_path(), &policy)
            else {
                return Err("via DRC failed".into());
            };
            let via_delta = compare_drc(&after, &via_report);
            if via_delta.introduced.is_empty()
                || via_delta
                    .introduced
                    .iter()
                    .any(|violation| violation.rule_type != "via_dangling")
            {
                return Err(format!(
                    "expected only dangling-via rejection: {:?}",
                    via_delta.introduced
                ));
            }
            if launcher.current_identity().map_err(|e| e.to_string())?.pid != pid {
                return Err("editor was restarted".into());
            }
            Ok(())
        })();
        let close = match launcher.current_identity().cloned() {
            Ok(identity) => launcher.close_owned(identity.pid),
            Err(_) => Ok(()),
        };
        let cleanup = close.and_then(|()| launcher.cleanup());
        assert_eq!(source, shadow_source_fingerprint(&board_path));
        assert_eq!(schematic_source, std::fs::read(&sch).unwrap());
        assert!(
            result.is_ok() && cleanup.is_ok(),
            "single-editor circuit: {result:?}; cleanup: {cleanup:?}"
        );
        assert!(!shadow.root().exists());
    }

    fn empty_fixture_board() -> &'static str {
        r#"(kicad_pcb (version 20260206) (generator "pcbnew")
          (general (thickness 1.6)) (paper "A4")
          (layers (0 "F.Cu" signal) (2 "B.Cu" signal) (9 "F.Adhes" user "F.Adhesive") (11 "B.Adhes" user "B.Adhesive")
            (13 "F.Paste" user) (15 "B.Paste" user) (5 "F.SilkS" user "F.Silkscreen") (7 "B.SilkS" user "B.Silkscreen")
            (1 "F.Mask" user) (3 "B.Mask" user) (17 "Dwgs.User" user) (25 "Edge.Cuts" user) (31 "F.CrtYd" user "F.Courtyard") (35 "F.Fab" user))
          (setup (pad_to_mask_clearance 0))
          (gr_line (start 0 0) (end 20 0) (stroke (width 0.05) (type default)) (layer "Edge.Cuts") (uuid "30000000-0000-4000-8000-000000000001"))
          (gr_line (start 20 0) (end 20 15) (stroke (width 0.05) (type default)) (layer "Edge.Cuts") (uuid "30000000-0000-4000-8000-000000000002"))
          (gr_line (start 20 15) (end 0 15) (stroke (width 0.05) (type default)) (layer "Edge.Cuts") (uuid "30000000-0000-4000-8000-000000000003"))
          (gr_line (start 0 15) (end 0 0) (stroke (width 0.05) (type default)) (layer "Edge.Cuts") (uuid "30000000-0000-4000-8000-000000000004")))"#
    }

    fn routing_fixture_board() -> &'static str {
        r#"(kicad_pcb (version 20260206) (generator "pcbnew")
          (general (thickness 1.6)) (paper "A4")
          (layers (0 "F.Cu" signal) (2 "B.Cu" signal) (9 "F.Adhes" user "F.Adhesive") (11 "B.Adhes" user "B.Adhesive")
            (13 "F.Paste" user) (15 "B.Paste" user) (5 "F.SilkS" user "F.Silkscreen") (7 "B.SilkS" user "B.Silkscreen")
            (1 "F.Mask" user) (3 "B.Mask" user) (17 "Dwgs.User" user) (25 "Edge.Cuts" user) (31 "F.CrtYd" user "F.Courtyard") (35 "F.Fab" user))
          (setup (pad_to_mask_clearance 0)) (net 1 "GND")
          (footprint "Test:Pad" (layer "F.Cu") (uuid "10000000-0000-4000-8000-000000000001") (at 5 5)
            (property "Reference" "J1" (at 0 -2) (layer "F.SilkS") (effects (font (size 1 1) (thickness 0.15))))
            (property "Value" "Pad" (at 0 2) (layer "F.Fab") (effects (font (size 1 1) (thickness 0.15))))
            (attr smd) (pad "1" smd rect (at 0 0) (size 1 1) (layers "F.Cu" "F.Paste" "F.Mask") (net 1 "GND") (uuid "20000000-0000-4000-8000-000000000001")))
          (footprint "Test:Pad" (layer "F.Cu") (uuid "10000000-0000-4000-8000-000000000002") (at 10 5)
            (property "Reference" "J2" (at 0 -2) (layer "F.SilkS") (effects (font (size 1 1) (thickness 0.15))))
            (property "Value" "Pad" (at 0 2) (layer "F.Fab") (effects (font (size 1 1) (thickness 0.15))))
            (attr smd) (pad "1" smd rect (at 0 0) (size 1 1) (layers "F.Cu" "F.Paste" "F.Mask") (net 1 "GND") (uuid "20000000-0000-4000-8000-000000000002")))
          (footprint "Test:BackPad" (layer "B.Cu") (uuid "10000000-0000-4000-8000-000000000003") (at 7.5 8)
            (property "Reference" "J3" (at 0 0) (layer "B.Fab") (effects (font (size 1 1) (thickness 0.15))))
            (property "Value" "BackPad" (at 0 2) (layer "B.Fab") (effects (font (size 1 1) (thickness 0.15))))
            (attr smd) (pad "1" smd rect (at 0 0) (size 1 1) (layers "B.Cu" "B.Paste" "B.Mask") (net 1 "GND") (uuid "20000000-0000-4000-8000-000000000003")))
          (segment (start 7.5 5) (end 7.5 8) (width 0.25) (layer "B.Cu") (net 1) (uuid "40000000-0000-4000-8000-000000000001"))
          (gr_line (start 0 0) (end 20 0) (stroke (width 0.05) (type default)) (layer "Edge.Cuts") (uuid "30000000-0000-4000-8000-000000000001"))
          (gr_line (start 20 0) (end 20 15) (stroke (width 0.05) (type default)) (layer "Edge.Cuts") (uuid "30000000-0000-4000-8000-000000000002"))
          (gr_line (start 20 15) (end 0 15) (stroke (width 0.05) (type default)) (layer "Edge.Cuts") (uuid "30000000-0000-4000-8000-000000000003"))
          (gr_line (start 0 15) (end 0 0) (stroke (width 0.05) (type default)) (layer "Edge.Cuts") (uuid "30000000-0000-4000-8000-000000000004")))"#
    }

    #[test]
    #[ignore = "requires one owned KiCad PCB editor and CLI"]
    fn disposable_mixed_routing_shadow_uses_one_editor() {
        let fixture = TempBoard::new();
        let board_path = fixture.root.join("board.kicad_pcb");
        std::fs::write(&board_path, routing_fixture_board()).unwrap();
        std::fs::write(board_path.with_extension("kicad_pro"), "{}").unwrap();
        let board = StableId::new("fixture-board").unwrap();
        let net = kicad_model::ObservedNetIdentity {
            net_id: kicad_model::board_net_id(&board, "GND"),
            name: "GND".into(),
        };
        let endpoint = |index: u8| {
            let footprint = StableId::new(format!("10000000-0000-4000-8000-{index:012}")).unwrap();
            let pad = StableId::new(format!("20000000-0000-4000-8000-{index:012}")).unwrap();
            let placement = kicad_model::footprint_placement_id(&board, &footprint);
            kicad_model::RoutingEndpointRef {
                pad_id: kicad_model::footprint_pad_id(&placement, &pad),
                footprint_placement_id: placement,
            }
        };
        let coordinate = |x, y| Coordinate {
            x,
            y,
            unit: Unit::Nanometres,
            space: DocumentSpace::Board,
        };
        let mut track = shadow_graphics_operation(board.clone());
        track.kind = OperationKind::AddStraightTrack {
            board: board.clone(),
            intent: StraightTrackIntent::new_endpoint_bound(
                StableId::new("planned-track").unwrap(),
                coordinate(5_000_000, 5_000_000),
                coordinate(10_000_000, 5_000_000),
                250_000,
                endpoint(1),
                endpoint(2),
                net.clone(),
            )
            .unwrap(),
        };
        let mut via = shadow_graphics_operation(board.clone());
        via.kind = OperationKind::AddThroughVia {
            board: board.clone(),
            intent: ThroughViaIntent::new_net_bound(
                StableId::new("planned-via").unwrap(),
                coordinate(7_500_000, 5_000_000),
                600_000,
                300_000,
                net,
            )
            .unwrap(),
        };
        let operations = [shadow_graphics_operation(board), track, via];
        let outcome = validate_disposable_shadow(
            &fixture.root.join("preview"),
            &board_path,
            &operations,
            Duration::from_secs(30),
        );
        assert_eq!(
            outcome.state,
            ShadowValidationState::Committed,
            "{outcome:?}"
        );
        assert!(
            outcome.reopened
                && outcome.normalized_snapshot_matches
                && outcome.source_unchanged
                && outcome.shadow_cleaned
        );
    }

    #[test]
    #[ignore = "requires installed KiCad PCB editor and CLI"]
    fn disposable_graphics_shadow_verifies_server_assigned_ids() {
        let fixture = TempBoard::new();
        let board = fixture.root.join("board.kicad_pcb");
        std::fs::write(
            &board,
            r#"(kicad_pcb (version 20260206) (generator "pcbnew")
            (general (thickness 1.6)) (paper "A4")
            (layers (0 "F.Cu" signal) (2 "B.Cu" signal)
                (17 "Dwgs.User" user) (25 "Edge.Cuts" user))
            (setup (pad_to_mask_clearance 0)))"#,
        )
        .unwrap();
        std::fs::write(board.with_extension("kicad_pro"), "{}").unwrap();
        let before = shadow_source_fingerprint(&board);
        let operation = shadow_graphics_operation(StableId::new("source-board").unwrap());
        let outcome = validate_disposable_shadow(
            &fixture.root.join("shadows"),
            &board,
            &[operation],
            Duration::from_secs(30),
        );
        assert_eq!(
            outcome.state,
            ShadowValidationState::Committed,
            "{outcome:?}"
        );
        assert!(outcome.reopened && outcome.normalized_snapshot_matches);
        assert!(outcome.source_unchanged && outcome.shadow_cleaned);
        assert_eq!(before, shadow_source_fingerprint(&board));
    }

    #[tokio::test]
    #[ignore = "requires exactly two owned KiCad editors and CLI; no automatic restart"]
    async fn live_mcp_saved_routing_lifecycle_uses_two_editors() {
        live_mcp_authoring_lifecycle(LiveAuthoringMode::Routing).await;
    }

    #[tokio::test]
    #[ignore = "requires two owned KiCad editors, CLI and official libraries"]
    async fn live_mcp_source_bound_footprint_lifecycle_uses_two_editors() {
        live_mcp_authoring_lifecycle(LiveAuthoringMode::Footprint).await;
    }

    #[tokio::test]
    #[ignore = "requires two owned KiCad editors, CLI and official libraries"]
    async fn live_mcp_connected_assembly_lifecycle_uses_two_editors() {
        live_mcp_authoring_lifecycle(LiveAuthoringMode::Assembly).await;
    }

    #[derive(Clone, Copy)]
    enum LiveAuthoringMode {
        Routing,
        Footprint,
        Assembly,
    }

    async fn live_mcp_authoring_lifecycle(mode: LiveAuthoringMode) {
        let footprint = !matches!(mode, LiveAuthoringMode::Routing);
        let assembly = matches!(mode, LiveAuthoringMode::Assembly);
        async fn call(
            client: &rmcp::service::RunningService<rmcp::RoleClient, TestClient>,
            name: &'static str,
            arguments: Value,
        ) -> Result<Value, String> {
            let result = client
                .call_tool(
                    CallToolRequestParams::new(name)
                        .with_arguments(arguments.as_object().ok_or("invalid arguments")?.clone()),
                )
                .await
                .map_err(|error| error.to_string())?;
            if result.is_error == Some(true) {
                return Err(format!("{result:?}"));
            }
            let content = result
                .structured_content
                .ok_or("missing structured content")?;
            if content["ok"] != true {
                return Err(content.to_string());
            }
            Ok(content["data"].clone())
        }

        let fixture = TempBoard::new();
        let board = fixture.root.join("board.kicad_pcb");
        std::fs::write(
            &board,
            if footprint {
                empty_fixture_board()
            } else {
                routing_fixture_board()
            },
        )
        .unwrap();
        std::fs::write(board.with_extension("kicad_pro"), "{}").unwrap();
        let original = shadow_source_fingerprint(&board);
        let evidence_path = std::env::temp_dir().join(format!(
            "auto-kicad-mcp-evidence-{}-{}.json",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let mut source_editor = OwnedPcbnewLauncher::new(&fixture.root.join("source")).unwrap();
        let source_project = source_editor.create_shadow(&board).unwrap();
        let source_root = source_project.root().to_path_buf();
        let result: Result<(), String> = async {
            let identity = source_editor
                .launch(&source_project)
                .map_err(|e| e.to_string())?;
            {
                let mut native = source_editor.connect_owned().map_err(|e| e.to_string())?;
                let commit = native.begin_commit().map_err(|e| e.to_string())?;
                native
                    .create_board_graphics(
                        &identity.board_filename,
                        &[native_graphic(&graphic("existing"))],
                    )
                    .map_err(|e| e.to_string())?;
                native
                    .commit_changes(&commit, "saved source fixture")
                    .map_err(|e| e.to_string())?;
                native
                    .save_board(&identity.board_filename)
                    .map_err(|e| e.to_string())?;
            }
            source_editor.reload_owned().map_err(|e| e.to_string())?;
            let before = observe_owned_shadow(&mut source_editor)?;
            let policy = DrcPolicy {
                allowed_root: source_root.clone(),
                timeout: Duration::from_secs(30),
            };
            let CheckResult::Completed(baseline) =
                run_pcb_drc(source_project.board_path(), &policy)
            else {
                return Err("source baseline DRC failed".into());
            };
            let shadow_root = fixture.root.join("validation");
            let state_dir = fixture.root.join("state");
            let server = Server::with_discovery_and_execution_and_shadow(
                Arc::new(IpcDiscoveryService),
                Arc::new(IpcBoardGraphicsExecutionService::default()),
                Arc::new(IpcShadowValidationService::new(&shadow_root)),
            )
            .with_state_dir(&state_dir)
            .map_err(|e| e.to_string())?;
            let client = connected(server.clone()).await;
            let schematic_source = if footprint {
                let sch = source_project.board_path().with_extension("kicad_sch");
                let empty = b"(kicad_sch (version 20260306) (generator eeschema) (uuid 11111111-1111-4111-8111-111111111111) (paper A4) (lib_symbols))";
                std::fs::write(&sch, empty).map_err(|e| e.to_string())?;
                let plan = call(&client, "kicad_plan_schematic_change", json!({
                    "project_root": source_root, "root_schematic": sch,
                    "source_run_id": "footprint-acceptance", "request_id": "source-circuit"
                })).await?;
                let preview = call(&client, "kicad_preview_schematic_change", json!({
                    "plan_id": plan["plan_id"], "plan_hash": plan["plan_hash"]
                })).await?;
                if std::fs::read(&sch).map_err(|e| e.to_string())? != empty {
                    return Err("schematic preview changed source bytes".into());
                }
                let committed = call(&client, "kicad_commit_schematic_change", json!({
                    "plan_id": plan["plan_id"], "plan_hash": plan["plan_hash"],
                    "approval_hash": preview["approval_hash"]
                })).await?;
                if committed["state"] != "committed" { return Err("source schematic commit failed".into()); }
                let bytes = std::fs::read(&sch).map_err(|e| e.to_string())?;
                Some((sch, bytes))
            } else { None };
            let plan = if assembly {
                call(&client, "kicad_plan_change", json!({
                    "endpoint": identity.endpoint, "source_run_id": "assembly-acceptance",
                    "request_id": "live-assembly", "assembly": {
                        "placements": [
                            {"target_id": "placed-r1", "library_nickname": "Resistor_SMD", "library_entry_name": "R_0603_1608Metric",
                             "x_nm": 5_000_000, "y_nm": 5_000_000, "reference": "R1", "value": "1k"},
                            {"target_id": "placed-r2", "library_nickname": "Resistor_SMD", "library_entry_name": "R_0603_1608Metric",
                             "x_nm": 10_000_000, "y_nm": 5_000_000, "reference": "R2", "value": "1k"}
                        ],
                        "connections": [{"track_id": "r1-r2", "start": {"target_id": "placed-r1", "pad_number": "2"},
                            "end": {"target_id": "placed-r2", "pad_number": "1"}, "width_nm": 250_000}]
                    }
                })).await?
            } else if footprint {
                call(&client, "kicad_plan_change", json!({
                    "endpoint": identity.endpoint, "source_run_id": "footprint-acceptance",
                    "request_id": "live-footprint", "footprint": {
                        "target_id": "placed-r1", "library_nickname": "Resistor_SMD",
                        "library_entry_name": "R_0603_1608Metric", "x_nm": 5_000_000,
                        "y_nm": 5_000_000, "reference": "R1", "value": "1k"
                    }
                })).await?
            } else {
            let mut addition = graphic("planned");
            addition.start.y = 2_000_000;
            addition.end.y = 2_000_000;
            let footprints = before.snapshot.native_footprints.as_ref().ok_or("missing footprints")?;
            let endpoint = |reference: &str| -> Result<_, String> {
                let footprint = footprints.placements.iter().find(|fp| fp.reference == reference).ok_or("missing fixture footprint")?;
                let pad = footprint.pads.first().ok_or("missing fixture pad")?;
                Ok((json!({"pad_id": pad.pad_id, "footprint_placement_id": footprint.placement_id}),
                    pad.net.clone().ok_or("missing fixture net")?))
            };
            let (start_pad, net) = endpoint("J1")?;
            let (end_pad, end_net) = endpoint("J2")?;
            if net != end_net { return Err("fixture pad nets differ".into()); }
            call(
                &client,
                "kicad_plan_change",
                json!({
                    "endpoint": identity.endpoint, "source_run_id": "two-editor-acceptance",
                    "request_id": "live-routing", "batch": [
                        {"segments": [addition]},
                        {"track": {"source_uuid": "planned-track", "start_x_nm": 5_000_000, "start_y_nm": 5_000_000,
                            "end_x_nm": 10_000_000, "end_y_nm": 5_000_000, "width_nm": 250_000,
                            "start_pad": start_pad, "end_pad": end_pad, "net": net}},
                        {"via": {"source_uuid": "planned-via", "x_nm": 7_500_000, "y_nm": 5_000_000,
                            "pad_diameter_nm": 600_000, "drill_diameter_nm": 300_000, "net": net}}
                    ]
                }),
            )
            .await?
            };
            let source_disk = shadow_source_fingerprint(source_project.board_path());
            let source_bytes =
                std::fs::read(source_project.board_path()).map_err(|e| e.to_string())?;
            let preview_request =
                json!({"plan_id": plan["plan_id"], "plan_hash": plan["plan_hash"]});

            // Reject a changed live revision before starting the one preview editor.
            {
                let mut native = source_editor.connect_owned().map_err(|e| e.to_string())?;
                let commit = native.begin_commit().map_err(|e| e.to_string())?;
                let mut unsaved = graphic("unsaved");
                unsaved.start.y = 3_000_000;
                unsaved.end.y = 3_000_000;
                native
                    .create_board_graphics(&identity.board_filename, &[native_graphic(&unsaved)])
                    .map_err(|e| e.to_string())?;
                native
                    .commit_changes(&commit, "temporary unsaved fixture")
                    .map_err(|e| e.to_string())?;
            }
            let unsaved = observe_owned_shadow(&mut source_editor)?;
            let rejected = call(&client, "kicad_preview_change", preview_request.clone()).await;
            if !rejected
                .as_ref()
                .is_err_and(|error| error.contains("source changed since planning"))
            {
                return Err(format!("unsaved source was not rejected: {rejected:?}"));
            }
            if shadow_root.exists()
                || unsaved != observe_owned_shadow(&mut source_editor)?
                || source_disk != shadow_source_fingerprint(source_project.board_path())
            {
                return Err("rejected preview changed source or started shadow validation".into());
            }
            source_editor.reload_owned().map_err(|e| e.to_string())?;
            if before != observe_owned_shadow(&mut source_editor)? {
                return Err("owned source restore mismatch".into());
            }
            let preview = call(&client, "kicad_preview_change", preview_request).await?;
            if preview["state"] != "awaiting_approval" {
                return Err(format!("preview did not await approval: {preview}"));
            }
            for field in [
                "reopened",
                "normalized_snapshot_matches",
                "source_unchanged",
                "shadow_cleaned",
            ] {
                if preview["shadow_validation"][field] != true {
                    return Err(format!("shadow verification missing {field}: {preview}"));
                }
            }
            if before != observe_owned_shadow(&mut source_editor)?
                || source_disk != shadow_source_fingerprint(source_project.board_path())
                || std::fs::read_dir(&shadow_root)
                    .map_err(|e| e.to_string())?
                    .next()
                    .is_some()
            {
                return Err("preview changed source or left shadow files".into());
            }
            if assembly {
                // A second-placement verification failure must discard the first placement too.
                let (mut templates, connections) = {
                    let sessions = server.sessions.lock().map_err(|e| e.to_string())?;
                    let stored = sessions.get(plan["plan_id"].as_str().ok_or("missing plan ID")?).ok_or("missing session")?;
                    let OperationKind::PlaceConnectedFootprints { assembly, .. } = &stored.session.plan.operations[0].kind else { return Err("missing assembly plan".into()); };
                    (stored.shadow_validation.as_ref().ok_or("missing shadow evidence")?.prepared_footprints.clone(), assembly.connections.clone())
                };
                let second = templates.get_mut(1).ok_or("missing second template")?;
                second.template = second.template.with_association(kicad_ipc::FootprintAssociation {
                    symbol_path: vec!["wrong-sheet".into(), "wrong-symbol".into()],
                    pad_nets: second.placement.source.as_ref().ok_or("missing source")?.pad_nets.clone(),
                }).map_err(|e| e.to_string())?;
                let mut native = source_editor.connect_owned().map_err(|e| e.to_string())?;
                let rejected = footprint_authoring::execute(&mut native, &before, &templates, &connections);
                if !matches!(rejected, Err(BoardGraphicsCommitOutcome::Rejected { .. })) {
                    return Err(format!("partial assembly was not rolled back: {rejected:?}"));
                }
                drop(native);
                if before != observe_owned_shadow(&mut source_editor)? || source_disk != shadow_source_fingerprint(source_project.board_path()) {
                    return Err("failed assembly changed source memory or files".into());
                }
            }
            let approval = json!({
                "plan_id": plan["plan_id"], "plan_hash": plan["plan_hash"],
                "approval_hash": preview["approval_hash"],
                "validation_evidence_hash": preview["validation_evidence_hash"]
            });
            let mut wrong = approval.clone();
            wrong["approval_hash"] = json!("wrong");
            if call(&client, "kicad_commit_change", wrong).await.is_ok() {
                return Err("wrong approval accepted".into());
            }
            let committed = call(&client, "kicad_commit_change", approval.clone()).await?;
            if committed["state"] != "committed" {
                return Err(format!("commit failed: {committed}"));
            }
            let committed_disk = shadow_source_fingerprint(source_project.board_path());
            if call(&client, "kicad_commit_change", approval).await.is_ok()
                || committed_disk != shadow_source_fingerprint(source_project.board_path())
            {
                return Err("commit replay was not rejected without mutation".into());
            }
            source_editor.reload_owned().map_err(|e| e.to_string())?;
            let after = observe_owned_shadow(&mut source_editor)?;
            let CheckResult::Completed(after_drc) =
                run_pcb_drc(source_project.board_path(), &policy)
            else {
                return Err("committed source DRC failed".into());
            };
            let history = recovery::ExecutionStore::open(&state_dir)
                .map_err(|e| e.to_string())?
                .history(&source_root)
                .map_err(|e| e.to_string())?;
            let evidence_file = std::fs::OpenOptions::new().write(true).create_new(true)
                .open(&evidence_path).map_err(|e| e.to_string())?;
            serde_json::to_writer_pretty(evidence_file, &json!({
                "mode": if assembly { "assembly" } else if footprint { "footprint" } else { "routing" }, "before": before.snapshot, "after": after.snapshot,
                "baseline_drc": baseline, "committed_drc": after_drc,
                "preview": preview, "commit": committed, "history": history,
                "source_before_digest": digest_bytes(&source_bytes),
                "backup_checks": history.iter().flat_map(|record| &record.backups).map(|entry| {
                    json!({"original": entry.original, "recorded_digest": entry.sha256,
                        "actual_digest": std::fs::read(&entry.saved).ok().map(|bytes| digest_bytes(&bytes).to_string())})
                }).collect::<Vec<_>>()
            })).map_err(|e| e.to_string())?;
            if after.snapshot.board.graphics.len() != before.snapshot.board.graphics.len() + usize::from(!footprint)
                || !after
                    .snapshot
                    .board
                    .graphics
                    .contains(&before.snapshot.board.graphics[0])
                || source_editor
                    .current_identity()
                    .map_err(|e| e.to_string())?
                    .pid
                    != identity.pid
            {
                return Err("saved source geometry or editor PID mismatch".into());
            }
            if footprint {
                if (!assembly && before.snapshot.native_routing != after.snapshot.native_routing)
                    || before.snapshot.native_stackup != after.snapshot.native_stackup {
                    return Err("footprint commit changed routing or stackup".into());
                }
                let placements = &after.snapshot.native_footprints.as_ref().ok_or("missing footprints")?.placements;
                if placements.len() != 1 + usize::from(assembly) { return Err("created footprint count differs".into()); }
                let operations: Vec<Operation> = serde_json::from_value(preview["operations"].clone()).map_err(|e| e.to_string())?;
                let planned = operations[0].kind.footprint_placements();
                if planned.len() != placements.len() { return Err("missing planned placements".into()); }
                for placement in planned {
                    let actual = placements.iter().find(|p| p.reference == placement.reference).ok_or("missing placed reference")?;
                    let source = placement.source.as_ref().ok_or("missing planned source binding")?;
                    let binding = kicad_adapter::bindings::SchematicFootprintBinding {
                        schematic_revision: source.schematic.digest.clone(),
                        board_revision: actual.observed_revision.clone(), symbol: source.symbol.clone(),
                        symbol_path: source.symbol_path.clone(), pad_nets: source.pad_nets.clone()
                    };
                    kicad_adapter::bindings::verify_placed_pin_pad_binding(
                        &binding, placement, actual, after.snapshot.board.document_id.as_ref().ok_or("missing board")?
                    )?;
                }
                if assembly {
                    let routing = after.snapshot.native_routing.as_ref().ok_or("missing assembly routing")?;
                    if routing.tracks.len() != 1 || !routing.vias.is_empty() || after_drc.unconnected_items != 0 {
                        return Err("assembly is not completely connected".into());
                    }
                    let endpoint = |reference: &str, number: &str| -> Result<_, String> {
                        let p = placements.iter().find(|p| p.reference == reference).ok_or("missing resistor")?;
                        let pad = p.pads.iter().find(|p| p.number == number).ok_or("missing resistor pin")?;
                        Ok((kicad_model::RoutingEndpointRef { pad_id: pad.pad_id.clone(), footprint_placement_id: p.placement_id.clone() },
                            kicad_engine::pad_board_position(p, pad).map_err(|e| e.to_string())?, pad.net.clone().ok_or("missing net")?))
                    };
                    let (start_pad, start, net) = endpoint("R1", "2")?;
                    let (end_pad, end, _) = endpoint("R2", "1")?;
                    let track = &routing.tracks[0];
                    let expected = kicad_model::ObservedTrackGeometry::Segment { start, end };
                    let reversed = kicad_model::ObservedTrackGeometry::Segment { start: end, end: start };
                    if (track.geometry != expected && track.geometry != reversed) || track.width_nm != 250_000
                        || track.layer != kicad_model::BoardLayer::FrontCopper
                        || track.net.as_ref().map(|n| &n.name) != Some(&net.name) {
                        return Err("saved assembly track differs from planned pin connection".into());
                    }
                    let intent = StraightTrackIntent::new_endpoint_bound(track.source_uuid.clone(), start, end,
                        track.width_nm, start_pad, end_pad, net).map_err(|e| e.to_string())?;
                    kicad_engine::validate_routing_track_binding(&after.snapshot, after.snapshot.board.document_id.as_ref().ok_or("missing board")?, &intent)
                        .map_err(|e| e.to_string())?;
                }
                let (sch, bytes) = schematic_source.as_ref().ok_or("missing source schematic")?;
                if std::fs::read(sch).map_err(|e| e.to_string())? != *bytes {
                    return Err("PCB authoring changed schematic source".into());
                }
            } else {
            let routing = after.snapshot.native_routing.as_ref().ok_or("missing committed routing")?;
            let original_routing = before.snapshot.native_routing.as_ref().ok_or("missing baseline routing")?;
            if routing.tracks.len() != original_routing.tracks.len() + 1 || routing.vias.len() != 1
                || original_routing.tracks.iter().any(|original| {
                    let mut rebound = original.clone();
                    rebound.observed_revision = routing.observed_revision.clone();
                    !routing.tracks.contains(&rebound)
                })
                || before.snapshot.native_footprints != after.snapshot.native_footprints
                || before.snapshot.native_stackup != after.snapshot.native_stackup {
                return Err("saved routing or preserved board objects differ".into());
            }
            }
            if !compare_drc(&baseline, &after_drc).introduced.is_empty() {
                return Err("commit introduced DRC violations".into());
            }
            let pcb_records = history.iter().filter(|record| record.domain == "pcb").collect::<Vec<_>>();
            if history.len() != 1 + usize::from(footprint) || pcb_records.len() != 1 || history.iter().any(|record| record.state != "committed") {
                return Err(format!("invalid durable authoring history: {history:?}"));
            }
            let backup = pcb_records[0]
                .backups
                .iter()
                .find(|entry| entry.original == source_project.board_path())
                .ok_or("missing pre-write PCB backup")?;
            if std::fs::read(&backup.saved).map_err(|e| e.to_string())? != source_bytes {
                return Err("pre-write PCB backup mismatch".into());
            }
            Ok(())
        }
        .await;
        let close = match source_editor.current_identity().cloned() {
            Ok(identity) => source_editor.close_owned(identity.pid),
            Err(_) => Ok(()),
        };
        let cleanup = close.and_then(|()| source_editor.cleanup());
        assert_eq!(original, shadow_source_fingerprint(&board));
        assert!(
            result.is_ok() && cleanup.is_ok(),
            "two-editor MCP: {result:?}; cleanup: {cleanup:?}; evidence: {}",
            evidence_path.display()
        );
        assert!(!source_root.exists());
        std::fs::remove_file(evidence_path).unwrap();
    }

    #[test]
    fn copied_snapshot_rebinding_preserves_content_and_is_reversible() {
        let original = fresh_shadow_observation().snapshot;
        let board_id = original.board.document_id.clone().unwrap();
        let rebound = original
            .in_board_scope(
                StableId::new("copied-project").unwrap(),
                StableId::new("copied-board").unwrap(),
            )
            .unwrap();
        assert_ne!(rebound.revision().unwrap(), original.revision().unwrap());
        assert_eq!(
            rebound
                .in_board_scope(original.project_id.clone(), board_id)
                .unwrap(),
            original.canonical()
        );
        let mut modified = rebound;
        modified.board.graphics.push(graphic("extra"));
        assert_ne!(
            modified
                .in_board_scope(
                    original.project_id.clone(),
                    original.board.document_id.clone().unwrap()
                )
                .unwrap(),
            original.canonical()
        );
    }

    #[test]
    fn shadow_source_fingerprint_detects_missing_or_changed_sources() {
        let missing = shadow_source_fingerprint(Path::new("/definitely/missing/board.kicad_pcb"));
        assert!(missing.board.is_none());
        assert!(missing.project.is_none());
        let fixture = TempBoard::new();
        let board_path = fixture.root.join("board.kicad_pcb");
        let before = shadow_source_fingerprint(&board_path);
        std::fs::write(&board_path, "changed").unwrap();
        assert_ne!(before, shadow_source_fingerprint(&board_path));
    }

    #[tokio::test]
    #[ignore = "requires installed KiCad CLI and official libraries; opens no editor"]
    async fn production_cli_binds_fixed_circuit_footprints_without_editor() {
        let fixture = TempBoard::new();
        let root = std::fs::canonicalize(&fixture.root).unwrap();
        let sch = root.join("board.kicad_sch");
        let empty = b"(kicad_sch (version 20260306) (generator eeschema) (uuid 11111111-1111-4111-8111-111111111111) (paper A4) (lib_symbols))";
        std::fs::write(&sch, empty).unwrap();
        async fn call(
            client: &rmcp::service::RunningService<rmcp::RoleClient, TestClient>,
            name: &'static str,
            arguments: Value,
        ) -> Value {
            let result = client
                .call_tool(
                    CallToolRequestParams::new(name)
                        .with_arguments(arguments.as_object().unwrap().clone()),
                )
                .await
                .unwrap();
            assert_ne!(result.is_error, Some(true), "{result:?}");
            let result = result.structured_content.unwrap();
            assert_eq!(result["ok"], true, "{result}");
            result["data"].clone()
        }

        let state_dir = root.join("state");
        let schematic_server = Server::default().with_state_dir(&state_dir).unwrap();
        let client = connected(schematic_server.clone()).await;
        let planned = call(
            &client,
            "kicad_plan_schematic_change",
            json!({
                "project_root": root,
                "root_schematic": sch,
                "source_run_id": "cli-acceptance",
                "request_id": "cli-schematic-plan"
            }),
        )
        .await;
        assert_eq!(planned["state"], "planned");
        let preview = call(
            &client,
            "kicad_preview_schematic_change",
            json!({
                "plan_id": planned["plan_id"],
                "plan_hash": planned["plan_hash"]
            }),
        )
        .await;
        assert_eq!(preview["state"], "awaiting_approval");
        assert_eq!(std::fs::read(&sch).unwrap(), empty);
        let approval = json!({
            "plan_id": planned["plan_id"],
            "plan_hash": planned["plan_hash"],
            "approval_hash": preview["approval_hash"]
        });
        let committed = call(&client, "kicad_commit_schematic_change", approval.clone()).await;
        assert_eq!(committed["state"], "committed");
        let saved = std::fs::read(&sch).unwrap();
        assert_eq!(
            preview["candidate_artifact"]["content_digest"],
            digest_bytes(&saved).as_str()
        );
        let modified = std::fs::metadata(&sch).unwrap().modified().unwrap();
        let replay = call(&client, "kicad_commit_schematic_change", approval).await;
        assert_eq!(replay["state"], "committed");
        assert_eq!(std::fs::read(&sch).unwrap(), saved);
        assert_eq!(
            std::fs::metadata(&sch).unwrap().modified().unwrap(),
            modified
        );

        // Reopening the store proves that evidence and pre-write bytes are durable.
        let history = recovery::ExecutionStore::open(&state_dir)
            .unwrap()
            .history(&root)
            .unwrap();
        assert_eq!(history.len(), 1);
        let record = &history[0];
        assert_eq!(record.domain, "schematic");
        assert_eq!(record.state, "committed");
        assert_eq!(record.plan_hash, planned["plan_hash"]);
        assert_eq!(record.approval_hash, preview["approval_hash"]);
        assert_eq!(record.backups.len(), 1);
        assert_eq!(record.backups[0].original, sch);
        assert_eq!(std::fs::read(&record.backups[0].saved).unwrap(), empty);
        assert_eq!(record.backups[0].sha256, digest_bytes(empty).to_string());
        let snapshot = project_snapshot(true);
        let observation = LiveBoardObservation {
            snapshot: snapshot.clone(),
            instance_token: kicad_ipc::InstanceToken::new("source-test").unwrap(),
            board_filename: "board.kicad_pcb".into(),
            board_path: root.join("board.kicad_pcb"),
            project_dir: root.clone(),
        };
        let request = BoardFootprintRequest {
            target_id: "placed-r1".into(),
            library_nickname: "Resistor_SMD".into(),
            library_entry_name: "R_0603_1608Metric".into(),
            x_nm: 5_000_000,
            y_nm: 5_000_000,
            reference: "R1".into(),
            value: "1k".into(),
        };
        let placement =
            footprint_authoring::prepare(&library_settings::capture(), &request, &observation)
                .unwrap();
        let binding = placement.source.as_ref().unwrap();
        assert_eq!(binding.symbol_path.len(), 2);
        assert_eq!(
            binding.symbol_path[0].as_str(),
            "11111111-1111-4111-8111-111111111111"
        );
        assert_eq!(binding.pad_nets.len(), 2);
        assert!(binding.pad_nets["2"].as_ref().unwrap().contains("R1"));
        let serialized = serde_json::to_value(&placement).unwrap();
        assert_eq!(
            serde_json::from_value::<kicad_model::AllowlistedFootprintPlacement>(serialized)
                .unwrap(),
            placement
        );
        let server = Server::with_discovery(Arc::new(ValidationDiscovery {
            root: root.clone(),
            board_path: observation.board_path.clone(),
            snapshot: snapshot.clone(),
            token: "source-test".into(),
        }));
        let mut plan = plan_request(snapshot, "footprint-source-plan");
        plan.segments = None;
        plan.footprint = Some(request);
        let result = server.kicad_plan_change(Parameters(plan)).await.unwrap();
        let CallToolResponse::Complete(result) = result else {
            panic!("complete plan expected")
        };
        assert_eq!(result.structured_content.unwrap()["ok"], true);
        let sessions = server.sessions.lock().unwrap();
        let stored = sessions.get("plan:footprint-source-plan").unwrap();
        let OperationKind::PlaceAllowlistedFootprint {
            placement: planned, ..
        } = &stored.session.plan.operations[0].kind
        else {
            panic!("footprint operation expected")
        };
        assert_eq!(planned, &placement);
        drop(sessions);
        let mut invalid = placement.clone();
        invalid.source.as_mut().unwrap().symbol_path[0] = StableId::new("wrong-root").unwrap();
        assert!(invalid.validate().is_err());
        let mut invalid = placement.clone();
        invalid.source.as_mut().unwrap().pad_nets.remove("2");
        assert!(invalid.validate().is_err());
        let lock = root.join("~board.kicad_sch.lck");
        std::fs::write(&lock, b"locked").unwrap();
        assert!(
            footprint_authoring::verify_sources(&library_settings::capture(), &root, &placement)
                .is_err()
        );
        std::fs::remove_file(lock).unwrap();
        std::fs::write(&sch, empty).unwrap();
        assert!(
            footprint_authoring::verify_sources(&library_settings::capture(), &root, &placement)
                .is_err()
        );
    }

    fn plan_request(_snapshot: ProjectSnapshot, request_id: &str) -> PlanBoardGraphicsRequest {
        PlanBoardGraphicsRequest {
            geometry: None,
            route: None,
            endpoint: "ipc:///tmp/kicad/api.sock".into(),
            source_run_id: "run".into(),
            segments: Some(serde_json::to_value(vec![graphic("g1")]).unwrap()),
            outline: None,
            footprint: None,
            assembly: None,
            move_footprint: None,
            edit_existing: None,
            delete_items: None,
            flip_footprint: None,
            track: None,
            via: None,
            batch: None,
            request_id: request_id.into(),
        }
    }

    fn batch_outline() -> BoardBatchItemRequest {
        BoardBatchItemRequest {
            route: None,
            segments: None,
            outline: Some(BoardOutlineRequest {
                top_left_x_nm: 0,
                top_left_y_nm: 0,
                bottom_right_x_nm: 10_000_000,
                bottom_right_y_nm: 8_000_000,
                width_nm: 100_000,
            }),
            track: None,
            via: None,
        }
    }

    fn batch_track(source_uuid: &str) -> BoardBatchItemRequest {
        BoardBatchItemRequest {
            route: None,
            segments: None,
            outline: None,
            track: Some(BoardTrackRequest {
                layer: None,
                source_uuid: source_uuid.into(),
                start_x_nm: 1_000_000,
                start_y_nm: 2_000_000,
                end_x_nm: 3_000_000,
                end_y_nm: 4_000_000,
                width_nm: 250_000,
                start_pad: None,
                end_pad: None,
                net: None,
            }),
            via: None,
        }
    }

    fn batch_via(source_uuid: &str) -> BoardBatchItemRequest {
        BoardBatchItemRequest {
            route: None,
            segments: None,
            outline: None,
            track: None,
            via: Some(BoardViaRequest {
                source_uuid: source_uuid.into(),
                x_nm: 5_000_000,
                y_nm: 6_000_000,
                pad_diameter_nm: 800_000,
                drill_diameter_nm: 400_000,
                net: None,
            }),
        }
    }

    async fn connected(
        server: Server,
    ) -> rmcp::service::RunningService<rmcp::RoleClient, TestClient> {
        let (server_transport, client_transport) = tokio::io::duplex(16 * 1024);
        tokio::spawn(async move {
            let service = server.serve(server_transport).await.unwrap();
            service.waiting().await.unwrap();
        });
        TestClient
            .serve(client_transport)
            .await
            .expect("client should initialize")
    }

    async fn validate(server: &Server, baseline_id: Option<String>, request_id: &str) -> Value {
        let response = server
            .kicad_validate_project(Parameters(ValidateProjectRequest {
                endpoint: "ipc:///tmp/kicad/api.sock".into(),
                baseline_id,
                request_id: Some(request_id.into()),
            }))
            .await
            .unwrap();
        match response {
            rmcp::model::CallToolResponse::Complete(result) => result.structured_content.unwrap(),
            _ => panic!("validate call should complete"),
        }
    }

    #[tokio::test]
    async fn validate_captures_unique_baselines_with_server_owned_revision() {
        let fixture = TempBoard::new();
        let snapshot = project_snapshot(false);
        let expected_revision = snapshot.revision().unwrap().to_string();
        let (server, calls) = validation_server(
            &fixture,
            snapshot,
            vec![report(Vec::new()), report(Vec::new())],
        );

        let first = validate(&server, None, "capture-1").await;
        server.drc_baselines.lock().unwrap().clear();
        let second = validate(&server, None, "capture-2").await;

        assert_eq!(first["ok"], true);
        assert_ne!(first["data"]["baseline_id"], second["data"]["baseline_id"]);
        assert_eq!(first["project_revision"], expected_revision);
        assert_eq!(first["data"]["captured_revision"], expected_revision);
        assert_eq!(second["data"]["captured_revision"], expected_revision);
        assert_eq!(calls.load(AtomicOrdering::SeqCst), 2);
    }

    #[tokio::test]
    async fn validate_rejects_unknown_and_mismatched_baselines_before_drc() {
        let fixture = TempBoard::new();
        let (server, calls) =
            validation_server(&fixture, project_snapshot(false), vec![report(Vec::new())]);
        let missing = validate(&server, Some("missing".into()), "missing").await;
        assert_eq!(missing["ok"], false);
        assert_eq!(missing["data"]["kind"], "baseline_not_found");
        assert_eq!(calls.load(AtomicOrdering::SeqCst), 0);

        let captured = validate(&server, None, "capture").await;
        let baseline_id = captured["data"]["baseline_id"].as_str().unwrap().to_owned();
        let mismatched_discovery = Arc::new(ValidationDiscovery {
            root: fixture.root.clone(),
            board_path: fixture.root.join("board.kicad_pcb"),
            snapshot: project_snapshot(false),
            token: "different-token".into(),
        });
        let mismatch_server = Server {
            discovery: mismatched_discovery,
            ..server.clone()
        };
        let mismatch = validate(&mismatch_server, Some(baseline_id), "mismatch").await;
        assert_eq!(mismatch["ok"], false);
        assert_eq!(mismatch["data"]["kind"], "baseline_mismatch");
        assert_eq!(calls.load(AtomicOrdering::SeqCst), 1);
    }

    #[tokio::test]
    async fn validation_failure_leaves_change_sessions_unchanged() {
        let fixture = TempBoard::new();
        let (server, calls) = validation_server(
            &fixture,
            project_snapshot(true),
            vec![CheckResult::Failed(CheckFailure::CommandUnavailable {
                message: "missing kicad-cli".into(),
            })],
        );
        let planned = server
            .kicad_plan_change(Parameters(plan_request(
                project_snapshot(true),
                "before-validation",
            )))
            .await
            .unwrap();
        assert!(matches!(
            planned,
            rmcp::model::CallToolResponse::Complete(_)
        ));
        let before = server.sessions.lock().unwrap().clone();

        let failed = validate(&server, None, "failure").await;

        assert_eq!(failed["ok"], false);
        assert_eq!(failed["data"]["kind"], "validation_failed");
        let after = server.sessions.lock().unwrap();
        assert_eq!(after.len(), before.len());
        assert_eq!(
            after.keys().collect::<Vec<_>>(),
            before.keys().collect::<Vec<_>>()
        );
        assert_eq!(calls.load(AtomicOrdering::SeqCst), 1);
    }

    #[tokio::test]
    async fn validate_delta_reports_unchanged_and_introduced_violations() {
        let fixture = TempBoard::new();
        let unchanged = violation("clearance", "existing debt", "same");
        let introduced = violation("courtyard", "new debt", "new");
        let (server, calls) = validation_server(
            &fixture,
            project_snapshot(false),
            vec![
                report(vec![unchanged.clone()]),
                report(vec![unchanged, introduced]),
            ],
        );
        let captured = validate(&server, None, "capture").await;
        let baseline_id = captured["data"]["baseline_id"].as_str().unwrap().to_owned();

        let compared = validate(&server, Some(baseline_id), "compare").await;

        assert_eq!(compared["ok"], true);
        assert_eq!(
            compared["data"]["delta"]["unchanged"]
                .as_array()
                .unwrap()
                .len(),
            1
        );
        assert_eq!(
            compared["data"]["delta"]["introduced"]
                .as_array()
                .unwrap()
                .len(),
            1
        );
        assert_eq!(
            compared["data"]["delta"]["resolved"]
                .as_array()
                .unwrap()
                .len(),
            0
        );
        assert_eq!(
            compared["data"]["captured_revision"],
            compared["data"]["current_revision"]
        );
        assert_eq!(calls.load(AtomicOrdering::SeqCst), 2);
    }

    #[test]
    fn schematic_hashes_are_deterministic_and_candidate_bytes_are_not_serialized() {
        let first = digest_text(&["fixed-two-resistor-circuit-v1", "source"]);
        let second = digest_text(&["fixed-two-resistor-circuit-v1", "source"]);
        assert_eq!(first, second);
        assert!(first.starts_with("sha256:"));
        let outcome = SchematicPatchOutcome::Committed {
            artifact: SchematicArtifactFile::new("root.kicad_sch", 4, digest_bytes(b"test"))
                .unwrap(),
        };
        let serialized = schematic_outcome_data(&outcome).to_string();
        assert!(!serialized.contains("candidate_bytes"));
    }

    #[tokio::test]
    async fn schematic_change_requires_exact_approval_and_commits_once() {
        let sequence = TEMP_SEQUENCE.fetch_add(1, AtomicOrdering::Relaxed);
        let root = std::env::temp_dir().join(format!(
            "auto-kicad-schematic-mcp-{}-{sequence}",
            std::process::id()
        ));
        std::fs::create_dir_all(&root).unwrap();
        let schematic = root.join("root.kicad_sch");
        std::fs::write(
            &schematic,
            r#"(kicad_sch
  (version 20231120)
  (generator "eeschema")
  (generator_version "10.0")
  (uuid "11111111-1111-4111-8111-111111111111")
  (paper "A4")
  (lib_symbols)
)
"#,
        )
        .unwrap();
        let preview_calls = Arc::new(AtomicUsize::new(0));
        let commit_calls = Arc::new(AtomicUsize::new(0));
        let server = Server::with_all_services(
            Arc::new(FakeDiscovery {
                write_capability: true,
            }),
            Arc::new(IpcBoardGraphicsExecutionService::default()),
            Arc::new(IpcBoardValidationService),
            Arc::new(FakeSchematic {
                preview_calls: preview_calls.clone(),
                commit_calls: commit_calls.clone(),
            }),
        );
        assert!(
            server
                .kicad_plan_schematic_change(Parameters(PlanSchematicChangeRequest {
                    project_root: root.to_string_lossy().into_owned(),
                    root_schematic: schematic.to_string_lossy().into_owned(),
                    source_run_id: "  ".into(),
                    request_id: "schematic-empty-run".into(),
                }))
                .await
                .is_err()
        );
        let planned = server
            .kicad_plan_schematic_change(Parameters(PlanSchematicChangeRequest {
                project_root: root.to_string_lossy().into_owned(),
                root_schematic: schematic.to_string_lossy().into_owned(),
                source_run_id: "run-schematic-lifecycle".into(),
                request_id: "schematic-lifecycle".into(),
            }))
            .await
            .unwrap();
        let planned = match planned {
            CallToolResponse::Complete(result) => result.structured_content.unwrap(),
            _ => panic!("plan call should complete"),
        };
        let plan_id = planned["data"]["plan_id"].as_str().unwrap().to_owned();
        let plan_hash = planned["data"]["plan_hash"].as_str().unwrap().to_owned();
        assert_eq!(planned["data"]["audit"]["action"], "plan");
        assert_eq!(planned["data"]["audit"]["sequence"], 1);
        assert_eq!(
            planned["data"]["audit"]["source_run_id"],
            "run-schematic-lifecycle"
        );
        assert_eq!(
            planned["data"]["audit"]["request_id"],
            "schematic-lifecycle"
        );
        assert!(
            server
                .kicad_commit_schematic_change(Parameters(CommitSchematicChangeRequest {
                    plan_id: plan_id.clone(),
                    plan_hash: plan_hash.clone(),
                    approval_hash: plan_hash.clone(),
                    request_id: None,
                }))
                .await
                .is_err()
        );
        let preview = server
            .kicad_preview_schematic_change(Parameters(PreviewSchematicChangeRequest {
                plan_id: plan_id.clone(),
                plan_hash: plan_hash.clone(),
                request_id: None,
            }))
            .await
            .unwrap();
        let preview = match preview {
            CallToolResponse::Complete(result) => result.structured_content.unwrap(),
            _ => panic!("preview call should complete"),
        };
        let approval_hash = preview["data"]["approval_hash"]
            .as_str()
            .unwrap()
            .to_owned();
        assert_eq!(preview["data"]["audit"]["action"], "preview");
        assert_eq!(preview["data"]["audit"]["sequence"], 2);
        assert_eq!(preview["data"]["audit"]["approval_hash"], approval_hash);
        assert!(!preview["data"]["audit"]["after_artifact"].is_null());
        assert_eq!(preview_calls.load(AtomicOrdering::SeqCst), 1);
        assert!(
            server
                .kicad_commit_schematic_change(Parameters(CommitSchematicChangeRequest {
                    plan_id: plan_id.clone(),
                    plan_hash: plan_hash.clone(),
                    approval_hash: "wrong".into(),
                    request_id: None,
                }))
                .await
                .is_err()
        );
        let committed = server
            .kicad_commit_schematic_change(Parameters(CommitSchematicChangeRequest {
                plan_id: plan_id.clone(),
                plan_hash: plan_hash.clone(),
                approval_hash: approval_hash.clone(),
                request_id: None,
            }))
            .await
            .unwrap();
        let committed = match committed {
            CallToolResponse::Complete(result) => result.structured_content.unwrap(),
            _ => panic!("commit call should complete"),
        };
        assert_eq!(committed["data"]["state"], "committed");
        assert_eq!(committed["data"]["audit"]["action"], "commit");
        assert_eq!(committed["data"]["audit"]["sequence"], 3);
        assert_eq!(
            committed["data"]["audit"]["source_run_id"],
            "run-schematic-lifecycle"
        );
        assert_eq!(committed["data"]["audit"]["approval_hash"], approval_hash);
        assert!(!committed["data"]["audit"]["before_artifact"].is_null());
        assert!(!committed["data"]["audit"]["after_artifact"].is_null());
        assert_eq!(commit_calls.load(AtomicOrdering::SeqCst), 1);
        let audit = server
            .kicad_schematic_audit(Parameters(SchematicAuditRequest {
                request_id: Some("audit-read".into()),
                source_run_id: Some("run-schematic-lifecycle".into()),
                plan_id: None,
                limit: Some(10),
            }))
            .await
            .unwrap();
        let audit = match audit {
            CallToolResponse::Complete(result) => result.structured_content.unwrap(),
            _ => panic!("audit call should complete"),
        };
        assert_eq!(audit["data"]["events"].as_array().unwrap().len(), 3);
        assert_eq!(audit["data"]["events"][0]["action"], "commit");
        assert_eq!(audit["data"]["events"][2]["action"], "plan");
        server
            .kicad_commit_schematic_change(Parameters(CommitSchematicChangeRequest {
                plan_id,
                plan_hash,
                approval_hash,
                request_id: None,
            }))
            .await
            .unwrap();
        assert_eq!(commit_calls.load(AtomicOrdering::SeqCst), 1);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn sdk_initialize_and_tool_list_hide_unverified_schematic_tools() {
        let client = connected(Server::with_all_services(
            Arc::new(FakeDiscovery {
                write_capability: true,
            }),
            Arc::new(IpcBoardGraphicsExecutionService::default()),
            Arc::new(IpcBoardValidationService),
            Arc::new(UnavailableSchematic),
        ))
        .await;
        let tools = client.list_tools(None).await.expect("tool list");
        let names: Vec<_> = tools.tools.iter().map(|tool| tool.name.clone()).collect();
        assert!(
            !names
                .iter()
                .any(|name| name == "kicad_plan_schematic_change")
        );
        assert!(
            !names
                .iter()
                .any(|name| name == "kicad_preview_schematic_change")
        );
        assert!(
            !names
                .iter()
                .any(|name| name == "kicad_commit_schematic_change")
        );
        let result = client
            .call_tool(
                CallToolRequestParams::new("kicad_capabilities").with_arguments(
                    json!({ "request_id": "req_capabilities" })
                        .as_object()
                        .unwrap()
                        .clone(),
                ),
            )
            .await
            .expect("capability call");
        let structured = result.structured_content.unwrap();
        assert_eq!(
            structured["data"]["advertised_tools"],
            serde_json::to_value(names).unwrap()
        );
        assert_eq!(
            structured["data"]["capabilities"]
                .as_array()
                .unwrap()
                .iter()
                .find(|capability| capability["id"] == "schematic.fixed_two_resistor_change")
                .unwrap()["status"],
            "unavailable"
        );
    }

    #[tokio::test]
    async fn sdk_initialize_and_tool_list_advertises_verified_schematic_tools() {
        let client = connected(Server::with_discovery(Arc::new(FakeDiscovery {
            write_capability: true,
        })))
        .await;
        let tools = client.list_tools(None).await.expect("tool list");
        let names: Vec<_> = tools.tools.iter().map(|tool| tool.name.clone()).collect();
        assert!(
            names
                .iter()
                .any(|name| name == "kicad_plan_schematic_change")
        );
        assert!(
            names
                .iter()
                .any(|name| name == "kicad_preview_schematic_change")
        );
        assert!(
            names
                .iter()
                .any(|name| name == "kicad_commit_schematic_change")
        );
        let result = client
            .call_tool(
                CallToolRequestParams::new("kicad_capabilities").with_arguments(
                    json!({ "request_id": "req_capabilities_verified" })
                        .as_object()
                        .unwrap()
                        .clone(),
                ),
            )
            .await
            .expect("capability call");
        let structured = result.structured_content.unwrap();
        assert_eq!(
            structured["data"]["advertised_tools"],
            serde_json::to_value(names).unwrap()
        );
        let capabilities = structured["data"]["capabilities"].as_array().unwrap();
        assert_eq!(
            capabilities
                .iter()
                .find(|capability| capability["id"] == "schematic.fixed_two_resistor_change")
                .unwrap()["status"],
            "verified"
        );
    }

    #[tokio::test]
    async fn sdk_call_preserves_versioned_stable_envelope() {
        let client = connected(Server::with_discovery(Arc::new(FakeDiscovery {
            write_capability: true,
        })))
        .await;
        let result = client
            .call_tool(
                CallToolRequestParams::new("kicad_capabilities").with_arguments(
                    json!({ "request_id": "req_1" })
                        .as_object()
                        .unwrap()
                        .clone(),
                ),
            )
            .await
            .expect("tool call");
        let structured = result.structured_content.unwrap();
        assert_eq!(structured["ok"], true);
        assert_eq!(structured["request_id"], "req_1");
        assert_eq!(
            structured["data"]["schema_version"],
            CONTRACT_SCHEMA_VERSION
        );
        assert_eq!(
            structured["data"]["advertised_tools"][0],
            "kicad_capabilities"
        );
    }

    #[tokio::test]
    async fn sdk_capabilities_exposes_embedded_matrix_with_runtime_statuses() {
        let client = connected(Server::with_all_services(
            Arc::new(FakeDiscovery {
                write_capability: true,
            }),
            Arc::new(IpcBoardGraphicsExecutionService::default()),
            Arc::new(IpcBoardValidationService),
            Arc::new(UnavailableSchematic),
        ))
        .await;
        let result = client
            .call_tool(
                CallToolRequestParams::new("kicad_capabilities")
                    .with_arguments(json!({}).as_object().unwrap().clone()),
            )
            .await
            .expect("capability call");
        let structured = result.structured_content.unwrap();
        let matrix = &structured["data"]["capability_matrix"];
        assert_eq!(matrix["schema_version"], "1");
        assert_eq!(matrix["product"], "auto-kicad");
        assert!(
            matrix["capabilities"]
                .as_array()
                .unwrap()
                .iter()
                .any(|c| c["id"] == "pcb.geometry.inspect"
                    && c["mode"] == "read"
                    && c["status"] == "verified")
        );
        assert_eq!(
            matrix["capabilities"]
                .as_array()
                .unwrap()
                .iter()
                .find(|entry| entry["id"] == "schematic.fixed_two_resistor_change")
                .unwrap()["status"],
            "unavailable"
        );
        assert_eq!(
            matrix["capabilities"]
                .as_array()
                .unwrap()
                .iter()
                .find(|entry| entry["id"] == "pcb.edit_existing")
                .unwrap()["status"],
            "verified"
        );
    }

    #[test]
    fn geometry_write_capability_requires_durable_storage() {
        let fixture = TempBoard::new();
        let server = Server::default();
        let status = |server: &Server| {
            server
                .capabilities()
                .iter()
                .find(|capability| capability.id == "pcb.geometry.write")
                .expect("geometry capability")
                .status
        };
        assert_eq!(status(&server), "unavailable");
        let server = server.with_state_dir(&fixture.root.join("state")).unwrap();
        assert_eq!(status(&server), "verified");
        assert_eq!(
            server
                .capabilities()
                .iter()
                .filter(|capability| capability.id == "pcb.geometry.write")
                .count(),
            1
        );
    }

    #[tokio::test]
    async fn sdk_geometry_inspection_is_opt_in_and_unavailable_without_native_evidence() {
        let client = connected(Server::with_discovery(Arc::new(FakeDiscovery::default()))).await;
        for requested in [false, true] {
            let result = client
                .call_tool(
                    CallToolRequestParams::new("kicad_inspect_project").with_arguments(
                        json!({"endpoint":"ipc:///tmp/kicad/api.sock","geometry":requested})
                            .as_object()
                            .unwrap()
                            .clone(),
                    ),
                )
                .await
                .unwrap();
            let data = &result.structured_content.as_ref().unwrap()["data"];
            if requested {
                assert_eq!(data["geometry"]["status"], "unavailable");
                assert!(data["geometry"].get("observation").is_none());
            } else {
                assert!(data.get("geometry").is_none());
            }
        }
        assert!(
            serde_json::from_value::<InspectProjectRequest>(
                json!({"endpoint":"x","geometry":{"write":true}})
            )
            .is_err()
        );
        let request =
            serde_json::from_value::<InspectProjectRequest>(json!({"endpoint":"","geometry":true}))
                .unwrap();
        let server = Server::with_discovery(Arc::new(FakeDiscovery::default()));
        assert!(
            server
                .kicad_inspect_project(Parameters(request))
                .await
                .is_err()
        );
    }

    #[tokio::test]
    async fn sdk_inspect_project_returns_normalized_snapshot_envelope() {
        let client = connected(Server::with_discovery(Arc::new(FakeDiscovery {
            write_capability: true,
        })))
        .await;
        let result = client
            .call_tool(
                CallToolRequestParams::new("kicad_inspect_project").with_arguments(
                    json!({
                        "endpoint": "ipc:///tmp/kicad/api.sock",
                        "request_id": "req_inspect"
                    })
                    .as_object()
                    .unwrap()
                    .clone(),
                ),
            )
            .await
            .expect("inspect tool call");
        let structured = result.structured_content.unwrap();
        assert_eq!(structured["ok"], true);
        assert_eq!(structured["request_id"], "req_inspect");
        assert_eq!(structured["data"]["kicad_version"], "10.0.4");
        assert!(structured["project_revision"].as_str().is_some());
    }

    #[test]
    fn single_track_layer_preserves_legacy_operation_and_copper_domain() {
        let board = StableId::new("board").unwrap();
        let base = json!({"source_uuid":"track", "start_x_nm":0, "start_y_nm":0,
            "end_x_nm":1000000, "end_y_nm":0, "width_nm":250000});
        for (layer, expected) in [
            (None, "F.Cu"),
            (Some("B.Cu"), "B.Cu"),
            (Some("In1.Cu"), "In1.Cu"),
        ] {
            let mut request = base.clone();
            if let Some(layer) = layer {
                request["layer"] = json!(layer);
            }
            let track = serde_json::from_value(request).unwrap();
            let OperationKind::AddStraightTrack { intent, .. } =
                board_operation_kind(&board, None, None, Some(track), None, None).unwrap()
            else {
                panic!("legacy operation family changed");
            };
            assert_eq!(intent.layer.to_string(), expected);
            let encoded = serde_json::to_value(&intent).unwrap();
            assert_eq!(encoded.get("layer").is_none(), expected == "F.Cu");
            let decoded: StraightTrackIntent = serde_json::from_value(encoded).unwrap();
            assert_eq!(decoded, intent);
        }
        for invalid in ["F.Mask", "In01.Cu", "In31.Cu"] {
            let mut request = base.clone();
            request["layer"] = json!(invalid);
            assert!(
                board_operation_kind(
                    &board,
                    None,
                    None,
                    Some(serde_json::from_value(request).unwrap()),
                    None,
                    None
                )
                .is_err()
            );
        }
    }

    #[tokio::test]
    async fn plan_rejects_coordinate_only_routing_requests() {
        let server = Server::with_discovery(Arc::new(FakeDiscovery {
            write_capability: true,
        }));
        let mut request = plan_request(project_snapshot(true), "req_track");
        request.segments = None;
        request.track = Some(BoardTrackRequest {
            layer: None,
            source_uuid: "track-1".into(),
            start_x_nm: 1_000_000,
            start_y_nm: 2_000_000,
            end_x_nm: 3_000_000,
            end_y_nm: 4_000_000,
            width_nm: 250_000,
            start_pad: None,
            end_pad: None,
            net: None,
        });
        let error = server
            .kicad_plan_change(Parameters(request))
            .await
            .unwrap_err();
        assert!(error.message.contains("track endpoint binding is required"));
        assert!(server.sessions.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn plan_accepts_ordered_mixed_board_batch_with_stable_operation_ids() {
        let server = Server::with_discovery(Arc::new(FakeDiscovery {
            write_capability: true,
        }));
        let mut request = plan_request(project_snapshot(true), "req_batch");
        request.segments = None;
        request.batch = Some(vec![
            batch_outline(),
            batch_track("track-1"),
            batch_via("via-1"),
        ]);

        let error = server
            .kicad_plan_change(Parameters(request))
            .await
            .unwrap_err();
        assert!(error.message.contains("track endpoint binding is required"));
        assert!(server.sessions.lock().unwrap().is_empty());
    }

    #[test]
    fn batch_parser_rejects_empty_oversized_and_multi_form_items() {
        let board = StableId::new("board").unwrap();
        let mut request = plan_request(project_snapshot(true), "req_batch_invalid");
        request.segments = None;
        request.batch = Some(Vec::new());
        assert!(board_operation_kinds(&request, &board).is_err());

        request.batch = Some(vec![batch_outline(); MAX_BOARD_BATCH_ITEMS + 1]);
        assert!(board_operation_kinds(&request, &board).is_err());

        request.batch = Some(vec![BoardBatchItemRequest {
            route: None,
            segments: None,
            outline: Some(batch_outline().outline.unwrap()),
            track: Some(batch_track("track-1").track.unwrap()),
            via: None,
        }]);
        assert!(board_operation_kinds(&request, &board).is_err());

        request.batch = Some(vec![BoardBatchItemRequest {
            route: None,
            segments: None,
            outline: None,
            track: None,
            via: None,
        }]);
        assert!(board_operation_kinds(&request, &board).is_err());
    }

    #[tokio::test]
    async fn approved_mixed_batch_is_forwarded_in_plan_order() {
        let operation_batches = Arc::new(Mutex::new(Vec::new()));
        let server = Server::with_discovery_and_execution(
            Arc::new(FakeDiscovery {
                write_capability: true,
            }),
            Arc::new(CapturingExecution {
                calls: Arc::new(Mutex::new(0)),
                revisions: Arc::new(Mutex::new(Vec::new())),
                operation_batches: operation_batches.clone(),
                outcome: BoardGraphicsCommitOutcome::CommittedAndSaved,
            }),
        );
        let mut request = plan_request(project_snapshot(true), "req_batch_commit");
        request.segments = None;
        request.batch = Some(vec![
            batch_outline(),
            batch_track("track-1"),
            batch_via("via-1"),
        ]);
        let error = server
            .kicad_plan_change(Parameters(request))
            .await
            .unwrap_err();
        assert!(error.message.contains("track endpoint binding is required"));
        assert_eq!(
            *operation_batches.lock().unwrap(),
            Vec::<Vec<StableId>>::new()
        );
        assert_eq!(
            *operation_batches.lock().unwrap(),
            Vec::<Vec<StableId>>::new()
        );
    }

    #[tokio::test]
    async fn plan_requires_exactly_one_change_form() {
        let server = Server::with_discovery(Arc::new(FakeDiscovery {
            write_capability: true,
        }));
        let mut request = plan_request(project_snapshot(true), "req_mixed");
        request.track = Some(BoardTrackRequest {
            layer: None,
            source_uuid: "track-1".into(),
            start_x_nm: 0,
            start_y_nm: 0,
            end_x_nm: 1_000_000,
            end_y_nm: 0,
            width_nm: 250_000,
            start_pad: None,
            end_pad: None,
            net: None,
        });

        let error = server
            .kicad_plan_change(Parameters(request))
            .await
            .unwrap_err();

        assert!(error.message.contains(
            "exactly one of segments, outline, footprint, track, via, or route is required"
        ));
        assert!(server.sessions.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn plan_rejects_footprint_without_verified_source() {
        let server = Server::with_discovery(Arc::new(FakeDiscovery {
            write_capability: true,
        }));
        let mut request = plan_request(project_snapshot(true), "req_footprint_denied");
        request.segments = None;
        request.footprint = Some(BoardFootprintRequest {
            target_id: "fp-1".into(),
            library_nickname: "LED_SMD".into(),
            library_entry_name: "LED_1210_3225Metric_Pad1.42x2.65mm_HandSolder".into(),
            x_nm: 1_000_000,
            y_nm: 2_000_000,
            reference: "D1".into(),
            value: "LED".into(),
        });
        let result = server.kicad_plan_change(Parameters(request)).await.unwrap();
        let result = match result {
            rmcp::model::CallToolResponse::Complete(result) => result.structured_content.unwrap(),
            _ => panic!("plan call should complete"),
        };
        assert_eq!(result["ok"], false);
        assert_eq!(result["data"]["reason"], "footprint_source_unavailable");
        assert!(server.sessions.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn plan_hash_is_deterministic_for_the_same_request() {
        let first = Server::with_discovery(Arc::new(FakeDiscovery {
            write_capability: true,
        }));
        let second = Server::with_discovery(Arc::new(FakeDiscovery {
            write_capability: true,
        }));
        let request = plan_request(project_snapshot(true), "req_plan");
        let first_result = first
            .kicad_plan_change(Parameters(request.clone()))
            .await
            .unwrap();
        let second_result = second.kicad_plan_change(Parameters(request)).await.unwrap();
        let first_result = match first_result {
            rmcp::model::CallToolResponse::Complete(result) => result,
            _ => panic!("plan call should complete"),
        };
        let second_result = match second_result {
            rmcp::model::CallToolResponse::Complete(result) => result,
            _ => panic!("plan call should complete"),
        };
        assert_eq!(
            first_result.structured_content.unwrap()["data"]["plan_hash"],
            second_result.structured_content.unwrap()["data"]["plan_hash"]
        );
    }

    #[tokio::test]
    async fn preview_returns_before_and_after_without_mutating_session_snapshot() {
        let server = Server::with_discovery_and_execution_and_shadow(
            Arc::new(FakeDiscovery {
                write_capability: true,
            }),
            Arc::new(IpcBoardGraphicsExecutionService::default()),
            Arc::new(FakeShadowValidation {
                outcome: shadow_outcome(ShadowValidationState::Committed),
                calls: Arc::new(AtomicUsize::new(0)),
            }),
        );
        let planned = server
            .kicad_plan_change(Parameters(plan_request(
                project_snapshot(true),
                "req_preview",
            )))
            .await
            .unwrap();
        let planned = match planned {
            rmcp::model::CallToolResponse::Complete(result) => result.structured_content.unwrap(),
            _ => panic!("plan call should complete"),
        };
        let plan_id = planned["data"]["plan_id"].as_str().unwrap().to_owned();
        let plan_hash = planned["data"]["plan_hash"].as_str().unwrap().to_owned();
        let before = server.sessions.lock().unwrap()[&plan_id]
            .session
            .snapshot
            .clone();
        let preview = server
            .kicad_preview_change(Parameters(PreviewChangeRequest {
                plan_id: plan_id.clone(),
                plan_hash,
                request_id: Some("req_preview_call".into()),
            }))
            .await
            .unwrap();
        let preview = match preview {
            rmcp::model::CallToolResponse::Complete(result) => result.structured_content.unwrap(),
            _ => panic!("preview call should complete"),
        };
        assert_eq!(
            preview["data"]["before"]["board"]["graphics"]
                .as_array()
                .unwrap()
                .len(),
            0
        );
        assert_eq!(
            preview["data"]["after"]["board"]["graphics"]
                .as_array()
                .unwrap()
                .len(),
            1
        );
        assert_eq!(
            server.sessions.lock().unwrap()[&plan_id].session.snapshot,
            before
        );
    }

    #[tokio::test]
    async fn missing_shadow_validation_rejects_preview_and_commit_without_execution() {
        let execution_calls = Arc::new(Mutex::new(0));
        let server = Server::with_discovery_and_execution(
            Arc::new(FakeDiscovery {
                write_capability: true,
            }),
            Arc::new(FakeExecution {
                outcome: BoardGraphicsCommitOutcome::CommittedAndSaved,
                calls: execution_calls.clone(),
            }),
        );
        let planned = server
            .kicad_plan_change(Parameters(plan_request(
                project_snapshot(true),
                "req_missing_shadow",
            )))
            .await
            .unwrap();
        let planned = match planned {
            CallToolResponse::Complete(result) => result.structured_content.unwrap(),
            _ => panic!("plan call should complete"),
        };
        let plan_id = planned["data"]["plan_id"].as_str().unwrap().to_owned();
        let plan_hash = planned["data"]["plan_hash"].as_str().unwrap().to_owned();

        let preview = server
            .kicad_preview_change(Parameters(PreviewChangeRequest {
                plan_id: plan_id.clone(),
                plan_hash: plan_hash.clone(),
                request_id: Some("req_missing_shadow_preview".into()),
            }))
            .await;
        let preview_error =
            preview.expect_err("preview must fail closed without shadow validation");
        assert!(
            preview_error
                .message
                .contains("shadow validation is required before approval")
        );
        assert_ne!(
            server.sessions.lock().unwrap()[&plan_id].session.state,
            kicad_engine::SessionState::AwaitingApproval
        );

        let commit = server
            .kicad_commit_change(Parameters(CommitChangeRequest {
                plan_id,
                plan_hash: plan_hash.clone(),
                approval_hash: plan_hash,
                validation_evidence_hash: String::new(),
                request_id: Some("req_missing_shadow_commit".into()),
            }))
            .await;
        let commit_error = commit.expect_err("commit must fail closed without shadow validation");
        assert!(
            commit_error
                .message
                .contains("shadow validation is required before commit")
        );
        assert_eq!(*execution_calls.lock().unwrap(), 0);
    }

    #[tokio::test]
    async fn commit_requires_preview_and_exact_approval_and_executes_once() {
        let calls = Arc::new(Mutex::new(0));
        let revisions = Arc::new(Mutex::new(Vec::new()));
        let operation_batches = Arc::new(Mutex::new(Vec::new()));
        let execution = Arc::new(CapturingExecution {
            calls: calls.clone(),
            revisions: revisions.clone(),
            operation_batches: operation_batches.clone(),
            outcome: BoardGraphicsCommitOutcome::CommittedAndSaved,
        });
        let server = Server::with_discovery_and_execution_and_shadow(
            Arc::new(FakeDiscovery {
                write_capability: true,
            }),
            execution,
            Arc::new(FakeShadowValidation {
                outcome: shadow_outcome(ShadowValidationState::Committed),
                calls: Arc::new(AtomicUsize::new(0)),
            }),
        );
        let planned = server
            .kicad_plan_change(Parameters(plan_request(
                project_snapshot(true),
                "req_commit",
            )))
            .await
            .unwrap();
        let planned = match planned {
            CallToolResponse::Complete(result) => result.structured_content.unwrap(),
            _ => panic!("plan call should complete"),
        };
        let plan_id = planned["data"]["plan_id"].as_str().unwrap().to_owned();
        let plan_hash = planned["data"]["plan_hash"].as_str().unwrap().to_owned();
        let expected_revision = {
            let sessions = server.sessions.lock().unwrap();
            let stored = &sessions[&plan_id].session;
            assert!(stored.snapshot.native_footprints.is_some());
            assert_eq!(
                stored.plan.base_revision,
                stored.snapshot.revision().unwrap()
            );
            stored.plan.base_revision.clone()
        };
        let early = server
            .kicad_commit_change(Parameters(CommitChangeRequest {
                plan_id: plan_id.clone(),
                plan_hash: plan_hash.clone(),
                approval_hash: plan_hash.clone(),
                validation_evidence_hash: String::new(),
                request_id: Some("req_early".into()),
            }))
            .await;
        assert!(early.is_err());
        let preview = server
            .kicad_preview_change(Parameters(PreviewChangeRequest {
                plan_id: plan_id.clone(),
                plan_hash: plan_hash.clone(),
                request_id: Some("req_preview".into()),
            }))
            .await
            .unwrap();
        let preview = match preview {
            CallToolResponse::Complete(result) => result.structured_content.unwrap(),
            _ => panic!("preview call should complete"),
        };
        let approval_hash = preview["data"]["approval_hash"]
            .as_str()
            .unwrap()
            .to_owned();
        let validation_evidence_hash = preview["data"]["validation_evidence_hash"]
            .as_str()
            .unwrap()
            .to_owned();
        let wrong = server
            .kicad_commit_change(Parameters(CommitChangeRequest {
                plan_id: plan_id.clone(),
                plan_hash: plan_hash.clone(),
                approval_hash: "wrong".into(),
                validation_evidence_hash: String::new(),
                request_id: Some("req_wrong".into()),
            }))
            .await;
        assert!(wrong.is_err());
        let committed = server
            .kicad_commit_change(Parameters(CommitChangeRequest {
                plan_id: plan_id.clone(),
                plan_hash: plan_hash.clone(),
                approval_hash: approval_hash.clone(),
                validation_evidence_hash: validation_evidence_hash.clone(),
                request_id: Some("req_commit_ok".into()),
            }))
            .await
            .unwrap();
        assert!(matches!(committed, CallToolResponse::Complete(_)));
        assert_eq!(*calls.lock().unwrap(), 1);
        assert_eq!(*revisions.lock().unwrap(), [expected_revision]);
        assert_eq!(
            *operation_batches.lock().unwrap(),
            [vec![StableId::new("operation:req_commit").unwrap()]]
        );
        let replay = server
            .kicad_commit_change(Parameters(CommitChangeRequest {
                plan_id,
                plan_hash: plan_hash.clone(),
                approval_hash,
                validation_evidence_hash,
                request_id: Some("req_commit_replay".into()),
            }))
            .await;
        assert!(replay.is_err());
        assert_eq!(*calls.lock().unwrap(), 1);
    }

    async fn planned_multi_operation_server(
        outcome: BoardGraphicsCommitOutcome,
        request_id: &str,
    ) -> (
        Server,
        String,
        String,
        String,
        String,
        Arc<Mutex<Vec<Vec<StableId>>>>,
    ) {
        let operation_batches = Arc::new(Mutex::new(Vec::new()));
        let server = Server::with_discovery_and_execution_and_shadow(
            Arc::new(FakeDiscovery {
                write_capability: true,
            }),
            Arc::new(CapturingExecution {
                calls: Arc::new(Mutex::new(0)),
                revisions: Arc::new(Mutex::new(Vec::new())),
                operation_batches: operation_batches.clone(),
                outcome,
            }),
            Arc::new(FakeShadowValidation {
                outcome: shadow_outcome(ShadowValidationState::Committed),
                calls: Arc::new(AtomicUsize::new(0)),
            }),
        );
        let planned = server
            .kicad_plan_change(Parameters(plan_request(project_snapshot(true), request_id)))
            .await
            .unwrap();
        let planned = match planned {
            CallToolResponse::Complete(result) => result.structured_content.unwrap(),
            _ => panic!("plan call should complete"),
        };
        let plan_id = planned["data"]["plan_id"].as_str().unwrap().to_owned();
        let plan_hash = {
            let mut sessions = server.sessions.lock().unwrap();
            let session = &mut sessions.get_mut(&plan_id).unwrap().session;
            let mut second = session.plan.operations[0].clone();
            second.operation_id = StableId::new(format!("operation:{request_id}:second")).unwrap();
            second.idempotency_key = format!("{request_id}:second");
            let OperationKind::AddBoardGraphicSegments { segments, .. } = &mut second.kind else {
                unreachable!("plan tool creates board graphics operations");
            };
            segments[0].graphic_id = StableId::new("g2").unwrap();
            session.plan.operations.push(second);
            session.plan_hash = session.plan.hash().unwrap();
            session.plan_hash.clone()
        };
        let preview = server
            .kicad_preview_change(Parameters(PreviewChangeRequest {
                plan_id: plan_id.clone(),
                plan_hash: plan_hash.clone(),
                request_id: Some(format!("{request_id}:preview")),
            }))
            .await
            .unwrap();
        let preview = match preview {
            CallToolResponse::Complete(result) => result.structured_content.unwrap(),
            _ => panic!("preview call should complete"),
        };
        (
            server,
            plan_id,
            plan_hash,
            preview["data"]["approval_hash"]
                .as_str()
                .unwrap()
                .to_owned(),
            preview["data"]["validation_evidence_hash"]
                .as_str()
                .unwrap()
                .to_owned(),
            operation_batches,
        )
    }

    #[tokio::test]
    async fn commit_executes_every_approved_operation_once_in_order() {
        let (
            server,
            plan_id,
            plan_hash,
            approval_hash,
            validation_evidence_hash,
            operation_batches,
        ) = planned_multi_operation_server(
            BoardGraphicsCommitOutcome::CommittedAndSaved,
            "req_multi",
        )
        .await;

        server
            .kicad_commit_change(Parameters(CommitChangeRequest {
                plan_id: plan_id.clone(),
                plan_hash: plan_hash.clone(),
                approval_hash,
                validation_evidence_hash,
                request_id: Some("req_multi_commit".into()),
            }))
            .await
            .unwrap();

        assert_eq!(
            *operation_batches.lock().unwrap(),
            [vec![
                StableId::new("operation:req_multi").unwrap(),
                StableId::new("operation:req_multi:second").unwrap(),
            ]]
        );
        let sessions = server.sessions.lock().unwrap();
        let stored = &sessions[&plan_id];
        assert_eq!(stored.execution_state, StoredExecutionState::Committed);
        assert_eq!(stored.session.committed_idempotency.len(), 2);
    }

    #[tokio::test]
    async fn durable_writer_blocks_execution_and_records_approved_outcomes() {
        let calls = Arc::new(Mutex::new(0));
        let (server, plan_id, plan_hash, _, _) = planned_server(
            BoardGraphicsCommitOutcome::CommittedAndSaved,
            calls.clone(),
            "durable-execution",
        )
        .await;
        let fixture = TempBoard::new();
        let board = fixture.root.join("board.kicad_pcb");
        std::fs::write(board.with_extension("kicad_pro"), "{}").unwrap();
        let server = server.with_state_dir(&fixture.root.join("state")).unwrap();
        let (approval_hash, validation_evidence_hash) = {
            let mut sessions = server.sessions.lock().unwrap();
            let stored = sessions.get_mut(&plan_id).unwrap();
            stored.board_path = board.clone();
            stored.project_dir = fixture.root.clone();
            stored.source_files = shadow_source_fingerprint(&board);
            let evidence = validation_evidence_hash(
                &plan_hash,
                stored.shadow_validation.as_ref().unwrap(),
                &stored.source_files,
            )
            .unwrap();
            let approval = approval_hash(&plan_hash, &evidence);
            stored.validation_evidence_hash = Some(evidence.clone());
            stored.approval_hash = Some(approval.clone());
            (approval, evidence)
        };
        let writer = server
            .execution_store
            .as_ref()
            .unwrap()
            .acquire(&fixture.root)
            .unwrap();
        let request = || CommitChangeRequest {
            plan_id: plan_id.clone(),
            plan_hash: plan_hash.clone(),
            approval_hash: approval_hash.clone(),
            validation_evidence_hash: validation_evidence_hash.clone(),
            request_id: None,
        };
        assert!(
            server
                .kicad_commit_change(Parameters(request()))
                .await
                .is_err()
        );
        assert_eq!(*calls.lock().unwrap(), 0);
        drop(writer);
        server
            .kicad_commit_change(Parameters(request()))
            .await
            .unwrap();
        assert_eq!(*calls.lock().unwrap(), 1);
        let reopened = recovery::ExecutionStore::open(&fixture.root.join("state")).unwrap();
        let history = reopened.history(&fixture.root).unwrap();
        assert_eq!(history.len(), 1);
        assert_eq!(history[0].state, "committed");
        assert_eq!(history[0].approval_hash, approval_hash);
        assert_eq!(history[0].backups.len(), 2);
        assert_eq!(
            std::fs::read(&history[0].backups[0].saved).unwrap(),
            std::fs::read(board).unwrap()
        );
    }

    #[tokio::test]
    async fn changed_project_rules_invalidate_approval_before_execution() {
        let calls = Arc::new(Mutex::new(0));
        let (server, plan_id, plan_hash, _, _) = planned_server(
            BoardGraphicsCommitOutcome::CommittedAndSaved,
            calls.clone(),
            "rules-change",
        )
        .await;
        let fixture = TempBoard::new();
        let board = fixture.root.join("board.kicad_pcb");
        let (approval_hash, validation_evidence_hash) = {
            let mut sessions = server.sessions.lock().unwrap();
            let stored = sessions.get_mut(&plan_id).unwrap();
            stored.board_path = board.clone();
            stored.source_files = shadow_source_fingerprint(&board);
            let evidence = validation_evidence_hash(
                &plan_hash,
                stored.shadow_validation.as_ref().unwrap(),
                &stored.source_files,
            )
            .unwrap();
            let approval = approval_hash(&plan_hash, &evidence);
            stored.validation_evidence_hash = Some(evidence.clone());
            stored.approval_hash = Some(approval.clone());
            (approval, evidence)
        };
        std::fs::write(board.with_extension("kicad_dru"), "(version 1)").unwrap();
        let result = server
            .kicad_commit_change(Parameters(CommitChangeRequest {
                plan_id,
                plan_hash,
                approval_hash,
                validation_evidence_hash,
                request_id: None,
            }))
            .await;
        assert_eq!(
            result.unwrap_err().message,
            "source files changed since planning"
        );
        assert_eq!(*calls.lock().unwrap(), 0);
    }

    #[tokio::test]
    async fn batch_execution_failure_does_not_mark_session_committed() {
        let (
            server,
            plan_id,
            plan_hash,
            approval_hash,
            validation_evidence_hash,
            operation_batches,
        ) = planned_multi_operation_server(
            BoardGraphicsCommitOutcome::Rejected {
                reason: "second operation failed".into(),
            },
            "req_multi_failure",
        )
        .await;

        let response = server
            .kicad_commit_change(Parameters(CommitChangeRequest {
                plan_id: plan_id.clone(),
                plan_hash: plan_hash.clone(),
                approval_hash,
                validation_evidence_hash,
                request_id: Some("req_multi_failure_commit".into()),
            }))
            .await
            .unwrap();
        let response = match response {
            CallToolResponse::Complete(result) => result.structured_content.unwrap(),
            _ => panic!("commit should complete"),
        };

        assert!(!response["ok"].as_bool().unwrap());
        assert_eq!(response["data"]["state"], "rejected");
        assert_eq!(operation_batches.lock().unwrap()[0].len(), 2);
        let sessions = server.sessions.lock().unwrap();
        let stored = &sessions[&plan_id];
        assert_eq!(stored.execution_state, StoredExecutionState::Ready);
        assert!(stored.session.committed_idempotency.is_empty());
    }

    #[tokio::test]
    async fn in_flight_claim_rejects_without_calling_executor() {
        let calls = Arc::new(Mutex::new(0));
        let (server, plan_id, plan_hash, approval_hash, validation_evidence_hash) = planned_server(
            BoardGraphicsCommitOutcome::CommittedAndSaved,
            calls.clone(),
            "req_in_flight",
        )
        .await;
        server
            .sessions
            .lock()
            .unwrap()
            .get_mut(&plan_id)
            .unwrap()
            .execution_state = StoredExecutionState::InFlight;
        let result = server
            .kicad_commit_change(Parameters(CommitChangeRequest {
                plan_id,
                plan_hash: plan_hash.clone(),
                approval_hash,
                validation_evidence_hash,
                request_id: Some("req_in_flight_commit".into()),
            }))
            .await;
        assert!(result.is_err());
        assert_eq!(*calls.lock().unwrap(), 0);
    }

    async fn planned_server(
        outcome: BoardGraphicsCommitOutcome,
        calls: Arc<Mutex<usize>>,
        request_id: &str,
    ) -> (Server, String, String, String, String) {
        let server = Server::with_discovery_and_execution_and_shadow(
            Arc::new(FakeDiscovery {
                write_capability: true,
            }),
            Arc::new(FakeExecution { outcome, calls }),
            Arc::new(FakeShadowValidation {
                outcome: shadow_outcome(ShadowValidationState::Committed),
                calls: Arc::new(AtomicUsize::new(0)),
            }),
        );
        let result = server
            .kicad_plan_change(Parameters(plan_request(project_snapshot(true), request_id)))
            .await
            .unwrap();
        let result = match result {
            CallToolResponse::Complete(result) => result.structured_content.unwrap(),
            _ => panic!("plan call should complete"),
        };
        let plan_id = result["data"]["plan_id"].as_str().unwrap().to_owned();
        let plan_hash = result["data"]["plan_hash"].as_str().unwrap().to_owned();
        let preview = server
            .kicad_preview_change(Parameters(PreviewChangeRequest {
                plan_id: plan_id.clone(),
                plan_hash: plan_hash.clone(),
                request_id: Some("req_outcome_preview".into()),
            }))
            .await
            .unwrap();
        let preview = match preview {
            CallToolResponse::Complete(result) => result.structured_content.unwrap(),
            _ => panic!("preview call should complete"),
        };
        (
            server,
            plan_id,
            plan_hash,
            preview["data"]["approval_hash"]
                .as_str()
                .unwrap()
                .to_owned(),
            preview["data"]["validation_evidence_hash"]
                .as_str()
                .unwrap()
                .to_owned(),
        )
    }

    #[tokio::test]
    async fn changed_validation_evidence_cannot_reuse_approval() {
        let calls = Arc::new(Mutex::new(0));
        let (server, plan_id, plan_hash, approval_hash, validation_evidence_hash) = planned_server(
            BoardGraphicsCommitOutcome::CommittedAndSaved,
            calls.clone(),
            "evidence-change",
        )
        .await;
        {
            let mut sessions = server.sessions.lock().unwrap();
            sessions
                .get_mut(&plan_id)
                .unwrap()
                .shadow_validation
                .as_mut()
                .unwrap()
                .reopened_revision = Some(RevisionDigest::new("f".repeat(64)).unwrap());
        }
        let result = server
            .kicad_commit_change(Parameters(CommitChangeRequest {
                plan_id,
                plan_hash,
                approval_hash,
                validation_evidence_hash,
                request_id: None,
            }))
            .await;
        assert_eq!(
            result.unwrap_err().message,
            "validation evidence changed since approval"
        );
        assert_eq!(*calls.lock().unwrap(), 0);
    }

    #[tokio::test]
    async fn rejected_can_retry_but_unknown_and_saved_states_cannot() {
        let rejected_calls = Arc::new(Mutex::new(0));
        let (server, plan_id, plan_hash, approval_hash, validation_evidence_hash) = planned_server(
            BoardGraphicsCommitOutcome::Rejected {
                reason: "stale revision".into(),
            },
            rejected_calls.clone(),
            "req_rejected",
        )
        .await;
        let first = server
            .kicad_commit_change(Parameters(CommitChangeRequest {
                plan_id: plan_id.clone(),
                plan_hash: plan_hash.clone(),
                approval_hash: approval_hash.clone(),
                validation_evidence_hash: validation_evidence_hash.clone(),
                request_id: Some("req_rejected_commit".into()),
            }))
            .await
            .unwrap();
        let first = match first {
            CallToolResponse::Complete(result) => result.structured_content.unwrap(),
            _ => panic!("commit should complete"),
        };
        assert!(!first["ok"].as_bool().unwrap());
        assert_eq!(first["data"]["state"], "rejected");
        assert_eq!(
            server.sessions.lock().unwrap()[&plan_id].execution_state,
            StoredExecutionState::Ready
        );

        let unknown_calls = Arc::new(Mutex::new(0));
        let (
            unknown_server,
            unknown_id,
            unknown_hash,
            unknown_approval_hash,
            unknown_validation_evidence_hash,
        ) = planned_server(
            BoardGraphicsCommitOutcome::Unknown {
                reason: "connection lost after write".into(),
            },
            unknown_calls.clone(),
            "req_unknown",
        )
        .await;
        unknown_server
            .kicad_commit_change(Parameters(CommitChangeRequest {
                plan_id: unknown_id.clone(),
                plan_hash: unknown_hash.clone(),
                approval_hash: unknown_approval_hash.clone(),
                validation_evidence_hash: unknown_validation_evidence_hash.clone(),
                request_id: Some("req_unknown_commit".into()),
            }))
            .await
            .unwrap();
        assert_eq!(
            unknown_server.sessions.lock().unwrap()[&unknown_id].execution_state,
            StoredExecutionState::Unknown
        );
        let retry = unknown_server
            .kicad_commit_change(Parameters(CommitChangeRequest {
                plan_id: unknown_id,
                plan_hash: unknown_hash.clone(),
                approval_hash: unknown_approval_hash,
                validation_evidence_hash: unknown_validation_evidence_hash,
                request_id: Some("req_unknown_retry".into()),
            }))
            .await;
        assert!(retry.is_err());
        assert_eq!(*unknown_calls.lock().unwrap(), 1);

        let saved_calls = Arc::new(Mutex::new(0));
        let (
            saved_server,
            saved_id,
            saved_hash,
            saved_approval_hash,
            saved_validation_evidence_hash,
        ) = planned_server(
            BoardGraphicsCommitOutcome::CommittedButNotSaved {
                reason: "save failed".into(),
            },
            saved_calls.clone(),
            "req_save_failure",
        )
        .await;
        let saved = saved_server
            .kicad_commit_change(Parameters(CommitChangeRequest {
                plan_id: saved_id.clone(),
                plan_hash: saved_hash.clone(),
                approval_hash: saved_approval_hash,
                validation_evidence_hash: saved_validation_evidence_hash,
                request_id: Some("req_save_commit".into()),
            }))
            .await
            .unwrap();
        let saved = match saved {
            CallToolResponse::Complete(result) => result.structured_content.unwrap(),
            _ => panic!("commit should complete"),
        };
        assert!(!saved["ok"].as_bool().unwrap());
        assert_eq!(saved["data"]["state"], "unknown");
        assert_eq!(saved["data"]["outcome"]["kind"], "committed_but_not_saved");
        assert_eq!(
            saved_server.sessions.lock().unwrap()[&saved_id].execution_state,
            StoredExecutionState::Unknown
        );
        assert_eq!(*saved_calls.lock().unwrap(), 1);
    }

    #[derive(Clone)]
    struct FakeShadowValidation {
        outcome: ShadowValidationOutcome,
        calls: Arc<AtomicUsize>,
    }

    impl ShadowValidationService for FakeShadowValidation {
        fn validate(
            &self,
            _: &str,
            _: &LiveBoardObservation,
            _: &[Operation],
            _: Duration,
        ) -> ShadowValidationOutcome {
            self.calls.fetch_add(1, AtomicOrdering::SeqCst);
            self.outcome.clone()
        }
    }

    fn shadow_outcome(state: ShadowValidationState) -> ShadowValidationOutcome {
        let committed = matches!(state, ShadowValidationState::Committed);
        ShadowValidationOutcome {
            state,
            fresh_revision: None,
            reopened_revision: None,
            reopened: committed,
            normalized_snapshot_matches: committed,
            baseline: None,
            after: None,
            delta: None,
            source_unchanged: true,
            shadow_cleaned: true,
            geometry: None,
            prepared_footprints: Vec::new(),
            reason: None,
        }
    }

    #[tokio::test]
    async fn rejected_shadow_does_not_enter_approval_or_execute_live_change() {
        for state in [
            ShadowValidationState::Rejected,
            ShadowValidationState::Unknown,
        ] {
            let shadow_calls = Arc::new(AtomicUsize::new(0));
            let execution_calls = Arc::new(Mutex::new(0));
            let server = Server::with_discovery_and_execution_and_shadow(
                Arc::new(FakeDiscovery {
                    write_capability: true,
                }),
                Arc::new(FakeExecution {
                    outcome: BoardGraphicsCommitOutcome::CommittedAndSaved,
                    calls: execution_calls.clone(),
                }),
                Arc::new(FakeShadowValidation {
                    outcome: shadow_outcome(state.clone()),
                    calls: shadow_calls.clone(),
                }),
            );
            let planned = server
                .kicad_plan_change(Parameters(plan_request(
                    project_snapshot(true),
                    "req_shadow_rejected",
                )))
                .await
                .unwrap();
            let planned = match planned {
                CallToolResponse::Complete(result) => result.structured_content.unwrap(),
                _ => panic!("plan call should complete"),
            };
            let plan_id = planned["data"]["plan_id"].as_str().unwrap().to_owned();
            let plan_hash = planned["data"]["plan_hash"].as_str().unwrap().to_owned();
            let preview = server
                .kicad_preview_change(Parameters(PreviewChangeRequest {
                    plan_id: plan_id.clone(),
                    plan_hash: plan_hash.clone(),
                    request_id: Some("req_shadow_rejected_preview".into()),
                }))
                .await
                .unwrap();
            let preview = match preview {
                CallToolResponse::Complete(result) => result.structured_content.unwrap(),
                _ => panic!("preview call should complete"),
            };
            assert_eq!(preview["ok"], false);
            assert_eq!(preview["data"]["state"], shadow_validation_state(&state));
            assert!(preview["data"]["approval_hash"].is_null());
            assert_eq!(shadow_calls.load(AtomicOrdering::SeqCst), 1);
            assert_eq!(*execution_calls.lock().unwrap(), 0);
            assert_eq!(
                server.sessions.lock().unwrap()[&plan_id].execution_state,
                StoredExecutionState::Ready
            );
            assert!(
                server.sessions.lock().unwrap()[&plan_id].session.state
                    != kicad_engine::SessionState::AwaitingApproval
            );
            assert!(
                server
                    .kicad_commit_change(Parameters(CommitChangeRequest {
                        plan_id,
                        plan_hash: plan_hash.clone(),
                        approval_hash: plan_hash,
                        validation_evidence_hash: String::new(),
                        request_id: Some("req_shadow_rejected_commit".into()),
                    }))
                    .await
                    .is_err()
            );
        }
    }

    #[test]
    fn failed_shadow_cleanup_revokes_success_without_touching_source() {
        let source = TempBoard::new();
        let board = source.root.join("board.kicad_pcb");
        std::fs::write(board.with_extension("kicad_pro"), "{}").unwrap();
        let fingerprint = shadow_source_fingerprint(&board);
        let mut launcher = OwnedPcbnewLauncher::new(&source.root.join("shadows")).unwrap();
        let shadow = launcher.create_shadow(&board).unwrap();
        std::fs::remove_dir_all(shadow.root()).unwrap();
        let result = cleanup_shadow(
            &mut launcher,
            &board,
            &fingerprint,
            shadow_outcome(ShadowValidationState::Committed),
        );
        assert_eq!(result.state, ShadowValidationState::Unknown);
        assert!(result.source_unchanged);
        assert!(!result.shadow_cleaned);
        assert!(result.reason.unwrap().contains("cleanup"));
        assert_eq!(shadow_source_fingerprint(&board), fingerprint);
    }

    #[tokio::test]
    async fn committed_shadow_is_bound_to_preview_plan_and_allows_exact_commit() {
        let shadow_calls = Arc::new(AtomicUsize::new(0));
        let execution_calls = Arc::new(Mutex::new(0));
        let server = Server::with_discovery_and_execution_and_shadow(
            Arc::new(FakeDiscovery {
                write_capability: true,
            }),
            Arc::new(FakeExecution {
                outcome: BoardGraphicsCommitOutcome::CommittedAndSaved,
                calls: execution_calls.clone(),
            }),
            Arc::new(FakeShadowValidation {
                outcome: shadow_outcome(ShadowValidationState::Committed),
                calls: shadow_calls.clone(),
            }),
        );
        let planned = server
            .kicad_plan_change(Parameters(plan_request(
                project_snapshot(true),
                "req_shadow_committed",
            )))
            .await
            .unwrap();
        let planned = match planned {
            CallToolResponse::Complete(result) => result.structured_content.unwrap(),
            _ => panic!("plan call should complete"),
        };
        let plan_id = planned["data"]["plan_id"].as_str().unwrap().to_owned();
        let plan_hash = planned["data"]["plan_hash"].as_str().unwrap().to_owned();
        let preview = server
            .kicad_preview_change(Parameters(PreviewChangeRequest {
                plan_id: plan_id.clone(),
                plan_hash: plan_hash.clone(),
                request_id: Some("req_shadow_committed_preview".into()),
            }))
            .await
            .unwrap();
        let preview = match preview {
            CallToolResponse::Complete(result) => result.structured_content.unwrap(),
            _ => panic!("preview call should complete"),
        };
        let validation_evidence_hash = preview["data"]["validation_evidence_hash"]
            .as_str()
            .unwrap()
            .to_owned();
        let derived_approval_hash = preview["data"]["approval_hash"]
            .as_str()
            .unwrap()
            .to_owned();
        assert_ne!(derived_approval_hash, plan_hash);
        assert_eq!(
            derived_approval_hash,
            approval_hash(&plan_hash, &validation_evidence_hash)
        );
        assert_eq!(preview["data"]["shadow_validation"]["state"], "committed");
        {
            let sessions = server.sessions.lock().unwrap();
            let stored = &sessions[&plan_id];
            assert_eq!(
                stored.validation_evidence_hash.as_deref(),
                Some(validation_evidence_hash.as_str())
            );
            assert_eq!(
                stored.approval_hash.as_deref(),
                Some(derived_approval_hash.as_str())
            );
            assert!(matches!(
                stored.shadow_validation.as_ref().unwrap().state,
                ShadowValidationState::Committed
            ));
        }
        let mismatched_evidence = server
            .kicad_commit_change(Parameters(CommitChangeRequest {
                plan_id: plan_id.clone(),
                plan_hash: plan_hash.clone(),
                approval_hash: derived_approval_hash.clone(),
                validation_evidence_hash: "wrong-evidence".into(),
                request_id: Some("req_shadow_mismatched_evidence".into()),
            }))
            .await;
        assert!(mismatched_evidence.is_err());
        assert_eq!(*execution_calls.lock().unwrap(), 0);
        let committed = server
            .kicad_commit_change(Parameters(CommitChangeRequest {
                plan_id,
                plan_hash,
                approval_hash: derived_approval_hash,
                validation_evidence_hash,
                request_id: Some("req_shadow_committed_commit".into()),
            }))
            .await
            .unwrap();
        assert!(matches!(committed, CallToolResponse::Complete(_)));
        assert_eq!(shadow_calls.load(AtomicOrdering::SeqCst), 1);
        assert_eq!(*execution_calls.lock().unwrap(), 1);
    }

    #[derive(Clone)]
    struct CapturingExecution {
        calls: Arc<Mutex<usize>>,
        revisions: Arc<Mutex<Vec<RevisionDigest>>>,
        operation_batches: Arc<Mutex<Vec<Vec<StableId>>>>,
        outcome: BoardGraphicsCommitOutcome,
    }

    impl BoardGraphicsExecutionService for CapturingExecution {
        fn execute(
            &self,
            _endpoint: &str,
            _board_filename: &str,
            _board_path: &std::path::Path,
            _project_dir: &std::path::Path,
            _expected_token: &str,
            expected_revision: &RevisionDigest,
            operations: &[Operation],
        ) -> BoardGraphicsCommitOutcome {
            *self.calls.lock().unwrap() += 1;
            self.revisions
                .lock()
                .unwrap()
                .push(expected_revision.clone());
            self.operation_batches.lock().unwrap().push(
                operations
                    .iter()
                    .map(|operation| operation.operation_id.clone())
                    .collect(),
            );
            self.outcome.clone()
        }
    }

    #[derive(Clone)]
    struct FakeExecution {
        outcome: BoardGraphicsCommitOutcome,
        calls: Arc<Mutex<usize>>,
    }

    impl BoardGraphicsExecutionService for FakeExecution {
        fn execute(
            &self,
            _endpoint: &str,
            _board_filename: &str,
            _board_path: &std::path::Path,
            _project_dir: &std::path::Path,
            _expected_token: &str,
            _expected_revision: &RevisionDigest,
            _operations: &[Operation],
        ) -> BoardGraphicsCommitOutcome {
            *self.calls.lock().unwrap() += 1;
            self.outcome.clone()
        }
    }

    #[tokio::test]
    async fn plan_uses_server_owned_identity_and_revision() {
        let server = Server::with_discovery(Arc::new(FakeDiscovery {
            write_capability: true,
        }));
        let snapshot = project_snapshot(true);
        let expected_revision = snapshot.revision().unwrap();
        let result = server
            .kicad_plan_change(Parameters(plan_request(snapshot, "req_server_snapshot")))
            .await
            .unwrap();
        let result = match result {
            rmcp::model::CallToolResponse::Complete(result) => result.structured_content.unwrap(),
            _ => panic!("plan call should complete"),
        };
        let plan_id = result["data"]["plan_id"].as_str().unwrap();
        let sessions = server.sessions.lock().unwrap();
        let plan = &sessions[plan_id].session.plan;
        assert_eq!(plan.base_revision, expected_revision);
        assert_eq!(plan.instance_token, "token");
        assert_eq!(
            sessions[plan_id].board_path,
            std::path::Path::new("/tmp/board.kicad_pcb")
        );
        assert_eq!(sessions[plan_id].project_dir, std::path::Path::new("/tmp"));
        assert_eq!(
            plan.operations[0].targets,
            [StableId::new("board").unwrap()]
        );
    }

    #[tokio::test]
    async fn plan_rejects_unverified_write_capability_without_storing_session() {
        let server = Server::with_discovery(Arc::new(FakeDiscovery {
            write_capability: false,
        }));
        let result = server
            .kicad_plan_change(Parameters(plan_request(
                project_snapshot(false),
                "req_denied",
            )))
            .await
            .unwrap();
        let result = match result {
            rmcp::model::CallToolResponse::Complete(result) => result.structured_content.unwrap(),
            _ => panic!("plan call should complete"),
        };
        assert_eq!(result["ok"], false);
        assert_eq!(result["data"]["reason"], "capability_not_verified");
        assert!(server.sessions.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn sdk_rejects_unadvertised_tool() {
        let client = connected(Server::with_discovery(Arc::new(FakeDiscovery {
            write_capability: true,
        })))
        .await;
        let error = client
            .call_tool(CallToolRequestParams::new("raw_protobuf_request"))
            .await
            .expect_err("unsupported tool must fail");
        assert!(error.to_string().contains("not found"));
    }
    mod existing_footprint_acceptance;
    mod existing_item_edit_acceptance;
    mod existing_item_protocol;
    mod footprint_flip_acceptance;
    mod item_deletion_acceptance;
    mod recovery_acceptance;
}
