use std::fs::{self, File};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use kicad_model::{RevisionDigest, SchematicArtifactFile};
use kiutils_sexpr::{Node, parse_one};
use sha2::{Digest, Sha256};
use tempfile::Builder;

use crate::{NetlistDocument, NetlistRunner, parse_schematic};

const SUPPORTED_FORMAT: &str = "20260306";
const MAX_CANDIDATE_BYTES: usize = 32 * 1024 * 1024;

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SchematicPatchOutcome {
    Committed { artifact: SchematicArtifactFile },
    Rejected { reason: SchematicPatchRejection },
    Unknown { reason: String },
}

pub const TWO_RESISTOR_OPERATION_IDENTITY: &str = "fixed-two-resistor-circuit-v1";

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SchematicPreviewReceipt {
    pub source_artifact: SchematicArtifactFile,
    pub candidate_artifact: SchematicArtifactFile,
    pub operation_identity: String,
    pub candidate_bytes: Vec<u8>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SchematicPatchRejection {
    InvalidProjectRoot,
    InvalidRootSchematic,
    EditorLockPresent,
    RevisionMismatch,
    UnsupportedFormat,
    UnsupportedStructure,
    CandidateInvalid,
    ConnectivityNotProven,
    ErcFailed,
    CandidateIdentityMismatch,
}

pub fn preview_two_resistor_circuit(
    project_root: &Path,
    root_schematic: &Path,
    expected_artifact: &SchematicArtifactFile,
) -> Result<SchematicPreviewReceipt, SchematicPatchOutcome> {
    let runner = NetlistRunner::production().map_err(|error| SchematicPatchOutcome::Unknown {
        reason: error.to_string(),
    })?;
    preview_two_resistor_circuit_with_runner(
        project_root,
        root_schematic,
        expected_artifact,
        &runner,
    )
}

pub fn commit_two_resistor_circuit(
    project_root: &Path,
    root_schematic: &Path,
    receipt: &SchematicPreviewReceipt,
) -> SchematicPatchOutcome {
    let runner = match NetlistRunner::production() {
        Ok(runner) => runner,
        Err(error) => {
            return SchematicPatchOutcome::Unknown {
                reason: error.to_string(),
            };
        }
    };
    match commit_inner(project_root, root_schematic, receipt, &runner) {
        Ok(artifact) => SchematicPatchOutcome::Committed { artifact },
        Err(error) => outcome_from_failure(error),
    }
}

pub fn create_two_resistor_circuit(
    project_root: &Path,
    root_schematic: &Path,
    expected_artifact: &SchematicArtifactFile,
) -> SchematicPatchOutcome {
    let runner = match NetlistRunner::production() {
        Ok(runner) => runner,
        Err(error) => {
            return SchematicPatchOutcome::Unknown {
                reason: error.to_string(),
            };
        }
    };
    let receipt = match preview_two_resistor_circuit_with_runner(
        project_root,
        root_schematic,
        expected_artifact,
        &runner,
    ) {
        Ok(receipt) => receipt,
        Err(outcome) => return outcome,
    };
    commit_two_resistor_circuit_with_runner(project_root, root_schematic, &receipt, &runner)
}

fn preview_two_resistor_circuit_with_runner(
    project_root: &Path,
    root_schematic: &Path,
    expected_artifact: &SchematicArtifactFile,
    runner: &NetlistRunner,
) -> Result<SchematicPreviewReceipt, SchematicPatchOutcome> {
    let result = preview_inner(project_root, root_schematic, expected_artifact, runner);
    result.map_err(outcome_from_failure)
}

fn preview_inner(
    project_root: &Path,
    root_schematic: &Path,
    expected_artifact: &SchematicArtifactFile,
    runner: &NetlistRunner,
) -> Result<SchematicPreviewReceipt, PatchFailure> {
    let root = canonical_project_root(project_root)?;
    let schematic = canonical_root_schematic(&root, root_schematic)?;
    let relative_path = relative_schematic_path(&root, &schematic)?;
    if expected_artifact.relative_path != relative_path {
        return Err(rejected(SchematicPatchRejection::RevisionMismatch));
    }
    if patch_lock_path(&schematic).is_file() {
        return Err(rejected(SchematicPatchRejection::EditorLockPresent));
    }
    let original = read_root(&schematic)?;
    let source_artifact = artifact(&relative_path, &original)?;
    if &source_artifact != expected_artifact {
        return Err(rejected(SchematicPatchRejection::RevisionMismatch));
    }
    let candidate_bytes = build_candidate(&original)?;
    if candidate_bytes.len() > MAX_CANDIDATE_BYTES {
        return Err(rejected(SchematicPatchRejection::CandidateInvalid));
    }
    let parent = schematic
        .parent()
        .ok_or_else(|| rejected(SchematicPatchRejection::InvalidRootSchematic))?;
    let mut candidate = Builder::new()
        .prefix(".auto-kicad-schematic-")
        .suffix(".kicad_sch")
        .tempfile_in(parent)
        .map_err(|error| unknown("create candidate", error))?;
    candidate
        .write_all(&candidate_bytes)
        .map_err(|error| unknown("write candidate", error))?;
    candidate
        .as_file()
        .sync_all()
        .map_err(|error| unknown("sync candidate", error))?;
    validate_candidate(
        &relative_path,
        candidate.path(),
        &candidate_bytes,
        runner,
        &root,
    )?;
    let candidate_artifact = artifact(&relative_path, &candidate_bytes)?;
    Ok(SchematicPreviewReceipt {
        source_artifact,
        candidate_artifact,
        operation_identity: TWO_RESISTOR_OPERATION_IDENTITY.to_owned(),
        candidate_bytes,
    })
}

fn commit_two_resistor_circuit_with_runner(
    project_root: &Path,
    root_schematic: &Path,
    receipt: &SchematicPreviewReceipt,
    runner: &NetlistRunner,
) -> SchematicPatchOutcome {
    let result = commit_inner(project_root, root_schematic, receipt, runner);
    match result {
        Ok(artifact) => SchematicPatchOutcome::Committed { artifact },
        Err(error) => outcome_from_failure(error),
    }
}

fn commit_inner(
    project_root: &Path,
    root_schematic: &Path,
    receipt: &SchematicPreviewReceipt,
    runner: &NetlistRunner,
) -> Result<SchematicArtifactFile, PatchFailure> {
    if receipt.operation_identity != TWO_RESISTOR_OPERATION_IDENTITY
        || artifact(
            &receipt.candidate_artifact.relative_path,
            &receipt.candidate_bytes,
        )? != receipt.candidate_artifact
    {
        return Err(rejected(SchematicPatchRejection::CandidateIdentityMismatch));
    }
    let root = canonical_project_root(project_root)?;
    let schematic = canonical_root_schematic(&root, root_schematic)?;
    let relative_path = relative_schematic_path(&root, &schematic)?;
    if receipt.source_artifact.relative_path != relative_path
        || receipt.candidate_artifact.relative_path != relative_path
    {
        return Err(rejected(SchematicPatchRejection::CandidateIdentityMismatch));
    }
    let current = read_root(&schematic)?;
    if artifact(&relative_path, &current)? != receipt.source_artifact {
        return Err(rejected(SchematicPatchRejection::RevisionMismatch));
    }
    if patch_lock_path(&schematic).is_file() {
        return Err(rejected(SchematicPatchRejection::EditorLockPresent));
    }
    let _lock = reserve_lock(&schematic)?;
    let current = read_root(&schematic)?;
    if artifact(&relative_path, &current)? != receipt.source_artifact {
        return Err(rejected(SchematicPatchRejection::RevisionMismatch));
    }
    let parent = schematic
        .parent()
        .ok_or_else(|| rejected(SchematicPatchRejection::InvalidRootSchematic))?;
    let mut candidate = Builder::new()
        .prefix(".auto-kicad-schematic-")
        .suffix(".kicad_sch")
        .tempfile_in(parent)
        .map_err(|error| unknown("create candidate", error))?;
    candidate
        .write_all(&receipt.candidate_bytes)
        .map_err(|error| unknown("write candidate", error))?;
    candidate
        .as_file()
        .sync_all()
        .map_err(|error| unknown("sync candidate", error))?;
    let current = read_root(&schematic)?;
    if artifact(&relative_path, &current)? != receipt.source_artifact {
        return Err(rejected(SchematicPatchRejection::RevisionMismatch));
    }
    let candidate_path = candidate.into_temp_path();
    fs::rename(&candidate_path, &schematic)
        .map_err(|error| unknown("atomically replace schematic", error))?;
    sync_directory(parent).map_err(|error| unknown("sync schematic directory", error))?;
    let committed = read_root(&schematic)?;
    if committed != receipt.candidate_bytes {
        return Err(unknown(
            "committed schematic differs from approved candidate",
            io::Error::other("byte mismatch"),
        ));
    }
    if let Err(error) = validate_candidate(&relative_path, &schematic, &committed, runner, &root) {
        return Err(match error {
            PatchFailure::Rejected(reason) => {
                unknown("post-commit validation", format!("{reason:?}"))
            }
            PatchFailure::Unknown(reason) => unknown("post-commit validation", reason),
        });
    }
    artifact(&relative_path, &committed)
}

#[derive(Debug)]
enum PatchFailure {
    Rejected(SchematicPatchRejection),
    Unknown(String),
}

fn outcome_from_failure(error: PatchFailure) -> SchematicPatchOutcome {
    match error {
        PatchFailure::Rejected(reason) => SchematicPatchOutcome::Rejected { reason },
        PatchFailure::Unknown(reason) => SchematicPatchOutcome::Unknown { reason },
    }
}

fn validate_candidate(
    relative_path: &str,
    candidate_path: &Path,
    bytes: &[u8],
    runner: &NetlistRunner,
    project_root: &Path,
) -> Result<(), PatchFailure> {
    let parsed = parse_schematic(relative_path, bytes)
        .map_err(|_| rejected(SchematicPatchRejection::CandidateInvalid))?;
    let mut references = parsed
        .symbols
        .iter()
        .map(|symbol| symbol.reference.as_str())
        .collect::<Vec<_>>();
    references.sort_unstable();
    if references != ["R1", "R2"]
        || parsed.symbols.iter().any(|symbol| {
            symbol.lib_id != "Device:R" || symbol.value != "1k" || symbol.pins.len() != 2
        })
        || !has_exact_fixed_topology(bytes)
    {
        return Err(rejected(SchematicPatchRejection::CandidateInvalid));
    }
    let netlist = runner
        .run(project_root, candidate_path)
        .map_err(|_| rejected(SchematicPatchRejection::ConnectivityNotProven))?;
    prove_connectivity(&netlist.document)?;
    runner
        .run_erc(project_root, candidate_path)
        .map_err(|_| rejected(SchematicPatchRejection::ErcFailed))?;
    Ok(())
}

fn prove_connectivity(document: &NetlistDocument) -> Result<(), PatchFailure> {
    let mut components = document
        .components
        .iter()
        .map(|component| {
            (
                component.reference.as_str(),
                component.value.as_str(),
                component.library.as_str(),
                component.library_part.as_str(),
            )
        })
        .collect::<Vec<_>>();
    components.sort_unstable();
    if components != [("R1", "1k", "Device", "R"), ("R2", "1k", "Device", "R")] {
        return Err(rejected(SchematicPatchRejection::ConnectivityNotProven));
    }

    let intended_nodes = [("R1", "2"), ("R2", "1")];
    let mut target_net_count = 0;
    for net in &document.nets {
        let mut nodes = net
            .nodes
            .iter()
            .map(|node| (node.reference.as_str(), node.pin.as_str()))
            .collect::<Vec<_>>();
        nodes.sort_unstable();
        if nodes == intended_nodes {
            target_net_count += 1;
        } else if net.nodes.len() != 1 || !net.nodes[0].pin_type.contains("no_connect") {
            return Err(rejected(SchematicPatchRejection::ConnectivityNotProven));
        }
    }
    if target_net_count != 1 || document.nets.len() != 3 {
        return Err(rejected(SchematicPatchRejection::ConnectivityNotProven));
    }
    Ok(())
}

fn has_exact_fixed_topology(bytes: &[u8]) -> bool {
    let Ok(text) = std::str::from_utf8(bytes) else {
        return false;
    };
    text.matches("(wire (pts (xy 50 46.19) (xy 50 63.97))")
        .count()
        == 1
        && text.matches("(no_connect (at 50 53.81)").count() == 1
        && text.matches("(no_connect (at 50 56.35)").count() == 1
        && text.matches("(no_connect ").count() == 2
}

fn build_candidate(original: &[u8]) -> Result<Vec<u8>, PatchFailure> {
    let text = std::str::from_utf8(original)
        .map_err(|_| rejected(SchematicPatchRejection::UnsupportedStructure))?;
    let document =
        parse_one(text).map_err(|_| rejected(SchematicPatchRejection::UnsupportedStructure))?;
    let root = document
        .nodes
        .first()
        .ok_or_else(|| rejected(SchematicPatchRejection::UnsupportedStructure))?;
    let Node::List {
        items: root_items,
        span: root_span,
    } = root
    else {
        return Err(rejected(SchematicPatchRejection::UnsupportedStructure));
    };
    if tag(root_items) != Some("kicad_sch") || root_span.end != text.trim_end().len() {
        return Err(rejected(SchematicPatchRejection::UnsupportedStructure));
    }

    let mut format = None;
    let mut root_uuid = None;
    let mut lib_symbols_span = None;
    let mut existing_no_connects = 0;
    for node in &root_items[1..] {
        let Node::List { items, span } = node else {
            return Err(rejected(SchematicPatchRejection::UnsupportedStructure));
        };
        match tag(items) {
            Some("version") => format = value(items, 1),
            Some("uuid") => root_uuid = value(items, 1),
            Some("lib_symbols") if items.len() == 1 && lib_symbols_span.is_none() => {
                lib_symbols_span = Some(*span)
            }
            Some("no_connect") => existing_no_connects += 1,
            Some(
                "symbol" | "sheet" | "wire" | "junction" | "bus_entry" | "bus" | "label"
                | "hierarchical_label" | "global_label",
            ) => return Err(rejected(SchematicPatchRejection::UnsupportedStructure)),
            _ => {}
        }
    }
    if format != Some(SUPPORTED_FORMAT) {
        return Err(rejected(SchematicPatchRejection::UnsupportedFormat));
    }
    if existing_no_connects != 0 {
        return Err(rejected(SchematicPatchRejection::UnsupportedStructure));
    }
    let root_uuid = root_uuid
        .filter(|uuid| !uuid.is_empty())
        .ok_or_else(|| rejected(SchematicPatchRejection::UnsupportedStructure))?;
    let lib_span =
        lib_symbols_span.ok_or_else(|| rejected(SchematicPatchRejection::UnsupportedStructure))?;
    let ids = FreshIds::new();
    let library = resistor_library_definition();
    let additions = circuit_forms(root_uuid, &ids);

    let mut result = Vec::with_capacity(original.len() + library.len() + additions.len());
    let library_insert = lib_span.end - 1;
    let root_insert = root_span.end - 1;
    result.extend_from_slice(&original[..library_insert]);
    result.extend_from_slice(library.as_bytes());
    result.extend_from_slice(&original[library_insert..root_insert]);
    result.extend_from_slice(additions.as_bytes());
    result.extend_from_slice(&original[root_insert..]);
    Ok(result)
}

fn resistor_library_definition() -> &'static str {
    r#"
    (symbol "Device:R"
      (pin_numbers hide)
      (pin_names (offset 0))
      (exclude_from_sim no)
      (in_bom yes)
      (on_board yes)
      (property "Reference" "R" (at 2.032 0 90) (effects (font (size 1.27 1.27))))
      (property "Value" "R" (at 0 0 90) (effects (font (size 1.27 1.27))))
      (property "Footprint" "" (at -1.778 0 90) (effects (font (size 1.27 1.27)) (hide yes)))
      (property "Datasheet" "~" (at 0 0 0) (effects (font (size 1.27 1.27)) (hide yes)))
      (property "Description" "Resistor" (at 0 0 0) (effects (font (size 1.27 1.27)) (hide yes)))
      (symbol "R_0_1"
        (rectangle (start -1.016 -2.54) (end 1.016 2.54)
          (stroke (width 0.254) (type default)) (fill (type none))))
      (symbol "R_1_1"
        (pin passive line (at 0 3.81 270) (length 1.27)
          (name "~" (effects (font (size 1.27 1.27))))
          (number "2" (effects (font (size 1.27 1.27)))))
        (pin passive line (at 0 -3.81 90) (length 1.27)
          (name "~" (effects (font (size 1.27 1.27))))
          (number "1" (effects (font (size 1.27 1.27)))))))
"#
}

fn circuit_forms(root_uuid: &str, ids: &FreshIds) -> String {
    format!(
        r#"
  (wire (pts (xy 50 46.19) (xy 50 63.97))
    (stroke (width 0) (type default)) (uuid "{}"))
  (no_connect (at 50 53.81) (uuid "{}"))
  (no_connect (at 50 56.35) (uuid "{}"))
  (symbol
    (lib_id "Device:R") (at 50 50 0) (unit 1) (exclude_from_sim no)
    (in_bom yes) (on_board yes) (dnp no) (uuid "{}")
    (property "Reference" "R1" (at 52.54 48.73 0) (effects (font (size 1.27 1.27))))
    (property "Value" "1k" (at 52.54 51.27 0) (effects (font (size 1.27 1.27))))
    (property "Footprint" "" (at 48.22 50 90) (effects (font (size 1.27 1.27)) (hide yes)))
    (property "Datasheet" "~" (at 50 50 0) (effects (font (size 1.27 1.27)) (hide yes)))
    (property "Description" "Resistor" (at 50 50 0) (effects (font (size 1.27 1.27)) (hide yes)))
    (pin "1" (uuid "{}")) (pin "2" (uuid "{}"))
    (instances (project "" (path "/{}" (reference "R1") (unit 1)))))
  (symbol
    (lib_id "Device:R") (at 50 60.16 0) (unit 1) (exclude_from_sim no)
    (in_bom yes) (on_board yes) (dnp no) (uuid "{}")
    (property "Reference" "R2" (at 52.54 58.89 0) (effects (font (size 1.27 1.27))))
    (property "Value" "1k" (at 52.54 61.43 0) (effects (font (size 1.27 1.27))))
    (property "Footprint" "" (at 48.22 60.16 90) (effects (font (size 1.27 1.27)) (hide yes)))
    (property "Datasheet" "~" (at 50 60.16 0) (effects (font (size 1.27 1.27)) (hide yes)))
    (property "Description" "Resistor" (at 50 60.16 0) (effects (font (size 1.27 1.27)) (hide yes)))
    (pin "1" (uuid "{}")) (pin "2" (uuid "{}"))
    (instances (project "" (path "/{}" (reference "R2") (unit 1)))))
"#,
        ids.wire,
        ids.nc1,
        ids.nc2,
        ids.r1,
        ids.r1_pin1,
        ids.r1_pin2,
        root_uuid,
        ids.r2,
        ids.r2_pin1,
        ids.r2_pin2,
        root_uuid
    )
}

struct FreshIds {
    wire: String,
    nc1: String,
    nc2: String,
    r1: String,
    r1_pin1: String,
    r1_pin2: String,
    r2: String,
    r2_pin1: String,
    r2_pin2: String,
}

impl FreshIds {
    fn new() -> Self {
        Self {
            wire: fresh_uuid(),
            nc1: fresh_uuid(),
            nc2: fresh_uuid(),
            r1: fresh_uuid(),
            r1_pin1: fresh_uuid(),
            r1_pin2: fresh_uuid(),
            r2: fresh_uuid(),
            r2_pin1: fresh_uuid(),
            r2_pin2: fresh_uuid(),
        }
    }
}

fn fresh_uuid() -> String {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let mut hasher = Sha256::new();
    hasher.update(now.to_le_bytes());
    hasher.update(std::process::id().to_le_bytes());
    hasher.update(NEXT.fetch_add(1, Ordering::Relaxed).to_le_bytes());
    let digest = hasher.finalize();
    format!(
        "{:02x}{:02x}{:02x}{:02x}-{:02x}{:02x}-4{:02x}{:02x}-8{:02x}{:02x}-{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}",
        digest[0],
        digest[1],
        digest[2],
        digest[3],
        digest[4],
        digest[5],
        digest[6] & 0x0f,
        digest[7],
        digest[8] & 0x3f,
        digest[9],
        digest[10],
        digest[11],
        digest[12],
        digest[13],
        digest[14],
        digest[15]
    )
}

fn canonical_project_root(path: &Path) -> Result<PathBuf, PatchFailure> {
    let canonical = fs::canonicalize(path)
        .map_err(|_| rejected(SchematicPatchRejection::InvalidProjectRoot))?;
    if canonical != path || !canonical.is_dir() {
        return Err(rejected(SchematicPatchRejection::InvalidProjectRoot));
    }
    Ok(canonical)
}

fn canonical_root_schematic(root: &Path, path: &Path) -> Result<PathBuf, PatchFailure> {
    let canonical = fs::canonicalize(path)
        .map_err(|_| rejected(SchematicPatchRejection::InvalidRootSchematic))?;
    if canonical != path
        || !canonical.starts_with(root)
        || !canonical.is_file()
        || canonical.extension().and_then(|value| value.to_str()) != Some("kicad_sch")
    {
        return Err(rejected(SchematicPatchRejection::InvalidRootSchematic));
    }
    Ok(canonical)
}

fn patch_lock_path(schematic: &Path) -> PathBuf {
    let name = schematic
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or_default();
    schematic.with_file_name(format!("~{name}.lck"))
}

fn relative_schematic_path(root: &Path, path: &Path) -> Result<String, PatchFailure> {
    path.strip_prefix(root)
        .ok()
        .and_then(|relative| relative.to_str())
        .filter(|relative| !relative.is_empty() && !relative.contains('\\'))
        .map(str::to_owned)
        .ok_or_else(|| rejected(SchematicPatchRejection::InvalidRootSchematic))
}

struct LockReservation {
    path: PathBuf,
}

impl Drop for LockReservation {
    fn drop(&mut self) {
        if fs::remove_file(&self.path).is_ok()
            && let Some(parent) = self.path.parent()
        {
            let _ = sync_directory(parent);
        }
    }
}

fn reserve_lock(schematic: &Path) -> Result<LockReservation, PatchFailure> {
    let name = schematic
        .file_name()
        .and_then(|value| value.to_str())
        .ok_or_else(|| rejected(SchematicPatchRejection::InvalidRootSchematic))?;
    let path = schematic.with_file_name(format!("~{name}.lck"));
    match fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&path)
    {
        Ok(file) => {
            let reservation = LockReservation { path };
            file.sync_all()
                .map_err(|error| unknown("sync schematic lock", error))?;
            if let Some(parent) = reservation.path.parent() {
                sync_directory(parent)
                    .map_err(|error| unknown("sync schematic lock directory", error))?;
            }
            Ok(reservation)
        }
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
            Err(rejected(SchematicPatchRejection::EditorLockPresent))
        }
        Err(error) => Err(unknown("reserve schematic lock", error)),
    }
}

fn read_root(path: &Path) -> Result<Vec<u8>, PatchFailure> {
    let metadata = fs::metadata(path).map_err(|error| unknown("read schematic metadata", error))?;
    if metadata.len() > MAX_CANDIDATE_BYTES as u64 {
        return Err(rejected(SchematicPatchRejection::UnsupportedStructure));
    }
    fs::read(path).map_err(|error| unknown("read schematic", error))
}

fn artifact(relative_path: &str, bytes: &[u8]) -> Result<SchematicArtifactFile, PatchFailure> {
    let digest = RevisionDigest::new(hex::encode(Sha256::digest(bytes)))
        .map_err(|error| PatchFailure::Unknown(error.to_string()))?;
    SchematicArtifactFile::new(relative_path, bytes.len() as u64, digest)
        .map_err(|error| PatchFailure::Unknown(error.to_string()))
}

fn tag(items: &[Node]) -> Option<&str> {
    value(items, 0)
}

fn value(items: &[Node], index: usize) -> Option<&str> {
    match items.get(index) {
        Some(Node::Atom {
            atom: kiutils_sexpr::Atom::Symbol(value) | kiutils_sexpr::Atom::Quoted(value),
            ..
        }) => Some(value),
        _ => None,
    }
}

fn sync_directory(path: &Path) -> io::Result<()> {
    File::open(path)?.sync_all()
}

fn rejected(reason: SchematicPatchRejection) -> PatchFailure {
    PatchFailure::Rejected(reason)
}

fn unknown(context: &str, error: impl std::fmt::Display) -> PatchFailure {
    PatchFailure::Unknown(format!("{context}: {error}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    const EMPTY: &str = r#"(kicad_sch
  (version 20260306)
  (generator "eeschema")
  (generator_version "8.0")
  (uuid "11111111-1111-4111-8111-111111111111")
  (paper "A4")
  (lib_symbols)
)
"#;

    struct Fixture {
        dir: tempfile::TempDir,
        root: PathBuf,
        runner: NetlistRunner,
    }

    impl Fixture {
        fn new() -> Self {
            let dir = tempfile::tempdir().unwrap();
            let root = dir.path().join("root.kicad_sch");
            fs::write(&root, EMPTY).unwrap();
            let cli = dir.path().join("kicad-cli");
            fs::write(
                &cli,
                r#"#!/bin/sh
if [ "$1" = "sch" ] && [ "$2" = "export" ]; then
cat <<'XML'
<export version="E"><design><sheet number="1" name="/" tstamps="/"/></design><components><comp ref="R1"><value>1k</value><libsource lib="Device" part="R"/><sheetpath names="/" tstamps="/"/></comp><comp ref="R2"><value>1k</value><libsource lib="Device" part="R"/><sheetpath names="/" tstamps="/"/></comp></components><nets><net code="1" name="Net-(R1-Pad2)"><node ref="R1" pin="2" pinfunction="~" pintype="passive"/><node ref="R2" pin="1" pinfunction="~" pintype="passive"/></net><net code="2" name="unconnected-(R1-Pad1)"><node ref="R1" pin="1" pinfunction="~" pintype="passive+no_connect"/></net><net code="3" name="unconnected-(R2-Pad2)"><node ref="R2" pin="2" pinfunction="~" pintype="passive+no_connect"/></net></nets></export>
XML
exit 0
fi
if [ "$1" = "sch" ] && [ "$2" = "erc" ]; then
if [ -f "$(dirname "$0")/erc-fail" ]; then exit 7; fi
while [ "$1" != "" ]; do
  if [ "$1" = "-o" ]; then shift; printf 'ERC passed\n' > "$1"; fi
  shift
done
exit 0
fi
exit 1
"#,
            )
            .unwrap();
            fs::set_permissions(&cli, fs::Permissions::from_mode(0o755)).unwrap();
            Self {
                dir,
                root,
                runner: NetlistRunner::new(cli),
            }
        }

        fn artifact(&self) -> SchematicArtifactFile {
            artifact("root.kicad_sch", &fs::read(&self.root).unwrap()).unwrap()
        }

        fn run(&self, expected: &SchematicArtifactFile) -> SchematicPatchOutcome {
            let receipt = match preview_two_resistor_circuit_with_runner(
                &fs::canonicalize(self.dir.path()).unwrap(),
                &fs::canonicalize(&self.root).unwrap(),
                expected,
                &self.runner,
            ) {
                Ok(receipt) => receipt,
                Err(outcome) => return outcome,
            };
            commit_two_resistor_circuit_with_runner(
                &fs::canonicalize(self.dir.path()).unwrap(),
                &fs::canonicalize(&self.root).unwrap(),
                &receipt,
                &self.runner,
            )
        }
    }

    #[test]
    fn rejects_revision_mismatch_without_change() {
        let fixture = Fixture::new();
        let original = fs::read(&fixture.root).unwrap();
        let mut expected = fixture.artifact();
        expected.size += 1;
        assert_eq!(
            fixture.run(&expected),
            SchematicPatchOutcome::Rejected {
                reason: SchematicPatchRejection::RevisionMismatch
            }
        );
        assert_eq!(fs::read(&fixture.root).unwrap(), original);
    }

    #[test]
    fn rejects_editor_lock_without_change() {
        let fixture = Fixture::new();
        let original = fs::read(&fixture.root).unwrap();
        fs::write(fixture.dir.path().join("~root.kicad_sch.lck"), "lock").unwrap();
        assert_eq!(
            fixture.run(&fixture.artifact()),
            SchematicPatchOutcome::Rejected {
                reason: SchematicPatchRejection::EditorLockPresent
            }
        );
        assert_eq!(fs::read(&fixture.root).unwrap(), original);
    }

    #[test]
    fn rejects_unsupported_existing_structure_without_change() {
        let fixture = Fixture::new();
        fs::write(
            &fixture.root,
            EMPTY.replace(
                "  (lib_symbols)\n",
                "  (lib_symbols)\n  (wire (pts (xy 1 1) (xy 2 2)))\n",
            ),
        )
        .unwrap();
        let original = fs::read(&fixture.root).unwrap();
        assert_eq!(
            fixture.run(&fixture.artifact()),
            SchematicPatchOutcome::Rejected {
                reason: SchematicPatchRejection::UnsupportedStructure
            }
        );
        assert_eq!(fs::read(&fixture.root).unwrap(), original);
    }

    #[test]
    fn rejects_unverified_20231120_format_without_change() {
        let fixture = Fixture::new();
        fs::write(&fixture.root, EMPTY.replace("20260306", "20231120")).unwrap();
        let original = fs::read(&fixture.root).unwrap();
        assert_eq!(
            fixture.run(&fixture.artifact()),
            SchematicPatchOutcome::Rejected {
                reason: SchematicPatchRejection::UnsupportedFormat
            }
        );
        assert_eq!(fs::read(&fixture.root).unwrap(), original);
    }

    #[test]
    fn reservation_blocks_competitors_and_cleans_up_on_drop() {
        let fixture = Fixture::new();
        let root = fs::canonicalize(&fixture.root).unwrap();
        let lock_path = fixture.dir.path().join("~root.kicad_sch.lck");
        let reservation = reserve_lock(&root).unwrap();
        assert!(lock_path.is_file());
        assert!(matches!(
            reserve_lock(&root),
            Err(PatchFailure::Rejected(
                SchematicPatchRejection::EditorLockPresent
            ))
        ));
        drop(reservation);
        assert!(!lock_path.exists());
        assert!(reserve_lock(&root).is_ok());
    }

    #[test]
    fn rejects_extra_topology_and_missing_no_connect() {
        let candidate = build_candidate(EMPTY.as_bytes()).unwrap();
        assert!(has_exact_fixed_topology(&candidate));
        assert!(!has_exact_fixed_topology(
            &String::from_utf8(candidate.clone())
                .unwrap()
                .replacen(
                    "  (no_connect (at 50 56.35)",
                    "  (junction (at 50 56.35)",
                    1
                )
                .into_bytes()
        ));

        let xml = br#"<export version="E"><design><sheet number="1" name="/" tstamps="/"/></design><components><comp ref="R1"><value>1k</value><libsource lib="Device" part="R"/><sheetpath names="/" tstamps="/"/></comp><comp ref="R2"><value>1k</value><libsource lib="Device" part="R"/><sheetpath names="/" tstamps="/"/></comp><comp ref="R3"><value>1k</value><libsource lib="Device" part="R"/><sheetpath names="/" tstamps="/"/></comp></components><nets><net code="1" name="n"><node ref="R1" pin="2" pinfunction="~" pintype="passive"/><node ref="R2" pin="1" pinfunction="~" pintype="passive"/><node ref="R3" pin="1" pinfunction="~" pintype="passive"/></net></nets></export>"#;
        let document = crate::parse_netlist_xml(xml, crate::NetlistLimits::default()).unwrap();
        assert!(matches!(
            prove_connectivity(&document),
            Err(PatchFailure::Rejected(
                SchematicPatchRejection::ConnectivityNotProven
            ))
        ));

        let xml_with_extra_net = br#"<export version="E"><design><sheet number="1" name="/" tstamps="/"/></design><components><comp ref="R1"><value>1k</value><libsource lib="Device" part="R"/><sheetpath names="/" tstamps="/"/></comp><comp ref="R2"><value>1k</value><libsource lib="Device" part="R"/><sheetpath names="/" tstamps="/"/></comp></components><nets><net code="1" name="n"><node ref="R1" pin="2" pinfunction="~" pintype="passive"/><node ref="R2" pin="1" pinfunction="~" pintype="passive"/></net><net code="2" name="other"/></nets></export>"#;
        let document =
            crate::parse_netlist_xml(xml_with_extra_net, crate::NetlistLimits::default()).unwrap();
        assert!(matches!(
            prove_connectivity(&document),
            Err(PatchFailure::Rejected(
                SchematicPatchRejection::ConnectivityNotProven
            ))
        ));
    }

    #[test]
    fn preview_does_not_modify_source_or_leave_lock() {
        let fixture = Fixture::new();
        let original = fs::read(&fixture.root).unwrap();
        let receipt = preview_two_resistor_circuit_with_runner(
            &fs::canonicalize(fixture.dir.path()).unwrap(),
            &fs::canonicalize(&fixture.root).unwrap(),
            &fixture.artifact(),
            &fixture.runner,
        )
        .unwrap();
        assert_eq!(fs::read(&fixture.root).unwrap(), original);
        assert!(!fixture.dir.path().join("~root.kicad_sch.lck").exists());
        assert_eq!(
            receipt.candidate_artifact.size,
            receipt.candidate_bytes.len() as u64
        );
        assert_eq!(
            receipt.candidate_artifact,
            artifact("root.kicad_sch", &receipt.candidate_bytes).unwrap()
        );
    }

    #[test]
    fn reusing_preview_receipt_preserves_candidate_identity() {
        let fixture = Fixture::new();
        let receipt = preview_two_resistor_circuit_with_runner(
            &fs::canonicalize(fixture.dir.path()).unwrap(),
            &fs::canonicalize(&fixture.root).unwrap(),
            &fixture.artifact(),
            &fixture.runner,
        )
        .unwrap();
        let reused = receipt.clone();
        assert_eq!(receipt.candidate_artifact, reused.candidate_artifact);
        assert_eq!(receipt.candidate_bytes, reused.candidate_bytes);
        assert_eq!(receipt.operation_identity, TWO_RESISTOR_OPERATION_IDENTITY);
    }

    #[test]
    fn commit_rejects_mismatched_candidate_identity() {
        let fixture = Fixture::new();
        let mut receipt = preview_two_resistor_circuit_with_runner(
            &fs::canonicalize(fixture.dir.path()).unwrap(),
            &fs::canonicalize(&fixture.root).unwrap(),
            &fixture.artifact(),
            &fixture.runner,
        )
        .unwrap();
        receipt.candidate_bytes.push(b' ');
        let original = fs::read(&fixture.root).unwrap();
        assert_eq!(
            commit_two_resistor_circuit_with_runner(
                &fs::canonicalize(fixture.dir.path()).unwrap(),
                &fs::canonicalize(&fixture.root).unwrap(),
                &receipt,
                &fixture.runner,
            ),
            SchematicPatchOutcome::Rejected {
                reason: SchematicPatchRejection::CandidateIdentityMismatch
            }
        );
        assert_eq!(fs::read(&fixture.root).unwrap(), original);
    }

    #[test]
    fn post_commit_validation_failure_is_unknown_after_replacement() {
        let fixture = Fixture::new();
        let receipt = preview_two_resistor_circuit_with_runner(
            &fs::canonicalize(fixture.dir.path()).unwrap(),
            &fs::canonicalize(&fixture.root).unwrap(),
            &fixture.artifact(),
            &fixture.runner,
        )
        .unwrap();
        fs::write(fixture.dir.path().join("erc-fail"), "fail").unwrap();
        let outcome = commit_two_resistor_circuit_with_runner(
            &fs::canonicalize(fixture.dir.path()).unwrap(),
            &fs::canonicalize(&fixture.root).unwrap(),
            &receipt,
            &fixture.runner,
        );
        assert!(matches!(outcome, SchematicPatchOutcome::Unknown { .. }));
        assert_eq!(fs::read(&fixture.root).unwrap(), receipt.candidate_bytes);
        assert!(!fixture.dir.path().join("~root.kicad_sch.lck").exists());
    }

    #[cfg(unix)]
    #[test]
    #[ignore = "requires the installed KiCad 10.0.5 kicad-cli"]
    fn disposable_runtime_patch_round_trip_with_production_kicad_cli() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("root.kicad_sch");
        fs::write(&root, EMPTY).unwrap();
        let project_root = fs::canonicalize(dir.path()).unwrap();
        let canonical_root = fs::canonicalize(&root).unwrap();
        let source = fs::read(&canonical_root).unwrap();
        let source_artifact = artifact("root.kicad_sch", &source).unwrap();
        let production_runner = NetlistRunner::production().expect("installed KiCad CLI");

        let receipt = preview_two_resistor_circuit_with_runner(
            &project_root,
            &canonical_root,
            &source_artifact,
            &production_runner,
        )
        .expect("production preview");
        assert_eq!(fs::read(&canonical_root).unwrap(), source);
        assert!(!project_root.join("~root.kicad_sch.lck").exists());
        assert_eq!(
            receipt.candidate_artifact,
            artifact("root.kicad_sch", &receipt.candidate_bytes).unwrap()
        );

        let committed = commit_two_resistor_circuit_with_runner(
            &project_root,
            &canonical_root,
            &receipt,
            &production_runner,
        );
        let SchematicPatchOutcome::Committed {
            artifact: committed_artifact,
        } = committed
        else {
            panic!("production commit failed: {committed:?}");
        };
        let committed_bytes = fs::read(&canonical_root).unwrap();
        assert_eq!(committed_bytes, receipt.candidate_bytes);
        assert_eq!(
            committed_artifact,
            artifact("root.kicad_sch", &committed_bytes).unwrap()
        );
        assert!(!project_root.join("~root.kicad_sch.lck").exists());

        let parsed = parse_schematic("root.kicad_sch", &committed_bytes).unwrap();
        let mut references = parsed
            .symbols
            .iter()
            .map(|symbol| symbol.reference.as_str())
            .collect::<Vec<_>>();
        references.sort_unstable();
        assert_eq!(references, ["R1", "R2"]);
        assert!(parsed.symbols.iter().all(|symbol| {
            symbol.lib_id == "Device:R" && symbol.value == "1k" && symbol.pins.len() == 2
        }));
        assert!(has_exact_fixed_topology(&committed_bytes));

        let verification_runner = NetlistRunner::production().expect("installed KiCad CLI");
        let netlist = verification_runner
            .run(&project_root, &canonical_root)
            .expect("independent production netlist export");
        assert_eq!(netlist.document.components.len(), 2);
        assert_eq!(netlist.document.nets.len(), 3);
        let mut components = netlist
            .document
            .components
            .iter()
            .map(|component| {
                (
                    component.reference.as_str(),
                    component.value.as_str(),
                    component.library.as_str(),
                    component.library_part.as_str(),
                )
            })
            .collect::<Vec<_>>();
        components.sort_unstable();
        assert_eq!(
            components,
            [("R1", "1k", "Device", "R"), ("R2", "1k", "Device", "R")]
        );
        let target_net = netlist
            .document
            .nets
            .iter()
            .find(|net| {
                let mut nodes = net
                    .nodes
                    .iter()
                    .map(|node| (node.reference.as_str(), node.pin.as_str()))
                    .collect::<Vec<_>>();
                nodes.sort_unstable();
                nodes == [("R1", "2"), ("R2", "1")]
            })
            .expect("unique target net");
        let mut nodes = target_net
            .nodes
            .iter()
            .map(|node| (node.reference.as_str(), node.pin.as_str()))
            .collect::<Vec<_>>();
        nodes.sort_unstable();
        assert_eq!(nodes, [("R1", "2"), ("R2", "1")]);
        verification_runner
            .run_erc(&project_root, &canonical_root)
            .expect("independent production ERC");
        assert!(!project_root.join("~root.kicad_sch.lck").exists());
    }

    #[test]
    fn commits_validated_fixed_circuit() {
        let fixture = Fixture::new();
        let original = fs::read(&fixture.root).unwrap();
        let outcome = fixture.run(&fixture.artifact());
        let SchematicPatchOutcome::Committed { artifact } = outcome else {
            panic!("unexpected outcome: {outcome:?}");
        };
        let committed = fs::read(&fixture.root).unwrap();
        assert_ne!(committed, original);
        assert_eq!(
            artifact,
            super::artifact("root.kicad_sch", &committed).unwrap()
        );
        let parsed = parse_schematic("root.kicad_sch", &committed).unwrap();
        let mut references = parsed
            .symbols
            .iter()
            .map(|symbol| symbol.reference.as_str())
            .collect::<Vec<_>>();
        references.sort_unstable();
        assert_eq!(references, vec!["R1", "R2"]);
        assert!(!fixture.dir.path().join("~root.kicad_sch.lck").exists());
    }
}
