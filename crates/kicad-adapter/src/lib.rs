use kicad_engine::{MAX_GRAPHIC_BOUNDING_AREA_NM2, MAX_GRAPHIC_SPAN_NM, OperationKind};
use kicad_ipc::{
    BoardGraphicLayer as IpcBoardGraphicLayer, BoardGraphicSegment as IpcBoardGraphicSegment,
    CommitId, KiCadClient, KiCadTransport, OpenDocument, OpenDocumentKind, VersionInfo,
};
use kicad_model::{
    BoardGraphicLayer, CapabilityObservation, CapabilityStatus, Diagnostic, DiscoveredDocument,
    DiscoveredDocumentKind, DiscoverySnapshot, ModelError, StableId,
};
use sha2::{Digest, Sha256};
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
    use kicad_model::{BoardGraphicSegment, Coordinate, DocumentSpace, Unit};

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
