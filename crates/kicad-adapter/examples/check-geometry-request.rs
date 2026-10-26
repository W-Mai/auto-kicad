//! Exercise JSON geometry requests, native definition edits and independent refill in one editor.
use kicad_adapter::{
    geometry::{GeometryObservation, observe_context},
    lifecycle::{DisposablePcbLifecycle, OwnedPcbnewLauncher},
    live_geometry::{RefillDefinitionEvidence, observe_live_geometry},
    refill_verification::VerifiedRefillContent,
};
use kicad_checks::{CheckResult, DrcPolicy, compare_drc, refill::refill_closed_copy, run_pcb_drc};
use kicad_ipc::{ClientConfig, KiCadClient, NngTransport, ZoneFillAttempt};
use kicad_model::{
    StableId,
    geometry::{GeometryRequest, RefillMode, ZoneEdit},
};
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    env, fs,
    path::{Path, PathBuf},
    process::Command,
    time::Duration,
};
type Error = Box<dyn std::error::Error>;
fn fill(
    client: KiCadClient<NngTransport>,
    filename: &str,
) -> Result<KiCadClient<NngTransport>, Error> {
    match client.request_zone_refill(filename) {
        ZoneFillAttempt::Returned { client, .. } => Ok(client),
        ZoneFillAttempt::NotDispatched { reason, .. } => {
            Err(format!("fill not dispatched: {reason}").into())
        }
        ZoneFillAttempt::Uncertain(reader) => {
            Err(format!("fill uncertain, no further writes: {}", reader.reason()).into())
        }
    }
}
fn bind(
    value: &mut Value,
    observed: &GeometryObservation,
    created: &BTreeMap<String, String>,
) -> Result<(), Error> {
    match value {
        Value::String(s) if s == "${revision}" => {
            *s = observed.context().revision.as_str().to_owned()
        }
        Value::String(s) if s == "${first_solid_zone}" => {
            *s = observed
                .context()
                .zones
                .iter()
                .find(|z| z.editable_solid && z.lock_state == kicad_model::LockState::Unlocked)
                .ok_or("missing editable solid zone")?
                .source_uuid
                .to_string()
        }
        Value::String(s) if s.starts_with("${net:") && s.ends_with('}') => {
            let name = &s[6..s.len() - 1];
            *s = observed
                .context()
                .nets
                .iter()
                .find(|n| n.name == name)
                .ok_or("missing named fixture net")?
                .net_id
                .to_string();
        }
        Value::String(s) if s.starts_with("${created:") && s.ends_with('}') => {
            *s = created
                .get(&s[10..s.len() - 1])
                .ok_or("missing created fixture identity")?
                .clone()
        }
        Value::Array(values) => {
            for v in values {
                bind(v, observed, created)?;
            }
        }
        Value::Object(values) => {
            for v in values.values_mut() {
                bind(v, observed, created)?;
            }
        }
        _ => (),
    }
    Ok(())
}
fn drc(board: &Path, policy: &DrcPolicy) -> Result<kicad_checks::DrcReport, Error> {
    match run_pcb_drc(board, policy) {
        CheckResult::Completed(r) => Ok(r),
        other => Err(format!("DRC failed: {other:?}").into()),
    }
}
fn main() -> Result<(), Error> {
    let args = env::args().skip(1).collect::<Vec<_>>();
    if args.len() != 3 {
        return Err("usage: check-geometry-request SOURCE SCENARIOS_JSON NEW_OUTPUT".into());
    }
    let source = fs::canonicalize(&args[0])?;
    let output = PathBuf::from(&args[2]);
    fs::create_dir(&output)?;
    let output = fs::canonicalize(output)?;
    let scenario = fs::read(&args[1])?;
    if scenario.len() > 2 * 1024 * 1024 {
        return Err("scenario exceeds bound".into());
    }
    let stages: Vec<Value> = serde_json::from_slice(&scenario)?;
    if stages.is_empty() || stages.len() > 32 {
        return Err("scenario count invalid".into());
    }
    fs::write(output.join("scenario.json"), scenario)?;
    let originals = ["kicad_pcb", "kicad_pro", "kicad_sch", "kicad_dru"].map(|ext| {
        let p = source.with_extension(ext);
        let b = fs::read(&p).ok();
        (p, b)
    });
    let mut editor = OwnedPcbnewLauncher::new(&output.join("work"))?;
    let copy = editor.create_shadow(&source)?;
    let helper=Command::new("/Applications/KiCad/KiCad.app/Contents/Frameworks/Python.framework/Versions/3.9/bin/python3.9").arg("scripts/check-refill-definitions.py").arg("prepare").arg(copy.board_path()).arg(&source).arg(&output).status()?;
    if !helper.success() {
        return Err("fixture preparation failed".into());
    }
    let identity = editor.launch(&copy)?;
    let result: Result<(), Error> = (|| {
        let mut config = ClientConfig::new(&identity.endpoint, "owned-json-geometry-acceptance")?;
        config.timeout = Duration::from_secs(60);
        let mut client = KiCadClient::new(NngTransport::connect(config.clone())?, config);
        if client.get_version()?.full_version != "10.0.7" {
            return Err("requires KiCad 10.0.7".into());
        }
        client.save_board(&identity.board_filename)?;
        editor.reload_owned()?;
        let protected = fs::read(copy.project_path())?;
        let unfilled = observe_live_geometry(&mut client, &identity.board_filename)?;
        client = fill(client, &identity.board_filename)?;
        let filled = observe_live_geometry(&mut client, &identity.board_filename)?;
        if RefillDefinitionEvidence::from_observation(&unfilled)?
            != RefillDefinitionEvidence::from_observation(&filled)?
        {
            return Err("baseline fill changed definitions".into());
        }
        client.save_board(&identity.board_filename)?;
        editor.reload_owned()?;
        let board_id = StableId::new("json-geometry-acceptance")?;
        let baseline = observe_context(&mut client, &identity.board_filename, &board_id)?;
        let policy = DrcPolicy {
            allowed_root: copy.board_path().parent().ok_or("missing parent")?.into(),
            timeout: Duration::from_secs(60),
        };
        let baseline_drc = drc(copy.board_path(), &policy)?;
        fs::write(
            output.join("baseline-drc.json"),
            serde_json::to_vec_pretty(&baseline_drc)?,
        )?;
        fs::copy(copy.board_path(), output.join("baseline.kicad_pcb"))?;
        let mut created = BTreeMap::new();
        let mut records = Vec::new();
        for (index, mut value) in stages.into_iter().enumerate() {
            let observed = observe_context(&mut client, &identity.board_filename, &board_id)?;
            bind(&mut value, &observed, &created)?;
            let request = GeometryRequest::from_json(&serde_json::to_string(&value)?)?;
            if request.outline.is_some() || request.refill != RefillMode::All {
                return Err(
                    "this zone acceptance requires zone-only requests with refill all".into(),
                );
            }
            fs::write(
                output.join(format!("request-{index}.json")),
                serde_json::to_vec_pretty(&request)?,
            )?;
            let prepared = observed.prepare(&request)?;
            let zones = prepared.zones().ok_or("expected zone mutation")?;
            let mut stale = request.clone();
            stale.observed_revision = kicad_model::RevisionDigest::new("f".repeat(64))?;
            if observed.prepare(&stale).is_ok() {
                return Err("stale JSON request accepted".into());
            }
            let before_items = client.capture_existing_board_items(&identity.board_filename)?;
            let footprints =
                client.capture_footprint_instance_templates(&identity.board_filename)?;
            let mut pads = Vec::new();
            for f in &footprints {
                pads.extend(
                    client
                        .get_footprint_definition_pads(
                            &identity.board_filename,
                            &f.metadata().source_uuid,
                        )?
                        .into_iter()
                        .map(|p| p.source_uuid),
                );
            }
            let clusters = client.connected_pad_clusters(&identity.board_filename, &pads)?;
            let before_drc = drc(copy.board_path(), &policy)?;
            observed.verify_current(&mut client, &identity.board_filename)?;
            let commit = client.begin_commit()?;
            let receipts = match zones.apply_in_transaction(&mut client, &identity.board_filename) {
                Ok(r) => r,
                Err(e) => {
                    client.drop_commit(&commit)?;
                    return Err(e.into());
                }
            };
            client.commit_changes(&commit, "Apply explicit solid-zone request")?;
            let mutated = observe_live_geometry(&mut client, &identity.board_filename)?;
            zones.verify_definitions(&mutated, &receipts)?;
            client.save_board(&identity.board_filename)?;
            editor.reload_owned()?;
            let before_fill = observe_live_geometry(&mut client, &identity.board_filename)?;
            zones.verify_definitions(&before_fill, &receipts)?;
            client = fill(client, &identity.board_filename)?;
            let after_fill = observe_live_geometry(&mut client, &identity.board_filename)?;
            if RefillDefinitionEvidence::from_observation(&before_fill)?
                != RefillDefinitionEvidence::from_observation(&after_fill)?
            {
                return Err("fill changed edited definitions".into());
            }
            client.save_board(&identity.board_filename)?;
            editor.reload_owned()?;
            let reloaded = observe_live_geometry(&mut client, &identity.board_filename)?;
            let independent = refill_closed_copy(copy.board_path(), &policy)
                .map_err(|e| format!("independent fill: {e:?}"))?;
            let verified = VerifiedRefillContent::verify(&before_fill, &reloaded, &independent)?;
            if !before_items
                .matches_saved(&client.capture_existing_board_items(&identity.board_filename)?)?
            {
                return Err("zone edit changed tracks or graphics".into());
            }
            let current = client.capture_footprint_instance_templates(&identity.board_filename)?;
            if current.len() != footprints.len() {
                return Err("zone edit changed footprint set".into());
            }
            for f in &footprints {
                let other = current
                    .iter()
                    .find(|n| n.metadata().source_uuid == f.metadata().source_uuid)
                    .ok_or("missing footprint")?;
                if !f.matches_saved_instance(other)? {
                    return Err("zone edit changed footprint fields".into());
                }
            }
            let after_clusters = client.connected_pad_clusters(&identity.board_filename, &pads)?;
            if after_clusters != clusters {
                return Err("fixture pad connectivity changed".into());
            }
            let after_drc = drc(copy.board_path(), &policy)?;
            let delta = compare_drc(&before_drc, &after_drc);
            if !delta.introduced.is_empty()
                || after_drc.unconnected_items > before_drc.unconnected_items
            {
                return Err(format!("JSON zone edit regressed DRC/connectivity: {delta:?}").into());
            }
            if fs::read(copy.project_path())? != protected {
                return Err("protected project changed".into());
            }
            let intents = request.zones.iter().filter_map(|z| {
                if let ZoneEdit::Create { zone_id, .. } = z {
                    Some(zone_id)
                } else {
                    None
                }
            });
            for (intent, receipt) in intents.zip(&receipts.created) {
                created.insert(intent.to_string(), receipt.source_uuid().to_owned());
            }
            let record = json!({"stage":index,"native_creation_ids":created,"refill_zone_ids":prepared.compiled().refill_zone_ids(),"affected_nets":prepared.compiled().affected_nets(),"verified":verified,"drc_delta":delta,"unconnected":after_drc.unconnected_items,"pad_count":pads.len(),"pad_clusters_preserved":true,"stale_request_refused":true});
            fs::copy(
                copy.board_path(),
                output.join(format!("stage-{index}.kicad_pcb")),
            )?;
            fs::write(
                output.join(format!("stage-{index}.json")),
                serde_json::to_vec_pretty(&record)?,
            )?;
            records.push(record);
        }
        let final_observation = observe_context(&mut client, &identity.board_filename, &board_id)?;
        let restored = RefillDefinitionEvidence::from_observation(baseline.live())?
            == RefillDefinitionEvidence::from_observation(final_observation.live())?
            && baseline.live().text.filled_copper()?
                == final_observation.live().text.filled_copper()?;
        fs::copy(copy.project_path(), output.join("result.kicad_pro"))?;
        fs::write(
            output.join("result.json"),
            serde_json::to_vec_pretty(
                &json!({"kicad_version":"10.0.7","pid":identity.pid,"stages":records,"restored_baseline":restored,"complete_intent_definitions_and_copper":true,"scope":"typed JSON compilation and native zone execution; combined outline/zone MCP approval/recovery is separate"}),
            )?,
        )?;
        Ok(())
    })();
    editor.close_owned(identity.pid)?;
    editor.cleanup()?;
    for (p, b) in originals {
        if fs::read(p).ok() != b {
            return Err("original source changed".into());
        }
    }
    fs::write(
        output.join("cleanup.json"),
        serde_json::to_vec_pretty(
            &json!({"owned_pid":identity.pid,"closed":true,"source_unchanged":true}),
        )?,
    )?;
    if let Err(e) = &result {
        fs::write(
            output.join("failure.json"),
            serde_json::to_vec_pretty(&json!({"reason":e.to_string()}))?,
        )?;
    }
    fs::remove_dir(output.join("work"))?;
    result
}
