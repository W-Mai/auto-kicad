use kicad_model::{AllowlistedFootprintPlacement, FootprintLibraryIdentifier, RevisionDigest};
use sha2::{Digest, Sha256};
use std::fmt::{Display, Formatter};
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::time::Duration;

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

#[derive(Debug)]
pub enum FootprintBackendError {
    InvalidRoot,
    PathOutsideProject,
    EditorLockPresent,
    UnsupportedLibrary,
    SourceMissing(PathBuf),
    InvalidPlacement(&'static str),
    UnsupportedRunner,
    RunnerProcess(String),
    RunnerProtocol(&'static str),
    RunnerTimeout,
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
            Self::RunnerProcess(message) => {
                write!(formatter, "disposable footprint helper failed: {message}")
            }
            Self::RunnerProtocol(message) => write!(
                formatter,
                "invalid disposable footprint helper response: {message}"
            ),
            Self::RunnerTimeout => formatter.write_str("disposable footprint helper timed out"),
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

const MAX_HELPER_OUTPUT_BYTES: usize = 1024 * 1024;
const HELPER_TIMEOUT: Duration = Duration::from_secs(30);
const FIXED_PCBNEW_PATHS: &[&str] = &[
    "/Applications/KiCad/KiCad.app/Contents/Frameworks/Python.framework/Versions/3.9/bin/python3.9",
];
const FIXED_HELPER_PATH: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/helpers/footprint_helper.py");

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

impl FootprintPostcondition {
    pub fn matches(&self, request: &PreparedFootprintPlacement) -> bool {
        let placement = request.placement();
        is_uuid(&self.footprint_uuid)
            && self.library_identifier == placement.library_identifier
            && self.reference == placement.reference
            && self.value == placement.value
            && self.position == placement.position
            && self.pad_count == self.pad_numbers.len()
            && self.saved
            && self.reopened
            && self.pad_numbers.iter().all(|number| !number.is_empty())
    }
}

struct FootprintHelperResponse {
    postcondition: FootprintPostcondition,
}

impl FootprintHelperResponse {
    fn from_json(value: serde_json::Value) -> Result<Self, FootprintBackendError> {
        let postcondition =
            value
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
            "footprint_uuid",
            "library_identifier",
            "reference",
            "value",
            "position",
            "pad_count",
            "pad_numbers",
            "saved",
            "reopened",
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
        Ok(Self {
            postcondition: FootprintPostcondition {
                footprint_uuid: string("footprint_uuid")?.to_owned(),
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
            },
        })
    }
}

pub struct BundledPcbnewRunner {
    executable: PathBuf,
    helper: PathBuf,
    timeout: Duration,
}

impl BundledPcbnewRunner {
    pub fn production() -> Result<Self, FootprintBackendError> {
        let executable = FIXED_PCBNEW_PATHS
            .iter()
            .map(PathBuf::from)
            .find(|path| path.is_file())
            .ok_or(FootprintBackendError::UnsupportedRunner)?;
        let helper = PathBuf::from(FIXED_HELPER_PATH);
        if !helper.is_file() {
            return Err(FootprintBackendError::UnsupportedRunner);
        }
        Ok(Self {
            executable,
            helper,
            timeout: HELPER_TIMEOUT,
        })
    }

    fn run_helper(
        &self,
        request: &PreparedFootprintPlacement,
    ) -> Result<FootprintPostcondition, FootprintBackendError> {
        let input = serde_json::json!({
            "board_path": request.context().board_path(),
            "project_root": request.context().project_root(),
            "source_path": request.resolved().source_path,
            "library_identifier": request.placement().library_identifier,
            "target_id": request.placement().target_id,
            "position": request.placement().position,
            "reference": request.placement().reference,
            "value": request.placement().value,
        });
        let mut child = Command::new(&self.executable)
            .arg(&self.helper)
            .arg("--fixed-footprint-placement-v1")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|error| FootprintBackendError::RunnerProcess(error.to_string()))?;
        let mut stdin = child.stdin.take().ok_or_else(|| {
            FootprintBackendError::RunnerProcess("helper stdin unavailable".to_owned())
        })?;
        let encoded = serde_json::to_vec(&input)
            .map_err(|_| FootprintBackendError::RunnerProtocol("request encoding"))?;
        std::thread::spawn(move || {
            use std::io::Write;
            let _ = stdin.write_all(&encoded);
        });
        let stdout = child.stdout.take().ok_or_else(|| {
            FootprintBackendError::RunnerProcess("helper stdout unavailable".to_owned())
        })?;
        let (sender, receiver) = mpsc::channel();
        std::thread::spawn(move || {
            let mut limited = stdout.take((MAX_HELPER_OUTPUT_BYTES + 1) as u64);
            let mut bytes = Vec::new();
            let result = limited.read_to_end(&mut bytes).map(|_| bytes);
            let _ = sender.send(result);
        });
        let bytes = receiver
            .recv_timeout(self.timeout)
            .map_err(|_| {
                let _ = child.kill();
                let _ = child.wait();
                FootprintBackendError::RunnerTimeout
            })?
            .map_err(|error| FootprintBackendError::RunnerProcess(error.to_string()))?;
        let status = child
            .wait()
            .map_err(|error| FootprintBackendError::RunnerProcess(error.to_string()))?;
        if !status.success() {
            return Err(FootprintBackendError::RunnerProcess(status.to_string()));
        }
        if bytes.len() > MAX_HELPER_OUTPUT_BYTES {
            return Err(FootprintBackendError::RunnerProtocol("response too large"));
        }
        let value: serde_json::Value = serde_json::from_slice(&bytes)
            .map_err(|_| FootprintBackendError::RunnerProtocol("malformed JSON"))?;
        let response = FootprintHelperResponse::from_json(value)?;
        if !response.postcondition.matches(request) {
            return Err(FootprintBackendError::RunnerProtocol(
                "postcondition mismatch",
            ));
        }
        Ok(response.postcondition)
    }
}

impl DisposableFootprintRunner for BundledPcbnewRunner {
    fn place(
        &self,
        request: &PreparedFootprintPlacement,
    ) -> Result<FootprintPlacementReceipt, FootprintBackendError> {
        request.context().validate_before_place()?;
        let postcondition = self.run_helper(request)?;
        if !postcondition.matches(request) {
            return Err(FootprintBackendError::RunnerProtocol(
                "postcondition mismatch",
            ));
        }
        Ok(FootprintPlacementReceipt {
            resolved: request.resolved().clone(),
            placement: request.placement().clone(),
            postcondition,
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
        assert!(!empty.matches(&prepared));
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
