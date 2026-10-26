//! Observe complete native zones, graphics and live serialization on an owned project copy.
use kicad_adapter::lifecycle::{DisposablePcbLifecycle, OwnedPcbnewLauncher};
use serde_json::json;
use std::{env, fs, path::PathBuf, process::Command};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args = env::args().skip(1).collect::<Vec<_>>();
    if args.len() != 2 {
        return Err("usage: check-native-geometry SOURCE.kicad_pcb NEW_OUTPUT_DIR".into());
    }
    let source = fs::canonicalize(&args[0])?;
    let output = PathBuf::from(&args[1]);
    fs::create_dir(&output)?;
    let output = fs::canonicalize(output)?;
    let originals = ["kicad_pcb", "kicad_pro", "kicad_dru", "kicad_sch"]
        .into_iter()
        .map(|ext| {
            let file = source.with_extension(ext);
            let bytes = if file.is_file() {
                Some(fs::read(&file)?)
            } else {
                None
            };
            Ok((file, bytes))
        })
        .collect::<Result<Vec<_>, std::io::Error>>()?;
    let mut editor = OwnedPcbnewLauncher::new(&output.join("work"))?;
    let copy = editor.create_shadow(&source)?;
    let identity = editor.launch(&copy)?;
    let result: Result<(), Box<dyn std::error::Error>> = (|| {
        let saved = fs::read(copy.board_path())?;
        let mut ipc = editor.connect_owned()?;
        let version = ipc.get_version()?;
        let before = ipc.capture_board_geometry(&identity.board_filename)?;
        let text = ipc.live_board_text(&identity.board_filename)?;
        let after = ipc.capture_board_geometry(&identity.board_filename)?;
        if before != after || saved != fs::read(copy.board_path())? {
            return Err("read-only observation changed geometry or saved bytes".into());
        }
        fs::write(output.join("live.kicad_pcb"), &text)?;
        fs::write(output.join("geometry.pb"), before.evidence_bytes()?)?;
        let zones = before.zones().iter().map(|(id, zone)| json!({
            "id":id, "native_locked":zone.locked.value(), "kind":zone.type_.value(),
            "layers":zone.layers.iter().map(|l| l.value()).collect::<Vec<_>>(),
            "filled":zone.filled, "outline_polygons":zone.outline.as_ref().map(|p| p.polygons.len()),
            "filled_layers":zone.filled_polygons.iter().map(|p| json!({"layer":p.layer.value(), "polygons":p.shapes.as_ref().map(|s| s.polygons.len()), "nodes":p.shapes.as_ref().map(|s| s.polygons.iter().map(|p| p.outline.iter().chain(p.holes.iter()).map(|r| r.nodes.len()).sum::<usize>()).sum::<usize>())})).collect::<Vec<_>>()
        })).collect::<Vec<_>>();
        let shapes = before.shapes().iter().map(|(id, shape)| json!({"id":id,"layer":shape.layer.value(),"geometry":format!("{:?}",shape.shape.as_ref().and_then(|s|s.geometry.as_ref()).map(std::mem::discriminant))})).collect::<Vec<_>>();
        fs::write(
            output.join("native.json"),
            serde_json::to_vec_pretty(
                &json!({"kicad_version":version.full_version,"source_pid":identity.pid,"source":source,"zones":zones,"shapes":shapes,"live_text_bytes":text.len(),"stable_read":true,"saved_source_unchanged":true}),
            )?,
        )?;
        let script = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../scripts/check-native-geometry.py");
        let status = Command::new("/Applications/KiCad/KiCad.app/Contents/Frameworks/Python.framework/Versions/3.9/bin/python3.9")
            .env("PYTHONDONTWRITEBYTECODE", "1").arg(script).arg(&output).status()?;
        if !status.success() {
            return Err("native-text geometry cross-check failed".into());
        }
        Ok(())
    })();
    editor.close_owned(identity.pid)?;
    editor.cleanup()?;
    for (file, bytes) in &originals {
        let actual = if file.is_file() {
            Some(fs::read(file)?)
        } else {
            None
        };
        if actual != *bytes {
            return Err("original project files changed".into());
        }
    }
    fs::remove_dir(output.join("work"))?;
    fs::write(
        output.join("cleanup.json"),
        serde_json::to_vec_pretty(
            &json!({"owned_editor_closed":true,"working_copy_removed":true,"original_files_unchanged":true}),
        )?,
    )?;
    result?;
    println!("Native geometry and live-text observation passed");
    Ok(())
}
