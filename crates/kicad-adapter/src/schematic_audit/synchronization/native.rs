//! Bounded isolated native readback of exact synchronization board bytes.
mod regions;
mod serialization;
use super::*;
pub(super) use regions::{NativeRegionOwnership, RegionRecoveryEvidence, capture_regions};
mod project_rules;
use board::SavedSynchronizationBoard;
pub(super) use project_rules::{
    verify_project_endpoint_netclasses, verify_region_project_endpoint_netclasses,
};
use serde::Deserialize;
#[cfg(unix)]
use std::os::unix::{fs::OpenOptionsExt, process::CommandExt};
use std::{
    fs::File,
    process::{Child, Command, Stdio},
    time::{Duration, Instant},
};

pub(super) const PYTHON: &str =
    "/Applications/KiCad/KiCad.app/Contents/Frameworks/Python.framework/Versions/3.9/bin/python3.9";
pub(super) const WORKER: &str = include_str!("../../../helpers/synchronization_board.py");
pub(super) const PROJECT_WORKER: &str = concat!(
    include_str!("../../../helpers/synchronization_board.py"),
    "\n",
    include_str!("../../../helpers/synchronization_project.py")
);
const MAX_INPUT: u64 = 128 * 1024 * 1024;
const MAX_REPORT: u64 = 64 * 1024 * 1024;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct NativeFootprint {
    pub uuid: String,
    pub reference: String,
    pub value: String,
    pub library: String,
    pub path: String,
    pub attributes: i64,
    pub locked: bool,
    pub position_nm: [i64; 2],
    pub orientation_degrees: f64,
    pub layer: i32,
}
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct NativePad {
    pub uuid: String,
    pub footprint: String,
    pub number: String,
    pub net: String,
    pub position_nm: [i64; 2],
    pub property: i32,
    pub pin_function: String,
    pub pin_type: String,
}
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct NativeCopper {
    pub uuid: String,
    pub kind: String,
    pub net: String,
    pub locked: bool,
    pub position_nm: [i64; 2],
}
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct NativeClassIdentity {
    name: String,
    priority: i32,
}
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct NativeClassDimension {
    defined: bool,
    value_nm: i64,
    parent: Option<NativeClassIdentity>,
}
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct NativeClassText {
    defined: bool,
    value: String,
    parent: Option<NativeClassIdentity>,
}
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct NativeEffectiveClass {
    name: String,
    priority: i32,
    description: String,
    tuning_profile: NativeClassText,
    constituents: Vec<NativeClassIdentity>,
    fields: BTreeMap<String, NativeClassDimension>,
}
const CLASS_FIELDS: [&str; 9] = [
    "Clearance",
    "TrackWidth",
    "ViaDiameter",
    "ViaDrill",
    "uViaDiameter",
    "uViaDrill",
    "DiffPairWidth",
    "DiffPairGap",
    "DiffPairViaGap",
];

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct NativeReport {
    pub native_version: String,
    netclasses: BTreeMap<String, NativeEffectiveClass>,
    pub board_sha256: String,
    pub project_sha256: String,
    pub native_saved_sha256: String,
    pub footprints: Vec<NativeFootprint>,
    pub pads: Vec<NativePad>,
    pub copper: Vec<NativeCopper>,
}
pub(super) struct NativeBoardProjection {
    pub project_classes: Option<project::native::ProjectNetclassSnapshot>,
    pub board: SavedSynchronizationBoard,
    pub report: NativeReport,
    pub digest: String,
    worker: NativeWorkerReceipt,
}
impl NativeBoardProjection {
    /// Recheck only fixed executable/environment authority after separately verified source transitions.
    pub(super) fn verify_execution(&self, audit: &SchematicAudit) -> Result<(), String> {
        self.worker.verify_execution(
            audit
                .inner
                .runner
                .execution_context()
                .ok_or("missing native process context")?,
        )
    }
    pub fn verify(&self, audit: &SchematicAudit) -> Result<(), String> {
        self.worker.verify(
            audit,
            audit
                .inner
                .runner
                .execution_context()
                .ok_or("missing native process context")?,
        )
    }
}

#[derive(Serialize)]
pub(super) struct NativeWorkerReceipt {
    execution: String,
    executable: InputFile,
    worker_sha256: String,
}
impl NativeWorkerReceipt {
    pub(super) fn verify(
        &self,
        audit: &SchematicAudit,
        context: &NativeExecutionContext,
    ) -> Result<(), String> {
        audit.verify()?;
        self.verify_execution(context)
    }
    pub(super) fn verify_prefix(
        &self,
        audit: &SchematicAudit,
        context: &NativeExecutionContext,
        prefix: &crate::schematic_audit::source_transition::VerifiedInputPrefix<'_>,
    ) -> Result<(), String> {
        audit.verify_publication_prefix(prefix)?;
        self.verify_execution(context)
    }
    fn verify_execution(&self, context: &NativeExecutionContext) -> Result<(), String> {
        context.verify().map_err(|e| e.to_string())?;
        if context.digest() != self.execution {
            return Err("native worker execution context changed".into());
        }
        let current = Inputs::read(&self.executable.requested)?;
        if current.canonical != self.executable.canonical
            || current.digest != self.executable.digest
            || current.size != self.executable.size
        {
            return Err("native board executable changed".into());
        }
        Ok(())
    }
}

pub(super) fn bounded_read(path: &Path, limit: u64) -> Result<Vec<u8>, String> {
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
    let file = options.open(path).map_err(|e| e.to_string())?;
    let metadata = file.metadata().map_err(|e| e.to_string())?;
    if !metadata.is_file() || metadata.len() == 0 || metadata.len() > limit {
        return Err("native synchronization input exceeds regular-file bound".into());
    }
    let mut bytes = Vec::new();
    file.take(limit + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| e.to_string())?;
    if bytes.len() as u64 != metadata.len() {
        return Err("native synchronization input changed while reading".into());
    }
    Ok(bytes)
}

struct OwnedWorker(Child);
impl Drop for OwnedWorker {
    fn drop(&mut self) {
        if self.0.try_wait().ok().flatten().is_none() {
            #[cfg(unix)]
            unsafe {
                libc::kill(-(self.0.id() as i32), libc::SIGKILL);
            }
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
}

pub(super) fn run_fixed_worker(
    audit: &SchematicAudit,
    context: &NativeExecutionContext,
    root: &Path,
    worker: &str,
    args: &[&Path],
) -> Result<(Vec<u8>, NativeWorkerReceipt), String> {
    audit.verify()?;
    context.verify().map_err(|e| e.to_string())?;
    if fs::canonicalize(root).map_err(|e| e.to_string())? != root
        || !root.is_dir()
        || args.iter().any(|p| !p.starts_with(root))
    {
        return Err("native worker paths are not private and canonical".into());
    }
    let mut executable = Inputs::read(&fs::canonicalize(PYTHON).map_err(|e| e.to_string())?)?;
    executable.bytes = None;
    let stdout = root.join("stdout.json");
    let stderr = root.join("stderr.log");
    let mut command = Command::new(
        executable
            .canonical
            .as_ref()
            .ok_or("native Python executable is absent")?,
    );
    command.env_clear();
    for name in context.environment_names() {
        command.env(
            name,
            context
                .environment_value(name)
                .ok_or("missing captured environment variable")?,
        );
    }
    command
        .args(["-I", "-B", "-c", worker])
        .args(args)
        .current_dir(root)
        .stdin(Stdio::null())
        .stdout(Stdio::from(
            File::create(&stdout).map_err(|e| e.to_string())?,
        ))
        .stderr(Stdio::from(
            File::create(&stderr).map_err(|e| e.to_string())?,
        ));
    #[cfg(unix)]
    unsafe {
        command.pre_exec(|| {
            if libc::setpgid(0, 0) != 0 {
                return Err(std::io::Error::last_os_error());
            }
            let limit = libc::rlimit {
                rlim_cur: MAX_INPUT + 1024 * 1024,
                rlim_max: MAX_INPUT + 1024 * 1024,
            };
            if libc::setrlimit(libc::RLIMIT_FSIZE, &limit) != 0 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    runtime::owned_temp::inherit_leases(&mut command);
    let mut child = OwnedWorker(
        command
            .spawn()
            .map_err(|e| format!("native board worker unavailable: {e}"))?,
    );
    let deadline = Instant::now() + Duration::from_secs(60);
    let status = loop {
        match child.0.try_wait().map_err(|e| e.to_string())? {
            Some(status) => break status,
            None if Instant::now() < deadline => {
                for (path, limit) in [(&stdout, MAX_REPORT), (&stderr, 1024 * 1024)] {
                    let metadata = fs::symlink_metadata(path).map_err(|e| e.to_string())?;
                    if !metadata.is_file() || metadata.len() > limit {
                        return Err("native worker output exceeds bounds".into());
                    }
                }
                std::thread::sleep(Duration::from_millis(10));
            }
            None => return Err("native board worker timed out".into()),
        }
    };
    if !status.success() {
        let stderr = bounded_read(&stderr, 1024 * 1024).unwrap_or_default();
        return Err(format!(
            "native board worker failed ({:?}): {}",
            status.code(),
            String::from_utf8_lossy(&stderr[stderr.len().saturating_sub(4096)..])
        ));
    }
    let report_bytes = bounded_read(&stdout, MAX_REPORT)?;
    kicad_checks::validate_json_structure(&report_bytes).map_err(|e| e.to_string())?;

    let receipt = NativeWorkerReceipt {
        execution: context.digest().into(),
        executable,
        worker_sha256: hash(worker.as_bytes()),
    };
    receipt.verify(audit, context)?;
    Ok((report_bytes, receipt))
}

pub(super) fn capture(
    audit: &SchematicAudit,
    filename: &std::ffi::OsStr,
    bytes: &[u8],
    project: &[u8],
) -> Result<NativeBoardProjection, String> {
    capture_with_worker(audit, filename, bytes, project, false)
}

pub(super) fn capture_project(
    audit: &SchematicAudit,
    filename: &std::ffi::OsStr,
    bytes: &[u8],
    project: &[u8],
) -> Result<NativeBoardProjection, String> {
    capture_with_worker(audit, filename, bytes, project, true)
}

fn capture_with_worker(
    audit: &SchematicAudit,
    filename: &std::ffi::OsStr,
    bytes: &[u8],
    project: &[u8],
    project_classes: bool,
) -> Result<NativeBoardProjection, String> {
    audit.verify()?;
    if bytes.is_empty()
        || bytes.len() as u64 > MAX_INPUT
        || project.is_empty()
        || project.len() > 32 * 1024 * 1024
    {
        return Err("native synchronization capture exceeds bounds".into());
    }
    if Path::new(filename).file_name() != Some(filename)
        || Path::new(filename).extension().and_then(|v| v.to_str()) != Some("kicad_pcb")
    {
        return Err("native board capture requires one basename".into());
    }
    let context = audit
        .inner
        .runner
        .execution_context()
        .ok_or("missing native process context")?;
    context.verify().map_err(|e| e.to_string())?;
    let owner = runtime::owned_temp::Builder::new()
        .prefix("auto-kicad-sync-native-")
        .tempdir()
        .map_err(|e| e.to_string())?;
    let root = fs::canonicalize(owner.path()).map_err(|e| e.to_string())?;
    let input = root.join(filename);
    let pro = input.with_extension("kicad_pro");
    let output = root.join(".native-result.kicad_pcb");
    write_new(&input, bytes)?;
    write_new(&pro, project)?;
    let (report_bytes, worker) = run_fixed_worker(
        audit,
        context,
        &root,
        if project_classes {
            PROJECT_WORKER
        } else {
            WORKER
        },
        &[&input, &output],
    )?;
    let report: NativeReport = serde_json::from_slice(&report_bytes).map_err(|e| e.to_string())?;
    let saved = bounded_read(&output, MAX_INPUT)?;
    if report.native_version != "10.0.7"
        || report.board_sha256 != hash(bytes)
        || report.project_sha256 != hash(project)
        || report.native_saved_sha256 != hash(&saved)
        || bounded_read(&input, MAX_INPUT)? != bytes
        || bounded_read(&pro, 32 * 1024 * 1024)? != project
    {
        return Err("native synchronization input/result binding differs".into());
    }
    let board =
        SavedSynchronizationBoard::parse(std::str::from_utf8(&saved).map_err(|e| e.to_string())?)?;
    validate_native(&board, &report)?;
    let original =
        SavedSynchronizationBoard::parse(std::str::from_utf8(bytes).map_err(|e| e.to_string())?)?;
    validate_native(&original, &report)?;
    original.verify_native_units(&board)?;
    let project_classes = if project_classes {
        let bytes = bounded_read(&root.join(".project-netclasses.json"), MAX_REPORT)?;
        kicad_checks::validate_json_structure(&bytes).map_err(|e| e.to_string())?;
        let observed: project::native::ProjectNetclassSnapshot =
            serde_json::from_slice(&bytes).map_err(|e| e.to_string())?;
        observed.validate(
            &report.board_sha256,
            project,
            &serde_json::to_value(&report.netclasses).map_err(|e| e.to_string())?,
        )?;
        Some(observed)
    } else {
        None
    };
    let mut evidence = serde_json::json!({"report":report,"worker":worker});
    if let Some(observed) = &project_classes {
        evidence["project_classes"] = serde_json::to_value(observed).map_err(|e| e.to_string())?;
    }
    let result = NativeBoardProjection {
        project_classes,
        digest: hash(&serde_json::to_vec(&evidence).map_err(|e| e.to_string())?),
        board,
        report,
        worker,
    };
    result.verify(audit)?;
    Ok(result)
}

fn validate_native(board: &SavedSynchronizationBoard, report: &NativeReport) -> Result<(), String> {
    if report.netclasses.keys().cloned().collect::<BTreeSet<_>>() != *board.net_names()
        || report.netclasses.len() > 100_000
    {
        return Err("native effective netclass inventory differs".into());
    }
    for (net, class) in &report.netclasses {
        let valid = |text: &str| !text.contains('\0') && text.len() <= 65536;
        if net.is_empty()
            || !valid(net)
            || class.name.is_empty()
            || !valid(&class.name)
            || !valid(&class.description)
            || !valid(&class.tuning_profile.value)
            || class.constituents.is_empty()
            || class.constituents.len() > 512
            || class
                .constituents
                .iter()
                .any(|c| c.name.is_empty() || !valid(&c.name))
            || class
                .constituents
                .iter()
                .map(|c| &c.name)
                .collect::<BTreeSet<_>>()
                .len()
                != class.constituents.len()
            || class
                .fields
                .keys()
                .map(String::as_str)
                .collect::<BTreeSet<_>>()
                != BTreeSet::from(CLASS_FIELDS)
            || class.fields.values().any(|v| {
                !(-1..=2_000_000_000).contains(&v.value_nm)
                    || (v.defined && v.value_nm < 0)
                    || v.parent
                        .as_ref()
                        .is_some_and(|p| p.name.is_empty() || !valid(&p.name))
            })
            || class
                .tuning_profile
                .parent
                .as_ref()
                .is_some_and(|p| p.name.is_empty() || !valid(&p.name))
        {
            return Err("incomplete or out-of-bound native effective netclass".into());
        }
    }
    if report.footprints.len() != board.footprints().len()
        || report.footprints.len() > 100_000
        || report.pads.len() > 100_000
        || report.copper.len() > 100_000
    {
        return Err("native board inventory bounds differ".into());
    }
    let mut ids = BTreeSet::new();
    for fp in &report.footprints {
        let parsed = board
            .footprints()
            .get(&fp.uuid)
            .ok_or("native footprint missing from saved source")?;
        if !ids.insert(fp.uuid.as_str())
            || fp.reference != parsed.reference
            || fp.value != parsed.value
            || fp.library != parsed.library
            || fp.path.trim_end_matches('/')
                != parsed.path.as_deref().unwrap_or("").trim_end_matches('/')
            || fp.locked != parsed.locked
            || !fp.orientation_degrees.is_finite()
            || fp
                .position_nm
                .iter()
                .any(|v| v.unsigned_abs() > 2_000_000_000)
        {
            return Err("native footprint metadata or position differs".into());
        }
    }
    let expected = board
        .footprints()
        .values()
        .flat_map(|fp| fp.pads.iter().map(move |p| (&p.uuid, (&fp.uuid, p))))
        .collect::<BTreeMap<_, _>>();
    if expected.len() != report.pads.len() {
        return Err("native pad set differs".into());
    }
    for pad in &report.pads {
        let (fp, parsed) = expected
            .get(&pad.uuid)
            .ok_or("native pad missing from saved source")?;
        if !ids.insert(pad.uuid.as_str())
            || *fp != &pad.footprint
            || parsed.number != pad.number
            || parsed.net != pad.net
            || parsed.pin_function != pad.pin_function
            || parsed.pin_type != pad.pin_type
            || pad
                .position_nm
                .iter()
                .any(|v| v.unsigned_abs() > 2_000_000_000)
        {
            return Err("native pad metadata differs".into());
        }
    }
    if report.copper.len() != board.copper().len() {
        return Err("native copper set differs".into());
    }
    for item in &report.copper {
        let (net, locked) = board
            .copper()
            .get(&item.uuid)
            .ok_or("native copper missing from saved source")?;
        if !ids.insert(item.uuid.as_str())
            || net != &item.net
            || locked != &item.locked
            || !["PCB_TRACK", "PCB_ARC", "PCB_VIA", "PCB_SHAPE"].contains(&item.kind.as_str())
            || item
                .position_nm
                .iter()
                .any(|v| v.unsigned_abs() > 2_000_000_000)
        {
            return Err("native copper metadata differs".into());
        }
    }
    Ok(())
}

pub(super) fn verify_retained_netclasses(
    before: &NativeReport,
    after: &NativeReport,
) -> Result<(), String> {
    let old = before
        .pads
        .iter()
        .map(|pad| (&pad.uuid, &pad.net))
        .chain(before.copper.iter().map(|item| (&item.uuid, &item.net)))
        .collect::<BTreeMap<_, _>>();
    let new = after
        .pads
        .iter()
        .map(|pad| (&pad.uuid, &pad.net))
        .chain(after.copper.iter().map(|item| (&item.uuid, &item.net)))
        .collect::<BTreeMap<_, _>>();
    for (id, name) in old {
        let Some(next) = new.get(id) else {
            continue;
        };
        if name.is_empty() || next.is_empty() {
            continue;
        }
        let old_class = before
            .netclasses
            .get(name)
            .ok_or("missing original effective netclass")?;
        let new_class = after
            .netclasses
            .get(*next)
            .ok_or("missing candidate effective netclass")?;
        if old_class != new_class {
            return Err(format!(
                "synchronization_effective_netclass_change_requires_explicit_project_edit: {id}: {} -> {}",
                old_class.name, new_class.name
            ));
        }
    }
    Ok(())
}

pub(super) fn verify_replacement_netclasses(
    before: &NativeReport,
    after: &NativeReport,
    mapping: &BTreeMap<String, String>,
) -> Result<(), String> {
    for (old, new) in mapping {
        let original = before
            .pads
            .iter()
            .find(|pad| &pad.uuid == old)
            .ok_or("missing original replacement endpoint")?;
        let candidate = after
            .pads
            .iter()
            .find(|pad| &pad.uuid == new)
            .ok_or("missing candidate replacement endpoint")?;
        if original.number != candidate.number {
            return Err("replacement endpoint changed pad number".into());
        }
        if original.net.is_empty() || candidate.net.is_empty() {
            continue;
        }
        let old_class = before
            .netclasses
            .get(&original.net)
            .ok_or("missing original replacement effective netclass")?;
        let new_class = after
            .netclasses
            .get(&candidate.net)
            .ok_or("missing candidate replacement effective netclass")?;
        if old_class != new_class {
            return Err(format!(
                "synchronization_replacement_effective_netclass_change_requires_explicit_project_edit: {old}"
            ));
        }
    }
    Ok(())
}
/// The complete final native class must contain every non-default source requirement.
pub(super) fn verify_source_netclasses(
    source: &SynchronizationSource,
    project: &[u8],
    report: &NativeReport,
) -> Result<BTreeMap<String, BTreeSet<String>>, String> {
    let requirements = super::source_classes::requirements(source, project)?;
    for (net, required) in &requirements {
        let actual = report
            .netclasses
            .get(net)
            .ok_or_else(|| format!("source-class target net is absent from native PCB: {net:?}"))?;
        for class in required {
            if !actual
                .constituents
                .iter()
                .any(|member| &member.name == class)
            {
                return Err(format!(
                    "source netclass {class:?} is missing on PCB net {net:?}; select an explicit project netclass change"
                ));
            }
        }
    }
    Ok(requirements)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn retained_endpoints_preserve_complete_effective_class_values_and_provenance() {
        let identity = NativeClassIdentity {
            name: "Default".into(),
            priority: i32::MAX,
        };
        let class = NativeEffectiveClass {
            name: identity.name.clone(),
            priority: identity.priority,
            description: String::new(),
            tuning_profile: NativeClassText {
                defined: true,
                value: "profile".into(),
                parent: Some(identity.clone()),
            },
            constituents: vec![identity.clone()],
            fields: CLASS_FIELDS
                .into_iter()
                .map(|key| {
                    (
                        key.into(),
                        NativeClassDimension {
                            defined: true,
                            value_nm: 200000,
                            parent: Some(identity.clone()),
                        },
                    )
                })
                .collect(),
        };
        let before = NativeReport {
            native_version: "10.0.7".into(),
            board_sha256: String::new(),
            project_sha256: String::new(),
            native_saved_sha256: String::new(),
            footprints: vec![],
            copper: vec![],
            pads: vec![NativePad {
                uuid: "pad".into(),
                footprint: "fp".into(),
                number: "1".into(),
                net: "old".into(),
                position_nm: [0, 0],
                property: 0,
                pin_function: String::new(),
                pin_type: String::new(),
            }],
            netclasses: BTreeMap::from([("old".into(), class.clone())]),
        };
        let mut after = before.clone();
        after.pads[0].net = "new".into();
        after.netclasses = BTreeMap::from([("new".into(), class.clone())]);
        verify_retained_netclasses(&before, &after).unwrap();
        for mut changed in [
            class.clone(),
            class.clone(),
            class.clone(),
            class.clone(),
            class.clone(),
            class.clone(),
        ]
        .into_iter()
        .enumerate()
        {
            match changed.0 {
                0 => changed.1.name = "Power".into(),
                1 => changed.1.fields.get_mut("Clearance").unwrap().value_nm += 1,
                2 => changed.1.fields.get_mut("ViaDrill").unwrap().defined = false,
                3 => {
                    changed
                        .1
                        .fields
                        .get_mut("TrackWidth")
                        .unwrap()
                        .parent
                        .as_mut()
                        .unwrap()
                        .priority = 0
                }
                4 => changed.1.constituents[0].name = "Power".into(),
                _ => changed.1.tuning_profile.value = "different".into(),
            }
            after.netclasses.insert("new".into(), changed.1);
            assert!(verify_retained_netclasses(&before, &after).is_err());
        }
    }
}
