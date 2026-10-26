//! Read effective netclasses on an owned four-layer fixture without mutating its board.
use kicad_adapter::lifecycle::{DisposablePcbLifecycle, OwnedPcbnewLauncher};
use kicad_adapter::routing_rules::{
    SavedRoutingConstraints, observe_routing_rules, read_saved_routing_rules,
};
use kicad_ipc::envelope::{ApiRequest, ApiResponse};
use kicad_ipc::{ClientConfig, IpcError, KiCadClient, KiCadTransport, NngTransport};
use serde_json::json;
use std::{env, fs, path::PathBuf};

// Inject a saved-file change during actual IPC observation on the same owned editor.
struct DriftTransport {
    native: NngTransport,
    change: Option<(PathBuf, Vec<u8>)>,
}
impl KiCadTransport for DriftTransport {
    fn request(&mut self, request: ApiRequest) -> Result<ApiResponse, IpcError> {
        let stackup = request
            .message
            .as_ref()
            .is_some_and(|message| message.type_url == kicad_ipc::GET_BOARD_STACKUP_TYPE_URL);
        let response = self.native.request(request)?;
        if stackup && let Some((path, bytes)) = self.change.take() {
            fs::write(path, bytes).map_err(|_| IpcError::MalformedEnvelope)?;
        }
        Ok(response)
    }
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let output = PathBuf::from(
        env::args()
            .nth(1)
            .ok_or("usage: check-routing-rules NEW_OUTPUT_DIR")?,
    );
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
    let result: Result<_, Box<dyn std::error::Error>> = (|| {
        let before = [fs::read(copy.board_path())?, fs::read(copy.project_path())?];
        let mut ipc = editor.connect_owned()?;
        let version = ipc.get_version()?;
        let token = ipc.instance_token().cloned();
        let stackup = ipc.get_board_stackup(&identity.board_filename)?;
        let nets = ipc.get_board_nets(&identity.board_filename)?;
        if !["GND", "VCC"]
            .iter()
            .all(|name| nets.iter().any(|net| &net.name == name))
        {
            return Err("fixture nets missing".into());
        }
        let names = vec!["GND".into(), "VCC".into()];
        let classes = ipc.get_effective_netclass_rules(&identity.board_filename, &names)?;
        let default = &classes["GND"];
        let power = &classes["VCC"];
        if default.class_name != "Default"
            || !power.implicit
            || !power.constituents.iter().any(|name| name == "Power")
            || !power.constituents.iter().any(|name| name == "Default")
            || default.preferred_track_width_nm != Some(250_000)
            || power.preferred_track_width_nm != Some(400_000)
            || power.clearance_nm != Some(300_000)
        {
            return Err(format!("unexpected classes: {classes:?}").into());
        }
        let via = power
            .preferred_via
            .as_ref()
            .ok_or("missing preferred via size")?;
        if via.diameter_nm != 800_000 || via.drill_nm != 400_000 {
            return Err("unexpected via preference".into());
        }
        if ipc
            .get_effective_netclass_rules(&identity.board_filename, &["UNKNOWN-NET".into()])
            .is_ok()
            || ipc.instance_token().cloned() != token
        {
            return Err("unknown net or instance was accepted".into());
        }
        let repeated = ipc.get_effective_netclass_rules(&identity.board_filename, &names)?;
        if classes != repeated
            || before != [fs::read(copy.board_path())?, fs::read(copy.project_path())?]
        {
            return Err("read-only rule query changed source or returned unstable evidence".into());
        }
        let combined = observe_routing_rules(&mut ipc, copy.board_path(), &names)?;
        let expected = SavedRoutingConstraints {
            min_clearance_nm: Some(100_000),
            min_track_width_nm: Some(200_000),
            min_via_diameter_nm: Some(500_000),
            min_via_annular_width_nm: Some(100_000),
            min_through_hole_diameter_nm: Some(200_000),
        };
        if combined.saved.constraints != expected
            || combined.saved.custom_rules.present
            || combined.netclasses["VCC"].preferred_track_width_nm != Some(400_000)
            || combined != observe_routing_rules(&mut ipc, copy.board_path(), &names)?
        {
            return Err("combined saved/native evidence mismatch".into());
        }
        let mut relocated = combined.clone();
        relocated.saved = read_saved_routing_rules(&board)?;
        relocated.stackup.board_id = kicad_model::StableId::new("relocated-board")?;
        if !combined.equivalent_preview_rules(&relocated)
            || combined.digest()? == relocated.digest()?
        {
            return Err("copy equivalence did not separate identity from rules".into());
        }
        let mut changed_project = before[1].clone();
        changed_project.push(b'\n');
        let custom_path = copy.board_path().with_extension("kicad_dru");
        let mut drift_errors = Vec::new();
        for (path, bytes) in [
            (copy.project_path().to_path_buf(), changed_project),
            (custom_path.clone(), b"(version 1)\n".to_vec()),
        ] {
            let config = ClientConfig::new(&identity.endpoint, "routing-rule-drift-probe")?;
            let transport = DriftTransport {
                native: NngTransport::connect(config.clone())?,
                change: Some((path.clone(), bytes)),
            };
            let mut drifting = KiCadClient::new(transport, config);
            drifting.get_version()?;
            let during = observe_routing_rules(&mut drifting, copy.board_path(), &names);
            let after = observe_routing_rules(&mut ipc, copy.board_path(), &names);
            // Restore owned bytes before checking any result, including failed observations.
            if path == custom_path {
                fs::remove_file(&path)?;
            } else {
                fs::write(&path, &before[1])?;
            }
            let error = during
                .err()
                .ok_or("mid-observation source drift was accepted")?;
            if !error.contains("changed while observing") {
                return Err(format!("unexpected drift diagnostic: {error}").into());
            }
            let after = after?;
            if combined.digest()? == after.digest()? || combined.equivalent_preview_rules(&after) {
                return Err("changed source did not invalidate rule evidence".into());
            }
            drift_errors.push(error);
        }
        if combined != observe_routing_rules(&mut ipc, copy.board_path(), &names)?
            || before != [fs::read(copy.board_path())?, fs::read(copy.project_path())?]
            || custom_path.exists()
        {
            return Err("owned rule sources were not restored exactly".into());
        }
        fs::write(
            output.join("combined-rules.json"),
            serde_json::to_vec_pretty(&combined)?,
        )?;
        let copper = stackup
            .layers
            .iter()
            .filter(|layer| {
                matches!(
                    layer.layer,
                    kicad_ipc::ObservedStackupLayerIdentity::FrontCopper
                        | kicad_ipc::ObservedStackupLayerIdentity::InnerCopper(_)
                        | kicad_ipc::ObservedStackupLayerIdentity::BackCopper
                ) && layer.enabled
            })
            .count();
        if copper != 4 {
            return Err(format!("expected four copper layers, got {copper}").into());
        }
        let observed=classes.iter().map(|(net,class)| json!({"net":net,"class":class.class_name,"implicit":class.implicit,"constituents":class.constituents,"clearance_nm":class.clearance_nm,"preferred_track_width_nm":class.preferred_track_width_nm,"via":class.preferred_via.as_ref().map(|via|json!({"diameter_nm":via.diameter_nm,"drill_nm":via.drill_nm})),"tuning_profile":class.tuning_profile,"native_payload_bytes":class.native_payload.len()})).collect::<Vec<_>>();
        for (net, class) in classes {
            fs::write(
                output.join(format!("{net}.netclass.pb")),
                class.native_payload,
            )?;
        }
        Ok(
            json!({"kicad_version":version.full_version,"copper_layers":copper,"classes":observed,"unknown_net_rejected":true,"stable_token":true,"repeated_native_evidence_equal":true,"read_only_source_unchanged":true,"source_pid":identity.pid,"saved_constraints":combined.saved.constraints,"combined_evidence_digest":combined.digest()?,"project_and_custom_rule_drift_rejected":drift_errors,"changed_evidence_not_preview_equivalent":true,"relocated_copy_evidence_equivalent":true,"restored_evidence_equal":true}),
        )
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
    let evidence = result?;
    fs::write(
        output.join("result.json"),
        serde_json::to_vec_pretty(&evidence)?,
    )?;
    fs::remove_dir(output.join("work"))?;
    println!("{}", serde_json::to_string(&evidence)?);
    Ok(())
}
