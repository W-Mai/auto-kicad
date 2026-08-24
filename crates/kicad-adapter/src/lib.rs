use kicad_ipc::{OpenDocument, OpenDocumentKind, VersionInfo};
use kicad_model::{
    CapabilityObservation, CapabilityStatus, Diagnostic, DiscoveredDocument,
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

#[cfg(test)]
mod tests {
    use super::*;

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
}
