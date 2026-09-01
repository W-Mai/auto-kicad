use std::fs::{self, File, OpenOptions, TryLockError};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use kicad_ipc::{ClientConfig, DocumentQuery, KiCadClient, NngTransport, OpenDocumentKind};

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ShadowProject {
    root: PathBuf,
    board_path: PathBuf,
    project_path: PathBuf,
}

impl ShadowProject {
    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn board_path(&self) -> &Path {
        &self.board_path
    }

    pub fn project_path(&self) -> &Path {
        &self.project_path
    }

    pub fn is_within(&self, root: &Path) -> bool {
        self.root.starts_with(root)
            && self.board_path.starts_with(&self.root)
            && self.project_path.starts_with(&self.root)
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct OwnedEditorIdentity {
    pub pid: u32,
    pub endpoint: String,
    pub board_filename: String,
    pub document_identity: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum LifecycleError {
    InvalidSource,
    SourceOutsideServerRoot,
    SourceLockPresent,
    ProjectMissing,
    ShadowCreation(String),
    InvalidIdentity(&'static str),
    NoShadow,
    NoOwnedEditor,
    ForeignEditor,
    StaleEndpoint,
    DocumentMismatch,
    InvalidTransition(&'static str),
    Cleanup(String),
    Launch(String),
    Handshake(String),
    StartupTimeout,
}

impl std::fmt::Display for LifecycleError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidSource => formatter.write_str("invalid disposable PCB source"),
            Self::SourceOutsideServerRoot => formatter.write_str("source is outside server root"),
            Self::SourceLockPresent => formatter.write_str("source editor lock is present"),
            Self::ProjectMissing => formatter.write_str("matching KiCad project is missing"),
            Self::ShadowCreation(message) => write!(formatter, "shadow creation failed: {message}"),
            Self::InvalidIdentity(message) | Self::InvalidTransition(message) => {
                formatter.write_str(message)
            }
            Self::NoShadow => formatter.write_str("no shadow project exists"),
            Self::NoOwnedEditor => formatter.write_str("no owned editor exists"),
            Self::ForeignEditor => formatter.write_str("editor is not owned by this session"),
            Self::StaleEndpoint => formatter.write_str("editor endpoint is stale"),
            Self::DocumentMismatch => formatter.write_str("editor document identity mismatched"),
            Self::Cleanup(message) => write!(formatter, "cleanup failed: {message}"),
            Self::Launch(message) => write!(formatter, "pcbnew launch failed: {message}"),
            Self::Handshake(message) => write!(formatter, "PCB editor handshake failed: {message}"),
            Self::StartupTimeout => formatter.write_str("PCB editor startup timed out"),
        }
    }
}

impl std::error::Error for LifecycleError {}

pub const KICAD_PCBNEW_CANDIDATES: &[&str] = &[
    "/Applications/KiCad/KiCad.app/Contents/Applications/pcbnew.app/Contents/MacOS/pcbnew",
    "/usr/bin/pcbnew",
];

pub struct OwnedPcbnewLauncher {
    lifecycle: FilesystemDisposablePcbLifecycle,
    child: Option<Child>,
    startup_timeout: Duration,
}

impl OwnedPcbnewLauncher {
    pub fn new(server_root: &Path) -> Result<Self, LifecycleError> {
        Ok(Self {
            lifecycle: FilesystemDisposablePcbLifecycle::new(server_root)?,
            child: None,
            startup_timeout: Duration::from_secs(15),
        })
    }

    pub fn with_startup_timeout(
        server_root: &Path,
        startup_timeout: Duration,
    ) -> Result<Self, LifecycleError> {
        let mut launcher = Self::new(server_root)?;
        launcher.startup_timeout = startup_timeout;
        Ok(launcher)
    }

    fn executable() -> Result<&'static str, LifecycleError> {
        KICAD_PCBNEW_CANDIDATES
            .iter()
            .copied()
            .find(|path| Path::new(path).is_file())
            .ok_or_else(|| LifecycleError::Launch("no fixed pcbnew executable found".into()))
    }

    fn api_socket_root() -> PathBuf {
        if cfg!(target_os = "macos") {
            PathBuf::from("/tmp/kicad")
        } else {
            std::env::temp_dir().join("kicad")
        }
    }

    fn guard_api_socket_namespace() -> Result<(Option<File>, bool), LifecycleError> {
        let root = Self::api_socket_root();
        fs::create_dir_all(&root).map_err(|error| LifecycleError::Handshake(error.to_string()))?;
        let default_socket_existed = root.join("api.sock").exists();
        let lock = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(root.join("api.lock"))
            .map_err(|error| LifecycleError::Handshake(error.to_string()))?;
        match lock.try_lock() {
            Ok(()) => Ok((Some(lock), default_socket_existed)),
            Err(TryLockError::WouldBlock) if default_socket_existed => Ok((None, true)),
            Err(TryLockError::WouldBlock) => Err(LifecycleError::Handshake(
                "KiCad API lock is held but the default socket is absent".into(),
            )),
            Err(TryLockError::Error(error)) => Err(LifecycleError::Handshake(error.to_string())),
        }
    }

    fn owned_endpoints(pid: u32, allow_default: bool) -> Vec<String> {
        let root = Self::api_socket_root();
        let mut endpoints = Vec::with_capacity(2);
        if allow_default {
            endpoints.push(format!("ipc://{}/api.sock", root.display()));
        }
        endpoints.push(format!("ipc://{}/api-{pid}.sock", root.display()));
        endpoints
    }

    fn launch_and_handshake(
        &mut self,
        shadow: &ShadowProject,
        previous: Option<&OwnedEditorIdentity>,
    ) -> Result<OwnedEditorIdentity, LifecycleError> {
        let executable = Self::executable()?;
        let board_filename = shadow
            .board_path()
            .file_name()
            .and_then(|name| name.to_str())
            .ok_or(LifecycleError::InvalidSource)?
            .to_owned();
        let (_socket_namespace_guard, default_socket_existed) = Self::guard_api_socket_namespace()?;
        let child = Command::new(executable)
            .arg(shadow.board_path())
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|error| LifecycleError::Launch(error.to_string()))?;
        let pid = child.id();
        let endpoint_candidates = Self::owned_endpoints(pid, !default_socket_existed);
        let deadline = Instant::now() + self.startup_timeout;
        let mut child = Some(child);
        let identity = loop {
            let early_status = match child
                .as_mut()
                .expect("child retained until handshake")
                .try_wait()
            {
                Ok(status) => status,
                Err(error) => {
                    let mut owned = child.take().expect("child retained until handshake");
                    let _ = owned.kill();
                    let _ = owned.wait();
                    return Err(LifecycleError::Launch(error.to_string()));
                }
            };
            if let Some(status) = early_status {
                child.take();
                return Err(LifecycleError::Launch(format!(
                    "owned pcbnew exited before IPC handshake: {status}"
                )));
            }
            if Instant::now() >= deadline {
                let mut owned = child.take().expect("child retained until handshake");
                let _ = owned.kill();
                let _ = owned.wait();
                return Err(LifecycleError::StartupTimeout);
            }
            let mut found_identity = None;
            for endpoint in &endpoint_candidates {
                let config = match ClientConfig::new(endpoint, "auto-kicad-adapter") {
                    Ok(config) => config,
                    Err(error) => {
                        let mut owned = child.take().expect("child retained until handshake");
                        let _ = owned.kill();
                        let _ = owned.wait();
                        return Err(LifecycleError::Handshake(error.to_string()));
                    }
                };
                if let Ok(transport) = NngTransport::connect(config.clone()) {
                    let mut client = KiCadClient::new(transport, config);
                    if let Ok(documents) = client.get_open_documents(DocumentQuery::Pcb)
                        && documents.len() == 1
                        && let OpenDocumentKind::Pcb {
                            board_filename: found,
                        } = &documents[0].kind
                    {
                        let project_matches = documents[0]
                            .project_path
                            .as_deref()
                            .and_then(|path| fs::canonicalize(path).ok())
                            .is_some_and(|path| {
                                path == shadow.root() || path == shadow.project_path()
                            });
                        if found == &board_filename && project_matches {
                            found_identity = Some(OwnedEditorIdentity {
                                pid,
                                endpoint: endpoint.clone(),
                                board_filename: board_filename.clone(),
                                document_identity: format!(
                                    "{}:{}:{}:{}",
                                    pid,
                                    endpoint,
                                    documents[0].project_path.as_deref().unwrap_or_default(),
                                    found
                                ),
                            });
                            break;
                        }
                    }
                }
            }
            if let Some(identity) = found_identity {
                break identity;
            }
            thread::sleep(Duration::from_millis(100));
        };
        if let Some(previous) = previous
            && (identity.pid == previous.pid
                || identity.document_identity == previous.document_identity)
        {
            let mut owned = child.take().expect("child retained after handshake");
            let _ = owned.kill();
            let _ = owned.wait();
            return Err(LifecycleError::StaleEndpoint);
        }
        self.child = child;
        Ok(identity)
    }

    pub fn launch(
        &mut self,
        shadow: &ShadowProject,
    ) -> Result<OwnedEditorIdentity, LifecycleError> {
        let identity = self.launch_and_handshake(shadow, None)?;
        if let Err(error) = self.lifecycle.register_launch(shadow, identity.clone()) {
            if let Some(mut child) = self.child.take() {
                let _ = child.kill();
                let _ = child.wait();
            }
            return Err(error);
        }
        Ok(identity)
    }

    pub fn current_shadow(&self) -> Result<&ShadowProject, LifecycleError> {
        self.lifecycle
            .shadow
            .as_ref()
            .ok_or(LifecycleError::NoShadow)
    }

    pub fn current_identity(&self) -> Result<&OwnedEditorIdentity, LifecycleError> {
        self.lifecycle
            .editor
            .as_ref()
            .ok_or(LifecycleError::NoOwnedEditor)
    }

    pub fn connect_owned(&mut self) -> Result<KiCadClient<NngTransport>, LifecycleError> {
        let editor = self
            .lifecycle
            .editor
            .as_ref()
            .ok_or(LifecycleError::NoOwnedEditor)?;
        let child = self.child.as_ref().ok_or(LifecycleError::NoOwnedEditor)?;
        if child.id() != editor.pid {
            return Err(LifecycleError::ForeignEditor);
        }
        let config = ClientConfig::new(&editor.endpoint, "auto-kicad-adapter")
            .map_err(|error| LifecycleError::Handshake(error.to_string()))?;
        let transport = NngTransport::connect(config.clone())
            .map_err(|error| LifecycleError::Handshake(error.to_string()))?;
        Ok(KiCadClient::new(transport, config))
    }

    pub fn reopen(&mut self) -> Result<OwnedEditorIdentity, LifecycleError> {
        let shadow = self
            .lifecycle
            .shadow
            .clone()
            .ok_or(LifecycleError::NoShadow)?;
        let previous = self
            .lifecycle
            .editor
            .clone()
            .ok_or(LifecycleError::NoOwnedEditor)?;
        if self.child.is_some() {
            self.close_owned(previous.pid)?;
        }
        let identity = self.launch_and_handshake(&shadow, Some(&previous))?;
        if let Err(error) = self.lifecycle.register_reopen(identity.clone()) {
            if let Some(mut child) = self.child.take() {
                let _ = child.kill();
                let _ = child.wait();
            }
            return Err(error);
        }
        Ok(identity)
    }
}

impl Drop for OwnedPcbnewLauncher {
    fn drop(&mut self) {
        if let Some(mut child) = self.child.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

impl DisposablePcbLifecycle for OwnedPcbnewLauncher {
    fn create_shadow(&mut self, source_board: &Path) -> Result<ShadowProject, LifecycleError> {
        self.lifecycle.create_shadow(source_board)
    }

    fn register_launch(
        &mut self,
        shadow: &ShadowProject,
        identity: OwnedEditorIdentity,
    ) -> Result<(), LifecycleError> {
        self.lifecycle.register_launch(shadow, identity)
    }

    fn close_owned(&mut self, pid: u32) -> Result<(), LifecycleError> {
        let child = self.child.as_mut().ok_or(LifecycleError::NoOwnedEditor)?;
        if child.id() != pid {
            return Err(LifecycleError::ForeignEditor);
        }
        child
            .kill()
            .map_err(|error| LifecycleError::Cleanup(error.to_string()))?;
        child
            .wait()
            .map_err(|error| LifecycleError::Cleanup(error.to_string()))?;
        self.child = None;
        self.lifecycle.close_owned(pid)
    }

    fn register_reopen(&mut self, identity: OwnedEditorIdentity) -> Result<(), LifecycleError> {
        self.lifecycle.register_reopen(identity)
    }

    fn is_reopened(&self) -> bool {
        self.lifecycle.is_reopened()
    }

    fn cleanup(&mut self) -> Result<(), LifecycleError> {
        if self.child.is_some() {
            return Err(LifecycleError::InvalidTransition(
                "owned editor must close before cleanup",
            ));
        }
        self.lifecycle.cleanup()
    }
}

pub trait DisposablePcbLifecycle {
    fn create_shadow(&mut self, source_board: &Path) -> Result<ShadowProject, LifecycleError>;
    fn register_launch(
        &mut self,
        shadow: &ShadowProject,
        identity: OwnedEditorIdentity,
    ) -> Result<(), LifecycleError>;
    fn close_owned(&mut self, pid: u32) -> Result<(), LifecycleError>;
    fn register_reopen(&mut self, identity: OwnedEditorIdentity) -> Result<(), LifecycleError>;
    fn is_reopened(&self) -> bool;
    fn cleanup(&mut self) -> Result<(), LifecycleError>;
}

#[derive(Clone, Debug)]
pub struct FilesystemDisposablePcbLifecycle {
    server_root: PathBuf,
    shadow: Option<ShadowProject>,
    editor: Option<OwnedEditorIdentity>,
    closed: bool,
    reopened: bool,
}

impl FilesystemDisposablePcbLifecycle {
    pub fn new(server_root: &Path) -> Result<Self, LifecycleError> {
        fs::create_dir_all(server_root)
            .map_err(|error| LifecycleError::ShadowCreation(error.to_string()))?;
        let server_root = fs::canonicalize(server_root)
            .map_err(|error| LifecycleError::ShadowCreation(error.to_string()))?;
        Ok(Self {
            server_root,
            shadow: None,
            editor: None,
            closed: false,
            reopened: false,
        })
    }

    fn validate_identity(identity: &OwnedEditorIdentity) -> Result<(), LifecycleError> {
        if identity.pid == 0
            || identity.endpoint.is_empty()
            || identity.board_filename.is_empty()
            || identity.document_identity.is_empty()
        {
            return Err(LifecycleError::InvalidIdentity(
                "editor identity fields must be non-empty",
            ));
        }
        Ok(())
    }
}

impl DisposablePcbLifecycle for FilesystemDisposablePcbLifecycle {
    fn create_shadow(&mut self, source_board: &Path) -> Result<ShadowProject, LifecycleError> {
        if self.shadow.is_some() {
            return Err(LifecycleError::InvalidTransition("shadow already exists"));
        }
        let source_board =
            fs::canonicalize(source_board).map_err(|_| LifecycleError::InvalidSource)?;
        if !source_board.is_file()
            || source_board.extension().and_then(|x| x.to_str()) != Some("kicad_pcb")
        {
            return Err(LifecycleError::InvalidSource);
        }
        let source_project = source_board.with_extension("kicad_pro");
        if !source_project.is_file() {
            return Err(LifecycleError::ProjectMissing);
        }
        if lock_path(&source_board).is_file() || lock_path(&source_project).is_file() {
            return Err(LifecycleError::SourceLockPresent);
        }
        let stem = source_board
            .file_stem()
            .and_then(|x| x.to_str())
            .ok_or(LifecycleError::InvalidSource)?;
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|_| LifecycleError::InvalidSource)?
            .as_nanos();
        let root = self.server_root.join(format!(
            "auto-kicad-shadow-{}-{}",
            std::process::id(),
            stamp
        ));
        if !root.starts_with(&self.server_root) {
            return Err(LifecycleError::SourceOutsideServerRoot);
        }
        fs::create_dir(&root).map_err(|error| LifecycleError::ShadowCreation(error.to_string()))?;
        let board_path = root.join(format!("{stem}.kicad_pcb"));
        let project_path = root.join(format!("{stem}.kicad_pro"));
        if let Err(error) = fs::copy(&source_board, &board_path)
            .and_then(|_| fs::copy(&source_project, &project_path).map(|_| ()))
        {
            let _ = fs::remove_dir_all(&root);
            return Err(LifecycleError::ShadowCreation(error.to_string()));
        }
        let shadow = ShadowProject {
            root,
            board_path,
            project_path,
        };
        self.shadow = Some(shadow.clone());
        self.closed = false;
        self.reopened = false;
        Ok(shadow)
    }

    fn register_launch(
        &mut self,
        shadow: &ShadowProject,
        identity: OwnedEditorIdentity,
    ) -> Result<(), LifecycleError> {
        Self::validate_identity(&identity)?;
        if self.shadow.as_ref() != Some(shadow) {
            return Err(LifecycleError::NoShadow);
        }
        if self.editor.is_some() {
            return Err(LifecycleError::InvalidTransition(
                "editor already registered",
            ));
        }
        if identity.board_filename
            != shadow
                .board_path
                .file_name()
                .and_then(|x| x.to_str())
                .unwrap_or_default()
        {
            return Err(LifecycleError::DocumentMismatch);
        }
        self.editor = Some(identity);
        Ok(())
    }

    fn close_owned(&mut self, pid: u32) -> Result<(), LifecycleError> {
        let editor = self.editor.as_ref().ok_or(LifecycleError::NoOwnedEditor)?;
        if editor.pid != pid {
            return Err(LifecycleError::ForeignEditor);
        }
        if self.closed {
            return Err(LifecycleError::InvalidTransition("editor already closed"));
        }
        self.closed = true;
        Ok(())
    }

    fn register_reopen(&mut self, identity: OwnedEditorIdentity) -> Result<(), LifecycleError> {
        Self::validate_identity(&identity)?;
        let previous = self.editor.as_ref().ok_or(LifecycleError::NoOwnedEditor)?;
        if !self.closed {
            return Err(LifecycleError::InvalidTransition(
                "owned editor must close before reopen",
            ));
        }
        if identity.pid == previous.pid {
            return Err(LifecycleError::StaleEndpoint);
        }
        if identity.board_filename != previous.board_filename {
            return Err(LifecycleError::DocumentMismatch);
        }
        if identity.document_identity == previous.document_identity {
            return Err(LifecycleError::DocumentMismatch);
        }
        self.editor = Some(identity);
        self.closed = false;
        self.reopened = true;
        Ok(())
    }

    fn is_reopened(&self) -> bool {
        self.reopened
    }

    fn cleanup(&mut self) -> Result<(), LifecycleError> {
        if self.editor.is_some() && !self.closed {
            return Err(LifecycleError::InvalidTransition(
                "owned editor must close before cleanup",
            ));
        }
        let shadow = self.shadow.take().ok_or(LifecycleError::NoShadow)?;
        fs::remove_dir_all(shadow.root)
            .map_err(|error| LifecycleError::Cleanup(error.to_string()))?;
        self.editor = None;
        Ok(())
    }
}

fn lock_path(path: &Path) -> PathBuf {
    let name = path
        .file_name()
        .and_then(|x| x.to_str())
        .unwrap_or_default();
    path.with_file_name(format!("~{name}.lck"))
}

#[derive(Clone, Debug, Default)]
pub struct FakeDisposablePcbLifecycle {
    shadow: Option<ShadowProject>,
    editor: Option<OwnedEditorIdentity>,
    closed: bool,
    reopened: bool,
}

impl DisposablePcbLifecycle for FakeDisposablePcbLifecycle {
    fn create_shadow(&mut self, source_board: &Path) -> Result<ShadowProject, LifecycleError> {
        if self.shadow.is_some() {
            return Err(LifecycleError::InvalidTransition("shadow already exists"));
        }
        let filename = source_board
            .file_name()
            .and_then(|value| value.to_str())
            .filter(|value| value.ends_with(".kicad_pcb"))
            .ok_or(LifecycleError::InvalidSource)?;
        let stem = filename.trim_end_matches(".kicad_pcb");
        let root = PathBuf::from("/fake/auto-kicad-shadow");
        let shadow = ShadowProject {
            root: root.clone(),
            board_path: root.join(filename),
            project_path: root.join(format!("{stem}.kicad_pro")),
        };
        self.shadow = Some(shadow.clone());
        Ok(shadow)
    }

    fn register_launch(
        &mut self,
        shadow: &ShadowProject,
        identity: OwnedEditorIdentity,
    ) -> Result<(), LifecycleError> {
        FilesystemDisposablePcbLifecycle::validate_identity(&identity)?;
        if self.shadow.as_ref() != Some(shadow) {
            return Err(LifecycleError::NoShadow);
        }
        if self.editor.is_some() {
            return Err(LifecycleError::InvalidTransition(
                "editor already registered",
            ));
        }
        if identity.board_filename
            != shadow
                .board_path
                .file_name()
                .and_then(|value| value.to_str())
                .unwrap_or_default()
        {
            return Err(LifecycleError::DocumentMismatch);
        }
        self.editor = Some(identity);
        Ok(())
    }

    fn close_owned(&mut self, pid: u32) -> Result<(), LifecycleError> {
        let editor = self.editor.as_ref().ok_or(LifecycleError::NoOwnedEditor)?;
        if editor.pid != pid {
            return Err(LifecycleError::ForeignEditor);
        }
        if self.closed {
            return Err(LifecycleError::InvalidTransition("editor already closed"));
        }
        self.closed = true;
        Ok(())
    }

    fn register_reopen(&mut self, identity: OwnedEditorIdentity) -> Result<(), LifecycleError> {
        FilesystemDisposablePcbLifecycle::validate_identity(&identity)?;
        let previous = self.editor.as_ref().ok_or(LifecycleError::NoOwnedEditor)?;
        if !self.closed {
            return Err(LifecycleError::InvalidTransition(
                "owned editor must close before reopen",
            ));
        }
        if identity.pid == previous.pid {
            return Err(LifecycleError::StaleEndpoint);
        }
        if identity.board_filename != previous.board_filename {
            return Err(LifecycleError::DocumentMismatch);
        }
        if identity.document_identity == previous.document_identity {
            return Err(LifecycleError::DocumentMismatch);
        }
        self.editor = Some(identity);
        self.closed = false;
        self.reopened = true;
        Ok(())
    }

    fn is_reopened(&self) -> bool {
        self.reopened
    }

    fn cleanup(&mut self) -> Result<(), LifecycleError> {
        if self.editor.is_some() && !self.closed {
            return Err(LifecycleError::InvalidTransition(
                "owned editor must close before cleanup",
            ));
        }
        if self.shadow.take().is_none() {
            return Err(LifecycleError::NoShadow);
        }
        self.editor = None;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn fixture() -> (PathBuf, PathBuf) {
        let root = std::env::temp_dir().join(format!(
            "auto-kicad-lifecycle-{}",
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&root).unwrap();
        let board = root.join("fixture.kicad_pcb");
        fs::write(&board, "(kicad_pcb)").unwrap();
        fs::write(root.join("fixture.kicad_pro"), "{}").unwrap();
        (root, board)
    }

    fn identity(pid: u32, endpoint: &str, document: &str) -> OwnedEditorIdentity {
        OwnedEditorIdentity {
            pid,
            endpoint: endpoint.into(),
            board_filename: "fixture.kicad_pcb".into(),
            document_identity: document.into(),
        }
    }

    #[test]
    fn owned_launcher_never_claims_a_preexisting_default_endpoint() {
        assert_eq!(
            OwnedPcbnewLauncher::owned_endpoints(42, false),
            ["ipc:///tmp/kicad/api-42.sock"]
        );
        assert_eq!(
            OwnedPcbnewLauncher::owned_endpoints(42, true),
            [
                "ipc:///tmp/kicad/api.sock".to_owned(),
                "ipc:///tmp/kicad/api-42.sock".to_owned(),
            ]
        );
    }

    #[test]
    fn fake_lifecycle_has_the_same_ownership_contract_without_io() {
        let mut lifecycle = FakeDisposablePcbLifecycle::default();
        let shadow = lifecycle
            .create_shadow(Path::new("fixture.kicad_pcb"))
            .unwrap();
        lifecycle
            .register_launch(&shadow, identity(20, "ipc://fake-one", "fake-doc-one"))
            .unwrap();
        assert!(!lifecycle.is_reopened());
        assert_eq!(
            lifecycle.close_owned(21),
            Err(LifecycleError::ForeignEditor)
        );
        lifecycle.close_owned(20).unwrap();
        lifecycle
            .register_reopen(identity(21, "ipc://fake-two", "fake-doc-two"))
            .unwrap();
        assert!(lifecycle.is_reopened());
        lifecycle.close_owned(21).unwrap();
        lifecycle.cleanup().unwrap();
    }

    #[test]
    fn shadow_contains_only_board_and_matching_project() {
        let (source_root, board) = fixture();
        let server = source_root.join("server");
        let mut lifecycle = FilesystemDisposablePcbLifecycle::new(&server).unwrap();
        let shadow = lifecycle.create_shadow(&board).unwrap();
        assert!(shadow.board_path().is_file());
        assert!(shadow.project_path().is_file());
        assert_eq!(fs::read_dir(shadow.root()).unwrap().count(), 2);
        lifecycle.cleanup().unwrap();
        fs::remove_dir_all(source_root).unwrap();
    }

    #[test]
    fn locks_and_missing_project_are_rejected() {
        let (source_root, board) = fixture();
        fs::write(lock_path(&board), "lock").unwrap();
        let mut lifecycle =
            FilesystemDisposablePcbLifecycle::new(&source_root.join("server")).unwrap();
        assert_eq!(
            lifecycle.create_shadow(&board),
            Err(LifecycleError::SourceLockPresent)
        );
        fs::remove_file(lock_path(&board)).unwrap();
        fs::remove_file(board.with_extension("kicad_pro")).unwrap();
        assert_eq!(
            lifecycle.create_shadow(&board),
            Err(LifecycleError::ProjectMissing)
        );
        fs::remove_dir_all(source_root).unwrap();
    }

    #[test]
    fn ownership_and_fresh_reopen_are_enforced() {
        let (source_root, board) = fixture();
        let mut lifecycle =
            FilesystemDisposablePcbLifecycle::new(&source_root.join("server")).unwrap();
        let shadow = lifecycle.create_shadow(&board).unwrap();
        lifecycle
            .register_launch(&shadow, identity(10, "ipc://one", "doc-one"))
            .unwrap();
        assert_eq!(
            lifecycle.close_owned(11),
            Err(LifecycleError::ForeignEditor)
        );
        assert_eq!(
            lifecycle.register_reopen(identity(12, "ipc://two", "doc-two")),
            Err(LifecycleError::InvalidTransition(
                "owned editor must close before reopen"
            ))
        );
        lifecycle.close_owned(10).unwrap();
        lifecycle
            .register_reopen(identity(12, "ipc://one", "doc-two"))
            .unwrap();
        assert_eq!(
            lifecycle.register_reopen(identity(13, "ipc://three", "doc-three")),
            Err(LifecycleError::InvalidTransition(
                "owned editor must close before reopen"
            ))
        );
        lifecycle.close_owned(12).unwrap();
        lifecycle.cleanup().unwrap();
        fs::remove_dir_all(source_root).unwrap();
    }
}
