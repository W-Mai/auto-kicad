//! Fixed native process inputs; environment values are never included in diagnostics.
use crate::NetlistRunnerError;
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    ffi::{OsStr, OsString},
    fs::{self, File},
    io::Read,
    path::{Path, PathBuf},
    process::Command,
};

#[derive(Clone, Eq, PartialEq)]
struct Environment(BTreeMap<OsString, OsString>);
impl Environment {
    fn capture() -> Result<Self, NetlistRunnerError> {
        Self::from_pairs(std::env::vars_os())
    }
    fn from_pairs(
        pairs: impl IntoIterator<Item = (OsString, OsString)>,
    ) -> Result<Self, NetlistRunnerError> {
        let mut values = BTreeMap::new();
        let mut size = 0;
        for (key, value) in pairs {
            size += key.as_encoded_bytes().len() + value.as_encoded_bytes().len();
            if size > 1024 * 1024
                || values.len() >= 4096
                || key.is_empty()
                || key.as_encoded_bytes().contains(&b'=')
                || key.as_encoded_bytes().contains(&0)
                || value.as_encoded_bytes().contains(&0)
            {
                return Err(error(
                    "process environment exceeds bounds or contains invalid entries",
                ));
            }
            if values.insert(key, value).is_some() {
                return Err(error("duplicate process environment key"));
            }
        }
        Ok(Self(values))
    }
    fn digest(&self) -> String {
        let mut hash = Sha256::new();
        for (key, value) in &self.0 {
            for token in [key, value] {
                let bytes = token.as_encoded_bytes();
                hash.update((bytes.len() as u64).to_le_bytes());
                hash.update(bytes);
            }
        }
        hex::encode(hash.finalize())
    }
}

/// Immutable executable/environment binding. Configuration and project-file binding are separate.
#[derive(Clone)]
pub struct NativeExecutionContext {
    executable: PathBuf,
    executable_digest: String,
    inherited: Environment,
    effective: Environment,
    environment_digest: String,
    configuration_home: Option<PathBuf>,
    digest: String,
}
impl std::fmt::Debug for NativeExecutionContext {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("NativeExecutionContext")
            .field("digest", &self.digest)
            .field("executable", &self.executable)
            .field("configuration_home", &self.configuration_home)
            .finish_non_exhaustive()
    }
}
fn error(message: &str) -> NetlistRunnerError {
    NetlistRunnerError::ExecutionContext(message.into())
}
fn binary_digest(path: &Path) -> Result<String, NetlistRunnerError> {
    let mut file = File::open(path).map_err(|_| error("native executable unavailable"))?;
    let meta = file
        .metadata()
        .map_err(|_| error("native executable metadata unavailable"))?;
    if !meta.is_file() || meta.len() == 0 || meta.len() > 128 * 1024 * 1024 {
        return Err(error("native executable exceeds bounds"));
    }
    let mut hash = Sha256::new();
    let mut buffer = [0u8; 65536];
    let mut total = 0u64;
    loop {
        let count = file
            .read(&mut buffer)
            .map_err(|_| error("native executable read failed"))?;
        if count == 0 {
            break;
        }
        total += count as u64;
        if total > meta.len() {
            return Err(error("native executable changed during reading"));
        }
        hash.update(&buffer[..count]);
    }
    if total != meta.len() {
        return Err(error("native executable changed during reading"));
    }
    Ok(hex::encode(hash.finalize()))
}
impl NativeExecutionContext {
    /// Hash inherited process inputs without disclosing their values.
    pub fn current_inherited_environment_digest() -> Result<String, NetlistRunnerError> {
        Ok(Environment::capture()?.digest())
    }

    pub fn inherited_environment_digest(&self) -> String {
        self.inherited.digest()
    }

    pub(crate) fn capture(
        executable: &Path,
        configuration_home: Option<&Path>,
    ) -> Result<Self, NetlistRunnerError> {
        let executable =
            fs::canonicalize(executable).map_err(|_| error("native executable unavailable"))?;
        let executable_digest = binary_digest(&executable)?;
        let inherited = Environment::capture()?;
        let mut effective = inherited.clone();
        let configuration_home = configuration_home
            .map(|p| {
                let canonical = fs::canonicalize(p)
                    .map_err(|_| error("native configuration directory unavailable"))?;
                if !p.is_absolute() || canonical != p || !canonical.is_dir() {
                    return Err(error("native configuration directory must be canonical"));
                }
                Ok(canonical)
            })
            .transpose()?;
        if let Some(path) = &configuration_home {
            effective
                .0
                .insert("KICAD_CONFIG_HOME".into(), path.as_os_str().to_owned());
        }
        let effective = Environment::from_pairs(effective.0)?;
        let environment_digest = effective.digest();
        let mut hash = Sha256::new();
        hash.update(b"native-execution-context-v1\0");
        hash.update(executable.as_os_str().as_encoded_bytes());
        hash.update([0]);
        hash.update(&executable_digest);
        hash.update(&environment_digest);
        let digest = hex::encode(hash.finalize());
        let result = Self {
            executable,
            executable_digest,
            inherited,
            effective,
            environment_digest,
            configuration_home,
            digest,
        };
        result.verify()?;
        Ok(result)
    }
    pub fn digest(&self) -> &str {
        &self.digest
    }
    pub fn executable_digest(&self) -> &str {
        &self.executable_digest
    }
    pub fn environment_digest(&self) -> &str {
        &self.environment_digest
    }
    pub fn executable(&self) -> &Path {
        &self.executable
    }
    pub fn configuration_home(&self) -> Option<&Path> {
        self.configuration_home.as_deref()
    }
    /// Enumerate captured variable names for server-side resource resolution.
    pub fn environment_names(&self) -> impl Iterator<Item = &OsStr> {
        self.effective.0.keys().map(OsString::as_os_str)
    }

    /// For server-side resource resolution. Do not include returned values in diagnostics.
    pub fn environment_value(&self, name: &OsStr) -> Option<&OsStr> {
        self.effective.0.get(name).map(OsString::as_os_str)
    }
    pub fn verify(&self) -> Result<(), NetlistRunnerError> {
        if Environment::capture()? != self.inherited {
            return Err(error("inherited process environment changed"));
        }
        if fs::canonicalize(&self.executable).ok().as_ref() != Some(&self.executable)
            || binary_digest(&self.executable)? != self.executable_digest
        {
            return Err(error("native executable changed"));
        }
        if let Some(path) = &self.configuration_home
            && (fs::canonicalize(path).ok().as_ref() != Some(path) || !path.is_dir())
        {
            return Err(error("native configuration directory changed"));
        }
        Ok(())
    }
    /// Apply the exact captured environment to a trusted native process command.
    pub fn apply(&self, command: &mut Command, project_root: &Path) {
        runtime::owned_temp::inherit_leases(command);
        command
            .env_clear()
            .envs(&self.effective.0)
            .current_dir(project_root);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn environment_hash_is_unambiguous_ordered_and_bounded() {
        let make = |pairs: &[(&str, &str)]| {
            Environment::from_pairs(
                pairs
                    .iter()
                    .map(|(k, v)| (OsString::from(k), OsString::from(v))),
            )
        };
        assert_eq!(
            make(&[("A", "bc"), ("D", "e")]).unwrap().digest(),
            make(&[("D", "e"), ("A", "bc")]).unwrap().digest()
        );
        assert_ne!(
            make(&[("A", "bc")]).unwrap().digest(),
            make(&[("Ab", "c")]).unwrap().digest()
        );
        assert!(make(&[("A", "1"), ("A", "2")]).is_err());
        assert!(make(&[("A=B", "1")]).is_err());
        assert!(make(&[("A", "\0")]).is_err());
        assert!(make(&[("A", &"x".repeat(1024 * 1024))]).is_err());
    }
    #[test]
    fn executable_drift_and_directory_replacement_reject() {
        let dir = tempfile::tempdir().unwrap();
        let root = fs::canonicalize(dir.path()).unwrap();
        let exe = root.join("cli");
        fs::write(&exe, b"native-one").unwrap();
        let config = root.join("config");
        fs::create_dir(&config).unwrap();
        let context = NativeExecutionContext::capture(&exe, Some(&config)).unwrap();
        context.verify().unwrap();
        let mut command = Command::new(&exe);
        context.apply(&mut command, &root);
        assert_eq!(command.get_current_dir(), Some(root.as_path()));
        assert!(
            command
                .get_envs()
                .any(|(k, v)| k == "KICAD_CONFIG_HOME" && v == Some(config.as_os_str()))
        );
        assert_eq!(
            context.environment_value(OsStr::new("KICAD_CONFIG_HOME")),
            Some(config.as_os_str())
        );
        fs::write(&exe, b"native-two").unwrap();
        assert!(context.verify().is_err());
        fs::write(&exe, b"native-one").unwrap();
        context.verify().unwrap();
        fs::remove_dir(&config).unwrap();
        fs::write(&config, b"replaced").unwrap();
        assert!(context.verify().is_err());
        let mut debug_context = context;
        debug_context
            .effective
            .0
            .insert("TEST_SECRET".into(), "not-for-logs".into());
        assert!(!format!("{debug_context:?}").contains("not-for-logs"));
        assert!(!format!("{debug_context:?}").contains("TEST_SECRET"));
    }
}
