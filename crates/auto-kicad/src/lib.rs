use kicad_adapter::{
    DiscoveryInput, DocumentObservation, artifact_discovery_snapshot, discovery_snapshot,
};
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
use std::sync::{Arc, Mutex};

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
    pub board_scope: String,
    pub base_revision: String,
    pub instance_token: String,
    pub source_run_id: String,
    pub segments: Value,
    pub snapshot: Value,
    pub request_id: String,
}

#[derive(Clone, Debug, Deserialize, schemars::JsonSchema)]
pub struct PreviewChangeRequest {
    pub plan_id: String,
    pub plan_hash: String,
    pub request_id: Option<String>,
}

#[derive(Clone)]
pub struct Server {
    sessions: Arc<Mutex<BTreeMap<String, ChangeSession>>>,
    instance_tokens: Arc<Mutex<BTreeMap<String, String>>>,
    capabilities: Vec<Capability>,
    discovery: Arc<dyn DiscoveryService>,
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
        Self::with_discovery(Arc::new(IpcDiscoveryService))
    }
}

impl Server {
    pub fn with_discovery(discovery: Arc<dyn DiscoveryService>) -> Self {
        Self {
            sessions: Arc::new(Mutex::new(BTreeMap::new())),
            instance_tokens: Arc::new(Mutex::new(BTreeMap::new())),
            capabilities: vec![
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
                "advertised_tools": [
                    "kicad_capabilities",
                    "kicad_inspect_project",
                    "kicad_plan_change",
                    "kicad_preview_change"
                ]
            }),
            diagnostics: Vec::new(),
            next_actions: Vec::new(),
        };
        let value = serde_json::to_value(envelope)
            .map_err(|error| ErrorData::internal_error(error.to_string(), None))?;
        Ok(rmcp::model::CallToolResult::structured(value).into())
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
        let snapshot: ProjectSnapshot = serde_json::from_value(arguments.snapshot.clone())
            .map_err(|error| ErrorData::invalid_params(error.to_string(), None))?;
        if arguments.endpoint.is_empty() || arguments.instance_token.is_empty() {
            return Ok(self.rejected(arguments.request_id, "identity_is_required"));
        }
        if !snapshot
            .capabilities
            .iter()
            .any(|capability| capability == "pcb.board_graphics.write")
        {
            return Ok(self.rejected(arguments.request_id, "capability_not_verified"));
        }
        let board = StableId::new(arguments.board_scope.clone())
            .map_err(|error| ErrorData::invalid_params(error.to_string(), None))?;
        if snapshot.board.document_id.as_ref() != Some(&board) {
            return Ok(self.rejected(arguments.request_id, "board_identity_mismatch"));
        }
        {
            let tokens = self.instance_tokens.lock().expect("instance token lock");
            if tokens
                .get(&arguments.endpoint)
                .is_some_and(|token| token != &arguments.instance_token)
            {
                return Ok(self.rejected(arguments.request_id, "instance_stale"));
            }
        }
        let segments: Vec<BoardGraphicSegment> = serde_json::from_value(arguments.segments)
            .map_err(|error| ErrorData::invalid_params(error.to_string(), None))?;
        let base_revision = RevisionDigest::new(arguments.base_revision)
            .map_err(|error| ErrorData::invalid_params(error.to_string(), None))?;
        let plan_id = StableId::new(format!("plan:{}", arguments.request_id))
            .map_err(|error| ErrorData::invalid_params(error.to_string(), None))?;
        let operation_id = StableId::new(format!("operation:{}", arguments.request_id))
            .map_err(|error| ErrorData::invalid_params(error.to_string(), None))?;
        let operation = Operation {
            operation_id,
            kind: OperationKind::AddBoardGraphicSegments { board, segments },
            targets: vec![StableId::new(arguments.board_scope).unwrap()],
            preconditions: vec![Precondition::Revision(base_revision.clone())],
            expected_effects: vec!["add board graphic segments".into()],
            risk_class: RiskClass::Medium,
            idempotency_key: arguments.request_id.clone(),
            source_run_id: arguments.source_run_id.clone(),
        };
        let plan = ChangePlan {
            plan_id: plan_id.clone(),
            source_run_id: arguments.source_run_id,
            base_revision,
            instance_token: arguments.instance_token.clone(),
            operations: vec![operation],
        };
        let engine = Engine::new(snapshot, arguments.instance_token.clone());
        let session = engine
            .begin_session(plan)
            .map_err(|error| ErrorData::invalid_params(error.to_string(), None))?;
        let plan_hash = session.plan_hash.clone();
        let base_revision = session.plan.base_revision.to_string();
        self.instance_tokens
            .lock()
            .expect("instance token lock")
            .insert(arguments.endpoint, arguments.instance_token);
        self.sessions
            .lock()
            .expect("session lock")
            .insert(plan_id.to_string(), session);
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
        description = "Preview a stored board graphics plan without mutating KiCad."
    )]
    pub async fn kicad_preview_change(
        &self,
        Parameters(arguments): Parameters<PreviewChangeRequest>,
    ) -> Result<CallToolResponse, ErrorData> {
        let session = self
            .sessions
            .lock()
            .expect("session lock")
            .get(&arguments.plan_id)
            .cloned()
            .ok_or_else(|| ErrorData::invalid_params("unknown plan_id", None))?;
        if session.plan_hash != arguments.plan_hash {
            return Err(ErrorData::invalid_params("plan hash mismatch", None));
        }
        let engine = Engine::new(
            session.snapshot.clone(),
            session.plan.instance_token.clone(),
        );
        let preview = engine
            .preview(&session.plan)
            .map_err(|error| ErrorData::invalid_params(error.to_string(), None))?;
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
                "plan_id": session.plan.plan_id,
                "plan_hash": preview.plan_hash,
                "before": preview.before,
                "after": preview.after,
                "base_revision": session.plan.base_revision,
                "state": "previewed"
            }),
        )
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
    use kicad_model::{
        Board, BoardGraphicLayer, CapabilityObservation, CapabilityStatus, Coordinate, Diagnostic,
        DocumentSpace, ProjectSnapshot, Schematic, Unit,
    };
    use rmcp::{ClientHandler, ServiceExt, model::CallToolRequestParams};

    #[derive(Clone, Default)]
    struct TestClient;

    impl ClientHandler for TestClient {}

    #[derive(Clone)]
    struct FakeDiscovery;

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

    fn plan_request(snapshot: ProjectSnapshot, request_id: &str) -> PlanBoardGraphicsRequest {
        PlanBoardGraphicsRequest {
            endpoint: "ipc:///tmp/kicad/api.sock".into(),
            board_scope: "board".into(),
            base_revision: snapshot.revision().unwrap().as_str().into(),
            instance_token: "token".into(),
            source_run_id: "run".into(),
            segments: serde_json::to_value(vec![graphic("g1")]).unwrap(),
            snapshot: serde_json::to_value(snapshot).unwrap(),
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

    #[tokio::test]
    async fn sdk_initialize_and_tool_list_expose_verified_read_tools() {
        let client = connected(Server::with_discovery(Arc::new(FakeDiscovery))).await;
        let tools = client.list_tools(None).await.expect("tool list");
        let names: Vec<_> = tools.tools.iter().map(|tool| tool.name.clone()).collect();
        assert_eq!(
            names,
            vec![
                "kicad_capabilities",
                "kicad_inspect_project",
                "kicad_plan_change",
                "kicad_preview_change"
            ]
        );
    }

    #[tokio::test]
    async fn sdk_call_preserves_versioned_stable_envelope() {
        let client = connected(Server::with_discovery(Arc::new(FakeDiscovery))).await;
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
        let client = connected(Server::with_discovery(Arc::new(FakeDiscovery))).await;
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
        let first = Server::with_discovery(Arc::new(FakeDiscovery));
        let second = Server::with_discovery(Arc::new(FakeDiscovery));
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
        let server = Server::with_discovery(Arc::new(FakeDiscovery));
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
        let before = server.sessions.lock().unwrap()[&plan_id].snapshot.clone();
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
        assert_eq!(server.sessions.lock().unwrap()[&plan_id].snapshot, before);
    }

    #[tokio::test]
    async fn plan_rejects_unverified_write_capability_without_storing_session() {
        let server = Server::with_discovery(Arc::new(FakeDiscovery));
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
        let client = connected(Server::with_discovery(Arc::new(FakeDiscovery))).await;
        let error = client
            .call_tool(CallToolRequestParams::new("raw_protobuf_request"))
            .await
            .expect_err("unsupported tool must fail");
        assert!(error.to_string().contains("not found"));
    }
}
