//! Observe complete native zones, graphics and live serialization on an owned project copy.
use kicad_adapter::lifecycle::{DisposablePcbLifecycle, OwnedPcbnewLauncher};
use kicad_adapter::live_geometry::{LiveBoardText, observe_live_geometry};
use kicad_ipc::{
    ClientConfig, IpcError, KiCadClient, KiCadTransport, NngTransport,
    envelope::{ApiRequest, ApiResponse},
};
use serde_json::json;
use std::{env, fs, path::PathBuf, process::Command};

use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

// Faults alter only received evidence, never the owned board or editor.
struct EvidenceFault {
    transport: NngTransport,
    token_fault: bool,
    versions: usize,
    injected: Arc<AtomicBool>,
}
impl KiCadTransport for EvidenceFault {
    fn request(&mut self, request: ApiRequest) -> Result<ApiResponse, IpcError> {
        let kind = request
            .message
            .as_ref()
            .map(|m| m.type_url.clone())
            .unwrap_or_default();
        let mut reply = self.transport.request(request)?;
        if kind.ends_with(".GetVersion") {
            self.versions += 1;
            if self.token_fault && self.versions == 2 {
                reply
                    .header
                    .as_mut()
                    .ok_or(IpcError::MissingResponseMessage)?
                    .kicad_token
                    .push_str("-injected-drift");
                self.injected.store(true, Ordering::Relaxed);
            }
        }
        if !self.token_fault
            && kind.ends_with(".SaveDocumentToString")
            && !self.injected.load(Ordering::Relaxed)
        {
            let payload = &mut reply
                .message
                .as_mut()
                .ok_or(IpcError::MissingResponseMessage)?
                .value;
            let needle = b"(zone\n\t\t(locked yes)";
            if let Some(offset) = payload
                .windows(needle.len())
                .position(|bytes| bytes == needle)
            {
                // Equal byte lengths preserve the protobuf envelope's string length.
                payload[offset..offset + needle.len()].copy_from_slice(b"(zone\n\t\t(locked no )");
                self.injected.store(true, Ordering::Relaxed);
            }
        }
        Ok(reply)
    }
}
fn reject_injected_drift(
    endpoint: &str,
    filename: &str,
    token_fault: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    let config = ClientConfig::new(endpoint, "geometry-evidence-fault")?;
    let injected = Arc::new(AtomicBool::new(false));
    let transport = EvidenceFault {
        transport: NngTransport::connect(config.clone())?,
        token_fault,
        versions: 0,
        injected: injected.clone(),
    };
    let mut client = KiCadClient::new(transport, config);
    client.get_version()?;
    let error = observe_live_geometry(&mut client, filename)
        .err()
        .ok_or("injected evidence drift was accepted")?;
    let expected = if token_fault {
        "live geometry instance changed during version query"
    } else {
        "live geometry, locks or document changed during observation"
    };
    if !injected.load(Ordering::Relaxed) || error != expected {
        return Err(format!("unexpected evidence drift result: {error}").into());
    }
    Ok(())
}

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
        let observed = observe_live_geometry(&mut ipc, &identity.board_filename)?;
        let before = observed.native;
        let text = observed.text.raw_text();
        let missing = LiveBoardText::parse("(kicad_pcb (version 20260206))")?;
        if !before.zones().is_empty() && missing.validate_native_zones(&before).is_ok() {
            return Err("missing live zone identity was accepted".into());
        }
        let after = ipc.capture_board_geometry(&identity.board_filename)?;
        if before != after || saved != fs::read(copy.board_path())? {
            return Err("read-only observation changed geometry or saved bytes".into());
        }
        reject_injected_drift(&identity.endpoint, &identity.board_filename, true)?;
        let lock_fault_tested = observed.text.zones().values().any(|zone| zone.locked);
        if lock_fault_tested {
            reject_injected_drift(&identity.endpoint, &identity.board_filename, false)?;
        }
        if observe_live_geometry(&mut ipc, &identity.board_filename)?
            .text
            .raw_text()
            != text
        {
            return Err("injected read-only faults changed actual editor text".into());
        }
        fs::write(output.join("live.kicad_pcb"), text)?;
        fs::write(output.join("geometry.pb"), before.evidence_bytes()?)?;
        let zones = before.zones().iter().map(|(id, zone)| json!({
            "id":id, "native_locked":zone.locked.value(), "text_locked":observed.text.zones()[id].locked, "text_zone_digest":observed.text.zones()[id].structure_digest, "kind":zone.type_.value(),
            "layers":zone.layers.iter().map(|l| l.value()).collect::<Vec<_>>(),
            "filled":zone.filled, "outline_polygons":zone.outline.as_ref().map(|p| p.polygons.len()),
            "filled_layers":zone.filled_polygons.iter().map(|p| json!({"layer":p.layer.value(), "polygons":p.shapes.as_ref().map(|s| s.polygons.len()), "nodes":p.shapes.as_ref().map(|s| s.polygons.iter().map(|p| p.outline.iter().chain(p.holes.iter()).map(|r| r.nodes.len()).sum::<usize>()).sum::<usize>())})).collect::<Vec<_>>()
        })).collect::<Vec<_>>();
        let shapes = before.shapes().iter().map(|(id, shape)| json!({"id":id,"layer":shape.layer.value(),"geometry":format!("{:?}",shape.shape.as_ref().and_then(|s|s.geometry.as_ref()).map(std::mem::discriminant))})).collect::<Vec<_>>();
        fs::write(
            output.join("native.json"),
            serde_json::to_vec_pretty(
                &json!({"kicad_version":version.full_version,"source_pid":identity.pid,"source":source,"zones":zones,"shapes":shapes,"live_text_bytes":text.len(),"live_structure_digest":observed.text.structure_digest(),"live_text_lock_parser":true,"injected_version_token_drift_rejected":true,"injected_live_lock_drift_rejected":lock_fault_tested,"stable_read":true,"saved_source_unchanged":true}),
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
