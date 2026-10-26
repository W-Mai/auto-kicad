//! Verify explicit routes through production stdio MCP on an owned board copy.
use auto_kicad::{DiscoveryService, IpcDiscoveryService};
use kicad_adapter::lifecycle::{DisposablePcbLifecycle, OwnedPcbnewLauncher};
use serde_json::json;
use std::{env, fs, path::PathBuf, process::Command};
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args = env::args().skip(1).collect::<Vec<_>>();
    if args.len() != 2 {
        return Err("usage: check-copper-route-mcp SERVER NEW_OUTPUT_DIR".into());
    }
    let binary = fs::canonicalize(&args[0])?;
    let output = PathBuf::from(&args[1]);
    fs::create_dir(&output)?;
    let output = fs::canonicalize(output)?;
    let input = output.join("fixture");
    fs::create_dir(&input)?;
    let board = input.join("board.kicad_pcb");
    fs::write(
        &board,
        include_str!("../../../scripts/fixtures/layered-route-anchors.kicad_pcb"),
    )?;
    let mut project: serde_json::Value = serde_json::from_str(include_str!(
        "../../../scripts/fixtures/layered-routing-rules.kicad_pro"
    ))?;
    // Keep the synthetic project's root-sheet index stable across editor loads.
    project["schematic"] = json!({"top_level_sheets": [{
        "filename": "board.kicad_sch", "name": "board",
        "uuid": "00000000-0000-0000-0000-000000000000"
    }]});
    fs::write(
        board.with_extension("kicad_pro"),
        serde_json::to_vec_pretty(&project)?,
    )?;
    let original = [
        fs::read(&board)?,
        fs::read(board.with_extension("kicad_pro"))?,
    ];
    let mut editor = OwnedPcbnewLauncher::new(&output.join("work"))?;
    let copy = editor.create_shadow(&board)?;
    let identity = editor.launch(&copy)?;
    let result: Result<(), Box<dyn std::error::Error>> = (|| {
        let mut ipc = editor.connect_owned()?;
        ipc.get_version()?;
        ipc.save_board(&identity.board_filename)?;
        editor.reload_owned()?;
        let original_items = ipc.capture_existing_board_items(&identity.board_filename)?;
        let original_footprints =
            ipc.capture_footprint_instance_templates(&identity.board_filename)?;
        let status = Command::new("python3")
            .env("PYTHONDONTWRITEBYTECODE", "1")
            .arg(
                PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                    .join("../../scripts/check-copper-route-mcp.py"),
            )
            .arg(&identity.endpoint)
            .arg(copy.board_path())
            .arg(&output)
            .arg(binary)
            .status()?;
        if !status.success() {
            return Err("copper route stdio acceptance failed; see mcp.json".into());
        }
        let committed = IpcDiscoveryService.live_board_observation(&identity.endpoint, None)?;
        editor.reload_owned()?;
        let reopened = IpcDiscoveryService.live_board_observation(&identity.endpoint, None)?;
        if committed != reopened || editor.current_identity()?.pid != identity.pid {
            return Err("route reload changed saved native observation or editor identity".into());
        }
        let footprints = ipc.capture_footprint_instance_templates(&identity.board_filename)?;
        if footprints.len() != original_footprints.len() {
            return Err("original footprint set changed".into());
        }
        for before in &original_footprints {
            let after = footprints
                .iter()
                .find(|fp| fp.metadata().source_uuid == before.metadata().source_uuid)
                .ok_or("original footprint missing")?;
            if !before.matches_saved_instance(after)? {
                return Err("original complete footprint changed".into());
            }
        }
        fs::create_dir(output.join("result"))?;
        for ext in ["kicad_pcb", "kicad_pro"] {
            fs::copy(
                copy.board_path().with_extension(ext),
                output.join(format!("result/board.{ext}")),
            )?;
        }
        fs::write(
            output.join("before-items.pb"),
            original_items.persistent_evidence()?,
        )?;
        fs::write(
            output.join("after-items.pb"),
            ipc.capture_existing_board_items(&identity.board_filename)?
                .persistent_evidence()?,
        )?;
        fs::write(
            output.join("native.json"),
            serde_json::to_vec_pretty(
                &json!({"source_pid":identity.pid,"same_process_reload":true,"native_footprints_preserved":true,"reopened_revision":reopened.snapshot.revision()?.to_string()}),
            )?,
        )?;
        Ok(())
    })();
    editor.close_owned(identity.pid)?;
    editor.cleanup()?;
    if original
        != [
            fs::read(&board)?,
            fs::read(board.with_extension("kicad_pro"))?,
        ]
    {
        return Err("original fixture changed".into());
    }
    fs::write(
        output.join("cleanup.json"),
        serde_json::to_vec_pretty(
            &json!({"owned_source_closed":true,"shadow_cleaned":true,"original_unchanged":true}),
        )?,
    )?;
    result?;
    fs::remove_dir(output.join("work"))?;
    println!("Production stdio copper-route acceptance passed");
    Ok(())
}
