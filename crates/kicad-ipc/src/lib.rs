include!(concat!(env!("OUT_DIR"), "/generated/mod.rs"));

use std::fmt::{Display, Formatter};
use std::time::Duration;

use nng::options::{Options, RecvTimeout, SendTimeout};
use nng::{Message as NngMessage, Protocol, Socket};
use protobuf::well_known_types::any::Any;
use protobuf::{Message, MessageField};

use envelope::{ApiRequest, ApiRequestHeader, ApiResponse, ApiStatusCode};

pub const GET_VERSION_TYPE_URL: &str = "type.googleapis.com/kiapi.common.commands.GetVersion";
pub const GET_OPEN_DOCUMENTS_TYPE_URL: &str =
    "type.googleapis.com/kiapi.common.commands.GetOpenDocuments";

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum OpenDocumentKind {
    Schematic {
        filename: Option<String>,
        sheet_path: Option<String>,
        sheet_kiids: Vec<String>,
    },
    Pcb {
        board_filename: String,
    },
    Other,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct OpenDocument {
    pub project_name: Option<String>,
    pub project_path: Option<String>,
    pub kind: OpenDocumentKind,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ClientConfig {
    pub endpoint: String,
    pub client_name: String,
    pub timeout: Duration,
}

impl ClientConfig {
    pub fn new(
        endpoint: impl Into<String>,
        client_name: impl Into<String>,
    ) -> Result<Self, IpcError> {
        let endpoint = endpoint.into();
        let client_name = client_name.into();
        if endpoint.is_empty() {
            return Err(IpcError::InvalidEndpoint);
        }
        if client_name.is_empty() {
            return Err(IpcError::InvalidClientName);
        }
        Ok(Self {
            endpoint,
            client_name,
            timeout: Duration::from_secs(5),
        })
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct InstanceToken(String);

impl InstanceToken {
    pub fn new(value: impl Into<String>) -> Result<Self, IpcError> {
        let value = value.into();
        if value.is_empty() {
            return Err(IpcError::EmptyInstanceToken);
        }
        Ok(Self(value))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Debug)]
pub enum IpcError {
    InvalidEndpoint,
    InvalidClientName,
    EmptyInstanceToken,
    MalformedEnvelope,
    MissingResponseStatus,
    MissingResponseHeader,
    MissingResponseMessage,
    UnexpectedMessageType {
        expected: &'static str,
        actual: String,
    },
    Status(ApiStatusCode, String),
    Transport(nng::Error),
}

impl PartialEq for IpcError {
    fn eq(&self, other: &Self) -> bool {
        self.code() == other.code()
    }
}

impl Eq for IpcError {}

impl IpcError {
    pub const fn code(&self) -> &'static str {
        match self {
            Self::InvalidEndpoint => "invalid_endpoint",
            Self::InvalidClientName => "invalid_client_name",
            Self::EmptyInstanceToken => "empty_instance_token",
            Self::MalformedEnvelope
            | Self::MissingResponseStatus
            | Self::MissingResponseHeader
            | Self::MissingResponseMessage
            | Self::UnexpectedMessageType { .. } => "protocol_decode_failed",
            Self::Status(status, _) => match status {
                ApiStatusCode::AS_TIMEOUT => "connection_unavailable",
                ApiStatusCode::AS_NOT_READY => "kicad_not_ready",
                ApiStatusCode::AS_TOKEN_MISMATCH => "token_mismatch",
                ApiStatusCode::AS_BUSY => "kicad_busy",
                ApiStatusCode::AS_UNIMPLEMENTED | ApiStatusCode::AS_UNHANDLED => {
                    "unsupported_capability"
                }
                _ => "kicad_status_error",
            },
            Self::Transport(_) => "connection_unavailable",
        }
    }
}

impl Display for IpcError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Status(_, message) if !message.is_empty() => {
                write!(formatter, "{}: {message}", self.code())
            }
            Self::UnexpectedMessageType { actual, .. } => {
                write!(formatter, "{}: unexpected type {actual}", self.code())
            }
            Self::Transport(error) => write!(formatter, "{}: {error}", self.code()),
            _ => formatter.write_str(self.code()),
        }
    }
}

impl std::error::Error for IpcError {}

impl From<nng::Error> for IpcError {
    fn from(error: nng::Error) -> Self {
        Self::Transport(error)
    }
}

pub trait KiCadTransport {
    fn request(&mut self, request: ApiRequest) -> Result<ApiResponse, IpcError>;
}

pub struct NngTransport {
    socket: Socket,
}

impl NngTransport {
    pub fn connect(config: ClientConfig) -> Result<Self, IpcError> {
        let socket = Socket::new(Protocol::Req0)?;
        socket.dial(&config.endpoint)?;
        socket.set_opt::<RecvTimeout>(Some(config.timeout))?;
        socket.set_opt::<SendTimeout>(Some(config.timeout))?;
        Ok(Self { socket })
    }
}

impl KiCadTransport for NngTransport {
    fn request(&mut self, request: ApiRequest) -> Result<ApiResponse, IpcError> {
        let bytes = request
            .write_to_bytes()
            .map_err(|_| IpcError::MalformedEnvelope)?;
        self.socket
            .send(NngMessage::from(&bytes))
            .map_err(|(_, error)| IpcError::Transport(error))?;
        let reply = self.socket.recv()?.as_slice().to_vec();
        ApiResponse::parse_from_bytes(&reply).map_err(|_| IpcError::MalformedEnvelope)
    }
}

pub struct KiCadClient<T> {
    transport: T,
    config: ClientConfig,
    token: Option<InstanceToken>,
}

impl<T: KiCadTransport> KiCadClient<T> {
    pub fn new(transport: T, config: ClientConfig) -> Self {
        Self {
            transport,
            config,
            token: None,
        }
    }

    pub fn get_version(&mut self) -> Result<base_types::KiCadVersion, IpcError> {
        let request = get_version_request(&self.config, self.token.as_ref());
        let response = self.transport.request(request)?;
        response_status(&response)?;
        let token = response_token(&response)?;
        let version = decode_any::<base_commands::GetVersionResponse>(
            &response,
            "type.googleapis.com/kiapi.common.commands.GetVersionResponse",
        )?;
        self.token = Some(token);
        version
            .version
            .into_option()
            .ok_or(IpcError::MissingResponseMessage)
    }

    pub fn get_open_documents(
        &mut self,
        document_type: base_types::DocumentType,
    ) -> Result<Vec<OpenDocument>, IpcError> {
        let request = get_open_documents_request(&self.config, self.token.as_ref(), document_type);
        let response = self.transport.request(request)?;
        response_status(&response)?;
        let token = response_token(&response)?;
        let response = decode_any::<editor_commands::GetOpenDocumentsResponse>(
            &response,
            "type.googleapis.com/kiapi.common.commands.GetOpenDocumentsResponse",
        )?;
        self.token = Some(token);
        response
            .documents
            .iter()
            .map(open_document_from_proto)
            .collect()
    }
}

pub fn get_version_request(config: &ClientConfig, token: Option<&InstanceToken>) -> ApiRequest {
    let command = base_commands::GetVersion::new();
    let any = Any {
        type_url: GET_VERSION_TYPE_URL.to_owned(),
        value: command
            .write_to_bytes()
            .expect("empty GetVersion serializes"),
        ..Any::default()
    };
    ApiRequest {
        header: MessageField::some(ApiRequestHeader {
            client_name: config.client_name.clone(),
            kicad_token: token.map_or_else(String::new, |token| token.as_str().to_owned()),
            ..ApiRequestHeader::default()
        }),
        message: MessageField::some(any),
        ..ApiRequest::default()
    }
}

pub fn get_open_documents_request(
    config: &ClientConfig,
    token: Option<&InstanceToken>,
    document_type: base_types::DocumentType,
) -> ApiRequest {
    let command = editor_commands::GetOpenDocuments {
        type_: protobuf::EnumOrUnknown::new(document_type),
        ..editor_commands::GetOpenDocuments::default()
    };
    let any = Any {
        type_url: GET_OPEN_DOCUMENTS_TYPE_URL.to_owned(),
        value: command
            .write_to_bytes()
            .expect("GetOpenDocuments serializes"),
        ..Any::default()
    };
    ApiRequest {
        header: MessageField::some(ApiRequestHeader {
            client_name: config.client_name.clone(),
            kicad_token: token.map_or_else(String::new, |token| token.as_str().to_owned()),
            ..ApiRequestHeader::default()
        }),
        message: MessageField::some(any),
        ..ApiRequest::default()
    }
}

fn open_document_from_proto(
    document: &base_types::DocumentSpecifier,
) -> Result<OpenDocument, IpcError> {
    let project_name = document
        .project
        .as_ref()
        .map(|project| project.name.clone());
    let project_path = document
        .project
        .as_ref()
        .map(|project| project.path.clone());
    let document_type = document.type_.enum_value_or_default();
    let kind = match (document_type, document.identifier.as_ref()) {
        (
            base_types::DocumentType::DOCTYPE_SCHEMATIC,
            Some(base_types::document_specifier::Identifier::SheetPath(sheet_path)),
        ) => OpenDocumentKind::Schematic {
            filename: None,
            sheet_path: Some(sheet_path.path_human_readable.clone()),
            sheet_kiids: sheet_path
                .path
                .iter()
                .map(|kiid| kiid.value.clone())
                .collect(),
        },
        (
            base_types::DocumentType::DOCTYPE_SCHEMATIC,
            Some(base_types::document_specifier::Identifier::BoardFilename(filename)),
        ) => OpenDocumentKind::Schematic {
            filename: Some(filename.clone()),
            sheet_path: None,
            sheet_kiids: Vec::new(),
        },
        (
            base_types::DocumentType::DOCTYPE_PCB,
            Some(base_types::document_specifier::Identifier::BoardFilename(filename)),
        ) => OpenDocumentKind::Pcb {
            board_filename: filename.clone(),
        },
        _ => OpenDocumentKind::Other,
    };
    Ok(OpenDocument {
        project_name,
        project_path,
        kind,
    })
}

pub fn response_status(response: &ApiResponse) -> Result<(), IpcError> {
    let status = response
        .status
        .as_ref()
        .ok_or(IpcError::MissingResponseStatus)?;
    let code = status.status.enum_value_or_default();
    if code == ApiStatusCode::AS_OK {
        Ok(())
    } else {
        Err(IpcError::Status(code, status.error_message.clone()))
    }
}

pub fn response_token(response: &ApiResponse) -> Result<InstanceToken, IpcError> {
    let header = response
        .header
        .as_ref()
        .ok_or(IpcError::MissingResponseHeader)?;
    InstanceToken::new(header.kicad_token.clone())
}

fn decode_any<M: Message + Default>(
    response: &ApiResponse,
    expected: &'static str,
) -> Result<M, IpcError> {
    let any = response
        .message
        .as_ref()
        .ok_or(IpcError::MissingResponseMessage)?;
    if any.type_url != expected {
        return Err(IpcError::UnexpectedMessageType {
            expected,
            actual: any.type_url.clone(),
        });
    }
    M::parse_from_bytes(&any.value).map_err(|_| IpcError::MalformedEnvelope)
}

#[cfg(test)]
mod tests {
    use super::*;

    struct FakeTransport {
        responses: Vec<ApiResponse>,
        requests: Vec<ApiRequest>,
    }

    impl KiCadTransport for FakeTransport {
        fn request(&mut self, request: ApiRequest) -> Result<ApiResponse, IpcError> {
            self.requests.push(request);
            Ok(self.responses.remove(0))
        }
    }

    fn open_documents_response(token: &str) -> ApiResponse {
        let response = editor_commands::GetOpenDocumentsResponse {
            documents: vec![
                base_types::DocumentSpecifier {
                    type_: protobuf::EnumOrUnknown::new(base_types::DocumentType::DOCTYPE_PCB),
                    identifier: Some(base_types::document_specifier::Identifier::BoardFilename(
                        "main.kicad_pcb".to_owned(),
                    )),
                    project: MessageField::some(base_types::ProjectSpecifier {
                        name: "main".to_owned(),
                        path: "/tmp/main".to_owned(),
                        ..base_types::ProjectSpecifier::default()
                    }),
                    ..base_types::DocumentSpecifier::default()
                },
                base_types::DocumentSpecifier {
                    type_: protobuf::EnumOrUnknown::new(
                        base_types::DocumentType::DOCTYPE_SCHEMATIC,
                    ),
                    identifier: Some(base_types::document_specifier::Identifier::SheetPath(
                        base_types::SheetPath {
                            path_human_readable: "/".to_owned(),
                            path: vec![base_types::KIID {
                                value: "root".to_owned(),
                                ..base_types::KIID::default()
                            }],
                            ..base_types::SheetPath::default()
                        },
                    )),
                    project: MessageField::some(base_types::ProjectSpecifier {
                        name: "main".to_owned(),
                        path: "/tmp/main".to_owned(),
                        ..base_types::ProjectSpecifier::default()
                    }),
                    ..base_types::DocumentSpecifier::default()
                },
                base_types::DocumentSpecifier {
                    type_: protobuf::EnumOrUnknown::new(
                        base_types::DocumentType::DOCTYPE_SCHEMATIC,
                    ),
                    identifier: Some(base_types::document_specifier::Identifier::BoardFilename(
                        "xLock.kicad_sch".to_owned(),
                    )),
                    ..base_types::DocumentSpecifier::default()
                },
            ],
            ..editor_commands::GetOpenDocumentsResponse::default()
        };
        ApiResponse {
            header: MessageField::some(envelope::ApiResponseHeader {
                kicad_token: token.to_owned(),
                ..envelope::ApiResponseHeader::default()
            }),
            status: MessageField::some(envelope::ApiResponseStatus {
                status: protobuf::EnumOrUnknown::new(ApiStatusCode::AS_OK),
                ..envelope::ApiResponseStatus::default()
            }),
            message: MessageField::some(Any {
                type_url: "type.googleapis.com/kiapi.common.commands.GetOpenDocumentsResponse"
                    .to_owned(),
                value: response.write_to_bytes().unwrap(),
                ..Any::default()
            }),
            ..ApiResponse::default()
        }
    }

    fn version_response(token: &str, major: u32) -> ApiResponse {
        let version = base_commands::GetVersionResponse {
            version: MessageField::some(base_types::KiCadVersion {
                major,
                ..base_types::KiCadVersion::default()
            }),
            ..base_commands::GetVersionResponse::default()
        };
        ApiResponse {
            header: MessageField::some(envelope::ApiResponseHeader {
                kicad_token: token.to_owned(),
                ..envelope::ApiResponseHeader::default()
            }),
            status: MessageField::some(envelope::ApiResponseStatus {
                status: protobuf::EnumOrUnknown::new(ApiStatusCode::AS_OK),
                ..envelope::ApiResponseStatus::default()
            }),
            message: MessageField::some(Any {
                type_url: "type.googleapis.com/kiapi.common.commands.GetVersionResponse".to_owned(),
                value: version.write_to_bytes().unwrap(),
                ..Any::default()
            }),
            ..ApiResponse::default()
        }
    }

    #[test]
    fn client_captures_and_reuses_instance_token() {
        let config = ClientConfig::new("ipc:///tmp/kicad/api.sock", "test-client").unwrap();
        let fake = FakeTransport {
            responses: vec![version_response("token", 10), version_response("token", 10)],
            requests: Vec::new(),
        };
        let mut client = KiCadClient::new(fake, config);
        assert_eq!(client.get_version().unwrap().major, 10);
        assert_eq!(client.get_version().unwrap().major, 10);
        assert_eq!(
            client.transport.requests[1]
                .header
                .as_ref()
                .unwrap()
                .kicad_token,
            "token"
        );
    }

    #[test]
    fn request_round_trip_preserves_any_and_empty_token() {
        let config = ClientConfig::new("ipc:///tmp/kicad/api.sock", "test-client").unwrap();
        let request = get_version_request(&config, None);
        let bytes = request.write_to_bytes().unwrap();
        let decoded = ApiRequest::parse_from_bytes(&bytes).unwrap();
        assert_eq!(decoded.header.unwrap().kicad_token, "");
        assert_eq!(decoded.message.unwrap().type_url, GET_VERSION_TYPE_URL);
    }

    #[test]
    fn get_open_documents_request_encodes_document_type() {
        let config = ClientConfig::new("ipc:///tmp/kicad/api.sock", "test-client").unwrap();
        let request = get_open_documents_request(
            &config,
            Some(&InstanceToken::new("token").unwrap()),
            base_types::DocumentType::DOCTYPE_PCB,
        );
        let any = request.message.as_ref().unwrap();
        let command = editor_commands::GetOpenDocuments::parse_from_bytes(&any.value).unwrap();
        assert_eq!(
            command.type_.enum_value_or_default(),
            base_types::DocumentType::DOCTYPE_PCB
        );
        assert_eq!(any.type_url, GET_OPEN_DOCUMENTS_TYPE_URL);
        assert_eq!(request.header.as_ref().unwrap().kicad_token, "token");
    }

    #[test]
    fn client_decodes_open_document_identities() {
        let config = ClientConfig::new("ipc:///tmp/kicad/api.sock", "test-client").unwrap();
        let fake = FakeTransport {
            responses: vec![open_documents_response("token")],
            requests: Vec::new(),
        };
        let mut client = KiCadClient::new(fake, config);
        let documents = client
            .get_open_documents(base_types::DocumentType::DOCTYPE_PCB)
            .unwrap();
        assert_eq!(documents.len(), 3);
        assert_eq!(documents[0].project_name, Some("main".to_owned()));
        assert_eq!(
            documents[0].kind,
            OpenDocumentKind::Pcb {
                board_filename: "main.kicad_pcb".to_owned()
            }
        );
        assert_eq!(
            documents[1].kind,
            OpenDocumentKind::Schematic {
                filename: None,
                sheet_path: Some("/".to_owned()),
                sheet_kiids: vec!["root".to_owned()]
            }
        );
        assert_eq!(
            documents[2].kind,
            OpenDocumentKind::Schematic {
                filename: Some("xLock.kicad_sch".to_owned()),
                sheet_path: None,
                sheet_kiids: Vec::new()
            }
        );
        assert_eq!(documents[2].project_name, None);
    }

    #[test]
    fn statuses_map_to_stable_errors() {
        let mut response = ApiResponse::new();
        response.status = MessageField::some(envelope::ApiResponseStatus {
            status: protobuf::EnumOrUnknown::new(ApiStatusCode::AS_TOKEN_MISMATCH),
            error_message: "wrong instance".to_owned(),
            ..envelope::ApiResponseStatus::default()
        });
        let error = response_status(&response).unwrap_err();
        assert_eq!(error.code(), "token_mismatch");
        assert_eq!(error.to_string(), "token_mismatch: wrong instance");

        response.status = MessageField::some(envelope::ApiResponseStatus {
            status: protobuf::EnumOrUnknown::new(ApiStatusCode::AS_UNHANDLED),
            error_message: "no handler".to_owned(),
            ..envelope::ApiResponseStatus::default()
        });
        let error = response_status(&response).unwrap_err();
        assert_eq!(error.code(), "unsupported_capability");
        assert_eq!(error.to_string(), "unsupported_capability: no handler");
    }
}
