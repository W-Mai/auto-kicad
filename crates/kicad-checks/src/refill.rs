//! Independently refill a private bounded saved-board snapshot without replacing the source.
use crate::{
    CheckFailure, DrcPolicy, DrcReport, confined_board_path, read_bounded_file,
    terminate_child_tree,
};
use serde::Serialize;
use sha2::{Digest, Sha256};
#[cfg(unix)]
use std::os::unix::{fs::OpenOptionsExt, process::CommandExt};
use std::{
    fs::{self, File, OpenOptions},
    io::{self, Read},
    path::{Path, PathBuf},
    process::{Command, Stdio},
    time::{Duration, Instant},
};
use wait_timeout::ChildExt;

const LIMIT: u64 = 32 * 1024 * 1024;
fn invalid(reason: impl ToString) -> CheckFailure {
    CheckFailure::InvalidInput {
        message: reason.to_string(),
    }
}
fn read(path: &Path) -> Result<Vec<u8>, CheckFailure> {
    let mut opts = OpenOptions::new();
    opts.read(true);
    #[cfg(unix)]
    opts.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
    let file = opts.open(path).map_err(invalid)?;
    let meta = file.metadata().map_err(invalid)?;
    if !meta.is_file() || meta.len() == 0 || meta.len() > LIMIT {
        return Err(invalid(
            "refill input must be a nonempty bounded regular file",
        ));
    }
    let mut bytes = Vec::new();
    file.take(LIMIT + 1)
        .read_to_end(&mut bytes)
        .map_err(invalid)?;
    if bytes.len() as u64 != meta.len() {
        return Err(invalid("refill input changed while reading"));
    }
    Ok(bytes)
}
fn sha(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

/// Immutable output of an actual private CLI refill. Copper and definition equivalence are separate gates.
#[derive(Clone, Debug, Serialize)]
pub struct ClosedRefillResult {
    input_sha256: String,
    output_sha256: String,
    project_sha256: String,
    rules_sha256: Option<String>,
    report: DrcReport,
    #[serde(skip)]
    input: String,
    #[serde(skip)]
    output: String,
}
impl ClosedRefillResult {
    pub fn input_board(&self) -> &str {
        &self.input
    }
    pub fn output_board(&self) -> &str {
        &self.output
    }
    pub fn input_sha256(&self) -> &str {
        &self.input_sha256
    }
    pub fn output_sha256(&self) -> &str {
        &self.output_sha256
    }
    pub fn report(&self) -> &DrcReport {
        &self.report
    }
}

/// Run fixed refill/save/DRC on an owned private PCB/project/rules copy. Library-context DRC acceptance and source approval remain caller responsibilities.
pub fn refill_closed_copy(
    board: &Path,
    policy: &DrcPolicy,
) -> Result<ClosedRefillResult, CheckFailure> {
    run(board, policy, Path::new("kicad-cli"))
}
fn run(
    board: &Path,
    policy: &DrcPolicy,
    executable: &Path,
) -> Result<ClosedRefillResult, CheckFailure> {
    let deadline = Instant::now() + policy.timeout.min(Duration::from_secs(60));
    if policy.timeout.is_zero() {
        return Err(invalid("refill deadline is empty"));
    }
    let board = confined_board_path(board, &policy.allowed_root)?;
    let input = read(&board)?;
    let project_path = board.with_extension("kicad_pro");
    let project = read(&project_path)?;
    serde_json::from_slice::<serde_json::Value>(&project).map_err(invalid)?;
    let rules_path = board.with_extension("kicad_dru");
    let rules = if rules_path.try_exists().map_err(invalid)? {
        Some(read(&rules_path)?)
    } else {
        None
    };
    let work = tempfile::Builder::new()
        .prefix(".auto-kicad-refill-")
        .tempdir_in(&policy.allowed_root)
        .map_err(invalid)?;
    let snapshot = work.path().join(
        board
            .file_name()
            .ok_or_else(|| invalid("missing PCB filename"))?,
    );
    fs::write(&snapshot, &input).map_err(invalid)?;
    fs::write(snapshot.with_extension("kicad_pro"), &project).map_err(invalid)?;
    if let Some(rules) = &rules {
        fs::write(snapshot.with_extension("kicad_dru"), rules).map_err(invalid)?;
    }
    let stdout = work.path().join("stdout.log");
    let stderr = work.path().join("stderr.log");
    let report_path = work.path().join("drc.json");
    let mut command = Command::new(executable);
    command
        .args([
            "pcb",
            "drc",
            "--refill-zones",
            "--save-board",
            "--severity-all",
            "--format",
            "json",
            "--exit-code-violations",
            "--output",
        ])
        .arg(&report_path)
        .arg(&snapshot)
        .current_dir(work.path())
        .stdin(Stdio::null())
        .stdout(Stdio::from(File::create(&stdout).map_err(invalid)?))
        .stderr(Stdio::from(File::create(&stderr).map_err(invalid)?));
    #[cfg(unix)]
    unsafe {
        command.pre_exec(|| {
            if libc::setpgid(0, 0) != 0 {
                return Err(io::Error::last_os_error());
            }
            let limit = libc::rlimit {
                rlim_cur: LIMIT,
                rlim_max: LIMIT,
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
    let status = match child.wait_timeout(deadline.saturating_duration_since(Instant::now())) {
        Ok(Some(status)) => status,
        Ok(None) => {
            terminate_child_tree(&mut child);
            return Err(CheckFailure::Timeout {
                stdout: read_bounded_file(&stdout),
                stderr: read_bounded_file(&stderr),
            });
        }
        Err(e) => {
            terminate_child_tree(&mut child);
            return Err(CheckFailure::ProcessFailed {
                status: None,
                stdout: read_bounded_file(&stdout),
                stderr: e.to_string(),
            });
        }
    };
    if !matches!(status.code(), Some(0 | 5)) {
        return Err(CheckFailure::ProcessFailed {
            status: status.code(),
            stdout: read_bounded_file(&stdout),
            stderr: read_bounded_file(&stderr),
        });
    }
    let report = DrcReport::parse(std::str::from_utf8(&read(&report_path)?).map_err(invalid)?)
        .map_err(|e| CheckFailure::MalformedReport {
            message: e.to_string(),
        })?;
    if report.kicad_version != "10.0.7"
        || PathBuf::from(&report.source).file_name() != snapshot.file_name()
    {
        return Err(CheckFailure::MalformedReport {
            message: "closed refill version or source identity differs".into(),
        });
    }
    let output = read(&snapshot)?;
    if snapshot
        .with_extension("kicad_dru")
        .try_exists()
        .map_err(invalid)?
        != rules.is_some()
        || read(&snapshot.with_extension("kicad_pro"))? != project
        || rules.as_ref().is_some_and(|expected| {
            read(&snapshot.with_extension("kicad_dru")).as_ref() != Ok(expected)
        })
        || read(&board)? != input
        || read(&project_path)? != project
        || rules_path.try_exists().map_err(invalid)? != rules.is_some()
        || rules
            .as_ref()
            .is_some_and(|expected| read(&rules_path).as_ref() != Ok(expected))
    {
        return Err(invalid(
            "refill changed source or protected project/rules bytes",
        ));
    }
    Ok(ClosedRefillResult {
        input_sha256: sha(&input),
        output_sha256: sha(&output),
        project_sha256: sha(&project),
        rules_sha256: rules.as_ref().map(|r| sha(r)),
        input: String::from_utf8(input).map_err(invalid)?,
        output: String::from_utf8(output).map_err(invalid)?,
        report,
    })
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;
    #[test]
    fn closed_refill_owns_its_outputs_and_checks_version_source_and_protected_files() {
        let work = tempfile::tempdir().unwrap();
        let board = work.path().join("board.kicad_pcb");
        fs::write(&board, "source board").unwrap();
        fs::write(board.with_extension("kicad_pro"), "{}").unwrap();
        let executable = work.path().join("fake-cli");
        let policy = DrcPolicy {
            allowed_root: work.path().into(),
            timeout: Duration::from_secs(5),
        };
        for mode in ["ok", "version", "source", "project", "rules", "bad-report"] {
            let script = format!(
                r#"#!/usr/bin/env python3
import json,sys
from pathlib import Path
board=Path(sys.argv[-1]); report=Path(sys.argv[sys.argv.index('--output')+1])
assert board.parent.name.startswith('.auto-kicad-refill-')
assert '--refill-zones' in sys.argv and '--save-board' in sys.argv
board.write_text('independently refilled')
if '{mode}'=='project': board.with_suffix('.kicad_pro').write_text('{{"changed":true}}')
if '{mode}'=='rules': board.with_suffix('.kicad_dru').write_text('(version 1)')
data={{'$schema':'https://schemas.kicad.org/drc.v1.json','kicad_version':'11' if '{mode}'=='version' else '10.0.7','source':'wrong.kicad_pcb' if '{mode}'=='source' else board.name,'violations':[]}}
report.write_text('broken' if '{mode}'=='bad-report' else json.dumps(data))
"#
            );
            fs::write(&executable, script).unwrap();
            fs::set_permissions(&executable, fs::Permissions::from_mode(0o700)).unwrap();
            let result = run(&board, &policy, &executable);
            if mode == "ok" {
                let result = result.unwrap();
                assert_eq!(result.input_board(), "source board");
                assert_eq!(result.output_board(), "independently refilled");
            } else {
                assert!(result.is_err(), "{mode}");
            }
            assert_eq!(fs::read_to_string(&board).unwrap(), "source board");
            assert_eq!(
                fs::read_to_string(board.with_extension("kicad_pro")).unwrap(),
                "{}"
            );
            assert!(fs::read_dir(work.path()).unwrap().all(|e| {
                !e.unwrap()
                    .file_name()
                    .to_string_lossy()
                    .starts_with(".auto-kicad-refill-")
            }));
        }
    }
    #[test]
    fn closed_refill_deadline_reaps_worker_and_removes_private_copy() {
        let work = tempfile::tempdir().unwrap();
        let board = work.path().join("board.kicad_pcb");
        fs::write(&board, "source").unwrap();
        fs::write(board.with_extension("kicad_pro"), "{}").unwrap();
        let executable = work.path().join("fake-cli");
        fs::write(&executable, "#!/bin/sh\nexec sleep 30\n").unwrap();
        fs::set_permissions(&executable, fs::Permissions::from_mode(0o700)).unwrap();
        let policy = DrcPolicy {
            allowed_root: work.path().into(),
            timeout: Duration::from_millis(100),
        };
        assert!(matches!(
            run(&board, &policy, &executable),
            Err(CheckFailure::Timeout { .. })
        ));
        assert!(fs::read_dir(work.path()).unwrap().all(|e| {
            !e.unwrap()
                .file_name()
                .to_string_lossy()
                .starts_with(".auto-kicad-refill-")
        }));
        let link = work.path().join("rules-link");
        std::os::unix::fs::symlink(&board, &link).unwrap();
        assert!(read(&link).is_err());
    }
}
