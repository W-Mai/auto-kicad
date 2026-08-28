use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

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
        if identity.pid == previous.pid || identity.endpoint == previous.endpoint {
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
        if identity.pid == previous.pid || identity.endpoint == previous.endpoint {
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
        Ok(())
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
    fn fake_lifecycle_has_the_same_ownership_contract_without_io() {
        let mut lifecycle = FakeDisposablePcbLifecycle::default();
        let shadow = lifecycle
            .create_shadow(Path::new("fixture.kicad_pcb"))
            .unwrap();
        lifecycle
            .register_launch(&shadow, identity(20, "ipc://fake-one", "fake-doc-one"))
            .unwrap();
        assert_eq!(
            lifecycle.close_owned(21),
            Err(LifecycleError::ForeignEditor)
        );
        lifecycle.close_owned(20).unwrap();
        lifecycle
            .register_reopen(identity(21, "ipc://fake-two", "fake-doc-two"))
            .unwrap();
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
        assert_eq!(
            lifecycle.register_reopen(identity(12, "ipc://one", "doc-two")),
            Err(LifecycleError::StaleEndpoint)
        );
        lifecycle
            .register_reopen(identity(12, "ipc://two", "doc-two"))
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
