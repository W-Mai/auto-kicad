//! Exercise an installed stdio server against an owned copy of a saved PCB.
use auto_kicad::{DiscoveryService, IpcDiscoveryService};
use kicad_adapter::lifecycle::{DisposablePcbLifecycle, OwnedPcbnewLauncher};
use serde_json::json;
use std::{env, fs, path::PathBuf, process::Command};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args = env::args().skip(1).collect::<Vec<_>>();
    if args.len() != 3 {
        return Err(
            "usage: check-board-copy SOURCE.kicad_pcb INSTALLED_SERVER NEW_OUTPUT_DIR".into(),
        );
    }
    let source = fs::canonicalize(&args[0])?;
    let binary = fs::canonicalize(&args[1])?;
    let output = PathBuf::from(&args[2]);
    fs::create_dir(&output)?;
    let output = fs::canonicalize(output)?;
    let original = fs::read(&source)?;
    let original_project = fs::read(source.with_extension("kicad_pro"))?;
    let mut editor = OwnedPcbnewLauncher::new(&output.join("work"))?;
    let copy = editor.create_shadow(&source)?;
    let identity = editor.launch(&copy)?;
    let result: Result<(), Box<dyn std::error::Error>> = (|| {
        let before = IpcDiscoveryService.live_board_observation(&identity.endpoint, None)?;
        let script =
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../scripts/check-board-copy.py");
        let status = Command::new("python3")
            .env("PYTHONDONTWRITEBYTECODE", "1")
            .arg(script)
            .arg(&identity.endpoint)
            .arg(copy.board_path())
            .arg(&output)
            .arg(binary)
            .status()?;
        if !status.success() {
            return Err("stdio acceptance failed; inspect mcp.json and stdio.log".into());
        }
        let committed = IpcDiscoveryService.live_board_observation(&identity.endpoint, None)?;
        editor.reload_owned()?;
        let reopened = IpcDiscoveryService.live_board_observation(&identity.endpoint, None)?;
        if committed != reopened || editor.current_identity()?.pid != identity.pid {
            return Err("saved reload changed snapshot or editor PID".into());
        }
        if before.snapshot.native_footprints != reopened.snapshot.native_footprints
            || before.snapshot.native_routing != reopened.snapshot.native_routing
            || before.snapshot.native_stackup != reopened.snapshot.native_stackup
            || reopened.snapshot.board.graphics.len() != before.snapshot.board.graphics.len() + 1
            || !before
                .snapshot
                .board
                .graphics
                .iter()
                .all(|g| reopened.snapshot.board.graphics.contains(g))
        {
            return Err("commit changed existing geometry, networks or stackup".into());
        }
        if fs::read(&source)? != original
            || fs::read(source.with_extension("kicad_pro"))? != original_project
        {
            return Err("original source files changed".into());
        }
        fs::create_dir(output.join("result"))?;
        fs::copy(copy.board_path(), output.join("result/board.kicad_pcb"))?;
        fs::copy(copy.project_path(), output.join("result/board.kicad_pro"))?;
        fs::write(
            output.join("snapshot.json"),
            serde_json::to_vec_pretty(&json!({
                "before": before.snapshot, "after": reopened.snapshot,
                "same_editor_pid": identity.pid, "saved_reload_matches": true,
                "original_source_unchanged": true
            }))?,
        )?;
        Ok(())
    })();
    editor.close_owned(identity.pid)?;
    editor.cleanup()?;
    result?;
    fs::remove_dir(output.join("work"))?;
    println!(
        "Saved reload, preservation of existing board objects and owned-editor cleanup passed"
    );
    Ok(())
}
