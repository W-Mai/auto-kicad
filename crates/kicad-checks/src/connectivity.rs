//! Immutable native electrical partitions of exact saved board and project bytes.
use crate::{
    CheckFailure, DrcPolicy, confined_board_path, outline::bounded_file, read_bounded_file,
    terminate_child_tree,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::BTreeSet;
use std::fs::{self, File};
use std::io;
#[cfg(unix)]
use std::os::unix::{fs::PermissionsExt, process::CommandExt};
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};
use wait_timeout::ChildExt;

const MAX_INPUT: u64 = 32 * 1024 * 1024;
const MAX_REPORT: u64 = 2 * 1024 * 1024;
const MAX_ITEMS: usize = 4096;
const PYTHON: &str =
    "/Applications/KiCad/KiCad.app/Contents/Frameworks/Python.framework/Versions/3.9/bin/python3.9";
const HELPER: &str = include_str!("../helpers/connectivity_check.py");

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ConnectivityKind {
    #[serde(rename = "PAD")]
    Pad,
    #[serde(rename = "PCB_TRACK")]
    Track,
    #[serde(rename = "PCB_ARC")]
    Arc,
    #[serde(rename = "PCB_VIA")]
    ThroughVia,
    #[serde(rename = "PCB_SHAPE")]
    CopperGraphic,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ConnectivityEndpoint {
    uuid: String,
    kind: ConnectivityKind,
    net: String,
    position_nm: [i64; 2],
    component: usize,
}
impl ConnectivityEndpoint {
    pub fn uuid(&self) -> &str {
        &self.uuid
    }
    pub fn kind(&self) -> ConnectivityKind {
        self.kind
    }
    pub fn net(&self) -> &str {
        &self.net
    }
    pub fn position_nm(&self) -> [i64; 2] {
        self.position_nm
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct NativeConnectivity {
    kicad_version: String,
    board_sha256: String,
    project_sha256: String,
    endpoints: Vec<ConnectivityEndpoint>,
    components: Vec<Vec<String>>,
}

/// Constructed only by the bounded native reader; not a live IPC or approval proof.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct ClosedConnectivityEvidence {
    native: NativeConnectivity,
}
impl ClosedConnectivityEvidence {
    /// Revalidate the exact saved inputs without launching another worker.
    pub fn verify_saved_inputs(
        &self,
        board: &Path,
        policy: &DrcPolicy,
    ) -> Result<(), CheckFailure> {
        let canonical = confined_board_path(board, &policy.allowed_root)?;
        let supplied = bounded_file(board, MAX_INPUT)?;
        let board = canonical;
        if digest(&supplied) != self.board_sha256()
            || digest(&bounded_file(&board, MAX_INPUT)?) != self.board_sha256()
            || digest(&bounded_file(
                &board.with_extension("kicad_pro"),
                MAX_INPUT,
            )?) != self.project_sha256()
        {
            return Err(invalid("saved connectivity inputs changed"));
        }
        Ok(())
    }

    pub fn board_sha256(&self) -> &str {
        &self.native.board_sha256
    }
    pub fn project_sha256(&self) -> &str {
        &self.native.project_sha256
    }
    pub fn endpoints(&self) -> &[ConnectivityEndpoint] {
        &self.native.endpoints
    }
    pub fn components(&self) -> &[Vec<String>] {
        &self.native.components
    }
    pub fn endpoint(&self, uuid: &str) -> Option<&ConnectivityEndpoint> {
        self.native
            .endpoints
            .binary_search_by(|item| item.uuid.as_str().cmp(uuid))
            .ok()
            .map(|index| &self.native.endpoints[index])
    }
    pub fn component(&self, uuid: &str) -> Option<&[String]> {
        self.endpoint(uuid)
            .map(|endpoint| self.native.components[endpoint.component].as_slice())
    }
}

fn invalid(message: impl ToString) -> CheckFailure {
    CheckFailure::InvalidInput {
        message: message.to_string(),
    }
}
fn malformed(message: impl ToString) -> CheckFailure {
    CheckFailure::MalformedReport {
        message: message.to_string(),
    }
}
fn digest(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}
fn valid_uuid(id: &str) -> bool {
    id.len() == 36
        && id.bytes().enumerate().all(|(index, value)| {
            if [8, 13, 18, 23].contains(&index) {
                value == b'-'
            } else {
                value.is_ascii_digit() || (b'a'..=b'f').contains(&value)
            }
        })
        && id != "00000000-0000-0000-0000-000000000000"
}

fn validate(
    native: NativeConnectivity,
    board: &[u8],
    project: &[u8],
) -> Result<ClosedConnectivityEvidence, CheckFailure> {
    if native.kicad_version != "10.0.7"
        || native.board_sha256 != digest(board)
        || native.project_sha256 != digest(project)
        || native.endpoints.len() > MAX_ITEMS
        || native.components.len() > native.endpoints.len()
        || native
            .endpoints
            .windows(2)
            .any(|pair| pair[0].uuid >= pair[1].uuid)
        || native.components.windows(2).any(|pair| pair[0] >= pair[1])
    {
        return Err(malformed(
            "connectivity input binding, ordering or bounds mismatch",
        ));
    }
    let evidence = ClosedConnectivityEvidence { native };
    for endpoint in evidence.endpoints() {
        if !valid_uuid(&endpoint.uuid)
            || endpoint.net.len() > 4096
            || endpoint.net.contains('\0')
            || endpoint
                .position_nm
                .iter()
                .any(|v| v.unsigned_abs() > 2_000_000_000)
            || endpoint.component >= evidence.native.components.len()
        {
            return Err(malformed("invalid native connectivity endpoint"));
        }
    }
    let mut seen = BTreeSet::new();
    for (index, component) in evidence.components().iter().enumerate() {
        if component.is_empty()
            || component.len() > MAX_ITEMS
            || component.windows(2).any(|pair| pair[0] >= pair[1])
        {
            return Err(malformed("invalid native connectivity partition"));
        }
        let mut net = None;
        for uuid in component {
            let endpoint = evidence
                .endpoint(uuid)
                .ok_or_else(|| malformed("unobserved component member"))?;
            if !seen.insert(uuid)
                || endpoint.component != index
                || net.is_some_and(|value| value != endpoint.net())
                || (endpoint.net.is_empty() && component.len() != 1)
            {
                return Err(malformed(
                    "inconsistent, overlapping or cross-net native partitions",
                ));
            }
            net = Some(endpoint.net());
        }
    }
    if seen.len() != evidence.endpoints().len() {
        return Err(malformed("incomplete native connectivity partition"));
    }
    Ok(evidence)
}

/// Read physical components without filling zones or modifying source files.
pub fn check_saved_connectivity(
    board: &Path,
    policy: &DrcPolicy,
) -> Result<ClosedConnectivityEvidence, CheckFailure> {
    run(board, policy, Path::new(PYTHON))
}

fn run(
    board: &Path,
    policy: &DrcPolicy,
    executable: &Path,
) -> Result<ClosedConnectivityEvidence, CheckFailure> {
    let deadline = Instant::now() + policy.timeout.min(Duration::from_secs(60));
    // Confine before reading; O_NOFOLLOW also rejects a supplied file symlink inside the root.
    let canonical = confined_board_path(board, &policy.allowed_root)?;
    let supplied_bytes = bounded_file(board, MAX_INPUT)?;
    let board = canonical;
    let project = board.with_extension("kicad_pro");
    let board_bytes = bounded_file(&board, MAX_INPUT)?;
    let project_bytes = bounded_file(&project, MAX_INPUT)?;
    if board_bytes != supplied_bytes {
        return Err(invalid("board changed during confinement"));
    }
    let work = runtime::owned_temp::Builder::new()
        .prefix(".auto-kicad-connectivity-")
        .tempdir_in(&policy.allowed_root)
        .map_err(invalid)?;
    #[cfg(unix)]
    fs::set_permissions(work.path(), fs::Permissions::from_mode(0o700)).map_err(invalid)?;
    let snapshot = work.path().join("board.kicad_pcb");
    let snapshot_project = snapshot.with_extension("kicad_pro");
    fs::write(&snapshot, &board_bytes).map_err(invalid)?;
    fs::write(&snapshot_project, &project_bytes).map_err(invalid)?;
    let stdout_path = work.path().join("stdout.json");
    let stderr_path = work.path().join("stderr.log");
    let mut command = Command::new(executable);
    command
        .args(["-I", "-B", "-c", HELPER])
        .arg(&snapshot)
        .current_dir(work.path())
        .stdin(Stdio::null())
        .stdout(Stdio::from(File::create(&stdout_path).map_err(invalid)?))
        .stderr(Stdio::from(File::create(&stderr_path).map_err(invalid)?));
    #[cfg(unix)]
    unsafe {
        command.pre_exec(|| {
            if libc::setpgid(0, 0) != 0 {
                return Err(io::Error::last_os_error());
            }
            let limit = libc::rlimit {
                rlim_cur: MAX_REPORT,
                rlim_max: MAX_REPORT,
            };
            if libc::setrlimit(libc::RLIMIT_FSIZE, &limit) != 0 {
                return Err(io::Error::last_os_error());
            }
            Ok(())
        });
    }
    if Instant::now() >= deadline {
        return Err(invalid("connectivity deadline expired before launch"));
    }
    runtime::owned_temp::inherit_leases(&mut command);
    let mut child = command
        .spawn()
        .map_err(|error| CheckFailure::CommandUnavailable {
            message: error.to_string(),
        })?;
    let status = match child.wait_timeout(deadline.saturating_duration_since(Instant::now())) {
        Ok(Some(status)) => status,
        Ok(None) => {
            terminate_child_tree(&mut child);
            return Err(CheckFailure::Timeout {
                stdout: read_bounded_file(&stdout_path),
                stderr: read_bounded_file(&stderr_path),
            });
        }
        Err(error) => {
            terminate_child_tree(&mut child);
            return Err(CheckFailure::ProcessFailed {
                status: None,
                stdout: read_bounded_file(&stdout_path),
                stderr: error.to_string(),
            });
        }
    };
    let stderr = read_bounded_file(&stderr_path);
    let unexpected = stderr.lines().any(|line| !line.trim().is_empty()
        && line != "./src/common/stdpbase.cpp(59): assert \"\"traits\"\" failed in Get(): create wxApp before calling this");
    if !status.success() || unexpected {
        return Err(CheckFailure::ProcessFailed {
            status: status.code(),
            stdout: read_bounded_file(&stdout_path),
            stderr,
        });
    }
    let bytes = bounded_file(&stdout_path, MAX_REPORT)?;
    let native = serde_json::from_slice(&bytes).map_err(malformed)?;
    let evidence = validate(native, &board_bytes, &project_bytes)?;
    if bounded_file(&snapshot, MAX_INPUT)? != board_bytes
        || bounded_file(&snapshot_project, MAX_INPUT)? != project_bytes
        || bounded_file(&board, MAX_INPUT)? != board_bytes
        || bounded_file(&project, MAX_INPUT)? != project_bytes
    {
        return Err(invalid("connectivity source or private input changed"));
    }
    Ok(evidence)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn fixture() -> NativeConnectivity {
        let ids = [
            "11111111-1111-1111-1111-111111111111",
            "22222222-2222-2222-2222-222222222222",
        ];
        NativeConnectivity {
            kicad_version: "10.0.7".into(),
            board_sha256: digest(b"board"),
            project_sha256: digest(b"project"),
            endpoints: ids
                .iter()
                .map(|id| ConnectivityEndpoint {
                    uuid: (*id).into(),
                    kind: ConnectivityKind::Pad,
                    net: "GND".into(),
                    position_nm: [0, 0],
                    component: 0,
                })
                .collect(),
            components: vec![ids.iter().map(|id| (*id).into()).collect()],
        }
    }
    #[test]
    fn evidence_requires_exact_inputs_and_complete_disjoint_same_net_partitions() {
        let good = fixture();
        let proof = validate(good.clone(), b"board", b"project").unwrap();
        assert_eq!(proof.component(&good.endpoints[0].uuid).unwrap().len(), 2);
        assert!(validate(good.clone(), b"changed", b"project").is_err());
        assert!(validate(good.clone(), b"board", b"changed").is_err());
        let reject = |native| assert!(validate(native, b"board", b"project").is_err());
        let mut bad = good.clone();
        bad.endpoints[1].net = "VCC".into();
        reject(bad);
        let mut bad = good.clone();
        bad.endpoints[1].component = 1;
        reject(bad);
        let mut bad = good.clone();
        bad.endpoints[1] = bad.endpoints[0].clone();
        reject(bad);
        let mut bad = good.clone();
        bad.components[0].pop();
        reject(bad);
        let mut bad = good.clone();
        bad.components[0].push("foreign".into());
        reject(bad);
        let mut bad = good.clone();
        bad.components.push(bad.components[0].clone());
        reject(bad);
        let mut bad = good.clone();
        bad.endpoints.iter_mut().for_each(|item| item.net.clear());
        reject(bad);
        let mut bad = good.clone();
        bad.endpoints[0].position_nm[0] = i64::MIN;
        reject(bad);
        let mut islands = good;
        islands.components = islands
            .endpoints
            .iter()
            .map(|item| vec![item.uuid.clone()])
            .collect();
        islands.endpoints[1].component = 1;
        let islands = validate(islands, b"board", b"project").unwrap();
        assert_ne!(
            islands.component(islands.endpoints()[0].uuid()),
            islands.component(islands.endpoints()[1].uuid())
        );
    }
}
