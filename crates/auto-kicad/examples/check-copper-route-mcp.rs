//! Verify explicit routes through production stdio MCP on an owned board copy.
use auto_kicad::{DiscoveryService, IpcDiscoveryService};
use kicad_adapter::lifecycle::{DisposablePcbLifecycle, OwnedPcbnewLauncher};
use serde_json::json;
use std::{env, fs, path::PathBuf, process::Command};
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args = env::args().skip(1).collect::<Vec<_>>();
    if !(args.len() == 2
        || (args.len() == 3 && args[2] == "--boundary")
        || (args.len() == 4 && args[2] == "--zoned"))
    {
        return Err(
            "usage: check-copper-route-mcp SERVER NEW_OUTPUT_DIR [--boundary | --zoned SOURCE_PCB]"
                .into(),
        );
    }
    let zoned = args.len() == 4;
    let script = if zoned {
        "check-zoned-additive-mcp.py"
    } else if args.len() == 3 {
        "check-route-boundary.py"
    } else {
        "check-copper-route-mcp.py"
    };
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
    let provided = if zoned {
        let source = fs::canonicalize(&args[3])?;
        let pcb = fs::read(&source)?;
        let pro = fs::read(source.with_extension("kicad_pro"))?;
        fs::write(&board, &pcb)?;
        fs::write(board.with_extension("kicad_pro"), &pro)?;
        Some((source, pcb, pro))
    } else {
        None
    };
    let original = [
        fs::read(&board)?,
        fs::read(board.with_extension("kicad_pro"))?,
    ];
    let mut editor = OwnedPcbnewLauncher::new(&output.join("work"))?;
    let copy = editor.create_shadow(&board)?;
    let identity = editor.launch(&copy)?;
    fs::write(
        output.join("owned-editor.json"),
        serde_json::to_vec_pretty(&json!({"pid":identity.pid,"endpoint":identity.endpoint}))?,
    )?;
    let result: Result<(), Box<dyn std::error::Error>> = (|| {
        let mut ipc = editor.connect_owned()?;
        ipc.get_version()?;
        if zoned {
            ipc = match ipc.request_zone_refill(&identity.board_filename) {
                kicad_ipc::ZoneFillAttempt::Returned { client, .. } => client,
                kicad_ipc::ZoneFillAttempt::NotDispatched { reason, .. } => {
                    return Err(reason.into());
                }
                kicad_ipc::ZoneFillAttempt::Uncertain(reader) => {
                    return Err(reader.reason().into());
                }
            };
        }
        ipc.save_board(&identity.board_filename)?;
        editor.reload_owned()?;
        let original_items = ipc.capture_existing_board_items(&identity.board_filename)?;
        let original_observation =
            IpcDiscoveryService.live_board_observation(&identity.endpoint, None)?;
        let original_footprints =
            ipc.capture_footprint_instance_templates(&identity.board_filename)?;
        let status = Command::new("python3")
            .env("PYTHONDONTWRITEBYTECODE", "1")
            .arg(
                PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                    .join("../../scripts")
                    .join(script),
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
        let original_json = serde_json::to_value(&original_observation.snapshot)?;
        let reopened_json = serde_json::to_value(&reopened.snapshot)?;
        let mut added_ids = Vec::new();
        for (container, collection, key) in [
            ("native_routing", "tracks", "source_uuid"),
            ("native_routing", "vias", "source_uuid"),
            ("board", "graphics", "graphic_id"),
        ] {
            let original = original_json[container][collection]
                .as_array()
                .ok_or("missing original items")?;
            for item in reopened_json[container][collection]
                .as_array()
                .ok_or("missing saved items")?
            {
                if !original.iter().any(|old| old[key] == item[key]) {
                    added_ids.push(
                        item[key]
                            .as_str()
                            .ok_or("missing created identity")?
                            .to_owned(),
                    );
                }
            }
        }
        if !original_items.matches_after_additions(
            &ipc.capture_existing_board_items(&identity.board_filename)?,
            &added_ids,
        )? {
            return Err("original complete board items changed".into());
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
                &json!({"source_pid":identity.pid,"same_process_reload":true,"native_footprints_preserved":true,"native_original_items_preserved":true,"added_items":added_ids.len(),"reopened_revision":reopened.snapshot.revision()?.to_string()}),
            )?,
        )?;
        Ok(())
    })();
    // Preserve saved evidence even when the native result is uncertain; never save or reload here.
    let retained = output.join("retained");
    fs::create_dir(&retained)?;
    for extension in ["kicad_pcb", "kicad_pro", "kicad_dru"] {
        let path = copy.board_path().with_extension(extension);
        if path.is_file() {
            fs::copy(&path, retained.join(format!("board.{extension}")))?;
        }
    }
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
    if let Some((source, pcb, pro)) = provided
        && (fs::read(&source)? != pcb || fs::read(source.with_extension("kicad_pro"))? != pro)
    {
        return Err("provided zoned source changed".into());
    }
    result?;
    fs::remove_dir(output.join("work"))?;
    println!("Production stdio copper-route acceptance passed");
    Ok(())
}
