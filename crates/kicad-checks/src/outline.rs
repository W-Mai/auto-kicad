//! Native topology evidence for a bounded private copy of saved PCB/project bytes.
use crate::{
    CheckFailure, DrcPolicy, confined_board_path, read_bounded_file, terminate_child_tree,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read};
#[cfg(unix)]
use std::os::unix::{fs::OpenOptionsExt, process::CommandExt};
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::Duration;
use wait_timeout::ChildExt;

const MAX_INPUT: u64 = 32 * 1024 * 1024;
const MAX_REPORT: u64 = 64 * 1024;
const PYTHON: &str =
    "/Applications/KiCad/KiCad.app/Contents/Frameworks/Python.framework/Versions/3.9/bin/python3.9";
const HELPER: &str = include_str!("../helpers/outline_check.py");

/// Proof of native construction, bounds and invalid-outline DRC, not complete DRC or source approval.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct OutlineTopology {
    native: NativeTopology,
    topology_drc: crate::DrcReport,
}
impl OutlineTopology {
    pub fn board_sha256(&self) -> &str {
        &self.native.board_sha256
    }
    pub fn project_sha256(&self) -> &str {
        &self.native.project_sha256
    }
    pub fn outer_count(&self) -> usize {
        self.native.outer_count
    }
    pub fn hole_counts(&self) -> &[usize] {
        &self.native.holes
    }
    pub fn bounds_nm(&self) -> [i64; 4] {
        self.native.bounds_nm
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct NativeTopology {
    kicad_version: String,
    board_sha256: String,
    project_sha256: String,
    edge_count: usize,
    outer_count: usize,
    holes: Vec<usize>,
    filled_areas_nm2: Vec<f64>,
    bounds_nm: [i64; 4],
    point_count: usize,
    chaining_epsilon_nm: i64,
    exact_endpoint_chains: bool,
    inferred_outline: bool,
}

fn invalid(message: impl ToString) -> CheckFailure {
    CheckFailure::InvalidInput {
        message: message.to_string(),
    }
}
fn bounded_file(path: &Path, limit: u64) -> Result<Vec<u8>, CheckFailure> {
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
    let file = options.open(path).map_err(invalid)?;
    let metadata = file.metadata().map_err(invalid)?;
    if !metadata.is_file() || metadata.len() == 0 || metadata.len() > limit {
        return Err(invalid("expected nonempty bounded regular file"));
    }
    let mut bytes = Vec::new();
    file.take(limit + 1)
        .read_to_end(&mut bytes)
        .map_err(invalid)?;
    if bytes.len() as u64 != metadata.len() {
        return Err(invalid("file changed size during read"));
    }
    Ok(bytes)
}
fn digest(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}
fn validate_report(
    native: NativeTopology,
    board: &[u8],
    project: &[u8],
) -> Result<NativeTopology, CheckFailure> {
    let [min_x, min_y, max_x, max_y] = native.bounds_nm;
    if native.kicad_version != "10.0.7"
        || native.board_sha256 != digest(board)
        || native.project_sha256 != digest(project)
        || !(1..=4096).contains(&native.edge_count)
        || !(1..=4096).contains(&native.outer_count)
        || native.holes.len() != native.outer_count
        || native
            .holes
            .iter()
            .try_fold(0usize, |sum, n| sum.checked_add(*n))
            .is_none_or(|n| n > 4096)
        || native.filled_areas_nm2.len() != native.outer_count
        || native
            .filled_areas_nm2
            .iter()
            .any(|n| !n.is_finite() || *n <= 0.0)
        || !(3..=500_000).contains(&native.point_count)
        || native.chaining_epsilon_nm != 1
        || !native.exact_endpoint_chains
        || native.inferred_outline
        || native
            .bounds_nm
            .iter()
            .any(|n| n.unsigned_abs() > 2_000_000_000)
        || max_x <= min_x
        || max_y <= min_y
        || max_x.saturating_sub(min_x) > 1_000_000_000
        || max_y.saturating_sub(min_y) > 1_000_000_000
    {
        return Err(CheckFailure::MalformedReport {
            message: "native outline evidence mismatch or unsupported bounds".into(),
        });
    }
    Ok(native)
}

/// Check saved geometry without inferred boundaries, endpoint snapping or source changes. Complete DRC and source/receipt/preservation checks remain independently mandatory.
pub fn check_saved_outline(
    board: &Path,
    policy: &DrcPolicy,
) -> Result<OutlineTopology, CheckFailure> {
    run(board, policy, Path::new(PYTHON))
}

fn run(
    board: &Path,
    policy: &DrcPolicy,
    executable: &Path,
) -> Result<OutlineTopology, CheckFailure> {
    let deadline = std::time::Instant::now() + policy.timeout.min(Duration::from_secs(60));
    let board = confined_board_path(board, &policy.allowed_root)?;
    let project = board.with_extension("kicad_pro");
    let board_bytes = bounded_file(&board, MAX_INPUT)?;
    let project_bytes = bounded_file(&project, MAX_INPUT)?;
    let settings: serde_json::Value = serde_json::from_slice(&project_bytes).map_err(invalid)?;
    if settings
        .pointer("/board/design_settings/rule_severities/invalid_outline")
        .and_then(|v| v.as_str())
        != Some("error")
    {
        return Err(invalid(
            "saved project must enable invalid_outline as error",
        ));
    }

    let work = tempfile::Builder::new()
        .prefix(".auto-kicad-outline-")
        .tempdir_in(&policy.allowed_root)
        .map_err(invalid)?;
    let snapshot = work.path().join("board.kicad_pcb");
    fs::write(&snapshot, &board_bytes).map_err(invalid)?;
    fs::write(snapshot.with_extension("kicad_pro"), &project_bytes).map_err(invalid)?;
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
    let mut child = command
        .spawn()
        .map_err(|e| CheckFailure::CommandUnavailable {
            message: e.to_string(),
        })?;
    let status =
        match child.wait_timeout(deadline.saturating_duration_since(std::time::Instant::now())) {
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
    // The bundled Python emits this startup diagnostic before any board API call.
    let unexpected_diagnostic = stderr.lines().any(|line| !line.trim().is_empty()
        && line != "./src/common/stdpbase.cpp(59): assert \"\"traits\"\" failed in Get(): create wxApp before calling this");
    if !status.success() || unexpected_diagnostic {
        return Err(CheckFailure::ProcessFailed {
            status: status.code(),
            stdout: read_bounded_file(&stdout_path),
            stderr,
        });
    }
    let bytes = bounded_file(&stdout_path, MAX_REPORT)?;
    let native = serde_json::from_slice(&bytes).map_err(|e| CheckFailure::MalformedReport {
        message: format!("invalid native topology response: {e}"),
    })?;
    let native = validate_report(native, &board_bytes, &project_bytes)?;
    let drc_policy = DrcPolicy {
        allowed_root: work.path().to_owned(),
        timeout: deadline.saturating_duration_since(std::time::Instant::now()),
    };
    let report =
        match crate::run_pcb_drc_with_options(&snapshot, &drc_policy, Path::new("kicad-cli"), true)
        {
            crate::CheckResult::Completed(report) => report,
            crate::CheckResult::Failed(error) => return Err(error),
        };
    if report.kicad_version != "10.0.7" || report.source != "board.kicad_pcb" {
        return Err(CheckFailure::MalformedReport {
            message: "topology DRC version or source mismatch".into(),
        });
    }
    if report
        .violations
        .iter()
        .any(|v| v.rule_type == "invalid_outline")
    {
        return Err(invalid("native DRC rejected final board outline"));
    }
    if bounded_file(&snapshot, MAX_INPUT)? != board_bytes
        || bounded_file(&snapshot.with_extension("kicad_pro"), MAX_INPUT)? != project_bytes
    {
        return Err(invalid("private snapshot changed during verification"));
    }
    if bounded_file(&board, MAX_INPUT)? != board_bytes
        || bounded_file(&project, MAX_INPUT)? != project_bytes
    {
        return Err(invalid("saved outline input changed during verification"));
    }
    Ok(OutlineTopology {
        native,
        topology_drc: report,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn native_proof_binds_exact_inputs_and_strict_bounded_geometry() {
        let native = NativeTopology {
            kicad_version: "10.0.7".into(),
            board_sha256: digest(b"board"),
            project_sha256: digest(b"project"),
            edge_count: 4,
            outer_count: 1,
            holes: vec![0],
            filled_areas_nm2: vec![100.0],
            bounds_nm: [0, 0, 10, 10],
            point_count: 4,
            chaining_epsilon_nm: 1,
            exact_endpoint_chains: true,
            inferred_outline: false,
        };
        assert!(validate_report(native.clone(), b"board", b"project").is_ok());
        assert!(validate_report(native.clone(), b"other", b"project").is_err());
        assert!(validate_report(native.clone(), b"board", b"other").is_err());
        let mut variants = Vec::new();
        let mut v = native.clone();
        v.inferred_outline = true;
        variants.push(v);
        let mut v = native.clone();
        v.chaining_epsilon_nm = 2;
        variants.push(v);
        let mut v = native.clone();
        v.filled_areas_nm2 = vec![0.0];
        variants.push(v);
        let mut v = native.clone();
        v.filled_areas_nm2 = vec![f64::NAN];
        variants.push(v);
        let mut v = native.clone();
        v.bounds_nm = [i64::MIN, 0, i64::MAX, 10];
        variants.push(v);
        let mut v = native.clone();
        v.bounds_nm = [0, 0, 1_000_000_001, 10];
        variants.push(v);
        let mut v = native.clone();
        v.holes = vec![usize::MAX];
        variants.push(v);
        let mut v = native.clone();
        v.kicad_version = "11".into();
        variants.push(v);
        for value in variants {
            assert!(validate_report(value, b"board", b"project").is_err());
        }
    }
    #[test]
    fn disabled_topology_rules_and_nonregular_inputs_never_launch_worker() {
        let work = tempfile::tempdir().unwrap();
        let board = work.path().join("board.kicad_pcb");
        fs::write(&board, "board").unwrap();
        fs::write(board.with_extension("kicad_pro"), "{}").unwrap();
        let policy = DrcPolicy {
            allowed_root: work.path().into(),
            timeout: Duration::from_secs(1),
        };
        assert!(matches!(
            run(&board, &policy, Path::new("missing-worker")),
            Err(CheckFailure::InvalidInput { .. })
        ));
        assert!(bounded_file(work.path(), MAX_INPUT).is_err());
        assert!(bounded_file(&board, 2).is_err());
        #[cfg(unix)]
        {
            let link = work.path().join("link");
            std::os::unix::fs::symlink(&board, &link).unwrap();
            assert!(bounded_file(&link, MAX_INPUT).is_err());
        }
    }
    #[cfg(unix)]
    #[test]
    fn private_worker_is_bounded_reaped_and_cleaned_on_failure() {
        use std::os::unix::fs::PermissionsExt;
        let work = tempfile::tempdir().unwrap();
        let board = work.path().join("board.kicad_pcb");
        let project = board.with_extension("kicad_pro");
        fs::write(&board, "board").unwrap();
        let project_text =
            br#"{"board":{"design_settings":{"rule_severities":{"invalid_outline":"error"}}}}"#;
        fs::write(&project, project_text).unwrap();
        let executable = work.path().join("fake-python");
        for (body, timeout) in [
            ("exec sleep 5", true),
            ("printf '{}'; exit 0", false),
            ("exit 2", false),
        ] {
            // Only the intentional timeout case uses a short deadline. Normal process startup
            // must not be mistaken for a timeout under concurrent build or desktop load.
            let policy = DrcPolicy {
                allowed_root: work.path().into(),
                timeout: if timeout {
                    Duration::from_millis(100)
                } else {
                    Duration::from_secs(5)
                },
            };
            fs::write(&executable, format!("#!/bin/sh\n{body}\n")).unwrap();
            fs::set_permissions(&executable, fs::Permissions::from_mode(0o755)).unwrap();
            let failure = run(&board, &policy, &executable).unwrap_err();
            assert_eq!(matches!(failure, CheckFailure::Timeout { .. }), timeout);
            assert!(fs::read_dir(work.path()).unwrap().all(|e| {
                !e.unwrap()
                    .file_name()
                    .to_string_lossy()
                    .starts_with(".auto-kicad-outline-")
            }));
            assert_eq!(fs::read(&board).unwrap(), b"board");
            assert_eq!(fs::read(&project).unwrap(), project_text);
        }
    }
}
