//! Verify selected-layer native track creation and saved readback in one owned editor.
use kicad_adapter::{
    NativePcbBatchReceipt, board_stackup_observation,
    lifecycle::{DisposablePcbLifecycle, OwnedPcbnewLauncher},
    native_copper_layer, read_verified_native_routing,
};
use kicad_checks::{CheckResult, DrcPolicy, compare_drc, run_pcb_drc};
use kicad_ipc::{CreateNetIdentity, CreateTrack};
use kicad_model::{CopperLayer, StableId};
use serde_json::json;
use std::{
    env, fs,
    path::{Path, PathBuf},
    time::Duration,
};

fn drc(board: &Path) -> Result<kicad_checks::DrcReport, Box<dyn std::error::Error>> {
    match run_pcb_drc(
        board,
        &DrcPolicy {
            allowed_root: board.parent().unwrap().to_path_buf(),
            timeout: Duration::from_secs(60),
        },
    ) {
        CheckResult::Completed(report) => Ok(report),
        CheckResult::Failed(error) => Err(format!("DRC failed: {error:?}").into()),
    }
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let output = PathBuf::from(
        env::args()
            .nth(1)
            .ok_or("usage: check-copper-layers NEW_OUTPUT_DIR")?,
    );
    fs::create_dir(&output)?;
    let output = fs::canonicalize(output)?;
    let input = output.join("fixture");
    fs::create_dir(&input)?;
    let board = input.join("board.kicad_pcb");
    let source = include_str!("../../../scripts/fixtures/layered-routing-rules.kicad_pcb");
    let index = source
        .rfind(')')
        .ok_or("fixture closing delimiter missing")?;
    let vias = r#"
      (via (at 20 20) (size 0.6) (drill 0.3) (layers "F.Cu" "B.Cu") (net 1) (uuid "50000000-0000-4000-8000-000000000001"))
      (via (at 40 20) (size 0.6) (drill 0.3) (layers "F.Cu" "B.Cu") (net 1) (uuid "50000000-0000-4000-8000-000000000002"))
    "#;
    fs::write(&board, format!("{}{vias})", &source[..index]))?;
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
        let mut ipc = editor.connect_owned()?;
        let version = ipc.get_version()?;
        // Settle only the disposable fixture before defining the preservation baseline.
        ipc.save_board(&identity.board_filename)?;
        editor.reload_owned()?;
        let token = ipc.instance_token().cloned();
        let baseline = ipc.capture_existing_board_items(&identity.board_filename)?;
        let protected = fs::read(copy.project_path())?;
        let before_drc = drc(copy.board_path())?;
        let observed = board_stackup_observation(
            StableId::new("probe-board")?,
            ipc.get_board_stackup(&identity.board_filename)?,
        )?;
        let layers =
            ["F.Cu", "In1.Cu", "In2.Cu", "B.Cu"].map(|name| name.parse::<CopperLayer>().unwrap());
        for layer in layers {
            layer.require_enabled_on(&observed)?;
        }
        if "In3.Cu"
            .parse::<CopperLayer>()?
            .require_enabled_on(&observed)
            .is_ok()
        {
            return Err("disabled copper layer was accepted".into());
        }
        let commit = ipc.begin_commit()?;
        let mut receipts = NativePcbBatchReceipt {
            graphics: vec![],
            tracks: vec![],
            vias: vec![],
        };
        for layer in layers {
            let request = CreateTrack {
                layer: native_copper_layer(layer),
                start_x_nm: 20_000_000,
                start_y_nm: 20_000_000,
                end_x_nm: 40_000_000,
                end_y_nm: 20_000_000,
                width_nm: 250_000,
                net: Some(CreateNetIdentity { name: "GND".into() }),
            };
            match ipc.create_track(&identity.board_filename, &request) {
                Ok(receipt) => receipts.tracks.push(receipt),
                Err(error) => {
                    ipc.drop_commit(&commit)?;
                    return Err(error.into());
                }
            }
        }
        ipc.commit_changes(&commit, "Create tracks on selected copper layers")?;
        ipc.save_board(&identity.board_filename)?;
        let before_reload =
            read_verified_native_routing(&mut ipc, &identity.board_filename, &receipts)?;
        editor.reload_owned()?;
        let after_reload =
            read_verified_native_routing(&mut ipc, &identity.board_filename, &receipts)?;
        let ids = receipts
            .tracks
            .iter()
            .map(|r| r.source_uuid.clone())
            .collect::<Vec<_>>();
        let after = ipc.capture_existing_board_items(&identity.board_filename)?;
        if before_reload != after_reload
            || !baseline.matches_after_additions(&after, &ids)?
            || ipc.instance_token().cloned() != token
            || fs::read(copy.project_path())? != protected
        {
            return Err("selected-layer save/reload preservation failed".into());
        }
        let after_drc = drc(copy.board_path())?;
        let delta = compare_drc(&before_drc, &after_drc);
        fs::write(
            output.join("baseline-drc.json"),
            serde_json::to_vec_pretty(&before_drc)?,
        )?;
        fs::write(
            output.join("final-drc.json"),
            serde_json::to_vec_pretty(&after_drc)?,
        )?;
        if !delta.introduced.is_empty()
            || after_drc.unconnected_items > before_drc.unconnected_items
        {
            return Err(format!("selected-layer DRC regression: {delta:?}").into());
        }
        let retained = output.join("result");
        fs::create_dir(&retained)?;
        fs::copy(copy.board_path(), retained.join("board.kicad_pcb"))?;
        fs::copy(copy.project_path(), retained.join("board.kicad_pro"))?;
        fs::write(
            output.join("baseline-items.pb"),
            baseline.persistent_evidence()?,
        )?;
        fs::write(output.join("final-items.pb"), after.persistent_evidence()?)?;
        Ok(
            json!({"kicad_version":version.full_version,"source_pid":identity.pid,"layers":layers,
            "new_tracks":ids,"exact_native_receipts":true,"same_process_reload":true,
            "existing_native_items_preserved":true,"project_bytes_preserved":true,"disabled_layer_rejected":true,
            "introduced_drc":delta.introduced.len(),"baseline_unconnected":before_drc.unconnected_items,"final_unconnected":after_drc.unconnected_items}),
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
        return Err("source fixture changed".into());
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
