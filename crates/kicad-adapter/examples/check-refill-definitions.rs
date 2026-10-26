//! Verify complete definition preservation across actual native and CLI refill.
use kicad_adapter::lifecycle::{DisposablePcbLifecycle, OwnedPcbnewLauncher};
use kicad_adapter::live_geometry::{
    LiveBoardText, RefillDefinitionEvidence, observe_live_geometry,
};
use kicad_ipc::{
    ClientConfig, KiCadClient, KiCadTransport, NngTransport, editor_commands,
    envelope::{ApiRequest, ApiRequestHeader},
    response_status, response_token,
};
use protobuf::{Message, MessageField, well_known_types::any::Any};
use serde_json::json;
use std::{
    env, fs,
    path::{Path, PathBuf},
    process::Command,
    time::{Duration, Instant},
};
type Error = Box<dyn std::error::Error>;
const PYTHON: &str =
    "/Applications/KiCad/KiCad.app/Contents/Frameworks/Python.framework/Versions/3.9/bin/python3.9";
fn helper(mode: &str, board: &Path, source: &Path, output: &Path) -> Result<(), Error> {
    let status = Command::new(PYTHON)
        .env("PYTHONDONTWRITEBYTECODE", "1")
        .arg(
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../../scripts/check-refill-definitions.py"),
        )
        .arg(mode)
        .arg(board)
        .arg(source)
        .arg(output)
        .status()?;
    if !status.success() {
        return Err("refill evidence helper failed".into());
    }
    Ok(())
}
fn main() -> Result<(), Error> {
    let args = env::args().skip(1).collect::<Vec<_>>();
    if args.len() != 2 {
        return Err("usage: check-refill-definitions SOURCE NEW_OUTPUT".into());
    }
    let source = fs::canonicalize(&args[0])?;
    let output = PathBuf::from(&args[1]);
    fs::create_dir(&output)?;
    let output = fs::canonicalize(output)?;
    let originals = ["kicad_pcb", "kicad_pro", "kicad_sch", "kicad_dru"].map(|ext| {
        let p = source.with_extension(ext);
        let data = fs::read(&p).ok();
        (p, data)
    });
    let mut editor = OwnedPcbnewLauncher::new(&output.join("work"))?;
    let copy = editor.create_shadow(&source)?;
    helper("prepare", copy.board_path(), &source, &output)?;
    let identity = editor.launch(&copy)?;
    let result: Result<(), Error> = (|| {
        let mut config = ClientConfig::new(&identity.endpoint, "owned-refill-definition-probe")?;
        config.timeout = Duration::from_secs(60);
        let mut client = KiCadClient::new(NngTransport::connect(config.clone())?, config.clone());
        let version = client.get_version()?;
        if version.full_version != "10.0.7" {
            return Err("probe requires exact 10.0.7".into());
        }
        let token = client
            .instance_token()
            .ok_or("missing token")?
            .as_str()
            .to_owned();
        client.save_board(&identity.board_filename)?;
        editor.reload_owned()?;
        let protected = fs::read(copy.project_path())?;
        let before_observed = observe_live_geometry(&mut client, &identity.board_filename)?;
        let definitions = RefillDefinitionEvidence::from_observation(&before_observed)?;
        fs::write(
            output.join("definitions.json"),
            serde_json::to_vec_pretty(&definitions)?,
        )?;
        let before = before_observed.native;
        if before.zones().values().any(|z| z.filled) {
            return Err("prepared board still filled".into());
        }
        fs::write(
            output.join("before-live.kicad_pcb"),
            client.live_board_text(&identity.board_filename)?,
        )?;
        let mut transport = NngTransport::connect(config)?;
        let mut records = Vec::new();
        for iteration in 0..2 {
            let started = Instant::now();
            let response = transport.request(ApiRequest {
                header: MessageField::some(ApiRequestHeader {
                    client_name: "owned-refill-definition-probe".into(),
                    kicad_token: token.clone(),
                    ..Default::default()
                }),
                message: MessageField::some(Any {
                    type_url: "type.googleapis.com/kiapi.common.commands.RunAction".into(),
                    value: editor_commands::RunAction {
                        action: "pcbnew.ZoneFiller.zoneFillAll".into(),
                        ..Default::default()
                    }
                    .write_to_bytes()?,
                    ..Default::default()
                }),
                ..Default::default()
            })?;
            let action_millis = started.elapsed().as_millis();
            response_status(&response)?;
            if response_token(&response)?.as_str() != token {
                return Err("action changed token".into());
            }
            let any = response.message.as_ref().ok_or("missing action response")?;
            if any.type_url != "type.googleapis.com/kiapi.common.commands.RunActionResponse" {
                return Err("wrong action response".into());
            }
            let status = editor_commands::RunActionResponse::parse_from_bytes(&any.value)?
                .status
                .value();
            if status != 1 {
                return Err("action not submitted".into());
            }
            let observed = observe_live_geometry(&mut client, &identity.board_filename)?;
            let after_definitions = RefillDefinitionEvidence::from_observation(&observed)?;
            fs::write(
                output.join(format!("definitions-{iteration}.json")),
                serde_json::to_vec_pretty(&after_definitions)?,
            )?;
            if definitions != after_definitions {
                return Err("native refill changed complete definitions".into());
            }
            let immediate = observed.native;
            fs::write(
                output.join(format!("after-{iteration}.kicad_pcb")),
                client.live_board_text(&identity.board_filename)?,
            )?;
            let second = client.capture_board_geometry(&identity.board_filename)?;
            if immediate != second {
                return Err("geometry continued changing after action response".into());
            }
            let filled = immediate.zones().values().filter(|z| z.filled).count();
            if filled == 0 {
                return Err("action returned without any filled zones".into());
            }
            records.push(json!({"iteration":iteration,"action_millis":action_millis,"immediate_capture_millis":started.elapsed().as_millis(),"status":status,"zones":immediate.zones().len(),"filled_zones":filled,"repeated_geometry_equal":true}));
            fs::write(
                output.join("action.json"),
                serde_json::to_vec_pretty(
                    &json!({"kicad_version":version.full_version,"pid":identity.pid,"records":records,"cancellation_tested":false}),
                )?,
            )?;
        }
        let expected_path = source
            .parent()
            .ok_or("missing source parent")?
            .join("expected-clusters.json");
        let expected = if expected_path.is_file() {
            Some(serde_json::from_slice::<Vec<Vec<String>>>(&fs::read(
                expected_path,
            )?)?)
        } else {
            None
        };
        if let Some(expected) = &expected {
            let ids = expected.iter().flatten().cloned().collect::<Vec<_>>();
            let actual = client.connected_pad_clusters(&identity.board_filename, &ids)?;
            fs::write(
                output.join("clusters-before-reload.json"),
                serde_json::to_vec_pretty(
                    &json!({"expected":expected,"actual":actual,"equal":expected==&actual}),
                )?,
            )?;
            if expected != &actual {
                return Err("native connectivity merged disconnected zone islands".into());
            }
        }
        client.save_board(&identity.board_filename)?;
        editor.reload_owned()?;
        fs::write(
            output.join("reloaded.kicad_pcb"),
            client.live_board_text(&identity.board_filename)?,
        )?;
        if let Some(expected) = &expected {
            let ids = expected.iter().flatten().cloned().collect::<Vec<_>>();
            let actual = client.connected_pad_clusters(&identity.board_filename, &ids)?;
            fs::write(
                output.join("clusters-after-reload.json"),
                serde_json::to_vec_pretty(
                    &json!({"expected":expected,"actual":actual,"equal":expected==&actual}),
                )?,
            )?;
            if expected != &actual {
                return Err("reloaded native connectivity merged disconnected islands".into());
            }
        }
        let reloaded = observe_live_geometry(&mut client, &identity.board_filename)?;
        let reload_definitions = RefillDefinitionEvidence::from_observation(&reloaded)?;
        fs::write(
            output.join("definitions-reload.json"),
            serde_json::to_vec_pretty(&reload_definitions)?,
        )?;
        if definitions != reload_definitions || fs::read(copy.project_path())? != protected {
            return Err("reload changed complete definitions or project bytes".into());
        }
        fs::create_dir(output.join("result"))?;
        fs::copy(copy.board_path(), output.join("result/board.kicad_pcb"))?;
        fs::copy(copy.project_path(), output.join("result/board.kicad_pro"))?;
        let saved_definitions = LiveBoardText::parse(&fs::read_to_string(copy.board_path())?)?
            .closed_refill_definitions()?;
        fs::write(
            output.join("saved-definitions.json"),
            serde_json::to_vec_pretty(&saved_definitions)?,
        )?;
        helper("verify", copy.board_path(), &source, &output)?;
        let independent_path = output
            .join("independent")
            .join(copy.board_path().file_name().ok_or("missing name")?);
        let independent = LiveBoardText::parse(&fs::read_to_string(&independent_path)?)?;
        let independent_definitions = independent.closed_refill_definitions()?;
        fs::write(
            output.join("independent-definitions.json"),
            serde_json::to_vec_pretty(
                &json!({"definitions":independent_definitions,"equal":independent_definitions==saved_definitions}),
            )?,
        )?;
        if independent_definitions != saved_definitions {
            return Err("independent CLI refill changed complete definitions".into());
        }
        fs::copy(&independent_path, output.join("cli-refilled.kicad_pcb"))?;
        fs::remove_dir_all(output.join("independent"))?;
        fs::write(
            output.join("result.json"),
            serde_json::to_vec_pretty(
                &json!({"kicad_version":version.full_version,"pid":identity.pid,"zone_count":before.zones().len(),"locked_zone_count":before_observed.text.zones().values().filter(|z| z.locked).count(),"complete_definitions_preserved":true,"repeated_fill_preserved":true,"native_reload_preserved":true,"independent_cli_preserved":true,"settled_project_unchanged":true,"cancellation_tested":false,"scope":"definition preservation only; not completed production fill, copper equivalence, DRC delta or MCP acceptance"}),
            )?,
        )?;
        Ok(())
    })();
    editor.close_owned(identity.pid)?;
    editor.cleanup()?;
    for (path, data) in &originals {
        if fs::read(path).ok() != *data {
            return Err("original files changed".into());
        }
    }
    fs::write(
        output.join("cleanup.json"),
        serde_json::to_vec_pretty(
            &json!({"owned_editor_closed":true,"source_files_unchanged":true,"owned_pid":identity.pid}),
        )?,
    )?;
    if let Err(error) = &result {
        fs::write(
            output.join("failure.json"),
            serde_json::to_vec_pretty(&json!({"error":error.to_string()}))?,
        )?;
    }
    fs::remove_dir(output.join("work"))?;
    result
}
