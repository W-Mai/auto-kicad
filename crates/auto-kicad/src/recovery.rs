use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
mod native_publication;
mod publication;
pub use native_publication::{NativeSchematicPublication, SchematicPublicationFailure};
pub use publication::{
    SchematicPublication, SchematicPublicationFile, SchematicPublicationIntent, SchematicWrite,
};

const MAX_FILE_BYTES: u64 = 128 * 1024 * 1024;
const MAX_RECORD_BYTES: u64 = 4 * 1024 * 1024;
const GEOMETRY_PENDING: &str = ".auto-kicad-geometry-pending";
const SCHEMATIC_PENDING: &str = ".auto-kicad-schematic-pending";

#[derive(Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct PendingGeometry {
    attempt_id: String,
    record_path: PathBuf,
}

fn pending_geometry(project: &Path) -> io::Result<Option<PendingGeometry>> {
    pending_marker(project, GEOMETRY_PENDING)
}

fn pending_marker(project: &Path, marker: &str) -> io::Result<Option<PendingGeometry>> {
    let path = project.join(marker);
    match fs::symlink_metadata(&path) {
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e),
        Ok(_) => Ok(Some(serde_json::from_slice(&read_bounded(&path, 4096)?)?)),
    }
}

fn matching_pending_geometry(
    project: &Path,
    record_path: &Path,
    attempt_id: &str,
) -> io::Result<bool> {
    matching_pending_marker(project, GEOMETRY_PENDING, record_path, attempt_id)
}

fn matching_pending_marker(
    project: &Path,
    marker: &str,
    record_path: &Path,
    attempt_id: &str,
) -> io::Result<bool> {
    let Some(pending) = pending_marker(project, marker)? else {
        return Ok(false);
    };
    if pending.record_path != record_path || pending.attempt_id != attempt_id {
        return Err(invalid(
            "another execution attempt requires recovery before project writes",
        ));
    }
    Ok(true)
}

fn clear_pending_geometry(project: &Path, record_path: &Path, attempt_id: &str) -> io::Result<()> {
    clear_pending_marker(project, GEOMETRY_PENDING, record_path, attempt_id)
}

fn clear_pending_marker(
    project: &Path,
    marker: &str,
    record_path: &Path,
    attempt_id: &str,
) -> io::Result<()> {
    if matching_pending_marker(project, marker, record_path, attempt_id)? {
        fs::remove_file(project.join(marker))?;
        sync_directory(project)?;
    }
    Ok(())
}

fn clear_execution_pending(
    project: &Path,
    record_path: &Path,
    attempt_id: &str,
    domain: &str,
) -> io::Result<()> {
    if domain == "schematic" {
        clear_pending_marker(project, SCHEMATIC_PENDING, record_path, attempt_id)
    } else {
        clear_pending_geometry(project, record_path, attempt_id)
    }
}

#[derive(Clone, Debug)]
pub struct ExecutionStore {
    root: PathBuf,
    plans: Arc<Mutex<BTreeMap<String, String>>>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ExecutionRecord {
    pub schema_version: u32,
    #[serde(default)]
    pub attempt_id: String,
    pub project_dir: PathBuf,
    pub plan_hash: String,
    pub approval_hash: String,
    pub domain: String,
    pub state: String,
    pub evidence: Value,
    pub outcome: Option<Value>,
    pub backups: Vec<Backup>,
    #[serde(default)]
    pub post_attempt: Option<Vec<SourceFingerprint>>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub created_sources: Vec<CreatedSchematicSource>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub publication: Option<SchematicPublicationIntent>,
}

/// Exact observed existence of a source path; absence never uses an empty hash sentinel.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum SourceExistence {
    Absent,
    Present { sha256: kicad_model::RevisionDigest },
}

/// A schematic file proven absent before the attempt. Without publication intent, missing post evidence permits continued absence only.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CreatedSchematicSource {
    pub original: PathBuf,
    pub post_attempt: Option<SourceExistence>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CreatedSchematicRecovery {
    pub original: PathBuf,
    pub current: SourceExistence,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SourceFingerprint {
    pub path: PathBuf,
    pub sha256: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Backup {
    pub original: PathBuf,
    pub saved: PathBuf,
    pub sha256: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RecoveryFile {
    pub original: PathBuf,
    pub backup: PathBuf,
    pub current_sha256: String,
    pub backup_sha256: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RecoveryRecord {
    pub schema_version: u32,
    pub recovery_id: String,
    pub attempt_id: String,
    pub project_dir: PathBuf,
    pub domain: String,
    pub action: String,
    pub plan_hash: String,
    pub approval_hash: Option<String>,
    pub state: String,
    pub files: Vec<RecoveryFile>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub created_sources: Vec<CreatedSchematicRecovery>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub staging_files: Vec<CreatedSchematicRecovery>,
    pub outcome: Option<Value>,
}

pub struct ProjectWriter {
    _lock: File,
    project: PathBuf,
    records: PathBuf,
}

pub struct ExecutionAttempt {
    path: PathBuf,
    record: ExecutionRecord,
}

#[derive(Clone, Debug)]
pub struct RecoveryAttempt {
    path: PathBuf,
    pub record: RecoveryRecord,
}

fn invalid(message: &str) -> io::Error {
    io::Error::other(message)
}

fn read_bounded(path: &Path, limit: u64) -> io::Result<Vec<u8>> {
    if fs::canonicalize(path)? != path || !fs::symlink_metadata(path)?.is_file() {
        return Err(invalid(
            "recovery path must be a canonical regular file without symlinks",
        ));
    }
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
    }
    let file = options.open(path)?;
    if !file.metadata()?.is_file() {
        return Err(invalid("recovery input is not a regular file"));
    }
    let mut bytes = Vec::new();
    file.take(limit + 1).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > limit {
        return Err(invalid("recovery file exceeds size limit"));
    }
    Ok(bytes)
}

fn schematic_existence(project: &Path, path: &Path) -> io::Result<SourceExistence> {
    if path.extension().and_then(|extension| extension.to_str()) != Some("kicad_sch") {
        return Err(invalid("schematic source extension is required"));
    }
    confined_existence(project, path)
}

fn confined_existence(project: &Path, path: &Path) -> io::Result<SourceExistence> {
    let parent = path
        .parent()
        .ok_or_else(|| invalid("schematic source has no parent"))?;
    if !path.starts_with(project) || fs::canonicalize(parent)? != parent || !parent.is_dir() {
        return Err(invalid(
            "new schematic path must have a confined canonical existing parent",
        ));
    }
    match fs::symlink_metadata(path) {
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(SourceExistence::Absent),
        Err(error) => Err(error),
        Ok(_) => Ok(SourceExistence::Present {
            sha256: crate::digest_bytes(&read_bounded(path, 32 * 1024 * 1024)?),
        }),
    }
}

fn valid_execution_schema(record: &ExecutionRecord) -> bool {
    (record.schema_version == 1
        && record.created_sources.is_empty()
        && record.publication.is_none())
        || (record.schema_version == 2
            && record.domain == "schematic"
            && !record.created_sources.is_empty()
            && record.publication.is_none())
        || (record.schema_version == 3
            && record.domain == "schematic"
            && record.publication.is_some())
}

fn write_new(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(path)?;
    let result = file.write_all(bytes).and_then(|()| file.sync_all());
    if result.is_err() {
        drop(file);
        let _ = fs::remove_file(path);
    }
    result
}

fn sync_directory(path: &Path) -> io::Result<()> {
    #[cfg(unix)]
    File::open(path)?.sync_all()?;
    Ok(())
}

impl ExecutionStore {
    pub fn check_project_ready(&self, project: &Path) -> io::Result<()> {
        require_project_ready(&fs::canonicalize(project)?)
    }
    pub fn open(root: &Path) -> io::Result<Self> {
        let existed = root.exists();
        fs::create_dir_all(root)?;
        let root = fs::canonicalize(root)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            if !existed {
                fs::set_permissions(&root, fs::Permissions::from_mode(0o700))?;
            }
        }
        Ok(Self {
            root,
            plans: Arc::default(),
        })
    }

    fn records_dir(&self, project: &Path) -> PathBuf {
        self.root
            .join(crate::digest_bytes(project.as_os_str().as_encoded_bytes()).to_string())
    }

    pub fn acquire(&self, project: &Path) -> io::Result<ProjectWriter> {
        let project = fs::canonicalize(project)?;
        if !project.is_dir() {
            return Err(invalid("project directory is unavailable"));
        }
        // The lock location is independent of the server's configured state directory.
        let path = project.join(".auto-kicad-writer.lock");
        if fs::symlink_metadata(&path).is_ok_and(|m| !m.file_type().is_file()) {
            return Err(invalid("project writer lock is not a regular file"));
        }
        let mut options = OpenOptions::new();
        options.read(true).write(true).create(true).truncate(false);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
        }
        let lock = options.open(&path)?;
        if !lock.metadata()?.is_file() {
            return Err(invalid("project writer lock is not a regular file"));
        }
        lock.try_lock()
            .map_err(|_| invalid("project writer is busy"))?;
        let records = self.records_dir(&project);
        fs::create_dir_all(&records)?;
        if fs::canonicalize(&records)? != records {
            return Err(invalid("execution directory contains symlinks"));
        }
        sync_directory(&self.root)?;
        Ok(ProjectWriter {
            _lock: lock,
            project,
            records,
        })
    }

    pub fn history(&self, project: &Path) -> io::Result<Vec<ExecutionRecord>> {
        let project = fs::canonicalize(project)?;
        read_records(&self.records_dir(&project), &project, 100)
    }
}

fn read_records(root: &Path, project: &Path, limit: usize) -> io::Result<Vec<ExecutionRecord>> {
    let entries = match fs::read_dir(root) {
        Ok(entries) => entries,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(error),
    };
    let mut paths = entries
        .map(|entry| entry.map(|entry| entry.path()))
        .collect::<io::Result<Vec<_>>>()?;
    paths.sort();
    let mut records = Vec::new();
    for directory in paths.into_iter().rev() {
        // Legacy recovery records shared this directory. Never parse them as executions.
        if directory
            .file_name()
            .is_some_and(|name| name.to_string_lossy().starts_with("recovery-"))
        {
            continue;
        }
        let path = directory.join("record.json");
        if !path.exists() {
            continue;
        }
        let mut record: ExecutionRecord =
            serde_json::from_slice(&read_bounded(&path, MAX_RECORD_BYTES)?)?;
        if !valid_execution_schema(&record) || record.project_dir != project {
            return Err(invalid("recovery record schema or project mismatch"));
        }
        if record.state == "in_flight" {
            record.state = "in_flight_or_interrupted".into();
        }
        if record.attempt_id.is_empty() {
            record.attempt_id = directory
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_default();
        }
        records.push(record);
        if records.len() == limit {
            break;
        }
    }
    Ok(records)
}

impl ProjectWriter {
    /// Ordinary writers reject pending recovery before source preflight; recovery uses its own checks.
    pub fn ensure_ready(&self) -> io::Result<()> {
        require_project_ready(&self.project)
    }

    pub fn begin(
        &self,
        domain: &str,
        plan_hash: &str,
        approval_hash: &str,
        evidence: Value,
        sources: &[PathBuf],
    ) -> io::Result<ExecutionAttempt> {
        self.begin_internal(domain, plan_hash, approval_hash, evidence, sources, &[])
    }

    /// Record existing backups and proven-absent schematic paths before any publication.
    pub fn begin_schematic(
        &self,
        plan_hash: &str,
        approval_hash: &str,
        evidence: Value,
        sources: &[PathBuf],
        created_sources: &[PathBuf],
    ) -> io::Result<ExecutionAttempt> {
        self.ensure_ready()?;
        if sources.len() + created_sources.len() == 0 || sources.len() + created_sources.len() > 32
        {
            return Err(invalid(
                "schematic publication requires 1..32 changed files",
            ));
        }
        require_closed_project(&self.project)?;
        let mut paths = std::collections::BTreeSet::new();
        for path in sources {
            if !paths.insert(path)
                || !matches!(
                    schematic_existence(&self.project, path)?,
                    SourceExistence::Present { .. }
                )
            {
                return Err(invalid(
                    "existing schematic sources are missing or repeated",
                ));
            }
        }
        for path in created_sources {
            if !paths.insert(path)
                || schematic_existence(&self.project, path)? != SourceExistence::Absent
            {
                return Err(invalid("new schematic sources must be absent and distinct"));
            }
        }
        self.begin_internal(
            "schematic",
            plan_hash,
            approval_hash,
            evidence,
            sources,
            created_sources,
        )
    }

    fn begin_internal(
        &self,
        domain: &str,
        plan_hash: &str,
        approval_hash: &str,
        evidence: Value,
        sources: &[PathBuf],
        created_paths: &[PathBuf],
    ) -> io::Result<ExecutionAttempt> {
        self.ensure_ready()?;
        if read_records(&self.records, &self.project, usize::MAX)?
            .iter()
            .any(|record| record.approval_hash == approval_hash && record.state != "rejected")
        {
            return Err(invalid(
                "approval was already executed or has an uncertain outcome; inspect and create a fresh plan",
            ));
        }
        static SEQUENCE: AtomicU64 = AtomicU64::new(0);
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_err(io::Error::other)?
            .as_nanos();
        let directory = self.records.join(format!(
            "{now:039}-{}-{}",
            std::process::id(),
            SEQUENCE.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&directory)?;
        let mut backups = Vec::new();
        for (index, source) in sources.iter().enumerate() {
            let original = fs::canonicalize(source)?;
            if !original.starts_with(&self.project) {
                return Err(invalid("recovery source escaped the project"));
            }
            let bytes = read_bounded(&original, MAX_FILE_BYTES)?;
            let saved = directory.join(format!("source-{index}.backup"));
            write_new(&saved, &bytes)?;
            backups.push(Backup {
                original,
                saved,
                sha256: crate::digest_bytes(&bytes).to_string(),
            });
        }
        let created_sources = created_paths
            .iter()
            .map(|path| {
                if schematic_existence(&self.project, path)? != SourceExistence::Absent {
                    return Err(invalid(
                        "new schematic source appeared during intent preparation",
                    ));
                }
                Ok(CreatedSchematicSource {
                    original: path.clone(),
                    post_attempt: None,
                })
            })
            .collect::<io::Result<Vec<_>>>()?;
        let record = ExecutionRecord {
            schema_version: if created_sources.is_empty() { 1 } else { 2 },
            attempt_id: directory
                .file_name()
                .expect("execution directory has a name")
                .to_string_lossy()
                .into_owned(),
            project_dir: self.project.clone(),
            plan_hash: plan_hash.into(),
            approval_hash: approval_hash.into(),
            domain: domain.into(),
            state: "in_flight".into(),
            evidence,
            outcome: None,
            backups,
            post_attempt: None,
            created_sources,
            publication: None,
        };
        let path = directory.join("record.json");
        let bytes = serde_json::to_vec(&record)?;
        if bytes.len() as u64 > MAX_RECORD_BYTES {
            return Err(invalid("recovery evidence exceeds size limit"));
        }
        write_new(&path, &bytes)?;
        sync_directory(&directory)?;
        sync_directory(&self.records)?;
        let marker = if domain == "schematic" {
            Some(SCHEMATIC_PENDING)
        } else if record
            .evidence
            .get("geometry_evidence")
            .is_some_and(|value| !value.is_null())
        {
            Some(GEOMETRY_PENDING)
        } else {
            None
        };
        if let Some(marker) = marker {
            let pending = PendingGeometry {
                attempt_id: record.attempt_id.clone(),
                record_path: path.clone(),
            };
            write_new(&self.project.join(marker), &serde_json::to_vec(&pending)?)?;
            sync_directory(&self.project)?;
        }
        Ok(ExecutionAttempt { path, record })
    }
}

fn require_project_ready(project: &Path) -> io::Result<()> {
    if pending_geometry(project)?.is_some() || pending_marker(project, SCHEMATIC_PENDING)?.is_some()
    {
        return Err(invalid(
            "execution outcome requires guarded recovery before further project writes",
        ));
    }
    Ok(())
}

#[derive(Debug)]
pub struct RecoveryFailure {
    pub unknown: bool,
    error: io::Error,
}

impl From<io::Error> for RecoveryFailure {
    fn from(error: io::Error) -> Self {
        Self {
            unknown: false,
            error,
        }
    }
}
impl std::fmt::Display for RecoveryFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.error.fmt(f)
    }
}
impl std::error::Error for RecoveryFailure {}

fn valid_id(id: &str) -> bool {
    !id.is_empty() && id.len() <= 120 && id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
}

fn recovery_hash(record: &RecoveryRecord) -> io::Result<String> {
    let mut fields = serde_json::json!({
        "schema_version": record.schema_version, "recovery_id": record.recovery_id,
        "attempt_id": record.attempt_id, "project_dir": record.project_dir,
        "domain": record.domain, "action": record.action, "files": record.files,
    });
    if !record.created_sources.is_empty() {
        fields["created_sources"] = serde_json::to_value(&record.created_sources)?;
    }
    if !record.staging_files.is_empty() {
        fields["staging_files"] = serde_json::to_value(&record.staging_files)?;
    }
    Ok(crate::digest_bytes(&serde_json::to_vec(&fields)?).to_string())
}

// Disk recovery cannot synchronize an editor's unsaved model. Require closed documents.
fn require_closed_project(project: &Path) -> io::Result<()> {
    let mut pending = vec![project.to_path_buf()];
    let mut count = 0;
    while let Some(directory) = pending.pop() {
        for entry in fs::read_dir(directory)? {
            let entry = entry?;
            count += 1;
            if count > 10000 {
                return Err(invalid("project lock scan exceeds limit"));
            }
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if name.starts_with('~') && name.ends_with(".lck") {
                return Err(invalid(&format!(
                    "close the project editors before recovery; editor lock is present: {}",
                    entry.path().display()
                )));
            }
            if entry.file_type()?.is_dir() && !name.starts_with('.') {
                pending.push(entry.path());
            }
        }
    }
    Ok(())
}

impl ExecutionStore {
    fn recovery_dir(&self, project: &Path) -> PathBuf {
        // Separate namespace: older execution readers never encounter these records.
        self.root
            .join("recoveries")
            .join(crate::digest_bytes(project.as_os_str().as_encoded_bytes()).to_string())
    }

    fn execution_record(&self, project: &Path, id: &str) -> io::Result<ExecutionRecord> {
        if !valid_id(id) || id.starts_with("recovery-") {
            return Err(invalid("invalid execution identity"));
        }
        let path = self.records_dir(project).join(id).join("record.json");
        let mut record: ExecutionRecord =
            serde_json::from_slice(&read_bounded(&path, MAX_RECORD_BYTES)?)?;
        if record.attempt_id.is_empty() {
            record.attempt_id = id.into();
        }
        if !valid_execution_schema(&record)
            || record.project_dir != project
            || record.attempt_id != id
        {
            return Err(invalid("execution record identity mismatch"));
        }
        Ok(record)
    }

    fn recovery_files(
        &self,
        project: &Path,
        record: &ExecutionRecord,
    ) -> io::Result<Vec<RecoveryFile>> {
        publication::validate_intent(project, record)?;
        let record_path = self
            .records_dir(project)
            .join(&record.attempt_id)
            .join("record.json");
        let pending = if record.domain == "schematic" {
            matching_pending_marker(project, SCHEMATIC_PENDING, &record_path, &record.attempt_id)?
        } else {
            matching_pending_geometry(project, &record_path, &record.attempt_id)?
        };
        if !(matches!(record.state.as_str(), "unknown" | "in_flight")
            || pending && matches!(record.state.as_str(), "committed" | "rejected"))
            || !matches!(record.domain.as_str(), "pcb" | "schematic")
            || (record.backups.is_empty() && record.created_sources.is_empty())
        {
            return Err(invalid("execution attempt is not recoverable"));
        }
        if record
            .post_attempt
            .as_ref()
            .is_some_and(|files| files.len() != record.backups.len())
        {
            return Err(invalid("incomplete post-attempt evidence"));
        }
        let backup_root = self.records_dir(project).join(&record.attempt_id);
        let mut paths = std::collections::BTreeSet::new();
        record
            .backups
            .iter()
            .map(|backup| {
                let extension = backup
                    .original
                    .extension()
                    .and_then(|s| s.to_str())
                    .unwrap_or("");
                let supported = match record.domain.as_str() {
                    "pcb" => matches!(extension, "kicad_pcb" | "kicad_pro" | "kicad_dru"),
                    "schematic" => extension == "kicad_sch",
                    _ => false,
                };
                if !supported
                    || !backup.original.starts_with(project)
                    || backup.saved.parent() != Some(backup_root.as_path())
                    || !paths.insert(&backup.original)
                {
                    return Err(invalid("invalid or duplicate recovery source/backup path"));
                }
                let current = read_bounded(&backup.original, MAX_FILE_BYTES)?;
                let saved = read_bounded(&backup.saved, MAX_FILE_BYTES)?;
                if crate::digest_bytes(&saved).to_string() != backup.sha256 {
                    return Err(invalid("recovery backup digest mismatch"));
                }
                let current_digest = crate::digest_bytes(&current);
                let current_hash = current_digest.to_string();
                let expected = match &record.post_attempt {
                    Some(files) => {
                        &files
                            .iter()
                            .find(|f| f.path == backup.original)
                            .ok_or_else(|| invalid("missing post-attempt fingerprint"))?
                            .sha256
                    }
                    // Legacy or interrupted attempts can only recover an unchanged file.
                    None => &backup.sha256,
                };
                let allowed = publication::intended_state(
                    record,
                    &backup.original,
                    &SourceExistence::Present {
                        sha256: current_digest,
                    },
                )
                .unwrap_or(&current_hash == expected);
                if !allowed {
                    return Err(invalid(
                        "source changed after execution or post-attempt evidence is unavailable",
                    ));
                }
                Ok(RecoveryFile {
                    original: backup.original.clone(),
                    backup: backup.saved.clone(),
                    current_sha256: current_hash,
                    backup_sha256: backup.sha256.clone(),
                })
            })
            .collect()
    }

    fn recovery_created_sources(
        &self,
        project: &Path,
        record: &ExecutionRecord,
    ) -> io::Result<Vec<CreatedSchematicRecovery>> {
        if record.created_sources.len() + record.backups.len() > 32
            && !record.created_sources.is_empty()
        {
            return Err(invalid(
                "mixed schematic recovery exceeds changed-file bound",
            ));
        }
        let mut paths = record
            .backups
            .iter()
            .map(|backup| backup.original.clone())
            .collect::<std::collections::BTreeSet<_>>();
        record
            .created_sources
            .iter()
            .map(|source| {
                if record.domain != "schematic" || !paths.insert(source.original.clone()) {
                    return Err(invalid(
                        "new schematic recovery sources overlap or have the wrong domain",
                    ));
                }
                let current = schematic_existence(project, &source.original)?;
                let expected = source
                    .post_attempt
                    .as_ref()
                    .unwrap_or(&SourceExistence::Absent);
                if !publication::intended_state(record, &source.original, &current)
                    .unwrap_or(&current == expected)
                {
                    return Err(invalid(
                        "new schematic changed or post-attempt presence evidence is unavailable",
                    ));
                }
                Ok(CreatedSchematicRecovery {
                    original: source.original.clone(),
                    current,
                })
            })
            .collect()
    }

    fn read_recovery(&self, project: &Path, id: &str) -> io::Result<RecoveryAttempt> {
        if !valid_id(id) || !id.starts_with("recovery-") {
            return Err(invalid("invalid recovery identity"));
        }
        let path = self.recovery_dir(project).join(id).join("record.json");
        let record: RecoveryRecord =
            serde_json::from_slice(&read_bounded(&path, MAX_RECORD_BYTES)?)?;
        if !(record.schema_version == 2
            && record.created_sources.is_empty()
            && record.staging_files.is_empty()
            || record.schema_version == 3
                && record.domain == "schematic"
                && !record.created_sources.is_empty()
                && record.staging_files.is_empty()
            || record.schema_version == 4
                && record.domain == "schematic"
                && !record.staging_files.is_empty())
            || record.project_dir != project
            || record.recovery_id != id
            || record.action != "restore_backups"
            || recovery_hash(&record)? != record.plan_hash
        {
            return Err(invalid("recovery record identity or digest mismatch"));
        }
        if self
            .plans
            .lock()
            .map_err(|_| invalid("recovery plans unavailable"))?
            .get(id)
            != Some(&record.plan_hash)
        {
            return Err(invalid(
                "recovery plan expired, was consumed, or belongs to another server session",
            ));
        }
        Ok(RecoveryAttempt { path, record })
    }

    fn persist_recovery(attempt: &RecoveryAttempt) -> io::Result<()> {
        let bytes = serde_json::to_vec(&attempt.record)?;
        if bytes.len() as u64 > MAX_RECORD_BYTES {
            return Err(invalid("recovery record exceeds size limit"));
        }
        let temporary = attempt.path.with_extension("pending");
        write_new(&temporary, &bytes)?;
        let result = fs::rename(&temporary, &attempt.path)
            .and_then(|()| sync_directory(attempt.path.parent().expect("record parent")));
        if result.is_err() {
            let _ = fs::remove_file(&temporary);
        }
        result
    }

    pub fn plan_recovery(&self, project: &Path, attempt_id: &str) -> io::Result<RecoveryRecord> {
        let writer = self.acquire(project)?;
        let project = &writer.project;
        require_closed_project(project)?;
        let execution = self.execution_record(project, attempt_id)?;
        let files = self.recovery_files(project, &execution)?;
        let created_sources = self.recovery_created_sources(project, &execution)?;
        let staging_files = publication::recovery_staging(project, &execution)?;
        static SEQUENCE: AtomicU64 = AtomicU64::new(0);
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_err(io::Error::other)?
            .as_nanos();
        let id = format!(
            "recovery-{now:039}-{}-{}",
            std::process::id(),
            SEQUENCE.fetch_add(1, Ordering::Relaxed)
        );
        let root = self.recovery_dir(project);
        fs::create_dir_all(&root)?;
        if fs::canonicalize(&root)? != root {
            return Err(invalid("recovery directory must not contain symlinks"));
        }
        let directory = root.join(&id);
        fs::create_dir(&directory)?;
        let mut record = RecoveryRecord {
            schema_version: if !staging_files.is_empty() {
                4
            } else if created_sources.is_empty() {
                2
            } else {
                3
            },
            recovery_id: id.clone(),
            attempt_id: execution.attempt_id,
            project_dir: project.clone(),
            domain: execution.domain,
            action: "restore_backups".into(),
            plan_hash: String::new(),
            approval_hash: None,
            state: "prepared".into(),
            files,
            created_sources,
            staging_files,
            outcome: None,
        };
        record.plan_hash = recovery_hash(&record)?;
        let attempt = RecoveryAttempt {
            path: directory.join("record.json"),
            record,
        };
        Self::persist_recovery(&attempt)?;
        sync_directory(&root)?;
        sync_directory(root.parent().expect("namespace"))?;
        sync_directory(&self.root)?;
        self.plans
            .lock()
            .map_err(|_| invalid("recovery plans unavailable"))?
            .insert(id, attempt.record.plan_hash.clone());
        Ok(attempt.record)
    }

    fn check_recovery_files(&self, record: &RecoveryRecord) -> io::Result<()> {
        require_closed_project(&record.project_dir)?;
        let execution = self.execution_record(&record.project_dir, &record.attempt_id)?;
        let current = self.recovery_files(&record.project_dir, &execution)?;
        let created = self.recovery_created_sources(&record.project_dir, &execution)?;
        let staging = publication::recovery_staging(&record.project_dir, &execution)?;
        if serde_json::to_value(current)? != serde_json::to_value(&record.files)?
            || created != record.created_sources
            || staging != record.staging_files
        {
            return Err(invalid(
                "source or backup changed since recovery preparation",
            ));
        }
        Ok(())
    }

    fn invalidate(&self, attempt: &mut RecoveryAttempt, reason: &io::Error) {
        self.plans
            .lock()
            .expect("plans")
            .remove(&attempt.record.recovery_id);
        attempt.record.state = "rejected".into();
        attempt.record.outcome =
            Some(serde_json::json!({"kind": "rejected", "reason": reason.to_string()}));
        let _ = Self::persist_recovery(attempt);
    }

    pub fn preview_recovery(
        &self,
        project: &Path,
        recovery_id: &str,
        plan_hash: &str,
    ) -> io::Result<String> {
        let writer = self.acquire(project)?;
        let mut attempt = self.read_recovery(&writer.project, recovery_id)?;
        if attempt.record.plan_hash != plan_hash || attempt.record.state != "prepared" {
            return Err(invalid("recovery plan is stale or already used"));
        }
        if let Err(error) = self.check_recovery_files(&attempt.record) {
            self.invalidate(&mut attempt, &error);
            return Err(error);
        }
        let approval =
            crate::digest_bytes(format!("{plan_hash}:{recovery_id}:restore_backups").as_bytes())
                .to_string();
        attempt.record.approval_hash = Some(approval.clone());
        attempt.record.state = "awaiting_approval".into();
        Self::persist_recovery(&attempt)?;
        Ok(approval)
    }

    pub fn commit_recovery(
        &self,
        project: &Path,
        recovery_id: &str,
        plan_hash: &str,
        approval_hash: &str,
    ) -> Result<RecoveryRecord, RecoveryFailure> {
        self.commit_recovery_internal(project, recovery_id, plan_hash, approval_hash, |_| Ok(()))
    }

    fn commit_recovery_internal(
        &self,
        project: &Path,
        recovery_id: &str,
        plan_hash: &str,
        approval_hash: &str,
        mut checkpoint: impl FnMut(usize) -> io::Result<()>,
    ) -> Result<RecoveryRecord, RecoveryFailure> {
        let writer = self.acquire(project)?;
        let mut attempt = self.read_recovery(&writer.project, recovery_id)?;
        let expected =
            crate::digest_bytes(format!("{plan_hash}:{recovery_id}:restore_backups").as_bytes())
                .to_string();
        if attempt.record.plan_hash != plan_hash
            || attempt.record.state != "awaiting_approval"
            || expected != approval_hash
            || attempt.record.approval_hash.as_deref() != Some(approval_hash)
        {
            return Err(invalid("recovery approval mismatch or state is not ready").into());
        }
        if let Err(error) = self.check_recovery_files(&attempt.record) {
            self.invalidate(&mut attempt, &error);
            return Err(error.into());
        }
        attempt.record.state = "in_flight".into();
        // Consume before any durable transition; persistence ambiguity cannot enable replay.
        self.plans.lock().expect("plans").remove(recovery_id);
        Self::persist_recovery(&attempt).map_err(|error| RecoveryFailure {
            unknown: true,
            error,
        })?;
        let result = (|| {
            for (index, file) in attempt.record.files.iter().enumerate() {
                require_closed_project(&writer.project)?;
                let bytes = read_bounded(&file.backup, MAX_FILE_BYTES)?;
                if crate::digest_bytes(&bytes).to_string() != file.backup_sha256 {
                    return Err(invalid("backup changed during recovery"));
                }
                let temporary = file
                    .original
                    .with_file_name(format!(".{recovery_id}-{index}.auto-kicad-recovery.tmp"));
                write_new(&temporary, &bytes)?;
                let result = (|| {
                    let current = read_bounded(&file.original, MAX_FILE_BYTES)?;
                    if crate::digest_bytes(&current).to_string() != file.current_sha256 {
                        return Err(invalid("source changed during recovery"));
                    }
                    fs::rename(&temporary, &file.original)?;
                    sync_directory(
                        file.original
                            .parent()
                            .ok_or_else(|| invalid("source has no parent"))?,
                    )
                })();
                let _ = fs::remove_file(&temporary);
                result?;
                checkpoint(index)?;
            }
            for (index, source) in attempt.record.created_sources.iter().enumerate() {
                require_closed_project(&writer.project)?;
                if schematic_existence(&writer.project, &source.original)? != source.current {
                    return Err(invalid("new schematic changed during recovery"));
                }
                if matches!(source.current, SourceExistence::Present { .. }) {
                    fs::remove_file(&source.original)?;
                    sync_directory(
                        source
                            .original
                            .parent()
                            .ok_or_else(|| invalid("new source has no parent"))?,
                    )?;
                }
                checkpoint(attempt.record.files.len() + index)?;
            }
            for (index, stage) in attempt.record.staging_files.iter().enumerate() {
                require_closed_project(&writer.project)?;
                if confined_existence(&writer.project, &stage.original)? != stage.current {
                    return Err(invalid("publication staging changed during recovery"));
                }
                if matches!(stage.current, SourceExistence::Present { .. }) {
                    fs::remove_file(&stage.original)?;
                    sync_directory(
                        stage
                            .original
                            .parent()
                            .ok_or_else(|| invalid("staging has no parent"))?,
                    )?;
                }
                checkpoint(
                    attempt.record.files.len() + attempt.record.created_sources.len() + index,
                )?;
            }
            for file in &attempt.record.files {
                if crate::digest_bytes(&read_bounded(&file.original, MAX_FILE_BYTES)?).to_string()
                    != file.backup_sha256
                {
                    return Err(invalid("restored file digest did not match backup"));
                }
            }
            for source in &attempt.record.created_sources {
                if schematic_existence(&writer.project, &source.original)?
                    != SourceExistence::Absent
                {
                    return Err(invalid("new schematic was not restored to absence"));
                }
            }
            for stage in &attempt.record.staging_files {
                if confined_existence(&writer.project, &stage.original)? != SourceExistence::Absent
                {
                    return Err(invalid("publication staging was not removed"));
                }
            }
            checkpoint(usize::MAX)?;
            attempt.record.state = "restored".into();
            attempt.record.outcome = Some(serde_json::json!({"kind": "restored"}));
            Self::persist_recovery(&attempt)?;
            let record_path = self
                .records_dir(&writer.project)
                .join(&attempt.record.attempt_id)
                .join("record.json");
            clear_execution_pending(
                &writer.project,
                &record_path,
                &attempt.record.attempt_id,
                &attempt.record.domain,
            )
        })();
        match result {
            Ok(()) => Ok(attempt.record),
            Err(error) => {
                attempt.record.state = "unknown".into();
                attempt.record.outcome =
                    Some(serde_json::json!({"kind": "unknown", "reason": error.to_string()}));
                let _ = Self::persist_recovery(&attempt);
                Err(RecoveryFailure {
                    unknown: true,
                    error,
                })
            }
        }
    }

    pub fn recovery_history(&self, project: &Path) -> io::Result<Vec<RecoveryRecord>> {
        let project = fs::canonicalize(project)?;
        let mut records = Vec::new();
        // Retain read-only visibility of the old, unsafe recovery schema.
        for root in [self.recovery_dir(&project), self.records_dir(&project)] {
            let entries = match fs::read_dir(&root) {
                Ok(entries) => entries,
                Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
                Err(error) => return Err(error),
            };
            let mut paths = entries
                .map(|entry| entry.map(|e| e.path()))
                .collect::<io::Result<Vec<_>>>()?;
            paths.sort();
            for directory in paths
                .into_iter()
                .rev()
                .filter(|p| {
                    p.file_name()
                        .is_some_and(|name| name.to_string_lossy().starts_with("recovery-"))
                })
                .take(100)
            {
                let path = directory.join("record.json");
                if !path.exists() {
                    continue;
                }
                let mut record: RecoveryRecord =
                    serde_json::from_slice(&read_bounded(&path, MAX_RECORD_BYTES)?)?;
                if record.project_dir != project || !matches!(record.schema_version, 1..=4) {
                    return Err(invalid("invalid recovery history record"));
                }
                if record.state == "in_flight" {
                    record.state = "unknown".into();
                }
                if matches!(record.state.as_str(), "prepared" | "awaiting_approval")
                    && !self
                        .plans
                        .lock()
                        .expect("plans")
                        .contains_key(&record.recovery_id)
                {
                    record.state = "expired".into();
                }
                records.push(record);
            }
        }
        records.sort_by(|a, b| b.recovery_id.cmp(&a.recovery_id));
        records.truncate(100);
        Ok(records)
    }
}

impl ExecutionAttempt {
    pub fn attempt_id(&self) -> &str {
        &self.record.attempt_id
    }

    pub fn finish(mut self, state: &str, outcome: Value) -> io::Result<()> {
        // Missing final evidence requires unchanged bytes unless durable publication intent binds exact candidate states.
        self.record.post_attempt = Some(
            self.record
                .backups
                .iter()
                .map(|backup| {
                    Ok(SourceFingerprint {
                        path: backup.original.clone(),
                        sha256: crate::digest_bytes(&read_bounded(
                            &backup.original,
                            MAX_FILE_BYTES,
                        )?)
                        .to_string(),
                    })
                })
                .collect::<io::Result<Vec<_>>>()?,
        );
        for source in &mut self.record.created_sources {
            source.post_attempt = Some(schematic_existence(
                &self.record.project_dir,
                &source.original,
            )?);
        }
        self.record.state = state.into();
        self.record.outcome = Some(outcome);
        let bytes = serde_json::to_vec(&self.record)?;
        if bytes.len() as u64 > MAX_RECORD_BYTES {
            return Err(invalid("recovery outcome exceeds size limit"));
        }
        let temporary = self.path.with_extension("pending");
        write_new(&temporary, &bytes)?;
        fs::rename(&temporary, &self.path)?;
        sync_directory(self.path.parent().expect("record has parent"))?;
        if matches!(state, "committed" | "rejected") {
            clear_execution_pending(
                &self.record.project_dir,
                &self.path,
                &self.record.attempt_id,
                &self.record.domain,
            )?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Fixture {
        root: PathBuf,
        project: PathBuf,
        source: PathBuf,
        store: ExecutionStore,
        id: String,
    }
    impl Fixture {
        fn new(interrupted: bool) -> Self {
            static SEQ: AtomicU64 = AtomicU64::new(0);
            let root = std::env::temp_dir().join(format!(
                "auto-kicad-recovery-test-{}-{}-{}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_nanos(),
                SEQ.fetch_add(1, Ordering::Relaxed)
            ));
            fs::create_dir(&root).unwrap();
            let root = fs::canonicalize(root).unwrap();
            let project = root.join("project");
            fs::create_dir(&project).unwrap();
            let source = project.join("board.kicad_pcb");
            fs::write(&source, b"before").unwrap();
            fs::write(source.with_extension("kicad_pro"), b"project-before").unwrap();
            let store = ExecutionStore::open(&root.join("state")).unwrap();
            let writer = store.acquire(&project).unwrap();
            let attempt = writer
                .begin(
                    "pcb",
                    "plan",
                    "approval",
                    Value::Null,
                    &[source.clone(), source.with_extension("kicad_pro")],
                )
                .unwrap();
            let id = attempt.attempt_id().to_owned();
            if interrupted {
                drop(attempt);
            } else {
                fs::write(&source, b"attempt-write").unwrap();
                fs::write(source.with_extension("kicad_pro"), b"project-attempt-write").unwrap();
                attempt.finish("unknown", Value::Null).unwrap();
            }
            drop(writer);
            Self {
                root,
                project,
                source,
                store,
                id,
            }
        }
        fn ready(&self) -> (RecoveryRecord, String) {
            let plan = self.store.plan_recovery(&self.project, &self.id).unwrap();
            let approval = self
                .store
                .preview_recovery(&self.project, &plan.recovery_id, &plan.plan_hash)
                .unwrap();
            (plan, approval)
        }
        fn commit(
            &self,
            p: &RecoveryRecord,
            approval: &str,
        ) -> Result<RecoveryRecord, RecoveryFailure> {
            self.store
                .commit_recovery(&self.project, &p.recovery_id, &p.plan_hash, approval)
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            fs::remove_dir_all(&self.root).unwrap();
        }
    }

    #[test]
    fn bounded_execution_records_preserve_dense_evidence_without_indentation_overhead() {
        let f = Fixture::new(false);
        let before = fs::read(&f.source).unwrap();
        let points = (0..512)
            .map(|i| serde_json::json!({"x_nm":3000000+i * 40000,"y_nm":43000000}))
            .collect::<Vec<_>>();
        let zones = (0..32).map(|i| serde_json::json!({"zone_id":format!("zone-{i}"),"definition":{"polygon":{"exterior":{"points":points}}}})).collect::<Vec<_>>();
        let evidence = serde_json::json!({
            "plan":{"operations":[{"kind":"EditGeometry","request":{"zones":zones}}]},
            "geometry_candidate":{"summary":{"zones":zones}},
            "geometry_evidence":{"bound":true}
        });
        let outcome = serde_json::json!({"geometry":{"summary":{"zones":zones}}});
        let writer = f.store.acquire(&f.project).unwrap();
        let attempt = writer
            .begin(
                "pcb",
                "dense-plan",
                "dense-approval",
                evidence.clone(),
                std::slice::from_ref(&f.source),
            )
            .unwrap();
        assert!(
            serde_json::to_vec_pretty(&attempt.record).unwrap().len() as u64 > MAX_RECORD_BYTES
        );
        let path = attempt.path.clone();
        let id = attempt.attempt_id().to_owned();
        attempt.finish("unknown", outcome.clone()).unwrap();
        let bytes = read_bounded(&path, MAX_RECORD_BYTES).unwrap();
        let record: ExecutionRecord = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(record.evidence, evidence);
        assert_eq!(record.outcome, Some(outcome));
        assert_eq!(fs::read(&f.source).unwrap(), before);
        drop(writer);
        let plan = f.store.plan_recovery(&f.project, &id).unwrap();
        let approval = f
            .store
            .preview_recovery(&f.project, &plan.recovery_id, &plan.plan_hash)
            .unwrap();
        f.store
            .commit_recovery(&f.project, &plan.recovery_id, &plan.plan_hash, &approval)
            .unwrap();
        assert_eq!(fs::read(&f.source).unwrap(), before);
        assert!(!f.project.join(GEOMETRY_PENDING).exists());
        let writer = f.store.acquire(&f.project).unwrap();
        assert!(
            writer
                .begin(
                    "pcb",
                    "oversized",
                    "oversized",
                    serde_json::json!({"payload":"x".repeat(MAX_RECORD_BYTES as usize)}),
                    std::slice::from_ref(&f.source)
                )
                .is_err()
        );
        assert!(!f.project.join(GEOMETRY_PENDING).exists());
        assert_eq!(fs::read(&f.source).unwrap(), before);
    }

    #[test]
    fn uncertain_geometry_blocks_all_writers_across_state_directories_until_matching_recovery() {
        for interrupted in [false, true] {
            let f = Fixture::new(false);
            let writer = f.store.acquire(&f.project).unwrap();
            let attempt = writer
                .begin(
                    "pcb",
                    "geometry-plan",
                    "geometry-approval",
                    serde_json::json!({"geometry_evidence":{"bound":true}}),
                    std::slice::from_ref(&f.source),
                )
                .unwrap();
            let id = attempt.attempt_id().to_owned();
            if interrupted {
                drop(attempt);
            } else {
                attempt.finish("unknown", Value::Null).unwrap();
            }
            drop(writer);
            let other = ExecutionStore::open(&f.root.join("other-state")).unwrap();
            for store in [&f.store, &other] {
                let writer = store.acquire(&f.project).unwrap();
                assert!(writer.ensure_ready().is_err());
                for domain in ["pcb", "schematic"] {
                    assert!(
                        writer
                            .begin(
                                domain,
                                "different-plan",
                                "different-approval",
                                Value::Null,
                                &[]
                            )
                            .is_err()
                    );
                }
            }
            assert!(f.store.plan_recovery(&f.project, &f.id).is_err());
            let plan = f.store.plan_recovery(&f.project, &id).unwrap();
            let approval = f
                .store
                .preview_recovery(&f.project, &plan.recovery_id, &plan.plan_hash)
                .unwrap();
            assert!(
                f.store
                    .commit_recovery(&f.project, &plan.recovery_id, &plan.plan_hash, "wrong")
                    .is_err()
            );
            assert!(pending_geometry(&f.project).unwrap().is_some());
            f.store
                .commit_recovery(&f.project, &plan.recovery_id, &plan.plan_hash, &approval)
                .unwrap();
            assert!(pending_geometry(&f.project).unwrap().is_none());
            other
                .acquire(&f.project)
                .unwrap()
                .begin("pcb", "new-plan", "new-approval", Value::Null, &[])
                .unwrap()
                .finish("committed", Value::Null)
                .unwrap();
        }
    }

    #[test]
    fn verified_geometry_releases_only_its_own_marker_and_terminal_marker_is_recoverable() {
        for state in ["committed", "rejected"] {
            let f = Fixture::new(false);
            let writer = f.store.acquire(&f.project).unwrap();
            let attempt = writer
                .begin(
                    "pcb",
                    "geometry-plan",
                    "geometry-approval",
                    serde_json::json!({"geometry_evidence":{"bound":true}}),
                    std::slice::from_ref(&f.source),
                )
                .unwrap();
            let id = attempt.attempt_id().to_owned();
            let marker = fs::read(f.project.join(GEOMETRY_PENDING)).unwrap();
            assert!(clear_pending_geometry(&f.project, &attempt.path, "foreign-attempt").is_err());
            assert_eq!(fs::read(f.project.join(GEOMETRY_PENDING)).unwrap(), marker);
            attempt.finish(state, Value::Null).unwrap();
            assert!(pending_geometry(&f.project).unwrap().is_none());
            // Emulate interruption after terminal journal persistence but before marker cleanup.
            write_new(&f.project.join(GEOMETRY_PENDING), &marker).unwrap();
            drop(writer);
            let plan = f.store.plan_recovery(&f.project, &id).unwrap();
            let approval = f
                .store
                .preview_recovery(&f.project, &plan.recovery_id, &plan.plan_hash)
                .unwrap();
            f.store
                .commit_recovery(&f.project, &plan.recovery_id, &plan.plan_hash, &approval)
                .unwrap();
            assert!(pending_geometry(&f.project).unwrap().is_none());
        }
    }

    #[test]
    fn recovery_preserves_execution_history_and_allows_subsequent_writes() {
        let f = Fixture::new(false);
        let execution_path = f
            .store
            .records_dir(&f.project)
            .join(&f.id)
            .join("record.json");
        let original_record = fs::read(&execution_path).unwrap();
        let (p, a) = f.ready();
        assert_eq!(f.store.history(&f.project).unwrap().len(), 1);
        assert!(!f.commit(&p, "wrong").unwrap_err().unknown);
        assert_eq!(f.commit(&p, &a).unwrap().state, "restored");
        assert_eq!(fs::read(&f.source).unwrap(), b"before");
        assert_eq!(
            fs::read(f.source.with_extension("kicad_pro")).unwrap(),
            b"project-before"
        );
        assert_eq!(fs::read(&execution_path).unwrap(), original_record);
        assert!(f.commit(&p, &a).is_err());
        let writer = f.store.acquire(&f.project).unwrap();
        assert!(f.store.acquire(&f.project).is_err());
        assert!(
            ExecutionStore::open(&f.root.join("other-state"))
                .unwrap()
                .acquire(&f.project)
                .is_err()
        );
        assert!(
            writer
                .begin("pcb", "plan", "approval", Value::Null, &[])
                .is_err()
        );
        writer
            .begin(
                "pcb",
                "next-plan",
                "next-approval",
                Value::Null,
                std::slice::from_ref(&f.source),
            )
            .unwrap()
            .finish("committed", Value::Null)
            .unwrap();
        assert_eq!(f.store.history(&f.project).unwrap().len(), 2);
        assert_eq!(
            f.store.recovery_history(&f.project).unwrap()[0].state,
            "restored"
        );
    }

    #[test]
    fn recovery_rejects_later_edits_at_every_boundary() {
        for boundary in 0..3 {
            let f = Fixture::new(false);
            let p = if boundary > 0 {
                Some(f.store.plan_recovery(&f.project, &f.id).unwrap())
            } else {
                None
            };
            let a = if boundary > 1 {
                let p = p.as_ref().unwrap();
                Some(
                    f.store
                        .preview_recovery(&f.project, &p.recovery_id, &p.plan_hash)
                        .unwrap(),
                )
            } else {
                None
            };
            fs::write(&f.source, b"user-edit").unwrap();
            match boundary {
                0 => assert!(f.store.plan_recovery(&f.project, &f.id).is_err()),
                1 => {
                    let p = p.unwrap();
                    assert!(
                        f.store
                            .preview_recovery(&f.project, &p.recovery_id, &p.plan_hash)
                            .is_err()
                    );
                }
                _ => assert!(!f.commit(&p.unwrap(), &a.unwrap()).unwrap_err().unknown),
            }
            assert_eq!(fs::read(&f.source).unwrap(), b"user-edit");
            if boundary > 0 {
                assert_eq!(
                    f.store.recovery_history(&f.project).unwrap()[0].state,
                    "rejected"
                );
            }
        }
    }

    #[test]
    fn recovery_requires_post_attempt_evidence_for_changed_interrupted_files() {
        let f = Fixture::new(true);
        let reopened = ExecutionStore::open(&f.root.join("state")).unwrap();
        assert_eq!(
            reopened.history(&f.project).unwrap()[0].state,
            "in_flight_or_interrupted"
        );
        let (p, a) = f.ready();
        assert!(f.commit(&p, &a).is_ok());
        fs::write(&f.source, b"unattributed-change").unwrap();
        assert!(reopened.plan_recovery(&f.project, &f.id).is_err());
    }

    #[test]
    fn recovery_approvals_expire_on_restart_and_record_tampering_is_rejected() {
        let f = Fixture::new(false);
        let (p, a) = f.ready();
        let reopened = ExecutionStore::open(&f.root.join("state")).unwrap();
        assert!(
            reopened
                .commit_recovery(&f.project, &p.recovery_id, &p.plan_hash, &a)
                .is_err()
        );
        assert_eq!(
            reopened.recovery_history(&f.project).unwrap()[0].state,
            "expired"
        );
        let path = f
            .store
            .recovery_dir(&f.project)
            .join(&p.recovery_id)
            .join("record.json");
        let mut modified = p.clone();
        modified.files[0].original = f.root.join("outside.kicad_pcb");
        modified.plan_hash = recovery_hash(&modified).unwrap();
        fs::write(path, serde_json::to_vec(&modified).unwrap()).unwrap();
        assert!(f.commit(&modified, &a).is_err());
        assert!(
            f.store
                .preview_recovery(&f.project, "../escaped", &p.plan_hash)
                .is_err()
        );
        assert_eq!(fs::read(&f.source).unwrap(), b"attempt-write");
    }

    #[test]
    fn recovery_rejects_missing_tampered_and_redirected_inputs() {
        for fault in 0..5 {
            let f = Fixture::new(false);
            let (p, a) = f.ready();
            match fault {
                0 => fs::remove_file(&p.files[0].backup).unwrap(),
                1 => fs::write(&p.files[0].backup, b"tampered").unwrap(),
                2 => fs::remove_file(&f.source).unwrap(),
                3 => fs::write(f.project.join("~board.kicad_pcb.lck"), b"lock").unwrap(),
                _ => {
                    let outside = f.root.join("outside.kicad_pcb");
                    fs::write(&outside, b"attempt-write").unwrap();
                    fs::remove_file(&f.source).unwrap();
                    #[cfg(unix)]
                    std::os::unix::fs::symlink(&outside, &f.source).unwrap();
                }
            }
            assert!(!f.commit(&p, &a).unwrap_err().unknown);
            assert_eq!(
                f.store.recovery_history(&f.project).unwrap()[0].state,
                "rejected"
            );
        }
    }

    #[test]
    fn recovery_partial_and_terminal_failures_are_unknown_and_nonreplayable() {
        for fault in [0, usize::MAX] {
            let f = Fixture::new(false);
            let (p, a) = f.ready();
            let error = f
                .store
                .commit_recovery_internal(&f.project, &p.recovery_id, &p.plan_hash, &a, |index| {
                    if index == fault {
                        if fault == usize::MAX {
                            let pending = f
                                .store
                                .recovery_dir(&f.project)
                                .join(&p.recovery_id)
                                .join("record.pending");
                            write_new(&pending, b"interrupted journal")?;
                        } else {
                            return Err(invalid("injected post-write failure"));
                        }
                    }
                    Ok(())
                })
                .unwrap_err();
            assert!(error.unknown);
            assert_eq!(
                f.store.recovery_history(&f.project).unwrap()[0].state,
                "unknown"
            );
            assert_eq!(fs::read(&f.source).unwrap(), b"before");
            assert_eq!(
                fs::read(f.source.with_extension("kicad_pro")).unwrap(),
                if fault == 0 {
                    b"project-attempt-write".as_slice()
                } else {
                    b"project-before".as_slice()
                }
            );
            assert!(f.commit(&p, &a).is_err());
            assert!(f.store.plan_recovery(&f.project, &f.id).is_err());
        }
    }

    #[test]
    fn recovery_legacy_records_remain_inspectable_without_blocking_execution_history() {
        let f = Fixture::new(false);
        let (mut p, _) = f.ready();
        p.schema_version = 1;
        let directory = f.store.records_dir(&f.project).join(&p.recovery_id);
        fs::create_dir(&directory).unwrap();
        fs::write(
            directory.join("record.json"),
            serde_json::to_vec(&p).unwrap(),
        )
        .unwrap();
        assert_eq!(f.store.history(&f.project).unwrap().len(), 1);
        assert_eq!(f.store.recovery_history(&f.project).unwrap().len(), 2);
    }
    #[test]
    fn recovery_detects_external_writes_during_restore_and_final_readback() {
        for after_index in [0, 1] {
            let f = Fixture::new(false);
            let (p, a) = f.ready();
            let changed = if after_index == 0 {
                f.source.with_extension("kicad_pro")
            } else {
                f.source.clone()
            };
            let error = f
                .store
                .commit_recovery_internal(&f.project, &p.recovery_id, &p.plan_hash, &a, |index| {
                    if index == after_index {
                        fs::write(&changed, b"external-change")?;
                    }
                    Ok(())
                })
                .unwrap_err();
            assert!(error.unknown);
            assert_eq!(fs::read(&changed).unwrap(), b"external-change");
            assert_eq!(
                f.store.recovery_history(&f.project).unwrap()[0].state,
                "unknown"
            );
            assert!(f.commit(&p, &a).is_err());
            assert!(!fs::read_dir(&f.project).unwrap().any(|entry| {
                entry
                    .unwrap()
                    .file_name()
                    .to_string_lossy()
                    .ends_with(".auto-kicad-recovery.tmp")
            }));
        }
    }

    #[test]
    fn mixed_schematic_recovery_restores_bytes_absence_and_project_writer_readiness() {
        let f = Fixture::new(false);
        let root = f.project.join("root.kicad_sch");
        let child = f.project.join("child.kicad_sch");
        fs::write(&root, b"root-before").unwrap();
        let writer = f.store.acquire(&f.project).unwrap();
        let attempt = writer
            .begin_schematic(
                "mixed-plan",
                "mixed-approval",
                Value::Null,
                std::slice::from_ref(&root),
                std::slice::from_ref(&child),
            )
            .unwrap();
        assert_eq!(attempt.record.schema_version, 2);
        assert!(attempt.record.created_sources[0].post_attempt.is_none());
        let id = attempt.attempt_id().to_owned();
        let pending = fs::read(f.project.join(SCHEMATIC_PENDING)).unwrap();
        assert!(
            clear_pending_marker(&f.project, SCHEMATIC_PENDING, &attempt.path, "foreign").is_err()
        );
        assert_eq!(
            fs::read(f.project.join(SCHEMATIC_PENDING)).unwrap(),
            pending
        );
        fs::write(&root, b"root-after").unwrap();
        fs::write(&child, b"new-child").unwrap();
        attempt.finish("unknown", Value::Null).unwrap();
        drop(writer);
        let other = ExecutionStore::open(&f.root.join("other-state")).unwrap();
        for store in [&f.store, &other] {
            let writer = store.acquire(&f.project).unwrap();
            assert!(writer.ensure_ready().is_err());
            assert!(
                writer
                    .begin("pcb", "unrelated", "unrelated-approval", Value::Null, &[])
                    .is_err()
            );
        }
        let plan = f.store.plan_recovery(&f.project, &id).unwrap();
        assert_eq!(plan.schema_version, 3);
        assert_eq!(plan.files.len(), 1);
        assert_eq!(plan.created_sources.len(), 1);
        assert!(matches!(
            plan.created_sources[0].current,
            SourceExistence::Present { .. }
        ));
        let approval = f
            .store
            .preview_recovery(&f.project, &plan.recovery_id, &plan.plan_hash)
            .unwrap();
        assert!(
            f.store
                .commit_recovery(&f.project, &plan.recovery_id, &plan.plan_hash, "wrong")
                .is_err()
        );
        f.store
            .commit_recovery(&f.project, &plan.recovery_id, &plan.plan_hash, &approval)
            .unwrap();
        assert_eq!(fs::read(&root).unwrap(), b"root-before");
        assert!(!child.exists());
        assert!(
            pending_marker(&f.project, SCHEMATIC_PENDING)
                .unwrap()
                .is_none()
        );
        assert!(
            f.store
                .commit_recovery(&f.project, &plan.recovery_id, &plan.plan_hash, &approval)
                .is_err()
        );
        other
            .acquire(&f.project)
            .unwrap()
            .begin("pcb", "subsequent", "subsequent-approval", Value::Null, &[])
            .unwrap()
            .finish("committed", Value::Null)
            .unwrap();
        let execution = f.store.execution_record(&f.project, &id).unwrap();
        assert_eq!(execution.state, "unknown");
        assert!(matches!(
            execution.created_sources[0].post_attempt,
            Some(SourceExistence::Present { .. })
        ));
    }

    #[test]
    fn new_schematic_sources_require_confined_distinct_absence_and_observed_post_state() {
        for mode in 0..7 {
            let f = Fixture::new(false);
            let root = f.project.join("root.kicad_sch");
            let child = f.project.join("child.kicad_sch");
            fs::write(&root, b"before").unwrap();
            let writer = f.store.acquire(&f.project).unwrap();
            let (sources, created) = match mode {
                0 => (vec![], vec![]),
                1 => (vec![root.clone()], vec![root.clone()]),
                2 => (vec![root.clone()], vec![child.clone(), child.clone()]),
                3 => (vec![], vec![f.root.join("outside.kicad_sch")]),
                4 => (vec![], vec![f.project.join("child.kicad_pcb")]),
                5 => (vec![], vec![f.project.join("missing/child.kicad_sch")]),
                _ => {
                    #[cfg(unix)]
                    std::os::unix::fs::symlink(&root, &child).unwrap();
                    #[cfg(not(unix))]
                    fs::write(&child, b"collision").unwrap();
                    (vec![], vec![child])
                }
            };
            assert!(
                writer
                    .begin_schematic(
                        "plan",
                        "schematic-approval",
                        Value::Null,
                        &sources,
                        &created
                    )
                    .is_err()
            );
            assert!(
                pending_marker(&f.project, SCHEMATIC_PENDING)
                    .unwrap()
                    .is_none()
            );
        }
        for changed in [false, true] {
            let f = Fixture::new(false);
            let child = f.project.join("child.kicad_sch");
            let writer = f.store.acquire(&f.project).unwrap();
            let attempt = writer
                .begin_schematic(
                    "plan",
                    "schematic-approval",
                    Value::Null,
                    &[],
                    std::slice::from_ref(&child),
                )
                .unwrap();
            let id = attempt.attempt_id().to_owned();
            if changed {
                fs::write(&child, b"unobserved-new-file").unwrap();
            }
            drop(attempt);
            drop(writer);
            if changed {
                assert!(f.store.plan_recovery(&f.project, &id).is_err());
                assert_eq!(fs::read(&child).unwrap(), b"unobserved-new-file");
                assert!(f.store.acquire(&f.project).unwrap().ensure_ready().is_err());
            } else {
                let plan = f.store.plan_recovery(&f.project, &id).unwrap();
                assert_eq!(plan.created_sources[0].current, SourceExistence::Absent);
                let approval = f
                    .store
                    .preview_recovery(&f.project, &plan.recovery_id, &plan.plan_hash)
                    .unwrap();
                f.store
                    .commit_recovery(&f.project, &plan.recovery_id, &plan.plan_hash, &approval)
                    .unwrap();
                assert!(!child.exists());
            }
        }
    }

    #[test]
    fn mixed_recovery_rejects_later_edits_and_preserves_files_on_partial_failure() {
        for checkpoint in [None, Some(0), Some(1)] {
            let f = Fixture::new(false);
            let root = f.project.join("root.kicad_sch");
            let child = f.project.join("child.kicad_sch");
            fs::write(&root, b"before").unwrap();
            let writer = f.store.acquire(&f.project).unwrap();
            let attempt = writer
                .begin_schematic(
                    "plan",
                    "schematic-approval",
                    Value::Null,
                    std::slice::from_ref(&root),
                    std::slice::from_ref(&child),
                )
                .unwrap();
            let id = attempt.attempt_id().to_owned();
            fs::write(&root, b"after").unwrap();
            fs::write(&child, b"child-after").unwrap();
            attempt.finish("unknown", Value::Null).unwrap();
            drop(writer);
            let plan = f.store.plan_recovery(&f.project, &id).unwrap();
            let approval = f
                .store
                .preview_recovery(&f.project, &plan.recovery_id, &plan.plan_hash)
                .unwrap();
            if checkpoint.is_none() {
                fs::write(&child, b"user-edit").unwrap();
            }
            let error = f
                .store
                .commit_recovery_internal(
                    &f.project,
                    &plan.recovery_id,
                    &plan.plan_hash,
                    &approval,
                    |index| {
                        if checkpoint == Some(index) {
                            fs::write(&child, b"user-edit")?;
                        }
                        Ok(())
                    },
                )
                .unwrap_err();
            assert_eq!(error.unknown, checkpoint.is_some());
            assert_eq!(fs::read(&child).unwrap(), b"user-edit");
            assert_eq!(
                fs::read(&root).unwrap(),
                if checkpoint.is_none() {
                    b"after".as_slice()
                } else {
                    b"before".as_slice()
                }
            );
            assert!(f.store.acquire(&f.project).unwrap().ensure_ready().is_err());
            assert!(
                f.store
                    .commit_recovery(&f.project, &plan.recovery_id, &plan.plan_hash, &approval)
                    .is_err()
            );
        }
    }

    #[test]
    fn schematic_terminal_marker_recovery_and_legacy_json_remain_compatible() {
        for state in ["committed", "rejected"] {
            let f = Fixture::new(false);
            let child = f.project.join("child.kicad_sch");
            let writer = f.store.acquire(&f.project).unwrap();
            let attempt = writer
                .begin_schematic(
                    "plan",
                    "schematic-approval",
                    Value::Null,
                    &[],
                    std::slice::from_ref(&child),
                )
                .unwrap();
            let id = attempt.attempt_id().to_owned();
            let marker = fs::read(f.project.join(SCHEMATIC_PENDING)).unwrap();
            if state == "committed" {
                fs::write(&child, b"created").unwrap();
            }
            attempt.finish(state, Value::Null).unwrap();
            assert!(
                pending_marker(&f.project, SCHEMATIC_PENDING)
                    .unwrap()
                    .is_none()
            );
            write_new(&f.project.join(SCHEMATIC_PENDING), &marker).unwrap();
            drop(writer);
            let plan = f.store.plan_recovery(&f.project, &id).unwrap();
            let approval = f
                .store
                .preview_recovery(&f.project, &plan.recovery_id, &plan.plan_hash)
                .unwrap();
            f.store
                .commit_recovery(&f.project, &plan.recovery_id, &plan.plan_hash, &approval)
                .unwrap();
            assert!(!child.exists());
        }
        let f = Fixture::new(false);
        let record = f.store.execution_record(&f.project, &f.id).unwrap();
        let json = serde_json::to_value(&record).unwrap();
        assert_eq!(json["schema_version"], 1);
        assert!(json.get("created_sources").is_none());
        let (plan, _) = f.ready();
        assert_eq!(plan.schema_version, 2);
        assert!(
            serde_json::to_value(&plan)
                .unwrap()
                .get("created_sources")
                .is_none()
        );
    }

    #[test]
    fn recovery_supports_schematic_bytes_and_rejects_unsupported_domains() {
        for domain in ["schematic", "unsupported"] {
            let f = Fixture::new(false);
            let schematic = f.project.join("board.kicad_sch");
            fs::write(&schematic, b"schematic-before").unwrap();
            let writer = f.store.acquire(&f.project).unwrap();
            let attempt = writer
                .begin(
                    domain,
                    "sch-plan",
                    "sch-approval",
                    Value::Null,
                    std::slice::from_ref(&schematic),
                )
                .unwrap();
            let id = attempt.attempt_id().to_owned();
            fs::write(&schematic, b"schematic-attempt").unwrap();
            attempt.finish("unknown", Value::Null).unwrap();
            drop(writer);
            let plan = f.store.plan_recovery(&f.project, &id);
            if domain == "unsupported" {
                assert!(plan.is_err());
                continue;
            }
            let plan = plan.unwrap();
            let approval = f
                .store
                .preview_recovery(&f.project, &plan.recovery_id, &plan.plan_hash)
                .unwrap();
            f.store
                .commit_recovery(&f.project, &plan.recovery_id, &plan.plan_hash, &approval)
                .unwrap();
            assert_eq!(fs::read(schematic).unwrap(), b"schematic-before");
        }
    }
}
