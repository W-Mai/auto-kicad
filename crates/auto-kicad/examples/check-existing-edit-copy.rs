//! Verify a native track edit on an owned copy of a populated PCB.
use auto_kicad::{DiscoveryService, IpcDiscoveryService};
use kicad_adapter::lifecycle::{DisposablePcbLifecycle, OwnedPcbnewLauncher};
use serde_json::json;
use std::{env, fs, path::PathBuf, process::Command};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args = env::args().skip(1).collect::<Vec<_>>();
    if args.len() != 3 {
        return Err(
            "usage: check-existing-edit-copy SOURCE.kicad_pcb SERVER NEW_OUTPUT_DIR".into(),
        );
    }
    let source = fs::canonicalize(&args[0])?;
    let binary = fs::canonicalize(&args[1])?;
    let output = PathBuf::from(&args[2]);
    fs::create_dir(&output)?;
    let output = fs::canonicalize(output)?;
    let originals = ["kicad_pcb", "kicad_pro", "kicad_sch", "kicad_dru"]
        .into_iter()
        .map(|ext| {
            let path = source.with_extension(ext);
            let content = if path.exists() {
                Some(fs::read(&path)?)
            } else {
                None
            };
            Ok((path, content))
        })
        .collect::<Result<Vec<_>, std::io::Error>>()?;
    let mut editor = OwnedPcbnewLauncher::new(&output.join("work"))?;
    let copy = editor.create_shadow(&source)?;
    let identity = editor.launch(&copy)?;
    let result: Result<(), Box<dyn std::error::Error>> = (|| {
        for _ in 0..2 {
            editor
                .connect_owned()?
                .save_board(&identity.board_filename)?;
            editor.reload_owned()?;
        }
        let before = IpcDiscoveryService.live_board_observation(&identity.endpoint, None)?;
        fs::create_dir(output.join("before"))?;
        fs::copy(copy.board_path(), output.join("before/board.kicad_pcb"))?;
        fs::copy(copy.project_path(), output.join("before/board.kicad_pro"))?;
        let script = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../scripts/check-existing-edit-copy.py");
        let status = Command::new("python3")
            .env("PYTHONDONTWRITEBYTECODE", "1")
            .arg(script)
            .arg(&identity.endpoint)
            .arg(copy.board_path())
            .arg(&output)
            .arg(binary)
            .status()?;
        if !status.success() {
            return Err(
                "populated-copy stdio acceptance failed; inspect mcp.json and stdio.log".into(),
            );
        }
        let committed = IpcDiscoveryService.live_board_observation(&identity.endpoint, None)?;
        editor.reload_owned()?;
        let reopened = IpcDiscoveryService.live_board_observation(&identity.endpoint, None)?;
        if committed != reopened || editor.current_identity()?.pid != identity.pid {
            return Err("saved reload changed snapshot or source editor PID".into());
        }
        for (path, content) in &originals {
            let actual = if path.exists() {
                Some(fs::read(path)?)
            } else {
                None
            };
            if actual != *content {
                return Err("original source files changed".into());
            }
        }
        fs::create_dir(output.join("result"))?;
        fs::copy(copy.board_path(), output.join("result/board.kicad_pcb"))?;
        fs::copy(copy.project_path(), output.join("result/board.kicad_pro"))?;
        for ext in ["kicad_sch", "kicad_dru"] {
            let path = copy.board_path().with_extension(ext);
            if path.is_file() {
                fs::copy(path, output.join(format!("result/board.{ext}")))?;
            }
        }
        fs::write(
            output.join("snapshot.json"),
            serde_json::to_vec_pretty(
                &json!({"before":before.snapshot,"after":reopened.snapshot,"same_editor_pid":identity.pid,"saved_reload_matches":true,"original_source_unchanged":true}),
            )?,
        )?;
        Ok(())
    })();
    editor.close_owned(identity.pid)?;
    editor.cleanup()?;
    result?;
    fs::remove_dir(output.join("work"))?;
    fs::write(
        output.join("cleanup.json"),
        serde_json::to_vec_pretty(
            &json!({"owned_editor_closed":true,"owned_shadow_cleaned":true,"result_retained":true}),
        )?,
    )?;
    println!("Populated-copy edit, native reload, original preservation and cleanup passed");
    Ok(())
}
