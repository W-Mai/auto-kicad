//! Exercise real geometry response loss, process interruption and approved recovery.
use auto_kicad::{DiscoveryService, IpcDiscoveryService};
use kicad_adapter::{
    lifecycle::{DisposablePcbLifecycle, OwnedPcbnewLauncher},
    live_geometry::{RefillDefinitionEvidence, observe_live_geometry},
};
use kicad_checks::{CheckResult, DrcPolicy, run_pcb_drc};
use kicad_ipc::ZoneFillAttempt;
use serde_json::{Value, json};
use std::{
    env, fs,
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    time::{Duration, Instant},
};

struct Relay(Child, PathBuf);
impl Relay {
    fn stop(&mut self) -> Result<(), Box<dyn std::error::Error>> {
        fs::write(self.1.join("stop"), [])?;
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            if let Some(status) = self.0.try_wait()? {
                return if status.success() {
                    Ok(())
                } else {
                    Err("relay failed".into())
                };
            }
            if Instant::now() >= deadline {
                return Err("relay did not exit".into());
            }
            std::thread::sleep(Duration::from_millis(20));
        }
    }
}
impl Drop for Relay {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn python(scripts: &Path, name: &str, args: &[&str]) -> Result<(), Box<dyn std::error::Error>> {
    if !Command::new("python3")
        .env("PYTHONDONTWRITEBYTECODE", "1")
        .arg(scripts.join(name))
        .args(args)
        .status()?
        .success()
    {
        return Err(format!("{name} failed").into());
    }
    Ok(())
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args = env::args().skip(1).collect::<Vec<_>>();
    if args.len() != 4 {
        return Err("usage: check-geometry-recovery SERVER RELAY SOURCE NEW_OUTPUT".into());
    }
    let binary = fs::canonicalize(&args[0])?;
    let relay_binary = fs::canonicalize(&args[1])?;
    let source = fs::canonicalize(&args[2])?;
    fs::create_dir(&args[3])?;
    let output = fs::canonicalize(&args[3])?;
    let originals = ["kicad_pcb", "kicad_pro", "kicad_dru", "kicad_sch"].map(|ext| {
        let path = source.with_extension(ext);
        let bytes = fs::read(&path).ok();
        (path, bytes)
    });
    let scripts = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../scripts");
    let native_python = "/Applications/KiCad/KiCad.app/Contents/Frameworks/Python.framework/Versions/3.9/bin/python3.9";
    let mut editor = OwnedPcbnewLauncher::new(&output.join("work"))?;
    let copy = editor.create_shadow(&source)?;
    if !Command::new(native_python)
        .arg(scripts.join("check-refill-definitions.py"))
        .arg("prepare")
        .arg(copy.board_path())
        .arg(&source)
        .arg(&output)
        .status()?
        .success()
    {
        return Err("fixture preparation failed".into());
    }
    let mut identity = editor.launch(&copy)?;
    let mut pids = vec![identity.pid];
    let result: Result<(), Box<dyn std::error::Error>> = (|| {
        let mut client = editor.connect_owned()?;
        client.get_version()?;
        client.save_board(&identity.board_filename)?;
        editor.reload_owned()?;
        client = match client.request_zone_refill(&identity.board_filename) {
            ZoneFillAttempt::Returned { client, .. } => client,
            _ => return Err("baseline refill did not return".into()),
        };
        client.save_board(&identity.board_filename)?;
        editor.reload_owned()?;
        let initial = IpcDiscoveryService.live_board_observation(&identity.endpoint, None)?;
        let geometry = observe_live_geometry(&mut client, &identity.board_filename)?;
        let definitions = RefillDefinitionEvidence::from_observation(&geometry)?;
        let copper = geometry.text.filled_copper()?;
        let baseline_bytes = fs::read(copy.board_path())?;
        fs::write(output.join("baseline.kicad_pcb"), &baseline_bytes)?;
        let policy = DrcPolicy {
            allowed_root: copy.root().to_path_buf(),
            timeout: Duration::from_secs(60),
        };
        let CheckResult::Completed(baseline_drc) = run_pcb_drc(copy.board_path(), &policy) else {
            return Err("baseline DRC failed".into());
        };
        let mut cases = Vec::new();
        for mode in ["corrupt_saved_reply", "interrupt_before_write"] {
            let case = output.join(mode);
            fs::create_dir(&case)?;
            let control = case.join("relay");
            let child = Command::new(&relay_binary)
                .arg(&identity.endpoint)
                .arg(copy.root())
                .arg(&control)
                .stdout(Stdio::null())
                .stderr(fs::File::create(case.join("relay.log"))?)
                .spawn()?;
            let mut relay = Relay(child, control.clone());
            let deadline = Instant::now() + Duration::from_secs(10);
            while !control.join("ready.json").exists() {
                if relay.0.try_wait()?.is_some() || Instant::now() >= deadline {
                    return Err("relay startup failed".into());
                }
                std::thread::sleep(Duration::from_millis(20));
            }
            let ready: Value = serde_json::from_slice(&fs::read(control.join("ready.json"))?)?;
            fs::write(
                case.join("owned.json"),
                serde_json::to_vec_pretty(&json!({"editor_pid":identity.pid,"relay":ready}))?,
            )?;
            let fault = python(
                &scripts,
                "check-geometry-recovery.py",
                &[
                    mode,
                    ready["endpoint"].as_str().ok_or("relay endpoint missing")?,
                    &identity.endpoint,
                    copy.board_path().to_str().ok_or("path")?,
                    case.to_str().ok_or("path")?,
                    binary.to_str().ok_or("path")?,
                    control.to_str().ok_or("path")?,
                ],
            );
            relay.stop()?;
            fault?;
            let case_result: Value = serde_json::from_slice(&fs::read(case.join("result.json"))?)?;
            editor.close_owned(identity.pid)?;
            // Only remove the exact disposable board/project locks after waiting for their owned child.
            for path in [copy.board_path(), copy.project_path()] {
                let lock = path.with_file_name(format!(
                    "~{}.lck",
                    path.file_name().ok_or("file name")?.to_string_lossy()
                ));
                if lock.exists() {
                    fs::remove_file(lock)?;
                }
            }
            python(
                &scripts,
                "check-recovery.py",
                &[
                    binary.to_str().ok_or("path")?,
                    case.join("state").to_str().ok_or("path")?,
                    copy.root().to_str().ok_or("path")?,
                    case_result["attempt_id"].as_str().ok_or("attempt")?,
                    case.join("recovery.json").to_str().ok_or("path")?,
                ],
            )?;
            if fs::read(copy.board_path())? != baseline_bytes
                || copy.root().join(".auto-kicad-geometry-pending").exists()
            {
                return Err("recovery bytes or marker mismatch".into());
            }
            identity = editor.reopen()?;
            pids.push(identity.pid);
            client = editor.connect_owned()?;
            client.get_version()?;
            let restored = IpcDiscoveryService.live_board_observation(&identity.endpoint, None)?;
            let restored_geometry = observe_live_geometry(&mut client, &identity.board_filename)?;
            if restored.snapshot != initial.snapshot
                || RefillDefinitionEvidence::from_observation(&restored_geometry)? != definitions
                || restored_geometry.text.filled_copper()? != copper
            {
                return Err("restored native content mismatch".into());
            }
            let CheckResult::Completed(restored_drc) = run_pcb_drc(copy.board_path(), &policy)
            else {
                return Err("restored DRC failed".into());
            };
            if serde_json::to_value(&restored_drc)? != serde_json::to_value(&baseline_drc)? {
                return Err("restored DRC mismatch".into());
            }
            cases.push(json!({"fault":case_result,"reopened_pid":identity.pid,"native_content_restored":true,"drc_restored":true}));
        }
        let normal = output.join("normal");
        fs::create_dir(&normal)?;
        python(
            &scripts,
            "check-geometry-recovery.py",
            &[
                "normal",
                &identity.endpoint,
                &identity.endpoint,
                copy.board_path().to_str().ok_or("path")?,
                normal.to_str().ok_or("path")?,
                binary.to_str().ok_or("path")?,
                normal.to_str().ok_or("path")?,
            ],
        )?;
        let before = observe_live_geometry(&mut client, &identity.board_filename)?;
        editor.reload_owned()?;
        let after = observe_live_geometry(&mut client, &identity.board_filename)?;
        if RefillDefinitionEvidence::from_observation(&before)?
            != RefillDefinitionEvidence::from_observation(&after)?
            || before.text.filled_copper()? != after.text.filled_copper()?
        {
            return Err("normal saved reload mismatch".into());
        }
        if !Command::new(native_python)
            .arg(scripts.join("check-geometry-request.py"))
            .arg(&normal)
            .status()?
            .success()
        {
            return Err("normal independent intent failed".into());
        }
        fs::write(
            output.join("result.json"),
            serde_json::to_vec_pretty(
                &json!({"passed":true,"kicad_version":"10.0.7","cases":cases,"subsequent_geometry_commit":true,"same_process_final_reload":true,"source_pids":pids}),
            )?,
        )?;
        Ok(())
    })();
    for ext in ["kicad_pcb", "kicad_pro", "kicad_dru"] {
        let path = copy.board_path().with_extension(ext);
        if path.exists() {
            fs::copy(path, output.join(format!("result.{ext}")))?;
        }
    }
    if editor.editor_is_open() {
        editor.close_owned(identity.pid)?;
    }
    editor.cleanup()?;
    for (path, bytes) in originals {
        if fs::read(path).ok() != bytes {
            return Err("original source changed".into());
        }
    }
    fs::write(
        output.join("cleanup.json"),
        serde_json::to_vec_pretty(
            &json!({"source_pids":pids,"owned_closed":true,"source_unchanged":true,"passed":result.is_ok()}),
        )?,
    )?;
    if let Err(error) = &result {
        fs::write(
            output.join("failure.json"),
            serde_json::to_vec_pretty(&json!({"reason":error.to_string()}))?,
        )?;
    }
    result
}
