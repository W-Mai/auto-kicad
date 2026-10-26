//! Exercise native geometry inspection through production stdio without source writes.
use kicad_adapter::lifecycle::{DisposablePcbLifecycle, OwnedPcbnewLauncher};
use serde_json::json;
use std::{env, fs, path::PathBuf, process::Command};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args = env::args().skip(1).collect::<Vec<_>>();
    if args.len() != 3 {
        return Err("usage: check-geometry-inspection SERVER SOURCE NEW_OUTPUT".into());
    }
    let binary = fs::canonicalize(&args[0])?;
    let source = fs::canonicalize(&args[1])?;
    let output = PathBuf::from(&args[2]);
    fs::create_dir(&output)?;
    let output = fs::canonicalize(output)?;
    let originals = ["kicad_pcb", "kicad_pro", "kicad_dru", "kicad_sch"].map(|e| {
        let p = source.with_extension(e);
        let b = fs::read(&p).ok();
        (p, b)
    });
    let mut editor = OwnedPcbnewLauncher::new(&output.join("work"))?;
    let copy = editor.create_shadow(&source)?;
    let identity = editor.launch(&copy)?;
    let result: Result<(), Box<dyn std::error::Error>> = (|| {
        let protected = ["kicad_pcb", "kicad_pro", "kicad_dru"].map(|e| {
            let p = copy.board_path().with_extension(e);
            let b = fs::read(&p).ok();
            (p, b)
        });
        let script = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../scripts/check-geometry-inspection.py");
        let status = Command::new("python3")
            .env("PYTHONDONTWRITEBYTECODE", "1")
            .arg(script)
            .arg(&identity.endpoint)
            .arg(copy.board_path())
            .arg(&output)
            .arg(&binary)
            .status()?;
        if !status.success() {
            return Err("geometry stdio acceptance failed; see retained records".into());
        }
        for (p, b) in protected {
            if fs::read(p).ok() != b {
                return Err("inspection changed protected copy files".into());
            }
        }
        fs::write(
            output.join("native.json"),
            serde_json::to_vec_pretty(
                &json!({"pid":identity.pid,"read_only":true,"source":source}),
            )?,
        )?;
        Ok(())
    })();
    editor.close_owned(identity.pid)?;
    editor.cleanup()?;
    for (p, b) in originals {
        if fs::read(p).ok() != b {
            return Err("inspection changed original files".into());
        }
    }
    fs::write(
        output.join("cleanup.json"),
        serde_json::to_vec_pretty(
            &json!({"pid":identity.pid,"closed":true,"source_unchanged":true}),
        )?,
    )?;
    fs::remove_dir(output.join("work"))?;
    result
}
