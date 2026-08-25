use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::fmt::{Display, Formatter};
use std::fs::{self, File};
use std::io::{self, Read};
#[cfg(unix)]
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::time::Duration;
use tempfile::Builder;
use wait_timeout::ChildExt;

const MAX_DIAGNOSTIC_BYTES: usize = 64 * 1024;

#[derive(Clone, Debug, Deserialize)]
struct RawReport {
    #[serde(rename = "$schema")]
    schema: String,
    kicad_version: String,
    source: String,
    #[serde(default)]
    ignored_checks: Vec<serde_json::Value>,
    #[serde(default)]
    unconnected_items: Vec<serde_json::Value>,
    violations: Vec<RawViolation>,
}

#[derive(Clone, Debug, Deserialize)]
struct RawViolation {
    #[serde(rename = "type")]
    rule_type: String,
    severity: String,
    description: String,
    items: Vec<RawItem>,
}

#[derive(Clone, Debug, Deserialize)]
struct RawItem {
    uuid: Option<String>,
    description: String,
    pos: Option<RawPosition>,
}

#[derive(Clone, Copy, Debug, PartialEq, Deserialize, Serialize)]
pub struct Position {
    pub x: f64,
    pub y: f64,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct DrcItem {
    pub uuid: Option<String>,
    pub description: String,
    pub position: Option<Position>,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct DrcViolation {
    pub identity: ViolationIdentity,
    pub rule_type: String,
    pub severity: String,
    pub description: String,
    pub items: Vec<DrcItem>,
}

#[derive(Clone, Debug, Eq, PartialEq, Ord, PartialOrd, Serialize)]
pub enum ViolationIdentity {
    Uuids {
        rule_type: String,
        uuids: Vec<String>,
    },
    Fallback(String),
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct DrcReport {
    pub parser_version: String,
    pub schema: String,
    pub kicad_version: String,
    pub source: String,
    pub ignored_checks: usize,
    pub unconnected_items: usize,
    pub violations: Vec<DrcViolation>,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct BaselineDelta {
    pub introduced: Vec<DrcViolation>,
    pub resolved: Vec<DrcViolation>,
    pub unchanged: Vec<DrcViolation>,
}

#[derive(Clone, Debug)]
pub struct DrcPolicy {
    pub allowed_root: PathBuf,
    pub timeout: Duration,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub enum CheckResult {
    Completed(DrcReport),
    Failed(CheckFailure),
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub enum CheckFailure {
    InvalidInput {
        message: String,
    },
    CommandUnavailable {
        message: String,
    },
    Timeout {
        stdout: String,
        stderr: String,
    },
    InputLoadFailed {
        status: Option<i32>,
        stdout: String,
        stderr: String,
    },
    ProcessFailed {
        status: Option<i32>,
        stdout: String,
        stderr: String,
    },
    ReportMissing,
    ReportWriteFailed {
        message: String,
    },
    MalformedReport {
        message: String,
    },
}

pub fn run_pcb_drc(board_path: &Path, policy: &DrcPolicy) -> CheckResult {
    run_pcb_drc_with_executable(board_path, policy, Path::new("kicad-cli"))
}

fn run_pcb_drc_with_executable(
    board_path: &Path,
    policy: &DrcPolicy,
    executable: &Path,
) -> CheckResult {
    let board_path = match confined_board_path(board_path, &policy.allowed_root) {
        Ok(path) => path,
        Err(failure) => return CheckResult::Failed(failure),
    };
    let allowed_root = match fs::canonicalize(&policy.allowed_root) {
        Ok(path) => path,
        Err(error) => {
            return CheckResult::Failed(CheckFailure::InvalidInput {
                message: format!("invalid allowed root: {error}"),
            });
        }
    };
    let report_dir = match Builder::new()
        .prefix(".auto-kicad-drc-")
        .tempdir_in(allowed_root)
    {
        Ok(directory) => directory,
        Err(error) => {
            return CheckResult::Failed(CheckFailure::ReportWriteFailed {
                message: error.to_string(),
            });
        }
    };
    let report_path = report_dir.path().join("report.json");
    let stdout_path = report_dir.path().join("stdout.log");
    let stderr_path = report_dir.path().join("stderr.log");
    let stdout_file = match File::create(&stdout_path) {
        Ok(file) => file,
        Err(error) => {
            return CheckResult::Failed(CheckFailure::ReportWriteFailed {
                message: error.to_string(),
            });
        }
    };
    let stderr_file = match File::create(&stderr_path) {
        Ok(file) => file,
        Err(error) => {
            return CheckResult::Failed(CheckFailure::ReportWriteFailed {
                message: error.to_string(),
            });
        }
    };
    let mut command = Command::new(executable);
    command
        .arg("pcb")
        .arg("drc")
        .arg("--format")
        .arg("json")
        .arg("--exit-code-violations")
        .arg("--output")
        .arg(&report_path)
        .arg(&board_path)
        .stdout(Stdio::from(stdout_file))
        .stderr(Stdio::from(stderr_file));
    #[cfg(unix)]
    unsafe {
        command.pre_exec(|| {
            if libc::setpgid(0, 0) == 0 {
                Ok(())
            } else {
                Err(io::Error::last_os_error())
            }
        });
    }

    let mut child = match command.spawn() {
        Ok(child) => child,
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            return CheckResult::Failed(CheckFailure::CommandUnavailable {
                message: error.to_string(),
            });
        }
        Err(error) => {
            return CheckResult::Failed(CheckFailure::ProcessFailed {
                status: None,
                stdout: String::new(),
                stderr: error.to_string(),
            });
        }
    };
    let status = match child.wait_timeout(policy.timeout) {
        Ok(Some(status)) => status,
        Ok(None) => {
            terminate_child_tree(&mut child);
            let stdout = read_bounded_file(&stdout_path);
            let stderr = read_bounded_file(&stderr_path);
            return CheckResult::Failed(CheckFailure::Timeout { stdout, stderr });
        }
        Err(error) => {
            terminate_child_tree(&mut child);
            return CheckResult::Failed(CheckFailure::ProcessFailed {
                status: None,
                stdout: read_bounded_file(&stdout_path),
                stderr: format!("{error}; {}", read_bounded_file(&stderr_path)),
            });
        }
    };
    let stdout = read_bounded_file(&stdout_path);
    let stderr = read_bounded_file(&stderr_path);
    if !matches!(status.code(), Some(0 | 5)) {
        return CheckResult::Failed(classify_process_failure(status, stdout, stderr));
    }
    let report = match fs::read_to_string(&report_path) {
        Ok(report) => report,
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            return CheckResult::Failed(CheckFailure::ReportMissing);
        }
        Err(error) => {
            return CheckResult::Failed(CheckFailure::ReportWriteFailed {
                message: error.to_string(),
            });
        }
    };
    match DrcReport::parse(&report) {
        Ok(report) => CheckResult::Completed(report),
        Err(error) => CheckResult::Failed(CheckFailure::MalformedReport {
            message: error.to_string(),
        }),
    }
}

fn confined_board_path(board_path: &Path, allowed_root: &Path) -> Result<PathBuf, CheckFailure> {
    let root = fs::canonicalize(allowed_root).map_err(|error| CheckFailure::InvalidInput {
        message: format!("invalid allowed root: {error}"),
    })?;
    let board = fs::canonicalize(board_path).map_err(|error| CheckFailure::InvalidInput {
        message: format!("invalid board path: {error}"),
    })?;
    if !board.starts_with(&root) {
        return Err(CheckFailure::InvalidInput {
            message: "board path escapes allowed root".into(),
        });
    }
    let metadata = fs::metadata(&board).map_err(|error| CheckFailure::InvalidInput {
        message: format!("invalid board path: {error}"),
    })?;
    if !metadata.is_file()
        || board.extension().and_then(|value| value.to_str()) != Some("kicad_pcb")
    {
        return Err(CheckFailure::InvalidInput {
            message: "board path must be an existing .kicad_pcb file".into(),
        });
    }
    Ok(board)
}

fn terminate_child_tree(child: &mut Child) {
    #[cfg(unix)]
    unsafe {
        libc::kill(-(child.id() as libc::pid_t), libc::SIGKILL);
    }
    #[cfg(not(unix))]
    let _ = child.kill();
    let _ = child.wait();
}

fn read_bounded_file(path: &Path) -> String {
    let mut retained = Vec::with_capacity(MAX_DIAGNOSTIC_BYTES);
    if let Ok(file) = File::open(path) {
        let _ = file
            .take(MAX_DIAGNOSTIC_BYTES as u64)
            .read_to_end(&mut retained);
    }
    String::from_utf8_lossy(&retained).into_owned()
}

fn classify_process_failure(status: ExitStatus, stdout: String, stderr: String) -> CheckFailure {
    if status.code() == Some(3) {
        CheckFailure::InputLoadFailed {
            status: status.code(),
            stdout,
            stderr,
        }
    } else {
        CheckFailure::ProcessFailed {
            status: status.code(),
            stdout,
            stderr,
        }
    }
}

#[derive(Debug)]
pub enum ParseError {
    Json(String),
    Schema(String),
}

impl Display for ParseError {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Json(message) => write!(f, "invalid DRC JSON: {message}"),
            Self::Schema(message) => write!(f, "invalid DRC schema: {message}"),
        }
    }
}

impl std::error::Error for ParseError {}

impl DrcReport {
    pub fn parse(input: &str) -> Result<Self, ParseError> {
        let raw: RawReport =
            serde_json::from_str(input).map_err(|error| ParseError::Json(error.to_string()))?;
        if raw.schema != "https://schemas.kicad.org/drc.v1.json" {
            return Err(ParseError::Schema("unsupported schema".into()));
        }
        if raw.kicad_version.trim().is_empty() || raw.source.trim().is_empty() {
            return Err(ParseError::Schema("version and source are required".into()));
        }
        let mut violations = raw
            .violations
            .into_iter()
            .map(normalize_violation)
            .collect::<Result<Vec<_>, _>>()?;
        violations.sort_by(violation_order);
        Ok(Self {
            parser_version: "kicad-drc-v1".into(),
            schema: raw.schema,
            kicad_version: raw.kicad_version,
            source: raw.source,
            ignored_checks: raw.ignored_checks.len(),
            unconnected_items: raw.unconnected_items.len(),
            violations,
        })
    }
}

pub fn compare_drc(baseline: &DrcReport, after: &DrcReport) -> BaselineDelta {
    let mut old = index(&baseline.violations);
    let mut introduced = Vec::new();
    let mut unchanged = Vec::new();
    for violation in &after.violations {
        if let Some(previous) = take_match(&mut old, violation) {
            unchanged.push(previous);
        } else {
            introduced.push(violation.clone());
        }
    }
    let mut resolved = old.into_values().flatten().collect::<Vec<_>>();
    introduced.sort_by(|a, b| a.identity.cmp(&b.identity));
    unchanged.sort_by(|a, b| a.identity.cmp(&b.identity));
    resolved.sort_by(|a, b| a.identity.cmp(&b.identity));
    BaselineDelta {
        introduced,
        resolved,
        unchanged,
    }
}

fn violation_order(left: &DrcViolation, right: &DrcViolation) -> std::cmp::Ordering {
    (
        &left.rule_type,
        &left.severity,
        &left.description,
        &left.identity,
    )
        .cmp(&(
            &right.rule_type,
            &right.severity,
            &right.description,
            &right.identity,
        ))
}

fn index(violations: &[DrcViolation]) -> BTreeMap<ViolationIdentity, Vec<DrcViolation>> {
    let mut result = BTreeMap::new();
    for violation in violations {
        result
            .entry(violation.identity.clone())
            .or_insert_with(Vec::new)
            .push(violation.clone());
    }
    result
}

fn take_match(
    index: &mut BTreeMap<ViolationIdentity, Vec<DrcViolation>>,
    after: &DrcViolation,
) -> Option<DrcViolation> {
    if let Some(values) = index.get_mut(&after.identity) {
        let result = values.pop();
        if values.is_empty() {
            index.remove(&after.identity);
        }
        return result;
    }
    let fallback = fallback_identity(after);
    let (key, candidate_index) = index.iter().find_map(|(key, values)| {
        values
            .iter()
            .position(|candidate| fallback_identity(candidate) == fallback)
            .map(|candidate_index| (key.clone(), candidate_index))
    })?;
    let values = index.get_mut(&key).expect("key found");
    let result = Some(values.remove(candidate_index));
    if values.is_empty() {
        index.remove(&key);
    }
    result
}

fn normalize_violation(raw: RawViolation) -> Result<DrcViolation, ParseError> {
    if raw.rule_type.trim().is_empty()
        || raw.severity.trim().is_empty()
        || raw.description.trim().is_empty()
    {
        return Err(ParseError::Schema(
            "violation type, severity and description are required".into(),
        ));
    }
    let mut items = raw
        .items
        .into_iter()
        .map(|item| DrcItem {
            uuid: item.uuid.filter(|uuid| !uuid.trim().is_empty()),
            description: item.description.trim().to_owned(),
            position: item.pos.map(|pos| Position { x: pos.x, y: pos.y }),
        })
        .collect::<Vec<_>>();
    if items.iter().any(|item| item.description.is_empty()) {
        return Err(ParseError::Schema("item description is required".into()));
    }
    items.sort_by_key(item_key);
    let identity = if items.iter().all(|item| item.uuid.is_some()) && !items.is_empty() {
        ViolationIdentity::Uuids {
            rule_type: raw.rule_type.clone(),
            uuids: items
                .iter()
                .map(|item| item.uuid.clone().expect("checked"))
                .collect(),
        }
    } else {
        ViolationIdentity::Fallback(fallback_parts(
            &raw.rule_type,
            &raw.severity,
            &raw.description,
            &items,
        ))
    };
    Ok(DrcViolation {
        identity,
        rule_type: raw.rule_type,
        severity: raw.severity,
        description: raw.description.trim().to_owned(),
        items,
    })
}

fn fallback_identity(violation: &DrcViolation) -> String {
    fallback_parts(
        &violation.rule_type,
        &violation.severity,
        &violation.description,
        &violation.items,
    )
}

fn fallback_parts(rule_type: &str, severity: &str, description: &str, items: &[DrcItem]) -> String {
    let mut value = format!(
        "{}|{}|{}|",
        clean(rule_type),
        clean(severity),
        clean(description)
    );
    for item in items {
        value.push_str(&item_key(item));
        value.push('|');
    }
    hex::encode(Sha256::digest(value.as_bytes()))
}

fn item_key(item: &DrcItem) -> String {
    let position = item
        .position
        .map(|pos| format!("{}:{}", quantize(pos.x), quantize(pos.y)))
        .unwrap_or_default();
    format!("{}|{}", clean(&item.description), position)
}

fn clean(value: &str) -> String {
    value
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase()
}
fn quantize(value: f64) -> i64 {
    (value * 1000.0).round() as i64
}

#[derive(Clone, Copy, Debug, Deserialize)]
struct RawPosition {
    x: f64,
    y: f64,
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(unix)]
    use std::os::unix::fs::PermissionsExt;

    fn report(violations: &str) -> DrcReport {
        DrcReport::parse(&format!(r#"{{"$schema":"https://schemas.kicad.org/drc.v1.json","kicad_version":"10.0.5","source":"fixture.kicad_pcb","ignored_checks":[],"unconnected_items":[],"violations":[{violations}]}}"#)).unwrap()
    }

    #[test]
    fn parses_and_canonicalizes_order() {
        let parsed = report(
            r#"{"type":"b","severity":"error","description":" B ","items":[{"description":"z"}]},{"type":"a","severity":"warning","description":"A","items":[{"description":"a"}]}"#,
        );
        assert_eq!(parsed.violations[0].rule_type, "a");
        assert_eq!(parsed.violations[1].description, "B");
    }

    #[test]
    fn rejects_wrong_schema_and_malformed_json() {
        assert!(matches!(DrcReport::parse("{}"), Err(ParseError::Json(_))));
        let value = r#"{"$schema":"wrong","kicad_version":"10","source":"x","violations":[],"ignored_checks":[],"unconnected_items":[]}"#;
        assert!(matches!(
            DrcReport::parse(value),
            Err(ParseError::Schema(_))
        ));
    }

    #[test]
    fn matches_unstable_uuid_by_fallback() {
        let old = report(
            r#"{"type":"invalid_outline","severity":"error","description":"bad outline","items":[{"uuid":"old","description":"Edge.Cuts","pos":{"x":1.0,"y":2.0}}]}"#,
        );
        let new = report(
            r#"{"type":"invalid_outline","severity":"error","description":"bad outline","items":[{"uuid":"new","description":"Edge.Cuts","pos":{"x":1.0,"y":2.0}}]}"#,
        );
        let delta = compare_drc(&old, &new);
        assert!(delta.introduced.is_empty());
        assert!(delta.resolved.is_empty());
        assert_eq!(delta.unchanged.len(), 1);
    }

    #[test]
    fn reports_introduced_resolved_and_unchanged() {
        let old = report(
            r#"{"type":"old","severity":"error","description":"old","items":[{"uuid":"1","description":"pad"}]},{"type":"same","severity":"error","description":"same","items":[{"uuid":"2","description":"pad"}]}"#,
        );
        let new = report(
            r#"{"type":"same","severity":"error","description":"same","items":[{"uuid":"2","description":"pad"}]},{"type":"new","severity":"warning","description":"new","items":[{"uuid":"3","description":"track"}]}"#,
        );
        let delta = compare_drc(&old, &new);
        assert_eq!(delta.introduced.len(), 1);
        assert_eq!(delta.resolved.len(), 1);
        assert_eq!(delta.unchanged.len(), 1);
    }

    #[test]
    fn does_not_merge_distinct_fallback_items() {
        let old = report(
            r#"{"type":"rule","severity":"error","description":"bad","items":[{"description":"pad","pos":{"x":1.0,"y":2.0}}]}"#,
        );
        let new = report(
            r#"{"type":"rule","severity":"error","description":"bad","items":[{"description":"pad","pos":{"x":1.1,"y":2.0}}]}"#,
        );
        let delta = compare_drc(&old, &new);
        assert_eq!(delta.introduced.len(), 1);
        assert_eq!(delta.resolved.len(), 1);
    }

    #[test]
    fn fallback_removes_the_exact_candidate_from_a_shared_uuid_bucket() {
        let old = report(
            r#"{"type":"rule","severity":"error","description":"first","items":[{"uuid":"shared","description":"pad","pos":{"x":1.0,"y":2.0}}]},{"type":"rule","severity":"error","description":"second","items":[{"uuid":"shared","description":"track","pos":{"x":3.0,"y":4.0}}]}"#,
        );
        let new = report(
            r#"{"type":"rule","severity":"error","description":"first","items":[{"uuid":"changed","description":"pad","pos":{"x":1.0,"y":2.0}}]}"#,
        );
        let delta = compare_drc(&old, &new);
        assert_eq!(delta.unchanged[0].description, "first");
        assert_eq!(delta.resolved[0].description, "second");
    }

    #[cfg(unix)]
    fn executable(root: &Path, body: &str) -> PathBuf {
        let path = root.join("fake-kicad-cli");
        fs::write(&path, format!("#!/bin/sh\nset -eu\n{body}\n")).unwrap();
        let mut permissions = fs::metadata(&path).unwrap().permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(&path, permissions).unwrap();
        path
    }

    #[cfg(unix)]
    fn runner_fixture() -> (tempfile::TempDir, PathBuf, DrcPolicy) {
        let root = tempfile::tempdir().unwrap();
        let board = root.path().join("board.kicad_pcb");
        fs::write(&board, "(kicad_pcb)").unwrap();
        let policy = DrcPolicy {
            allowed_root: root.path().to_owned(),
            timeout: Duration::from_secs(2),
        };
        (root, board, policy)
    }

    #[cfg(unix)]
    fn assert_no_report_directories(root: &Path) {
        assert!(!fs::read_dir(root).unwrap().any(|entry| {
            entry
                .unwrap()
                .file_name()
                .to_string_lossy()
                .starts_with(".auto-kicad-drc-")
        }));
    }

    #[cfg(unix)]
    #[test]
    fn runner_uses_fixed_arguments_and_accepts_violation_exit() {
        let (root, board, policy) = runner_fixture();
        let argv = root.path().join("argv");
        let executable = executable(
            root.path(),
            &format!(
                r#"printf '%s\n' "$@" > '{}'
report=''
while [ "$#" -gt 0 ]; do
  if [ "$1" = '--output' ]; then report="$2"; break; fi
  shift
done
printf '%s' '{{"$schema":"https://schemas.kicad.org/drc.v1.json","kicad_version":"10.0.5","source":"board.kicad_pcb","ignored_checks":[],"unconnected_items":[],"violations":[]}}' > "$report"
exit 5"#,
                argv.display()
            ),
        );
        let result = run_pcb_drc_with_executable(&board, &policy, &executable);
        assert!(matches!(result, CheckResult::Completed(_)));
        let args = fs::read_to_string(argv).unwrap();
        let lines = args.lines().collect::<Vec<_>>();
        assert_eq!(
            &lines[..6],
            [
                "pcb",
                "drc",
                "--format",
                "json",
                "--exit-code-violations",
                "--output"
            ]
        );
        assert!(lines[6].ends_with("/report.json"));
        assert_eq!(Path::new(lines[7]), fs::canonicalize(&board).unwrap());
        assert_no_report_directories(root.path());
    }

    #[cfg(unix)]
    #[test]
    fn runner_rejects_symlink_escape() {
        let allowed = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let board = outside.path().join("outside.kicad_pcb");
        fs::write(&board, "(kicad_pcb)").unwrap();
        let link = allowed.path().join("linked.kicad_pcb");
        std::os::unix::fs::symlink(&board, &link).unwrap();
        let result = run_pcb_drc_with_executable(
            &link,
            &DrcPolicy {
                allowed_root: allowed.path().to_owned(),
                timeout: Duration::from_secs(1),
            },
            Path::new("unused"),
        );
        assert!(matches!(
            result,
            CheckResult::Failed(CheckFailure::InvalidInput { .. })
        ));
    }

    #[cfg(unix)]
    #[test]
    fn runner_reports_missing_executable() {
        let (root, board, policy) = runner_fixture();
        let result = run_pcb_drc_with_executable(&board, &policy, &root.path().join("missing"));
        assert!(matches!(
            result,
            CheckResult::Failed(CheckFailure::CommandUnavailable { .. })
        ));
        assert_no_report_directories(root.path());
    }

    #[cfg(unix)]
    fn assert_process_gone(pid: &str) {
        let deadline = std::time::Instant::now() + Duration::from_secs(1);
        loop {
            let status = Command::new("kill")
                .args(["-0", pid])
                .stderr(Stdio::null())
                .status()
                .unwrap();
            if !status.success() {
                return;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "process {pid} survived"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    #[cfg(unix)]
    #[test]
    fn runner_kills_and_reaps_on_timeout() {
        let (root, board, mut policy) = runner_fixture();
        policy.timeout = Duration::from_millis(50);
        let pid_file = root.path().join("pid");
        let descendant_pid_file = root.path().join("descendant-pid");
        let executable = executable(
            root.path(),
            &format!(
                "printf '%s' $$ > '{}'; sleep 10 & printf '%s' $! > '{}'; wait",
                pid_file.display(),
                descendant_pid_file.display()
            ),
        );
        let started = std::time::Instant::now();
        let result = run_pcb_drc_with_executable(&board, &policy, &executable);
        assert!(started.elapsed() < Duration::from_secs(1));
        assert!(matches!(
            result,
            CheckResult::Failed(CheckFailure::Timeout { .. })
        ));
        let pid = fs::read_to_string(pid_file).unwrap();
        let descendant_pid = fs::read_to_string(descendant_pid_file).unwrap();
        assert_process_gone(pid.trim());
        assert_process_gone(descendant_pid.trim());
        assert_no_report_directories(root.path());
    }

    #[cfg(unix)]
    #[test]
    fn runner_classifies_missing_malformed_and_process_reports() {
        let (root, board, policy) = runner_fixture();
        let missing = executable(root.path(), "exit 0");
        assert!(matches!(
            run_pcb_drc_with_executable(&board, &policy, &missing),
            CheckResult::Failed(CheckFailure::ReportMissing)
        ));

        let malformed = executable(
            root.path(),
            "eval \"report=\\${7}\"; printf '%s' nope > \"$report\"; exit 0",
        );
        assert!(matches!(
            run_pcb_drc_with_executable(&board, &policy, &malformed),
            CheckResult::Failed(CheckFailure::MalformedReport { .. })
        ));

        let failed = executable(root.path(), "printf '%s' boom >&2; exit 9");
        assert!(matches!(
            run_pcb_drc_with_executable(&board, &policy, &failed),
            CheckResult::Failed(CheckFailure::ProcessFailed {
                status: Some(9),
                ..
            })
        ));

        let load_failed = executable(root.path(), "printf '%s' '无法加载电路板' >&2; exit 3");
        match run_pcb_drc_with_executable(&board, &policy, &load_failed) {
            CheckResult::Failed(CheckFailure::InputLoadFailed {
                status: Some(3),
                stderr,
                ..
            }) => assert_eq!(stderr, "无法加载电路板"),
            result => panic!("unexpected result: {result:?}"),
        }

        let verbose = executable(root.path(), "yes x | head -c 70000; exit 9");
        match run_pcb_drc_with_executable(&board, &policy, &verbose) {
            CheckResult::Failed(CheckFailure::ProcessFailed { stdout, .. }) => {
                assert_eq!(stdout.len(), MAX_DIAGNOSTIC_BYTES);
            }
            result => panic!("unexpected result: {result:?}"),
        }
        assert_no_report_directories(root.path());
    }
}
