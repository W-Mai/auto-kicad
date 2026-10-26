//! Check complete saved native geometry without opening a PCB Editor.
use kicad_checks::{DrcPolicy, outline::check_saved_outline};
use std::{env, path::PathBuf, time::Duration};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let board = PathBuf::from(env::args().nth(1).ok_or("expected saved PCB path")?);
    let root = board.parent().ok_or("missing parent")?.to_owned();
    let result = check_saved_outline(
        &board,
        &DrcPolicy {
            allowed_root: root,
            timeout: Duration::from_secs(60),
        },
    );
    println!("{}", serde_json::to_string_pretty(&result)?);
    if result.is_err() {
        std::process::exit(1);
    }
    Ok(())
}
