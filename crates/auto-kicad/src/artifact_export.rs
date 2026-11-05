//! Source-bound export sessions and exclusive durable directory publication.
use crate::{digest_text, library_settings, recovery::ExecutionStore};
use kicad_adapter::schematic_audit::{
    SchematicAudit,
    artifact_export::{ArtifactFormat, NativeArtifactExport},
};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
#[cfg(unix)]
use std::os::unix::{
    ffi::OsStrExt,
    fs::{MetadataExt, OpenOptionsExt, PermissionsExt},
};
use std::{
    collections::BTreeMap,
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    path::{Component, Path, PathBuf},
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, Ordering},
    },
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

static JOURNAL_SEQUENCE: AtomicU64 = AtomicU64::new(0);
const PLAN_PREFIX: &str = "artifact-export-plan:";
const ATTEMPT_PREFIX: &str = "artifact-export-attempt:";
const RECOVERY_PREFIX: &str = "artifact-export-recovery:";
const PENDING: &str = ".auto-kicad-artifact-export-pending";
const TTL: Duration = Duration::from_secs(900);
const MAX_BYTES: u64 = 256 * 1024 * 1024;

#[derive(Clone, Debug, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Inspection {
    Observe {
        project_root: String,
        root_schematic: String,
    },
    Release {
        #[serde(default)]
        observation_ids: Vec<String>,
        #[serde(default)]
        plan_ids: Vec<String>,
    },
    History {
        project_root: String,
    },
}
#[derive(Clone, Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Request {
    pub schema_version: u32,
    pub observation_id: String,
    pub observed_revision: String,
    pub output_dir: String,
    pub formats: Vec<ArtifactFormat>,
}
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ExportFile {
    pub path: String,
    pub format: String,
    pub bytes: u64,
    pub sha256: String,
}
#[derive(Clone, Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Expectation {
    pub files: Vec<ExportFile>,
}
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Fingerprint {
    path: PathBuf,
    canonical: Option<PathBuf>,
    sha256: Option<String>,
    bytes: u64,
}
impl Fingerprint {
    fn capture(path: &Path) -> Result<Self, String> {
        if !path.exists() && fs::symlink_metadata(path).is_err() {
            return Ok(Self {
                path: path.into(),
                canonical: None,
                sha256: None,
                bytes: 0,
            });
        }
        let canonical = fs::canonicalize(path).map_err(|e| e.to_string())?;
        let bytes = read_bound(&canonical, 128 * 1024 * 1024)?;
        Ok(Self {
            path: path.into(),
            canonical: Some(canonical),
            sha256: Some(hash(&bytes)),
            bytes: bytes.len() as u64,
        })
    }
    fn verify(&self) -> Result<(), String> {
        if Self::capture(&self.path)? != *self {
            Err("saved export input changed".into())
        } else {
            Ok(())
        }
    }
}
struct Observation {
    audit: SchematicAudit,
    board: Fingerprint,
    project: PathBuf,
    revision: String,
    created: Instant,
}
struct Prepared {
    native: NativeArtifactExport,
    target: PathBuf,
    parent: PathBuf,
    parent_identity: [u64; 2],
    files: Vec<ExportFile>,
    manifest: Vec<u8>,
    board: Fingerprint,
    project: PathBuf,
}
impl Prepared {
    fn verify(&self) -> Result<(), String> {
        self.native.verify()?;
        self.board.verify()?;
        if canonical_directory(&self.parent)? != self.parent
            || directory_identity(&self.parent)? != self.parent_identity
        {
            return Err("export destination parent changed".into());
        }
        if fs::symlink_metadata(&self.target).is_ok() {
            return Err("export destination already exists; overwrites are not supported".into());
        }
        Ok(())
    }
}
enum Phase {
    Planned(Arc<Prepared>),
    Approved {
        prepared: Arc<Prepared>,
        approval: String,
        validation: String,
    },
    Busy,
    Terminal(Value),
}
struct Plan {
    hash: String,
    project: PathBuf,
    created: Instant,
    phase: Mutex<Phase>,
}
struct Recovery {
    project: PathBuf,
    record_path: PathBuf,
    record_sha256: String,
    hash: String,
    action: String,
    created: Instant,
    approval: Option<String>,
}
#[derive(Default)]
struct State {
    observations: BTreeMap<String, Arc<Observation>>,
    plans: BTreeMap<String, Arc<Plan>>,
    recoveries: BTreeMap<String, Recovery>,
    busy: usize,
}
pub(crate) struct Registry {
    state: Mutex<State>,
    sequence: AtomicU64,
    nonce: String,
}
impl Default for Registry {
    fn default() -> Self {
        Self {
            state: Mutex::new(State::default()),
            sequence: AtomicU64::new(0),
            nonce: format!(
                "{}-{}",
                std::process::id(),
                SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .expect("clock")
                    .as_nanos()
            ),
        }
    }
}
fn hash(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}
pub(crate) fn is_plan(id: &str) -> bool {
    id.starts_with(PLAN_PREFIX)
}
pub(crate) fn is_attempt(id: &str) -> bool {
    id.starts_with(ATTEMPT_PREFIX)
}
pub(crate) fn is_recovery(id: &str) -> bool {
    id.starts_with(RECOVERY_PREFIX)
}
fn canonical_directory(path: &Path) -> Result<PathBuf, String> {
    let value = fs::canonicalize(path).map_err(|e| e.to_string())?;
    if value != path || !value.is_dir() {
        return Err("export paths must be canonical directories".into());
    }
    Ok(value)
}
fn directory_identity(path: &Path) -> Result<[u64; 2], String> {
    let metadata = fs::symlink_metadata(path).map_err(|e| e.to_string())?;
    if !metadata.is_dir() {
        return Err("export parent is not a plain directory".into());
    }
    #[cfg(unix)]
    {
        Ok([metadata.dev(), metadata.ino()])
    }
    #[cfg(not(unix))]
    {
        Err("export publication is unavailable on this platform".into())
    }
}
fn read_bound(path: &Path, limit: u64) -> Result<Vec<u8>, String> {
    let metadata = fs::symlink_metadata(path).map_err(|e| e.to_string())?;
    if !metadata.is_file() || metadata.len() > limit {
        return Err("export input is not regular or exceeds bounds".into());
    }
    let mut bytes = Vec::new();
    File::open(path)
        .map_err(|e| e.to_string())?
        .take(limit + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| e.to_string())?;
    if bytes.len() as u64 != metadata.len() {
        return Err("export input changed while reading".into());
    }
    Ok(bytes)
}
fn sync_directory(path: &Path) -> Result<(), String> {
    File::open(path)
        .and_then(|file| file.sync_all())
        .map_err(|e| e.to_string())
}
fn directory(path: &Path) -> Result<(), String> {
    if let Ok(metadata) = fs::symlink_metadata(path) {
        if !metadata.is_dir() {
            return Err("export state contains a symlink or special file".into());
        }
        return Ok(());
    }
    fs::create_dir(path).map_err(|e| e.to_string())?;
    #[cfg(unix)]
    fs::set_permissions(path, fs::Permissions::from_mode(0o700)).map_err(|e| e.to_string())?;
    sync_directory(path.parent().ok_or("missing directory parent")?)
}
fn write_new(path: &Path, bytes: &[u8]) -> Result<(), String> {
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    options.mode(0o600).custom_flags(libc::O_NOFOLLOW);
    let mut file = options.open(path).map_err(|e| e.to_string())?;
    file.write_all(bytes)
        .and_then(|_| file.sync_all())
        .map_err(|e| e.to_string())
}
fn write_json(path: &Path, value: &impl Serialize) -> Result<(), String> {
    let bytes = serde_json::to_vec_pretty(value).map_err(|e| e.to_string())?;
    if bytes.len() > 8 * 1024 * 1024 {
        return Err("export journal exceeds bound".into());
    }
    let stage = path.with_extension(format!(
        "json.staging-{}-{}",
        std::process::id(),
        JOURNAL_SEQUENCE.fetch_add(1, Ordering::Relaxed)
    ));
    write_new(&stage, &bytes)?;
    fs::rename(&stage, path).map_err(|e| e.to_string())?;
    sync_directory(path.parent().ok_or("missing journal parent")?)
}
fn exclusive_rename(source: &Path, target: &Path) -> Result<(), String> {
    #[cfg(unix)]
    {
        let source =
            std::ffi::CString::new(source.as_os_str().as_bytes()).map_err(|e| e.to_string())?;
        let target =
            std::ffi::CString::new(target.as_os_str().as_bytes()).map_err(|e| e.to_string())?;
        #[cfg(target_os = "macos")]
        let status = unsafe {
            libc::renameatx_np(
                libc::AT_FDCWD,
                source.as_ptr(),
                libc::AT_FDCWD,
                target.as_ptr(),
                libc::RENAME_EXCL,
            )
        };
        #[cfg(target_os = "linux")]
        let status = unsafe {
            libc::renameat2(
                libc::AT_FDCWD,
                source.as_ptr(),
                libc::AT_FDCWD,
                target.as_ptr(),
                libc::RENAME_NOREPLACE,
            )
        };
        #[cfg(not(any(target_os = "macos", target_os = "linux")))]
        return Err("exclusive directory publication is unavailable".into());
        #[cfg(any(target_os = "macos", target_os = "linux"))]
        if status != 0 {
            return Err(std::io::Error::last_os_error().to_string());
        }
        Ok(())
    }
    #[cfg(not(unix))]
    {
        Err("exclusive directory publication is unavailable".into())
    }
}
fn destination(project: &Path, relative: &str) -> Result<(PathBuf, PathBuf, [u64; 2]), String> {
    let path = Path::new(relative);
    if relative.is_empty()
        || relative.len() > 512
        || path.is_absolute()
        || path.components().count() > 5
        || path
            .components()
            .any(|part| !matches!(part, Component::Normal(_)))
        || path.components().any(|part| {
            part.as_os_str() == ".git"
                || part
                    .as_os_str()
                    .to_string_lossy()
                    .starts_with(".auto-kicad")
        })
    {
        return Err("export destination must be a bounded relative project directory".into());
    }
    let target = project.join(path);
    let parent = canonical_directory(target.parent().ok_or("missing output parent")?)?;
    if !parent.starts_with(project) || fs::symlink_metadata(&target).is_ok() {
        return Err("export destination is occupied or outside the project".into());
    }
    let identity = directory_identity(&parent)?;
    Ok((target, parent, identity))
}

impl Registry {
    fn id(&self, prefix: &str) -> String {
        format!(
            "{prefix}{}:{}",
            self.nonce,
            self.sequence.fetch_add(1, Ordering::Relaxed)
        )
    }
    fn prune(state: &mut State) {
        state
            .observations
            .retain(|_, observation| observation.created.elapsed() < TTL);
        state.plans.retain(|_, plan| {
            plan.created.elapsed() < TTL
                || matches!(*plan.phase.lock().expect("export phase"), Phase::Busy)
        });
        state
            .recoveries
            .retain(|_, recovery| recovery.created.elapsed() < TTL);
        while state.plans.len() > 36 {
            if let Some(id) = state.plans.iter().find_map(|(id, plan)| {
                matches!(
                    *plan.phase.lock().expect("export phase"),
                    Phase::Terminal(_)
                )
                .then(|| id.clone())
            }) {
                state.plans.remove(&id);
            } else {
                break;
            }
        }
    }
    pub(crate) fn inspect(
        &self,
        settings: &library_settings::SharedLibrarySettings,
        store: Option<&ExecutionStore>,
        query: Inspection,
    ) -> Result<Value, String> {
        match query {
            Inspection::Observe {
                project_root,
                root_schematic,
            } => {
                {
                    let mut state = self.state.lock().expect("export registry");
                    Self::prune(&mut state);
                    if state.observations.len() + state.busy >= 4 {
                        return Err(
                            "export observation limit reached; release old observations".into()
                        );
                    }
                    state.busy += 1;
                }
                let result = (|| -> Result<_, String> {
                    let project = canonical_directory(Path::new(&project_root))?;
                    let source = fs::canonicalize(&root_schematic).map_err(|e| e.to_string())?;
                    if source != Path::new(&root_schematic) || !source.starts_with(&project) {
                        return Err("artifact source must be canonical and confined".into());
                    }
                    let policy = settings
                        .as_ref()
                        .as_ref()
                        .map_err(Clone::clone)?
                        .schematic_policy(&project, &source)?;
                    let audit = SchematicAudit::prepare(&policy)?;
                    let board = Fingerprint::capture(&source.with_extension("kicad_pcb"))?;
                    let revision = digest_text(&[
                        "artifact-export-observation-v1",
                        audit.revision(),
                        &serde_json::to_string(&board).map_err(|e| e.to_string())?,
                    ]);
                    audit.verify()?;
                    board.verify()?;
                    let id = self.id("artifact-export-observation:");
                    let response = json!({"schema_version":1,"observation_id":id,"revision":revision,"project_root":project,"root_schematic":source,
                        "board_sha256":board.sha256,"formats":["gerber","drill","bom","schematic_pdf"],"source":"saved_documents","source_context":audit.evidence()?,"expires_in_seconds":900});
                    Ok((
                        id,
                        Arc::new(Observation {
                            audit,
                            board,
                            project,
                            revision,
                            created: Instant::now(),
                        }),
                        response,
                    ))
                })();
                let mut state = self.state.lock().expect("export registry");
                state.busy -= 1;
                let (id, observation, response) = result?;
                state.observations.insert(id, observation);
                Ok(response)
            }
            Inspection::Release {
                observation_ids,
                plan_ids,
            } => {
                if observation_ids.len() > 4 || plan_ids.len() > 36 {
                    return Err("export release exceeds bounds".into());
                }
                let mut state = self.state.lock().expect("export registry");
                for id in &plan_ids {
                    if state.plans.get(id).is_some_and(|plan| {
                        matches!(*plan.phase.lock().expect("export phase"), Phase::Busy)
                    }) {
                        return Err("export plan is busy".into());
                    }
                }
                for id in observation_ids {
                    state.observations.remove(&id);
                }
                for id in plan_ids {
                    state.plans.remove(&id);
                }
                Ok(json!({"released":true}))
            }
            Inspection::History { project_root } => {
                let store = store.ok_or("durable export state is unavailable")?;
                let project = canonical_directory(Path::new(&project_root))?;
                let root = store.artifact_export_root();
                let mut rows = Vec::new();
                if root.exists() {
                    for entry in fs::read_dir(root).map_err(|e| e.to_string())?.take(4097) {
                        let entry = entry.map_err(|e| e.to_string())?;
                        let path = entry.path().join("record.json");
                        if !path.is_file() {
                            continue;
                        }
                        let value: Value =
                            serde_json::from_slice(&read_bound(&path, 8 * 1024 * 1024)?)
                                .map_err(|e| e.to_string())?;
                        if value["project_root"].as_str() == project.to_str() {
                            rows.push(value);
                        }
                        if rows.len() > 4096 {
                            return Err("export history exceeds scan bound".into());
                        }
                    }
                }
                rows.sort_by(|a, b| b["attempt_id"].as_str().cmp(&a["attempt_id"].as_str()));
                rows.truncate(32);
                let state = self.state.lock().expect("export registry");
                let terminal = state
                    .plans
                    .values()
                    .filter(|plan| plan.project == project)
                    .filter_map(|plan| {
                        let phase = plan.phase.lock().expect("export phase");
                        if let Phase::Terminal(value) = &*phase {
                            Some(value.clone())
                        } else {
                            None
                        }
                    })
                    .collect::<Vec<_>>();
                Ok(json!({"history":rows,"session_results":terminal}))
            }
        }
    }
    pub(crate) fn plan(
        &self,
        store: &ExecutionStore,
        run: &str,
        request_id: &str,
        request: Request,
    ) -> Result<Value, String> {
        if request.schema_version != 1 || request.formats.is_empty() || request.formats.len() > 4 {
            return Err("unsupported export request version or formats".into());
        }
        let observation = {
            let mut state = self.state.lock().expect("export registry");
            Self::prune(&mut state);
            if state
                .plans
                .values()
                .filter(|plan| {
                    !matches!(
                        *plan.phase.lock().expect("export phase"),
                        Phase::Terminal(_)
                    )
                })
                .count()
                + state.busy
                >= 4
            {
                return Err("export plan limit reached".into());
            }
            let observed = state
                .observations
                .get(&request.observation_id)
                .cloned()
                .ok_or("unknown or expired export observation")?;
            if observed.revision != request.observed_revision {
                return Err("export observation revision mismatch".into());
            }
            state.busy += 1;
            observed
        };
        let result = (|| -> Result<_, String> {
            store
                .check_project_ready(&observation.project)
                .map_err(|e| e.to_string())?;
            pending_absent(&observation.project)?;
            observation.audit.verify()?;
            observation.board.verify()?;
            let (target, parent, parent_identity) =
                destination(&observation.project, &request.output_dir)?;
            let native = observation
                .audit
                .prepare_artifact_export(&request.formats)?;
            observation.board.verify()?;
            let mut files = native
                .files()
                .iter()
                .map(|file| ExportFile {
                    path: file.path.clone(),
                    format: serde_json::to_value(file.format)
                        .expect("format")
                        .as_str()
                        .expect("format string")
                        .into(),
                    bytes: file.bytes,
                    sha256: file.sha256.clone(),
                })
                .collect::<Vec<_>>();
            let manifest=serde_json::to_vec_pretty(&json!({"schema_version":1,"native_version":"10.0.7","source_revision":observation.revision,"formats":request.formats,"files":files})).map_err(|e|e.to_string())?;
            files.push(ExportFile {
                path: "auto-kicad-manifest.json".into(),
                format: "manifest".into(),
                bytes: manifest.len() as u64,
                sha256: hash(&manifest),
            });
            files.sort_by(|a, b| a.path.cmp(&b.path));
            let prepared = Arc::new(Prepared {
                native,
                target,
                parent,
                parent_identity,
                files,
                manifest,
                board: observation.board.clone(),
                project: observation.project.clone(),
            });
            prepared.verify()?;
            if prepared.files.iter().map(|file| file.bytes).sum::<u64>() > MAX_BYTES {
                return Err("complete export including manifest exceeds byte bound".into());
            }
            let id = self.id(PLAN_PREFIX);
            let hash = digest_text(&[
                "artifact-export-plan-v1",
                &self.nonce,
                &id,
                run,
                request_id,
                &request.observed_revision,
                &request.output_dir,
                prepared.native.digest(),
                &serde_json::to_string(&prepared.files).map_err(|e| e.to_string())?,
            ]);
            let response = json!({"schema_version":1,"plan_id":id,"plan_hash":hash,"state":"planned","output_dir":prepared.target,"files":prepared.files,"source_revision":observation.revision,"native_preview":{"native_version":"10.0.7","candidate_digest":prepared.native.digest(),"evidence":prepared.native.evidence_reference()?,"sheet_count":prepared.native.evidence()["sheet_count"],"physical_hole_count":prepared.native.evidence()["physical"]["physical_holes"].as_array().map(Vec::len)},"requires_complete_inventory_expectation":true,"expires_in_seconds":900});
            Ok((
                id,
                Arc::new(Plan {
                    hash,
                    project: prepared.project.clone(),
                    created: Instant::now(),
                    phase: Mutex::new(Phase::Planned(prepared)),
                }),
                response,
            ))
        })();
        let mut state = self.state.lock().expect("export registry");
        state.busy -= 1;
        let (id, plan, response) = result?;
        state.plans.insert(id, plan);
        Ok(response)
    }
    fn get(&self, id: &str, hash: &str) -> Result<Arc<Plan>, String> {
        let mut state = self.state.lock().expect("export registry");
        Self::prune(&mut state);
        let plan = state
            .plans
            .get(id)
            .cloned()
            .ok_or("unknown or expired export plan")?;
        if plan.hash != hash {
            return Err("export plan hash mismatch".into());
        }
        Ok(plan)
    }
    pub(crate) fn preview(
        &self,
        id: &str,
        hash: &str,
        expected: Expectation,
    ) -> Result<Value, String> {
        let plan = self.get(id, hash)?;
        let mut phase = plan.phase.lock().expect("export phase");
        let prepared = match &*phase {
            Phase::Planned(value)
            | Phase::Approved {
                prepared: value, ..
            } => value.clone(),
            _ => return Err("export plan cannot be approved in its current state".into()),
        };
        if expected.files != prepared.files {
            return Err("complete artifact inventory differs from expectation".into());
        }
        if let Err(error) = prepared.verify() {
            *phase = Phase::Terminal(json!({"state":"rejected","reason":error}));
            return Err(error);
        }
        let validation = digest_text(&[
            "artifact-export-validation-v1",
            hash,
            prepared.native.digest(),
            &serde_json::to_string(&expected.files).map_err(|e| e.to_string())?,
        ]);
        let approval = digest_text(&[
            "artifact-export-approval-v1",
            &self.nonce,
            id,
            hash,
            &validation,
        ]);
        *phase = Phase::Approved {
            prepared,
            approval: approval.clone(),
            validation: validation.clone(),
        };
        Ok(
            json!({"state":"awaiting_approval","plan_id":id,"plan_hash":hash,"approval_hash":approval,"validation_evidence_hash":validation}),
        )
    }
    pub(crate) fn commit(
        &self,
        store: &ExecutionStore,
        id: &str,
        hash: &str,
        approval: &str,
        validation: &str,
    ) -> Result<Value, String> {
        let plan = self.get(id, hash)?;
        let prepared = {
            let mut phase = plan.phase.lock().expect("export phase");
            match &*phase {
                Phase::Approved {
                    prepared,
                    approval: a,
                    validation: v,
                } if a == approval && v == validation => {
                    let result = prepared.clone();
                    *phase = Phase::Busy;
                    result
                }
                _ => return Err("exact current export approval and validation are required".into()),
            }
        };
        let result = self.publish(store, &plan, prepared);
        let value = match &result {
            Ok(value) => value.clone(),
            Err(error) => json!({"state":"rejected","reason":error}),
        };
        *plan.phase.lock().expect("export phase") = Phase::Terminal(value);
        result
    }
    fn publish(
        &self,
        store: &ExecutionStore,
        plan: &Plan,
        prepared: Arc<Prepared>,
    ) -> Result<Value, String> {
        let _writer = store.acquire(&plan.project).map_err(|e| e.to_string())?;
        store
            .check_project_ready(&plan.project)
            .map_err(|e| e.to_string())?;
        pending_absent(&plan.project)?;
        prepared.verify()?;
        let id = self.id(ATTEMPT_PREFIX);
        let root = store.artifact_export_root();
        directory(root.parent().ok_or("missing export state parent")?)?;
        directory(&root)?;
        let attempt = root.join(hash(id.as_bytes()));
        directory(&attempt)?;
        let stage = prepared
            .parent
            .join(format!(".auto-kicad-export-{}", hash(id.as_bytes())));
        let candidate = attempt.join("candidate");
        let mut record = json!({"schema_version":1,"domain":"artifact_export","attempt_id":id,"project_root":prepared.project,"output_dir":prepared.target,
            "staging_dir":stage,"candidate_dir":candidate,"parent_identity":prepared.parent_identity,"state":"preparing","files":prepared.files,
            "source_dependencies":prepared.native.source_dependencies()?,"board_dependency":prepared.board,"plan_hash":plan.hash,
            "candidate_sha256":prepared.native.digest(),"candidate_retained":true});
        let record_path = attempt.join("record.json");
        write_json(&record_path, &record)?;
        let outcome = (|| -> Result<(), String> {
            let marker = json!({"schema_version":1,"domain":"artifact_export","attempt_id":id,"record_path":record_path});
            write_new(
                &plan.project.join(PENDING),
                &serde_json::to_vec(&marker).map_err(|e| e.to_string())?,
            )?;
            sync_directory(&plan.project)?;
            directory(&candidate)?;
            for file in prepared.native.files() {
                let path = candidate.join(&file.path);
                create_parents(&candidate, path.parent().ok_or("missing candidate parent")?)?;
                write_new(&path, &prepared.native.read_artifact(file)?)?;
            }
            write_new(
                &candidate.join("auto-kicad-manifest.json"),
                &prepared.manifest,
            )?;
            write_json(
                &attempt.join("native-evidence.json"),
                prepared.native.evidence(),
            )?;
            let proof_path = attempt.join("native-evidence.json");
            let proof_bytes = read_bound(&proof_path, 64 * 1024 * 1024)?;
            record["native_evidence"] =
                json!({"path":proof_path,"sha256":hash(&proof_bytes),"bytes":proof_bytes.len()});
            verify_inventory(&candidate, &prepared.files, false)?;
            sync_tree(&candidate)?;
            record["state"] = json!("staged");
            write_json(&record_path, &record)?;
            prepared.verify()?;
            copy_inventory(&candidate, &stage, &prepared.files)?;
            prepared.verify()?;
            exclusive_rename(&stage, &prepared.target)?;
            sync_directory(&prepared.parent)?;
            record["state"] = json!("published");
            write_json(&record_path, &record)?;
            prepared.native.verify()?;
            prepared.board.verify()?;
            verify_inventory(&prepared.target, &prepared.files, false)?;
            record["state"] = json!("committed");
            record["actual_published_inventory_verified"] = json!(true);
            write_json(&record_path, &record)?;
            clear_pending(&plan.project, &id)?;
            Ok(())
        })();
        if let Err(error) = outcome {
            record["state"] = json!("unknown");
            record["reason"] = json!(error);
            let _ = write_json(&record_path, &record);
            return Ok(
                json!({"state":"unknown","attempt_id":id,"record_path":record_path,"reason":error,"requires_fresh_recovery":true}),
            );
        }
        if fs::remove_dir_all(&candidate).is_ok() {
            record["candidate_retained"] = json!(false);
            let _ = write_json(&record_path, &record);
        }
        Ok(
            json!({"state":"committed","attempt_id":id,"record_path":record_path,"output_dir":prepared.target,"files":prepared.files,"source_files_unchanged":true}),
        )
    }
    pub(crate) fn plan_recovery(
        &self,
        store: &ExecutionStore,
        project: &str,
        attempt_id: &str,
        action: &str,
    ) -> Result<Value, String> {
        if !matches!(action, "finalize_export" | "retain_artifacts") {
            return Err("export recovery supports finalize_export or retain_artifacts".into());
        }
        let project = canonical_directory(Path::new(project))?;
        let path = record_path(store, &project, attempt_id)?;
        let bytes = read_bound(&path, 8 * 1024 * 1024)?;
        let record: Value = serde_json::from_slice(&bytes).map_err(|e| e.to_string())?;
        validate_record(&record, &project, attempt_id)?;
        if matches!(record["state"].as_str(), Some("committed" | "retained"))
            && !project.join(PENDING).exists()
        {
            return Err("export attempt is already terminal".into());
        }
        validate_recovery(&record, action)?;
        let id = self.id(RECOVERY_PREFIX);
        let sha = hash(&bytes);
        let hash = digest_text(&[
            "artifact-export-recovery-v1",
            &self.nonce,
            &id,
            &sha,
            action,
        ]);
        let mut state = self.state.lock().expect("export registry");
        Self::prune(&mut state);
        if state.recoveries.len() >= 4 {
            return Err("export recovery plan limit reached".into());
        }
        state.recoveries.insert(
            id.clone(),
            Recovery {
                project: project.clone(),
                record_path: path,
                record_sha256: sha,
                hash: hash.clone(),
                action: action.into(),
                created: Instant::now(),
                approval: None,
            },
        );
        Ok(
            json!({"recovery_id":id,"recovery_plan_hash":hash,"project_dir":project,"action":action,"output_dir":record["output_dir"],"files":record["files"],"source_files_are_never_restored":true}),
        )
    }
    pub(crate) fn preview_recovery(
        &self,
        project: &str,
        id: &str,
        hash: &str,
    ) -> Result<Value, String> {
        let project = canonical_directory(Path::new(project))?;
        let mut state = self.state.lock().expect("export registry");
        Self::prune(&mut state);
        let recovery = state
            .recoveries
            .get_mut(id)
            .ok_or("unknown or expired export recovery")?;
        if recovery.project != project || recovery.hash != hash {
            return Err("export recovery identity differs".into());
        }
        let bytes = read_bound(&recovery.record_path, 8 * 1024 * 1024)?;
        if hash_bytes(&bytes) != recovery.record_sha256 {
            return Err("export recovery journal changed".into());
        }
        let record: Value = serde_json::from_slice(&bytes).map_err(|e| e.to_string())?;
        validate_recovery(&record, &recovery.action)?;
        let approval = digest_text(&[
            "artifact-export-recovery-approval-v1",
            &self.nonce,
            id,
            hash,
            &recovery.record_sha256,
        ]);
        recovery.approval = Some(approval.clone());
        Ok(
            json!({"state":"awaiting_approval","recovery_id":id,"recovery_plan_hash":hash,"approval_hash":approval}),
        )
    }
    pub(crate) fn commit_recovery(
        &self,
        store: &ExecutionStore,
        project: &str,
        id: &str,
        hash: &str,
        approval: &str,
    ) -> Result<Value, String> {
        let project = canonical_directory(Path::new(project))?;
        let recovery = {
            let mut state = self.state.lock().expect("export registry");
            Self::prune(&mut state);
            let candidate = state
                .recoveries
                .get(id)
                .ok_or("unknown or expired export recovery")?;
            if candidate.project != project
                || candidate.hash != hash
                || candidate.approval.as_deref() != Some(approval)
            {
                return Err("fresh exact export recovery approval is required".into());
            }
            state.recoveries.remove(id).expect("recovery exists")
        };
        let _writer = store.acquire(&project).map_err(|e| e.to_string())?;
        let bytes = read_bound(&recovery.record_path, 8 * 1024 * 1024)?;
        if hash_bytes(&bytes) != recovery.record_sha256 {
            return Err("export journal changed after approval".into());
        }
        let mut record: Value = serde_json::from_slice(&bytes).map_err(|e| e.to_string())?;
        validate_recovery(&record, &recovery.action)?;
        let outcome = (|| -> Result<(), String> {
            if recovery.action == "finalize_export" {
                let target = value_path(&record, "output_dir")?;
                let stage = value_path(&record, "staging_dir")?;
                let candidate = value_path(&record, "candidate_dir")?;
                let files: Vec<ExportFile> =
                    serde_json::from_value(record["files"].clone()).map_err(|e| e.to_string())?;
                if !target.exists() {
                    if !stage.exists() {
                        copy_inventory(&candidate, &stage, &files)?;
                    } else {
                        verify_inventory(&stage, &files, false)?;
                    }
                    exclusive_rename(&stage, &target)?;
                    sync_directory(target.parent().ok_or("missing export parent")?)?;
                }
                verify_inventory(&target, &files, false)?;
                record["state"] = json!("committed");
                record["actual_published_inventory_verified"] = json!(true);
            } else {
                record["state"] = json!("retained");
                record["user_files_and_partial_artifacts_preserved"] = json!(true);
            }
            write_json(&recovery.record_path, &record)?;
            clear_pending(
                &project,
                record["attempt_id"].as_str().ok_or("missing attempt")?,
            )?;
            Ok(())
        })();
        if let Err(error) = outcome {
            record["state"] = json!("unknown");
            record["reason"] = json!(error);
            let _ = write_json(&recovery.record_path, &record);
            return Ok(
                json!({"state":"unknown","attempt_id":record["attempt_id"],"record_path":recovery.record_path,
                "reason":error,"requires_fresh_recovery":true,"source_files_unchanged":true}),
            );
        }
        Ok(
            json!({"state":record["state"],"attempt_id":record["attempt_id"],"output_dir":record["output_dir"],"source_files_unchanged":true,"later_user_bytes_preserved":true}),
        )
    }
}

fn hash_bytes(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}
fn create_parents(root: &Path, parent: &Path) -> Result<(), String> {
    let relative = parent.strip_prefix(root).map_err(|e| e.to_string())?;
    let mut current = root.to_path_buf();
    for part in relative.components() {
        if !matches!(part, Component::Normal(_)) {
            return Err("artifact path escapes staging".into());
        }
        current.push(part);
        directory(&current)?;
    }
    Ok(())
}
fn copy_inventory(source: &Path, target: &Path, files: &[ExportFile]) -> Result<(), String> {
    if fs::symlink_metadata(target).is_ok() {
        return Err("export staging directory is occupied".into());
    }
    directory(target)?;
    for file in files {
        let input = source.join(&file.path);
        let bytes = read_bound(&input, 64 * 1024 * 1024)?;
        if bytes.len() as u64 != file.bytes || hash(&bytes) != file.sha256 {
            return Err("durable artifact candidate changed".into());
        }
        let destination = target.join(&file.path);
        create_parents(target, destination.parent().ok_or("missing stage parent")?)?;
        write_new(&destination, &bytes)?;
    }
    verify_inventory(target, files, false)?;
    sync_tree(target)
}
fn list_files(root: &Path) -> Result<Vec<PathBuf>, String> {
    let mut pending = vec![(root.to_path_buf(), 0)];
    let mut files = Vec::new();
    while let Some((path, depth)) = pending.pop() {
        if depth > 4 {
            return Err("artifact inventory depth exceeds bound".into());
        }
        for entry in fs::read_dir(path).map_err(|e| e.to_string())? {
            let entry = entry.map_err(|e| e.to_string())?;
            let kind = entry.file_type().map_err(|e| e.to_string())?;
            if kind.is_dir() {
                pending.push((entry.path(), depth + 1));
            } else if kind.is_file() {
                files.push(entry.path());
            } else {
                return Err("artifact inventory contains a symlink or special file".into());
            }
            if files.len() + pending.len() > 513 {
                return Err("artifact inventory exceeds file bound".into());
            }
        }
    }
    files.sort();
    Ok(files)
}
fn verify_inventory(root: &Path, files: &[ExportFile], allow_partial: bool) -> Result<(), String> {
    canonical_directory(root)?;
    if files.is_empty()
        || files
            .iter()
            .any(|file| file.bytes == 0 || file.bytes > 64 * 1024 * 1024)
        || files.len() > 513
        || files.iter().map(|file| file.bytes).sum::<u64>() > MAX_BYTES
    {
        return Err("invalid recorded artifact inventory".into());
    }
    let expected = files
        .iter()
        .map(|file| (PathBuf::from(&file.path), file))
        .collect::<BTreeMap<_, _>>();
    if expected.len() != files.len() {
        return Err("duplicate artifact filename".into());
    }
    let actual = list_files(root)?;
    if !allow_partial && actual.len() != files.len() {
        return Err("artifact inventory is incomplete or has later user files".into());
    }
    for path in actual {
        let relative = path.strip_prefix(root).map_err(|e| e.to_string())?;
        let file = expected
            .get(relative)
            .ok_or("later user artifact was not approved")?;
        let bytes = read_bound(&path, 64 * 1024 * 1024)?;
        if bytes.len() as u64 != file.bytes || hash(&bytes) != file.sha256 {
            return Err("later user artifact bytes differ from the approved export".into());
        }
    }
    Ok(())
}
fn sync_tree(root: &Path) -> Result<(), String> {
    let mut dirs = vec![root.to_path_buf()];
    for file in list_files(root)? {
        File::open(&file)
            .and_then(|file| file.sync_all())
            .map_err(|e| e.to_string())?;
        if let Some(parent) = file.parent() {
            dirs.push(parent.into());
        }
    }
    dirs.sort();
    dirs.dedup();
    for directory in dirs.into_iter().rev() {
        sync_directory(&directory)?;
    }
    Ok(())
}
fn pending_absent(project: &Path) -> Result<(), String> {
    if fs::symlink_metadata(project.join(PENDING)).is_ok() {
        return Err("an export publication requires fresh recovery before another export".into());
    }
    Ok(())
}
fn clear_pending(project: &Path, id: &str) -> Result<(), String> {
    let path = project.join(PENDING);
    if let Err(error) = fs::symlink_metadata(&path) {
        if error.kind() == std::io::ErrorKind::NotFound {
            return Ok(());
        }
        return Err(error.to_string());
    }
    let value: Value =
        serde_json::from_slice(&read_bound(&path, 65536)?).map_err(|e| e.to_string())?;
    if value["attempt_id"] != id {
        return Err("another export owns the pending marker".into());
    }
    fs::remove_file(path).map_err(|e| e.to_string())?;
    sync_directory(project)
}
fn record_path(store: &ExecutionStore, project: &Path, id: &str) -> Result<PathBuf, String> {
    if !is_attempt(id) || id.len() > 256 {
        return Err("invalid export attempt identity".into());
    }
    let own = store
        .artifact_export_root()
        .join(hash(id.as_bytes()))
        .join("record.json");
    if own.is_file() {
        return Ok(own);
    }
    let marker: Value = serde_json::from_slice(&read_bound(&project.join(PENDING), 65536)?)
        .map_err(|e| e.to_string())?;
    if marker["attempt_id"] != id || marker["domain"] != "artifact_export" {
        return Err("pending export identity differs".into());
    }
    let path = value_path(&marker, "record_path")?;
    let parent = path.parent().ok_or("missing export journal parent")?;
    if parent.file_name().and_then(|s| s.to_str()) != Some(hash(id.as_bytes()).as_str())
        || parent
            .parent()
            .and_then(Path::file_name)
            .and_then(|s| s.to_str())
            != Some("artifact-exports")
        || fs::canonicalize(&path).map_err(|e| e.to_string())? != path
    {
        return Err("foreign pending export journal has no canonical ownership path".into());
    }
    Ok(path)
}
fn value_path(value: &Value, name: &str) -> Result<PathBuf, String> {
    value[name]
        .as_str()
        .map(PathBuf::from)
        .ok_or_else(|| format!("missing export {name}"))
}
fn validate_record(record: &Value, project: &Path, id: &str) -> Result<(), String> {
    if record["schema_version"] != 1
        || record["domain"] != "artifact_export"
        || record["attempt_id"] != id
        || record["project_root"].as_str() != project.to_str()
    {
        return Err("export journal identity differs".into());
    }
    let target = value_path(record, "output_dir")?;
    let parent = canonical_directory(target.parent().ok_or("missing export parent")?)?;
    if !parent.starts_with(project)
        || target == project
        || record["parent_identity"] != json!(directory_identity(&parent)?)
    {
        return Err("export destination authority changed".into());
    }
    let stage = value_path(record, "staging_dir")?;
    if stage.parent() != Some(parent.as_path())
        || stage.file_name().and_then(|s| s.to_str())
            != Some(format!(".auto-kicad-export-{}", hash(id.as_bytes())).as_str())
    {
        return Err("export stage identity differs".into());
    }
    let candidate = value_path(record, "candidate_dir")?;
    let attempt = candidate
        .parent()
        .ok_or("missing durable candidate parent")?;
    if !candidate.is_absolute()
        || candidate.file_name() != Some(std::ffi::OsStr::new("candidate"))
        || attempt.file_name().and_then(|s| s.to_str()) != Some(hash(id.as_bytes()).as_str())
        || attempt.parent().and_then(Path::file_name)
            != Some(std::ffi::OsStr::new("artifact-exports"))
        || canonical_directory(attempt)? != attempt
    {
        return Err("durable export candidate authority differs".into());
    }
    if !record["native_evidence"].is_null()
        && value_path(&record["native_evidence"], "path")? != attempt.join("native-evidence.json")
    {
        return Err("durable export proof authority differs".into());
    }
    let files: Vec<ExportFile> =
        serde_json::from_value(record["files"].clone()).map_err(|e| e.to_string())?;
    for file in files {
        let path = Path::new(&file.path);
        if path.is_absolute()
            || path
                .components()
                .any(|part| !matches!(part, Component::Normal(_)))
            || file.sha256.len() != 64
        {
            return Err("invalid recorded artifact path/digest".into());
        }
    }
    Ok(())
}
fn validate_recovery(record: &Value, action: &str) -> Result<(), String> {
    let project = value_path(record, "project_root")?;
    let id = record["attempt_id"]
        .as_str()
        .ok_or("missing export attempt")?;
    validate_record(record, &project, id)?;
    let pending = project.join(PENDING);
    if fs::symlink_metadata(&pending).is_ok() {
        let marker: Value =
            serde_json::from_slice(&read_bound(&pending, 65536)?).map_err(|e| e.to_string())?;
        let candidate = value_path(record, "candidate_dir")?;
        if marker["schema_version"] != 1
            || marker["domain"] != "artifact_export"
            || marker["attempt_id"] != id
            || value_path(&marker, "record_path")?
                != candidate
                    .parent()
                    .ok_or("missing attempt directory")?
                    .join("record.json")
        {
            return Err("another publication owns the pending export marker".into());
        }
    }
    if action == "retain_artifacts" {
        return Ok(());
    }
    let proof = &record["native_evidence"];
    let proof_path = value_path(proof, "path")?;
    let proof_bytes = read_bound(&proof_path, 64 * 1024 * 1024)?;
    if proof["sha256"].as_str() != Some(hash(&proof_bytes).as_str())
        || proof["bytes"].as_u64() != Some(proof_bytes.len() as u64)
    {
        return Err("native artifact evidence changed or is incomplete".into());
    }
    validate_dependencies(&record["source_dependencies"])?;
    let board: Fingerprint =
        serde_json::from_value(record["board_dependency"].clone()).map_err(|e| e.to_string())?;
    board.verify()?;
    let files: Vec<ExportFile> =
        serde_json::from_value(record["files"].clone()).map_err(|e| e.to_string())?;
    let target = value_path(record, "output_dir")?;
    if fs::symlink_metadata(&target).is_ok() {
        verify_inventory(&target, &files, false)?;
    } else {
        verify_inventory(&value_path(record, "candidate_dir")?, &files, false)?;
        let stage = value_path(record, "staging_dir")?;
        if stage.exists() {
            verify_inventory(&stage, &files, false)?;
        }
    }
    Ok(())
}
fn validate_dependencies(value: &Value) -> Result<(), String> {
    if let Some(expected) = value.get("inherited_environment_sha256") {
        let actual =
            kicad_schematic::NativeExecutionContext::current_inherited_environment_digest()
                .map_err(|e| e.to_string())?;
        if expected.as_str() != Some(actual.as_str()) {
            return Err("export inherited environment changed".into());
        }
    }
    if let Some(object) = value.as_object() {
        if object.contains_key("requested")
            && object.contains_key("canonical")
            && object.contains_key("digest")
            && object.contains_key("size")
        {
            let path = value_path(value, "requested")?;
            let actual = Fingerprint::capture(&path)?;
            if serde_json::to_value(&actual.canonical).map_err(|e| e.to_string())?
                != value["canonical"]
                || serde_json::to_value(&actual.sha256).map_err(|e| e.to_string())?
                    != value["digest"]
                || actual.bytes != value["size"].as_u64().ok_or("invalid dependency size")?
            {
                return Err("export source/resource dependency changed".into());
            }
        }
        if let (Some(root), Some(expected)) = (
            object.get("configuration_root"),
            object.get("configuration_files"),
        ) {
            let root = PathBuf::from(root.as_str().ok_or("invalid configuration root")?);
            let mut actual = list_files(&root)?
                .into_iter()
                .map(|path| path.to_string_lossy().into_owned())
                .collect::<Vec<_>>();
            actual.sort();
            let mut wanted = expected
                .as_array()
                .ok_or("invalid configuration inventory")?
                .iter()
                .map(|path| {
                    path.as_str()
                        .map(str::to_owned)
                        .ok_or("invalid configuration filename")
                })
                .collect::<Result<Vec<_>, _>>()?;
            wanted.sort();
            if actual != wanted {
                return Err("export configuration namespace changed".into());
            }
        }
        for child in object.values() {
            validate_dependencies(child)?;
        }
    } else if let Some(items) = value.as_array() {
        for child in items {
            validate_dependencies(child)?;
        }
    }
    Ok(())
}
