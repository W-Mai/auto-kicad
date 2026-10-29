//! Exercise durable per-file publication and process-exit recovery on an owned native project.
use auto_kicad::recovery::{ExecutionStore, SchematicWrite, SourceExistence};
use kicad_adapter::schematic_audit::{
    SchematicAudit, SchematicAuditPolicy, SchematicEditExpectation,
};
use kicad_model::RevisionDigest;
use kicad_schematic::editing::{EditAction, EditScope, SchematicEdit, SymbolProperty};
use serde::{Deserialize, Serialize};
use serde_json::json;
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    env, fs,
    path::{Path, PathBuf},
    process::Command,
};

type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

#[derive(Serialize, Deserialize)]
struct WriteInput {
    original: PathBuf,
    before: SourceExistence,
    candidate: PathBuf,
}
#[derive(Serialize, Deserialize)]
struct Manifest {
    project: PathBuf,
    state: PathBuf,
    writes: Vec<WriteInput>,
    boundary: usize,
}

fn digest(bytes: &[u8]) -> RevisionDigest {
    RevisionDigest::new(hex::encode(Sha256::digest(bytes))).expect("sha256")
}

fn snapshot(root: &Path) -> Result<BTreeMap<PathBuf, RevisionDigest>> {
    let mut stack = vec![root.to_owned()];
    let mut files = BTreeMap::new();
    let mut total = 0;
    while let Some(path) = stack.pop() {
        let metadata = fs::symlink_metadata(&path)?;
        if metadata.file_type().is_symlink() {
            return Err("symlink in owned input".into());
        }
        if metadata.is_dir() {
            for entry in fs::read_dir(path)? {
                stack.push(entry?.path());
            }
        } else if metadata.is_file() {
            if metadata.len() > 32 * 1024 * 1024 {
                return Err("input file exceeds bound".into());
            }
            let bytes = fs::read(&path)?;
            total += bytes.len();
            if total > 128 * 1024 * 1024 || files.len() >= 8192 {
                return Err("input snapshot exceeds bound".into());
            }
            files.insert(path.strip_prefix(root)?.to_owned(), digest(&bytes));
        } else {
            return Err("special file in owned input".into());
        }
    }
    Ok(files)
}

fn child(manifest: &Path) -> Result<()> {
    let input: Manifest = serde_json::from_slice(&fs::read(manifest)?)?;
    let store = ExecutionStore::open(&input.state)?;
    let writer = store.acquire(&input.project)?;
    let writes = input
        .writes
        .iter()
        .map(|file| {
            Ok(SchematicWrite {
                original: file.original.clone(),
                before: file.before.clone(),
                bytes: fs::read(&file.candidate)?,
            })
        })
        .collect::<Result<Vec<_>>>()?;
    let mut publication = writer.prepare_schematic_publication(
        "native-publication-plan",
        "native-publication-approval",
        json!({"acceptance_only_file_io": true}),
        &writes,
    )?;
    let path = manifest.with_extension("attempt-id");
    fs::write(&path, publication.attempt_id())?;
    fs::File::open(path)?.sync_all()?;
    for _ in 0..input.boundary {
        if !publication.publish_next()? {
            return Err("publication ended before requested exit boundary".into());
        }
    }
    if input.boundary == writes.len() {
        publication.verify_complete()?;
    }
    // Acceptance-only hard process exit: no Rust destructors and no terminal execution record.
    std::process::exit(79);
}

fn main() -> Result<()> {
    let args = env::args_os().skip(1).collect::<Vec<_>>();
    if args.len() == 2 && args[0] == "--publication-child" {
        return child(Path::new(&args[1]));
    }
    if args.len() != 4 {
        return Err(
            "usage: check-schematic-publication SERVER OWNED_SCHEMATIC OWNED_CONFIG NEW_OUTPUT"
                .into(),
        );
    }
    let server = fs::canonicalize(&args[0])?;
    let source = fs::canonicalize(&args[1])?;
    let project = source.parent().ok_or("missing schematic parent")?;
    let config = fs::canonicalize(&args[2])?;
    for root in [project, &config] {
        if fs::read_to_string(root.join(".auto-kicad-audit-owned"))?
            != "schematic-audit-acceptance\n"
        {
            return Err("owned-copy marker missing".into());
        }
    }
    let output = PathBuf::from(&args[3]);
    if output.starts_with(project) || output.starts_with(&config) {
        return Err("output overlaps source or configuration".into());
    }
    fs::create_dir(&output)?;
    let output = fs::canonicalize(output)?;
    let shared = Path::new("/Applications/KiCad/KiCad.app/Contents/SharedSupport");
    let policy = SchematicAuditPolicy::new(
        project,
        &source,
        &config,
        &shared.join("symbols"),
        &shared.join("footprints"),
        &[],
    )?;
    // Include the persistent lock in exact source snapshots before native observation.
    let initial_store = ExecutionStore::open(&output.join("initial-state"))?;
    drop(initial_store.acquire(project)?);
    let originals = snapshot(project)?;
    let configuration = snapshot(&config)?;
    let audit = SchematicAudit::prepare(&policy)?;
    let native_before = audit.netlist().clone();
    let erc_before = audit.erc().clone();
    let connectivity_before = audit.observe_pin_connectivity()?.connectivity().clone();
    let mut targets = BTreeMap::new();
    for unit in audit.identities().units() {
        if unit.reference.starts_with('R') {
            targets.entry(unit.file.clone()).or_insert(unit);
        }
    }
    if targets.len() != 2 {
        return Err(
            "owned fixture requires resistors in exactly two physical schematic files".into(),
        );
    }
    let edits = targets
        .values()
        .enumerate()
        .map(|(index, unit)| SchematicEdit {
            file: unit.file.clone(),
            scope: EditScope::FileAllInstances,
            action: EditAction::SetProperty {
                uuid: unit.source_uuid.clone(),
                property: SymbolProperty::Value,
                value: format!("AK_PUBLISH_{index}"),
            },
        })
        .collect::<Vec<_>>();
    let mut reference_changes = BTreeMap::new();
    for edit in &edits {
        let EditAction::SetProperty { uuid, value, .. } = &edit.action else {
            unreachable!()
        };
        for unit in audit
            .identities()
            .units()
            .iter()
            .filter(|unit| unit.file == edit.file && unit.source_uuid == *uuid)
        {
            reference_changes.insert(unit.reference.clone(), (edit.file.clone(), value.clone()));
        }
    }
    let preview = audit.preview_edits(&edits)?;
    let matched = preview.match_expectations(&SchematicEditExpectation::default())?;
    matched.verify()?;
    let mut writes = Vec::new();
    for (index, relative) in preview.candidate().replacements().keys().enumerate() {
        let original = project.join(relative);
        let candidate = output.join(format!("candidate-{index}.kicad_sch"));
        fs::write(&candidate, &preview.candidate().files()[relative])?;
        writes.push(WriteInput {
            before: SourceExistence::Present {
                sha256: digest(&fs::read(&original)?),
            },
            original,
            candidate,
        });
    }
    let new_child = project.join("auto-kicad-publication-child.kicad_sch");
    let candidate_child = output.join("candidate-new.kicad_sch");
    fs::write(&candidate_child, b"(kicad_sch (version 20260306) (generator eeschema) (uuid 88888888-8888-4888-8888-888888888888) (paper A4) (lib_symbols))\n")?;
    writes.push(WriteInput {
        original: new_child,
        before: SourceExistence::Absent,
        candidate: candidate_child,
    });
    if writes.len() != 3 {
        return Err("expected two existing candidates and one new source".into());
    }
    fs::write(
        output.join("preview.json"),
        serde_json::to_vec_pretty(preview.evidence())?,
    )?;
    fs::write(
        output.join("matching.json"),
        serde_json::to_vec_pretty(matched.evidence())?,
    )?;
    let baseline_private = audit.private_root().to_owned();
    let preview_private = preview.observed().private_root().to_owned();
    drop(matched);
    drop(preview);
    drop(audit);
    if baseline_private.exists() || preview_private.exists() {
        return Err("preview cleanup failed".into());
    }
    let mut cases = Vec::new();
    for boundary in 0..=writes.len() {
        let case = output.join(format!("boundary-{boundary}"));
        fs::create_dir(&case)?;
        let manifest = Manifest {
            project: project.into(),
            state: case.join("state"),
            writes: writes
                .iter()
                .map(|file| WriteInput {
                    original: file.original.clone(),
                    before: file.before.clone(),
                    candidate: file.candidate.clone(),
                })
                .collect(),
            boundary,
        };
        let manifest_path = case.join("manifest.json");
        fs::write(&manifest_path, serde_json::to_vec_pretty(&manifest)?)?;
        let status = Command::new(env::current_exe()?)
            .arg("--publication-child")
            .arg(&manifest_path)
            .status()?;
        if status.code() != Some(79) {
            return Err(format!("publication child failed at {boundary}: {status}").into());
        }
        let id = fs::read_to_string(manifest_path.with_extension("attempt-id"))?;
        let store = ExecutionStore::open(&manifest.state)?;
        let history = store.history(project)?;
        if history.len() != 1
            || history[0].state != "in_flight_or_interrupted"
            || history[0].post_attempt.is_some()
        {
            return Err("hard exit did not retain incomplete execution evidence".into());
        }
        let alternate = ExecutionStore::open(&case.join("other-state"))?;
        if alternate.acquire(project)?.ensure_ready().is_ok() {
            return Err("hard-exited publication allowed a foreign writer".into());
        }
        for (index, file) in writes.iter().enumerate() {
            if index < boundary {
                if fs::read(&file.original)? != fs::read(&file.candidate)? {
                    return Err("published bytes differ from candidate".into());
                }
            } else {
                let current = if file.original.exists() {
                    SourceExistence::Present {
                        sha256: digest(&fs::read(&file.original)?),
                    }
                } else {
                    SourceExistence::Absent
                };
                if current != file.before {
                    return Err("unpublished source changed".into());
                }
            }
        }
        let partial = SchematicAudit::prepare(&policy)?;
        let mut expected_native = native_before.clone();
        for component in &mut expected_native.components {
            if let Some((relative, value)) = reference_changes.get(&component.reference)
                && writes
                    .iter()
                    .take(boundary)
                    .any(|file| file.original == project.join(relative))
            {
                component.value = value.clone();
            }
        }
        if partial.netlist() != &expected_native
            || !erc_before.equivalent_to(partial.erc())?
            || partial.observe_pin_connectivity()?.connectivity() != &connectivity_before
        {
            return Err(
                "native partial publication differs from independently expected property changes"
                    .into(),
            );
        }
        fs::write(
            case.join("partial-native.json"),
            serde_json::to_vec_pretty(&partial.evidence()?)?,
        )?;
        let private = partial.private_root().to_owned();
        drop(partial);
        if private.exists() {
            return Err("partial audit cleanup failed".into());
        }
        let status = Command::new("python3")
            .env("PYTHONDONTWRITEBYTECODE", "1")
            .arg(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../scripts/check-recovery.py"))
            .arg(&server)
            .arg(&manifest.state)
            .arg(project)
            .arg(&id)
            .arg(case.join("mcp.json"))
            .status()?;
        if !status.success() {
            return Err("actual stdio publication recovery failed".into());
        }
        if snapshot(project)? != originals || snapshot(&config)? != configuration {
            return Err("restored project/configuration differs from original".into());
        }
        alternate.acquire(project)?.ensure_ready()?;
        let restored = SchematicAudit::prepare(&policy)?;
        if restored.netlist() != &native_before
            || !erc_before.equivalent_to(restored.erc())?
            || restored.observe_pin_connectivity()?.connectivity() != &connectivity_before
        {
            return Err("restored native baseline differs".into());
        }
        fs::write(
            case.join("restored-native.json"),
            serde_json::to_vec_pretty(&restored.evidence()?)?,
        )?;
        let private = restored.private_root().to_owned();
        drop(restored);
        if private.exists() {
            return Err("restored audit cleanup failed".into());
        }
        cases.push(json!({"published_files":boundary,"child_exit_code":79,"post_attempt_evidence_absent":true,
            "partial_native_expected":true,"actual_stdio_recovery":true,"complete_original_native_restored":true,
            "exact_project_and_configuration_restored":true,"foreign_state_writer_blocked":true,"private_snapshots_removed":true}));
    }
    let result = json!({"passed":true,"native_version":"10.0.7","existing_files":2,"new_unreferenced_files":1,
        "actual_hard_process_exit_cases":cases,"gui_launched":false,"general_native_publisher_implemented":false});
    fs::write(
        output.join("result.json"),
        serde_json::to_vec_pretty(&result)?,
    )?;
    println!("{}", serde_json::to_string(&result)?);
    Ok(())
}
