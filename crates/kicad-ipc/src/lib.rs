include!(concat!(env!("OUT_DIR"), "/generated/mod.rs"));

use std::fmt::{Display, Formatter};
use std::time::Duration;

use nng::options::{Options, RecvTimeout, SendTimeout};
use nng::{Message as NngMessage, Protocol, Socket};
use protobuf::well_known_types::any::Any;
use protobuf::{Message, MessageField};

use envelope::{ApiRequest, ApiRequestHeader, ApiResponse, ApiStatusCode};

pub const GET_VERSION_TYPE_URL: &str = "type.googleapis.com/kiapi.common.commands.GetVersion";

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
                ApiStatusCode::AS_UNIMPLEMENTED => "unsupported_capability",
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
    }
}
