//! Verify general footprint creation through production stdio on an owned fixture.
use auto_kicad::{DiscoveryService, IpcDiscoveryService};
use kicad_adapter::lifecycle::{DisposablePcbLifecycle, OwnedPcbnewLauncher};
use serde_json::json;
use std::{env, fs, path::PathBuf, process::Command};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args = env::args().skip(1).collect::<Vec<_>>();
    if args.len() != 2 {
        return Err("usage: check-general-footprints SERVER NEW_OUTPUT_DIR".into());
    }
    let binary = fs::canonicalize(&args[0])?;
    let output = PathBuf::from(&args[1]);
    fs::create_dir(&output)?;
    let output = fs::canonicalize(output)?;
    let fixture = output.join("fixture");
    fs::create_dir(&fixture)?;
    fs::write(
        fixture.join("board.kicad_pcb"),
        include_str!("../../../scripts/fixtures/general-footprints.kicad_pcb"),
    )?;
    fs::write(
        fixture.join("board.kicad_pro"),
        r#"{"board":{"design_settings":{"rules":{"min_through_hole_diameter":0.2}}}}"#,
    )?;
    let script =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../scripts/check-general-footprints.py");
    let mut editor = OwnedPcbnewLauncher::new(&output.join("work"))?;
    let copy = editor.create_shadow(&fixture.join("board.kicad_pcb"))?;
    let generated = Command::new("python3")
        .env("PYTHONDONTWRITEBYTECODE", "1")
        .arg(&script)
        .arg("fixture")
        .arg(copy.root())
        .status()?;
    if !generated.success() {
        return Err("fixture generation failed".into());
    }
    let identity = editor.launch(&copy)?;
    let result: Result<(), Box<dyn std::error::Error>> = (|| {
        for _ in 0..2 {
            editor
                .connect_owned()?
                .save_board(&identity.board_filename)?;
            editor.reload_owned()?;
        }
        let protected = [
            copy.project_path().to_path_buf(),
            copy.board_path().with_extension("kicad_sch"),
            copy.root().join("fp-lib-table"),
        ]
        .into_iter()
        .map(|p| fs::read(&p).map(|bytes| (p, bytes)))
        .collect::<Result<Vec<_>, _>>()?;
        let status = Command::new("python3")
            .env("PYTHONDONTWRITEBYTECODE", "1")
            .arg(&script)
            .arg("check")
            .arg(&identity.endpoint)
            .arg(copy.board_path())
            .arg(&output)
            .arg(binary)
            .status()?;
        if !status.success() {
            return Err("general footprint stdio acceptance failed; see mcp.json".into());
        }
        let committed = IpcDiscoveryService.live_board_observation(&identity.endpoint, None)?;
        let native = editor
            .connect_owned()?
            .capture_footprint_instance_templates(&identity.board_filename)?;
        editor.reload_owned()?;
        let reopened = IpcDiscoveryService.live_board_observation(&identity.endpoint, None)?;
        let reloaded = editor
            .connect_owned()?
            .capture_footprint_instance_templates(&identity.board_filename)?;
        if committed != reopened
            || editor.current_identity()?.pid != identity.pid
            || native.len() != reloaded.len()
        {
            return Err("saved reload changed native board or editor identity".into());
        }
        let thermal = native
            .iter()
            .find(|fp| fp.metadata().library_identifier.nickname == "Package_DFN_QFN")
            .ok_or("official thermal QFN missing")?;
        let properties = thermal.pad_fabrication_properties()?;
        if properties.len() != 52 || properties.values().filter(|value| **value == 6).count() != 11
        {
            return Err("official thermal QFN fabrication properties changed".into());
        }
        for before in &native {
            let after = reloaded
                .iter()
                .find(|after| after.metadata().source_uuid == before.metadata().source_uuid)
                .ok_or("footprint disappeared on reload")?;
            if !before.matches_saved_instance(after)? {
                return Err("saved reload changed full native footprint payload".into());
            }
        }
        for (path, bytes) in &protected {
            if fs::read(path)? != *bytes {
                return Err("protected fixture changed".into());
            }
        }
        fs::create_dir(output.join("result"))?;
        for ext in ["kicad_pcb", "kicad_pro", "kicad_sch"] {
            fs::copy(
                copy.board_path().with_extension(ext),
                output.join(format!("result/board.{ext}")),
            )?;
        }
        fs::copy(
            copy.root().join("fp-lib-table"),
            output.join("result/fp-lib-table"),
        )?;
        for library in ["BoardParts.pretty", "QfnParts.pretty"] {
            fs::create_dir(output.join("result").join(library))?;
            for file in fs::read_dir(copy.root().join(library))? {
                let file = file?;
                fs::copy(
                    file.path(),
                    output.join("result").join(library).join(file.file_name()),
                )?;
            }
        }
        fs::write(
            output.join("native.json"),
            serde_json::to_vec_pretty(
                &json!({"same_editor_pid":identity.pid,"native_reload_matches":true,"official_qfn_physical_pads":52,"official_qfn_heatsink_pads":11,"protected_files_unchanged":true,"snapshot":reopened.snapshot}),
            )?,
        )?;
        Ok(())
    })();
    editor.close_owned(identity.pid)?;
    editor.cleanup()?;
    fs::write(
        output.join("cleanup.json"),
        serde_json::to_vec_pretty(
            &json!({"owned_editor_closed":true,"owned_shadow_cleaned":true}),
        )?,
    )?;
    result?;
    fs::remove_dir(output.join("work"))?;
    println!("General footprint stdio creation and complete native reload passed");
    Ok(())
}
