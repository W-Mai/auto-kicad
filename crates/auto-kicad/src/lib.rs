use kicad_adapter::{
    BoardGraphicsCommitOutcome, BoardGraphicsExecutor, DiscoveryInput, DocumentObservation,
    LiveBoardObservation, artifact_discovery_snapshot, discovery_snapshot, live_board_observation,
};
use kicad_checks::{CheckResult, DrcPolicy, DrcReport, compare_drc, run_pcb_drc};
use kicad_engine::{
    ChangePlan, ChangeSession, Engine, Operation, OperationKind, Precondition, RiskClass,
};
use kicad_ipc::{ClientConfig, DocumentQuery, KiCadClient, NngTransport};
use kicad_model::{
    BoardGraphicSegment, DiscoverySnapshot, ProjectSnapshot, RevisionDigest, StableId,
};
use rmcp::{
    ErrorData, ServerHandler,
    handler::server::{router::tool::ToolRouter, wrapper::Parameters},
    model::{CallToolResponse, ServerCapabilities, ServerInfo},
    tool, tool_handler, tool_router,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

pub const CONTRACT_SCHEMA_VERSION: &str = "1";

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Capability {
    pub id: &'static str,
    pub domain: &'static str,
    pub mode: &'static str,
    pub status: &'static str,
}

pub trait DiscoveryService: Send + Sync {
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
                board_filename.as_str()
            }
            _ => return Err("open PCB document has no usable filename".into()),
        };
        let graphics = client
            .get_board_graphics(board_filename)
            .map_err(|error| error.to_string())?;
        let instance_token = client
            .instance_token()
            .cloned()
            .ok_or_else(|| "KiCad instance token was not observed".to_owned())?;
        live_board_observation(version, document, graphics, instance_token)
            .map_err(|error| error.to_string())
    }
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
    pub endpoint: String,
    pub source_run_id: String,
    pub segments: Value,
    pub request_id: String,
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
    pub request_id: Option<String>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum StoredExecutionState {
    Ready,
    InFlight,
    Committed,
    Unknown,
}

#[derive(Clone, Debug)]
struct StoredChangeSession {
    session: ChangeSession,
    endpoint: String,
    board_filename: String,
    execution_state: StoredExecutionState,
}

pub trait BoardGraphicsExecutionService: Send + Sync {
    fn execute(
        &self,
        endpoint: &str,
        board_filename: &str,
        expected_token: &str,
        expected_revision: &RevisionDigest,
        operation: &OperationKind,
    ) -> BoardGraphicsCommitOutcome;
}

#[derive(Clone, Debug, Default)]
pub struct IpcBoardGraphicsExecutionService;

impl BoardGraphicsExecutionService for IpcBoardGraphicsExecutionService {
    fn execute(
        &self,
        endpoint: &str,
        board_filename: &str,
        expected_token: &str,
        expected_revision: &RevisionDigest,
        operation: &OperationKind,
    ) -> BoardGraphicsCommitOutcome {
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
        let graphics = match client.get_board_graphics(board_filename) {
            Ok(graphics) => graphics,
            Err(reason) => {
                return BoardGraphicsCommitOutcome::Unknown {
                    reason: reason.to_string(),
                };
            }
        };
        let token = match client.instance_token().cloned() {
            Some(token) => token,
            None => {
                return BoardGraphicsCommitOutcome::Unknown {
                    reason: "KiCad instance token was not observed".into(),
                };
            }
        };
        let observation = match live_board_observation(version, document, graphics, token) {
            Ok(observation) => observation,
            Err(reason) => {
                return BoardGraphicsCommitOutcome::Rejected {
                    reason: reason.to_string(),
                };
            }
        };
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
        BoardGraphicsExecutor::default().execute(
            &mut client,
            operation,
            board_filename,
            "auto-kicad MCP approved board graphics change",
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
    sessions: Arc<Mutex<BTreeMap<String, StoredChangeSession>>>,
    drc_baselines: Arc<Mutex<BTreeMap<String, StoredDrcBaseline>>>,
    capabilities: Vec<Capability>,
    discovery: Arc<dyn DiscoveryService>,
    execution: Arc<dyn BoardGraphicsExecutionService>,
    validation: Arc<dyn BoardValidationService>,
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
pub struct InspectProjectRequest {
    pub endpoint: String,
    pub project_dir: Option<String>,
    pub request_id: Option<String>,
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
        Self::with_discovery_and_execution(
            Arc::new(IpcDiscoveryService),
            Arc::new(IpcBoardGraphicsExecutionService),
        )
    }
}

impl Server {
    pub fn with_discovery(discovery: Arc<dyn DiscoveryService>) -> Self {
        Self::with_discovery_and_execution(discovery, Arc::new(IpcBoardGraphicsExecutionService))
    }

    pub fn with_discovery_and_execution(
        discovery: Arc<dyn DiscoveryService>,
        execution: Arc<dyn BoardGraphicsExecutionService>,
    ) -> Self {
        Self::with_services(discovery, execution, Arc::new(IpcBoardValidationService))
    }

    pub fn with_services(
        discovery: Arc<dyn DiscoveryService>,
        execution: Arc<dyn BoardGraphicsExecutionService>,
        validation: Arc<dyn BoardValidationService>,
    ) -> Self {
        Self {
            sessions: Arc::new(Mutex::new(BTreeMap::new())),
            drc_baselines: Arc::new(Mutex::new(BTreeMap::new())),
            capabilities: vec![
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
                    id: "project.inspect",
                    domain: "project",
                    mode: "read",
                    status: "verified",
                },
            ],
            discovery,
            execution,
            validation,
            baseline_sequence: Arc::new(AtomicU64::new(0)),
            tool_router: Self::tool_router(),
        }
    }

    pub fn capabilities(&self) -> &[Capability] {
        &self.capabilities
    }
}

#[tool_router]
impl Server {
    #[tool(
        name = "kicad_capabilities",
        description = "Report verified KiCad capabilities and advertised tools."
    )]
    pub async fn kicad_capabilities(
        &self,
        Parameters(arguments): Parameters<CapabilityRequest>,
    ) -> Result<CallToolResponse, ErrorData> {
        let request_id = arguments
            .request_id
            .unwrap_or_else(|| "req_capabilities".to_owned());
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
                "advertised_tools": ["kicad_capabilities", "kicad_inspect_project", "kicad_validate_project", "kicad_plan_change", "kicad_preview_change", "kicad_commit_change"]
            }),
            diagnostics: Vec::new(),
            next_actions: Vec::new(),
        };
        let value = serde_json::to_value(envelope)
            .map_err(|error| ErrorData::internal_error(error.to_string(), None))?;
        Ok(rmcp::model::CallToolResult::structured(value).into())
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
        description = "Read the open KiCad documents and return a normalized project snapshot."
    )]
    pub async fn kicad_inspect_project(
        &self,
        Parameters(arguments): Parameters<InspectProjectRequest>,
    ) -> Result<CallToolResponse, ErrorData> {
        let request_id = arguments
            .request_id
            .unwrap_or_else(|| "req_inspect_project".to_owned());
        let project_dir = arguments.project_dir.as_deref().map(std::path::Path::new);
        let snapshot = self
            .discovery
            .discover(&arguments.endpoint, project_dir)
            .map_err(|error| ErrorData::internal_error(error.to_string(), None))?;
        let revision = snapshot
            .revision()
            .map_err(|error| ErrorData::internal_error(error.to_string(), None))?;
        let data = serde_json::to_value(snapshot)
            .map_err(|error| ErrorData::internal_error(error.to_string(), None))?;
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
        name = "kicad_plan_change",
        description = "Plan bounded Dwgs.User board graphics without mutating KiCad."
    )]
    pub async fn kicad_plan_change(
        &self,
        Parameters(arguments): Parameters<PlanBoardGraphicsRequest>,
    ) -> Result<CallToolResponse, ErrorData> {
        if arguments.endpoint.is_empty() {
            return Ok(self.rejected(arguments.request_id, "identity_is_required"));
        }
        let observation = self
            .discovery
            .live_board_observation(&arguments.endpoint, None)
            .map_err(|error| ErrorData::internal_error(error, None))?;
        let observed_token = observation.instance_token.as_str();
        let snapshot = observation.snapshot;
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
        let segments: Vec<BoardGraphicSegment> = serde_json::from_value(arguments.segments)
            .map_err(|error| ErrorData::invalid_params(error.to_string(), None))?;
        let base_revision = snapshot
            .revision()
            .map_err(|error| ErrorData::internal_error(error.to_string(), None))?;
        let plan_id = StableId::new(format!("plan:{}", arguments.request_id))
            .map_err(|error| ErrorData::invalid_params(error.to_string(), None))?;
        let operation_id = StableId::new(format!("operation:{}", arguments.request_id))
            .map_err(|error| ErrorData::invalid_params(error.to_string(), None))?;
        let operation = Operation {
            operation_id,
            kind: OperationKind::AddBoardGraphicSegments {
                board: board.clone(),
                segments,
            },
            targets: vec![board],
            preconditions: vec![Precondition::Revision(base_revision.clone())],
            expected_effects: vec!["add board graphic segments".into()],
            risk_class: RiskClass::Medium,
            idempotency_key: arguments.request_id.clone(),
            source_run_id: arguments.source_run_id.clone(),
        };
        let server_token = observed_token.to_owned();
        let plan = ChangePlan {
            plan_id: plan_id.clone(),
            source_run_id: arguments.source_run_id,
            base_revision,
            instance_token: server_token.clone(),
            operations: vec![operation],
        };
        let engine = Engine::new(snapshot, server_token.clone());
        let session = engine
            .begin_session(plan)
            .map_err(|error| ErrorData::invalid_params(error.to_string(), None))?;
        let plan_hash = session.plan_hash.clone();
        let base_revision = session.plan.base_revision.to_string();
        self.sessions.lock().expect("session lock").insert(
            plan_id.to_string(),
            StoredChangeSession {
                session,
                endpoint: arguments.endpoint,
                board_filename: observation.board_filename,
                execution_state: StoredExecutionState::Ready,
            },
        );
        self.envelope(
            arguments.request_id,
            Some(base_revision),
            json!({
                "plan_id": plan_id,
                "plan_hash": plan_hash,
                "state": "planned"
            }),
        )
    }

    #[tool(
        name = "kicad_preview_change",
        description = "Preview and validate a planned KiCad board graphics change, then await approval."
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
        let engine = Engine::new(
            stored.session.snapshot.clone(),
            stored.session.plan.instance_token.clone(),
        );
        let preview = engine
            .preview_session(&mut stored.session)
            .and_then(|preview| {
                engine.validate(&mut stored.session)?;
                engine.request_approval(&mut stored.session)?;
                Ok(preview)
            })
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
        self.envelope(
            arguments
                .request_id
                .unwrap_or_else(|| "req_preview_change".into()),
            Some(after_revision),
            json!({
                "plan_id": stored.session.plan.plan_id,
                "plan_hash": preview.plan_hash,
                "before": preview.before,
                "after": preview.after,
                "base_revision": stored.session.plan.base_revision,
                "state": "awaiting_approval"
            }),
        )
    }

    #[tool(
        name = "kicad_commit_change",
        description = "Commit an approved KiCad board graphics change."
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
        if stored.session.plan_hash != arguments.plan_hash
            || arguments.approval_hash != arguments.plan_hash
        {
            return Err(ErrorData::invalid_params("approval mismatch", None));
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
        let engine = Engine::new(
            stored.session.snapshot.clone(),
            stored.session.plan.instance_token.clone(),
        );
        engine
            .approve(&mut stored.session, &arguments.approval_hash)
            .map_err(|error| ErrorData::invalid_params(error.to_string(), None))?;

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

        let outcome = self.execution.execute(
            &stored.endpoint,
            &stored.board_filename,
            &stored.session.plan.instance_token,
            &stored.session.plan.base_revision,
            &stored.session.plan.operations[0].kind,
        );
        let (ok, state, outcome_data) = match &outcome {
            BoardGraphicsCommitOutcome::CommittedAndSaved => {
                let mut commit_engine = Engine::new(
                    stored.session.snapshot.clone(),
                    stored.session.plan.instance_token.clone(),
                );
                if let Err(error) = commit_engine.commit(&mut stored.session) {
                    stored.execution_state = StoredExecutionState::Unknown;
                    self.sessions
                        .lock()
                        .expect("session lock")
                        .insert(arguments.plan_id.clone(), stored);
                    return Err(ErrorData::internal_error(error.to_string(), None));
                }
                stored.execution_state = StoredExecutionState::Committed;
                (true, "committed", json!({"kind": "committed_and_saved"}))
            }
            BoardGraphicsCommitOutcome::CommittedButNotSaved { reason } => {
                let mut commit_engine = Engine::new(
                    stored.session.snapshot.clone(),
                    stored.session.plan.instance_token.clone(),
                );
                if let Err(error) = commit_engine.commit(&mut stored.session) {
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
                    json!({"kind": "committed_but_not_saved", "reason": reason}),
                )
            }
            BoardGraphicsCommitOutcome::Rejected { reason } => {
                stored.execution_state = StoredExecutionState::Ready;
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

    fn rejected(&self, request_id: String, reason: &str) -> CallToolResponse {
        self.envelope_sync(request_id, None, json!({ "reason": reason }))
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
        Board, BoardGraphicLayer, CapabilityObservation, CapabilityStatus, Coordinate, Diagnostic,
        DocumentSpace, ProjectSnapshot, Schematic, Unit,
    };
    use rmcp::{ClientHandler, ServiceExt, model::CallToolRequestParams};
    use std::collections::VecDeque;
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicUsize, Ordering as AtomicOrdering};

    #[derive(Clone, Default)]
    struct TestClient;

    impl ClientHandler for TestClient {}

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
                Arc::new(IpcBoardGraphicsExecutionService),
                validation,
            ),
            calls,
        )
    }

    fn project_snapshot(write_capability: bool) -> ProjectSnapshot {
        ProjectSnapshot {
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

    fn plan_request(_snapshot: ProjectSnapshot, request_id: &str) -> PlanBoardGraphicsRequest {
        PlanBoardGraphicsRequest {
            endpoint: "ipc:///tmp/kicad/api.sock".into(),
            source_run_id: "run".into(),
            segments: serde_json::to_value(vec![graphic("g1")]).unwrap(),
            request_id: request_id.into(),
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

    #[tokio::test]
    async fn sdk_initialize_and_tool_list_expose_verified_read_tools() {
        let client = connected(Server::with_discovery(Arc::new(FakeDiscovery {
            write_capability: true,
        })))
        .await;
        let tools = client.list_tools(None).await.expect("tool list");
        let names: Vec<_> = tools.tools.iter().map(|tool| tool.name.clone()).collect();
        assert_eq!(
            names,
            vec![
                "kicad_capabilities",
                "kicad_commit_change",
                "kicad_inspect_project",
                "kicad_plan_change",
                "kicad_preview_change",
                "kicad_validate_project",
            ]
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
        let server = Server::with_discovery(Arc::new(FakeDiscovery {
            write_capability: true,
        }));
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
    async fn commit_requires_preview_and_exact_approval_and_executes_once() {
        let calls = Arc::new(Mutex::new(0));
        let execution = Arc::new(FakeExecution {
            outcome: BoardGraphicsCommitOutcome::CommittedAndSaved,
            calls: calls.clone(),
        });
        let server = Server::with_discovery_and_execution(
            Arc::new(FakeDiscovery {
                write_capability: true,
            }),
            execution,
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
        let early = server
            .kicad_commit_change(Parameters(CommitChangeRequest {
                plan_id: plan_id.clone(),
                plan_hash: plan_hash.clone(),
                approval_hash: plan_hash.clone(),
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
        assert!(matches!(preview, CallToolResponse::Complete(_)));
        let wrong = server
            .kicad_commit_change(Parameters(CommitChangeRequest {
                plan_id: plan_id.clone(),
                plan_hash: plan_hash.clone(),
                approval_hash: "wrong".into(),
                request_id: Some("req_wrong".into()),
            }))
            .await;
        assert!(wrong.is_err());
        let committed = server
            .kicad_commit_change(Parameters(CommitChangeRequest {
                plan_id: plan_id.clone(),
                plan_hash: plan_hash.clone(),
                approval_hash: plan_hash.clone(),
                request_id: Some("req_commit_ok".into()),
            }))
            .await
            .unwrap();
        assert!(matches!(committed, CallToolResponse::Complete(_)));
        assert_eq!(*calls.lock().unwrap(), 1);
        let replay = server
            .kicad_commit_change(Parameters(CommitChangeRequest {
                plan_id,
                plan_hash: plan_hash.clone(),
                approval_hash: plan_hash,
                request_id: Some("req_commit_replay".into()),
            }))
            .await;
        assert!(replay.is_err());
        assert_eq!(*calls.lock().unwrap(), 1);
    }

    #[tokio::test]
    async fn in_flight_claim_rejects_without_calling_executor() {
        let calls = Arc::new(Mutex::new(0));
        let (server, plan_id, plan_hash) = planned_server(
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
                approval_hash: plan_hash,
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
    ) -> (Server, String, String) {
        let server = Server::with_discovery_and_execution(
            Arc::new(FakeDiscovery {
                write_capability: true,
            }),
            Arc::new(FakeExecution { outcome, calls }),
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
        server
            .kicad_preview_change(Parameters(PreviewChangeRequest {
                plan_id: plan_id.clone(),
                plan_hash: plan_hash.clone(),
                request_id: Some("req_outcome_preview".into()),
            }))
            .await
            .unwrap();
        (server, plan_id, plan_hash)
    }

    #[tokio::test]
    async fn rejected_can_retry_but_unknown_and_saved_states_cannot() {
        let rejected_calls = Arc::new(Mutex::new(0));
        let (server, plan_id, plan_hash) = planned_server(
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
                approval_hash: plan_hash.clone(),
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
        let (unknown_server, unknown_id, unknown_hash) = planned_server(
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
                approval_hash: unknown_hash.clone(),
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
                approval_hash: unknown_hash,
                request_id: Some("req_unknown_retry".into()),
            }))
            .await;
        assert!(retry.is_err());
        assert_eq!(*unknown_calls.lock().unwrap(), 1);

        let saved_calls = Arc::new(Mutex::new(0));
        let (saved_server, saved_id, saved_hash) = planned_server(
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
                approval_hash: saved_hash.clone(),
                request_id: Some("req_save_commit".into()),
            }))
            .await
            .unwrap();
        let saved = match saved {
            CallToolResponse::Complete(result) => result.structured_content.unwrap(),
            _ => panic!("commit should complete"),
        };
        assert!(saved["ok"].as_bool().unwrap());
        assert_eq!(saved["data"]["outcome"]["kind"], "committed_but_not_saved");
        assert_eq!(
            saved_server.sessions.lock().unwrap()[&saved_id].execution_state,
            StoredExecutionState::Committed
        );
        assert_eq!(*saved_calls.lock().unwrap(), 1);
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
            _expected_token: &str,
            _expected_revision: &RevisionDigest,
            _operation: &OperationKind,
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
}
