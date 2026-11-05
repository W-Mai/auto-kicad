//! Persistent ownership and inherited process leases for native temporary directories.
use serde::{Deserialize, Serialize};
#[cfg(unix)]
use std::os::unix::{
    ffi::OsStrExt,
    fs::{MetadataExt, OpenOptionsExt, PermissionsExt},
    io::AsRawFd,
    process::CommandExt,
};
use std::{
    ffi::{CString, OsStr, OsString},
    fs::{self, File, OpenOptions},
    io::{self, Read, Seek, SeekFrom, Write},
    path::{Path, PathBuf},
    process::Command,
    sync::{Arc, Mutex, OnceLock, Weak},
    time::{SystemTime, UNIX_EPOCH},
};

const ATTRIBUTE: &[u8] = b"user.auto-kicad.owner-v1\0";
const MAX_RECORDS: usize = 4096;
static LEASES: OnceLock<Mutex<Vec<Weak<File>>>> = OnceLock::new();
static LAST_CLEANUP: OnceLock<CleanupReport> = OnceLock::new();

#[derive(Clone, Debug, Default, Serialize)]
pub struct CleanupReport {
    pub removed: usize,
    pub live_leases: usize,
    pub live_or_reused_owners: usize,
    pub unrecognized: usize,
    pub errors: usize,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Owner {
    schema_version: u32,
    token: String,
    path: PathBuf,
    device: u64,
    inode: u64,
    pid: u32,
    process_birth: Option<String>,
    released: bool,
    created_ns: u128,
}

pub struct TempDir {
    inner: Option<tempfile::TempDir>,
    lease: Option<Arc<File>>,
    record: PathBuf,
    owner: Owner,
}
impl std::fmt::Debug for TempDir {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("OwnedTempDir")
            .field("path", &self.owner.path)
            .finish_non_exhaustive()
    }
}
impl TempDir {
    pub fn path(&self) -> &Path {
        self.inner.as_ref().expect("temporary owner exists").path()
    }
    pub fn close(mut self) -> io::Result<()> {
        self.release()
    }
    fn release(&mut self) -> io::Result<()> {
        let Some(inner) = self.inner.take() else {
            return Ok(());
        };
        // The OS lease, not a surviving Rust parent, determines whether a native worker still owns bytes.
        let _retained = inner.keep();
        self.owner.released = true;
        if let Some(file) = &self.lease {
            let bytes = serde_json::to_vec(&self.owner)
                .map_err(|_| invalid("cannot update temporary release state"))?;
            let mut writer = &**file;
            writer.seek(SeekFrom::Start(0))?;
            writer.write_all(&bytes)?;
            writer.set_len(bytes.len() as u64)?;
            writer.sync_all()?;
        }
        self.lease.take();
        match reclaim(&self.record, Some(&self.owner), true)? {
            Reclaimed::Removed | Reclaimed::Absent => Ok(()),
            Reclaimed::Leased | Reclaimed::OwnerAlive => Ok(()),
        }
    }
}
impl AsRef<Path> for TempDir {
    fn as_ref(&self) -> &Path {
        self.path()
    }
}
impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = self.release();
    }
}

#[derive(Debug)]
pub struct Builder {
    prefix: OsString,
    suffix: OsString,
}
impl Default for Builder {
    fn default() -> Self {
        Self::new()
    }
}
impl Builder {
    pub fn new() -> Self {
        Self {
            prefix: OsString::from(".tmp"),
            suffix: OsString::new(),
        }
    }
    pub fn prefix<S: AsRef<OsStr>>(&mut self, prefix: S) -> &mut Self {
        self.prefix = prefix.as_ref().to_owned();
        self
    }
    pub fn suffix<S: AsRef<OsStr>>(&mut self, suffix: S) -> &mut Self {
        self.suffix = suffix.as_ref().to_owned();
        self
    }
    pub fn tempdir(&self) -> io::Result<TempDir> {
        register(
            tempfile::Builder::new()
                .prefix(&self.prefix)
                .suffix(&self.suffix)
                .tempdir()?,
        )
    }
    pub fn tempdir_in<P: AsRef<Path>>(&self, parent: P) -> io::Result<TempDir> {
        register(
            tempfile::Builder::new()
                .prefix(&self.prefix)
                .suffix(&self.suffix)
                .tempdir_in(parent)?,
        )
    }
    pub fn tempfile(&self) -> io::Result<tempfile::NamedTempFile> {
        tempfile::Builder::new()
            .prefix(&self.prefix)
            .suffix(&self.suffix)
            .tempfile()
    }
    pub fn tempfile_in<P: AsRef<Path>>(&self, parent: P) -> io::Result<tempfile::NamedTempFile> {
        tempfile::Builder::new()
            .prefix(&self.prefix)
            .suffix(&self.suffix)
            .tempfile_in(parent)
    }
}

fn invalid(message: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}
fn registry() -> io::Result<PathBuf> {
    let root = fs::canonicalize(std::env::temp_dir())?.join(".auto-kicad-temp-owners-v1");
    match fs::create_dir(&root) {
        Ok(()) => {
            #[cfg(unix)]
            fs::set_permissions(&root, fs::Permissions::from_mode(0o700))?;
        }
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
        Err(error) => return Err(error),
    }
    let metadata = fs::symlink_metadata(&root)?;
    if !metadata.is_dir() || fs::canonicalize(&root)? != root {
        return Err(invalid(
            "temporary ownership registry is not a canonical directory",
        ));
    }
    #[cfg(unix)]
    if metadata.uid() != unsafe { libc::geteuid() } || metadata.mode() & 0o077 != 0 {
        return Err(invalid(
            "temporary ownership registry has untrusted ownership or mode",
        ));
    }
    Ok(root)
}
fn process_birth() -> Option<String> {
    static BIRTH: OnceLock<Option<String>> = OnceLock::new();
    BIRTH
        .get_or_init(|| {
            let output = Command::new("/bin/ps")
                .args(["-p", &std::process::id().to_string(), "-o", "lstart="])
                .output()
                .ok()?;
            if !output.status.success() || output.stdout.len() > 512 {
                return None;
            }
            let value = String::from_utf8(output.stdout).ok()?.trim().to_owned();
            if value.is_empty() { None } else { Some(value) }
        })
        .clone()
}
fn random_token() -> io::Result<String> {
    let mut bytes = [0u8; 32];
    File::open("/dev/urandom")?.read_exact(&mut bytes)?;
    Ok(bytes.iter().map(|byte| format!("{byte:02x}")).collect())
}
fn open_directory(path: &Path) -> io::Result<File> {
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    options.custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW);
    let file = options.open(path)?;
    if !file.metadata()?.is_dir() {
        return Err(invalid("owned resource is not a directory"));
    }
    Ok(file)
}
#[cfg(unix)]
fn set_token(directory: &File, token: &str) -> io::Result<()> {
    #[cfg(target_os = "macos")]
    let status = unsafe {
        libc::fsetxattr(
            directory.as_raw_fd(),
            ATTRIBUTE.as_ptr().cast(),
            token.as_ptr().cast(),
            token.len(),
            0,
            libc::XATTR_CREATE,
        )
    };
    #[cfg(target_os = "linux")]
    let status = unsafe {
        libc::fsetxattr(
            directory.as_raw_fd(),
            ATTRIBUTE.as_ptr().cast(),
            token.as_ptr().cast(),
            token.len(),
            libc::XATTR_CREATE,
        )
    };
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    return Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "temporary ownership attributes unavailable",
    ));
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    if status != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}
#[cfg(not(unix))]
fn set_token(_directory: &File, _token: &str) -> io::Result<()> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "native temporary ownership unavailable",
    ))
}
#[cfg(unix)]
fn token_matches(directory: &File, token: &str) -> io::Result<bool> {
    let mut bytes = [0u8; 64];
    #[cfg(target_os = "macos")]
    let size = unsafe {
        libc::fgetxattr(
            directory.as_raw_fd(),
            ATTRIBUTE.as_ptr().cast(),
            bytes.as_mut_ptr().cast(),
            bytes.len(),
            0,
            0,
        )
    };
    #[cfg(target_os = "linux")]
    let size = unsafe {
        libc::fgetxattr(
            directory.as_raw_fd(),
            ATTRIBUTE.as_ptr().cast(),
            bytes.as_mut_ptr().cast(),
            bytes.len(),
        )
    };
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    return Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "temporary ownership attributes unavailable",
    ));
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    {
        if size < 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(size as usize == bytes.len() && token.as_bytes() == bytes)
    }
}
#[cfg(not(unix))]
fn token_matches(_directory: &File, _token: &str) -> io::Result<bool> {
    Ok(false)
}
fn try_lease(file: &File) -> io::Result<bool> {
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    {
        // OFD locks remain attached to the inherited open file description after its creator dies.
        let mut lock: libc::flock = unsafe { std::mem::zeroed() };
        lock.l_type = libc::F_WRLCK as _;
        lock.l_whence = libc::SEEK_SET as _;
        #[cfg(target_os = "macos")]
        const SET_OFD_LOCK: libc::c_int = 90;
        #[cfg(target_os = "linux")]
        const SET_OFD_LOCK: libc::c_int = libc::F_OFD_SETLK;
        if unsafe { libc::fcntl(file.as_raw_fd(), SET_OFD_LOCK, &lock) } == 0 {
            return Ok(true);
        }
        let error = io::Error::last_os_error();
        if matches!(error.raw_os_error(), Some(libc::EAGAIN | libc::EACCES)) {
            return Ok(false);
        }
        Err(error)
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    {
        let _ = file;
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "native OFD leases unavailable",
        ))
    }
}
fn register(inner: tempfile::TempDir) -> io::Result<TempDir> {
    let root = registry()?;
    let path = fs::canonicalize(inner.path())?;
    let directory = open_directory(&path)?;
    let metadata = directory.metadata()?;
    let token = random_token()?;
    set_token(&directory, &token)?;
    #[cfg(unix)]
    let (device, inode) = (metadata.dev(), metadata.ino());
    #[cfg(not(unix))]
    let (device, inode) = (0, 0);
    let owner = Owner {
        schema_version: 1,
        token: token.clone(),
        path,
        device,
        inode,
        pid: std::process::id(),
        process_birth: process_birth(),
        released: false,
        created_ns: SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|_| invalid("clock before epoch"))?
            .as_nanos(),
    };
    let record = root.join(format!("{token}.json"));
    let mut options = OpenOptions::new();
    options.read(true).write(true).create_new(true);
    #[cfg(unix)]
    options.mode(0o600).custom_flags(libc::O_NOFOLLOW);
    let mut file = options.open(&record)?;
    if !try_lease(&file)? {
        return Err(invalid("new ownership lease is already held"));
    }
    let encoded =
        serde_json::to_vec(&owner).map_err(|_| invalid("cannot encode temporary ownership"))?;
    file.write_all(&encoded)?;
    file.sync_all()?;
    File::open(&root)?.sync_all()?;
    let lease = Arc::new(file);
    LEASES
        .get_or_init(|| Mutex::new(Vec::new()))
        .lock()
        .map_err(|_| invalid("temporary lease registry poisoned"))?
        .push(Arc::downgrade(&lease));
    Ok(TempDir {
        inner: Some(inner),
        lease: Some(lease),
        record,
        owner,
    })
}

/// Inherit every live ownership lease without changing process environment or CAD recovery fingerprints.
pub fn inherit_leases(command: &mut Command) {
    #[cfg(unix)]
    {
        let leases = if let Some(registry) = LEASES.get() {
            let mut registry = registry.lock().expect("temporary lease registry");
            registry.retain(|lease| lease.strong_count() > 0);
            registry
                .iter()
                .filter_map(Weak::upgrade)
                .collect::<Vec<_>>()
        } else {
            Vec::new()
        };
        if leases.is_empty() {
            return;
        }
        let descriptors = leases
            .iter()
            .map(|file| file.as_raw_fd())
            .collect::<Vec<_>>();
        // pre_exec forces the fork/exec path and preserves these exact open-file-description leases.
        unsafe {
            command.pre_exec(move || {
                let _held = &leases;
                for descriptor in &descriptors {
                    let flags = libc::fcntl(*descriptor, libc::F_GETFD);
                    if flags < 0
                        || libc::fcntl(*descriptor, libc::F_SETFD, flags & !libc::FD_CLOEXEC) < 0
                    {
                        return Err(io::Error::last_os_error());
                    }
                }
                Ok(())
            });
        }
    }
    #[cfg(not(unix))]
    let _ = command;
}

enum Reclaimed {
    Removed,
    Absent,
    Leased,
    OwnerAlive,
}
fn owner_alive(pid: u32) -> bool {
    #[cfg(unix)]
    {
        if pid == 0 {
            return true;
        }
        let result = unsafe { libc::kill(pid as i32, 0) };
        result == 0 || io::Error::last_os_error().raw_os_error() != Some(libc::ESRCH)
    }
    #[cfg(not(unix))]
    {
        let _ = pid;
        true
    }
}
fn verified_directory(path: &Path, owner: &Owner) -> io::Result<File> {
    let file = open_directory(path)?;
    let metadata = file.metadata()?;
    #[cfg(unix)]
    if metadata.dev() != owner.device || metadata.ino() != owner.inode {
        return Err(invalid("temporary resource inode/device changed"));
    }
    if !token_matches(&file, &owner.token)? {
        return Err(invalid("temporary resource ownership token changed"));
    }
    Ok(file)
}
fn rename_exclusive(source: &Path, target: &Path) -> io::Result<()> {
    #[cfg(unix)]
    {
        let source = CString::new(source.as_os_str().as_bytes())
            .map_err(|_| invalid("invalid temporary source path"))?;
        let target = CString::new(target.as_os_str().as_bytes())
            .map_err(|_| invalid("invalid temporary target path"))?;
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
        return Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "exclusive reclaim unavailable",
        ));
        #[cfg(any(target_os = "macos", target_os = "linux"))]
        if status != 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }
    #[cfg(not(unix))]
    {
        let _ = (source, target);
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "exclusive reclaim unavailable",
        ))
    }
}
fn reclaim(record: &Path, expected: Option<&Owner>, own_drop: bool) -> io::Result<Reclaimed> {
    let root = registry()?;
    if record.parent() != Some(root.as_path()) {
        return Err(invalid("untrusted temporary owner record path"));
    }
    let mut options = OpenOptions::new();
    options.read(true).write(true);
    #[cfg(unix)]
    options.custom_flags(libc::O_NOFOLLOW);
    let mut lease = match options.open(record) {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Reclaimed::Absent),
        Err(error) => return Err(error),
    };
    let metadata = lease.metadata()?;
    if !metadata.is_file() || metadata.len() > 8192 {
        return Err(invalid("invalid temporary owner record"));
    }
    if !try_lease(&lease)? {
        return Ok(Reclaimed::Leased);
    }
    let mut bytes = Vec::new();
    lease.read_to_end(&mut bytes)?;
    let owner: Owner =
        serde_json::from_slice(&bytes).map_err(|_| invalid("invalid temporary owner JSON"))?;
    if owner.schema_version != 1
        || owner.token.len() != 64
        || !owner.token.bytes().all(|value| value.is_ascii_hexdigit())
        || record.file_stem().and_then(|value| value.to_str()) != Some(owner.token.as_str())
        || expected.is_some_and(|expected| {
            expected.token != owner.token
                || expected.path != owner.path
                || expected.device != owner.device
                || expected.inode != owner.inode
        })
    {
        return Err(invalid("temporary ownership identity differs"));
    }
    if !own_drop && !owner.released && owner_alive(owner.pid) {
        return Ok(Reclaimed::OwnerAlive);
    }
    let quarantine = owner
        .path
        .parent()
        .ok_or_else(|| invalid("missing temporary parent"))?
        .join(format!(".auto-kicad-reclaim-{}", owner.token));
    let selected = if fs::symlink_metadata(&quarantine).is_ok() {
        verified_directory(&quarantine, &owner)?;
        quarantine
    } else if fs::symlink_metadata(&owner.path).is_ok() {
        let _identity = verified_directory(&owner.path, &owner)?;
        rename_exclusive(&owner.path, &quarantine)?;
        verified_directory(&quarantine, &owner)?;
        quarantine
    } else {
        fs::remove_file(record)?;
        File::open(&root)?.sync_all()?;
        return Ok(Reclaimed::Absent);
    };
    fs::remove_dir_all(&selected)?;
    #[cfg(unix)]
    {
        let named = fs::symlink_metadata(record)?;
        if named.dev() != metadata.dev() || named.ino() != metadata.ino() {
            return Err(invalid("temporary owner record changed during cleanup"));
        }
    }
    fs::remove_file(record)?;
    File::open(&root)?.sync_all()?;
    Ok(Reclaimed::Removed)
}

/// Reclaim only registered dead-owner directories whose native worker leases have all closed.
pub fn cleanup_stale() -> io::Result<CleanupReport> {
    let root = registry()?;
    let mut report = CleanupReport::default();
    let entries = fs::read_dir(&root)?
        .take(MAX_RECORDS + 1)
        .collect::<Result<Vec<_>, _>>()?;
    if entries.len() > MAX_RECORDS {
        return Err(invalid("temporary ownership scan exceeds bounds"));
    }
    for entry in entries {
        let path = entry.path();
        if path.extension() != Some(OsStr::new("json")) {
            continue;
        }
        match reclaim(&path, None, false) {
            Ok(Reclaimed::Removed | Reclaimed::Absent) => report.removed += 1,
            Ok(Reclaimed::Leased) => report.live_leases += 1,
            Ok(Reclaimed::OwnerAlive) => report.live_or_reused_owners += 1,
            Err(error) if error.kind() == io::ErrorKind::InvalidData => report.unrecognized += 1,
            Err(_) => report.errors += 1,
        }
    }
    let _ = LAST_CLEANUP.set(report.clone());
    Ok(report)
}
pub fn last_cleanup() -> Option<&'static CleanupReport> {
    LAST_CLEANUP.get()
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    #[test]
    fn native_worker_lease_outlives_rust_directory_owner() {
        let owner = Builder::new()
            .prefix("auto-kicad-lease-test-")
            .tempdir()
            .unwrap();
        let path = owner.path().to_owned();
        let record = owner.record.clone();
        fs::write(path.join("evidence"), b"native worker input").unwrap();
        let mut command = Command::new("/bin/sh");
        command.args(["-c", "sleep 0.2"]);
        inherit_leases(&mut command);
        let mut worker = command.spawn().unwrap();
        drop(command);
        owner.close().unwrap();
        assert!(path.join("evidence").is_file());
        assert!(matches!(
            reclaim(&record, None, false).unwrap(),
            Reclaimed::Leased
        ));
        assert!(worker.wait().unwrap().success());
        reclaim(&record, None, false).unwrap();
        assert!(!path.exists());
        assert!(!record.exists());
    }

    #[test]
    fn substituted_directory_is_preserved_until_original_identity_returns() {
        let owner = Builder::new()
            .prefix("auto-kicad-identity-test-")
            .tempdir()
            .unwrap();
        let path = owner.path().to_owned();
        let record = owner.record.clone();
        let moved = path.with_extension("original");
        fs::rename(&path, &moved).unwrap();
        fs::create_dir(&path).unwrap();
        fs::write(path.join("user-data"), b"keep").unwrap();
        assert!(owner.close().is_err());
        assert_eq!(fs::read(path.join("user-data")).unwrap(), b"keep");
        fs::remove_file(path.join("user-data")).unwrap();
        fs::remove_dir(&path).unwrap();
        fs::rename(&moved, &path).unwrap();
        reclaim(&record, None, false).unwrap();
        assert!(!path.exists());
        assert!(!record.exists());
    }
}
