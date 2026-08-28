use kicad_model::{AllowlistedFootprintPlacement, FootprintLibraryIdentifier, RevisionDigest};
use sha2::{Digest, Sha256};
use std::fmt::{Display, Formatter};
use std::fs;
use std::path::{Path, PathBuf};

pub const MAX_PLACEMENT_COORDINATE_NM: i64 = 2_000_000_000;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AllowlistedFootprint {
    OfficialLed1210,
    ProjectTypeC,
}

impl AllowlistedFootprint {
    pub fn identifier(self) -> FootprintLibraryIdentifier {
        match self {
            Self::OfficialLed1210 => FootprintLibraryIdentifier {
                nickname: "LED_SMD".to_owned(),
                entry_name: "LED_1210_3225Metric_Pad1.42x2.65mm_HandSolder".to_owned(),
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
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FootprintSourceKind {
    OfficialLibrary,
    ProjectLibrary,
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
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ResolvedFootprint {
    pub library: AllowlistedFootprint,
    pub identifier: FootprintLibraryIdentifier,
    pub source_path: PathBuf,
    pub provenance: LibraryProvenance,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FootprintPlacementReceipt {
    pub resolved: ResolvedFootprint,
    pub placement: AllowlistedFootprintPlacement,
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

#[derive(Debug)]
pub enum FootprintBackendError {
    InvalidRoot,
    PathOutsideProject,
    EditorLockPresent,
    UnsupportedLibrary,
    SourceMissing(PathBuf),
    InvalidPlacement(&'static str),
    UnsupportedRunner,
}

impl Display for FootprintBackendError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidRoot => formatter.write_str("footprint backend root must be a directory"),
            Self::PathOutsideProject => formatter.write_str("board path is outside project root"),
            Self::EditorLockPresent => formatter.write_str("PCB editor lock is present"),
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
        }
    }
}

impl std::error::Error for FootprintBackendError {}

#[derive(Clone, Debug)]
pub struct FootprintRegistry {
    official_library: PathBuf,
    project_library: PathBuf,
}

impl FootprintRegistry {
    pub fn new(
        official_library: &Path,
        project_library: &Path,
    ) -> Result<Self, FootprintBackendError> {
        Ok(Self {
            official_library: canonical_directory(official_library)?,
            project_library: canonical_directory(project_library)?,
        })
    }

    pub fn resolve(
        &self,
        library: AllowlistedFootprint,
    ) -> Result<ResolvedFootprint, FootprintBackendError> {
        let root = match library {
            AllowlistedFootprint::OfficialLed1210 => &self.official_library,
            AllowlistedFootprint::ProjectTypeC => &self.project_library,
        };
        let source_path = root.join(library.source_subpath());
        let source_path = fs::canonicalize(&source_path)
            .map_err(|_| FootprintBackendError::SourceMissing(source_path.clone()))?;
        if !source_path.starts_with(root) || !source_path.is_file() {
            return Err(FootprintBackendError::SourceMissing(source_path));
        }
        let source_bytes = fs::read(&source_path)
            .map_err(|_| FootprintBackendError::SourceMissing(source_path.clone()))?;
        let mut hasher = Sha256::new();
        hasher.update(&source_bytes);
        let source_digest = RevisionDigest::new(hex::encode(hasher.finalize()))
            .expect("sha256 is a valid revision digest");
        let source_size = source_bytes.len() as u64;
        Ok(ResolvedFootprint {
            library,
            identifier: library.identifier(),
            source_path: source_path.clone(),
            provenance: LibraryProvenance {
                source_kind: match library {
                    AllowlistedFootprint::OfficialLed1210 => FootprintSourceKind::OfficialLibrary,
                    AllowlistedFootprint::ProjectTypeC => FootprintSourceKind::ProjectLibrary,
                },
                registry_root: root.clone(),
                source_path,
                source_size,
                source_digest,
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
        let resolved = registry.resolve(library)?;
        if placement.library_identifier != resolved.identifier {
            return Err(FootprintBackendError::UnsupportedLibrary);
        }
        Ok(PreparedFootprintPlacement {
            context: self.clone(),
            resolved,
            placement,
        })
    }
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
    use kicad_model::{Coordinate, DocumentSpace, StableId, Unit};
    use std::time::{SystemTime, UNIX_EPOCH};

    fn fixture() -> (PathBuf, PathBuf, PathBuf, PathBuf) {
        let root = std::env::temp_dir().join(format!(
            "auto-kicad-footprint-{}",
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
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
