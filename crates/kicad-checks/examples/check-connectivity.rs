//! Read closed native connectivity from one saved board without opening an editor.
use kicad_checks::{DrcPolicy, connectivity::check_saved_connectivity};
use std::path::PathBuf;
use std::time::Duration;

fn main() -> Result<(), String> {
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    if args.len() != 1 {
        return Err("usage: check-connectivity BOARD.kicad_pcb".into());
    }
    let board = PathBuf::from(&args[0]);
    let parent = board.parent().ok_or("board path has no parent")?;
    let evidence = check_saved_connectivity(
        &board,
        &DrcPolicy {
            allowed_root: parent.to_owned(),
            timeout: Duration::from_secs(60),
        },
    )
    .map_err(|error| format!("{error:?}"))?;
    println!(
        "{}",
        serde_json::to_string_pretty(&evidence).map_err(|e| e.to_string())?
    );
    Ok(())
}
