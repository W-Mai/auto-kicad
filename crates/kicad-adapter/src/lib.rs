use kicad_engine::{MAX_GRAPHIC_BOUNDING_AREA_NM2, MAX_GRAPHIC_SPAN_NM, OperationKind};
use kicad_ipc::{
    BoardGraphicLayer as IpcBoardGraphicLayer, BoardGraphicSegment as IpcBoardGraphicSegment,
    CommitId, InstanceToken, KiCadClient, KiCadTransport, ObservedBoardGraphicSegment,
    OpenDocument, OpenDocumentKind, VersionInfo,
};
use kicad_model::{
    Board, BoardGraphicLayer, BoardGraphicSegment, CapabilityObservation, CapabilityStatus,
    Coordinate, Diagnostic, DiscoveredDocument, DiscoveredDocumentKind, DiscoverySnapshot,
    DocumentSpace, ModelError, ObservedSchematicSymbol, ProjectSnapshot, RevisionDigest, Schematic,
    StableId, Unit,
};
use kicad_schematic::{NetlistDocument, ParsedSchematicProject, join_netlist_connectivity};
use sha2::{Digest, Sha256};
use std::collections::BTreeSet;
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum DocumentObservation {
    Documents(Vec<OpenDocument>),
    Unsupported(String),
    Failed { code: String, message: String },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DiscoveryInput {
    pub endpoint: String,
    pub version: VersionInfo,
    pub schematic: DocumentObservation,
    pub pcb: DocumentObservation,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProjectArtifacts {
    pub project_dir: PathBuf,
    pub project_name: String,
    pub project_file: PathBuf,
    pub schematic_file: Option<PathBuf>,
    pub board_file: Option<PathBuf>,
}

#[derive(Debug)]
pub enum ArtifactError {
    InvalidProjectDirectory,
    ProjectNotFound,
    AmbiguousProject,
    Io(std::io::Error),
    InvalidProjectJson(serde_json::Error),
}

impl std::fmt::Display for ArtifactError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidProjectDirectory => formatter.write_str("invalid project directory"),
            Self::ProjectNotFound => formatter.write_str("KiCad project not found"),
            Self::AmbiguousProject => formatter.write_str("multiple KiCad projects found"),
            Self::Io(error) => write!(formatter, "artifact I/O failed: {error}"),
            Self::InvalidProjectJson(error) => {
                write!(formatter, "invalid KiCad project JSON: {error}")
            }
        }
    }
}

impl std::error::Error for ArtifactError {}

pub fn discover_project_artifacts(project_dir: &Path) -> Result<ProjectArtifacts, ArtifactError> {
    if !project_dir.is_absolute() || !project_dir.is_dir() {
        return Err(ArtifactError::InvalidProjectDirectory);
    }
    let mut projects = fs::read_dir(project_dir)
        .map_err(ArtifactError::Io)?
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| {
            path.extension()
                .is_some_and(|extension| extension == "kicad_pro")
        })
        .collect::<Vec<_>>();
    projects.sort();
    let project_file = match projects.as_slice() {
        [] => return Err(ArtifactError::ProjectNotFound),
        [project] => project.clone(),
        _ => return Err(ArtifactError::AmbiguousProject),
    };
    let project_content = fs::read_to_string(&project_file).map_err(ArtifactError::Io)?;
    serde_json::from_str::<serde_json::Value>(&project_content)
        .map_err(ArtifactError::InvalidProjectJson)?;
    let project_name = project_file
        .file_stem()
        .and_then(|name| name.to_str())
        .ok_or(ArtifactError::ProjectNotFound)?
        .to_owned();
    let schematic = project_dir.join(format!("{project_name}.kicad_sch"));
    let board = project_dir.join(format!("{project_name}.kicad_pcb"));
    let schematic_file = validate_artifact(&schematic, "(kicad_sch")?;
    let board_file = validate_artifact(&board, "(kicad_pcb")?;
    Ok(ProjectArtifacts {
        project_dir: project_dir.to_path_buf(),
        project_name,
        project_file,
        schematic_file,
        board_file,
    })
}

fn validate_artifact(path: &Path, expected_root: &str) -> Result<Option<PathBuf>, ArtifactError> {
    if !path.is_file() {
        return Ok(None);
    }
    let mut file = fs::File::open(path).map_err(ArtifactError::Io)?;
    let mut prefix = [0_u8; 64];
    let length = file.read(&mut prefix).map_err(ArtifactError::Io)?;
    let content = std::str::from_utf8(&prefix[..length]).unwrap_or_default();
    if !content.trim_start().starts_with(expected_root) {
        return Err(ArtifactError::Io(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!("unexpected KiCad artifact root: {}", path.display()),
        )));
    }
    Ok(Some(path.to_path_buf()))
}

pub fn artifact_discovery_snapshot(
    project_dir: &Path,
    endpoint: &str,
    version: VersionInfo,
) -> Result<DiscoverySnapshot, ArtifactError> {
    let artifacts = discover_project_artifacts(project_dir)?;
    let schematic = artifacts.schematic_file.as_ref().map(|file| OpenDocument {
        project_name: Some(artifacts.project_name.clone()),
        project_path: Some(artifacts.project_dir.display().to_string()),
        kind: OpenDocumentKind::Schematic {
            filename: Some(file.display().to_string()),
            sheet_path: None,
            sheet_kiids: Vec::new(),
        },
    });
    let pcb = artifacts.board_file.as_ref().map(|file| OpenDocument {
        project_name: Some(artifacts.project_name.clone()),
        project_path: Some(artifacts.project_dir.display().to_string()),
        kind: OpenDocumentKind::Pcb {
            board_filename: file.display().to_string(),
        },
    });
    let mut snapshot = discovery_snapshot(DiscoveryInput {
        endpoint: endpoint.to_owned(),
        version,
        schematic: schematic.map_or_else(
            || DocumentObservation::Unsupported("schematic artifact not found".to_owned()),
            |document| DocumentObservation::Documents(vec![document]),
        ),
        pcb: pcb.map_or_else(
            || DocumentObservation::Unsupported("PCB artifact not found".to_owned()),
            |document| DocumentObservation::Documents(vec![document]),
        ),
    })
    .map_err(|error| ArtifactError::Io(std::io::Error::other(error.to_string())))?;
    for capability in &mut snapshot.capabilities {
        if matches!(
            capability.id.as_str(),
            "schematic.documents.discover" | "pcb.documents.discover"
        ) {
            capability.detail = Some("offline artifact source".to_owned());
        }
    }
    snapshot.diagnostics.push(Diagnostic {
        code: "offline_artifact_source".to_owned(),
        message: artifacts.project_file.display().to_string(),
    });
    Ok(snapshot.canonical())
}

pub fn discovery_snapshot(input: DiscoveryInput) -> Result<DiscoverySnapshot, ModelError> {
    let mut documents = Vec::new();
    let mut capabilities = vec![CapabilityObservation {
        id: "base.get_version".to_owned(),
        status: CapabilityStatus::Verified,
        detail: None,
    }];
    let mut diagnostics = Vec::new();
    collect_observation(
        "schematic.documents.discover",
        input.schematic,
        &mut documents,
        &mut capabilities,
        &mut diagnostics,
    )?;
    collect_observation(
        "pcb.documents.discover",
        input.pcb,
        &mut documents,
        &mut capabilities,
        &mut diagnostics,
    )?;
    capabilities.push(CapabilityObservation {
        id: "schematic.items.read".to_owned(),
        status: CapabilityStatus::Unknown,
        detail: Some("not probed".to_owned()),
    });
    capabilities.push(CapabilityObservation {
        id: "pcb.items.read".to_owned(),
        status: CapabilityStatus::Unknown,
        detail: Some("not probed".to_owned()),
    });
    diagnostics.push(Diagnostic {
        code: "content_not_observed".to_owned(),
        message: "document discovery does not include schematic or board items".to_owned(),
    });
    Ok(DiscoverySnapshot {
        kicad_version: format!(
            "{}.{}.{}",
            input.version.major, input.version.minor, input.version.patch
        ),
        endpoint: input.endpoint,
        documents,
        capabilities,
        diagnostics,
    }
    .canonical())
}

fn collect_observation(
    capability_id: &str,
    observation: DocumentObservation,
    documents: &mut Vec<DiscoveredDocument>,
    capabilities: &mut Vec<CapabilityObservation>,
    diagnostics: &mut Vec<Diagnostic>,
) -> Result<(), ModelError> {
    match observation {
        DocumentObservation::Documents(observed) => {
            capabilities.push(CapabilityObservation {
                id: capability_id.to_owned(),
                status: CapabilityStatus::Verified,
                detail: None,
            });
            for document in observed {
                match discovered_document(document)? {
                    Some(document) => {
                        if document.project_name.is_none() || document.project_path.is_none() {
                            diagnostics.push(Diagnostic {
                                code: "project_identity_incomplete".to_owned(),
                                message: document.document_id.to_string(),
                            });
                        }
                        documents.push(document);
                    }
                    None => diagnostics.push(Diagnostic {
                        code: "document_identity_unknown".to_owned(),
                        message: capability_id.to_owned(),
                    }),
                }
            }
        }
        DocumentObservation::Unsupported(detail) => {
            capabilities.push(CapabilityObservation {
                id: capability_id.to_owned(),
                status: CapabilityStatus::Unsupported,
                detail: Some(detail),
            });
        }
        DocumentObservation::Failed { code, message } => {
            capabilities.push(CapabilityObservation {
                id: capability_id.to_owned(),
                status: CapabilityStatus::Unknown,
                detail: Some(code.clone()),
            });
            diagnostics.push(Diagnostic { code, message });
        }
    }
    Ok(())
}

fn push_field(output: &mut Vec<u8>, value: &[u8]) {
    output.extend_from_slice(&value.len().to_be_bytes());
    output.extend_from_slice(value);
}

fn push_optional(output: &mut Vec<u8>, value: Option<&str>) {
    match value {
        Some(value) => {
            output.push(1);
            push_field(output, value.as_bytes());
        }
        None => output.push(0),
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SchematicObservation {
    pub project_id: StableId,
    pub revision: RevisionDigest,
    pub symbols: Vec<ObservedSchematicSymbol>,
    pub capabilities: Vec<CapabilityObservation>,
    pub diagnostics: Vec<Diagnostic>,
    pub baseline_eligible: bool,
}

#[derive(Debug)]
pub enum SchematicObservationError {
    Join(kicad_schematic::ConnectivityJoinError),
}

impl std::fmt::Display for SchematicObservationError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Join(error) => error.fmt(formatter),
        }
    }
}

impl std::error::Error for SchematicObservationError {}

pub fn schematic_observation(
    project: &ParsedSchematicProject,
    netlist: &NetlistDocument,
    project_id: StableId,
) -> Result<SchematicObservation, SchematicObservationError> {
    let symbols = join_netlist_connectivity(project, netlist, &project_id)
        .map_err(SchematicObservationError::Join)?
        .into_iter()
        .map(|component| component.symbol.canonical())
        .collect::<Vec<_>>();
    let locked = project.editor_lock_present || project.revision.editor_lock_present;
    let mut diagnostics = Vec::new();
    if locked {
        diagnostics.push(Diagnostic {
            code: "schematic_editor_lock_present".to_owned(),
            message: "saved schematic observation is stale and cannot be a commit baseline"
                .to_owned(),
        });
    }
    let status = if locked {
        CapabilityStatus::Unknown
    } else {
        CapabilityStatus::Verified
    };
    Ok(SchematicObservation {
        project_id,
        revision: project.revision.digest.clone(),
        symbols,
        capabilities: vec![CapabilityObservation {
            id: "schematic.saved_artifact.read".to_owned(),
            status,
            detail: locked.then_some("editor lock present".to_owned()),
        }],
        diagnostics,
        baseline_eligible: !locked,
    })
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LiveBoardObservation {
    pub snapshot: ProjectSnapshot,
    pub instance_token: InstanceToken,
    pub board_filename: String,
    pub board_path: PathBuf,
    pub project_dir: PathBuf,
}

#[derive(Debug)]
pub enum LiveBoardObservationError {
    NonPcbDocument,
    MissingProjectIdentity,
    MissingDocumentIdentity,
    DuplicateGraphicId(String),
    UnsupportedGraphicLayer,
    Model(ModelError),
}

impl std::fmt::Display for LiveBoardObservationError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NonPcbDocument => formatter.write_str("open document is not a PCB"),
            Self::MissingProjectIdentity => formatter.write_str("project identity is unavailable"),
            Self::MissingDocumentIdentity => {
                formatter.write_str("PCB document identity is unavailable")
            }
            Self::DuplicateGraphicId(id) => write!(formatter, "duplicate board graphic id: {id}"),
            Self::UnsupportedGraphicLayer => formatter.write_str("unsupported board graphic layer"),
            Self::Model(error) => error.fmt(formatter),
        }
    }
}

impl std::error::Error for LiveBoardObservationError {}

impl From<ModelError> for LiveBoardObservationError {
    fn from(error: ModelError) -> Self {
        Self::Model(error)
    }
}

pub fn live_board_observation(
    version: VersionInfo,
    document: OpenDocument,
    graphics: Vec<ObservedBoardGraphicSegment>,
    instance_token: InstanceToken,
) -> Result<LiveBoardObservation, LiveBoardObservationError> {
    let board_filename = match &document.kind {
        OpenDocumentKind::Pcb { board_filename } if !board_filename.is_empty() => {
            board_filename.clone()
        }
        OpenDocumentKind::Pcb { .. } => {
            return Err(LiveBoardObservationError::MissingDocumentIdentity);
        }
        _ => return Err(LiveBoardObservationError::NonPcbDocument),
    };
    if document.project_path.is_none() && document.project_name.is_none() {
        return Err(LiveBoardObservationError::MissingProjectIdentity);
    }
    let project_path = document
        .project_path
        .as_deref()
        .filter(|path| !path.is_empty())
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
        .ok_or(LiveBoardObservationError::MissingProjectIdentity)?;
    let board_path = {
        let filename = Path::new(&board_filename);
        if filename.is_absolute() {
            filename.to_owned()
        } else {
            project_path.join(filename)
        }
    };
    let discovered = discovered_document(document.clone())?
        .ok_or(LiveBoardObservationError::MissingDocumentIdentity)?;
    let mut project_identity = Vec::new();
    push_optional(&mut project_identity, document.project_path.as_deref());
    push_optional(&mut project_identity, document.project_name.as_deref());
    let project_id = StableId::new(format!(
        "project:sha256:{}",
        hex::encode(Sha256::digest(&project_identity))
    ))?;
    let mut graphic_ids = BTreeSet::new();
    let graphics = graphics
        .into_iter()
        .map(|observed| {
            if !graphic_ids.insert(observed.graphic_id.clone()) {
                return Err(LiveBoardObservationError::DuplicateGraphicId(
                    observed.graphic_id,
                ));
            }
            if observed.segment.layer != IpcBoardGraphicLayer::DwgsUser {
                return Err(LiveBoardObservationError::UnsupportedGraphicLayer);
            }
            let graphic = BoardGraphicSegment {
                graphic_id: StableId::new(observed.graphic_id)?,
                start: Coordinate {
                    x: observed.segment.start_x_nm,
                    y: observed.segment.start_y_nm,
                    unit: Unit::Nanometres,
                    space: DocumentSpace::Board,
                },
                end: Coordinate {
                    x: observed.segment.end_x_nm,
                    y: observed.segment.end_y_nm,
                    unit: Unit::Nanometres,
                    space: DocumentSpace::Board,
                },
                width_nm: observed.segment.width_nm,
                layer: BoardGraphicLayer::DwgsUser,
            };
            graphic.validate()?;
            Ok(graphic)
        })
        .collect::<Result<Vec<_>, LiveBoardObservationError>>()?;
    let snapshot = ProjectSnapshot {
        project_id,
        kicad_version: version.full_version,
        capabilities: vec![
            "pcb.board_graphics.read".to_owned(),
            "pcb.board_graphics.write".to_owned(),
        ],
        schematic: Schematic::default(),
        board: Board {
            document_id: Some(discovered.document_id),
            footprints: Vec::new(),
            graphics,
        },
    }
    .canonical();
    Ok(LiveBoardObservation {
        snapshot,
        instance_token,
        board_filename,
        board_path,
        project_dir: project_path,
    })
}

fn discovered_document(document: OpenDocument) -> Result<Option<DiscoveredDocument>, ModelError> {
    let kind = match document.kind {
        OpenDocumentKind::Schematic {
            filename,
            sheet_path,
            sheet_kiids,
        } => {
            if filename.is_none() && sheet_path.is_none() && sheet_kiids.is_empty() {
                return Ok(None);
            }
            DiscoveredDocumentKind::Schematic {
                filename,
                sheet_path,
                sheet_kiids,
            }
        }
        OpenDocumentKind::Pcb { board_filename } => {
            if board_filename.is_empty() {
                return Ok(None);
            }
            DiscoveredDocumentKind::Pcb {
                filename: board_filename,
            }
        }
        OpenDocumentKind::Other => return Ok(None),
    };
    let mut identity = Vec::new();
    push_optional(&mut identity, document.project_path.as_deref());
    push_optional(&mut identity, document.project_name.as_deref());
    match &kind {
        DiscoveredDocumentKind::Schematic {
            filename,
            sheet_path,
            sheet_kiids,
        } => {
            push_field(&mut identity, b"schematic");
            push_optional(&mut identity, filename.as_deref());
            push_optional(&mut identity, sheet_path.as_deref());
            for kiid in sheet_kiids {
                push_field(&mut identity, kiid.as_bytes());
            }
        }
        DiscoveredDocumentKind::Pcb { filename } => {
            push_field(&mut identity, b"pcb");
            push_field(&mut identity, filename.as_bytes());
        }
        DiscoveredDocumentKind::Unknown => push_field(&mut identity, b"unknown"),
    }
    let digest = Sha256::digest(&identity);
    Ok(Some(DiscoveredDocument {
        document_id: StableId::new(format!("document:sha256:{}", hex::encode(digest)))?,
        project_name: document.project_name,
        project_path: document.project_path,
        kind,
    }))
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum BoardGraphicsCommitOutcome {
    CommittedAndSaved,
    CommittedButNotSaved { reason: String },
    Rejected { reason: String },
    Unknown { reason: String },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BoardGraphicsExecutor {
    pub max_segments: usize,
}

impl Default for BoardGraphicsExecutor {
    fn default() -> Self {
        Self { max_segments: 100 }
    }
}

pub trait BoardGraphicsIpc {
    fn begin_commit(&mut self) -> Result<String, String>;
    fn create_board_graphics(
        &mut self,
        board_filename: &str,
        graphics: &[IpcBoardGraphicSegment],
    ) -> Result<(), String>;
    fn commit_changes(&mut self, commit: &str, message: &str) -> Result<(), String>;
    fn drop_commit(&mut self, commit: &str) -> Result<(), String>;
    fn save_board(&mut self, board_filename: &str) -> Result<(), String>;
}

impl<T: KiCadTransport> BoardGraphicsIpc for KiCadClient<T> {
    fn begin_commit(&mut self) -> Result<String, String> {
        KiCadClient::begin_commit(self)
            .map(|commit| commit.as_str().to_owned())
            .map_err(|error| error.to_string())
    }

    fn create_board_graphics(
        &mut self,
        board_filename: &str,
        graphics: &[IpcBoardGraphicSegment],
    ) -> Result<(), String> {
        KiCadClient::create_board_graphics(self, board_filename, graphics)
            .map_err(|error| error.to_string())
    }

    fn commit_changes(&mut self, commit: &str, message: &str) -> Result<(), String> {
        let commit = CommitId::new(commit).map_err(|error| error.to_string())?;
        KiCadClient::commit_changes(self, &commit, message).map_err(|error| error.to_string())
    }

    fn drop_commit(&mut self, commit: &str) -> Result<(), String> {
        let commit = CommitId::new(commit).map_err(|error| error.to_string())?;
        KiCadClient::drop_commit(self, &commit).map_err(|error| error.to_string())
    }

    fn save_board(&mut self, board_filename: &str) -> Result<(), String> {
        KiCadClient::save_board(self, board_filename).map_err(|error| error.to_string())
    }
}

impl BoardGraphicsExecutor {
    pub fn execute<I: BoardGraphicsIpc>(
        &self,
        ipc: &mut I,
        operation: &OperationKind,
        board_filename: &str,
        message: &str,
    ) -> BoardGraphicsCommitOutcome {
        let OperationKind::AddBoardGraphicSegments { segments, .. } = operation else {
            return BoardGraphicsCommitOutcome::Rejected {
                reason: "operation is not board graphics".into(),
            };
        };
        if board_filename.is_empty() {
            return BoardGraphicsCommitOutcome::Rejected {
                reason: "board filename is empty".into(),
            };
        }
        if segments.is_empty() || segments.len() > self.max_segments {
            return BoardGraphicsCommitOutcome::Rejected {
                reason: "graphic segment count exceeded bounds".into(),
            };
        }
        if segments.iter().any(|segment| {
            segment.validate().is_err() || !matches!(segment.layer, BoardGraphicLayer::DwgsUser)
        }) {
            return BoardGraphicsCommitOutcome::Rejected {
                reason: "graphic segment is invalid or uses an unsupported layer".into(),
            };
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
        let area = width.checked_mul(height);
        if width > MAX_GRAPHIC_SPAN_NM
            || height > MAX_GRAPHIC_SPAN_NM
            || area.is_none_or(|area| area > MAX_GRAPHIC_BOUNDING_AREA_NM2)
        {
            return BoardGraphicsCommitOutcome::Rejected {
                reason: "graphic bounds exceeded".into(),
            };
        }
        let graphics = segments
            .iter()
            .map(|segment| IpcBoardGraphicSegment {
                start_x_nm: segment.start.x,
                start_y_nm: segment.start.y,
                end_x_nm: segment.end.x,
                end_y_nm: segment.end.y,
                width_nm: segment.width_nm,
                layer: IpcBoardGraphicLayer::DwgsUser,
            })
            .collect::<Vec<_>>();
        let commit = match ipc.begin_commit() {
            Ok(commit) => commit,
            Err(reason) => return BoardGraphicsCommitOutcome::Unknown { reason },
        };
        if let Err(reason) = ipc.create_board_graphics(board_filename, &graphics) {
            return match ipc.drop_commit(&commit) {
                Ok(()) => BoardGraphicsCommitOutcome::Rejected { reason },
                Err(drop_reason) => BoardGraphicsCommitOutcome::Unknown {
                    reason: format!("create failed: {reason}; drop failed: {drop_reason}"),
                },
            };
        }
        if let Err(reason) = ipc.commit_changes(&commit, message) {
            return BoardGraphicsCommitOutcome::Unknown { reason };
        }
        match ipc.save_board(board_filename) {
            Ok(()) => BoardGraphicsCommitOutcome::CommittedAndSaved,
            Err(reason) => BoardGraphicsCommitOutcome::CommittedButNotSaved { reason },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use kicad_model::{
        BoardGraphicSegment, Coordinate, DocumentSpace, RevisionDigest, SchematicArtifactFile,
        SchematicArtifactRevision, Unit,
    };
    use kicad_schematic::{
        InstancePath, NetlistComponent, NetlistNet, NetlistNode, ParsedSchematic, Position,
        RawSymbol, RawSymbolPin,
    };

    fn version() -> VersionInfo {
        VersionInfo {
            major: 10,
            minor: 0,
            patch: 4,
            full_version: "10.0.4".to_owned(),
        }
    }

    fn schematic(filename: &str) -> OpenDocument {
        OpenDocument {
            project_name: None,
            project_path: None,
            kind: OpenDocumentKind::Schematic {
                filename: Some(filename.to_owned()),
                sheet_path: None,
                sheet_kiids: Vec::new(),
            },
        }
    }

    fn input(documents: Vec<OpenDocument>) -> DiscoveryInput {
        DiscoveryInput {
            endpoint: "ipc:///tmp/kicad/api.sock".to_owned(),
            version: version(),
            schematic: DocumentObservation::Documents(documents),
            pcb: DocumentObservation::Unsupported("no handler".to_owned()),
        }
    }

    fn pcb() -> OpenDocument {
        OpenDocument {
            project_name: Some("xLock".to_owned()),
            project_path: Some("/projects/xLock".to_owned()),
            kind: OpenDocumentKind::Pcb {
                board_filename: "xLock.kicad_pcb".to_owned(),
            },
        }
    }

    fn schematic_fixture(locked: bool) -> (ParsedSchematicProject, NetlistDocument) {
        let artifact = SchematicArtifactFile::new(
            "root.kicad_sch",
            1,
            RevisionDigest::new("a".repeat(64)).unwrap(),
        )
        .unwrap();
        let revision =
            SchematicArtifactRevision::new(vec![artifact.clone()], "10.0.4", 1, locked).unwrap();
        let symbol = RawSymbol {
            source_uuid: "symbol-uuid".to_owned(),
            lib_id: "Device:R".to_owned(),
            position: Position {
                x: 1.0,
                y: 2.0,
                orientation_degrees: Some(0.0),
            },
            unit: 1,
            body_style: None,
            reference: "R1".to_owned(),
            value: "10k".to_owned(),
            footprint: Some("Resistor_SMD:R_0603".to_owned()),
            on_board: Some(true),
            in_bom: Some(true),
            dnp: Some(false),
            pins: vec![RawSymbolPin {
                number: "1".to_owned(),
                source_uuid: "pin-uuid".to_owned(),
            }],
            instance_paths: vec![InstancePath {
                project: "demo".to_owned(),
                path: "/file-uuid/root/symbol-uuid".to_owned(),
                reference: Some("R1".to_owned()),
                unit: Some(1),
                page: Some("1".to_owned()),
            }],
        };
        let parsed_file = ParsedSchematic {
            format_version: "20250114".to_owned(),
            generator: "kicad".to_owned(),
            generator_version: Some("10.0.4".to_owned()),
            file_uuid: "file-uuid".to_owned(),
            sheets: Vec::new(),
            symbols: vec![symbol],
            diagnostics: Vec::new(),
            artifact,
        };
        let project = ParsedSchematicProject {
            root_relative_path: "root.kicad_sch".to_owned(),
            files: vec![parsed_file],
            revision,
            editor_lock_present: locked,
        };
        let netlist = NetlistDocument {
            export_version: "D".to_owned(),
            sheets: Vec::new(),
            components: vec![NetlistComponent {
                reference: "R1".to_owned(),
                value: "10k".to_owned(),
                footprint: Some("Resistor_SMD:R_0603".to_owned()),
                symbol_uuid: Some("symbol-uuid".to_owned()),
                sheet_path: "/root".to_owned(),
                sheet_uuid_path: "/root/symbol-uuid".to_owned(),
                library: "Device".to_owned(),
                library_part: "R".to_owned(),
            }],
            nets: vec![NetlistNet {
                code: "1".to_owned(),
                name: "GND".to_owned(),
                class: None,
                nodes: vec![NetlistNode {
                    reference: "R1".to_owned(),
                    pin: "1".to_owned(),
                    pin_function: "passive".to_owned(),
                    pin_type: "passive".to_owned(),
                }],
            }],
        };
        (project, netlist)
    }

    #[test]
    fn schematic_observation_maps_connectivity_and_is_deterministic() {
        let (project, netlist) = schematic_fixture(false);
        let first =
            schematic_observation(&project, &netlist, StableId::new("project").unwrap()).unwrap();
        let second =
            schematic_observation(&project, &netlist, StableId::new("project").unwrap()).unwrap();
        assert_eq!(first, second);
        assert!(first.baseline_eligible);
        assert_eq!(first.capabilities[0].status, CapabilityStatus::Verified);
        assert!(matches!(
            first.symbols[0].pins[0].net,
            kicad_model::ReferenceResolution::Resolved(_)
        ));
        assert_eq!(
            first.symbols[0].footprint_field.as_deref(),
            Some("Resistor_SMD:R_0603")
        );
    }

    #[test]
    fn schematic_observation_lock_is_not_a_verified_baseline() {
        let (project, netlist) = schematic_fixture(true);
        let observation =
            schematic_observation(&project, &netlist, StableId::new("project").unwrap()).unwrap();
        assert!(!observation.baseline_eligible);
        assert_eq!(
            observation.capabilities[0].status,
            CapabilityStatus::Unknown
        );
        assert_eq!(
            observation.diagnostics[0].code,
            "schematic_editor_lock_present"
        );
    }

    #[test]
    fn schematic_observation_propagates_join_errors() {
        let (project, mut netlist) = schematic_fixture(false);
        netlist.nets[0].nodes[0].reference = "X1".to_owned();
        assert!(matches!(
            schematic_observation(&project, &netlist, StableId::new("project").unwrap()),
            Err(SchematicObservationError::Join(
                kicad_schematic::ConnectivityJoinError::UnknownNode { .. }
            ))
        ));
    }

    fn observed_graphic(id: &str, x: i64) -> ObservedBoardGraphicSegment {
        ObservedBoardGraphicSegment {
            graphic_id: id.to_owned(),
            segment: IpcBoardGraphicSegment {
                start_x_nm: x,
                start_y_nm: 2,
                end_x_nm: x + 10,
                end_y_nm: 4,
                width_nm: 100_000,
                layer: IpcBoardGraphicLayer::DwgsUser,
            },
        }
    }

    #[test]
    fn live_board_observation_maps_identity_geometry_and_capabilities() {
        let observation = live_board_observation(
            version(),
            pcb(),
            vec![observed_graphic("graphic-1", 1)],
            InstanceToken::new("token").unwrap(),
        )
        .unwrap();
        let discovered = discovered_document(pcb()).unwrap().unwrap();
        assert_eq!(observation.board_filename, "xLock.kicad_pcb");
        assert_eq!(observation.instance_token.as_str(), "token");
        assert_eq!(
            observation.snapshot.board.document_id,
            Some(discovered.document_id)
        );
        assert_eq!(
            observation.snapshot.capabilities,
            ["pcb.board_graphics.read", "pcb.board_graphics.write"]
        );
        assert!(observation.snapshot.schematic.nets.is_empty());
        assert!(observation.snapshot.schematic.symbols.is_empty());
        assert!(observation.snapshot.board.footprints.is_empty());
        let graphic = &observation.snapshot.board.graphics[0];
        assert_eq!(graphic.graphic_id.as_str(), "graphic-1");
        assert_eq!((graphic.start.x, graphic.end.x), (1, 11));
        assert_eq!(graphic.start.space, DocumentSpace::Board);
        assert_eq!(graphic.width_nm, 100_000);
    }

    #[test]
    fn live_board_revision_is_independent_of_graphic_order() {
        let first = live_board_observation(
            version(),
            pcb(),
            vec![observed_graphic("b", 20), observed_graphic("a", 1)],
            InstanceToken::new("token").unwrap(),
        )
        .unwrap();
        let second = live_board_observation(
            version(),
            pcb(),
            vec![observed_graphic("a", 1), observed_graphic("b", 20)],
            InstanceToken::new("token").unwrap(),
        )
        .unwrap();
        assert_eq!(first.snapshot, second.snapshot);
        assert_eq!(
            first.snapshot.revision().unwrap(),
            second.snapshot.revision().unwrap()
        );
    }

    #[test]
    fn live_board_observation_rejects_duplicate_or_invalid_graphic_ids() {
        let duplicate = live_board_observation(
            version(),
            pcb(),
            vec![observed_graphic("same", 1), observed_graphic("same", 20)],
            InstanceToken::new("token").unwrap(),
        )
        .unwrap_err();
        assert!(matches!(
            duplicate,
            LiveBoardObservationError::DuplicateGraphicId(ref id) if id == "same"
        ));
        let invalid = live_board_observation(
            version(),
            pcb(),
            vec![observed_graphic("", 1)],
            InstanceToken::new("token").unwrap(),
        )
        .unwrap_err();
        assert!(matches!(
            invalid,
            LiveBoardObservationError::Model(ModelError::EmptyStableId)
        ));
    }

    #[test]
    fn live_board_observation_rejects_missing_project_identity() {
        let mut document = pcb();
        document.project_name = None;
        document.project_path = None;
        let error = live_board_observation(
            version(),
            document,
            Vec::new(),
            InstanceToken::new("token").unwrap(),
        )
        .unwrap_err();
        assert!(matches!(
            error,
            LiveBoardObservationError::MissingProjectIdentity
        ));
    }

    #[test]
    fn live_board_observation_rejects_non_pcb_document() {
        let error = live_board_observation(
            version(),
            schematic("xLock.kicad_sch"),
            Vec::new(),
            InstanceToken::new("token").unwrap(),
        )
        .unwrap_err();
        assert!(matches!(error, LiveBoardObservationError::NonPcbDocument));

        let mut document = pcb();
        document.kind = OpenDocumentKind::Other;
        let error = live_board_observation(
            version(),
            document,
            Vec::new(),
            InstanceToken::new("token").unwrap(),
        )
        .unwrap_err();
        assert!(matches!(error, LiveBoardObservationError::NonPcbDocument));
    }

    #[test]
    fn digest_is_independent_of_document_insertion_order() {
        let first = discovery_snapshot(input(vec![
            schematic("b.kicad_sch"),
            schematic("a.kicad_sch"),
        ]))
        .unwrap();
        let second = discovery_snapshot(input(vec![
            schematic("a.kicad_sch"),
            schematic("b.kicad_sch"),
        ]))
        .unwrap();
        assert_eq!(first.revision().unwrap(), second.revision().unwrap());
    }

    #[test]
    fn missing_project_identity_remains_explicit() {
        let snapshot = discovery_snapshot(input(vec![schematic("xLock.kicad_sch")])).unwrap();
        assert_eq!(snapshot.documents[0].project_name, None);
        assert!(
            snapshot
                .diagnostics
                .iter()
                .any(|diagnostic| { diagnostic.code == "project_identity_incomplete" })
        );
    }

    #[test]
    fn artifact_discovery_finds_same_named_project_documents() {
        let root = std::env::temp_dir().join(format!(
            "kicad-adapter-{}-{}",
            std::process::id(),
            std::thread::current().name().unwrap_or("test")
        ));
        fs::create_dir_all(&root).unwrap();
        fs::write(root.join("fixture.kicad_pro"), "{}").unwrap();
        fs::write(root.join("fixture.kicad_sch"), "(kicad_sch)").unwrap();
        fs::write(root.join("fixture.kicad_pcb"), "(kicad_pcb)").unwrap();
        let snapshot =
            artifact_discovery_snapshot(&root, "ipc:///tmp/kicad/api.sock", version()).unwrap();
        assert_eq!(snapshot.documents.len(), 2);
        assert!(snapshot.capabilities.iter().any(|capability| {
            capability.id == "schematic.documents.discover"
                && capability.detail.as_deref() == Some("offline artifact source")
        }));
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn unsupported_pcb_and_unprobed_items_are_not_advertised_as_verified() {
        let snapshot = discovery_snapshot(input(vec![schematic("xLock.kicad_sch")])).unwrap();
        let pcb = snapshot
            .capabilities
            .iter()
            .find(|capability| capability.id == "pcb.documents.discover")
            .unwrap();
        let items = snapshot
            .capabilities
            .iter()
            .find(|capability| capability.id == "schematic.items.read")
            .unwrap();
        assert_eq!(pcb.status, CapabilityStatus::Unsupported);
        assert_eq!(items.status, CapabilityStatus::Unknown);
        assert!(
            snapshot
                .diagnostics
                .iter()
                .any(|diagnostic| { diagnostic.code == "content_not_observed" })
        );
    }

    struct FakeGraphicsIpc {
        begin: Result<String, String>,
        create: Result<(), String>,
        commit: Result<(), String>,
        drop: Result<(), String>,
        save: Result<(), String>,
        calls: Vec<String>,
        graphics: Vec<IpcBoardGraphicSegment>,
    }

    impl Default for FakeGraphicsIpc {
        fn default() -> Self {
            Self {
                begin: Ok("c1".into()),
                create: Ok(()),
                commit: Ok(()),
                drop: Ok(()),
                save: Ok(()),
                calls: Vec::new(),
                graphics: Vec::new(),
            }
        }
    }

    impl BoardGraphicsIpc for FakeGraphicsIpc {
        fn begin_commit(&mut self) -> Result<String, String> {
            self.calls.push("begin".into());
            self.begin.clone()
        }

        fn create_board_graphics(
            &mut self,
            filename: &str,
            graphics: &[IpcBoardGraphicSegment],
        ) -> Result<(), String> {
            self.calls.push(format!("create:{filename}"));
            self.graphics = graphics.to_vec();
            self.create.clone()
        }

        fn commit_changes(&mut self, commit: &str, _: &str) -> Result<(), String> {
            self.calls.push(format!("commit:{commit}"));
            self.commit.clone()
        }

        fn drop_commit(&mut self, commit: &str) -> Result<(), String> {
            self.calls.push(format!("drop:{commit}"));
            self.drop.clone()
        }

        fn save_board(&mut self, filename: &str) -> Result<(), String> {
            self.calls.push(format!("save:{filename}"));
            self.save.clone()
        }
    }

    fn graphic() -> BoardGraphicSegment {
        BoardGraphicSegment {
            graphic_id: StableId::new("g1").unwrap(),
            start: Coordinate {
                x: 1,
                y: 2,
                unit: Unit::Nanometres,
                space: DocumentSpace::Board,
            },
            end: Coordinate {
                x: 3,
                y: 4,
                unit: Unit::Nanometres,
                space: DocumentSpace::Board,
            },
            width_nm: 100,
            layer: BoardGraphicLayer::DwgsUser,
        }
    }

    fn graphics_operation() -> OperationKind {
        OperationKind::AddBoardGraphicSegments {
            board: StableId::new("board").unwrap(),
            segments: vec![graphic()],
        }
    }

    #[test]
    fn graphics_executor_rejects_invalid_input_before_ipc() {
        let mut ipc = FakeGraphicsIpc {
            begin: Ok("c1".into()),
            ..Default::default()
        };
        let mut invalid = graphic();
        invalid.start.space = DocumentSpace::Schematic;
        let operation = OperationKind::AddBoardGraphicSegments {
            board: StableId::new("board").unwrap(),
            segments: vec![invalid],
        };
        let outcome = BoardGraphicsExecutor::default().execute(
            &mut ipc,
            &operation,
            "board.kicad_pcb",
            "draw",
        );
        assert!(matches!(
            outcome,
            BoardGraphicsCommitOutcome::Rejected { .. }
        ));
        assert!(ipc.calls.is_empty());
    }

    #[test]
    fn graphics_executor_reports_create_rejection_after_drop() {
        let mut ipc = FakeGraphicsIpc {
            begin: Ok("c1".into()),
            create: Err("create failed".into()),
            drop: Ok(()),
            ..Default::default()
        };
        let outcome = BoardGraphicsExecutor::default().execute(
            &mut ipc,
            &graphics_operation(),
            "board.kicad_pcb",
            "draw",
        );
        assert_eq!(
            outcome,
            BoardGraphicsCommitOutcome::Rejected {
                reason: "create failed".into()
            }
        );
        assert_eq!(ipc.calls, ["begin", "create:board.kicad_pcb", "drop:c1"]);
    }

    #[test]
    fn graphics_executor_reports_unknown_when_create_and_drop_fail() {
        let mut ipc = FakeGraphicsIpc {
            create: Err("create failed".into()),
            drop: Err("drop failed".into()),
            ..Default::default()
        };
        let outcome = BoardGraphicsExecutor::default().execute(
            &mut ipc,
            &graphics_operation(),
            "board.kicad_pcb",
            "draw",
        );
        assert_eq!(
            outcome,
            BoardGraphicsCommitOutcome::Unknown {
                reason: "create failed: create failed; drop failed: drop failed".into()
            }
        );
    }

    #[test]
    fn graphics_executor_reports_unknown_when_commit_transport_fails() {
        let mut ipc = FakeGraphicsIpc {
            begin: Ok("c1".into()),
            create: Ok(()),
            commit: Err("transport lost".into()),
            ..Default::default()
        };
        let outcome = BoardGraphicsExecutor::default().execute(
            &mut ipc,
            &graphics_operation(),
            "board.kicad_pcb",
            "draw",
        );
        assert_eq!(
            outcome,
            BoardGraphicsCommitOutcome::Unknown {
                reason: "transport lost".into()
            }
        );
    }

    #[test]
    fn graphics_executor_reports_committed_but_not_saved() {
        let mut ipc = FakeGraphicsIpc {
            begin: Ok("c1".into()),
            create: Ok(()),
            commit: Ok(()),
            save: Err("save failed".into()),
            ..Default::default()
        };
        let outcome = BoardGraphicsExecutor::default().execute(
            &mut ipc,
            &graphics_operation(),
            "board.kicad_pcb",
            "draw",
        );
        assert_eq!(
            outcome,
            BoardGraphicsCommitOutcome::CommittedButNotSaved {
                reason: "save failed".into()
            }
        );
        assert_eq!(ipc.graphics[0].layer, IpcBoardGraphicLayer::DwgsUser);
    }
}
