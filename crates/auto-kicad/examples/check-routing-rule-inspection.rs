//! Verify read-only routing rules through the production stdio MCP server.
use kicad_adapter::lifecycle::{DisposablePcbLifecycle, OwnedPcbnewLauncher};
use serde_json::json;
use std::{env, fs, path::PathBuf, process::Command};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args = env::args().skip(1).collect::<Vec<_>>();
    if args.len() != 2 {
        return Err("usage: check-routing-rule-inspection SERVER NEW_OUTPUT_DIR".into());
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
        include_str!("../../../scripts/fixtures/layered-routing-rules.kicad_pcb"),
    )?;
    fs::write(
        board.with_extension("kicad_pro"),
        include_str!("../../../scripts/fixtures/layered-routing-rules.kicad_pro"),
    )?;
    let original = [
        fs::read(&board)?,
        fs::read(board.with_extension("kicad_pro"))?,
    ];
    let mut editor = OwnedPcbnewLauncher::new(&output.join("work"))?;
    let copy = editor.create_shadow(&board)?;
    let identity = editor.launch(&copy)?;
    let result: Result<(), Box<dyn std::error::Error>> = (|| {
        let protected = [fs::read(copy.board_path())?, fs::read(copy.project_path())?];
        let status = Command::new("python3")
            .env("PYTHONDONTWRITEBYTECODE", "1")
            .arg(
                PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                    .join("../../scripts/check-routing-rule-inspection.py"),
            )
            .arg(&identity.endpoint)
            .arg(copy.board_path())
            .arg(&output)
            .arg(binary)
            .status()?;
        if !status.success() {
            return Err("routing rule stdio acceptance failed; see mcp.json".into());
        }
        if protected != [fs::read(copy.board_path())?, fs::read(copy.project_path())?]
            || copy.board_path().with_extension("kicad_dru").exists()
        {
            return Err("read-only source preservation failed".into());
        }
        fs::write(
            output.join("native.json"),
            serde_json::to_vec_pretty(
                &json!({"source_pid":identity.pid,"read_only_sources_restored":true,"original_native_files_unchanged":true}),
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
            &json!({"owned_editor_closed":true,"shadow_cleaned":true,"original_unchanged":true}),
        )?,
    )?;
    result?;
    fs::remove_dir(output.join("work"))?;
    println!("Production stdio routing-rule inspection passed");
    Ok(())
}
