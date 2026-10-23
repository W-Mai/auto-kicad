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

pub struct ProjectWriter {
    _lock: File,
    project: PathBuf,
    records: PathBuf,
}

pub struct ExecutionAttempt {
    path: PathBuf,
    record: ExecutionRecord,
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

impl ExecutionAttempt {
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
}
