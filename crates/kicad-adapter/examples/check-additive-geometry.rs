//! Verify additive definitions and complete refill on sequential owned zoned-board copies.
use kicad_adapter::{
    NativePcbBatch,
    copper_connectivity::VerifiedCopperConnectivity,
    geometry::observe_generated_context,
    geometry_candidate::GeometryCandidateEvidence,
    lifecycle::{DisposablePcbLifecycle, OwnedPcbnewLauncher},
    teardrop_refill::TeardropRefillObservation,
};
use kicad_checks::{
    CompleteDrcReport, DrcPolicy, compare_drc, refill::refill_closed_copy, run_pcb_drc_complete,
};
use kicad_ipc::{
    BoardGraphicLayer, BoardGraphicSegment, CopperLayer, CreateNetIdentity, CreateThroughVia,
    CreateTrack, KiCadClient, NngTransport, ZoneFillAttempt,
};
use kicad_model::{
    StableId,
    geometry::{GeometryRequest, RefillMode},
};
use serde_json::json;
use std::{env, fs, path::Path, time::Duration};
type Error = Box<dyn std::error::Error>;

fn fill(
    client: KiCadClient<NngTransport>,
    filename: &str,
) -> Result<KiCadClient<NngTransport>, Error> {
    match client.request_zone_refill(filename) {
        ZoneFillAttempt::Returned { client, .. } => Ok(client),
        ZoneFillAttempt::NotDispatched { reason, .. } => Err(reason.into()),
        ZoneFillAttempt::Uncertain(reader) => {
            Err(format!("uncertain fill; no further writes: {}", reader.reason()).into())
        }
    }
}

fn batch(layers: Vec<CopperLayer>) -> NativePcbBatch {
    let mut tracks = Vec::new();
    for (layer, net, points) in [
        (
            CopperLayer::FRONT,
            "GND",
            vec![(15, 10), (20, 10), (20, 20), (25, 20), (25, 25)],
        ),
        (CopperLayer::BACK, "GND", vec![(25, 25), (40, 25), (40, 20)]),
        (
            CopperLayer::inner(1).unwrap(),
            "VCC",
            vec![(35, 10), (45, 10)],
        ),
    ] {
        for pair in points.windows(2) {
            tracks.push(CreateTrack {
                layer,
                start_x_nm: pair[0].0 * 1_000_000,
                start_y_nm: pair[0].1 * 1_000_000,
                end_x_nm: pair[1].0 * 1_000_000,
                end_y_nm: pair[1].1 * 1_000_000,
                width_nm: 250_000,
                net: Some(CreateNetIdentity { name: net.into() }),
            });
        }
    }
    NativePcbBatch {
        tracks,
        vias: vec![CreateThroughVia {
            copper_layers: layers,
            position_x_nm: 25_000_000,
            position_y_nm: 25_000_000,
            pad_diameter_nm: 600_000,
            drill_diameter_nm: 300_000,
            net: Some(CreateNetIdentity { name: "GND".into() }),
        }],
        graphics: vec![BoardGraphicSegment {
            layer: BoardGraphicLayer::DwgsUser,
            start_x_nm: 10_000_000,
            start_y_nm: 60_000_000,
            end_x_nm: 15_000_000,
            end_y_nm: 60_000_000,
            width_nm: 100_000,
        }],
    }
}

fn run(
    source: &Path,
    output: &Path,
) -> Result<
    (
        GeometryCandidateEvidence,
        CompleteDrcReport,
        VerifiedCopperConnectivity,
    ),
    Error,
> {
    fs::create_dir(output)?;
    let mut editor = OwnedPcbnewLauncher::new(&output.join("work"))?;
    let copy = editor.create_shadow(source)?;
    let identity = editor.launch(&copy)?;
    let result = (|| {
        let filename = &identity.board_filename;
        let mut client = editor.connect_owned()?;
        if client.get_version()?.full_version != "10.0.7" {
            return Err("requires KiCad 10.0.7".into());
        }
        client = fill(client, filename)?;
        client.save_board(filename)?;
        editor.reload_owned()?;
        let board_id = StableId::new(format!("additive:{}", copy.board_path().display()))?;
        let observed = observe_generated_context(&mut client, filename, &board_id)?;
        let request = GeometryRequest {
            observed_revision: observed.context().revision.clone(),
            outline: None,
            zones: vec![],
            refill: RefillMode::All,
        };
        let input = batch(
            observed
                .context()
                .enabled_layers
                .iter()
                .copied()
                .map(kicad_adapter::native_copper_layer)
                .collect(),
        );
        let prepared = observed
            .prepare(&request)?
            .with_additions(&mut client, input.clone())?;
        let mut wrong = input;
        wrong.tracks[0].width_nm += 1;
        let wrong = observed
            .prepare(&request)?
            .with_additions(&mut client, wrong)?;
        let footprints = client.capture_footprint_instance_templates(filename)?;
        let mut pads = Vec::new();
        for footprint in &footprints {
            pads.extend(
                client
                    .get_footprint_definition_pads(filename, &footprint.metadata().source_uuid)?
                    .into_iter()
                    .map(|p| p.source_uuid),
            );
        }
        let clusters = client.connected_pad_clusters(filename, &pads)?;
        let protected = fs::read(copy.project_path())?;
        let policy = DrcPolicy {
            allowed_root: copy.board_path().parent().unwrap().into(),
            timeout: Duration::from_secs(60),
        };
        let baseline = run_pcb_drc_complete(copy.board_path(), &policy)
            .map_err(|e| format!("baseline DRC: {e:?}"))?;
        fs::copy(copy.board_path(), output.join("before.kicad_pcb"))?;
        let commit = client.begin_commit()?;
        let receipts = prepared.apply_in_transaction(&mut client, filename)?;
        client.commit_changes(&commit, "Create additive geometry with complete refill")?;
        let mutated = TeardropRefillObservation::capture(&mut client, filename)?;
        prepared.verify_additive_definitions(&mut client, filename, &mutated, &receipts, false)?;
        client.save_board(filename)?;
        editor.reload_owned()?;
        let before_fill = TeardropRefillObservation::capture(&mut client, filename)?;
        let proof = prepared.verify_additive_definitions(
            &mut client,
            filename,
            &before_fill,
            &receipts,
            true,
        )?;
        client = fill(client, filename)?;
        let filled = TeardropRefillObservation::capture(&mut client, filename)?;
        client.save_board(filename)?;
        editor.reload_owned()?;
        let reloaded = TeardropRefillObservation::capture(&mut client, filename)?;
        let independent = refill_closed_copy(copy.board_path(), &policy)
            .map_err(|e| format!("independent refill: {e:?}"))?;
        let candidate = GeometryCandidateEvidence::capture_generated_additions(
            &prepared,
            &receipts,
            &proof,
            &filled,
            &reloaded,
            &independent,
        )?;
        if GeometryCandidateEvidence::capture_generated_additions(
            &wrong,
            &receipts,
            &proof,
            &filled,
            &reloaded,
            &independent,
        )
        .is_ok()
        {
            return Err("candidate accepted a different physical route intent".into());
        }
        if observed.live().text.filled_copper()?
            == reloaded.authored_geometry()?.text.filled_copper()?
        {
            return Err("fixture route did not change filled zone copper".into());
        }
        if client.connected_pad_clusters(filename, &pads)? != clusters {
            return Err("fixture pad partitions changed".into());
        }
        let after_footprints = client.capture_footprint_instance_templates(filename)?;
        if footprints.len() != after_footprints.len() {
            return Err("footprint set changed".into());
        }
        for original in &footprints {
            let after = after_footprints
                .iter()
                .find(|f| f.metadata().source_uuid == original.metadata().source_uuid)
                .ok_or("missing footprint")?;
            if !original.matches_saved_instance(after)? {
                return Err("complete footprint changed".into());
            }
        }
        let final_observation = observe_generated_context(&mut client, filename, &board_id)?;
        let connectivity = VerifiedCopperConnectivity::capture(
            &mut client,
            filename,
            &final_observation,
            &fs::read_to_string(copy.board_path())?,
            copy.board_path(),
            &policy,
        )?;
        let after = run_pcb_drc_complete(copy.board_path(), &policy)
            .map_err(|e| format!("result DRC: {e:?}"))?;
        connectivity.verify_current(
            &mut client,
            filename,
            &final_observation,
            copy.board_path(),
            &policy,
        )?;
        let delta = compare_drc(baseline.normalized(), after.normalized());
        if !delta.introduced.is_empty()
            || after.normalized().unconnected_items > baseline.normalized().unconnected_items
        {
            return Err(format!("additive geometry regressed DRC: {delta:?}").into());
        }
        if fs::read(copy.project_path())? != protected
            || editor.current_identity()?.pid != identity.pid
        {
            return Err("project or editor changed".into());
        }
        fs::copy(copy.board_path(), output.join("after.kicad_pcb"))?;
        fs::copy(copy.project_path(), output.join("after.kicad_pro"))?;
        fs::write(
            output.join("result.json"),
            serde_json::to_vec_pretty(&json!({
                "pid":identity.pid, "candidate":candidate, "connectivity":connectivity, "baseline":baseline,"after":after,"delta":delta,
                "tracks":7,"vias":1,"graphics":1,"original_items_preserved":true,"complete_footprints_preserved":true,
                "filled_copper_changed":true,"pad_partitions_preserved":true,"same_process_reload":true,"wrong_intent_rejected":true,
                "scope":"native additive geometry and refill; no public MCP approval"
            }))?,
        )?;
        Ok((candidate, after, connectivity))
    })();
    editor.close_owned(identity.pid)?;
    editor.cleanup()?;
    fs::write(
        output.join("cleanup.json"),
        serde_json::to_vec_pretty(
            &json!({"pid":identity.pid,"owned_editor_closed":true,"shadow_cleaned":true}),
        )?,
    )?;
    result
}

fn main() -> Result<(), Error> {
    let output = std::path::PathBuf::from(
        env::args()
            .nth(1)
            .ok_or("usage: check-additive-geometry NEW_OUTPUT")?,
    );
    fs::create_dir(&output)?;
    let output = fs::canonicalize(output)?;
    let fixture = output.join("fixture");
    fs::create_dir(&fixture)?;
    let mut text =
        include_str!("../../../scripts/fixtures/layered-route-anchors.kicad_pcb").to_owned();
    let mut zones = String::new();
    for (index, layer) in ["In1.Cu", "B.Cu"].iter().enumerate() {
        zones.push_str(&format!(r#"(zone (net "GND") (layer "{layer}") (uuid "80000000-0000-4000-8000-{:012}")
            (name "retained plane {index}") (hatch edge 0.5) (connect_pads (clearance 0.2)) (min_thickness 0.2)
            (fill (thermal_gap 0.5) (thermal_bridge_width 0.5) (island_removal_mode 1))
            (polygon (pts (xy 2 2) (xy 98 2) (xy 98 78) (xy 2 78))))"#,index+1));
    }
    text.insert_str(text.rfind(')').unwrap(), &zones);
    let source = fixture.join("board.kicad_pcb");
    fs::write(&source, text)?;
    fs::write(
        source.with_extension("kicad_pro"),
        include_str!("../../../scripts/fixtures/layered-routing-rules.kicad_pro"),
    )?;
    // Settle native-generated property UUIDs once before copying the fixture into independent editors.
    let status = std::process::Command::new(
        "/Applications/KiCad/KiCad.app/Contents/Frameworks/Python.framework/Versions/3.9/bin/python3.9",
    ).args(["-c", "import pcbnew, sys; board = pcbnew.LoadBoard(sys.argv[1]); pcbnew.SaveBoard(sys.argv[1], board)"])
        .arg(&source).status()?;
    if !status.success() {
        return Err("native fixture preparation failed".into());
    }
    let original = [
        fs::read(&source)?,
        fs::read(source.with_extension("kicad_pro"))?,
    ];
    let (preview, preview_drc, preview_connectivity) = run(&source, &output.join("preview"))?;
    let (actual, actual_drc, actual_connectivity) = run(&source, &output.join("source"))?;
    let correspondence = preview.equivalent_to(&actual)?;
    if correspondence.len() != 9
        || correspondence.iter().any(|(a, b)| a == b)
        || !preview_drc.equivalent_with_closed_connectivity(
            &actual_drc,
            &correspondence,
            preview_connectivity.closed(),
            actual_connectivity.closed(),
        )?
    {
        return Err("independent additive candidate or complete DRC correspondence differs".into());
    }
    if original
        != [
            fs::read(&source)?,
            fs::read(source.with_extension("kicad_pro"))?,
        ]
    {
        return Err("original changed".into());
    }
    fs::write(
        output.join("comparison.json"),
        serde_json::to_vec_pretty(
            &json!({"correspondence":correspondence,"complete_candidate_matches":true,"complete_drc_matches":true,"original_unchanged":true}),
        )?,
    )?;
    println!("Additive geometry candidate and independent refill acceptance passed");
    Ok(())
}
