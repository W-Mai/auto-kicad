//! Verify mixed existing/new schematic recovery through production stdio MCP and native KiCad.
use auto_kicad::recovery::ExecutionStore;
use kicad_adapter::schematic_audit::{
    SchematicAudit, SchematicAuditPolicy, SchematicEditExpectation,
};
use kicad_schematic::editing::{EditAction, EditScope, SchematicEdit, SymbolProperty};
use serde_json::json;
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    env, fs,
    path::{Path, PathBuf},
    process::Command,
};

type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

fn snapshot(root: &Path) -> Result<BTreeMap<PathBuf, String>> {
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
            files.insert(
                path.strip_prefix(root)?.to_owned(),
                hex::encode(Sha256::digest(bytes)),
            );
        } else {
            return Err("special file in owned input".into());
        }
    }
    Ok(files)
}

fn main() -> Result<()> {
    let args = env::args_os().skip(1).collect::<Vec<_>>();
    if args.len() != 4 {
        return Err(
            "usage: check-schematic-recovery SERVER OWNED_SCHEMATIC OWNED_CONFIG NEW_OUTPUT".into(),
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
    let state = output.join("state");
    let store = ExecutionStore::open(&state)?;
    let writer = store.acquire(project)?;
    let originals = snapshot(project)?;
    let configuration = snapshot(&config)?;
    let shared = Path::new("/Applications/KiCad/KiCad.app/Contents/SharedSupport");
    let policy = SchematicAuditPolicy::new(
        project,
        &source,
        &config,
        &shared.join("symbols"),
        &shared.join("footprints"),
        &[],
    )?;
    let audit = SchematicAudit::prepare(&policy)?;
    let native_before = audit.netlist().clone();
    let erc_before = audit.erc().clone();
    let connectivity_before = audit.observe_pin_connectivity()?.connectivity().clone();
    let target = audit
        .identities()
        .units()
        .iter()
        .find(|unit| unit.reference == "R2")
        .ok_or("owned fixture needs R2")?;
    let preview = audit.preview_edits(&[SchematicEdit {
        file: target.file.clone(),
        scope: EditScope::FileAllInstances,
        action: EditAction::SetProperty {
            uuid: target.source_uuid.clone(),
            property: SymbolProperty::Value,
            value: "AK_RECOVERY_VALUE".into(),
        },
    }])?;
    let matched = preview.match_expectations(&SchematicEditExpectation::default())?;
    matched.verify()?;
    let candidate = preview
        .candidate()
        .files()
        .get(
            source
                .file_name()
                .and_then(|name| name.to_str())
                .ok_or("invalid basename")?,
        )
        .ok_or("root candidate missing")?
        .clone();
    let child = project.join("auto-kicad-recovery-child.kicad_sch");
    let attempt = writer.begin_schematic("native-mixed-schematic-plan", "native-mixed-schematic-approval",
        json!({"matched_preview_digest":matched.digest(),"source_artifacts":preview.candidate().source_revision(),"candidate_artifacts":preview.candidate().parsed().revision}),
        std::slice::from_ref(&source),std::slice::from_ref(&child))?;
    let id = attempt.attempt_id().to_owned();
    matched.verify()?;
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
        return Err("baseline/preview private cleanup failed".into());
    }
    // Controlled uncertain writes exercise recovery evidence; this is not the general publisher.
    fs::write(&source, &candidate)?;
    fs::write(&child,b"(kicad_sch (version 20260306) (generator eeschema) (uuid 88888888-8888-4888-8888-888888888888) (paper A4) (lib_symbols))\n")?;
    let uncertain = SchematicAudit::prepare(&policy)?;
    if !uncertain
        .netlist()
        .components
        .iter()
        .any(|component| component.reference == "R2" && component.value == "AK_RECOVERY_VALUE")
    {
        return Err("native uncertain value edit was not observed".into());
    }
    fs::write(
        output.join("uncertain-native.json"),
        serde_json::to_vec_pretty(&uncertain.evidence()?)?,
    )?;
    let uncertain_private = uncertain.private_root().to_owned();
    drop(uncertain);
    attempt.finish(
        "unknown",
        json!({"controlled_uncertain_schematic_write":true}),
    )?;
    drop(writer);
    let alternate = ExecutionStore::open(&output.join("other-state"))?;
    if alternate.acquire(project)?.ensure_ready().is_ok() {
        return Err("uncertain schematic did not block another state directory".into());
    }
    let status = Command::new("python3")
        .env("PYTHONDONTWRITEBYTECODE", "1")
        .arg(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../scripts/check-recovery.py"))
        .arg(&server)
        .arg(&state)
        .arg(project)
        .arg(&id)
        .arg(output.join("mcp.json"))
        .status()?;
    if !status.success() {
        return Err("production stdio schematic recovery failed".into());
    }
    if child.exists() || snapshot(project)? != originals || snapshot(&config)? != configuration {
        return Err(
            "mixed recovery did not restore exact bytes/absence or changed protected inputs".into(),
        );
    }
    alternate.acquire(project)?.ensure_ready()?;
    let restored = SchematicAudit::prepare(&policy)?;
    if restored.netlist() != &native_before
        || !erc_before.equivalent_to(restored.erc())?
        || restored.observe_pin_connectivity()?.connectivity() != &connectivity_before
    {
        return Err("restored native netlist/ERC/connectivity differs from original".into());
    }
    restored.verify()?;
    fs::write(
        output.join("restored-native.json"),
        serde_json::to_vec_pretty(&restored.evidence()?)?,
    )?;
    let restored_private = restored.private_root().to_owned();
    drop(restored);
    if restored_private.exists() || uncertain_private.exists() {
        return Err("private native snapshots were not removed".into());
    }
    let result = json!({"passed":true,"native_version":"10.0.7","production_stdio_mcp":true,"existing_value_edit_observed_natively":true,
        "new_child_restored_to_absence":true,"complete_native_baseline_restored":true,"source_and_configuration_restored":true,
        "cross_state_directory_write_block":true,"wrong_approval_restart_and_replay_rejected":true,"private_directories_removed":true,
        "gui_launched":false,"general_schematic_publisher_implemented":false});
    fs::write(
        output.join("result.json"),
        serde_json::to_vec_pretty(&result)?,
    )?;
    println!("{}", serde_json::to_string(&result)?);
    Ok(())
}
