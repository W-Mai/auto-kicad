use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

const MAX_FILE_BYTES: u64 = 128 * 1024 * 1024;
const MAX_RECORD_BYTES: u64 = 4 * 1024 * 1024;

#[derive(Clone, Debug)]
pub struct ExecutionStore {
    root: PathBuf,
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
    let mut bytes = Vec::new();
    File::open(path)?.take(limit + 1).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > limit {
        return Err(invalid("recovery file exceeds size limit"));
    }
    Ok(bytes)
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
    file.write_all(bytes)?;
    file.sync_all()
}

fn sync_directory(path: &Path) -> io::Result<()> {
    #[cfg(unix)]
    File::open(path)?.sync_all()?;
    Ok(())
}

impl ExecutionStore {
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
        Ok(Self { root })
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
        let lock = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(&path)?;
        lock.try_lock()
            .map_err(|_| invalid("project writer is busy"))?;
        let records = self.records_dir(&project);
        fs::create_dir_all(&records)?;
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
        let path = directory.join("record.json");
        if !path.exists() {
            continue;
        }
        let mut record: ExecutionRecord =
            serde_json::from_slice(&read_bounded(&path, MAX_RECORD_BYTES)?)?;
        if record.schema_version != 1 || record.project_dir != project {
            return Err(invalid("recovery record schema or project mismatch"));
        }
        if record.state == "in_flight" {
            record.state = "in_flight_or_interrupted".into();
        }
        if record.attempt_id.is_empty() {
            record.attempt_id = record
                .backups
                .first()
                .and_then(|backup| backup.saved.parent())
                .and_then(Path::file_name)
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
    pub fn begin(
        &self,
        domain: &str,
        plan_hash: &str,
        approval_hash: &str,
        evidence: Value,
        sources: &[PathBuf],
    ) -> io::Result<ExecutionAttempt> {
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
        let record = ExecutionRecord {
            schema_version: 1,
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
        };
        let path = directory.join("record.json");
        let bytes = serde_json::to_vec_pretty(&record)?;
        if bytes.len() as u64 > MAX_RECORD_BYTES {
            return Err(invalid("recovery evidence exceeds size limit"));
        }
        write_new(&path, &bytes)?;
        sync_directory(&directory)?;
        sync_directory(&self.records)?;
        Ok(ExecutionAttempt { path, record })
    }
}

impl ExecutionStore {
    fn recovery_dir(&self, project: &Path) -> PathBuf {
        self.records_dir(project)
    }

    fn read_recovery(&self, project: &Path, recovery_id: &str) -> io::Result<RecoveryAttempt> {
        let project = fs::canonicalize(project)?;
        if recovery_id.is_empty() {
            return Err(invalid("recovery identity is missing"));
        }
        let path = self
            .recovery_dir(&project)
            .join(recovery_id)
            .join("record.json");
        let record: RecoveryRecord =
            serde_json::from_slice(&read_bounded(&path, MAX_RECORD_BYTES)?)?;
        if record.schema_version != 1
            || record.project_dir != project
            || record.recovery_id != recovery_id
        {
            return Err(invalid("recovery record identity mismatch"));
        }
        Ok(RecoveryAttempt { path, record })
    }

    fn persist_recovery(attempt: &RecoveryAttempt) -> io::Result<()> {
        let bytes = serde_json::to_vec_pretty(&attempt.record)?;
        if bytes.len() as u64 > MAX_RECORD_BYTES {
            return Err(invalid("recovery outcome exceeds size limit"));
        }
        let temporary = attempt.path.with_extension("pending");
        write_new(&temporary, &bytes)?;
        fs::rename(&temporary, &attempt.path)?;
        sync_directory(attempt.path.parent().expect("recovery record has parent"))
    }

    pub fn plan_recovery(&self, project: &Path, attempt_id: &str) -> io::Result<RecoveryRecord> {
        let project = fs::canonicalize(project)?;
        let record = self
            .history(&project)?
            .into_iter()
            .find(|record| record.attempt_id == attempt_id)
            .ok_or_else(|| invalid("execution attempt was not found"))?;
        if !matches!(
            record.state.as_str(),
            "unknown" | "in_flight_or_interrupted"
        ) {
            return Err(invalid("execution attempt is not recoverable"));
        }
        if record.backups.is_empty() {
            return Err(invalid("execution attempt has no complete backups"));
        }
        let mut files = Vec::with_capacity(record.backups.len());
        for backup in &record.backups {
            if !backup.original.starts_with(&project) || !backup.saved.starts_with(&self.root) {
                return Err(invalid("recovery path escaped its safety boundary"));
            }
            let current = read_bounded(&backup.original, MAX_FILE_BYTES)?;
            let saved = read_bounded(&backup.saved, MAX_FILE_BYTES)?;
            let saved_hash = crate::digest_bytes(&saved).to_string();
            if saved_hash != backup.sha256 {
                return Err(invalid("recovery backup digest mismatch"));
            }
            files.push(RecoveryFile {
                original: backup.original.clone(),
                backup: backup.saved.clone(),
                current_sha256: crate::digest_bytes(&current).to_string(),
                backup_sha256: saved_hash,
            });
        }
        static SEQUENCE: AtomicU64 = AtomicU64::new(0);
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_err(io::Error::other)?
            .as_nanos();
        let recovery_id = format!(
            "recovery-{now:039}-{}-{}",
            std::process::id(),
            SEQUENCE.fetch_add(1, Ordering::Relaxed)
        );
        let plan_input = serde_json::json!({
            "attempt_id": record.attempt_id.clone(),
            "project_dir": project,
            "domain": record.domain.clone(),
            "action": "restore_backups",
            "files": files,
        });
        let plan_hash =
            crate::digest_bytes(&serde_json::to_vec(&plan_input).map_err(io::Error::other)?)
                .to_string();
        let recovery_dir = self.recovery_dir(&project).join(&recovery_id);
        fs::create_dir(&recovery_dir)?;
        let attempt = RecoveryAttempt {
            path: recovery_dir.join("record.json"),
            record: RecoveryRecord {
                schema_version: 1,
                recovery_id: recovery_id.clone(),
                attempt_id: record.attempt_id,
                project_dir: project,
                domain: record.domain,
                action: "restore_backups".into(),
                plan_hash,
                approval_hash: None,
                state: "prepared".into(),
                files,
                outcome: None,
            },
        };
        let bytes = serde_json::to_vec_pretty(&attempt.record)?;
        write_new(&attempt.path, &bytes)?;
        sync_directory(&recovery_dir)?;
        sync_directory(&self.recovery_dir(&attempt.record.project_dir))?;
        Ok(attempt.record)
    }

    fn check_recovery_files(record: &RecoveryRecord) -> io::Result<()> {
        for file in &record.files {
            let current = read_bounded(&file.original, MAX_FILE_BYTES)?;
            if crate::digest_bytes(&current).to_string() != file.current_sha256 {
                return Err(invalid("source files changed since recovery preparation"));
            }
            let backup = read_bounded(&file.backup, MAX_FILE_BYTES)?;
            if crate::digest_bytes(&backup).to_string() != file.backup_sha256 {
                return Err(invalid("recovery backup changed since preparation"));
            }
        }
        Ok(())
    }

    pub fn preview_recovery(
        &self,
        project: &Path,
        recovery_id: &str,
        plan_hash: &str,
    ) -> io::Result<String> {
        let mut attempt = self.read_recovery(project, recovery_id)?;
        if attempt.record.plan_hash != plan_hash || attempt.record.state != "prepared" {
            return Err(invalid("recovery plan is stale or already used"));
        }
        Self::check_recovery_files(&attempt.record)?;
        let approval =
            crate::digest_bytes(format!("{}:{}", plan_hash, attempt.record.attempt_id).as_bytes())
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
    ) -> io::Result<RecoveryRecord> {
        self.commit_recovery_internal(project, recovery_id, plan_hash, approval_hash, None)
    }

    fn commit_recovery_internal(
        &self,
        project: &Path,
        recovery_id: &str,
        plan_hash: &str,
        approval_hash: &str,
        fail_after: Option<usize>,
    ) -> io::Result<RecoveryRecord> {
        let mut attempt = self.read_recovery(project, recovery_id)?;
        if attempt.record.plan_hash != plan_hash
            || attempt.record.state != "awaiting_approval"
            || attempt.record.approval_hash.as_deref() != Some(approval_hash)
        {
            return Err(invalid("recovery approval mismatch or state is not ready"));
        }
        Self::check_recovery_files(&attempt.record)?;
        attempt.record.state = "in_flight".into();
        Self::persist_recovery(&attempt)?;
        let result = (|| {
            for (index, file) in attempt.record.files.iter().enumerate() {
                let bytes = read_bounded(&file.backup, MAX_FILE_BYTES)?;
                let temporary = file
                    .original
                    .with_file_name(format!(".{}.auto-kicad-recovery.tmp", recovery_id));
                write_new(&temporary, &bytes)?;
                fs::rename(&temporary, &file.original)?;
                sync_directory(
                    file.original
                        .parent()
                        .ok_or_else(|| invalid("recovery source has no parent"))?,
                )?;
                let restored = read_bounded(&file.original, MAX_FILE_BYTES)?;
                if crate::digest_bytes(&restored).to_string() != file.backup_sha256 {
                    return Err(invalid("restored file digest did not match backup"));
                }
                if fail_after == Some(index) {
                    return Err(invalid("injected recovery write failure"));
                }
            }
            Ok::<(), io::Error>(())
        })();
        match result {
            Ok(()) => {
                attempt.record.state = "restored".into();
                attempt.record.outcome = Some(serde_json::json!({"kind": "restored"}));
                Self::persist_recovery(&attempt)?;
                Ok(attempt.record)
            }
            Err(error) => {
                attempt.record.state = "unknown".into();
                attempt.record.outcome = Some(serde_json::json!({
                    "kind": "unknown",
                    "reason": error.to_string()
                }));
                let _ = Self::persist_recovery(&attempt);
                Err(error)
            }
        }
    }

    #[cfg(test)]
    fn commit_recovery_with_fault(
        &self,
        project: &Path,
        recovery_id: &str,
        plan_hash: &str,
        approval_hash: &str,
        fail_after: usize,
    ) -> io::Result<RecoveryRecord> {
        self.commit_recovery_internal(
            project,
            recovery_id,
            plan_hash,
            approval_hash,
            Some(fail_after),
        )
    }

    pub fn recovery_history(&self, project: &Path) -> io::Result<Vec<RecoveryRecord>> {
        let project = fs::canonicalize(project)?;
        let root = self.recovery_dir(&project);
        let entries = match fs::read_dir(&root) {
            Ok(entries) => entries,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(error) => return Err(error),
        };
        let mut records = Vec::new();
        for entry in entries {
            let directory = entry?.path();
            if !directory
                .file_name()
                .is_some_and(|name| name.to_string_lossy().starts_with("recovery-"))
            {
                continue;
            }
            let path = directory.join("record.json");
            if !path.exists() {
                continue;
            }
            let record: RecoveryRecord =
                serde_json::from_slice(&read_bounded(&path, MAX_RECORD_BYTES)?)?;
            if record.project_dir == project && record.schema_version == 1 {
                records.push(record);
            }
        }
        records.sort_by(|left, right| left.recovery_id.cmp(&right.recovery_id));
        records.reverse();
        Ok(records)
    }
}

impl ExecutionAttempt {
    pub fn attempt_id(&self) -> &str {
        &self.record.attempt_id
    }

    pub fn finish(mut self, state: &str, outcome: Value) -> io::Result<()> {
        self.record.state = state.into();
        self.record.outcome = Some(outcome);
        let bytes = serde_json::to_vec_pretty(&self.record)?;
        if bytes.len() as u64 > MAX_RECORD_BYTES {
            return Err(invalid("recovery outcome exceeds size limit"));
        }
        let temporary = self.path.with_extension("pending");
        write_new(&temporary, &bytes)?;
        fs::rename(&temporary, &self.path)?;
        sync_directory(self.path.parent().expect("record has parent"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn writer_exclusion_and_interrupted_evidence_survive_store_recreation() {
        let root = std::env::temp_dir().join(format!(
            "auto-kicad-recovery-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let project = root.join("project");
        fs::create_dir_all(&project).unwrap();
        let source = project.join("board.kicad_pcb");
        fs::write(&source, b"saved source").unwrap();
        let store = ExecutionStore::open(&root.join("state")).unwrap();
        let writer = store.acquire(&project).unwrap();
        assert!(store.acquire(&project).is_err());
        let other_store = ExecutionStore::open(&root.join("other-state")).unwrap();
        assert!(other_store.acquire(&project).is_err());
        let attempt = writer
            .begin(
                "pcb",
                "plan",
                "approval",
                serde_json::json!({"revision": "before"}),
                std::slice::from_ref(&source),
            )
            .unwrap();
        drop(attempt);
        drop(writer);
        let reopened = ExecutionStore::open(&root.join("state")).unwrap();
        let history = reopened.history(&project).unwrap();
        assert_eq!(history[0].state, "in_flight_or_interrupted");
        assert_eq!(
            fs::read(&history[0].backups[0].saved).unwrap(),
            b"saved source"
        );
        let writer = reopened.acquire(&project).unwrap();
        assert!(
            writer
                .begin("pcb", "plan", "approval", Value::Null, &[])
                .is_err()
        );
        let attempt = writer
            .begin(
                "pcb",
                "fresh-plan",
                "fresh-approval",
                Value::Null,
                &[source],
            )
            .unwrap();
        attempt.finish("committed", Value::Null).unwrap();
        assert_eq!(reopened.history(&project).unwrap()[0].state, "committed");
        drop(writer);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn guarded_recovery_restores_only_after_fresh_hash_checks() {
        let root = std::env::temp_dir().join(format!(
            "auto-kicad-guarded-recovery-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let project = root.join("project");
        fs::create_dir_all(&project).unwrap();
        let source = project.join("board.kicad_pcb");
        fs::write(&source, b"before").unwrap();
        let store = ExecutionStore::open(&root.join("state")).unwrap();
        let writer = store.acquire(&project).unwrap();
        let attempt = writer
            .begin(
                "pcb",
                "plan",
                "approval",
                Value::Null,
                std::slice::from_ref(&source),
            )
            .unwrap();
        let attempt_id = attempt.record.attempt_id.clone();
        attempt
            .finish("unknown", serde_json::json!({"kind": "unknown"}))
            .unwrap();
        fs::write(&source, b"later user edit").unwrap();
        let plan = store.plan_recovery(&project, &attempt_id).unwrap();
        let approval = store
            .preview_recovery(&project, &plan.recovery_id, &plan.plan_hash)
            .unwrap();
        let restored = store
            .commit_recovery(&project, &plan.recovery_id, &plan.plan_hash, &approval)
            .unwrap();
        assert_eq!(restored.state, "restored");
        assert_eq!(fs::read(&source).unwrap(), b"before");
        drop(writer);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn guarded_recovery_rejects_edits_after_preview_without_writing() {
        let root = std::env::temp_dir().join(format!(
            "auto-kicad-guarded-recovery-drift-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let project = root.join("project");
        fs::create_dir_all(&project).unwrap();
        let source = project.join("board.kicad_pcb");
        fs::write(&source, b"before").unwrap();
        let store = ExecutionStore::open(&root.join("state")).unwrap();
        let writer = store.acquire(&project).unwrap();
        let attempt = writer
            .begin(
                "pcb",
                "plan",
                "approval",
                Value::Null,
                std::slice::from_ref(&source),
            )
            .unwrap();
        let attempt_id = attempt.record.attempt_id.clone();
        attempt.finish("unknown", Value::Null).unwrap();
        fs::write(&source, b"first edit").unwrap();
        let plan = store.plan_recovery(&project, &attempt_id).unwrap();
        let approval = store
            .preview_recovery(&project, &plan.recovery_id, &plan.plan_hash)
            .unwrap();
        fs::write(&source, b"second edit").unwrap();
        assert!(
            store
                .commit_recovery(&project, &plan.recovery_id, &plan.plan_hash, &approval)
                .is_err()
        );
        assert_eq!(fs::read(&source).unwrap(), b"second edit");
        let state = store
            .recovery_history(&project)
            .unwrap()
            .into_iter()
            .find(|record| record.recovery_id == plan.recovery_id)
            .unwrap()
            .state;
        assert_eq!(state, "awaiting_approval");
        drop(writer);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn guarded_recovery_marks_partial_restore_unknown() {
        let root = std::env::temp_dir().join(format!(
            "auto-kicad-guarded-recovery-fault-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let project = root.join("project");
        fs::create_dir_all(&project).unwrap();
        let first = project.join("board.kicad_pcb");
        let second = project.join("board.kicad_pro");
        fs::write(&first, b"board-before").unwrap();
        fs::write(&second, b"project-before").unwrap();
        let store = ExecutionStore::open(&root.join("state")).unwrap();
        let writer = store.acquire(&project).unwrap();
        let attempt = writer
            .begin(
                "pcb",
                "plan",
                "approval",
                Value::Null,
                &[first.clone(), second.clone()],
            )
            .unwrap();
        let attempt_id = attempt.record.attempt_id.clone();
        attempt.finish("unknown", Value::Null).unwrap();
        fs::write(&first, b"board-later").unwrap();
        fs::write(&second, b"project-later").unwrap();
        let plan = store.plan_recovery(&project, &attempt_id).unwrap();
        let approval = store
            .preview_recovery(&project, &plan.recovery_id, &plan.plan_hash)
            .unwrap();
        assert!(
            store
                .commit_recovery_with_fault(
                    &project,
                    &plan.recovery_id,
                    &plan.plan_hash,
                    &approval,
                    0,
                )
                .is_err()
        );
        assert_eq!(fs::read(&first).unwrap(), b"board-before");
        assert_eq!(fs::read(&second).unwrap(), b"project-later");
        let recovery = store
            .recovery_history(&project)
            .unwrap()
            .into_iter()
            .find(|record| record.recovery_id == plan.recovery_id)
            .unwrap();
        assert_eq!(recovery.state, "unknown");
        drop(writer);
        fs::remove_dir_all(root).unwrap();
    }
}
