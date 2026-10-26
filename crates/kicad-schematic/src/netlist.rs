use std::fs::{self, File};
use std::io::{self, BufReader, Read};
#[cfg(unix)]
use std::os::unix::{fs::PermissionsExt, process::CommandExt};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use quick_xml::Reader;
use quick_xml::events::{BytesStart, Event};
use tempfile::Builder;
use thiserror::Error;

const DEFAULT_MAX_STDOUT_BYTES: u64 = 32 * 1024 * 1024;
const DEFAULT_MAX_STDERR_BYTES: u64 = 1024 * 1024;
const DEFAULT_MAX_FIELD_BYTES: usize = 64 * 1024;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct NetlistLimits {
    pub max_xml_bytes: u64,
    pub max_field_bytes: usize,
    pub max_sheets: usize,
    pub max_components: usize,
    pub max_nets: usize,
    pub max_nodes: usize,
}

impl Default for NetlistLimits {
    fn default() -> Self {
        Self {
            max_xml_bytes: DEFAULT_MAX_STDOUT_BYTES,
            max_field_bytes: DEFAULT_MAX_FIELD_BYTES,
            max_sheets: 1024,
            max_components: 100_000,
            max_nets: 1_000_000,
            max_nodes: 5_000_000,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct NetlistDocument {
    pub export_version: String,
    pub sheets: Vec<NetlistSheet>,
    pub components: Vec<NetlistComponent>,
    pub nets: Vec<NetlistNet>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct NetlistSheet {
    pub number: String,
    pub name: String,
    pub uuid_path: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct NetlistComponent {
    pub reference: String,
    pub value: String,
    pub footprint: Option<String>,
    pub symbol_uuid: Option<String>,
    pub sheet_path: String,
    pub sheet_uuid_path: String,
    pub library: String,
    pub library_part: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct NetlistNet {
    pub code: String,
    pub name: String,
    pub class: Option<String>,
    pub nodes: Vec<NetlistNode>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct NetlistNode {
    pub reference: String,
    pub pin: String,
    pub pin_function: String,
    pub pin_type: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct NetlistRun {
    pub document: NetlistDocument,
    pub stderr: String,
}

#[derive(Clone, Debug)]
pub struct NetlistRunner {
    executable: PathBuf,
    timeout: Duration,
    max_stdout_bytes: u64,
    max_stderr_bytes: u64,
    limits: NetlistLimits,
}

impl NetlistRunner {
    pub fn production() -> Result<Self, NetlistRunnerError> {
        if !cfg!(unix) {
            return Err(NetlistRunnerError::UnsupportedPlatform);
        }
        let mut candidates = std::env::var_os("PATH")
            .into_iter()
            .flat_map(|path| {
                std::env::split_paths(&path)
                    .map(|directory| directory.join("kicad-cli"))
                    .collect::<Vec<_>>()
            })
            .collect::<Vec<_>>();
        candidates.extend([
            PathBuf::from("/Applications/KiCad/KiCad.app/Contents/MacOS/kicad-cli"),
            PathBuf::from("/opt/homebrew/bin/kicad-cli"),
            PathBuf::from("/usr/local/bin/kicad-cli"),
            PathBuf::from("/usr/bin/kicad-cli"),
        ]);
        let executable = candidates
            .into_iter()
            .find(|path| canonical_executable(path).is_ok())
            .ok_or(NetlistRunnerError::ExecutableNotFound)?;
        Ok(Self::from_executable(executable))
    }

    #[cfg(test)]
    pub(crate) fn new(executable: impl Into<PathBuf>) -> Self {
        Self::from_executable(executable)
    }

    fn from_executable(executable: impl Into<PathBuf>) -> Self {
        Self {
            executable: executable.into(),
            timeout: Duration::from_secs(30),
            max_stdout_bytes: DEFAULT_MAX_STDOUT_BYTES,
            max_stderr_bytes: DEFAULT_MAX_STDERR_BYTES,
            limits: NetlistLimits::default(),
        }
    }

    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    pub fn with_output_limits(mut self, max_stdout_bytes: u64, max_stderr_bytes: u64) -> Self {
        self.max_stdout_bytes = max_stdout_bytes;
        self.max_stderr_bytes = max_stderr_bytes;
        self
    }

    pub fn with_parser_limits(mut self, limits: NetlistLimits) -> Self {
        self.limits = limits;
        self
    }

    pub fn run(
        &self,
        project_root: &Path,
        root_schematic: &Path,
    ) -> Result<NetlistRun, NetlistRunnerError> {
        if !cfg!(unix) {
            return Err(NetlistRunnerError::UnsupportedPlatform);
        }
        let root = canonical_directory(project_root)?;
        let schematic = canonical_schematic(&root, root_schematic)?;
        let executable = canonical_executable(&self.executable)?;
        let output_dir = Builder::new()
            .prefix(".auto-kicad-netlist-")
            .tempdir_in(&root)
            .map_err(|error| NetlistRunnerError::TemporaryOutput(error.to_string()))?;
        let stdout_path = output_dir.path().join("stdout.xml");
        let stderr_path = output_dir.path().join("stderr.log");
        let stdout = File::create(&stdout_path)
            .map_err(|error| NetlistRunnerError::TemporaryOutput(error.to_string()))?;
        let stderr = File::create(&stderr_path)
            .map_err(|error| NetlistRunnerError::TemporaryOutput(error.to_string()))?;

        let mut command = Command::new(executable);
        command
            .arg("sch")
            .arg("export")
            .arg("netlist")
            .arg("--format")
            .arg("kicadxml")
            .arg("-o")
            .arg("/dev/stdout")
            .arg(&schematic)
            .stdout(Stdio::from(stdout))
            .stderr(Stdio::from(stderr));
        configure_process_group(&mut command);

        let mut child = command
            .spawn()
            .map_err(|error| NetlistRunnerError::Spawn(error.to_string()))?;
        let status = wait_with_output_limits(
            &mut child,
            self.timeout,
            &stdout_path,
            self.max_stdout_bytes,
            &stderr_path,
            self.max_stderr_bytes,
        )?;

        enforce_output_size(&stdout_path, self.max_stdout_bytes, true)?;
        enforce_output_size(&stderr_path, self.max_stderr_bytes, false)?;
        let stderr = read_utf8_file(&stderr_path, self.max_stderr_bytes)?;
        if !status.success() {
            return Err(NetlistRunnerError::ProcessFailed {
                status: status.code(),
                stdout: read_diagnostic(&stdout_path, self.max_stdout_bytes),
                stderr,
            });
        }
        let stdout = fs::read(&stdout_path)
            .map_err(|error| NetlistRunnerError::OutputRead(error.to_string()))?;
        let document = parse_netlist_xml(&stdout, self.limits)?;
        Ok(NetlistRun { document, stderr })
    }

    pub fn run_erc(
        &self,
        project_root: &Path,
        root_schematic: &Path,
    ) -> Result<(), NetlistRunnerError> {
        if !cfg!(unix) {
            return Err(NetlistRunnerError::UnsupportedPlatform);
        }
        let root = canonical_directory(project_root)?;
        let schematic = canonical_schematic(&root, root_schematic)?;
        let executable = canonical_executable(&self.executable)?;
        let output_dir = Builder::new()
            .prefix(".auto-kicad-erc-")
            .tempdir_in(&root)
            .map_err(|error| NetlistRunnerError::TemporaryOutput(error.to_string()))?;
        let report_path = output_dir.path().join("erc.rpt");
        let stderr_path = output_dir.path().join("stderr.log");
        let stderr = File::create(&stderr_path)
            .map_err(|error| NetlistRunnerError::TemporaryOutput(error.to_string()))?;

        let mut command = Command::new(executable);
        command
            .arg("sch")
            .arg("erc")
            .arg("--format")
            .arg("report")
            .arg("--severity-error")
            .arg("--exit-code-violations")
            .arg("-o")
            .arg(&report_path)
            .arg(&schematic)
            .stdout(Stdio::null())
            .stderr(Stdio::from(stderr));
        configure_process_group(&mut command);

        let mut child = command
            .spawn()
            .map_err(|error| NetlistRunnerError::Spawn(error.to_string()))?;
        let status = wait_with_output_limits(
            &mut child,
            self.timeout,
            &report_path,
            self.max_stdout_bytes,
            &stderr_path,
            self.max_stderr_bytes,
        )?;

        enforce_output_size(&report_path, self.max_stdout_bytes, true)?;
        enforce_output_size(&stderr_path, self.max_stderr_bytes, false)?;
        let stderr = read_utf8_file(&stderr_path, self.max_stderr_bytes)?;
        if !status.success() {
            return Err(NetlistRunnerError::ProcessFailed {
                status: status.code(),
                stdout: read_diagnostic(&report_path, self.max_stdout_bytes),
                stderr,
            });
        }
        Ok(())
    }
}

#[derive(Debug, Error)]
pub enum NetlistRunnerError {
    #[error("netlist export through /dev/stdout is unsupported on this platform")]
    UnsupportedPlatform,
    #[error("project root is not a canonical directory")]
    InvalidProjectRoot,
    #[error("root schematic must be a canonical .kicad_sch file inside the project root")]
    InvalidSchematic,
    #[error("KiCad CLI executable was not found at a verified system path")]
    ExecutableNotFound,
    #[error("KiCad CLI executable must be a canonical regular file")]
    InvalidExecutable,
    #[error("failed to create netlist output capture: {0}")]
    TemporaryOutput(String),
    #[error("failed to spawn KiCad CLI: {0}")]
    Spawn(String),
    #[error("KiCad CLI timed out")]
    Timeout { stdout: String, stderr: String },
    #[error("failed while waiting for KiCad CLI: {0}")]
    Wait(String),
    #[error("KiCad CLI stdout exceeds {limit} bytes")]
    StdoutTooLarge { limit: u64 },
    #[error("KiCad CLI stderr exceeds {limit} bytes")]
    StderrTooLarge { limit: u64 },
    #[error("KiCad CLI exited unsuccessfully with status {status:?}")]
    ProcessFailed {
        status: Option<i32>,
        stdout: String,
        stderr: String,
    },
    #[error("failed to read KiCad CLI output: {0}")]
    OutputRead(String),
    #[error(transparent)]
    Parse(#[from] NetlistParseError),
}

#[derive(Debug, Error, Eq, PartialEq)]
pub enum NetlistParseError {
    #[error("netlist XML exceeds {limit} bytes")]
    InputTooLarge { limit: u64 },
    #[error("netlist XML contains a field exceeding {limit} bytes")]
    FieldTooLarge { limit: usize },
    #[error("netlist XML contains a prohibited DTD or entity")]
    ProhibitedXmlConstruct,
    #[error("malformed netlist XML: {0}")]
    Malformed(String),
    #[error("invalid netlist schema: {0}")]
    Schema(&'static str),
    #[error("netlist exceeds maximum {kind} count {limit}")]
    CountLimit { kind: &'static str, limit: usize },
}

#[derive(Default)]
struct ComponentBuilder {
    reference: String,
    value: Option<String>,
    footprint: Option<String>,
    symbol_uuid: Option<String>,
    sheet_path: Option<String>,
    sheet_uuid_path: Option<String>,
    library: Option<String>,
    library_part: Option<String>,
}

struct NetBuilder {
    code: String,
    name: String,
    class: Option<String>,
    nodes: Vec<NetlistNode>,
}

pub fn parse_netlist_xml(
    bytes: &[u8],
    limits: NetlistLimits,
) -> Result<NetlistDocument, NetlistParseError> {
    if bytes.len() as u64 > limits.max_xml_bytes {
        return Err(NetlistParseError::InputTooLarge {
            limit: limits.max_xml_bytes,
        });
    }
    let mut reader = Reader::from_reader(BufReader::new(bytes));
    reader.config_mut().trim_text(false);
    let mut buffer = Vec::new();
    let mut stack = Vec::<Vec<u8>>::new();
    let mut export_version = None;
    let mut saw_components = false;
    let mut saw_nets = false;
    let mut sheets = Vec::new();
    let mut components = Vec::new();
    let mut nets = Vec::new();
    let mut component = None::<ComponentBuilder>;
    let mut net = None::<NetBuilder>;
    let mut node_count = 0usize;

    loop {
        match reader.read_event_into(&mut buffer) {
            Ok(Event::Start(event)) => {
                let name = event.name().as_ref().to_vec();
                handle_start(
                    &event,
                    &stack,
                    &limits,
                    &mut export_version,
                    &mut saw_components,
                    &mut saw_nets,
                    &mut sheets,
                    &mut component,
                    &mut net,
                    &mut node_count,
                )?;
                stack.push(name);
            }
            Ok(Event::Empty(event)) => {
                handle_start(
                    &event,
                    &stack,
                    &limits,
                    &mut export_version,
                    &mut saw_components,
                    &mut saw_nets,
                    &mut sheets,
                    &mut component,
                    &mut net,
                    &mut node_count,
                )?;
                handle_end(
                    event.name().as_ref(),
                    &mut component,
                    &mut net,
                    &mut components,
                    &mut nets,
                    &limits,
                )?;
            }
            Ok(Event::Text(text)) => {
                let value = text
                    .decode()
                    .map_err(|error| NetlistParseError::Malformed(error.to_string()))?;
                check_field(value.as_bytes(), limits.max_field_bytes)?;
                handle_text(&stack, value.into_owned(), &mut component);
            }
            Ok(Event::CData(text)) => {
                check_field(text.as_ref(), limits.max_field_bytes)?;
                let value = String::from_utf8(text.into_inner().into_owned())
                    .map_err(|error| NetlistParseError::Malformed(error.to_string()))?;
                handle_text(&stack, value, &mut component);
            }
            Ok(Event::End(event)) => {
                let expected = stack
                    .pop()
                    .ok_or(NetlistParseError::Schema("unexpected closing tag"))?;
                if expected.as_slice() != event.name().as_ref() {
                    return Err(NetlistParseError::Schema("mismatched closing tag"));
                }
                handle_end(
                    event.name().as_ref(),
                    &mut component,
                    &mut net,
                    &mut components,
                    &mut nets,
                    &limits,
                )?;
            }
            Ok(Event::DocType(_) | Event::GeneralRef(_)) => {
                return Err(NetlistParseError::ProhibitedXmlConstruct);
            }
            Ok(Event::Eof) => break,
            Ok(Event::Decl(_) | Event::Comment(_) | Event::PI(_)) => {}
            Err(error) => return Err(NetlistParseError::Malformed(error.to_string())),
        }
        buffer.clear();
    }

    if !stack.is_empty() {
        return Err(NetlistParseError::Schema("unclosed tag"));
    }
    if !saw_components {
        return Err(NetlistParseError::Schema("missing components section"));
    }
    if !saw_nets {
        return Err(NetlistParseError::Schema("missing nets section"));
    }
    sheets.sort_by(|left, right| {
        (&left.uuid_path, &left.name, &left.number).cmp(&(
            &right.uuid_path,
            &right.name,
            &right.number,
        ))
    });
    components.sort_by(|left, right| component_order(left).cmp(&component_order(right)));
    for item in &mut nets {
        item.nodes
            .sort_by(|left, right| node_order(left).cmp(&node_order(right)));
    }
    nets.sort_by(|left, right| (&left.code, &left.name).cmp(&(&right.code, &right.name)));
    Ok(NetlistDocument {
        export_version: export_version.ok_or(NetlistParseError::Schema("missing export root"))?,
        sheets,
        components,
        nets,
    })
}

fn canonical_directory(path: &Path) -> Result<PathBuf, NetlistRunnerError> {
    let canonical = fs::canonicalize(path).map_err(|_| NetlistRunnerError::InvalidProjectRoot)?;
    if canonical != path || !canonical.is_dir() {
        return Err(NetlistRunnerError::InvalidProjectRoot);
    }
    Ok(canonical)
}

fn canonical_schematic(root: &Path, path: &Path) -> Result<PathBuf, NetlistRunnerError> {
    let canonical = fs::canonicalize(path).map_err(|_| NetlistRunnerError::InvalidSchematic)?;
    let metadata = fs::metadata(&canonical).map_err(|_| NetlistRunnerError::InvalidSchematic)?;
    if canonical != path
        || !canonical.starts_with(root)
        || !metadata.is_file()
        || canonical.extension().and_then(|value| value.to_str()) != Some("kicad_sch")
    {
        return Err(NetlistRunnerError::InvalidSchematic);
    }
    Ok(canonical)
}

fn canonical_executable(path: &Path) -> Result<PathBuf, NetlistRunnerError> {
    let canonical = fs::canonicalize(path).map_err(|_| NetlistRunnerError::InvalidExecutable)?;
    let metadata = fs::metadata(&canonical).map_err(|_| NetlistRunnerError::InvalidExecutable)?;
    if !metadata.is_file() {
        return Err(NetlistRunnerError::InvalidExecutable);
    }
    #[cfg(unix)]
    if metadata.permissions().mode() & 0o111 == 0 {
        return Err(NetlistRunnerError::InvalidExecutable);
    }
    Ok(canonical)
}

fn configure_process_group(command: &mut Command) {
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
}

fn wait_with_output_limits(
    child: &mut Child,
    timeout: Duration,
    stdout_path: &Path,
    max_stdout_bytes: u64,
    stderr_path: &Path,
    max_stderr_bytes: u64,
) -> Result<std::process::ExitStatus, NetlistRunnerError> {
    let deadline = Instant::now() + timeout;
    loop {
        if let Some(status) = child
            .try_wait()
            .map_err(|error| NetlistRunnerError::Wait(error.to_string()))?
        {
            return Ok(status);
        }
        if stdout_path.exists()
            && let Err(error) = enforce_output_size(stdout_path, max_stdout_bytes, true)
        {
            terminate_child_tree(child);
            return Err(error);
        }
        if stderr_path.exists()
            && let Err(error) = enforce_output_size(stderr_path, max_stderr_bytes, false)
        {
            terminate_child_tree(child);
            return Err(error);
        }
        if Instant::now() >= deadline {
            terminate_child_tree(child);
            return Err(NetlistRunnerError::Timeout {
                stdout: read_diagnostic(stdout_path, max_stdout_bytes),
                stderr: read_diagnostic(stderr_path, max_stderr_bytes),
            });
        }
        std::thread::sleep(Duration::from_millis(10));
    }
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

fn enforce_output_size(path: &Path, limit: u64, stdout: bool) -> Result<(), NetlistRunnerError> {
    let length = fs::metadata(path)
        .map_err(|error| NetlistRunnerError::OutputRead(error.to_string()))?
        .len();
    if length > limit {
        return if stdout {
            Err(NetlistRunnerError::StdoutTooLarge { limit })
        } else {
            Err(NetlistRunnerError::StderrTooLarge { limit })
        };
    }
    Ok(())
}

fn read_utf8_file(path: &Path, limit: u64) -> Result<String, NetlistRunnerError> {
    let mut bytes = Vec::new();
    File::open(path)
        .and_then(|file| file.take(limit.saturating_add(1)).read_to_end(&mut bytes))
        .map_err(|error| NetlistRunnerError::OutputRead(error.to_string()))?;
    String::from_utf8(bytes).map_err(|error| NetlistRunnerError::OutputRead(error.to_string()))
}

fn read_diagnostic(path: &Path, limit: u64) -> String {
    let mut bytes = Vec::new();
    if let Ok(file) = File::open(path) {
        let _ = file.take(limit).read_to_end(&mut bytes);
    }
    String::from_utf8_lossy(&bytes).into_owned()
}

#[allow(clippy::too_many_arguments)]
fn handle_start(
    event: &BytesStart<'_>,
    stack: &[Vec<u8>],
    limits: &NetlistLimits,
    export_version: &mut Option<String>,
    saw_components: &mut bool,
    saw_nets: &mut bool,
    sheets: &mut Vec<NetlistSheet>,
    component: &mut Option<ComponentBuilder>,
    net: &mut Option<NetBuilder>,
    node_count: &mut usize,
) -> Result<(), NetlistParseError> {
    let name = event.name();
    let name = name.as_ref();
    match name {
        b"export" if stack.is_empty() => {
            if export_version.is_some() {
                return Err(NetlistParseError::Schema("duplicate export root"));
            }
            *export_version = Some(required_attribute(event, b"version", limits)?);
        }
        b"components" if parent(stack) == Some(b"export") => {
            if *saw_components {
                return Err(NetlistParseError::Schema("duplicate components section"));
            }
            *saw_components = true;
        }
        b"nets" if parent(stack) == Some(b"export") => {
            if *saw_nets {
                return Err(NetlistParseError::Schema("duplicate nets section"));
            }
            *saw_nets = true;
        }
        b"sheet" if parent(stack) == Some(b"design") => {
            if sheets.len() >= limits.max_sheets {
                return Err(NetlistParseError::CountLimit {
                    kind: "sheet",
                    limit: limits.max_sheets,
                });
            }
            sheets.push(NetlistSheet {
                number: required_attribute(event, b"number", limits)?,
                name: required_attribute(event, b"name", limits)?,
                uuid_path: required_attribute(event, b"tstamps", limits)?,
            });
        }
        b"comp" if parent(stack) == Some(b"components") => {
            if component.is_some() {
                return Err(NetlistParseError::Schema("nested component"));
            }
            *component = Some(ComponentBuilder {
                reference: required_attribute(event, b"ref", limits)?,
                ..ComponentBuilder::default()
            });
        }
        b"footprint" if component.is_some() => {
            component.as_mut().expect("checked above").footprint = Some(String::new());
        }
        b"sheetpath" if component.is_some() => {
            let builder = component.as_mut().expect("checked above");
            builder.sheet_path = Some(required_attribute(event, b"names", limits)?);
            builder.sheet_uuid_path = Some(required_attribute(event, b"tstamps", limits)?);
        }
        b"libsource" if component.is_some() => {
            let builder = component.as_mut().expect("checked above");
            builder.library = Some(required_attribute(event, b"lib", limits)?);
            builder.library_part = Some(required_attribute(event, b"part", limits)?);
        }
        b"net" if parent(stack) == Some(b"nets") => {
            if net.is_some() {
                return Err(NetlistParseError::Schema("nested net"));
            }
            *net = Some(NetBuilder {
                code: required_attribute(event, b"code", limits)?,
                name: required_attribute(event, b"name", limits)?,
                class: optional_attribute(event, b"class", limits)?,
                nodes: Vec::new(),
            });
        }
        b"node" if net.is_some() => {
            if *node_count >= limits.max_nodes {
                return Err(NetlistParseError::CountLimit {
                    kind: "node",
                    limit: limits.max_nodes,
                });
            }
            let builder = net.as_mut().expect("checked above");
            builder.nodes.push(NetlistNode {
                reference: required_attribute(event, b"ref", limits)?,
                pin: required_attribute(event, b"pin", limits)?,
                pin_function: optional_attribute(event, b"pinfunction", limits)?
                    .unwrap_or_default(),
                pin_type: required_attribute(event, b"pintype", limits)?,
            });
            *node_count += 1;
        }
        _ => {}
    }
    Ok(())
}

fn handle_end(
    name: &[u8],
    component: &mut Option<ComponentBuilder>,
    net: &mut Option<NetBuilder>,
    components: &mut Vec<NetlistComponent>,
    nets: &mut Vec<NetlistNet>,
    limits: &NetlistLimits,
) -> Result<(), NetlistParseError> {
    match name {
        b"comp" => {
            let builder = component.take().ok_or(NetlistParseError::Schema(
                "component closed without opening",
            ))?;
            if components.len() >= limits.max_components {
                return Err(NetlistParseError::CountLimit {
                    kind: "component",
                    limit: limits.max_components,
                });
            }
            components.push(NetlistComponent {
                reference: builder.reference,
                value: builder
                    .value
                    .ok_or(NetlistParseError::Schema("component missing value"))?,
                footprint: builder.footprint,
                symbol_uuid: builder.symbol_uuid,
                sheet_path: builder
                    .sheet_path
                    .ok_or(NetlistParseError::Schema("component missing sheetpath"))?,
                sheet_uuid_path: builder.sheet_uuid_path.ok_or(NetlistParseError::Schema(
                    "component missing sheetpath tstamps",
                ))?,
                library: builder
                    .library
                    .ok_or(NetlistParseError::Schema("component missing libsource"))?,
                library_part: builder.library_part.ok_or(NetlistParseError::Schema(
                    "component missing libsource part",
                ))?,
            });
        }
        b"net" => {
            let builder = net
                .take()
                .ok_or(NetlistParseError::Schema("net closed without opening"))?;
            if nets.len() >= limits.max_nets {
                return Err(NetlistParseError::CountLimit {
                    kind: "net",
                    limit: limits.max_nets,
                });
            }
            nets.push(NetlistNet {
                code: builder.code,
                name: builder.name,
                class: builder.class,
                nodes: builder.nodes,
            });
        }
        _ => {}
    }
    Ok(())
}

fn handle_text(stack: &[Vec<u8>], value: String, component: &mut Option<ComponentBuilder>) {
    let Some(builder) = component.as_mut() else {
        return;
    };
    match stack.last().map(Vec::as_slice) {
        Some(b"value") => builder.value = Some(value),
        Some(b"footprint") => builder.footprint = Some(value),
        Some(b"tstamps") => builder.symbol_uuid = Some(value),
        _ => {}
    }
}

fn required_attribute(
    event: &BytesStart<'_>,
    key: &[u8],
    limits: &NetlistLimits,
) -> Result<String, NetlistParseError> {
    optional_attribute(event, key, limits)?
        .ok_or(NetlistParseError::Schema("missing required attribute"))
}

fn optional_attribute(
    event: &BytesStart<'_>,
    key: &[u8],
    limits: &NetlistLimits,
) -> Result<Option<String>, NetlistParseError> {
    for attribute in event.attributes() {
        let attribute =
            attribute.map_err(|error| NetlistParseError::Malformed(error.to_string()))?;
        check_field(attribute.key.as_ref(), limits.max_field_bytes)?;
        check_field(attribute.value.as_ref(), limits.max_field_bytes)?;
        if attribute.key.as_ref() == key {
            let value = attribute
                .decode_and_unescape_value(event.decoder())
                .map_err(|error| NetlistParseError::Malformed(error.to_string()))?
                .into_owned();
            check_field(value.as_bytes(), limits.max_field_bytes)?;
            return Ok(Some(value));
        }
    }
    Ok(None)
}

fn check_field(value: &[u8], limit: usize) -> Result<(), NetlistParseError> {
    if value.len() > limit {
        return Err(NetlistParseError::FieldTooLarge { limit });
    }
    Ok(())
}

fn parent(stack: &[Vec<u8>]) -> Option<&[u8]> {
    stack.last().map(Vec::as_slice)
}

fn component_order(component: &NetlistComponent) -> (&str, &str, &str, &str) {
    (
        &component.sheet_uuid_path,
        &component.reference,
        component.symbol_uuid.as_deref().unwrap_or(""),
        &component.value,
    )
}

fn node_order(node: &NetlistNode) -> (&str, &str, &str, &str) {
    (
        &node.reference,
        &node.pin,
        &node.pin_function,
        &node.pin_type,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    const XML: &str = r#"<?xml version="1.0"?>
<export version="D">
  <design>
    <sheet number="2" name="/Power/" tstamps="/power/"/>
    <sheet number="1" name="/" tstamps="/"/>
  </design>
  <components>
    <comp ref="R2"><value>two</value><footprint></footprint><libsource lib="Device" part="R"/><sheetpath names="/" tstamps="/"/><tstamps>u2</tstamps></comp>
    <comp ref="R1"><value>one</value><libsource lib="Device" part="R"/><sheetpath names="/" tstamps="/"/><tstamps>u1</tstamps></comp>
    <comp ref="R1"><value>duplicate</value><libsource lib="Device" part="R"/><sheetpath names="/" tstamps="/"/><tstamps>u3</tstamps></comp>
  </components>
  <nets>
    <net code="2" name="N2"><node ref="R2" pin="2" pinfunction="" pintype="passive"/><node ref="R1" pin="1" pinfunction="in" pintype="input"/></net>
    <net code="1" name="N1"><node ref="R1" pin="1" pinfunction="in" pintype="input"/></net>
  </nets>
</export>"#;

    fn limited_limits() -> NetlistLimits {
        NetlistLimits {
            max_xml_bytes: 1024 * 1024,
            max_field_bytes: 1024,
            ..NetlistLimits::default()
        }
    }

    #[test]
    fn parses_and_sorts_preserving_duplicates_and_footprint_presence() {
        let document = parse_netlist_xml(XML.as_bytes(), limited_limits()).unwrap();
        assert_eq!(document.export_version, "D");
        assert_eq!(document.sheets.len(), 2);
        assert_eq!(document.sheets[0].number, "1");
        assert_eq!(document.sheets[0].name, "/");
        assert_eq!(document.sheets[1].uuid_path, "/power/");
        assert_eq!(document.components.len(), 3);
        assert_eq!(document.components[0].reference, "R1");
        assert_eq!(document.components[1].reference, "R1");
        assert_eq!(document.components[2].reference, "R2");
        assert_eq!(document.components[0].footprint, None);
        assert_eq!(document.components[2].footprint, Some(String::new()));
        assert_eq!(document.nets[0].code, "1");
        assert_eq!(document.nets[1].nodes[0].reference, "R1");
        assert_eq!(document.nets[1].nodes[1].reference, "R2");
    }

    #[test]
    fn rejects_malformed_dtd_and_missing_schema_data() {
        for xml in [
            "<export version=\"D\"><components/><nets>",
            "<!DOCTYPE export [<!ENTITY x SYSTEM \"file:///etc/passwd\">]><export version=\"D\"><components/><nets/></export>",
            "<export version=\"D\"><components/></export>",
            "<export><components/><nets/></export>",
            "<export version=\"D\"><components><comp ref=\"R1\"><value>x</value></comp></components><nets/></export>",
        ] {
            assert!(
                parse_netlist_xml(xml.as_bytes(), limited_limits()).is_err(),
                "accepted: {xml}"
            );
        }
    }

    #[test]
    fn enforces_xml_field_and_count_limits() {
        let mut limits = limited_limits();
        limits.max_xml_bytes = 4;
        assert!(matches!(
            parse_netlist_xml(XML.as_bytes(), limits),
            Err(NetlistParseError::InputTooLarge { .. })
        ));

        let mut limits = limited_limits();
        limits.max_components = 1;
        assert!(matches!(
            parse_netlist_xml(XML.as_bytes(), limits),
            Err(NetlistParseError::CountLimit {
                kind: "component",
                ..
            })
        ));

        let mut limits = limited_limits();
        limits.max_field_bytes = 1;
        assert!(matches!(
            parse_netlist_xml(XML.as_bytes(), limits),
            Err(NetlistParseError::FieldTooLarge { .. })
        ));
    }

    #[cfg(unix)]
    fn fake_cli(script: &str) -> (tempfile::TempDir, PathBuf, PathBuf, PathBuf) {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let executable = dir.path().join("fake-kicad-cli");
        let args = dir.path().join("args");
        let stderr = dir.path().join("stderr");
        let body = script.replace("ARGS_FILE", &args.to_string_lossy());
        fs::write(&executable, body).unwrap();
        fs::set_permissions(&executable, fs::Permissions::from_mode(0o755)).unwrap();
        let executable = fs::canonicalize(executable).unwrap();
        (dir, executable, args, stderr)
    }

    #[cfg(unix)]
    fn project() -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let root = fs::canonicalize(dir.path()).unwrap();
        let schematic = root.join("root.kicad_sch");
        fs::write(&schematic, "(kicad_sch)").unwrap();
        (dir, schematic)
    }

    #[cfg(unix)]
    #[test]
    fn runner_uses_fixed_arguments_and_retains_stderr() {
        let script = format!(
            "#!/bin/sh\nprintf '%s\\n' \"$@\" > ARGS_FILE\nprintf '%s' '{}'; printf '%s' 'warning\\n' >&2",
            XML
        );
        let (_cli_dir, executable, args, _stderr) = fake_cli(&script);
        let (project_root, schematic) = project();
        let project_root = fs::canonicalize(project_root.path()).unwrap();
        let run = NetlistRunner::new(executable)
            .run(&project_root, &schematic)
            .unwrap();
        assert_eq!(run.stderr, "warning\\n");
        let argv = fs::read_to_string(args).unwrap();
        assert_eq!(
            argv.lines().collect::<Vec<_>>(),
            vec![
                "sch",
                "export",
                "netlist",
                "--format",
                "kicadxml",
                "-o",
                "/dev/stdout",
                schematic.to_str().unwrap()
            ]
        );
    }

    #[cfg(unix)]
    #[test]
    fn runner_accepts_symlinked_executable() {
        let script = format!("#!/bin/sh\nprintf '%s' '{}'", XML);
        let (cli_dir, executable, _args, _stderr) = fake_cli(&script);
        let symlink = cli_dir.path().join("kicad-cli");
        std::os::unix::fs::symlink(&executable, &symlink).unwrap();
        let (project_root, schematic) = project();
        let run = NetlistRunner::new(symlink)
            .run(&fs::canonicalize(project_root.path()).unwrap(), &schematic)
            .unwrap();
        assert_eq!(run.document.components.len(), 3);
    }

    #[cfg(unix)]
    #[test]
    fn runner_reports_nonzero_timeout_and_stdout_limit() {
        let (_cli_dir, executable, _args, _stderr) =
            fake_cli("#!/bin/sh\necho warning >&2\nexit 7\n");
        let (project_root, schematic) = project();
        let error = NetlistRunner::new(executable)
            .run(&fs::canonicalize(project_root.path()).unwrap(), &schematic)
            .unwrap_err();
        assert!(matches!(
            error,
            NetlistRunnerError::ProcessFailed {
                status: Some(7),
                ..
            }
        ));

        let (_cli_dir, executable, _args, _stderr) = fake_cli("#!/bin/sh\nsleep 2\n");
        let (project_root, schematic) = project();
        let error = NetlistRunner::new(executable)
            .with_timeout(Duration::from_millis(20))
            .run(&fs::canonicalize(project_root.path()).unwrap(), &schematic)
            .unwrap_err();
        assert!(matches!(error, NetlistRunnerError::Timeout { .. }));

        let (_cli_dir, executable, completion, _stderr) =
            fake_cli("#!/bin/sh\nprintf '1234567890'\nsleep 30\nprintf done > 'ARGS_FILE'\n");
        let (project_root, schematic) = project();
        let started = Instant::now();
        let error = NetlistRunner::new(executable)
            .with_timeout(Duration::from_secs(2))
            .with_output_limits(3, 100)
            .run(&fs::canonicalize(project_root.path()).unwrap(), &schematic)
            .unwrap_err();
        assert!(matches!(
            error,
            NetlistRunnerError::StdoutTooLarge { limit: 3 }
        ));
        // Verify early termination of a long-running writer, allowing scheduler and filesystem
        // latency instead of imposing an unrelated subsecond desktop performance requirement.
        assert!(!completion.exists());
        assert!(started.elapsed() < Duration::from_secs(5));
    }
}
