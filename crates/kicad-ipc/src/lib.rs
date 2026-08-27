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
pub const GET_NETS_TYPE_URL: &str = "type.googleapis.com/kiapi.board.commands.GetNets";
pub const GET_NETS_RESPONSE_TYPE_URL: &str =
    "type.googleapis.com/kiapi.board.commands.NetsResponse";
pub const GET_ITEMS_TYPE_URL: &str = "type.googleapis.com/kiapi.common.commands.GetItems";
pub const GET_ITEMS_RESPONSE_TYPE_URL: &str =
    "type.googleapis.com/kiapi.common.commands.GetItemsResponse";
pub const BEGIN_COMMIT_TYPE_URL: &str = "type.googleapis.com/kiapi.common.commands.BeginCommit";
pub const BEGIN_COMMIT_RESPONSE_TYPE_URL: &str =
    "type.googleapis.com/kiapi.common.commands.BeginCommitResponse";
pub const CREATE_ITEMS_TYPE_URL: &str = "type.googleapis.com/kiapi.common.commands.CreateItems";
pub const CREATE_ITEMS_RESPONSE_TYPE_URL: &str =
    "type.googleapis.com/kiapi.common.commands.CreateItemsResponse";
pub const END_COMMIT_TYPE_URL: &str = "type.googleapis.com/kiapi.common.commands.EndCommit";
pub const END_COMMIT_RESPONSE_TYPE_URL: &str =
    "type.googleapis.com/kiapi.common.commands.EndCommitResponse";
pub const SAVE_DOCUMENT_TYPE_URL: &str = "type.googleapis.com/kiapi.common.commands.SaveDocument";
pub const EMPTY_RESPONSE_TYPE_URL: &str = "type.googleapis.com/google.protobuf.Empty";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DocumentQuery {
    Schematic,
    Pcb,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct VersionInfo {
    pub major: u32,
    pub minor: u32,
    pub patch: u32,
    pub full_version: String,
}

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
pub struct BoardNet {
    pub code: Option<i32>,
    pub name: String,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BoardGraphicLayer {
    DwgsUser,
    FSilkS,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BoardGraphicSegment {
    pub start_x_nm: i64,
    pub start_y_nm: i64,
    pub end_x_nm: i64,
    pub end_y_nm: i64,
    pub width_nm: i64,
    pub layer: BoardGraphicLayer,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ObservedBoardGraphicSegment {
    pub graphic_id: String,
    pub segment: BoardGraphicSegment,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PcbItemKind {
    Footprint,
    Pad,
    Trace,
    Via,
    Zone,
}

impl PcbItemKind {
    pub fn all() -> [Self; 5] {
        [
            Self::Footprint,
            Self::Pad,
            Self::Trace,
            Self::Via,
            Self::Zone,
        ]
    }

    pub fn name(self) -> &'static str {
        match self {
            Self::Footprint => "footprint",
            Self::Pad => "pad",
            Self::Trace => "trace",
            Self::Via => "via",
            Self::Zone => "zone",
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PcbItemDiagnostic {
    pub requested_kind: PcbItemKind,
    pub type_url: String,
    pub byte_length: usize,
    pub decoded_kind: String,
    pub stable_id: Option<String>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FootprintSide {
    Front,
    Back,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ObservedLockState {
    Unknown,
    Unlocked,
    Locked,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ObservedFootprintLibraryIdentifier {
    pub nickname: String,
    pub entry_name: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ObservedFootprintInstance {
    pub source_uuid: String,
    pub position_x_nm: i64,
    pub position_y_nm: i64,
    pub orientation_mdeg: i64,
    pub side: FootprintSide,
    pub lock_state: ObservedLockState,
    pub library_identifier: ObservedFootprintLibraryIdentifier,
    pub reference: String,
    pub value: String,
    pub symbol_sheet_path: Vec<String>,
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum ObservedBoardLayer {
    FrontCopper,
    InnerCopper(u8),
    BackCopper,
    FrontMask,
    BackMask,
    FrontPaste,
    BackPaste,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ObservedPadKind {
    Unknown,
    PlatedThroughHole,
    SurfaceMount,
    EdgeConnector,
    NonPlatedThroughHole,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ObservedPadStackType {
    Unknown,
    Normal,
    FrontInnerBack,
    Custom,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ObservedPadShape {
    Unknown,
    Circle,
    Rectangle,
    Oval,
    Trapezoid,
    RoundedRectangle,
    ChamferedRectangle,
    Custom,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ObservedPadCopperShape {
    pub layer: ObservedBoardLayer,
    pub shape: ObservedPadShape,
    pub size_x_nm: i64,
    pub size_y_nm: i64,
    pub offset_x_nm: i64,
    pub offset_y_nm: i64,
    pub corner_rounding_ratio: f64,
    pub chamfer_ratio: f64,
    pub chamfer_top_left: bool,
    pub chamfer_top_right: bool,
    pub chamfer_bottom_left: bool,
    pub chamfer_bottom_right: bool,
    pub trapezoid_delta_x_nm: i64,
    pub trapezoid_delta_y_nm: i64,
    pub custom_anchor_shape: Option<ObservedPadShape>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ObservedDrillShape {
    Unknown,
    Circle,
    Oblong,
    Undefined,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ObservedPadDrill {
    pub shape: ObservedDrillShape,
    pub size_x_nm: i64,
    pub size_y_nm: i64,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ObservedPad {
    pub source_uuid: String,
    pub footprint_source_uuid: String,
    pub number: String,
    pub kind: ObservedPadKind,
    pub lock_state: ObservedLockState,
    pub position_x_nm: i64,
    pub position_y_nm: i64,
    pub stack_type: ObservedPadStackType,
    pub orientation_mdeg: i64,
    pub layers: Vec<ObservedBoardLayer>,
    pub copper_shapes: Vec<ObservedPadCopperShape>,
    pub drill: Option<ObservedPadDrill>,
    pub net_name: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CommitId(String);

impl CommitId {
    pub fn new(value: impl Into<String>) -> Result<Self, IpcError> {
        let value = value.into();
        if value.is_empty() {
            return Err(IpcError::EmptyCommitId);
        }
        Ok(Self(value))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
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
    EmptyCommitId,
    MalformedEnvelope,
    MissingResponseStatus,
    MissingResponseHeader,
    MissingResponseMessage,
    UnexpectedMessageType {
        expected: &'static str,
        actual: String,
    },
    CreateItemsCountMismatch {
        expected: usize,
        actual: usize,
    },
    CreateItemFailed {
        index: usize,
        status: i32,
        message: String,
    },
    InvalidBoardGraphic(&'static str),
    InvalidObservedBoardGraphic(&'static str),
    InvalidObservedFootprint(&'static str),
    InvalidObservedPad(&'static str),
    UnknownPcbItemType(String),
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
            Self::EmptyCommitId => "empty_commit_id",
            Self::MalformedEnvelope
            | Self::MissingResponseStatus
            | Self::MissingResponseHeader
            | Self::MissingResponseMessage
            | Self::UnexpectedMessageType { .. } => "protocol_decode_failed",
            Self::CreateItemsCountMismatch { .. } => "create_items_count_mismatch",
            Self::CreateItemFailed { .. } => "create_item_failed",
            Self::InvalidBoardGraphic(_) => "invalid_board_graphic",
            Self::InvalidObservedBoardGraphic(_) => "invalid_observed_board_graphic",
            Self::InvalidObservedFootprint(_) => "invalid_observed_footprint",
            Self::InvalidObservedPad(_) => "invalid_observed_pad",
            Self::UnknownPcbItemType(_) => "unknown_pcb_item_type",
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
            Self::CreateItemsCountMismatch { expected, actual } => write!(
                formatter,
                "{}: expected {expected} results, received {actual}",
                self.code()
            ),
            Self::CreateItemFailed {
                index,
                status,
                message,
            } if !message.is_empty() => write!(
                formatter,
                "{}: item {index} status {status}: {message}",
                self.code()
            ),
            Self::CreateItemFailed { index, status, .. } => {
                write!(formatter, "{}: item {index} status {status}", self.code())
            }
            Self::InvalidBoardGraphic(message) => {
                write!(formatter, "{}: {message}", self.code())
            }
            Self::InvalidObservedBoardGraphic(message) => {
                write!(formatter, "{}: {message}", self.code())
            }
            Self::InvalidObservedFootprint(message) => {
                write!(formatter, "{}: {message}", self.code())
            }
            Self::InvalidObservedPad(message) => {
                write!(formatter, "{}: {message}", self.code())
            }
            Self::UnknownPcbItemType(type_url) => {
                write!(formatter, "{}: {type_url}", self.code())
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

    pub fn instance_token(&self) -> Option<&InstanceToken> {
        self.token.as_ref()
    }

    pub fn get_version(&mut self) -> Result<VersionInfo, IpcError> {
        let request = get_version_request(&self.config, self.token.as_ref());
        let response = self.transport.request(request)?;
        response_status(&response)?;
        let token = response_token(&response)?;
        let version = decode_any::<base_commands::GetVersionResponse>(
            &response,
            "type.googleapis.com/kiapi.common.commands.GetVersionResponse",
        )?
        .version
        .into_option()
        .ok_or(IpcError::MissingResponseMessage)?;
        self.token = Some(token);
        Ok(VersionInfo {
            major: version.major,
            minor: version.minor,
            patch: version.patch,
            full_version: version.full_version,
        })
    }

    pub fn begin_commit(&mut self) -> Result<CommitId, IpcError> {
        let request = simple_command_request(
            &self.config,
            self.token.as_ref(),
            BEGIN_COMMIT_TYPE_URL,
            &editor_commands::BeginCommit::default(),
        )?;
        let response = self.transport.request(request)?;
        response_status(&response)?;
        let token = response_token(&response)?;
        let response = decode_any::<editor_commands::BeginCommitResponse>(
            &response,
            BEGIN_COMMIT_RESPONSE_TYPE_URL,
        )?;
        self.token = Some(token);
        let id = response
            .id
            .into_option()
            .ok_or(IpcError::MissingResponseMessage)?;
        Ok(CommitId(id.value))
    }

    pub fn create_board_graphics(
        &mut self,
        board_filename: &str,
        graphics: &[BoardGraphicSegment],
    ) -> Result<(), IpcError> {
        let items = graphics
            .iter()
            .map(board_graphic_segment_item)
            .collect::<Result<Vec<_>, IpcError>>()?;
        let command = editor_commands::CreateItems {
            header: MessageField::some(board_item_header(board_filename)),
            items,
            ..editor_commands::CreateItems::default()
        };
        let request = command_request(
            &self.config,
            self.token.as_ref(),
            CREATE_ITEMS_TYPE_URL,
            command,
        )?;
        let response = self.transport.request(request)?;
        response_status(&response)?;
        let token = response_token(&response)?;
        let response = decode_any::<editor_commands::CreateItemsResponse>(
            &response,
            CREATE_ITEMS_RESPONSE_TYPE_URL,
        )?;
        self.token = Some(token);
        validate_create_items_response(&response, graphics.len())
    }

    pub fn commit_changes(&mut self, commit: &CommitId, message: &str) -> Result<(), IpcError> {
        self.end_commit(commit, editor_commands::CommitAction::CMA_COMMIT, message)
    }

    pub fn save_board(&mut self, board_filename: &str) -> Result<(), IpcError> {
        let command = editor_commands::SaveDocument {
            document: MessageField::some(board_document(board_filename)),
            ..editor_commands::SaveDocument::default()
        };
        let request = command_request(
            &self.config,
            self.token.as_ref(),
            SAVE_DOCUMENT_TYPE_URL,
            command,
        )?;
        let response = self.transport.request(request)?;
        response_status(&response)?;
        let token = response_token(&response)?;
        let _ = decode_any::<protobuf::well_known_types::empty::Empty>(
            &response,
            EMPTY_RESPONSE_TYPE_URL,
        )?;
        self.token = Some(token);
        Ok(())
    }

    pub fn drop_commit(&mut self, commit: &CommitId) -> Result<(), IpcError> {
        self.end_commit(commit, editor_commands::CommitAction::CMA_DROP, "")
    }

    fn end_commit(
        &mut self,
        commit: &CommitId,
        action: editor_commands::CommitAction,
        message: &str,
    ) -> Result<(), IpcError> {
        let command = editor_commands::EndCommit {
            id: MessageField::some(base_types::KIID {
                value: commit.0.clone(),
                ..base_types::KIID::default()
            }),
            action: protobuf::EnumOrUnknown::new(action),
            message: message.to_owned(),
            ..editor_commands::EndCommit::default()
        };
        let request = command_request(
            &self.config,
            self.token.as_ref(),
            END_COMMIT_TYPE_URL,
            command,
        )?;
        let response = self.transport.request(request)?;
        response_status(&response)?;
        let token = response_token(&response)?;
        let _ = decode_any::<editor_commands::EndCommitResponse>(
            &response,
            END_COMMIT_RESPONSE_TYPE_URL,
        )?;
        self.token = Some(token);
        Ok(())
    }

    pub fn get_board_nets(&mut self, board_filename: &str) -> Result<Vec<BoardNet>, IpcError> {
        let request = get_nets_request(&self.config, self.token.as_ref(), board_filename);
        let response = self.transport.request(request)?;
        response_status(&response)?;
        let token = response_token(&response)?;
        let response =
            decode_any::<board_commands::NetsResponse>(&response, GET_NETS_RESPONSE_TYPE_URL)?;
        self.token = Some(token);
        Ok(response
            .nets
            .iter()
            .map(|net| BoardNet {
                code: net.code.as_ref().map(|code| code.value),
                name: net.name.clone(),
            })
            .collect())
    }

    pub fn get_pcb_item_diagnostics(
        &mut self,
        board_filename: &str,
        kinds: &[PcbItemKind],
    ) -> Result<Vec<PcbItemDiagnostic>, IpcError> {
        if kinds.is_empty() {
            return Ok(Vec::new());
        }
        let types = kinds
            .iter()
            .map(|kind| {
                protobuf::EnumOrUnknown::new(match kind {
                    PcbItemKind::Footprint => enums::KiCadObjectType::KOT_PCB_FOOTPRINT,
                    PcbItemKind::Pad => enums::KiCadObjectType::KOT_PCB_PAD,
                    PcbItemKind::Trace => enums::KiCadObjectType::KOT_PCB_TRACE,
                    PcbItemKind::Via => enums::KiCadObjectType::KOT_PCB_VIA,
                    PcbItemKind::Zone => enums::KiCadObjectType::KOT_PCB_ZONE,
                })
            })
            .collect();
        let command = editor_commands::GetItems {
            header: MessageField::some(board_item_header(board_filename)),
            types,
            ..editor_commands::GetItems::default()
        };
        let request = command_request(
            &self.config,
            self.token.as_ref(),
            GET_ITEMS_TYPE_URL,
            command,
        )?;
        let response = self.transport.request(request)?;
        response_status(&response)?;
        let token = response_token(&response)?;
        let response = decode_any::<editor_commands::GetItemsResponse>(
            &response,
            GET_ITEMS_RESPONSE_TYPE_URL,
        )?;
        let diagnostics = pcb_item_diagnostics(&response, kinds)?;
        self.token = Some(token);
        Ok(diagnostics)
    }

    pub fn get_footprint_instances(
        &mut self,
        board_filename: &str,
    ) -> Result<Vec<ObservedFootprintInstance>, IpcError> {
        let command = editor_commands::GetItems {
            header: MessageField::some(board_item_header(board_filename)),
            types: vec![protobuf::EnumOrUnknown::new(
                enums::KiCadObjectType::KOT_PCB_FOOTPRINT,
            )],
            ..editor_commands::GetItems::default()
        };
        let request = command_request(
            &self.config,
            self.token.as_ref(),
            GET_ITEMS_TYPE_URL,
            command,
        )?;
        let response = self.transport.request(request)?;
        response_status(&response)?;
        let token = response_token(&response)?;
        let response = decode_any::<editor_commands::GetItemsResponse>(
            &response,
            GET_ITEMS_RESPONSE_TYPE_URL,
        )?;
        let footprints = footprint_instances_from_response(&response)?;
        self.token = Some(token);
        Ok(footprints)
    }

    pub fn get_footprint_pads(
        &mut self,
        board_filename: &str,
        footprint_source_uuid: &str,
    ) -> Result<Vec<ObservedPad>, IpcError> {
        if footprint_source_uuid.is_empty() {
            return Err(IpcError::InvalidObservedPad(
                "parent footprint identity is missing",
            ));
        }
        let command = editor_commands::GetItems {
            header: MessageField::some(footprint_item_header(
                board_filename,
                footprint_source_uuid,
            )),
            types: vec![protobuf::EnumOrUnknown::new(
                enums::KiCadObjectType::KOT_PCB_PAD,
            )],
            ..editor_commands::GetItems::default()
        };
        let request = command_request(
            &self.config,
            self.token.as_ref(),
            GET_ITEMS_TYPE_URL,
            command,
        )?;
        let response = self.transport.request(request)?;
        response_status(&response)?;
        let token = response_token(&response)?;
        let response = decode_any::<editor_commands::GetItemsResponse>(
            &response,
            GET_ITEMS_RESPONSE_TYPE_URL,
        )?;
        let pads = footprint_pads_from_response(&response, footprint_source_uuid)?;
        self.token = Some(token);
        Ok(pads)
    }

    pub fn get_board_graphics(
        &mut self,
        board_filename: &str,
    ) -> Result<Vec<ObservedBoardGraphicSegment>, IpcError> {
        let command = editor_commands::GetItems {
            header: MessageField::some(board_item_header(board_filename)),
            types: vec![protobuf::EnumOrUnknown::new(
                enums::KiCadObjectType::KOT_PCB_SHAPE,
            )],
            ..editor_commands::GetItems::default()
        };
        let request = command_request(
            &self.config,
            self.token.as_ref(),
            GET_ITEMS_TYPE_URL,
            command,
        )?;
        let response = self.transport.request(request)?;
        response_status(&response)?;
        let token = response_token(&response)?;
        let response = decode_any::<editor_commands::GetItemsResponse>(
            &response,
            GET_ITEMS_RESPONSE_TYPE_URL,
        )?;
        let graphics = board_graphics_from_response(&response)?;
        self.token = Some(token);
        Ok(graphics)
    }

    pub fn get_open_documents(
        &mut self,
        query: DocumentQuery,
    ) -> Result<Vec<OpenDocument>, IpcError> {
        let request = get_open_documents_request(&self.config, self.token.as_ref(), query);
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

fn command_request<M: Message>(
    config: &ClientConfig,
    token: Option<&InstanceToken>,
    type_url: &str,
    command: M,
) -> Result<ApiRequest, IpcError> {
    let any = Any {
        type_url: type_url.to_owned(),
        value: command
            .write_to_bytes()
            .map_err(|_| IpcError::MalformedEnvelope)?,
        ..Any::default()
    };
    Ok(ApiRequest {
        header: MessageField::some(ApiRequestHeader {
            client_name: config.client_name.clone(),
            kicad_token: token.map_or_else(String::new, |token| token.as_str().to_owned()),
            ..ApiRequestHeader::default()
        }),
        message: MessageField::some(any),
        ..ApiRequest::default()
    })
}

fn simple_command_request<M: Message>(
    config: &ClientConfig,
    token: Option<&InstanceToken>,
    type_url: &str,
    command: &M,
) -> Result<ApiRequest, IpcError> {
    command_request(config, token, type_url, command.clone())
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

fn board_document(board_filename: &str) -> base_types::DocumentSpecifier {
    let mut board = base_types::DocumentSpecifier {
        type_: protobuf::EnumOrUnknown::new(base_types::DocumentType::DOCTYPE_PCB),
        ..base_types::DocumentSpecifier::default()
    };
    board.set_board_filename(board_filename.to_owned());
    board
}

fn board_item_header(board_filename: &str) -> base_types::ItemHeader {
    base_types::ItemHeader {
        document: MessageField::some(board_document(board_filename)),
        ..base_types::ItemHeader::default()
    }
}

fn footprint_item_header(
    board_filename: &str,
    footprint_source_uuid: &str,
) -> base_types::ItemHeader {
    base_types::ItemHeader {
        document: MessageField::some(board_document(board_filename)),
        container: MessageField::some(base_types::KIID {
            value: footprint_source_uuid.to_owned(),
            ..base_types::KIID::default()
        }),
        ..base_types::ItemHeader::default()
    }
}

fn validate_create_items_response(
    response: &editor_commands::CreateItemsResponse,
    expected: usize,
) -> Result<(), IpcError> {
    if response.status.enum_value_or_default() != base_types::ItemRequestStatus::IRS_OK {
        return Err(IpcError::Status(
            ApiStatusCode::AS_BAD_REQUEST,
            format!(
                "create items status: {:?}",
                response.status.enum_value_or_default()
            ),
        ));
    }
    if response.created_items.len() != expected {
        return Err(IpcError::CreateItemsCountMismatch {
            expected,
            actual: response.created_items.len(),
        });
    }
    for (index, result) in response.created_items.iter().enumerate() {
        let status = result.status.as_ref();
        let code = status.map_or(0, |status| status.code.value());
        if code != editor_commands::ItemStatusCode::ISC_OK as i32 {
            return Err(IpcError::CreateItemFailed {
                index,
                status: code,
                message: status.map_or_else(String::new, |status| status.error_message.clone()),
            });
        }
    }
    Ok(())
}

fn pcb_item_diagnostics(
    response: &editor_commands::GetItemsResponse,
    requested_kinds: &[PcbItemKind],
) -> Result<Vec<PcbItemDiagnostic>, IpcError> {
    if response.status.enum_value_or_default() != base_types::ItemRequestStatus::IRS_OK {
        return Err(IpcError::Status(
            ApiStatusCode::AS_BAD_REQUEST,
            format!(
                "get items status: {:?}",
                response.status.enum_value_or_default()
            ),
        ));
    }

    response
        .items
        .iter()
        .map(|item| {
            let (requested_kind, decoded_kind, stable_id) = match item.type_url.as_str() {
                "type.googleapis.com/kiapi.board.types.FootprintInstance" => {
                    let value = board_types::FootprintInstance::parse_from_bytes(&item.value)
                        .map_err(|_| IpcError::MalformedEnvelope)?;
                    (
                        PcbItemKind::Footprint,
                        "FootprintInstance",
                        kiid_value(value.id.as_ref()),
                    )
                }
                "type.googleapis.com/kiapi.board.types.Pad" => {
                    let value = board_types::Pad::parse_from_bytes(&item.value)
                        .map_err(|_| IpcError::MalformedEnvelope)?;
                    (PcbItemKind::Pad, "Pad", kiid_value(value.id.as_ref()))
                }
                "type.googleapis.com/kiapi.board.types.Track" => {
                    let value = board_types::Track::parse_from_bytes(&item.value)
                        .map_err(|_| IpcError::MalformedEnvelope)?;
                    (PcbItemKind::Trace, "Track", kiid_value(value.id.as_ref()))
                }
                "type.googleapis.com/kiapi.board.types.Arc" => {
                    let value = board_types::Arc::parse_from_bytes(&item.value)
                        .map_err(|_| IpcError::MalformedEnvelope)?;
                    (PcbItemKind::Trace, "Arc", kiid_value(value.id.as_ref()))
                }
                "type.googleapis.com/kiapi.board.types.Via" => {
                    let value = board_types::Via::parse_from_bytes(&item.value)
                        .map_err(|_| IpcError::MalformedEnvelope)?;
                    (PcbItemKind::Via, "Via", kiid_value(value.id.as_ref()))
                }
                "type.googleapis.com/kiapi.board.types.Zone" => {
                    let value = board_types::Zone::parse_from_bytes(&item.value)
                        .map_err(|_| IpcError::MalformedEnvelope)?;
                    (PcbItemKind::Zone, "Zone", kiid_value(value.id.as_ref()))
                }
                _ => return Err(IpcError::UnknownPcbItemType(item.type_url.clone())),
            };
            if !requested_kinds.contains(&requested_kind) {
                return Err(IpcError::UnknownPcbItemType(item.type_url.clone()));
            }
            Ok(PcbItemDiagnostic {
                requested_kind,
                type_url: item.type_url.clone(),
                byte_length: item.value.len(),
                decoded_kind: decoded_kind.to_owned(),
                stable_id,
            })
        })
        .collect()
}

fn kiid_value(id: Option<&base_types::KIID>) -> Option<String> {
    id.map(|id| id.value.clone()).filter(|id| !id.is_empty())
}

fn footprint_instances_from_response(
    response: &editor_commands::GetItemsResponse,
) -> Result<Vec<ObservedFootprintInstance>, IpcError> {
    if response.status.enum_value_or_default() != base_types::ItemRequestStatus::IRS_OK {
        return Err(IpcError::Status(
            ApiStatusCode::AS_BAD_REQUEST,
            format!(
                "get items status: {:?}",
                response.status.enum_value_or_default()
            ),
        ));
    }
    let mut footprints = response
        .items
        .iter()
        .map(footprint_instance_from_any)
        .collect::<Result<Vec<_>, _>>()?;
    footprints.sort_by(|left, right| left.source_uuid.cmp(&right.source_uuid));
    if footprints
        .windows(2)
        .any(|pair| pair[0].source_uuid == pair[1].source_uuid)
    {
        return Err(IpcError::InvalidObservedFootprint(
            "footprint identities must be unique",
        ));
    }
    Ok(footprints)
}

fn footprint_instance_from_any(item: &Any) -> Result<ObservedFootprintInstance, IpcError> {
    if item.type_url != "type.googleapis.com/kiapi.board.types.FootprintInstance" {
        return Err(IpcError::UnexpectedMessageType {
            expected: "type.googleapis.com/kiapi.board.types.FootprintInstance",
            actual: item.type_url.clone(),
        });
    }
    let footprint = board_types::FootprintInstance::parse_from_bytes(&item.value)
        .map_err(|_| IpcError::MalformedEnvelope)?;
    let source_uuid = footprint
        .id
        .as_ref()
        .map(|id| id.value.clone())
        .filter(|id| !id.is_empty())
        .ok_or(IpcError::InvalidObservedFootprint(
            "footprint identity is missing",
        ))?;
    let position = footprint
        .position
        .as_ref()
        .ok_or(IpcError::InvalidObservedFootprint(
            "footprint position is missing",
        ))?;
    let orientation = footprint
        .orientation
        .as_ref()
        .ok_or(IpcError::InvalidObservedFootprint(
            "footprint orientation is missing",
        ))?;
    let orientation_mdeg = (orientation.value_degrees * 1000.0).round();
    if !orientation_mdeg.is_finite()
        || !(-9_223_372_036_854_775_808.0..9_223_372_036_854_775_808.0).contains(&orientation_mdeg)
    {
        return Err(IpcError::InvalidObservedFootprint(
            "footprint orientation is invalid",
        ));
    }
    let side = match footprint.layer.enum_value() {
        Ok(board_types::BoardLayer::BL_F_Cu) => FootprintSide::Front,
        Ok(board_types::BoardLayer::BL_B_Cu) => FootprintSide::Back,
        _ => {
            return Err(IpcError::InvalidObservedFootprint(
                "footprint layer is not front or back copper",
            ));
        }
    };
    let lock_state = match footprint.locked.enum_value() {
        Ok(base_types::LockedState::LS_UNLOCKED) => ObservedLockState::Unlocked,
        Ok(base_types::LockedState::LS_LOCKED) => ObservedLockState::Locked,
        _ => ObservedLockState::Unknown,
    };
    let definition = footprint
        .definition
        .as_ref()
        .ok_or(IpcError::InvalidObservedFootprint(
            "footprint definition is missing",
        ))?;
    let library = definition
        .id
        .as_ref()
        .ok_or(IpcError::InvalidObservedFootprint(
            "footprint library identity is missing",
        ))?;
    if library.entry_name.is_empty() {
        return Err(IpcError::InvalidObservedFootprint(
            "footprint library entry is missing",
        ));
    }
    let reference = field_text(footprint.reference_field.as_ref())?;
    let value = field_text(footprint.value_field.as_ref())?;
    let symbol_sheet_path = footprint
        .symbol_path
        .as_ref()
        .map(|path| {
            path.path
                .iter()
                .map(|id| {
                    if id.value.is_empty() {
                        Err(IpcError::InvalidObservedFootprint(
                            "symbol sheet path contains an empty identity",
                        ))
                    } else {
                        Ok(id.value.clone())
                    }
                })
                .collect::<Result<Vec<_>, _>>()
        })
        .transpose()?
        .unwrap_or_default();
    Ok(ObservedFootprintInstance {
        source_uuid,
        position_x_nm: position.x_nm,
        position_y_nm: position.y_nm,
        orientation_mdeg: orientation_mdeg as i64,
        side,
        lock_state,
        library_identifier: ObservedFootprintLibraryIdentifier {
            nickname: library.library_nickname.clone(),
            entry_name: library.entry_name.clone(),
        },
        reference,
        value,
        symbol_sheet_path,
    })
}

fn field_text(field: Option<&board_types::Field>) -> Result<String, IpcError> {
    field
        .and_then(|field| field.text.as_ref())
        .and_then(|text| text.text.as_ref())
        .map(|text| text.text.clone())
        .ok_or(IpcError::InvalidObservedFootprint(
            "footprint field text is missing",
        ))
}

fn footprint_pads_from_response(
    response: &editor_commands::GetItemsResponse,
    footprint_source_uuid: &str,
) -> Result<Vec<ObservedPad>, IpcError> {
    if response.status.enum_value_or_default() != base_types::ItemRequestStatus::IRS_OK {
        return Err(IpcError::Status(
            ApiStatusCode::AS_BAD_REQUEST,
            format!(
                "get items status: {:?}",
                response.status.enum_value_or_default()
            ),
        ));
    }
    if let Some(container) = response
        .header
        .as_ref()
        .and_then(|header| header.container.as_ref())
        && container.value != footprint_source_uuid
    {
        return Err(IpcError::InvalidObservedPad(
            "response container does not match parent footprint",
        ));
    }
    let mut pads = response
        .items
        .iter()
        .map(|item| footprint_pad_from_any(item, footprint_source_uuid))
        .collect::<Result<Vec<_>, _>>()?;
    pads.sort_by(|left, right| left.source_uuid.cmp(&right.source_uuid));
    if pads
        .windows(2)
        .any(|pair| pair[0].source_uuid == pair[1].source_uuid)
    {
        return Err(IpcError::InvalidObservedPad(
            "pad identities must be unique",
        ));
    }
    Ok(pads)
}

fn footprint_pad_from_any(
    item: &Any,
    footprint_source_uuid: &str,
) -> Result<ObservedPad, IpcError> {
    if item.type_url != "type.googleapis.com/kiapi.board.types.Pad" {
        return Err(IpcError::UnexpectedMessageType {
            expected: "type.googleapis.com/kiapi.board.types.Pad",
            actual: item.type_url.clone(),
        });
    }
    let pad =
        board_types::Pad::parse_from_bytes(&item.value).map_err(|_| IpcError::MalformedEnvelope)?;
    let source_uuid = kiid_value(pad.id.as_ref())
        .ok_or(IpcError::InvalidObservedPad("pad identity is missing"))?;
    let position = pad
        .position
        .as_ref()
        .ok_or(IpcError::InvalidObservedPad("pad position is missing"))?;
    let stack = pad
        .pad_stack
        .as_ref()
        .ok_or(IpcError::InvalidObservedPad("pad stack is missing"))?;
    if stack.secondary_drill.is_some() || stack.tertiary_drill.is_some() {
        return Err(IpcError::InvalidObservedPad(
            "secondary and tertiary drills are unsupported",
        ));
    }
    let stack_type = match stack.type_.enum_value() {
        Ok(board_types::PadStackType::PST_NORMAL) => ObservedPadStackType::Normal,
        Ok(board_types::PadStackType::PST_FRONT_INNER_BACK) => ObservedPadStackType::FrontInnerBack,
        Ok(board_types::PadStackType::PST_CUSTOM) => ObservedPadStackType::Custom,
        _ => ObservedPadStackType::Unknown,
    };
    let orientation_mdeg = match stack.angle.as_ref() {
        Some(angle) => angle_to_mdeg(angle.value_degrees)?,
        None => 0,
    };
    let mut layers = stack
        .layers
        .iter()
        .map(observed_board_layer)
        .collect::<Result<Vec<_>, _>>()?;
    layers.sort();
    if layers.windows(2).any(|pair| pair[0] == pair[1]) {
        return Err(IpcError::InvalidObservedPad("pad layers must be unique"));
    }
    let mut copper_shapes = stack
        .copper_layers
        .iter()
        .map(observed_pad_copper_shape)
        .collect::<Result<Vec<_>, _>>()?;
    copper_shapes.sort_by_key(|shape| shape.layer);
    if copper_shapes
        .windows(2)
        .any(|pair| pair[0].layer == pair[1].layer)
    {
        return Err(IpcError::InvalidObservedPad(
            "pad copper shape layers must be unique",
        ));
    }
    let drill = stack
        .drill
        .as_ref()
        .map(observed_pad_drill)
        .transpose()?
        .flatten();
    let kind = match pad.type_.enum_value() {
        Ok(board_types::PadType::PT_PTH) => ObservedPadKind::PlatedThroughHole,
        Ok(board_types::PadType::PT_SMD) => ObservedPadKind::SurfaceMount,
        Ok(board_types::PadType::PT_EDGE_CONNECTOR) => ObservedPadKind::EdgeConnector,
        Ok(board_types::PadType::PT_NPTH) => ObservedPadKind::NonPlatedThroughHole,
        _ => ObservedPadKind::Unknown,
    };
    let lock_state = match pad.locked.enum_value() {
        Ok(base_types::LockedState::LS_UNLOCKED) => ObservedLockState::Unlocked,
        Ok(base_types::LockedState::LS_LOCKED) => ObservedLockState::Locked,
        _ => ObservedLockState::Unknown,
    };
    Ok(ObservedPad {
        source_uuid,
        footprint_source_uuid: footprint_source_uuid.to_owned(),
        number: pad.number,
        kind,
        lock_state,
        position_x_nm: position.x_nm,
        position_y_nm: position.y_nm,
        stack_type,
        orientation_mdeg,
        layers,
        copper_shapes,
        drill,
        net_name: pad
            .net
            .as_ref()
            .map(|net| net.name.clone())
            .filter(|name| !name.is_empty()),
    })
}

fn angle_to_mdeg(degrees: f64) -> Result<i64, IpcError> {
    let millidegrees = (degrees * 1000.0).round();
    if !millidegrees.is_finite()
        || !(-9_223_372_036_854_775_808.0..9_223_372_036_854_775_808.0).contains(&millidegrees)
    {
        return Err(IpcError::InvalidObservedPad(
            "pad stack orientation is invalid",
        ));
    }
    Ok(millidegrees as i64)
}

fn observed_board_layer(
    layer: &protobuf::EnumOrUnknown<board_types::BoardLayer>,
) -> Result<ObservedBoardLayer, IpcError> {
    let value = layer.value();
    match value {
        3 => Ok(ObservedBoardLayer::FrontCopper),
        4..=33 => Ok(ObservedBoardLayer::InnerCopper((value - 3) as u8)),
        34 => Ok(ObservedBoardLayer::BackCopper),
        37 => Ok(ObservedBoardLayer::BackPaste),
        38 => Ok(ObservedBoardLayer::FrontPaste),
        41 => Ok(ObservedBoardLayer::BackMask),
        42 => Ok(ObservedBoardLayer::FrontMask),
        _ => Err(IpcError::InvalidObservedPad("pad layer is unsupported")),
    }
}

fn observed_pad_copper_shape(
    layer: &board_types::PadStackLayer,
) -> Result<ObservedPadCopperShape, IpcError> {
    if !layer.custom_shapes.is_empty() {
        return Err(IpcError::InvalidObservedPad(
            "custom pad geometry is unsupported",
        ));
    }
    if !layer.corner_rounding_ratio.is_finite() || !layer.chamfer_ratio.is_finite() {
        return Err(IpcError::InvalidObservedPad(
            "pad shape ratios must be finite",
        ));
    }
    let board_layer = observed_board_layer(&layer.layer)?;
    if !matches!(
        board_layer,
        ObservedBoardLayer::FrontCopper
            | ObservedBoardLayer::InnerCopper(_)
            | ObservedBoardLayer::BackCopper
    ) {
        return Err(IpcError::InvalidObservedPad(
            "pad copper shape must use a copper layer",
        ));
    }
    let size = layer
        .size
        .as_ref()
        .ok_or(IpcError::InvalidObservedPad("pad shape size is missing"))?;
    if size.x_nm <= 0 || size.y_nm <= 0 {
        return Err(IpcError::InvalidObservedPad(
            "pad shape size must be positive",
        ));
    }
    let (offset_x_nm, offset_y_nm) = layer
        .offset
        .as_ref()
        .map(|offset| (offset.x_nm, offset.y_nm))
        .unwrap_or_default();
    let chamfered_corners = layer.chamfered_corners.as_ref();
    let (trapezoid_delta_x_nm, trapezoid_delta_y_nm) = layer
        .trapezoid_delta
        .as_ref()
        .map(|delta| (delta.x_nm, delta.y_nm))
        .unwrap_or_default();
    let custom_anchor_shape = match layer.custom_anchor_shape.enum_value() {
        Ok(board_types::PadStackShape::PSS_UNKNOWN) => None,
        Ok(board_types::PadStackShape::PSS_CIRCLE) => Some(ObservedPadShape::Circle),
        Ok(board_types::PadStackShape::PSS_RECTANGLE) => Some(ObservedPadShape::Rectangle),
        _ => {
            return Err(IpcError::InvalidObservedPad(
                "custom pad anchor shape is unsupported",
            ));
        }
    };
    let shape = match layer.shape.enum_value() {
        Ok(board_types::PadStackShape::PSS_CIRCLE) => ObservedPadShape::Circle,
        Ok(board_types::PadStackShape::PSS_RECTANGLE) => ObservedPadShape::Rectangle,
        Ok(board_types::PadStackShape::PSS_OVAL) => ObservedPadShape::Oval,
        Ok(board_types::PadStackShape::PSS_TRAPEZOID) => ObservedPadShape::Trapezoid,
        Ok(board_types::PadStackShape::PSS_ROUNDRECT) => ObservedPadShape::RoundedRectangle,
        Ok(board_types::PadStackShape::PSS_CHAMFEREDRECT) => ObservedPadShape::ChamferedRectangle,
        Ok(board_types::PadStackShape::PSS_CUSTOM) => {
            return Err(IpcError::InvalidObservedPad(
                "custom pad geometry is unsupported",
            ));
        }
        _ => ObservedPadShape::Unknown,
    };
    Ok(ObservedPadCopperShape {
        layer: board_layer,
        shape,
        size_x_nm: size.x_nm,
        size_y_nm: size.y_nm,
        offset_x_nm,
        offset_y_nm,
        corner_rounding_ratio: layer.corner_rounding_ratio,
        chamfer_ratio: layer.chamfer_ratio,
        chamfer_top_left: chamfered_corners.is_some_and(|corners| corners.top_left),
        chamfer_top_right: chamfered_corners.is_some_and(|corners| corners.top_right),
        chamfer_bottom_left: chamfered_corners.is_some_and(|corners| corners.bottom_left),
        chamfer_bottom_right: chamfered_corners.is_some_and(|corners| corners.bottom_right),
        trapezoid_delta_x_nm,
        trapezoid_delta_y_nm,
        custom_anchor_shape,
    })
}

fn observed_pad_drill(
    drill: &board_types::DrillProperties,
) -> Result<Option<ObservedPadDrill>, IpcError> {
    let is_empty_metadata = (drill.start_layer.value() == 0
        && drill.end_layer.value() == 0
        && drill.shape.value() == 0
        || drill.start_layer.value() == board_types::BoardLayer::BL_F_Cu as i32
            && drill.end_layer.value() == board_types::BoardLayer::BL_B_Cu as i32
            && drill.shape.value() == board_types::DrillShape::DS_CIRCLE as i32)
        && drill.capped.value() == 0
        && drill.filled.value() == 0;
    let Some(diameter) = drill.diameter.as_ref() else {
        return if !is_empty_metadata {
            Err(IpcError::InvalidObservedPad(
                "pad drill metadata requires a positive size",
            ))
        } else {
            Ok(None)
        };
    };
    if diameter.x_nm == 0 && diameter.y_nm == 0 {
        return if !is_empty_metadata {
            Err(IpcError::InvalidObservedPad(
                "pad drill metadata requires a positive size",
            ))
        } else {
            Ok(None)
        };
    }
    if diameter.x_nm <= 0 || diameter.y_nm <= 0 {
        return Err(IpcError::InvalidObservedPad(
            "pad drill size must be positive",
        ));
    }
    let shape = match drill.shape.enum_value() {
        Ok(board_types::DrillShape::DS_CIRCLE) => ObservedDrillShape::Circle,
        Ok(board_types::DrillShape::DS_OBLONG) => ObservedDrillShape::Oblong,
        Ok(board_types::DrillShape::DS_UNDEFINED) => ObservedDrillShape::Undefined,
        _ => ObservedDrillShape::Unknown,
    };
    Ok(Some(ObservedPadDrill {
        shape,
        size_x_nm: diameter.x_nm,
        size_y_nm: diameter.y_nm,
    }))
}

fn board_graphics_from_response(
    response: &editor_commands::GetItemsResponse,
) -> Result<Vec<ObservedBoardGraphicSegment>, IpcError> {
    if response.status.enum_value_or_default() != base_types::ItemRequestStatus::IRS_OK {
        return Err(IpcError::Status(
            ApiStatusCode::AS_BAD_REQUEST,
            format!(
                "get items status: {:?}",
                response.status.enum_value_or_default()
            ),
        ));
    }
    response
        .items
        .iter()
        .filter_map(observed_board_graphic)
        .collect()
}

fn observed_board_graphic(item: &Any) -> Option<Result<ObservedBoardGraphicSegment, IpcError>> {
    const BOARD_GRAPHIC_SHAPE_TYPE_URL: &str =
        "type.googleapis.com/kiapi.board.types.BoardGraphicShape";
    if item.type_url != BOARD_GRAPHIC_SHAPE_TYPE_URL {
        return Some(Err(IpcError::UnexpectedMessageType {
            expected: BOARD_GRAPHIC_SHAPE_TYPE_URL,
            actual: item.type_url.clone(),
        }));
    }
    let graphic = match board_types::BoardGraphicShape::parse_from_bytes(&item.value) {
        Ok(graphic) => graphic,
        Err(_) => return Some(Err(IpcError::MalformedEnvelope)),
    };
    if graphic.layer.enum_value_or_default() != board_types::BoardLayer::BL_Dwgs_User {
        return None;
    }
    Some(observed_dwgs_user_segment(&graphic))
}

fn observed_dwgs_user_segment(
    graphic: &board_types::BoardGraphicShape,
) -> Result<ObservedBoardGraphicSegment, IpcError> {
    let graphic_id = graphic
        .id
        .as_ref()
        .map(|id| id.value.clone())
        .filter(|id| !id.is_empty())
        .ok_or(IpcError::InvalidObservedBoardGraphic(
            "board graphic id must not be empty",
        ))?;
    let shape = graphic
        .shape
        .as_ref()
        .ok_or(IpcError::InvalidObservedBoardGraphic(
            "board graphic shape is required",
        ))?;
    let segment = match shape.geometry.as_ref() {
        Some(base_types::graphic_shape::Geometry::Segment(segment)) => segment,
        Some(_) => {
            return Err(IpcError::InvalidObservedBoardGraphic(
                "Dwgs.User graphic must be a segment",
            ));
        }
        None => {
            return Err(IpcError::InvalidObservedBoardGraphic(
                "board graphic geometry is required",
            ));
        }
    };
    let start = segment
        .start
        .as_ref()
        .ok_or(IpcError::InvalidObservedBoardGraphic(
            "segment start is required",
        ))?;
    let end = segment
        .end
        .as_ref()
        .ok_or(IpcError::InvalidObservedBoardGraphic(
            "segment end is required",
        ))?;
    if start.x_nm == end.x_nm && start.y_nm == end.y_nm {
        return Err(IpcError::InvalidObservedBoardGraphic(
            "segment endpoints must differ",
        ));
    }
    let width_nm = shape
        .attributes
        .as_ref()
        .and_then(|attributes| attributes.stroke.as_ref())
        .and_then(|stroke| stroke.width.as_ref())
        .map(|width| width.value_nm)
        .ok_or(IpcError::InvalidObservedBoardGraphic(
            "segment width is required",
        ))?;
    if width_nm <= 0 {
        return Err(IpcError::InvalidObservedBoardGraphic(
            "segment width must be positive",
        ));
    }
    Ok(ObservedBoardGraphicSegment {
        graphic_id,
        segment: BoardGraphicSegment {
            start_x_nm: start.x_nm,
            start_y_nm: start.y_nm,
            end_x_nm: end.x_nm,
            end_y_nm: end.y_nm,
            width_nm,
            layer: BoardGraphicLayer::DwgsUser,
        },
    })
}

fn board_graphic_segment_item(segment: &BoardGraphicSegment) -> Result<Any, IpcError> {
    if segment.width_nm <= 0 {
        return Err(IpcError::InvalidBoardGraphic(
            "segment width must be positive",
        ));
    }
    if segment.start_x_nm == segment.end_x_nm && segment.start_y_nm == segment.end_y_nm {
        return Err(IpcError::InvalidBoardGraphic(
            "segment endpoints must differ",
        ));
    }
    let layer = match segment.layer {
        BoardGraphicLayer::DwgsUser => board_types::BoardLayer::BL_Dwgs_User,
        BoardGraphicLayer::FSilkS => board_types::BoardLayer::BL_F_SilkS,
    };
    let graphic = board_types::BoardGraphicShape {
        shape: MessageField::some(base_types::GraphicShape {
            attributes: MessageField::some(base_types::GraphicAttributes {
                stroke: MessageField::some(base_types::StrokeAttributes {
                    width: MessageField::some(base_types::Distance {
                        value_nm: segment.width_nm,
                        ..base_types::Distance::default()
                    }),
                    ..base_types::StrokeAttributes::default()
                }),
                ..base_types::GraphicAttributes::default()
            }),
            geometry: Some(base_types::graphic_shape::Geometry::Segment(
                base_types::GraphicSegmentAttributes {
                    start: MessageField::some(base_types::Vector2 {
                        x_nm: segment.start_x_nm,
                        y_nm: segment.start_y_nm,
                        ..base_types::Vector2::default()
                    }),
                    end: MessageField::some(base_types::Vector2 {
                        x_nm: segment.end_x_nm,
                        y_nm: segment.end_y_nm,
                        ..base_types::Vector2::default()
                    }),
                    ..base_types::GraphicSegmentAttributes::default()
                },
            )),
            ..base_types::GraphicShape::default()
        }),
        layer: protobuf::EnumOrUnknown::new(layer),
        ..board_types::BoardGraphicShape::default()
    };
    Ok(Any {
        type_url: "type.googleapis.com/kiapi.board.types.BoardGraphicShape".to_owned(),
        value: graphic
            .write_to_bytes()
            .map_err(|_| IpcError::MalformedEnvelope)?,
        ..Any::default()
    })
}

fn get_nets_request(
    config: &ClientConfig,
    token: Option<&InstanceToken>,
    board_filename: &str,
) -> ApiRequest {
    let mut board = base_types::DocumentSpecifier {
        type_: protobuf::EnumOrUnknown::new(base_types::DocumentType::DOCTYPE_PCB),
        ..base_types::DocumentSpecifier::default()
    };
    board.set_board_filename(board_filename.to_owned());
    let command = board_commands::GetNets {
        board: MessageField::some(board),
        ..board_commands::GetNets::default()
    };
    let any = Any {
        type_url: GET_NETS_TYPE_URL.to_owned(),
        value: command.write_to_bytes().expect("GetNets serializes"),
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

fn get_open_documents_request(
    config: &ClientConfig,
    token: Option<&InstanceToken>,
    query: DocumentQuery,
) -> ApiRequest {
    let document_type = match query {
        DocumentQuery::Schematic => base_types::DocumentType::DOCTYPE_SCHEMATIC,
        DocumentQuery::Pcb => base_types::DocumentType::DOCTYPE_PCB,
    };
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

    fn create_items_response(
        token: &str,
        statuses: &[(editor_commands::ItemStatusCode, &str)],
    ) -> ApiResponse {
        let response = editor_commands::CreateItemsResponse {
            status: protobuf::EnumOrUnknown::new(base_types::ItemRequestStatus::IRS_OK),
            created_items: statuses
                .iter()
                .map(|(code, message)| editor_commands::ItemCreationResult {
                    status: MessageField::some(editor_commands::ItemStatus {
                        code: protobuf::EnumOrUnknown::new(*code),
                        error_message: (*message).to_owned(),
                        ..editor_commands::ItemStatus::default()
                    }),
                    ..editor_commands::ItemCreationResult::default()
                })
                .collect(),
            ..editor_commands::CreateItemsResponse::default()
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
                type_url: CREATE_ITEMS_RESPONSE_TYPE_URL.to_owned(),
                value: response.write_to_bytes().unwrap(),
                ..Any::default()
            }),
            ..ApiResponse::default()
        }
    }

    fn get_items_response(
        token: &str,
        status: base_types::ItemRequestStatus,
        items: Vec<Any>,
    ) -> ApiResponse {
        let response = editor_commands::GetItemsResponse {
            status: protobuf::EnumOrUnknown::new(status),
            items,
            ..editor_commands::GetItemsResponse::default()
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
                type_url: GET_ITEMS_RESPONSE_TYPE_URL.to_owned(),
                value: response.write_to_bytes().unwrap(),
                ..Any::default()
            }),
            ..ApiResponse::default()
        }
    }

    fn field(value: &str) -> board_types::Field {
        board_types::Field {
            text: MessageField::some(board_types::BoardText {
                text: MessageField::some(base_types::Text {
                    text: value.to_owned(),
                    ..base_types::Text::default()
                }),
                ..board_types::BoardText::default()
            }),
            ..board_types::Field::default()
        }
    }

    fn footprint_item(id: &str) -> Any {
        let footprint = board_types::FootprintInstance {
            id: MessageField::some(base_types::KIID {
                value: id.to_owned(),
                ..base_types::KIID::default()
            }),
            position: MessageField::some(base_types::Vector2 {
                x_nm: 10,
                y_nm: -20,
                ..base_types::Vector2::default()
            }),
            orientation: MessageField::some(base_types::Angle {
                value_degrees: -1.2346,
                ..base_types::Angle::default()
            }),
            layer: protobuf::EnumOrUnknown::new(board_types::BoardLayer::BL_F_Cu),
            locked: protobuf::EnumOrUnknown::new(base_types::LockedState::LS_LOCKED),
            definition: MessageField::some(board_types::Footprint {
                id: MessageField::some(base_types::LibraryIdentifier {
                    library_nickname: "Resistor_SMD".to_owned(),
                    entry_name: "R_0603".to_owned(),
                    ..base_types::LibraryIdentifier::default()
                }),
                ..board_types::Footprint::default()
            }),
            reference_field: MessageField::some(field("R1")),
            value_field: MessageField::some(field("10k")),
            symbol_path: MessageField::some(base_types::SheetPath {
                path: vec![
                    base_types::KIID {
                        value: "root".to_owned(),
                        ..base_types::KIID::default()
                    },
                    base_types::KIID {
                        value: "child".to_owned(),
                        ..base_types::KIID::default()
                    },
                ],
                ..base_types::SheetPath::default()
            }),
            ..board_types::FootprintInstance::default()
        };
        Any {
            type_url: "type.googleapis.com/kiapi.board.types.FootprintInstance".to_owned(),
            value: footprint.write_to_bytes().unwrap(),
            ..Any::default()
        }
    }

    fn footprint_client(response: ApiResponse) -> KiCadClient<FakeTransport> {
        let config = ClientConfig::new("ipc:///tmp/kicad/api.sock", "test-client").unwrap();
        KiCadClient::new(
            FakeTransport {
                responses: vec![response],
                requests: Vec::new(),
            },
            config,
        )
    }

    fn pad_item(id: &str, number: &str) -> Any {
        let pad = board_types::Pad {
            id: MessageField::some(base_types::KIID {
                value: id.to_owned(),
                ..base_types::KIID::default()
            }),
            locked: protobuf::EnumOrUnknown::new(base_types::LockedState::LS_LOCKED),
            number: number.to_owned(),
            net: MessageField::some(board_types::Net {
                name: "GND".to_owned(),
                ..board_types::Net::default()
            }),
            type_: protobuf::EnumOrUnknown::new(board_types::PadType::PT_PTH),
            pad_stack: MessageField::some(board_types::PadStack {
                type_: protobuf::EnumOrUnknown::new(board_types::PadStackType::PST_NORMAL),
                layers: vec![
                    protobuf::EnumOrUnknown::new(board_types::BoardLayer::BL_F_Mask),
                    protobuf::EnumOrUnknown::new(board_types::BoardLayer::BL_B_Cu),
                    protobuf::EnumOrUnknown::new(board_types::BoardLayer::BL_F_Cu),
                    protobuf::EnumOrUnknown::new(board_types::BoardLayer::BL_F_Paste),
                    protobuf::EnumOrUnknown::new(board_types::BoardLayer::BL_In2_Cu),
                ],
                drill: MessageField::some(board_types::DrillProperties {
                    diameter: MessageField::some(base_types::Vector2 {
                        x_nm: 300_000,
                        y_nm: 400_000,
                        ..base_types::Vector2::default()
                    }),
                    shape: protobuf::EnumOrUnknown::new(board_types::DrillShape::DS_OBLONG),
                    ..board_types::DrillProperties::default()
                }),
                copper_layers: vec![
                    board_types::PadStackLayer {
                        layer: protobuf::EnumOrUnknown::new(board_types::BoardLayer::BL_B_Cu),
                        shape: protobuf::EnumOrUnknown::new(board_types::PadStackShape::PSS_OVAL),
                        size: MessageField::some(base_types::Vector2 {
                            x_nm: 900_000,
                            y_nm: 700_000,
                            ..base_types::Vector2::default()
                        }),
                        offset: MessageField::some(base_types::Vector2 {
                            x_nm: 2,
                            y_nm: -3,
                            ..base_types::Vector2::default()
                        }),
                        ..board_types::PadStackLayer::default()
                    },
                    board_types::PadStackLayer {
                        layer: protobuf::EnumOrUnknown::new(board_types::BoardLayer::BL_F_Cu),
                        shape: protobuf::EnumOrUnknown::new(
                            board_types::PadStackShape::PSS_ROUNDRECT,
                        ),
                        corner_rounding_ratio: 0.25,
                        size: MessageField::some(base_types::Vector2 {
                            x_nm: 800_000,
                            y_nm: 800_000,
                            ..base_types::Vector2::default()
                        }),
                        ..board_types::PadStackLayer::default()
                    },
                ],
                angle: MessageField::some(base_types::Angle {
                    value_degrees: -1.2346,
                    ..base_types::Angle::default()
                }),
                ..board_types::PadStack::default()
            }),
            position: MessageField::some(base_types::Vector2 {
                x_nm: 10,
                y_nm: -20,
                ..base_types::Vector2::default()
            }),
            ..board_types::Pad::default()
        };
        Any {
            type_url: "type.googleapis.com/kiapi.board.types.Pad".to_owned(),
            value: pad.write_to_bytes().unwrap(),
            ..Any::default()
        }
    }

    fn pad_client(response: ApiResponse) -> KiCadClient<FakeTransport> {
        footprint_client(response)
    }

    fn update_pad(item: &mut Any, update: impl FnOnce(&mut board_types::Pad)) {
        let mut pad = board_types::Pad::parse_from_bytes(&item.value).unwrap();
        update(&mut pad);
        item.value = pad.write_to_bytes().unwrap();
    }

    fn observed_segment_item(layer: board_types::BoardLayer) -> Any {
        let shape = base_types::GraphicShape {
            attributes: MessageField::some(base_types::GraphicAttributes {
                stroke: MessageField::some(base_types::StrokeAttributes {
                    width: MessageField::some(base_types::Distance {
                        value_nm: 100_000,
                        ..base_types::Distance::default()
                    }),
                    ..base_types::StrokeAttributes::default()
                }),
                ..base_types::GraphicAttributes::default()
            }),
            geometry: Some(base_types::graphic_shape::Geometry::Segment(
                base_types::GraphicSegmentAttributes {
                    start: MessageField::some(base_types::Vector2 {
                        x_nm: 1,
                        y_nm: 2,
                        ..base_types::Vector2::default()
                    }),
                    end: MessageField::some(base_types::Vector2 {
                        x_nm: 3,
                        y_nm: 4,
                        ..base_types::Vector2::default()
                    }),
                    ..base_types::GraphicSegmentAttributes::default()
                },
            )),
            ..base_types::GraphicShape::default()
        };
        let graphic = board_types::BoardGraphicShape {
            shape: MessageField::some(shape),
            layer: protobuf::EnumOrUnknown::new(layer),
            id: MessageField::some(base_types::KIID {
                value: "graphic-1".to_owned(),
                ..base_types::KIID::default()
            }),
            ..board_types::BoardGraphicShape::default()
        };
        Any {
            type_url: "type.googleapis.com/kiapi.board.types.BoardGraphicShape".to_owned(),
            value: graphic.write_to_bytes().unwrap(),
            ..Any::default()
        }
    }

    fn graphics_client(response: ApiResponse) -> KiCadClient<FakeTransport> {
        let config = ClientConfig::new("ipc:///tmp/kicad/api.sock", "test-client").unwrap();
        KiCadClient::new(
            FakeTransport {
                responses: vec![response],
                requests: Vec::new(),
            },
            config,
        )
    }

    fn segment() -> BoardGraphicSegment {
        BoardGraphicSegment {
            start_x_nm: 1,
            start_y_nm: 2,
            end_x_nm: 3,
            end_y_nm: 4,
            width_nm: 100_000,
            layer: BoardGraphicLayer::DwgsUser,
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
    fn get_footprint_instances_maps_fields_sorts_and_uses_document_container() {
        let mut client = footprint_client(get_items_response(
            "token",
            base_types::ItemRequestStatus::IRS_OK,
            vec![footprint_item("b"), footprint_item("a")],
        ));
        let footprints = client.get_footprint_instances("main.kicad_pcb").unwrap();
        assert_eq!(footprints[0].source_uuid, "a");
        assert_eq!(footprints[0].position_x_nm, 10);
        assert_eq!(footprints[0].position_y_nm, -20);
        assert_eq!(footprints[0].orientation_mdeg, -1235);
        assert_eq!(footprints[0].side, FootprintSide::Front);
        assert_eq!(footprints[0].lock_state, ObservedLockState::Locked);
        assert_eq!(footprints[0].library_identifier.nickname, "Resistor_SMD");
        assert_eq!(footprints[0].library_identifier.entry_name, "R_0603");
        assert_eq!(footprints[0].reference, "R1");
        assert_eq!(footprints[0].value, "10k");
        assert_eq!(footprints[0].symbol_sheet_path, ["root", "child"]);
        let command = editor_commands::GetItems::parse_from_bytes(
            &client.transport.requests[0].message.as_ref().unwrap().value,
        )
        .unwrap();
        assert_eq!(command.types.len(), 1);
        assert_eq!(
            command.types[0].enum_value_or_default(),
            enums::KiCadObjectType::KOT_PCB_FOOTPRINT
        );
        let header = command.header.as_ref().unwrap();
        assert!(header.container.is_none());
        assert_eq!(
            header.document.as_ref().unwrap().board_filename(),
            "main.kicad_pcb"
        );
    }

    #[test]
    fn get_footprint_instances_accepts_empty_and_unknown_lock() {
        let mut empty = footprint_client(get_items_response(
            "token",
            base_types::ItemRequestStatus::IRS_OK,
            Vec::new(),
        ));
        assert!(
            empty
                .get_footprint_instances("main.kicad_pcb")
                .unwrap()
                .is_empty()
        );
        let mut item = footprint_item("a");
        let mut footprint = board_types::FootprintInstance::parse_from_bytes(&item.value).unwrap();
        footprint.locked = protobuf::EnumOrUnknown::from_i32(999);
        footprint.symbol_path = MessageField::none();
        item.value = footprint.write_to_bytes().unwrap();
        let mut client = footprint_client(get_items_response(
            "token",
            base_types::ItemRequestStatus::IRS_OK,
            vec![item],
        ));
        let observed = client.get_footprint_instances("main.kicad_pcb").unwrap();
        assert_eq!(observed[0].lock_state, ObservedLockState::Unknown);
        assert!(observed[0].symbol_sheet_path.is_empty());
    }

    #[test]
    fn get_footprint_instances_rejects_status_type_and_duplicates() {
        let mut status = footprint_client(get_items_response(
            "token",
            base_types::ItemRequestStatus::IRS_DOCUMENT_NOT_FOUND,
            Vec::new(),
        ));
        assert_eq!(
            status
                .get_footprint_instances("main.kicad_pcb")
                .unwrap_err()
                .code(),
            "kicad_status_error"
        );
        let mut wrong = footprint_item("a");
        wrong.type_url = "type.googleapis.com/kiapi.board.types.Pad".to_owned();
        let mut client = footprint_client(get_items_response(
            "token",
            base_types::ItemRequestStatus::IRS_OK,
            vec![wrong],
        ));
        assert_eq!(
            client
                .get_footprint_instances("main.kicad_pcb")
                .unwrap_err()
                .code(),
            "protocol_decode_failed"
        );
        let mut duplicate = footprint_client(get_items_response(
            "token",
            base_types::ItemRequestStatus::IRS_OK,
            vec![footprint_item("a"), footprint_item("a")],
        ));
        assert_eq!(
            duplicate
                .get_footprint_instances("main.kicad_pcb")
                .unwrap_err()
                .code(),
            "invalid_observed_footprint"
        );
        assert!(duplicate.instance_token().is_none());
    }

    #[test]
    fn get_footprint_instances_rejects_missing_fields_and_invalid_side() {
        let mut item = footprint_item("a");
        let mut footprint = board_types::FootprintInstance::parse_from_bytes(&item.value).unwrap();
        footprint.position = MessageField::none();
        item.value = footprint.write_to_bytes().unwrap();
        let mut missing = footprint_client(get_items_response(
            "token",
            base_types::ItemRequestStatus::IRS_OK,
            vec![item],
        ));
        assert_eq!(
            missing
                .get_footprint_instances("main.kicad_pcb")
                .unwrap_err()
                .code(),
            "invalid_observed_footprint"
        );
        let mut item = footprint_item("a");
        let mut footprint = board_types::FootprintInstance::parse_from_bytes(&item.value).unwrap();
        footprint.layer = protobuf::EnumOrUnknown::new(board_types::BoardLayer::BL_F_SilkS);
        item.value = footprint.write_to_bytes().unwrap();
        let mut side = footprint_client(get_items_response(
            "token",
            base_types::ItemRequestStatus::IRS_OK,
            vec![item],
        ));
        assert_eq!(
            side.get_footprint_instances("main.kicad_pcb")
                .unwrap_err()
                .code(),
            "invalid_observed_footprint"
        );
    }

    #[test]
    fn get_footprint_instances_rejects_missing_identity_definition_and_fields() {
        let mut invalid = Vec::new();
        for index in 0..6 {
            let mut item = footprint_item("a");
            let mut footprint =
                board_types::FootprintInstance::parse_from_bytes(&item.value).unwrap();
            match index {
                0 => footprint.id.as_mut().unwrap().value.clear(),
                1 => footprint.definition = MessageField::none(),
                2 => footprint.definition.as_mut().unwrap().id = MessageField::none(),
                3 => footprint
                    .definition
                    .as_mut()
                    .unwrap()
                    .id
                    .as_mut()
                    .unwrap()
                    .entry_name
                    .clear(),
                4 => footprint.reference_field.as_mut().unwrap().text = MessageField::none(),
                5 => footprint.value_field = MessageField::none(),
                _ => unreachable!(),
            }
            item.value = footprint.write_to_bytes().unwrap();
            invalid.push(item);
        }
        for item in invalid {
            let mut client = footprint_client(get_items_response(
                "token",
                base_types::ItemRequestStatus::IRS_OK,
                vec![item],
            ));
            assert_eq!(
                client
                    .get_footprint_instances("main.kicad_pcb")
                    .unwrap_err()
                    .code(),
                "invalid_observed_footprint"
            );
        }
    }

    #[test]
    fn get_footprint_instances_rejects_invalid_orientation_and_sheet_path() {
        let mut item = footprint_item("a");
        let mut footprint = board_types::FootprintInstance::parse_from_bytes(&item.value).unwrap();
        footprint.orientation.as_mut().unwrap().value_degrees = f64::NAN;
        item.value = footprint.write_to_bytes().unwrap();
        let mut orientation = footprint_client(get_items_response(
            "token",
            base_types::ItemRequestStatus::IRS_OK,
            vec![item],
        ));
        assert_eq!(
            orientation
                .get_footprint_instances("main.kicad_pcb")
                .unwrap_err()
                .code(),
            "invalid_observed_footprint"
        );
        let mut item = footprint_item("a");
        let mut footprint = board_types::FootprintInstance::parse_from_bytes(&item.value).unwrap();
        footprint.symbol_path.as_mut().unwrap().path[0]
            .value
            .clear();
        item.value = footprint.write_to_bytes().unwrap();
        let mut path = footprint_client(get_items_response(
            "token",
            base_types::ItemRequestStatus::IRS_OK,
            vec![item],
        ));
        assert_eq!(
            path.get_footprint_instances("main.kicad_pcb")
                .unwrap_err()
                .code(),
            "invalid_observed_footprint"
        );
    }

    #[test]
    fn get_board_graphics_decodes_dwgs_user_segments_and_request() {
        let mut client = graphics_client(get_items_response(
            "token",
            base_types::ItemRequestStatus::IRS_OK,
            vec![observed_segment_item(board_types::BoardLayer::BL_Dwgs_User)],
        ));
        let graphics = client.get_board_graphics("main.kicad_pcb").unwrap();
        assert_eq!(graphics[0].graphic_id, "graphic-1");
        assert_eq!(graphics[0].segment, segment());
        let command = editor_commands::GetItems::parse_from_bytes(
            &client.transport.requests[0].message.as_ref().unwrap().value,
        )
        .unwrap();
        assert_eq!(
            command.types[0].enum_value_or_default(),
            enums::KiCadObjectType::KOT_PCB_SHAPE
        );
    }

    #[test]
    fn get_board_graphics_ignores_other_layers() {
        let mut client = graphics_client(get_items_response(
            "token",
            base_types::ItemRequestStatus::IRS_OK,
            vec![observed_segment_item(board_types::BoardLayer::BL_F_SilkS)],
        ));
        assert!(
            client
                .get_board_graphics("main.kicad_pcb")
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn get_board_graphics_rejects_non_segment() {
        let mut item = observed_segment_item(board_types::BoardLayer::BL_Dwgs_User);
        let mut graphic = board_types::BoardGraphicShape::parse_from_bytes(&item.value).unwrap();
        graphic.shape.as_mut().unwrap().geometry = None;
        item.value = graphic.write_to_bytes().unwrap();
        let mut client = graphics_client(get_items_response(
            "token",
            base_types::ItemRequestStatus::IRS_OK,
            vec![item],
        ));
        assert_eq!(
            client
                .get_board_graphics("main.kicad_pcb")
                .unwrap_err()
                .code(),
            "invalid_observed_board_graphic"
        );
    }

    #[test]
    fn get_board_graphics_rejects_missing_required_fields() {
        let mut item = observed_segment_item(board_types::BoardLayer::BL_Dwgs_User);
        let mut graphic = board_types::BoardGraphicShape::parse_from_bytes(&item.value).unwrap();
        graphic.id = MessageField::none();
        item.value = graphic.write_to_bytes().unwrap();
        let mut client = graphics_client(get_items_response(
            "token",
            base_types::ItemRequestStatus::IRS_OK,
            vec![item],
        ));
        assert_eq!(
            client
                .get_board_graphics("main.kicad_pcb")
                .unwrap_err()
                .code(),
            "invalid_observed_board_graphic"
        );
    }

    #[test]
    fn get_board_graphics_rejects_response_status() {
        let mut client = graphics_client(get_items_response(
            "token",
            base_types::ItemRequestStatus::IRS_DOCUMENT_NOT_FOUND,
            Vec::new(),
        ));
        assert_eq!(
            client.get_board_graphics("main.kicad_pcb").unwrap_err(),
            IpcError::Status(
                ApiStatusCode::AS_BAD_REQUEST,
                "get items status: IRS_DOCUMENT_NOT_FOUND".to_owned()
            )
        );
    }

    #[test]
    fn client_captures_and_reuses_instance_token() {
        let config = ClientConfig::new("ipc:///tmp/kicad/api.sock", "test-client").unwrap();
        let fake = FakeTransport {
            responses: vec![version_response("token", 10), version_response("token", 10)],
            requests: Vec::new(),
        };
        let mut client = KiCadClient::new(fake, config);
        assert!(client.instance_token().is_none());
        assert_eq!(client.get_version().unwrap().major, 10);
        assert_eq!(client.instance_token().unwrap().as_str(), "token");
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
    fn create_board_graphics_rejects_result_count_mismatch() {
        let config = ClientConfig::new("ipc:///tmp/kicad/api.sock", "test-client").unwrap();
        let fake = FakeTransport {
            responses: vec![create_items_response(
                "token",
                &[(editor_commands::ItemStatusCode::ISC_OK, "")],
            )],
            requests: Vec::new(),
        };
        let mut client = KiCadClient::new(fake, config);
        let error = client
            .create_board_graphics("main.kicad_pcb", &[segment(), segment()])
            .unwrap_err();
        assert_eq!(
            error,
            IpcError::CreateItemsCountMismatch {
                expected: 2,
                actual: 1
            }
        );
        assert_eq!(error.code(), "create_items_count_mismatch");
    }

    #[test]
    fn create_board_graphics_rejects_per_item_failure() {
        let config = ClientConfig::new("ipc:///tmp/kicad/api.sock", "test-client").unwrap();
        let fake = FakeTransport {
            responses: vec![create_items_response(
                "token",
                &[
                    (editor_commands::ItemStatusCode::ISC_OK, ""),
                    (
                        editor_commands::ItemStatusCode::ISC_INVALID_DATA,
                        "invalid segment",
                    ),
                ],
            )],
            requests: Vec::new(),
        };
        let mut client = KiCadClient::new(fake, config);
        let error = client
            .create_board_graphics("main.kicad_pcb", &[segment(), segment()])
            .unwrap_err();
        assert_eq!(
            error,
            IpcError::CreateItemFailed {
                index: 1,
                status: editor_commands::ItemStatusCode::ISC_INVALID_DATA as i32,
                message: "invalid segment".to_owned(),
            }
        );
        assert_eq!(error.code(), "create_item_failed");
        assert_eq!(
            error.to_string(),
            "create_item_failed: item 1 status 7: invalid segment"
        );
    }

    #[test]
    fn create_board_graphics_rejects_non_positive_width_before_request() {
        let mut invalid = segment();
        invalid.width_nm = 0;
        let error = board_graphic_segment_item(&invalid).unwrap_err();
        assert_eq!(error.code(), "invalid_board_graphic");
        assert_eq!(
            error.to_string(),
            "invalid_board_graphic: segment width must be positive"
        );
    }

    #[test]
    fn create_board_graphics_rejects_zero_length_before_request() {
        let mut invalid = segment();
        invalid.end_x_nm = invalid.start_x_nm;
        invalid.end_y_nm = invalid.start_y_nm;
        let error = board_graphic_segment_item(&invalid).unwrap_err();
        assert_eq!(error.code(), "invalid_board_graphic");
        assert_eq!(
            error.to_string(),
            "invalid_board_graphic: segment endpoints must differ"
        );
    }

    fn pcb_item_any(type_url: &str, value: Vec<u8>) -> Any {
        Any {
            type_url: type_url.to_owned(),
            value,
            ..Any::default()
        }
    }

    #[test]
    fn pcb_item_probe_encodes_all_fixed_types_and_decodes_each_payload() {
        let items = vec![
            pcb_item_any(
                "type.googleapis.com/kiapi.board.types.FootprintInstance",
                board_types::FootprintInstance::default()
                    .write_to_bytes()
                    .unwrap(),
            ),
            pcb_item_any(
                "type.googleapis.com/kiapi.board.types.Pad",
                board_types::Pad::default().write_to_bytes().unwrap(),
            ),
            pcb_item_any(
                "type.googleapis.com/kiapi.board.types.Track",
                board_types::Track::default().write_to_bytes().unwrap(),
            ),
            pcb_item_any(
                "type.googleapis.com/kiapi.board.types.Via",
                board_types::Via::default().write_to_bytes().unwrap(),
            ),
            pcb_item_any(
                "type.googleapis.com/kiapi.board.types.Zone",
                board_types::Zone::default().write_to_bytes().unwrap(),
            ),
        ];
        let mut client = graphics_client(get_items_response(
            "next-token",
            base_types::ItemRequestStatus::IRS_OK,
            items,
        ));
        let diagnostics = client
            .get_pcb_item_diagnostics("main.kicad_pcb", &PcbItemKind::all())
            .unwrap();
        assert_eq!(diagnostics.len(), 5);
        assert_eq!(diagnostics[0].decoded_kind, "FootprintInstance");
        assert_eq!(diagnostics[1].decoded_kind, "Pad");
        assert_eq!(diagnostics[2].decoded_kind, "Track");
        assert_eq!(diagnostics[3].decoded_kind, "Via");
        assert_eq!(diagnostics[4].decoded_kind, "Zone");
        assert_eq!(client.token.as_ref().unwrap().as_str(), "next-token");

        let command = editor_commands::GetItems::parse_from_bytes(
            &client.transport.requests[0].message.as_ref().unwrap().value,
        )
        .unwrap();
        assert_eq!(command.types.len(), 5);
        assert_eq!(
            command
                .types
                .iter()
                .map(|kind| kind.enum_value_or_default())
                .collect::<Vec<_>>(),
            vec![
                enums::KiCadObjectType::KOT_PCB_FOOTPRINT,
                enums::KiCadObjectType::KOT_PCB_PAD,
                enums::KiCadObjectType::KOT_PCB_TRACE,
                enums::KiCadObjectType::KOT_PCB_VIA,
                enums::KiCadObjectType::KOT_PCB_ZONE,
            ]
        );
    }

    #[test]
    fn pcb_item_probe_empty_kinds_do_not_send_request() {
        let config = ClientConfig::new("ipc:///tmp/kicad/api.sock", "test-client").unwrap();
        let fake = FakeTransport {
            responses: Vec::new(),
            requests: Vec::new(),
        };
        let mut client = KiCadClient::new(fake, config);
        assert!(
            client
                .get_pcb_item_diagnostics("main.kicad_pcb", &[])
                .unwrap()
                .is_empty()
        );
        assert!(client.transport.requests.is_empty());
    }

    #[test]
    fn pcb_item_probe_accepts_arc_as_trace() {
        let mut client = graphics_client(get_items_response(
            "token",
            base_types::ItemRequestStatus::IRS_OK,
            vec![pcb_item_any(
                "type.googleapis.com/kiapi.board.types.Arc",
                board_types::Arc::default().write_to_bytes().unwrap(),
            )],
        ));
        let diagnostics = client
            .get_pcb_item_diagnostics("main.kicad_pcb", &[PcbItemKind::Trace])
            .unwrap();
        assert_eq!(diagnostics.len(), 1);
        assert_eq!(diagnostics[0].requested_kind, PcbItemKind::Trace);
        assert_eq!(diagnostics[0].decoded_kind, "Arc");
    }

    #[test]
    fn pcb_item_probe_rejects_non_ok_response_status() {
        let mut response =
            get_items_response("token", base_types::ItemRequestStatus::IRS_OK, Vec::new());
        response.status.as_mut().unwrap().status =
            protobuf::EnumOrUnknown::new(ApiStatusCode::AS_BUSY);
        let mut client = graphics_client(response);
        let error = client
            .get_pcb_item_diagnostics("main.kicad_pcb", &[PcbItemKind::Pad])
            .unwrap_err();
        assert_eq!(error.code(), "kicad_busy");
    }

    #[test]
    fn pcb_item_probe_rejects_unknown_payload_type_url() {
        let mut client = graphics_client(get_items_response(
            "token",
            base_types::ItemRequestStatus::IRS_OK,
            vec![pcb_item_any(
                "type.googleapis.com/kiapi.board.types.Unknown",
                Vec::new(),
            )],
        ));
        let error = client
            .get_pcb_item_diagnostics("main.kicad_pcb", &[PcbItemKind::Pad])
            .unwrap_err();
        assert_eq!(
            error,
            IpcError::UnknownPcbItemType(
                "type.googleapis.com/kiapi.board.types.Unknown".to_owned()
            )
        );
        assert_eq!(
            error.to_string(),
            "unknown_pcb_item_type: type.googleapis.com/kiapi.board.types.Unknown"
        );
    }

    #[test]
    fn get_footprint_pads_maps_and_sorts_container_bound_observations() {
        let mut client = pad_client(get_items_response(
            "token",
            base_types::ItemRequestStatus::IRS_OK,
            vec![pad_item("b", "2"), pad_item("a", "1")],
        ));
        let pads = client
            .get_footprint_pads("main.kicad_pcb", "footprint-1")
            .unwrap();
        assert_eq!(pads[0].source_uuid, "a");
        assert_eq!(pads[0].footprint_source_uuid, "footprint-1");
        assert_eq!((pads[0].position_x_nm, pads[0].position_y_nm), (10, -20));
        assert_eq!(pads[0].stack_type, ObservedPadStackType::Normal);
        assert_eq!(pads[0].orientation_mdeg, -1235);
        assert_eq!(pads[0].kind, ObservedPadKind::PlatedThroughHole);
        assert_eq!(pads[0].lock_state, ObservedLockState::Locked);
        assert_eq!(pads[0].net_name.as_deref(), Some("GND"));
        assert!(pads[0].layers.contains(&ObservedBoardLayer::InnerCopper(2)));
        assert!(pads[0].layers.contains(&ObservedBoardLayer::FrontMask));
        assert!(pads[0].layers.contains(&ObservedBoardLayer::FrontPaste));
        assert_eq!(pads[0].copper_shapes[0].offset_x_nm, 0);
        assert_eq!(pads[0].copper_shapes[0].corner_rounding_ratio, 0.25);
        assert_eq!(pads[0].copper_shapes[1].offset_y_nm, -3);
        assert_eq!(pads[0].drill.unwrap().shape, ObservedDrillShape::Oblong);
        let command = editor_commands::GetItems::parse_from_bytes(
            &client.transport.requests[0].message.as_ref().unwrap().value,
        )
        .unwrap();
        assert_eq!(
            command.types,
            [protobuf::EnumOrUnknown::new(
                enums::KiCadObjectType::KOT_PCB_PAD
            )]
        );
        let header = command.header.as_ref().unwrap();
        assert_eq!(header.container.as_ref().unwrap().value, "footprint-1");
        assert_eq!(
            header.document.as_ref().unwrap().board_filename(),
            "main.kicad_pcb"
        );
        assert_eq!(client.token.as_ref().unwrap().as_str(), "token");
    }

    #[test]
    fn get_footprint_pads_accepts_empty_numbers_results_and_unknown_enums() {
        let mut empty = pad_client(get_items_response(
            "token",
            base_types::ItemRequestStatus::IRS_OK,
            Vec::new(),
        ));
        assert!(
            empty
                .get_footprint_pads("main.kicad_pcb", "footprint-1")
                .unwrap()
                .is_empty()
        );
        let mut first = pad_item("a", "");
        update_pad(&mut first, |pad| {
            pad.type_ = protobuf::EnumOrUnknown::from_i32(999);
            pad.locked = protobuf::EnumOrUnknown::from_i32(999);
            let stack = pad.pad_stack.as_mut().unwrap();
            stack.type_ = protobuf::EnumOrUnknown::from_i32(999);
            stack.copper_layers[0].shape = protobuf::EnumOrUnknown::from_i32(999);
            stack.drill.as_mut().unwrap().shape = protobuf::EnumOrUnknown::from_i32(999);
            pad.net = MessageField::none();
        });
        let mut no_drill = pad_item("b", "");
        update_pad(&mut no_drill, |pad| {
            pad.pad_stack.as_mut().unwrap().drill =
                MessageField::some(board_types::DrillProperties {
                    start_layer: protobuf::EnumOrUnknown::new(board_types::BoardLayer::BL_F_Cu),
                    end_layer: protobuf::EnumOrUnknown::new(board_types::BoardLayer::BL_B_Cu),
                    diameter: MessageField::some(base_types::Vector2::default()),
                    shape: protobuf::EnumOrUnknown::new(board_types::DrillShape::DS_CIRCLE),
                    ..board_types::DrillProperties::default()
                });
        });
        let mut client = pad_client(get_items_response(
            "token",
            base_types::ItemRequestStatus::IRS_OK,
            vec![first, no_drill],
        ));
        let pads = client
            .get_footprint_pads("main.kicad_pcb", "footprint-1")
            .unwrap();
        assert_eq!((pads[0].number.as_str(), pads[1].number.as_str()), ("", ""));
        assert_eq!(pads[0].kind, ObservedPadKind::Unknown);
        assert_eq!(pads[0].lock_state, ObservedLockState::Unknown);
        assert_eq!(pads[0].stack_type, ObservedPadStackType::Unknown);
        assert_eq!(pads[0].copper_shapes[1].shape, ObservedPadShape::Unknown);
        assert_eq!(pads[0].drill.unwrap().shape, ObservedDrillShape::Unknown);
        assert_eq!(pads[0].net_name, None);
        assert_eq!(pads[1].drill, None);
    }

    #[test]
    fn get_footprint_pads_rejects_protocol_status_duplicates_and_preserves_token() {
        let mut empty_parent = pad_client(get_items_response(
            "new",
            base_types::ItemRequestStatus::IRS_OK,
            Vec::new(),
        ));
        assert_eq!(
            empty_parent
                .get_footprint_pads("main.kicad_pcb", "")
                .unwrap_err()
                .code(),
            "invalid_observed_pad"
        );
        assert!(empty_parent.transport.requests.is_empty());
        let mut status = pad_client(get_items_response(
            "new",
            base_types::ItemRequestStatus::IRS_DOCUMENT_NOT_FOUND,
            Vec::new(),
        ));
        assert_eq!(
            status
                .get_footprint_pads("main.kicad_pcb", "f")
                .unwrap_err()
                .code(),
            "kicad_status_error"
        );
        assert!(status.token.is_none());
        let mut wrong = pad_item("a", "1");
        wrong.type_url = "type.googleapis.com/kiapi.board.types.FootprintInstance".to_owned();
        let mut protocol = pad_client(get_items_response(
            "new",
            base_types::ItemRequestStatus::IRS_OK,
            vec![wrong],
        ));
        protocol.token = Some(InstanceToken::new("old").unwrap());
        assert_eq!(
            protocol
                .get_footprint_pads("main.kicad_pcb", "f")
                .unwrap_err()
                .code(),
            "protocol_decode_failed"
        );
        assert_eq!(protocol.token.as_ref().unwrap().as_str(), "old");
        let mismatched = editor_commands::GetItemsResponse {
            header: MessageField::some(base_types::ItemHeader {
                container: MessageField::some(base_types::KIID {
                    value: "other-footprint".to_owned(),
                    ..base_types::KIID::default()
                }),
                ..base_types::ItemHeader::default()
            }),
            status: protobuf::EnumOrUnknown::new(base_types::ItemRequestStatus::IRS_OK),
            ..editor_commands::GetItemsResponse::default()
        };
        assert_eq!(
            footprint_pads_from_response(&mismatched, "footprint-1")
                .unwrap_err()
                .code(),
            "invalid_observed_pad"
        );
        let mut duplicate = pad_client(get_items_response(
            "new",
            base_types::ItemRequestStatus::IRS_OK,
            vec![pad_item("a", "1"), pad_item("a", "2")],
        ));
        assert_eq!(
            duplicate
                .get_footprint_pads("main.kicad_pcb", "f")
                .unwrap_err()
                .code(),
            "invalid_observed_pad"
        );
        assert!(duplicate.token.is_none());
    }

    #[test]
    fn get_footprint_pads_rejects_missing_invalid_and_lossy_geometry() {
        let mut cases = Vec::new();
        let mut missing = pad_item("a", "1");
        update_pad(&mut missing, |pad| pad.position = MessageField::none());
        cases.push(missing);
        let mut size = pad_item("b", "1");
        update_pad(&mut size, |pad| {
            pad.pad_stack.as_mut().unwrap().copper_layers[0]
                .size
                .as_mut()
                .unwrap()
                .x_nm = 0
        });
        cases.push(size);
        let mut layer = pad_item("c", "1");
        update_pad(&mut layer, |pad| {
            pad.pad_stack.as_mut().unwrap().layers[0] =
                protobuf::EnumOrUnknown::new(board_types::BoardLayer::BL_Dwgs_User)
        });
        cases.push(layer);
        let mut duplicate_layer = pad_item("d", "1");
        update_pad(&mut duplicate_layer, |pad| {
            let stack = pad.pad_stack.as_mut().unwrap();
            stack.layers.push(stack.layers[0]);
        });
        cases.push(duplicate_layer);
        let mut custom = pad_item("e", "1");
        update_pad(&mut custom, |pad| {
            pad.pad_stack.as_mut().unwrap().copper_layers[0]
                .custom_shapes
                .push(board_types::BoardGraphicShape::default())
        });
        cases.push(custom);
        let mut custom_shape = pad_item("f", "1");
        update_pad(&mut custom_shape, |pad| {
            pad.pad_stack.as_mut().unwrap().copper_layers[0].shape =
                protobuf::EnumOrUnknown::new(board_types::PadStackShape::PSS_CUSTOM);
        });
        cases.push(custom_shape);
        let mut custom_anchor = pad_item("g", "1");
        update_pad(&mut custom_anchor, |pad| {
            pad.pad_stack.as_mut().unwrap().copper_layers[0].custom_anchor_shape =
                protobuf::EnumOrUnknown::new(board_types::PadStackShape::PSS_OVAL);
        });
        cases.push(custom_anchor);
        let mut invalid_ratio = pad_item("h", "1");
        update_pad(&mut invalid_ratio, |pad| {
            pad.pad_stack.as_mut().unwrap().copper_layers[0].corner_rounding_ratio = f64::NAN;
        });
        cases.push(invalid_ratio);
        let mut contradictory_drill = pad_item("i", "1");
        update_pad(&mut contradictory_drill, |pad| {
            pad.pad_stack.as_mut().unwrap().drill =
                MessageField::some(board_types::DrillProperties {
                    shape: protobuf::EnumOrUnknown::new(board_types::DrillShape::DS_OBLONG),
                    ..board_types::DrillProperties::default()
                });
        });
        cases.push(contradictory_drill);
        let mut secondary = pad_item("j", "1");
        update_pad(&mut secondary, |pad| {
            pad.pad_stack.as_mut().unwrap().secondary_drill =
                MessageField::some(board_types::DrillProperties::default())
        });
        cases.push(secondary);
        for item in cases {
            let mut client = pad_client(get_items_response(
                "new",
                base_types::ItemRequestStatus::IRS_OK,
                vec![item],
            ));
            assert_eq!(
                client
                    .get_footprint_pads("main.kicad_pcb", "footprint-1")
                    .unwrap_err()
                    .code(),
                "invalid_observed_pad"
            );
            assert!(client.token.is_none());
        }
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
            DocumentQuery::Pcb,
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
    fn get_nets_request_encodes_board_document() {
        let config = ClientConfig::new("ipc:///tmp/kicad/api.sock", "test-client").unwrap();
        let request = get_nets_request(
            &config,
            Some(&InstanceToken::new("token").unwrap()),
            "xLock.kicad_pcb",
        );
        let any = request.message.as_ref().unwrap();
        let command = board_commands::GetNets::parse_from_bytes(&any.value).unwrap();
        let board = command.board.as_ref().unwrap();
        assert_eq!(
            board.type_.enum_value_or_default(),
            base_types::DocumentType::DOCTYPE_PCB
        );
        assert_eq!(board.board_filename(), "xLock.kicad_pcb");
        assert_eq!(any.type_url, GET_NETS_TYPE_URL);
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
        let documents = client.get_open_documents(DocumentQuery::Pcb).unwrap();
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
