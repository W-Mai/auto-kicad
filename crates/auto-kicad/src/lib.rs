use kicad_adapter::{DiscoveryInput, DocumentObservation, discovery_snapshot};
use kicad_ipc::{ClientConfig, DocumentQuery, KiCadClient, NngTransport};
use kicad_model::DiscoverySnapshot;
use rmcp::{
    ErrorData, ServerHandler,
    handler::server::{router::tool::ToolRouter, wrapper::Parameters},
    model::{CallToolResponse, ServerCapabilities, ServerInfo},
    tool, tool_handler, tool_router,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::sync::Arc;

pub const CONTRACT_SCHEMA_VERSION: &str = "1";

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Capability {
    pub id: &'static str,
    pub domain: &'static str,
    pub mode: &'static str,
    pub status: &'static str,
}

pub trait DiscoveryService: Send + Sync {
    fn discover(&self, endpoint: &str) -> Result<DiscoverySnapshot, String>;
}

#[derive(Clone, Debug, Default)]
pub struct IpcDiscoveryService;

impl DiscoveryService for IpcDiscoveryService {
    fn discover(&self, endpoint: &str) -> Result<DiscoverySnapshot, String> {
        let version_config =
            ClientConfig::new(endpoint, "auto-kicad-mcp").map_err(|error| error.to_string())?;
        let version_transport =
            NngTransport::connect(version_config.clone()).map_err(|error| error.to_string())?;
        let mut version_client = KiCadClient::new(version_transport, version_config);
        let version = version_client
            .get_version()
            .map_err(|error| error.to_string())?;
        let schematic = observe_documents(endpoint, DocumentQuery::Schematic)?;
        let pcb = observe_documents(endpoint, DocumentQuery::Pcb)?;
        discovery_snapshot(DiscoveryInput {
            endpoint: endpoint.to_owned(),
            version,
            schematic,
            pcb,
        })
        .map_err(|error| error.to_string())
    }
}

fn observe_documents(endpoint: &str, query: DocumentQuery) -> Result<DocumentObservation, String> {
    let config =
        ClientConfig::new(endpoint, "auto-kicad-mcp").map_err(|error| error.to_string())?;
    let transport = NngTransport::connect(config.clone()).map_err(|error| error.to_string())?;
    let mut client = KiCadClient::new(transport, config);
    client.get_version().map_err(|error| error.to_string())?;
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

#[derive(Clone)]
pub struct Server {
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
                "advertised_tools": ["kicad_capabilities", "kicad_inspect_project"]
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
        let snapshot = self
            .discovery
            .discover(&arguments.endpoint)
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
    use kicad_model::{CapabilityObservation, CapabilityStatus, Diagnostic};
    use rmcp::{ClientHandler, ServiceExt, model::CallToolRequestParams};

    #[derive(Clone, Default)]
    struct TestClient;

    impl ClientHandler for TestClient {}

    #[derive(Clone)]
    struct FakeDiscovery;

    impl DiscoveryService for FakeDiscovery {
        fn discover(&self, endpoint: &str) -> Result<DiscoverySnapshot, String> {
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
        assert_eq!(names, vec!["kicad_capabilities", "kicad_inspect_project"]);
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
    async fn sdk_rejects_unadvertised_tool() {
        let client = connected(Server::with_discovery(Arc::new(FakeDiscovery))).await;
        let error = client
            .call_tool(CallToolRequestParams::new("raw_protobuf_request"))
            .await
            .expect_err("unsupported tool must fail");
        assert!(error.to_string().contains("not found"));
    }
}
