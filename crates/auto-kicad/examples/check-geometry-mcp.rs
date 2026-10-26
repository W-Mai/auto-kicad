//! Verify geometry approval and native effects through production stdio on an owned PCB.
use kicad_adapter::{
    lifecycle::{DisposablePcbLifecycle, OwnedPcbnewLauncher},
    live_geometry::{RefillDefinitionEvidence, observe_live_geometry},
};
use kicad_ipc::ZoneFillAttempt;
use serde_json::json;
use std::{env, fs, path::PathBuf, process::Command};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args = env::args().skip(1).collect::<Vec<_>>();
    if args.len() != 4 {
        return Err("usage: check-geometry-mcp SERVER SOURCE REQUEST_JSON NEW_OUTPUT".into());
    }
    let binary = fs::canonicalize(&args[0])?;
    let source = fs::canonicalize(&args[1])?;
    let requests = fs::canonicalize(&args[2])?;
    let output = PathBuf::from(&args[3]);
    fs::create_dir(&output)?;
    let output = fs::canonicalize(output)?;
    let originals = ["kicad_pcb", "kicad_pro", "kicad_dru", "kicad_sch"].map(|ext| {
        let p = source.with_extension(ext);
        let b = fs::read(&p).ok();
        (p, b)
    });
    let mut editor = OwnedPcbnewLauncher::new(&output.join("work"))?;
    let copy = editor.create_shadow(&source)?;
    let scripts = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../scripts");
    let python = "/Applications/KiCad/KiCad.app/Contents/Frameworks/Python.framework/Versions/3.9/bin/python3.9";
    let status = Command::new(python)
        .arg(scripts.join("check-refill-definitions.py"))
        .arg("prepare")
        .arg(copy.board_path())
        .arg(&source)
        .arg(&output)
        .status()?;
    if !status.success() {
        editor.cleanup()?;
        return Err("owned geometry preparation failed".into());
    }
    let identity = editor.launch(&copy)?;
    fs::write(
        output.join("owned-editor.json"),
        serde_json::to_vec_pretty(&json!({"pid":identity.pid,"endpoint":identity.endpoint}))?,
    )?;
    let result: Result<(), Box<dyn std::error::Error>> = (|| {
        let mut client = editor.connect_owned()?;
        client.get_version()?;
        client.save_board(&identity.board_filename)?;
        editor.reload_owned()?;
        let before = observe_live_geometry(&mut client, &identity.board_filename)?;
        client = match client.request_zone_refill(&identity.board_filename) {
            ZoneFillAttempt::Returned { client, .. } => client,
            ZoneFillAttempt::NotDispatched { reason, .. } => return Err(reason.to_string().into()),
            ZoneFillAttempt::Uncertain(reader) => return Err(reader.reason().to_string().into()),
        };
        let filled = observe_live_geometry(&mut client, &identity.board_filename)?;
        if RefillDefinitionEvidence::from_observation(&before)?
            != RefillDefinitionEvidence::from_observation(&filled)?
        {
            return Err("baseline fill changed definitions".into());
        }
        client.save_board(&identity.board_filename)?;
        editor.reload_owned()?;
        fs::copy(copy.board_path(), output.join("baseline.kicad_pcb"))?;
        let protected = ["kicad_pro", "kicad_dru"].map(|ext| {
            let p = copy.board_path().with_extension(ext);
            let b = fs::read(&p).ok();
            (p, b)
        });
        let status = Command::new("python3")
            .env("PYTHONDONTWRITEBYTECODE", "1")
            .arg(scripts.join("check-geometry-mcp.py"))
            .arg(&identity.endpoint)
            .arg(copy.board_path())
            .arg(&output)
            .arg(&binary)
            .arg(&requests)
            .status()?;
        for ext in ["kicad_pcb", "kicad_pro", "kicad_dru"] {
            let path = copy.board_path().with_extension(ext);
            if path.exists() {
                fs::copy(path, output.join(format!("result.{ext}")))?;
            }
        }
        if !status.success() {
            return Err("geometry stdio acceptance failed; retained mcp.json".into());
        }
        for (p, b) in protected {
            if fs::read(p).ok() != b {
                return Err("geometry changed protected files".into());
            }
        }
        let before_reload = observe_live_geometry(&mut client, &identity.board_filename)?;
        editor.reload_owned()?;
        let after_reload = observe_live_geometry(&mut client, &identity.board_filename)?;
        if RefillDefinitionEvidence::from_observation(&before_reload)?
            != RefillDefinitionEvidence::from_observation(&after_reload)?
            || before_reload.text.filled_copper()? != after_reload.text.filled_copper()?
            || editor.current_identity()?.pid != identity.pid
        {
            return Err("same-process reload changed complete geometry or copper".into());
        }
        let status = Command::new(python)
            .arg(scripts.join("check-geometry-request.py"))
            .arg(&output)
            .status()?;
        if !status.success() {
            return Err("independent saved intent verification failed".into());
        }
        Ok(())
    })();
    editor.close_owned(identity.pid)?;
    editor.cleanup()?;
    for (p, b) in originals {
        if fs::read(p).ok() != b {
            return Err("original project changed".into());
        }
    }
    fs::write(
        output.join("cleanup.json"),
        serde_json::to_vec_pretty(
            &json!({"pid":identity.pid,"closed":true,"source_unchanged":true,"same_process_reload_verified":result.is_ok()}),
        )?,
    )?;
    if let Err(e) = &result {
        fs::write(
            output.join("failure.json"),
            serde_json::to_vec_pretty(&json!({"reason":e.to_string()}))?,
        )?;
    }
    result
}
