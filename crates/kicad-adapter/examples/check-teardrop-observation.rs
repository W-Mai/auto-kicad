//! Record exact native and serialized teardrop observations on one owned editor.
use kicad_adapter::lifecycle::{DisposablePcbLifecycle, OwnedPcbnewLauncher};
use kicad_adapter::live_geometry::{LiveBoardText, TeardropTextPartition};
use kicad_ipc::ZoneFillAttempt;
use protobuf::Message;
use serde_json::json;
use std::{env, fs, path::PathBuf};
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args = env::args().skip(1).collect::<Vec<_>>();
    if args.len() != 2 {
        return Err("usage: check-teardrop-observation SOURCE NEW_OUTPUT".into());
    }
    let source = fs::canonicalize(&args[0])?;
    fs::create_dir(&args[1])?;
    let output = fs::canonicalize(PathBuf::from(&args[1]))?;
    let originals = ["kicad_pcb", "kicad_pro", "kicad_dru", "kicad_sch"].map(|ext| {
        let p = source.with_extension(ext);
        let b = fs::read(&p).ok();
        (p, b)
    });
    let mut editor = OwnedPcbnewLauncher::new(&output.join("work"))?;
    let copy = editor.create_shadow(&source)?;
    let identity = editor.launch(&copy)?;
    fs::write(
        output.join("owned.json"),
        serde_json::to_vec_pretty(&json!({"pid":identity.pid,"endpoint":identity.endpoint}))?,
    )?;
    let result: Result<(), Box<dyn std::error::Error>> = (|| {
        let mut client = editor.connect_owned()?;
        if client.get_version()?.full_version != "10.0.7" {
            return Err("requires exact 10.0.7".into());
        }
        let mut stages = Vec::new();
        for stage in 0..3 {
            if stage > 0 {
                client = match client.request_zone_refill(&identity.board_filename) {
                    ZoneFillAttempt::Returned { client, .. } => client,
                    _ => return Err("native refill outcome uncertain".into()),
                };
            }
            let native = client.capture_board_geometry(&identity.board_filename)?;
            let text = client.live_board_text(&identity.board_filename)?;
            if native != client.capture_board_geometry(&identity.board_filename)?
                || text != client.live_board_text(&identity.board_filename)?
            {
                return Err("native/text changed while binding".into());
            }
            let partition = TeardropTextPartition::parse(&text)?;
            let identities = partition.bind(&native)?;
            if identities.len() != 4 || partition.authored().zones().len() != 3 {
                return Err("unexpected fixture counts".into());
            }
            if LiveBoardText::parse(&text).is_ok() {
                return Err("ordinary parser must remain strict".into());
            }
            let wrong =
                TeardropTextPartition::parse(&text.replace("(type padvia)", "(type track_end)"))?;
            if wrong.bind(&native).is_ok() {
                return Err("mismatched native kind was accepted".into());
            }
            fs::write(output.join(format!("stage-{stage}-live.kicad_pcb")), &text)?;
            let mut zones = Vec::new();
            for (id, zone) in native.zones() {
                fs::write(
                    output.join(format!("stage-{stage}-{id}.bin")),
                    zone.write_to_bytes()?,
                )?;
                let details = if zone.type_.enum_value()
                    == Ok(kicad_ipc::board_types::ZoneType::ZT_TEARDROP)
                {
                    let points = zone.outline.as_ref().ok_or("outline")?.polygons[0]
                        .outline
                        .as_ref()
                        .ok_or("boundary")?
                        .nodes
                        .iter()
                        .map(|node| match &node.geometry {
                            Some(kicad_ipc::base_types::poly_line_node::Geometry::Point(p)) => {
                                Ok([p.x_nm, p.y_nm])
                            }
                            _ => Err("fixture requires native point outlines"),
                        })
                        .collect::<Result<Vec<_>, _>>()?;
                    let Some(kicad_ipc::board_types::zone::Settings::CopperSettings(settings)) =
                        &zone.settings
                    else {
                        return Err("settings".into());
                    };
                    json!({"points":points,"layer_name":format!("{:?}",zone.layers[0].enum_value().map_err(|_|"layer")?).trim_start_matches("BL_").replace('_',"."),"net":settings.net.as_ref().map_or("",|n|n.name.as_str()),"priority":zone.priority})
                } else {
                    serde_json::Value::Null
                };
                zones.push(json!({"id":id,"native_type":zone.type_.value(),"layers":zone.layers.iter().map(|l|l.value()).collect::<Vec<_>>(),"independent_fields":details,"protobuf":protobuf::text_format::print_to_string(zone)}));
            }
            client.save_board(&identity.board_filename)?;
            fs::copy(
                copy.board_path(),
                output.join(format!("stage-{stage}.kicad_pcb")),
            )?;
            let saved = TeardropTextPartition::parse(&fs::read_to_string(copy.board_path())?)?;
            let saved_identities = saved.bind(&native)?;
            if saved_identities != identities {
                return Err("saved/generated evidence differs from live".into());
            }
            editor.reload_owned()?;
            let reloaded_native = client.capture_board_geometry(&identity.board_filename)?;
            let reloaded =
                TeardropTextPartition::parse(&client.live_board_text(&identity.board_filename)?)?;
            let reloaded_identities = reloaded.bind(&reloaded_native)?;
            let keys = |values: &[kicad_adapter::live_geometry::TeardropIdentity]| {
                values
                    .iter()
                    .map(|v| v.correspondence_key().to_owned())
                    .collect::<Vec<_>>()
            };
            if keys(&identities) != keys(&reloaded_identities) {
                return Err("reload changed generated correspondence".into());
            }
            stages.push(json!({"stage":stage,"zones":zones,"identities":identities,"reloaded_identities":reloaded_identities,"authored_definition_digest":partition.authored().refill_definition_digest()?,"saved_binding":true,"strict_parser_rejected":true,"mismatched_type_rejected":true}));
        }
        fs::write(
            output.join("observations.json"),
            serde_json::to_vec_pretty(
                &json!({"kicad_version":"10.0.7","pid":identity.pid,"stages":stages}),
            )?,
        )?;
        Ok(())
    })();
    editor.close_owned(identity.pid)?;
    editor.cleanup()?;
    for (p, b) in originals {
        if fs::read(p).ok() != b {
            return Err("source changed".into());
        }
    }
    fs::write(
        output.join("cleanup.json"),
        serde_json::to_vec_pretty(
            &json!({"pid":identity.pid,"closed":true,"source_unchanged":true,"passed":result.is_ok()}),
        )?,
    )?;
    result
}
