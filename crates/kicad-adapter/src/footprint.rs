use crate::library_catalog::{
    FootprintCatalog, FootprintResolutionEvidence, LibraryScope, read_source_digest,
};
use crate::lifecycle::{DisposablePcbLifecycle, LifecycleError, OwnedPcbnewLauncher};
use kicad_ipc::{
    CreatedFootprintInstanceReceipt, FootprintInstanceTemplate, KiCadClient, KiCadTransport,
    ObservedFootprintInstance, ObservedPad,
};
use kicad_model::{AllowlistedFootprintPlacement, FootprintLibraryIdentifier, RevisionDigest};
#[cfg(test)]
use sha2::{Digest, Sha256};
use std::collections::HashSet;
use std::fmt::{Display, Formatter};
use std::fs;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::time::Duration;

pub const MAX_PLACEMENT_COORDINATE_NM: i64 = 2_000_000_000;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AllowlistedFootprint {
    OfficialLed1210,
    OfficialResistor0603,
    ProjectTypeC,
}

impl AllowlistedFootprint {
    pub fn from_identifier(identifier: &FootprintLibraryIdentifier) -> Option<Self> {
        [
            Self::OfficialLed1210,
            Self::OfficialResistor0603,
            Self::ProjectTypeC,
        ]
        .into_iter()
        .find(|library| library.identifier() == *identifier)
    }

    pub fn identifier(self) -> FootprintLibraryIdentifier {
        match self {
            Self::OfficialLed1210 => FootprintLibraryIdentifier {
                nickname: "LED_SMD".to_owned(),
                entry_name: "LED_1210_3225Metric_Pad1.42x2.65mm_HandSolder".to_owned(),
            },
            Self::OfficialResistor0603 => FootprintLibraryIdentifier {
                nickname: "Resistor_SMD".into(),
                entry_name: "R_0603_1608Metric".into(),
            },
            Self::ProjectTypeC => FootprintLibraryIdentifier {
                nickname: "Type-C".to_owned(),
                entry_name: "USB_C_GCT_USB4085".to_owned(),
            },
        }
    }

    const fn source_subpath(self) -> &'static str {
        match self {
            Self::OfficialLed1210 => "LED_1210_3225Metric_Pad1.42x2.65mm_HandSolder.kicad_mod",
            Self::ProjectTypeC => "USB_C_GCT_USB4085.kicad_mod",
            Self::OfficialResistor0603 => "R_0603_1608Metric.kicad_mod",
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FootprintSourceKind {
    OfficialLibrary,
    ProjectLibrary,
    GlobalLibrary,
    ConfiguredLibrary,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ResolutionEvidence {
    pub method: &'static str,
    pub identifier: FootprintLibraryIdentifier,
    pub source_exists: bool,
    pub source_is_confined: bool,
    pub digest_verified: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LibraryProvenance {
    pub source_kind: FootprintSourceKind,
    pub registry_root: PathBuf,
    pub source_path: PathBuf,
    pub source_size: u64,
    pub source_digest: RevisionDigest,
    pub resolution_evidence: ResolutionEvidence,
    pub catalog_evidence: Option<FootprintResolutionEvidence>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ResolvedFootprint {
    pub identifier: FootprintLibraryIdentifier,
    pub source_path: PathBuf,
    pub provenance: LibraryProvenance,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FootprintPlacementReceipt {
    pub resolved: ResolvedFootprint,
    pub placement: AllowlistedFootprintPlacement,
    pub postcondition: FootprintPostcondition,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PreparedFootprintPlacement {
    context: DisposableFootprintContext,
    resolved: ResolvedFootprint,
    placement: AllowlistedFootprintPlacement,
}

impl PreparedFootprintPlacement {
    pub fn context(&self) -> &DisposableFootprintContext {
        &self.context
    }

    pub fn resolved(&self) -> &ResolvedFootprint {
        &self.resolved
    }

    pub fn placement(&self) -> &AllowlistedFootprintPlacement {
        &self.placement
    }
}

#[allow(dead_code)]
pub(crate) fn place_allowlisted_footprints(
    server_root: &Path,
    registry: &FootprintRegistry,
    source_board: &Path,
    placements: &[AllowlistedFootprintPlacement],
) -> Result<Vec<FootprintPlacementReceipt>, FootprintBackendError> {
    let mut launcher = OwnedPcbnewLauncher::new(server_root).map_err(lifecycle_error)?;
    let shadow = match launcher.create_shadow(source_board) {
        Ok(shadow) => shadow,
        Err(error) => {
            return cleanup_disposable_footprint_failure(&mut launcher, lifecycle_error(error));
        }
    };
    let context = match DisposableFootprintContext::new(shadow.root(), shadow.board_path()) {
        Ok(context) => context,
        Err(error) => return cleanup_disposable_footprint_failure(&mut launcher, error),
    };
    if let Err(error) = context.validate_before_place() {
        return cleanup_disposable_footprint_failure(&mut launcher, error);
    }
    if placements.is_empty() {
        return cleanup_disposable_footprint_failure(
            &mut launcher,
            FootprintBackendError::InvalidPlacement("footprint batch must not be empty"),
        );
    }

    let mut requests = Vec::with_capacity(placements.len());
    for placement in placements.iter().cloned() {
        match context.prepare_identifier(registry, placement) {
            Ok(request) => requests.push(request),
            Err(error) => return cleanup_disposable_footprint_failure(&mut launcher, error),
        }
    }

    let shadow = shadow.clone();
    if let Err(error) = launcher.launch(&shadow) {
        return cleanup_disposable_footprint_failure(&mut launcher, lifecycle_error(error));
    }
    let receipts = match DisposablePythonFootprintExecutor::place_batch_preserving_shadow(
        &mut launcher,
        &requests,
        "allowlisted footprint placement",
    ) {
        Ok(receipts) => receipts,
        Err(error) => return cleanup_disposable_footprint_failure(&mut launcher, error),
    };
    launcher.cleanup().map_err(lifecycle_error)?;
    Ok(receipts)
}

fn cleanup_disposable_footprint_failure(
    launcher: &mut OwnedPcbnewLauncher,
    error: FootprintBackendError,
) -> Result<Vec<FootprintPlacementReceipt>, FootprintBackendError> {
    let mut cleanup_errors = Vec::new();
    let shadow_root = launcher
        .current_shadow()
        .ok()
        .map(|shadow| shadow.root().to_path_buf());
    if let Ok(identity) = launcher.current_identity().cloned() {
        let mut use_lifecycle_cleanup = true;
        match launcher.close_owned(identity.pid) {
            Ok(()) => {}
            Err(LifecycleError::NoOwnedEditor) => {
                use_lifecycle_cleanup = false;
                if let Some(shadow_root) = &shadow_root
                    && let Err(error) = fs::remove_dir_all(shadow_root)
                {
                    cleanup_errors.push(error.to_string());
                }
            }
            Err(error) => cleanup_errors.push(error.to_string()),
        }
        if use_lifecycle_cleanup && let Err(error) = launcher.cleanup() {
            cleanup_errors.push(error.to_string());
        }
    } else if let Some(shadow_root) = &shadow_root
        && let Err(error) = fs::remove_dir_all(shadow_root)
    {
        cleanup_errors.push(error.to_string());
    }
    if cleanup_errors.is_empty() {
        Err(error)
    } else {
        Err(FootprintBackendError::Lifecycle(format!(
            "{}; {}",
            error,
            cleanup_errors.join("; ")
        )))
    }
}

#[derive(Debug)]
pub enum FootprintBackendError {
    InvalidRoot,
    PathOutsideProject,
    EditorLockPresent,
    UnsupportedLibrary,
    SourceResolution(String),
    SourceMissing(PathBuf),
    InvalidPlacement(&'static str),
    UnsupportedRunner,
    RunnerProcess(String),
    RunnerProtocol(&'static str),
    RunnerTimeout,
    Lifecycle(String),
    Ipc(String),
    TemplateNotFound,
    TemplateProvenanceUnavailable,
    PostconditionMismatch,
    UuidSetMismatch,
}

impl Display for FootprintBackendError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidRoot => formatter.write_str("footprint backend root must be a directory"),
            Self::PathOutsideProject => formatter.write_str("board path is outside project root"),
            Self::EditorLockPresent => formatter.write_str("PCB editor lock is present"),
            Self::SourceResolution(reason) => {
                write!(formatter, "footprint source resolution failed: {reason}")
            }
            Self::UnsupportedLibrary => formatter.write_str("footprint library is not allowlisted"),
            Self::SourceMissing(path) => write!(
                formatter,
                "allowlisted footprint source is missing: {}",
                path.display()
            ),
            Self::InvalidPlacement(message) => formatter.write_str(message),
            Self::UnsupportedRunner => {
                formatter.write_str("disposable footprint runner is unavailable")
            }
            Self::RunnerProcess(message) => {
                write!(formatter, "disposable footprint helper failed: {message}")
            }
            Self::RunnerProtocol(message) => write!(
                formatter,
                "invalid disposable footprint helper response: {message}"
            ),
            Self::RunnerTimeout => formatter.write_str("disposable footprint helper timed out"),
            Self::Lifecycle(message) => {
                write!(formatter, "disposable PCB lifecycle failed: {message}")
            }
            Self::Ipc(message) => write!(formatter, "footprint IPC failed: {message}"),
            Self::TemplateNotFound => {
                formatter.write_str("matching allowlisted footprint template was not observed")
            }
            Self::TemplateProvenanceUnavailable => formatter.write_str(
                "captured footprint template is not bound to the allowlisted source provenance",
            ),
            Self::PostconditionMismatch => {
                formatter.write_str("footprint postcondition did not match the prepared placement")
            }
            Self::UuidSetMismatch => {
                formatter.write_str("footprint UUID set difference was not exactly one")
            }
        }
    }
}

impl std::error::Error for FootprintBackendError {}

#[derive(Clone, Debug)]
pub struct FootprintRegistry {
    official_library: PathBuf,
    project_library: PathBuf,
    catalog: Option<FootprintCatalog>,
}

impl FootprintRegistry {
    pub fn new(
        official_library: &Path,
        project_library: &Path,
    ) -> Result<Self, FootprintBackendError> {
        Ok(Self {
            official_library: canonical_directory(official_library)?,
            project_library: canonical_directory(project_library)?,
            catalog: None,
        })
    }

    pub fn from_catalog(catalog: FootprintCatalog) -> Self {
        Self {
            official_library: PathBuf::new(),
            project_library: PathBuf::new(),
            catalog: Some(catalog),
        }
    }

    pub fn resolve_identifier(
        &self,
        identifier: &FootprintLibraryIdentifier,
    ) -> Result<ResolvedFootprint, FootprintBackendError> {
        let Some(catalog) = &self.catalog else {
            let library = AllowlistedFootprint::from_identifier(identifier)
                .ok_or(FootprintBackendError::UnsupportedLibrary)?;
            return self.resolve_legacy(library);
        };
        let evidence = catalog
            .resolve(identifier)
            .map_err(FootprintBackendError::SourceResolution)?;
        Ok(ResolvedFootprint {
            identifier: identifier.clone(),
            source_path: evidence.canonical_source_path.clone(),
            provenance: LibraryProvenance {
                source_kind: match evidence.scope {
                    LibraryScope::Project => FootprintSourceKind::ProjectLibrary,
                    LibraryScope::Global => FootprintSourceKind::GlobalLibrary,
                    LibraryScope::LegacyConfigured => FootprintSourceKind::ConfiguredLibrary,
                },
                registry_root: evidence.canonical_library_root.clone(),
                source_path: evidence.canonical_source_path.clone(),
                source_size: evidence.source_size,
                source_digest: RevisionDigest::new(&evidence.source_digest).expect("sha256"),
                resolution_evidence: ResolutionEvidence {
                    method: "canonical_library_catalog",
                    identifier: identifier.clone(),
                    source_exists: true,
                    source_is_confined: true,
                    digest_verified: true,
                },
                catalog_evidence: Some(evidence),
            },
        })
    }

    pub fn resolve(
        &self,
        library: AllowlistedFootprint,
    ) -> Result<ResolvedFootprint, FootprintBackendError> {
        self.resolve_identifier(&library.identifier())
    }

    fn resolve_legacy(
        &self,
        library: AllowlistedFootprint,
    ) -> Result<ResolvedFootprint, FootprintBackendError> {
        let root = match library {
            AllowlistedFootprint::OfficialLed1210 | AllowlistedFootprint::OfficialResistor0603 => {
                &self.official_library
            }
            AllowlistedFootprint::ProjectTypeC => &self.project_library,
        };
        let source_path = root.join(library.source_subpath());
        let source_path = fs::canonicalize(&source_path)
            .map_err(|_| FootprintBackendError::SourceMissing(source_path.clone()))?;
        if !source_path.starts_with(root) || !source_path.is_file() {
            return Err(FootprintBackendError::SourceMissing(source_path));
        }
        if fs::symlink_metadata(root.join(library.source_subpath()))
            .map_err(|_| FootprintBackendError::SourceMissing(source_path.clone()))?
            .file_type()
            .is_symlink()
        {
            return Err(FootprintBackendError::PathOutsideProject);
        }
        let (source_size, digest) =
            read_source_digest(&source_path).map_err(FootprintBackendError::SourceResolution)?;
        let source_digest = RevisionDigest::new(digest).expect("sha256");
        Ok(ResolvedFootprint {
            identifier: library.identifier(),
            source_path: source_path.clone(),
            provenance: LibraryProvenance {
                source_kind: match library {
                    AllowlistedFootprint::OfficialLed1210
                    | AllowlistedFootprint::OfficialResistor0603 => {
                        FootprintSourceKind::OfficialLibrary
                    }
                    AllowlistedFootprint::ProjectTypeC => FootprintSourceKind::ProjectLibrary,
                },
                registry_root: root.clone(),
                source_path,
                source_size,
                source_digest,
                catalog_evidence: None,
                resolution_evidence: ResolutionEvidence {
                    method: "closed_allowlist_canonical_path",
                    identifier: library.identifier(),
                    source_exists: true,
                    source_is_confined: true,
                    digest_verified: true,
                },
            },
        })
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DisposableFootprintContext {
    project_root: PathBuf,
    board_path: PathBuf,
}

impl DisposableFootprintContext {
    pub fn new(project_root: &Path, board_path: &Path) -> Result<Self, FootprintBackendError> {
        let project_root = canonical_directory(project_root)?;
        let board_path =
            fs::canonicalize(board_path).map_err(|_| FootprintBackendError::PathOutsideProject)?;
        if !board_path.is_file() || !board_path.starts_with(&project_root) {
            return Err(FootprintBackendError::PathOutsideProject);
        }
        Ok(Self {
            project_root,
            board_path,
        })
    }

    pub fn validate_before_place(&self) -> Result<(), FootprintBackendError> {
        if pcb_lock_path(&self.board_path).is_file() {
            return Err(FootprintBackendError::EditorLockPresent);
        }
        Ok(())
    }

    pub fn project_root(&self) -> &Path {
        &self.project_root
    }

    pub fn board_path(&self) -> &Path {
        &self.board_path
    }

    pub fn prepare(
        &self,
        registry: &FootprintRegistry,
        library: AllowlistedFootprint,
        placement: AllowlistedFootprintPlacement,
    ) -> Result<PreparedFootprintPlacement, FootprintBackendError> {
        self.validate_before_place()?;
        validate_placement(&placement)?;
        if placement.library_identifier != library.identifier() {
            return Err(FootprintBackendError::UnsupportedLibrary);
        }
        self.prepare_identifier(registry, placement)
    }

    pub fn prepare_identifier(
        &self,
        registry: &FootprintRegistry,
        placement: AllowlistedFootprintPlacement,
    ) -> Result<PreparedFootprintPlacement, FootprintBackendError> {
        self.validate_before_place()?;
        validate_placement(&placement)?;
        let resolved = registry.resolve_identifier(&placement.library_identifier)?;
        Ok(PreparedFootprintPlacement {
            context: self.clone(),
            resolved,
            placement,
        })
    }
}

fn is_uuid(value: &str) -> bool {
    value.len() == 36
        && value.as_bytes().iter().enumerate().all(|(index, byte)| {
            if matches!(index, 8 | 13 | 18 | 23) {
                *byte == b'-'
            } else {
                byte.is_ascii_hexdigit()
            }
        })
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FootprintPostcondition {
    pub footprint_uuid: String,
    pub library_identifier: FootprintLibraryIdentifier,
    pub reference: String,
    pub value: String,
    pub position: kicad_model::Coordinate,
    pub pad_count: usize,
    pub pad_numbers: Vec<String>,
    pub saved: bool,
    pub reopened: bool,
    pub source_digest: RevisionDigest,
    pub source_size: u64,
}

impl FootprintPostcondition {
    pub fn matches(&self, request: &PreparedFootprintPlacement) -> bool {
        let placement = request.placement();
        let provenance = &request.resolved().provenance;
        is_uuid(&self.footprint_uuid)
            && self.library_identifier == placement.library_identifier
            && self.reference == placement.reference
            && self.value == placement.value
            && self.position == placement.position
            && self.pad_count == self.pad_numbers.len()
            && self.saved
            && self.reopened
            && (1..=kicad_model::MAX_FOOTPRINT_PADS).contains(&self.pad_count)
            && self.source_digest == provenance.source_digest
            && self.source_size == provenance.source_size
    }
}

struct FootprintHelperPostcondition {
    library_identifier: FootprintLibraryIdentifier,
    reference: String,
    value: String,
    position: kicad_model::Coordinate,
    pad_count: usize,
    pad_numbers: Vec<String>,
    saved: bool,
    reopened: bool,
    source_digest: RevisionDigest,
    source_size: u64,
}

impl FootprintHelperPostcondition {
    fn from_json(value: serde_json::Value) -> Result<Self, FootprintBackendError> {
        let response = value
            .as_object()
            .ok_or(FootprintBackendError::RunnerProtocol(
                "response is not an object",
            ))?;
        if response.len() != 1 || !response.contains_key("postcondition") {
            return Err(FootprintBackendError::RunnerProtocol(
                "response fields are not exact",
            ));
        }
        let postcondition =
            response
                .get("postcondition")
                .cloned()
                .ok_or(FootprintBackendError::RunnerProtocol(
                    "missing postcondition",
                ))?;
        let object = postcondition
            .as_object()
            .ok_or(FootprintBackendError::RunnerProtocol(
                "postcondition is not an object",
            ))?;
        const REQUIRED_FIELDS: &[&str] = &[
            "library_identifier",
            "reference",
            "value",
            "position",
            "pad_count",
            "pad_numbers",
            "saved",
            "reopened",
            "source_digest",
            "source_size",
        ];
        if object.len() != REQUIRED_FIELDS.len()
            || REQUIRED_FIELDS
                .iter()
                .any(|field| !object.contains_key(*field))
        {
            return Err(FootprintBackendError::RunnerProtocol(
                "postcondition fields are not exact",
            ));
        }
        let string = |name: &'static str| {
            object
                .get(name)
                .and_then(serde_json::Value::as_str)
                .ok_or(FootprintBackendError::RunnerProtocol(name))
        };
        let position = object
            .get("position")
            .and_then(serde_json::Value::as_object)
            .ok_or(FootprintBackendError::RunnerProtocol("position"))?;
        let coordinate = |name: &'static str| {
            position
                .get(name)
                .and_then(serde_json::Value::as_i64)
                .ok_or(FootprintBackendError::RunnerProtocol(name))
        };
        if position.len() != 4
            || ["x", "y", "unit", "space"]
                .iter()
                .any(|field| !position.contains_key(*field))
        {
            return Err(FootprintBackendError::RunnerProtocol(
                "position fields are not exact",
            ));
        }
        let unit = position
            .get("unit")
            .and_then(serde_json::Value::as_str)
            .ok_or(FootprintBackendError::RunnerProtocol("unit"))?;
        let space = position
            .get("space")
            .and_then(serde_json::Value::as_str)
            .ok_or(FootprintBackendError::RunnerProtocol("space"))?;
        if unit != "Nanometres" || space != "Board" {
            return Err(FootprintBackendError::RunnerProtocol("position units"));
        }
        let identifier = object
            .get("library_identifier")
            .and_then(serde_json::Value::as_object)
            .ok_or(FootprintBackendError::RunnerProtocol("library_identifier"))?;
        if identifier.len() != 2
            || !identifier.contains_key("nickname")
            || !identifier.contains_key("entry_name")
        {
            return Err(FootprintBackendError::RunnerProtocol(
                "library identifier fields are not exact",
            ));
        }
        let library_identifier = FootprintLibraryIdentifier {
            nickname: identifier
                .get("nickname")
                .and_then(serde_json::Value::as_str)
                .ok_or(FootprintBackendError::RunnerProtocol("nickname"))?
                .to_owned(),
            entry_name: identifier
                .get("entry_name")
                .and_then(serde_json::Value::as_str)
                .ok_or(FootprintBackendError::RunnerProtocol("entry_name"))?
                .to_owned(),
        };
        let pad_numbers = object
            .get("pad_numbers")
            .and_then(serde_json::Value::as_array)
            .ok_or(FootprintBackendError::RunnerProtocol("pad_numbers"))?
            .iter()
            .map(|value| {
                value
                    .as_str()
                    .map(str::to_owned)
                    .ok_or(FootprintBackendError::RunnerProtocol("pad number"))
            })
            .collect::<Result<Vec<_>, _>>()?;
        let pad_count = object
            .get("pad_count")
            .and_then(serde_json::Value::as_u64)
            .and_then(|value| usize::try_from(value).ok())
            .ok_or(FootprintBackendError::RunnerProtocol("pad_count"))?;
        let boolean = |name: &'static str| {
            object
                .get(name)
                .and_then(serde_json::Value::as_bool)
                .ok_or(FootprintBackendError::RunnerProtocol(name))
        };
        let source_digest = RevisionDigest::new(string("source_digest")?.to_owned())
            .map_err(|_| FootprintBackendError::RunnerProtocol("source_digest"))?;
        let source_size = object
            .get("source_size")
            .and_then(serde_json::Value::as_u64)
            .ok_or(FootprintBackendError::RunnerProtocol("source_size"))?;
        Ok(Self {
            library_identifier,
            reference: string("reference")?.to_owned(),
            value: string("value")?.to_owned(),
            position: kicad_model::Coordinate {
                x: coordinate("x")?,
                y: coordinate("y")?,
                unit: kicad_model::Unit::Nanometres,
                space: kicad_model::DocumentSpace::Board,
            },
            pad_count,
            pad_numbers,
            saved: boolean("saved")?,
            reopened: boolean("reopened")?,
            source_digest,
            source_size,
        })
    }
}

#[allow(dead_code)]
pub(crate) trait FootprintIpc {
    fn capture_templates(&mut self, board: &str) -> Result<Vec<FootprintInstanceTemplate>, String>;
    fn create_footprint(
        &mut self,
        board: &str,
        template: &FootprintInstanceTemplate,
        placement: &AllowlistedFootprintPlacement,
    ) -> Result<CreatedFootprintInstanceReceipt, String>;
    fn begin_commit(&mut self) -> Result<String, String>;
    fn commit(&mut self, commit: &str, message: &str) -> Result<(), String>;
    fn drop_commit(&mut self, commit: &str) -> Result<(), String>;
    fn save(&mut self, board: &str) -> Result<(), String>;
    fn footprints(&mut self, board: &str) -> Result<Vec<ObservedFootprintInstance>, String>;
    fn pads(&mut self, board: &str, footprint: &str) -> Result<Vec<ObservedPad>, String>;
}

impl<T: KiCadTransport> FootprintIpc for KiCadClient<T> {
    fn capture_templates(&mut self, board: &str) -> Result<Vec<FootprintInstanceTemplate>, String> {
        self.capture_footprint_instance_templates(board)
            .map_err(|error| error.to_string())
    }

    fn create_footprint(
        &mut self,
        board: &str,
        template: &FootprintInstanceTemplate,
        placement: &AllowlistedFootprintPlacement,
    ) -> Result<CreatedFootprintInstanceReceipt, String> {
        self.create_footprint_instance(
            board,
            template,
            placement.position.x,
            placement.position.y,
            &placement.reference,
            &placement.value,
        )
        .map_err(|error| error.to_string())
    }

    fn begin_commit(&mut self) -> Result<String, String> {
        KiCadClient::begin_commit(self)
            .map(|commit| commit.as_str().to_owned())
            .map_err(|error| error.to_string())
    }

    fn commit(&mut self, commit: &str, message: &str) -> Result<(), String> {
        let commit = kicad_ipc::CommitId::new(commit).map_err(|error| error.to_string())?;
        self.commit_changes(&commit, message)
            .map_err(|error| error.to_string())
    }

    fn drop_commit(&mut self, commit: &str) -> Result<(), String> {
        let commit = kicad_ipc::CommitId::new(commit).map_err(|error| error.to_string())?;
        self.drop_commit(&commit).map_err(|error| error.to_string())
    }

    fn save(&mut self, board: &str) -> Result<(), String> {
        self.save_board(board).map_err(|error| error.to_string())
    }

    fn footprints(&mut self, board: &str) -> Result<Vec<ObservedFootprintInstance>, String> {
        self.get_footprint_instances(board)
            .map_err(|error| error.to_string())
    }

    fn pads(&mut self, board: &str, footprint: &str) -> Result<Vec<ObservedPad>, String> {
        self.get_footprint_pads(board, footprint)
            .map_err(|error| error.to_string())
    }
}

#[allow(dead_code)]
pub(crate) struct DisposableIpcFootprintExecutor;

#[allow(dead_code)]
impl DisposableIpcFootprintExecutor {
    pub fn place(
        launcher: &mut OwnedPcbnewLauncher,
        request: &PreparedFootprintPlacement,
        message: &str,
    ) -> Result<FootprintPlacementReceipt, FootprintBackendError> {
        request.context().validate_before_place()?;
        verify_source_provenance(request)?;
        let shadow = launcher.current_shadow().map_err(lifecycle_error)?;
        if shadow.board_path() != request.context().board_path()
            || shadow.root() != request.context().project_root()
        {
            return Err(FootprintBackendError::Lifecycle(
                "prepared request is not bound to the owned shadow".into(),
            ));
        }
        let identity = launcher
            .current_identity()
            .map_err(lifecycle_error)?
            .clone();
        let mut ipc = launcher.connect_owned().map_err(lifecycle_error)?;
        let templates = match FootprintIpc::capture_templates(&mut ipc, &identity.board_filename) {
            Ok(templates) => templates,
            Err(error) => {
                return abort_footprint_transaction(
                    ipc,
                    None,
                    launcher,
                    identity.pid,
                    FootprintBackendError::Ipc(error),
                );
            }
        };
        let template = match templates.into_iter().find(|template| {
            let metadata = template.metadata();
            metadata.library_identifier.nickname == request.placement().library_identifier.nickname
                && metadata.library_identifier.entry_name
                    == request.placement().library_identifier.entry_name
        }) {
            Some(template) => template,
            None => {
                return abort_footprint_transaction(
                    ipc,
                    None,
                    launcher,
                    identity.pid,
                    FootprintBackendError::TemplateNotFound,
                );
            }
        };
        let commit = match FootprintIpc::begin_commit(&mut ipc) {
            Ok(commit) => commit,
            Err(error) => {
                return abort_footprint_transaction(
                    ipc,
                    None,
                    launcher,
                    identity.pid,
                    FootprintBackendError::Ipc(error.to_string()),
                );
            }
        };
        let receipt = match FootprintIpc::create_footprint(
            &mut ipc,
            &identity.board_filename,
            &template,
            request.placement(),
        ) {
            Ok(receipt) => receipt,
            Err(error) => {
                return abort_footprint_transaction(
                    ipc,
                    Some(commit),
                    launcher,
                    identity.pid,
                    FootprintBackendError::Ipc(error.to_string()),
                );
            }
        };
        if let Err(error) = FootprintIpc::commit(&mut ipc, &commit, message) {
            return abort_footprint_transaction(
                ipc,
                Some(commit),
                launcher,
                identity.pid,
                FootprintBackendError::Ipc(error),
            );
        }
        if let Err(error) = FootprintIpc::save(&mut ipc, &identity.board_filename) {
            return abort_footprint_transaction(
                ipc,
                None,
                launcher,
                identity.pid,
                FootprintBackendError::Ipc(error),
            );
        }
        let pid = identity.pid;
        drop(ipc);
        launcher.close_owned(pid).map_err(lifecycle_error)?;
        let reopened = launcher.reopen().map_err(lifecycle_error)?.clone();
        let mut reopened_ipc = launcher.connect_owned().map_err(lifecycle_error)?;
        let observed = match FootprintIpc::footprints(&mut reopened_ipc, &reopened.board_filename) {
            Ok(observed) => observed,
            Err(error) => {
                return abort_reopened_footprint(
                    reopened_ipc,
                    launcher,
                    reopened.pid,
                    FootprintBackendError::Ipc(error),
                );
            }
        };
        let matching: Vec<_> = observed
            .into_iter()
            .filter(|footprint| {
                footprint.source_uuid == receipt.source_uuid
                    && footprint.library_identifier.nickname
                        == request.placement().library_identifier.nickname
                    && footprint.library_identifier.entry_name
                        == request.placement().library_identifier.entry_name
                    && footprint.reference == request.placement().reference
                    && footprint.value == request.placement().value
                    && footprint.position_x_nm == request.placement().position.x
                    && footprint.position_y_nm == request.placement().position.y
            })
            .collect();
        let footprint = match matching.len() {
            1 => matching.into_iter().next().expect("length checked"),
            _ => {
                return abort_reopened_footprint(
                    reopened_ipc,
                    launcher,
                    reopened.pid,
                    FootprintBackendError::PostconditionMismatch,
                );
            }
        };
        let pads = match FootprintIpc::pads(
            &mut reopened_ipc,
            &reopened.board_filename,
            &footprint.source_uuid,
        ) {
            Ok(pads) if !pads.is_empty() => pads,
            Ok(_) => {
                return abort_reopened_footprint(
                    reopened_ipc,
                    launcher,
                    reopened.pid,
                    FootprintBackendError::PostconditionMismatch,
                );
            }
            Err(error) => {
                return abort_reopened_footprint(
                    reopened_ipc,
                    launcher,
                    reopened.pid,
                    FootprintBackendError::Ipc(error),
                );
            }
        };
        let postcondition = FootprintPostcondition {
            footprint_uuid: footprint.source_uuid,
            library_identifier: request.placement().library_identifier.clone(),
            reference: footprint.reference,
            value: footprint.value,
            position: request.placement().position,
            pad_count: pads.len(),
            pad_numbers: pads.into_iter().map(|pad| pad.number).collect(),
            saved: true,
            reopened: launcher.is_reopened(),
            source_digest: request.resolved().provenance.source_digest.clone(),
            source_size: request.resolved().provenance.source_size,
        };
        if !postcondition.matches(request) {
            return abort_reopened_footprint(
                reopened_ipc,
                launcher,
                reopened.pid,
                FootprintBackendError::PostconditionMismatch,
            );
        }
        let receipt = FootprintPlacementReceipt {
            resolved: request.resolved().clone(),
            placement: request.placement().clone(),
            postcondition,
        };
        launcher
            .close_owned(reopened.pid)
            .map_err(lifecycle_error)?;
        launcher.cleanup().map_err(lifecycle_error)?;
        Ok(receipt)
    }
}

const MAX_HELPER_OUTPUT_BYTES: usize = 1024 * 1024;
const HELPER_TIMEOUT: Duration = Duration::from_secs(30);
const FIXED_PYTHON_PATH: &str =
    "/Applications/KiCad/KiCad.app/Contents/Frameworks/Python.framework/Versions/3.9/bin/python3.9";
const FIXED_HELPER_SOURCE: &str = include_str!("../helpers/footprint_helper.py");

#[derive(Clone, Debug)]
pub struct SourceFootprintTemplate {
    pub resolved: ResolvedFootprint,
    pub placement: AllowlistedFootprintPlacement,
    pub template: FootprintInstanceTemplate,
    pub observed: crate::ObservedFootprintWithPads,
}

struct TemplateScratch(PathBuf);

impl Drop for TemplateScratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

/// Capture a source-derived native definition while retaining the owned editor process.
pub fn capture_source_footprint_template(
    launcher: &mut OwnedPcbnewLauncher,
    registry: &FootprintRegistry,
    placement: AllowlistedFootprintPlacement,
) -> Result<SourceFootprintTemplate, FootprintBackendError> {
    validate_placement(&placement)?;
    let resolved = registry.resolve_identifier(&placement.library_identifier)?;
    let shadow = launcher.current_shadow().map_err(lifecycle_error)?.clone();
    let identity = launcher
        .current_identity()
        .map_err(lifecycle_error)?
        .clone();
    let mut ipc = launcher.connect_owned().map_err(lifecycle_error)?;
    let before = ipc
        .get_footprint_instances(&identity.board_filename)
        .map_err(|e| FootprintBackendError::Ipc(e.to_string()))?;
    if before
        .iter()
        .any(|existing| existing.reference == placement.reference)
    {
        return Err(FootprintBackendError::InvalidPlacement(
            "footprint reference already exists",
        ));
    }
    ipc.save_board(&identity.board_filename)
        .map_err(|e| FootprintBackendError::Ipc(e.to_string()))?;
    drop(ipc);
    let original = fs::read(shadow.board_path())
        .map_err(|e| FootprintBackendError::RunnerProcess(e.to_string()))?;
    if original.len() > 128 * 1024 * 1024 {
        return Err(FootprintBackendError::InvalidPlacement(
            "template board exceeds size limit",
        ));
    }
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(|e| FootprintBackendError::RunnerProcess(e.to_string()))?
        .as_nanos();
    let root = shadow
        .root()
        .join(format!("footprint-template-{}-{now}", std::process::id()));
    fs::create_dir(&root).map_err(|e| FootprintBackendError::RunnerProcess(e.to_string()))?;
    let scratch = TemplateScratch(root);
    let staged_board = scratch.0.join("template.kicad_pcb");
    fs::write(&staged_board, &original)
        .map_err(|e| FootprintBackendError::RunnerProcess(e.to_string()))?;
    fs::copy(
        shadow.project_path(),
        staged_board.with_extension("kicad_pro"),
    )
    .map_err(|e| FootprintBackendError::RunnerProcess(e.to_string()))?;
    let request = PreparedFootprintPlacement {
        context: DisposableFootprintContext::new(&scratch.0, &staged_board)?,
        resolved: resolved.clone(),
        placement: placement.clone(),
    };
    request.context.validate_before_place()?;
    verify_source_provenance(&request)?;
    let helper = run_fixed_python_helper(&request)?;
    verify_source_provenance(&request)?;
    let staged_bytes =
        fs::read(&staged_board).map_err(|e| FootprintBackendError::RunnerProcess(e.to_string()))?;
    let result = (|| {
        fs::write(shadow.board_path(), staged_bytes)
            .map_err(|e| FootprintBackendError::RunnerProcess(e.to_string()))?;
        launcher.reload_owned().map_err(lifecycle_error)?;
        let mut ipc = launcher.connect_owned().map_err(lifecycle_error)?;
        let after = ipc
            .get_footprint_instances(&identity.board_filename)
            .map_err(|e| FootprintBackendError::Ipc(e.to_string()))?;
        let uuid = unique_new_footprint_uuid(&before, &after)?;
        let footprint = after
            .into_iter()
            .find(|item| item.source_uuid == uuid)
            .ok_or(FootprintBackendError::UuidSetMismatch)?;
        if footprint.library_identifier.nickname != placement.library_identifier.nickname
            || footprint.library_identifier.entry_name != placement.library_identifier.entry_name
            || footprint.reference != placement.reference
            || footprint.value != placement.value
            || footprint.position_x_nm != placement.position.x
            || footprint.position_y_nm != placement.position.y
            || footprint.side != kicad_ipc::FootprintSide::Front
            || footprint.orientation_mdeg != 0
        {
            return Err(FootprintBackendError::PostconditionMismatch);
        }
        let pads = ipc
            .get_footprint_definition_pads(&identity.board_filename, &uuid)
            .map_err(|e| FootprintBackendError::Ipc(e.to_string()))?;
        let mut numbers = pads
            .iter()
            .map(|pad| pad.number.clone())
            .collect::<Vec<_>>();
        numbers.sort();
        if numbers != helper.pad_numbers {
            return Err(FootprintBackendError::PostconditionMismatch);
        }
        let templates = ipc
            .capture_footprint_instance_templates(&identity.board_filename)
            .map_err(|e| FootprintBackendError::Ipc(e.to_string()))?;
        let mut matches = templates
            .into_iter()
            .filter(|template| template.metadata().source_uuid == uuid);
        let template = matches
            .next()
            .ok_or(FootprintBackendError::TemplateNotFound)?;
        if matches.next().is_some() {
            return Err(FootprintBackendError::TemplateNotFound);
        }
        verify_source_provenance(&request)?;
        Ok(SourceFootprintTemplate {
            resolved,
            placement,
            template,
            observed: crate::ObservedFootprintWithPads { footprint, pads },
        })
    })();
    // Restoration is mandatory on both success and failure; only the owned shadow is rewritten.
    fs::write(shadow.board_path(), &original).map_err(|e| {
        FootprintBackendError::Lifecycle(format!("template baseline restoration failed: {e}"))
    })?;
    launcher.reload_owned().map_err(lifecycle_error)?;
    let mut ipc = launcher.connect_owned().map_err(lifecycle_error)?;
    let restored = ipc
        .get_footprint_instances(&identity.board_filename)
        .map_err(|e| FootprintBackendError::Ipc(e.to_string()))?;
    if restored != before
        || launcher.current_identity().map_err(lifecycle_error)?.pid != identity.pid
    {
        return Err(FootprintBackendError::Lifecycle(
            "template capture did not restore the owned baseline".into(),
        ));
    }
    result
}

fn unique_new_footprint_uuid(
    before: &[ObservedFootprintInstance],
    after: &[ObservedFootprintInstance],
) -> Result<String, FootprintBackendError> {
    let before_ids = before
        .iter()
        .map(|footprint| footprint.source_uuid.as_str())
        .collect::<HashSet<_>>();
    if before_ids.len() != before.len() {
        return Err(FootprintBackendError::UuidSetMismatch);
    }
    let after_ids = after
        .iter()
        .map(|footprint| footprint.source_uuid.as_str())
        .collect::<HashSet<_>>();
    if after_ids.len() != after.len() {
        return Err(FootprintBackendError::UuidSetMismatch);
    }
    let removed = before_ids.difference(&after_ids).next().is_some();
    let added = after_ids
        .difference(&before_ids)
        .map(|uuid| (*uuid).to_owned())
        .collect::<Vec<_>>();
    if !removed && after.len() == before.len() + 1 && added.len() == 1 {
        Ok(added.into_iter().next().expect("length checked"))
    } else {
        Err(FootprintBackendError::UuidSetMismatch)
    }
}

fn run_fixed_python_helper(
    request: &PreparedFootprintPlacement,
) -> Result<FootprintHelperPostcondition, FootprintBackendError> {
    let input = serde_json::json!({
        "board_path": request.context().board_path(),
        "project_root": request.context().project_root(),
        "source_registry_root": request.resolved().provenance.registry_root,
        "source_path": request.resolved().source_path,
        "library_identifier": request.placement().library_identifier,
        "target_id": request.placement().target_id,
        "position": request.placement().position,
        "reference": request.placement().reference,
        "value": request.placement().value,
        "source_digest": request.resolved().provenance.source_digest,
        "source_size": request.resolved().provenance.source_size,
    });
    let deadline = std::time::Instant::now() + HELPER_TIMEOUT;
    let mut child = Command::new(FIXED_PYTHON_PATH)
        .arg("-c")
        .arg(FIXED_HELPER_SOURCE)
        .arg("--fixed-footprint-placement-v2")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|error| FootprintBackendError::RunnerProcess(error.to_string()))?;
    let mut stdin = match child.stdin.take() {
        Some(stdin) => stdin,
        None => {
            let _ = child.kill();
            let _ = child.wait();
            return Err(FootprintBackendError::RunnerProcess(
                "helper stdin unavailable".to_owned(),
            ));
        }
    };
    let encoded = match serde_json::to_vec(&input) {
        Ok(encoded) => encoded,
        Err(_) => {
            let _ = child.kill();
            let _ = child.wait();
            return Err(FootprintBackendError::RunnerProtocol("request encoding"));
        }
    };
    std::thread::spawn(move || {
        let _ = stdin.write_all(&encoded);
    });
    let stdout = match child.stdout.take() {
        Some(stdout) => stdout,
        None => {
            let _ = child.kill();
            let _ = child.wait();
            return Err(FootprintBackendError::RunnerProcess(
                "helper stdout unavailable".to_owned(),
            ));
        }
    };
    let stderr = match child.stderr.take() {
        Some(stderr) => stderr,
        None => {
            let _ = child.kill();
            let _ = child.wait();
            return Err(FootprintBackendError::RunnerProcess(
                "helper stderr unavailable".to_owned(),
            ));
        }
    };
    let (sender, receiver) = mpsc::channel();
    std::thread::spawn(move || {
        let mut limited = stdout.take((MAX_HELPER_OUTPUT_BYTES + 1) as u64);
        let mut bytes = Vec::new();
        let result = limited.read_to_end(&mut bytes).map(|_| bytes);
        let _ = sender.send(result);
    });
    let (error_sender, error_receiver) = mpsc::channel();
    std::thread::spawn(move || {
        let mut limited = stderr.take((MAX_HELPER_OUTPUT_BYTES + 1) as u64);
        let mut bytes = Vec::new();
        let result = limited.read_to_end(&mut bytes).map(|_| bytes);
        let _ = error_sender.send(result);
    });
    let bytes = match receiver
        .recv_timeout(deadline.saturating_duration_since(std::time::Instant::now()))
    {
        Ok(result) => match result {
            Ok(bytes) => bytes,
            Err(error) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(FootprintBackendError::RunnerProcess(error.to_string()));
            }
        },
        Err(_) => {
            let _ = child.kill();
            let _ = child.wait();
            return Err(FootprintBackendError::RunnerTimeout);
        }
    };
    let stderr = match error_receiver
        .recv_timeout(deadline.saturating_duration_since(std::time::Instant::now()))
    {
        Ok(result) => match result {
            Ok(stderr) => stderr,
            Err(error) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(FootprintBackendError::RunnerProcess(error.to_string()));
            }
        },
        Err(_) => {
            let _ = child.kill();
            let _ = child.wait();
            return Err(FootprintBackendError::RunnerTimeout);
        }
    };
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if std::time::Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(10))
            }
            Ok(None) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(FootprintBackendError::RunnerTimeout);
            }
            Err(error) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(FootprintBackendError::RunnerProcess(error.to_string()));
            }
        }
    };
    if !status.success() {
        let detail = String::from_utf8_lossy(&stderr).trim().to_owned();
        return Err(FootprintBackendError::RunnerProcess(if detail.is_empty() {
            status.to_string()
        } else {
            detail
        }));
    }
    if bytes.len() > MAX_HELPER_OUTPUT_BYTES {
        return Err(FootprintBackendError::RunnerProtocol("response too large"));
    }
    let value: serde_json::Value = serde_json::from_slice(&bytes)
        .map_err(|_| FootprintBackendError::RunnerProtocol("malformed JSON"))?;
    let response = FootprintHelperPostcondition::from_json(value)?;
    let provenance = &request.resolved().provenance;
    if response.source_digest != provenance.source_digest
        || response.source_size != provenance.source_size
        || response.library_identifier != request.placement().library_identifier
        || response.reference != request.placement().reference
        || response.value != request.placement().value
        || response.position != request.placement().position
        || response.pad_count != response.pad_numbers.len()
        || response.pad_count > kicad_model::MAX_FOOTPRINT_PADS
        || !response.saved
        || !response.reopened
    {
        return Err(FootprintBackendError::RunnerProtocol(
            "helper postcondition mismatch",
        ));
    }
    Ok(response)
}

#[allow(dead_code)]
pub(crate) struct DisposablePythonFootprintExecutor;

#[allow(dead_code)]
impl DisposablePythonFootprintExecutor {
    pub fn place(
        launcher: &mut OwnedPcbnewLauncher,
        request: &PreparedFootprintPlacement,
        message: &str,
    ) -> Result<FootprintPlacementReceipt, FootprintBackendError> {
        let mut receipts = Self::place_batch(launcher, std::slice::from_ref(request), message)?;
        Ok(receipts.remove(0))
    }

    pub fn place_batch(
        launcher: &mut OwnedPcbnewLauncher,
        requests: &[PreparedFootprintPlacement],
        message: &str,
    ) -> Result<Vec<FootprintPlacementReceipt>, FootprintBackendError> {
        let receipts = Self::place_batch_preserving_shadow(launcher, requests, message)?;
        launcher.cleanup().map_err(lifecycle_error)?;
        Ok(receipts)
    }

    pub(crate) fn place_batch_preserving_shadow(
        launcher: &mut OwnedPcbnewLauncher,
        requests: &[PreparedFootprintPlacement],
        _message: &str,
    ) -> Result<Vec<FootprintPlacementReceipt>, FootprintBackendError> {
        if requests.is_empty() {
            return Err(FootprintBackendError::InvalidPlacement(
                "footprint batch must not be empty",
            ));
        }
        for request in requests {
            verify_source_provenance(request)?;
        }
        let shadow = launcher.current_shadow().map_err(lifecycle_error)?.clone();
        let first_context = requests[0].context();
        if shadow.board_path() != first_context.board_path()
            || shadow.root() != first_context.project_root()
            || requests.iter().any(|request| {
                request.context().board_path() != shadow.board_path()
                    || request.context().project_root() != shadow.root()
            })
        {
            return Err(FootprintBackendError::Lifecycle(
                "prepared requests are not bound to the owned shadow".into(),
            ));
        }

        let mut open_pid = Some(launcher.current_identity().map_err(lifecycle_error)?.pid);
        let mut receipts = Vec::with_capacity(requests.len());
        for request in requests {
            let identity = launcher
                .current_identity()
                .map_err(lifecycle_error)?
                .clone();
            let mut ipc = match launcher.connect_owned() {
                Ok(ipc) => ipc,
                Err(error) => {
                    return abort_python_transaction(
                        launcher,
                        open_pid,
                        FootprintBackendError::Lifecycle(error.to_string()),
                    );
                }
            };
            let before = match FootprintIpc::footprints(&mut ipc, &identity.board_filename) {
                Ok(observed) => observed,
                Err(error) => {
                    return abort_python_transaction(
                        launcher,
                        open_pid,
                        FootprintBackendError::Ipc(error),
                    );
                }
            };
            drop(ipc);
            if let Err(error) = launcher.close_owned(identity.pid) {
                return abort_python_transaction(
                    launcher,
                    open_pid,
                    FootprintBackendError::Lifecycle(error.to_string()),
                );
            }
            open_pid = None;

            let helper = match run_fixed_python_helper(request) {
                Ok(helper) => helper,
                Err(error) => return abort_python_transaction(launcher, open_pid, error),
            };
            if helper.source_digest != request.resolved().provenance.source_digest
                || helper.source_size != request.resolved().provenance.source_size
                || verify_source_provenance(request).is_err()
            {
                return abort_python_transaction(
                    launcher,
                    open_pid,
                    FootprintBackendError::RunnerProtocol("source provenance changed"),
                );
            }

            let reopened = match launcher.reopen() {
                Ok(identity) => identity,
                Err(error) => {
                    return abort_python_transaction(
                        launcher,
                        open_pid,
                        FootprintBackendError::Lifecycle(error.to_string()),
                    );
                }
            };
            open_pid = Some(reopened.pid);
            let mut reopened_ipc = match launcher.connect_owned() {
                Ok(ipc) => ipc,
                Err(error) => {
                    return abort_python_transaction(
                        launcher,
                        open_pid,
                        FootprintBackendError::Lifecycle(error.to_string()),
                    );
                }
            };
            let after = match FootprintIpc::footprints(&mut reopened_ipc, &reopened.board_filename)
            {
                Ok(observed) => observed,
                Err(error) => {
                    return abort_python_transaction(
                        launcher,
                        open_pid,
                        FootprintBackendError::Ipc(error),
                    );
                }
            };
            let uuid = match unique_new_footprint_uuid(&before, &after) {
                Ok(uuid) => uuid,
                Err(error) => return abort_python_transaction(launcher, open_pid, error),
            };
            let footprint = match after.iter().find(|footprint| footprint.source_uuid == uuid) {
                Some(footprint) => footprint,
                None => {
                    return abort_python_transaction(
                        launcher,
                        open_pid,
                        FootprintBackendError::UuidSetMismatch,
                    );
                }
            };
            if footprint.library_identifier.nickname != helper.library_identifier.nickname
                || footprint.library_identifier.entry_name != helper.library_identifier.entry_name
                || footprint.reference != helper.reference
                || footprint.value != helper.value
                || footprint.position_x_nm != helper.position.x
                || footprint.position_y_nm != helper.position.y
                || footprint.orientation_mdeg != 0
                || footprint.side != kicad_ipc::FootprintSide::Front
            {
                return abort_python_transaction(
                    launcher,
                    open_pid,
                    FootprintBackendError::PostconditionMismatch,
                );
            }
            let mut pads =
                match reopened_ipc.get_footprint_definition_pads(&reopened.board_filename, &uuid) {
                    Ok(pads) => pads,
                    Err(error) => {
                        return abort_python_transaction(
                            launcher,
                            open_pid,
                            FootprintBackendError::Ipc(error.to_string()),
                        );
                    }
                };
            pads.sort_by(|left, right| left.number.cmp(&right.number));
            let pad_numbers: Vec<_> = pads.into_iter().map(|pad| pad.number).collect();
            if pad_numbers != helper.pad_numbers
                || helper.pad_count != pad_numbers.len()
                || pad_numbers.len() > kicad_model::MAX_FOOTPRINT_PADS
            {
                return abort_python_transaction(
                    launcher,
                    open_pid,
                    FootprintBackendError::PostconditionMismatch,
                );
            }
            receipts.push(FootprintPlacementReceipt {
                resolved: request.resolved().clone(),
                placement: request.placement().clone(),
                postcondition: FootprintPostcondition {
                    footprint_uuid: uuid,
                    library_identifier: request.placement().library_identifier.clone(),
                    reference: footprint.reference.clone(),
                    value: footprint.value.clone(),
                    position: request.placement().position,
                    pad_count: pad_numbers.len(),
                    pad_numbers,
                    saved: helper.saved,
                    reopened: launcher.is_reopened() && helper.reopened,
                    source_digest: request.resolved().provenance.source_digest.clone(),
                    source_size: request.resolved().provenance.source_size,
                },
            });
        }
        let pid = open_pid.expect("batch keeps an owned editor open");
        if let Err(error) = launcher.close_owned(pid) {
            return abort_python_transaction(
                launcher,
                Some(pid),
                FootprintBackendError::Lifecycle(error.to_string()),
            );
        }
        Ok(receipts)
    }
}

#[allow(dead_code)]
fn abort_python_transaction(
    launcher: &mut OwnedPcbnewLauncher,
    open_pid: Option<u32>,
    error: FootprintBackendError,
) -> Result<Vec<FootprintPlacementReceipt>, FootprintBackendError> {
    let mut details = error.to_string();
    if let Some(pid) = open_pid
        && let Err(close_error) = launcher.close_owned(pid)
    {
        details.push_str(&format!("; close owned editor failed: {close_error}"));
    }
    if let Err(cleanup_error) = launcher.cleanup() {
        details.push_str(&format!("; cleanup failed: {cleanup_error}"));
    }
    Err(FootprintBackendError::Lifecycle(details))
}

#[allow(dead_code)]
fn abort_reopened_footprint<I: FootprintIpc>(
    ipc: I,
    launcher: &mut OwnedPcbnewLauncher,
    pid: u32,
    error: FootprintBackendError,
) -> Result<FootprintPlacementReceipt, FootprintBackendError> {
    drop(ipc);
    abort_footprint_transaction_without_ipc(launcher, pid, error)
}

#[allow(dead_code)]
fn abort_footprint_transaction_without_ipc(
    launcher: &mut OwnedPcbnewLauncher,
    pid: u32,
    error: FootprintBackendError,
) -> Result<FootprintPlacementReceipt, FootprintBackendError> {
    let mut details = error.to_string();
    if let Err(close_error) = launcher.close_owned(pid) {
        details.push_str(&format!("; close owned editor failed: {close_error}"));
    }
    Err(FootprintBackendError::Lifecycle(details))
}

#[allow(dead_code)]
fn abort_footprint_transaction<I: FootprintIpc>(
    mut ipc: I,
    commit: Option<String>,
    launcher: &mut OwnedPcbnewLauncher,
    pid: u32,
    error: FootprintBackendError,
) -> Result<FootprintPlacementReceipt, FootprintBackendError> {
    let mut details = error.to_string();
    if let Some(commit) = commit
        && let Err(drop_error) = ipc.drop_commit(&commit)
    {
        details.push_str(&format!("; drop commit failed: {drop_error}"));
    }
    drop(ipc);
    if let Err(close_error) = launcher.close_owned(pid) {
        details.push_str(&format!("; close owned editor failed: {close_error}"));
    }
    Err(FootprintBackendError::Lifecycle(details))
}

#[allow(dead_code)]
fn verify_source_provenance(
    request: &PreparedFootprintPlacement,
) -> Result<(), FootprintBackendError> {
    let source = &request.resolved().provenance;
    let path = &source.source_path;
    if fs::symlink_metadata(path)
        .map_err(|_| FootprintBackendError::SourceMissing(path.clone()))?
        .file_type()
        .is_symlink()
        || !fs::canonicalize(path)
            .map_err(|_| FootprintBackendError::SourceMissing(path.clone()))?
            .starts_with(&source.registry_root)
    {
        return Err(FootprintBackendError::PathOutsideProject);
    }
    let (size, digest) =
        read_source_digest(path).map_err(FootprintBackendError::SourceResolution)?;
    if size != source.source_size || digest != source.source_digest.as_str() {
        return Err(FootprintBackendError::RunnerProtocol(
            "footprint source changed",
        ));
    }
    Ok(())
}

#[allow(dead_code)]
fn lifecycle_error(error: LifecycleError) -> FootprintBackendError {
    FootprintBackendError::Lifecycle(error.to_string())
}

pub trait DisposableFootprintRunner: Send + Sync {
    fn place(
        &self,
        request: &PreparedFootprintPlacement,
    ) -> Result<FootprintPlacementReceipt, FootprintBackendError>;
}

#[derive(Clone, Copy, Debug, Default)]
pub struct DisabledFootprintRunner;

impl DisposableFootprintRunner for DisabledFootprintRunner {
    fn place(
        &self,
        _: &PreparedFootprintPlacement,
    ) -> Result<FootprintPlacementReceipt, FootprintBackendError> {
        Err(FootprintBackendError::UnsupportedRunner)
    }
}

pub fn validate_placement(
    placement: &AllowlistedFootprintPlacement,
) -> Result<(), FootprintBackendError> {
    placement.validate().map_err(|_| {
        FootprintBackendError::InvalidPlacement("invalid board-space footprint placement")
    })?;
    if placement.position.x.unsigned_abs() > MAX_PLACEMENT_COORDINATE_NM as u64
        || placement.position.y.unsigned_abs() > MAX_PLACEMENT_COORDINATE_NM as u64
    {
        return Err(FootprintBackendError::InvalidPlacement(
            "footprint position bounds exceeded",
        ));
    }
    Ok(())
}

fn canonical_directory(path: &Path) -> Result<PathBuf, FootprintBackendError> {
    let canonical = fs::canonicalize(path).map_err(|_| FootprintBackendError::InvalidRoot)?;
    if canonical.is_dir() {
        Ok(canonical)
    } else {
        Err(FootprintBackendError::InvalidRoot)
    }
}

fn pcb_lock_path(board: &Path) -> PathBuf {
    let name = board
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or_default();
    board.with_file_name(format!("~{name}.lck"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{BoardGraphicsExecutor, verify_native_pcb_batch};
    use kicad_checks::{CheckResult, DrcPolicy, compare_drc, run_pcb_drc};
    use kicad_ipc::{BoardGraphicLayer, BoardGraphicSegment, CreateThroughVia, CreateTrack};
    use kicad_model::{Coordinate, DocumentSpace, StableId, Unit};
    use std::time::{Duration, SystemTime, UNIX_EPOCH};

    struct FixtureCleanup(PathBuf);

    impl Drop for FixtureCleanup {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn fixture() -> (PathBuf, PathBuf, PathBuf, PathBuf) {
        static SEQUENCE: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let sequence = SEQUENCE.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let root = std::env::temp_dir().join(format!(
            "auto-kicad-footprint-{}-{sequence}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir(&root).unwrap();
        let official = root.join("official");
        let project = root.join("project");
        fs::create_dir_all(&official).unwrap();
        fs::create_dir_all(&project).unwrap();
        fs::write(
            official.join(AllowlistedFootprint::OfficialLed1210.source_subpath()),
            "led",
        )
        .unwrap();
        fs::write(
            project.join(AllowlistedFootprint::ProjectTypeC.source_subpath()),
            "type-c",
        )
        .unwrap();
        let board = root.join("shadow.kicad_pcb");
        fs::write(&board, "(kicad_pcb)").unwrap();
        (root, official, project, board)
    }

    fn observed(uuid: &str) -> ObservedFootprintInstance {
        ObservedFootprintInstance {
            source_uuid: uuid.to_owned(),
            position_x_nm: 0,
            position_y_nm: 0,
            orientation_mdeg: 0,
            side: kicad_ipc::FootprintSide::Front,
            lock_state: kicad_ipc::ObservedLockState::Unlocked,
            library_identifier: kicad_ipc::ObservedFootprintLibraryIdentifier {
                nickname: "LED_SMD".to_owned(),
                entry_name: "LED_1210_3225Metric_Pad1.42x2.65mm_HandSolder".to_owned(),
            },
            reference: "P1".to_owned(),
            value: "LED".to_owned(),
            symbol_sheet_path: Vec::new(),
        }
    }

    #[test]
    fn template_capture_requires_an_owned_editor_before_helper_or_board_writes() {
        let (root, official, project, board) = fixture();
        let _cleanup = FixtureCleanup(root.clone());
        fs::write(board.with_extension("kicad_pro"), "{}").unwrap();
        let registry = FootprintRegistry::new(&official, &project).unwrap();
        let mut launcher = OwnedPcbnewLauncher::new(&root.join("preview")).unwrap();
        let shadow = launcher.create_shadow(&board).unwrap();
        let before = fs::read(shadow.board_path()).unwrap();
        assert!(
            capture_source_footprint_template(&mut launcher, &registry, placement(10)).is_err()
        );
        assert_eq!(fs::read(shadow.board_path()).unwrap(), before);
        assert!(!launcher.editor_is_open());
        assert_eq!(fs::read_dir(shadow.root()).unwrap().count(), 2);
    }

    #[test]
    fn uuid_difference_requires_exactly_one_unique_server_identity() {
        let before = vec![observed("old")];
        assert_eq!(
            unique_new_footprint_uuid(&before, &[observed("old"), observed("new")]).unwrap(),
            "new"
        );
        assert!(matches!(
            unique_new_footprint_uuid(&before, &[observed("old")]),
            Err(FootprintBackendError::UuidSetMismatch)
        ));
        assert!(matches!(
            unique_new_footprint_uuid(
                &before,
                &[observed("old"), observed("new"), observed("newer")]
            ),
            Err(FootprintBackendError::UuidSetMismatch)
        ));
        assert!(matches!(
            unique_new_footprint_uuid(
                &before,
                &[observed("old"), observed("new"), observed("new")]
            ),
            Err(FootprintBackendError::UuidSetMismatch)
        ));
        assert!(matches!(
            unique_new_footprint_uuid(
                &[observed("old"), observed("kept")],
                &[observed("kept"), observed("new")]
            ),
            Err(FootprintBackendError::UuidSetMismatch)
        ));
    }

    fn placement(x: i64) -> AllowlistedFootprintPlacement {
        AllowlistedFootprintPlacement::new(
            StableId::new("target").unwrap(),
            AllowlistedFootprint::OfficialLed1210.identifier(),
            Coordinate {
                x,
                y: 0,
                unit: Unit::Nanometres,
                space: DocumentSpace::Board,
            },
            "P1",
            "LED",
        )
        .unwrap()
    }

    #[test]
    #[ignore = "disposable KiCad 10.0.5 smoke"]
    fn disposable_pcb_vertical_slice_saves_and_reopens_all_native_items() {
        let live = PathBuf::from("/Users/w-mai/Projects/Hardware/PCBs/xLock/xLock.kicad_pcb");
        let digest = |path: &Path| {
            let mut hasher = Sha256::new();
            hasher.update(fs::read(path).unwrap());
            hex::encode(hasher.finalize())
        };
        let live_digest = digest(&live);
        let source_root = std::env::temp_dir().join(format!(
            "auto-kicad-source-{}",
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let server_root = std::env::temp_dir().join(format!(
            "auto-kicad-server-{}",
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&source_root).unwrap();
        let source_board = source_root.join("xLock.kicad_pcb");
        fs::copy(&live, &source_board).unwrap();
        fs::copy(
            live.with_extension("kicad_pro"),
            source_board.with_extension("kicad_pro"),
        )
        .unwrap();
        let official = PathBuf::from(
            "/Applications/KiCad/KiCad.app/Contents/SharedSupport/footprints/LED_SMD.pretty",
        );
        let project = PathBuf::from("/Users/w-mai/Projects/Hardware/Libs/Type-C.pretty");
        let mut launcher = OwnedPcbnewLauncher::new(&server_root).unwrap();
        let shadow = launcher.create_shadow(&source_board).unwrap();
        let context = DisposableFootprintContext::new(shadow.root(), shadow.board_path()).unwrap();
        let drc_policy = DrcPolicy {
            allowed_root: shadow.root().to_path_buf(),
            timeout: Duration::from_secs(60),
        };
        let baseline = match run_pcb_drc(shadow.board_path(), &drc_policy) {
            CheckResult::Completed(report) => report,
            CheckResult::Failed(error) => panic!("baseline DRC failed: {error:?}"),
        };
        let registry = FootprintRegistry::new(&official, &project).unwrap();
        let make = |library, target, x, reference, value| {
            context
                .prepare(
                    &registry,
                    library,
                    AllowlistedFootprintPlacement::new(
                        StableId::new(target).unwrap(),
                        library.identifier(),
                        Coordinate {
                            x,
                            y: 100_000_000,
                            unit: Unit::Nanometres,
                            space: DocumentSpace::Board,
                        },
                        reference,
                        value,
                    )
                    .unwrap(),
                )
                .unwrap()
        };
        let first = make(
            AllowlistedFootprint::OfficialLed1210,
            "led-target",
            100_000_000,
            "R_TEST1",
            "LED",
        );
        let second = make(
            AllowlistedFootprint::ProjectTypeC,
            "type-c-target",
            120_000_000,
            "J_TEST1",
            "USB-C",
        );
        launcher.launch(&shadow).unwrap();
        let receipts = DisposablePythonFootprintExecutor::place_batch_preserving_shadow(
            &mut launcher,
            &[first, second],
            "disposable fallback smoke",
        )
        .unwrap();
        assert_eq!(receipts.len(), 2);
        assert_ne!(
            receipts[0].postcondition.footprint_uuid,
            receipts[1].postcondition.footprint_uuid
        );
        assert_eq!(receipts[0].placement.reference, "R_TEST1");
        assert_eq!(receipts[0].placement.position.x, 100_000_000);
        assert_eq!(receipts[0].postcondition.pad_numbers, ["1", "2"]);
        assert_eq!(receipts[1].placement.reference, "J_TEST1");
        assert_eq!(receipts[1].placement.position.x, 120_000_000);
        assert_eq!(receipts[1].postcondition.pad_count, 20);
        assert!(receipts.iter().all(|receipt| {
            is_uuid(&receipt.postcondition.footprint_uuid)
                && receipt.postcondition.matches(&PreparedFootprintPlacement {
                    context: context.clone(),
                    resolved: receipt.resolved.clone(),
                    placement: receipt.placement.clone(),
                })
                && !receipt.postcondition.pad_numbers.is_empty()
        }));

        let native_identity = launcher.reopen().unwrap();
        let mut native_ipc = launcher.connect_owned().unwrap();
        let graphics = [
            BoardGraphicSegment {
                start_x_nm: 80_000_000,
                start_y_nm: 80_000_000,
                end_x_nm: 140_000_000,
                end_y_nm: 80_000_000,
                width_nm: 50_000,
                layer: BoardGraphicLayer::EdgeCuts,
            },
            BoardGraphicSegment {
                start_x_nm: 140_000_000,
                start_y_nm: 80_000_000,
                end_x_nm: 140_000_000,
                end_y_nm: 120_000_000,
                width_nm: 50_000,
                layer: BoardGraphicLayer::EdgeCuts,
            },
            BoardGraphicSegment {
                start_x_nm: 140_000_000,
                start_y_nm: 120_000_000,
                end_x_nm: 80_000_000,
                end_y_nm: 120_000_000,
                width_nm: 50_000,
                layer: BoardGraphicLayer::EdgeCuts,
            },
            BoardGraphicSegment {
                start_x_nm: 80_000_000,
                start_y_nm: 120_000_000,
                end_x_nm: 80_000_000,
                end_y_nm: 80_000_000,
                width_nm: 50_000,
                layer: BoardGraphicLayer::EdgeCuts,
            },
        ];
        let tracks = [CreateTrack {
            start_x_nm: 100_000_000,
            start_y_nm: 100_000_000,
            end_x_nm: 120_000_000,
            end_y_nm: 100_000_000,
            width_nm: 250_000,
            net: None,
        }];
        let vias = [CreateThroughVia {
            position_x_nm: 110_000_000,
            position_y_nm: 100_000_000,
            pad_diameter_nm: 800_000,
            drill_diameter_nm: 400_000,
            net: None,
        }];
        let native_receipt = BoardGraphicsExecutor::default()
            .execute_native_batch(
                &mut native_ipc,
                &native_identity.board_filename,
                &graphics,
                &tracks,
                &vias,
                "disposable PCB vertical slice",
            )
            .unwrap();
        assert_eq!(native_receipt.graphics.len(), 4);
        assert_eq!(native_receipt.tracks.len(), 1);
        assert_eq!(native_receipt.vias.len(), 1);
        drop(native_ipc);
        launcher.close_owned(native_identity.pid).unwrap();

        let verified_identity = launcher.reopen().unwrap();
        let mut verified_ipc = launcher.connect_owned().unwrap();
        verify_native_pcb_batch(
            &mut verified_ipc,
            &verified_identity.board_filename,
            &native_receipt,
        )
        .unwrap();
        let verified_footprints =
            FootprintIpc::footprints(&mut verified_ipc, &verified_identity.board_filename).unwrap();
        for receipt in &receipts {
            let footprint = verified_footprints
                .iter()
                .find(|footprint| footprint.source_uuid == receipt.postcondition.footprint_uuid)
                .expect("placed footprint survives final reopen");
            assert_eq!(
                footprint.library_identifier.nickname,
                receipt.postcondition.library_identifier.nickname
            );
            assert_eq!(
                footprint.library_identifier.entry_name,
                receipt.postcondition.library_identifier.entry_name
            );
            assert_eq!(footprint.reference, receipt.postcondition.reference);
            assert_eq!(footprint.value, receipt.postcondition.value);
            assert_eq!(footprint.position_x_nm, receipt.postcondition.position.x);
            assert_eq!(footprint.position_y_nm, receipt.postcondition.position.y);
            assert_eq!(footprint.orientation_mdeg, 0);
            assert_eq!(footprint.side, kicad_ipc::FootprintSide::Front);
            let mut pads = verified_ipc
                .get_footprint_definition_pads(
                    &verified_identity.board_filename,
                    &receipt.postcondition.footprint_uuid,
                )
                .unwrap()
                .into_iter()
                .map(|pad| pad.number)
                .collect::<Vec<_>>();
            pads.sort();
            assert_eq!(pads, receipt.postcondition.pad_numbers);
        }
        drop(verified_ipc);
        launcher.close_owned(verified_identity.pid).unwrap();

        let after = match run_pcb_drc(shadow.board_path(), &drc_policy) {
            CheckResult::Completed(report) => report,
            CheckResult::Failed(error) => panic!("after DRC failed: {error:?}"),
        };
        let delta = compare_drc(&baseline, &after);
        eprintln!(
            "disposable PCB DRC: baseline={}, after={}, introduced={}, resolved={}",
            baseline.violations.len(),
            after.violations.len(),
            delta.introduced.len(),
            delta.resolved.len()
        );
        launcher.cleanup().unwrap();
        assert!(!shadow.root().exists());
        assert_eq!(digest(&live), live_digest);
        fs::remove_dir_all(source_root).unwrap();
        fs::remove_dir_all(server_root).unwrap();
    }

    #[test]
    fn preparation_failure_cleans_up_shadow() {
        let (root, official, project, board) = fixture();
        let _cleanup = FixtureCleanup(root.clone());
        fs::write(board.with_extension("kicad_pro"), "{}").unwrap();
        let server_root = root.join("server");
        let registry = FootprintRegistry::new(&official, &project).unwrap();
        let mut invalid = placement(0);
        invalid.position.x = MAX_PLACEMENT_COORDINATE_NM + 1;

        assert!(matches!(
            place_allowlisted_footprints(&server_root, &registry, &board, &[invalid]),
            Err(FootprintBackendError::InvalidPlacement(_))
        ));
        assert!(
            fs::read_dir(&server_root).unwrap().next().is_none(),
            "failed preparation must not leave a shadow"
        );
    }

    #[test]
    fn resolves_only_server_allowlisted_sources_and_confines_context() {
        let (root, official, project, board) = fixture();
        let registry = FootprintRegistry::new(&official, &project).unwrap();
        let resolved = registry
            .resolve(AllowlistedFootprint::ProjectTypeC)
            .unwrap();
        assert!(
            resolved
                .source_path
                .starts_with(fs::canonicalize(&project).unwrap())
        );
        assert_eq!(resolved.identifier.entry_name, "USB_C_GCT_USB4085");
        assert_eq!(
            resolved.provenance.source_kind,
            FootprintSourceKind::ProjectLibrary
        );
        assert!(resolved.provenance.resolution_evidence.digest_verified);
        assert_eq!(resolved.provenance.source_size, 6);
        assert_eq!(resolved.provenance.source_path, resolved.source_path);
        assert_eq!(
            resolved.provenance.source_digest.as_str(),
            "e57b2abcae3132d7b6e0295fc6ccadc861593038de6257ab58e980c55d6472ae"
        );
        let context = DisposableFootprintContext::new(&root, &board).unwrap();
        assert_eq!(context.board_path(), fs::canonicalize(&board).unwrap());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn rejects_unmatched_library_identity_before_runner() {
        let (root, official, project, board) = fixture();
        let registry = FootprintRegistry::new(&official, &project).unwrap();
        let context = DisposableFootprintContext::new(&root, &board).unwrap();
        let mut request = placement(0);
        request.library_identifier = AllowlistedFootprint::ProjectTypeC.identifier();
        assert!(matches!(
            context.prepare(&registry, AllowlistedFootprint::OfficialLed1210, request),
            Err(FootprintBackendError::UnsupportedLibrary)
        ));
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn postcondition_matcher_is_strict_and_requires_reopen() {
        let (root, official, project, board) = fixture();
        let registry = FootprintRegistry::new(&official, &project).unwrap();
        let context = DisposableFootprintContext::new(&root, &board).unwrap();
        let prepared = context
            .prepare(
                &registry,
                AllowlistedFootprint::OfficialLed1210,
                placement(123),
            )
            .unwrap();
        let good = FootprintPostcondition {
            footprint_uuid: "01234567-89ab-cdef-0123-456789abcdef".to_owned(),
            library_identifier: AllowlistedFootprint::OfficialLed1210.identifier(),
            reference: "P1".to_owned(),
            value: "LED".to_owned(),
            position: placement(123).position,
            pad_count: 2,
            pad_numbers: vec!["1".to_owned(), "2".to_owned()],
            saved: true,
            reopened: true,
            source_digest: prepared.resolved().provenance.source_digest.clone(),
            source_size: prepared.resolved().provenance.source_size,
        };
        assert!(good.matches(&prepared));
        let mut bad = good.clone();
        bad.reopened = false;
        assert!(!bad.matches(&prepared));
        let mut bad = good.clone();
        bad.footprint_uuid.replace_range(8..9, "0");
        assert!(!bad.matches(&prepared));
        let mut bad = good.clone();
        bad.reference = "P2".to_owned();
        assert!(!bad.matches(&prepared));
        let mut repeated = good.clone();
        repeated.pad_numbers = vec!["S1".to_owned(), "S1".to_owned()];
        assert!(repeated.matches(&prepared));
        let mut empty = good.clone();
        empty.pad_numbers = vec!["1".to_owned(), String::new()];
        assert!(empty.matches(&prepared));
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn rejects_lock_and_out_of_bounds_placements() {
        let (root, official, project, board) = fixture();
        let registry = FootprintRegistry::new(&official, &project).unwrap();
        let context = DisposableFootprintContext::new(&root, &board).unwrap();
        assert!(matches!(
            validate_placement(&placement(MAX_PLACEMENT_COORDINATE_NM + 1)),
            Err(FootprintBackendError::InvalidPlacement(_))
        ));
        let prepared = context
            .prepare(
                &registry,
                AllowlistedFootprint::OfficialLed1210,
                placement(0),
            )
            .unwrap();
        assert!(matches!(
            DisabledFootprintRunner.place(&prepared),
            Err(FootprintBackendError::UnsupportedRunner)
        ));
        fs::write(board.with_file_name("~shadow.kicad_pcb.lck"), "lock").unwrap();
        assert!(matches!(
            context.prepare(
                &registry,
                AllowlistedFootprint::OfficialLed1210,
                placement(0),
            ),
            Err(FootprintBackendError::EditorLockPresent)
        ));
        fs::remove_dir_all(root).unwrap();
    }
}
