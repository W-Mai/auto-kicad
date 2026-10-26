//! Verify typed outline replacement and retained native content in one owned editor.
use kicad_adapter::lifecycle::{DisposablePcbLifecycle, OwnedPcbnewLauncher};
use kicad_adapter::live_geometry::observe_live_geometry;
use kicad_adapter::outline::{compile_outline_change, native_outline_primitives, outline_revision};
use kicad_checks::{CheckResult, DrcPolicy, compare_drc, run_pcb_drc};
use kicad_ipc::{CreatedOutlineReceipt, NativeBoardGeometry, OutlineGeometry, OutlinePrimitive};
use kicad_model::{OutlineEdit, RevisionDigest, StableId};
use serde_json::json;
use std::{
    env, fs,
    path::{Path, PathBuf},
    time::Duration,
};
type Error = Box<dyn std::error::Error>;
fn segment(a: (i64, i64), b: (i64, i64)) -> OutlinePrimitive {
    OutlinePrimitive {
        geometry: OutlineGeometry::Segment { start: a, end: b },
        width_nm: 50_000,
    }
}
fn drc(board: &Path) -> Result<kicad_checks::DrcReport, Error> {
    match run_pcb_drc(
        board,
        &DrcPolicy {
            allowed_root: board.parent().ok_or("missing parent")?.to_path_buf(),
            timeout: Duration::from_secs(60),
        },
    ) {
        CheckResult::Completed(report) => Ok(report),
        CheckResult::Failed(error) => Err(format!("DRC failed: {error:?}").into()),
    }
}
fn verify_created(
    receipts: &[CreatedOutlineReceipt],
    actual: &NativeBoardGeometry,
) -> Result<(), Error> {
    for receipt in receipts {
        if !receipt.matches_saved(
            actual
                .shapes()
                .get(&receipt.source_uuid)
                .ok_or("created outline missing")?,
        ) {
            return Err(format!("saved outline differs: {}", receipt.source_uuid).into());
        }
    }
    Ok(())
}
fn main() -> Result<(), Error> {
    let output = PathBuf::from(
        env::args()
            .nth(1)
            .ok_or("usage: check-native-outline NEW_OUTPUT")?,
    );
    fs::create_dir(&output)?;
    let output = fs::canonicalize(output)?;
    let fixture = output.join("fixture");
    fs::create_dir(&fixture)?;
    let board = fixture.join("board.kicad_pcb");
    let base = include_str!("../../../scripts/fixtures/layered-routing-rules.kicad_pcb");
    let extra = r#"
      (gr_circle (center 70 60) (end 74 60) (stroke (width 0.05) (type solid)) (fill none) (layer "Edge.Cuts") (locked yes) (uuid "60000000-0000-4000-8000-000000000001"))
      (gr_rect (start 2 2) (end 6 6) (stroke (width 0.1) (type solid)) (fill none) (layer "Dwgs.User") (uuid "60000000-0000-4000-8000-000000000002"))
      (gr_poly (pts (xy 2 20) (xy 6 20) (xy 4 24)) (stroke (width 0.1) (type solid)) (fill none) (layer "Dwgs.User") (uuid "60000000-0000-4000-8000-000000000003"))
    "#;
    fs::write(
        &board,
        format!(
            "{}{extra})",
            &base[..base.rfind(')').ok_or("missing fixture end")?]
        ),
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
    let result: Result<_, Error> = (|| {
        let mut ipc = editor.connect_owned()?;
        let version = ipc.get_version()?;
        ipc.save_board(&identity.board_filename)?;
        editor.reload_owned()?;
        let baseline = ipc.capture_existing_board_items(&identity.board_filename)?;
        let geometry = ipc.capture_board_geometry(&identity.board_filename)?;
        let protected = fs::read(copy.project_path())?;
        let baseline_drc = drc(copy.board_path())?;
        let remove = (1..=4)
            .map(|n| format!("30000000-0000-4000-8000-{n:012}"))
            .collect::<Vec<_>>();
        for ids in [
            vec!["60000000-0000-4000-8000-000000000001".into()],
            vec!["60000000-0000-4000-8000-000000000002".into()],
            vec!["missing".into()],
            vec![remove[0].clone(), remove[0].clone()],
        ] {
            if ipc
                .delete_outline_items(&identity.board_filename, &geometry, &ids)
                .is_ok()
            {
                return Err("invalid outline removal accepted".into());
            }
        }
        let maximum = (0..512)
            .map(|i| segment((i * 100_000, 100_000_000), ((i + 1) * 100_000, 100_000_000)))
            .collect::<Vec<_>>();
        if ipc
            .create_outline_primitives(&identity.board_filename, &vec![maximum[0].clone(); 513])
            .is_ok()
        {
            return Err("oversized native batch accepted".into());
        }
        let commit = ipc.begin_commit()?;
        let trial = ipc.create_outline_primitives(&identity.board_filename, &maximum);
        ipc.drop_commit(&commit)?;
        if trial?.len() != 512 || ipc.capture_board_geometry(&identity.board_filename)? != geometry
        {
            return Err("maximum native batch cancellation failed".into());
        }
        let intent: OutlineEdit =
            serde_json::from_str(include_str!("../../../scripts/fixtures/outline-edit.json"))?;
        let observed = observe_live_geometry(&mut ipc, &identity.board_filename)?;
        let revision = outline_revision(&observed)?;
        let compiled = compile_outline_change(&intent, &revision, &observed)?;
        if !compiled.requires_native_topology_check()
            || compiled
                .removed_ids()
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                != remove
        {
            return Err(
                "compiled outline lost required native verification or exact removals".into(),
            );
        }
        let mut stale = revision.as_str().to_owned();
        stale.replace_range(..1, if stale.starts_with('0') { "1" } else { "0" });
        if compile_outline_change(&intent, &RevisionDigest::new(stale)?, &observed).is_ok() {
            return Err("stale outline revision accepted".into());
        }
        let mut invalid = intent.clone();
        invalid.loops[0].edges.pop();
        if compile_outline_change(&invalid, &revision, &observed).is_ok() {
            return Err("open compiled outline accepted".into());
        }
        invalid = intent.clone();
        invalid.remove_ids = vec![StableId::new("60000000-0000-4000-8000-000000000001")?];
        if compile_outline_change(&invalid, &revision, &observed).is_ok() {
            return Err("compiled locked removal accepted".into());
        }
        let requested = native_outline_primitives(&compiled);
        fs::write(
            output.join("intent.json"),
            serde_json::to_vec_pretty(&intent)?,
        )?;
        fs::write(
            output.join("compiled.json"),
            serde_json::to_vec_pretty(
                &json!({"revision":revision,"native_topology_required":true,"primitives":compiled.primitives().iter().map(|p|json!({"target_id":p.target_id,"edge":p.edge,"width_nm":p.width_nm})).collect::<Vec<_>>()}),
            )?,
        )?;
        let commit = ipc.begin_commit()?;
        let edits = (|| {
            ipc.delete_outline_items(&identity.board_filename, &geometry, &remove)?;
            ipc.create_outline_primitives(&identity.board_filename, &requested)
        })();
        let receipts = match edits {
            Ok(value) => value,
            Err(error) => {
                ipc.drop_commit(&commit)?;
                return Err(error.into());
            }
        };
        ipc.commit_changes(
            &commit,
            "Replace board outline with curved and concave boundaries",
        )?;
        ipc.save_board(&identity.board_filename)?;
        let saved = ipc.capture_board_geometry(&identity.board_filename)?;
        verify_created(&receipts, &saved)?;
        editor.reload_owned()?;
        let reloaded = ipc.capture_board_geometry(&identity.board_filename)?;
        verify_created(&receipts, &reloaded)?;
        let actual = ipc.capture_existing_board_items(&identity.board_filename)?;
        let ids = receipts
            .iter()
            .map(|r| r.source_uuid.clone())
            .collect::<Vec<_>>();
        if !baseline.matches_after_outline_replacement(&actual, &remove, &ids)?
            || fs::read(copy.project_path())? != protected
            || !reloaded.zones().is_empty()
        {
            return Err("outline replacement altered unrelated content".into());
        }
        let after_drc = drc(copy.board_path())?;
        let delta = compare_drc(&baseline_drc, &after_drc);
        fs::write(
            output.join("baseline-drc.json"),
            serde_json::to_vec_pretty(&baseline_drc)?,
        )?;
        fs::write(
            output.join("final-drc.json"),
            serde_json::to_vec_pretty(&after_drc)?,
        )?;
        if !delta.introduced.is_empty()
            || after_drc.unconnected_items > baseline_drc.unconnected_items
        {
            return Err(format!("outline DRC regression: {delta:?}").into());
        }
        let retained = output.join("result");
        fs::create_dir(&retained)?;
        fs::copy(copy.board_path(), retained.join("board.kicad_pcb"))?;
        fs::copy(copy.project_path(), retained.join("board.kicad_pro"))?;
        fs::write(
            output.join("baseline-items.pb"),
            baseline.persistent_evidence()?,
        )?;
        fs::write(output.join("final-items.pb"), actual.persistent_evidence()?)?;
        Ok(
            json!({"kicad_version":version.full_version,"pid":identity.pid,"created":ids,"removed":remove,"arcs":2,"compiled_from_serialized_intent":true,"compiler_stale_open_locked_rejected":true,"native_maximum_cancelled":512,"oversized_rejected":513,"locked_wrong_layer_missing_duplicate_removals_rejected":true,"unrelated_native_payloads_preserved":true,"saved_receipts_exact":true,"same_process_reload":true,"project_preserved":true,"introduced_drc":delta.introduced.len(),"baseline_unconnected":baseline_drc.unconnected_items,"final_unconnected":after_drc.unconnected_items,"scope":"native outline primitives and exact replacement; not general topology planning or MCP acceptance"}),
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
            &json!({"pid":identity.pid,"owned_editor_closed":true,"original_unchanged":true}),
        )?,
    )?;
    let evidence = result?;
    fs::write(
        output.join("result.json"),
        serde_json::to_vec_pretty(&evidence)?,
    )?;
    let script =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../scripts/check-outline-topology.py");
    let status=std::process::Command::new("/Applications/KiCad/KiCad.app/Contents/Frameworks/Python.framework/Versions/3.9/bin/python3.9")
        .env("PYTHONDONTWRITEBYTECODE","1").arg(script).arg(&output).status()?;
    if !status.success() {
        return Err("native outline DRC negative controls failed".into());
    }
    fs::remove_dir(output.join("work"))?;
    println!("{evidence}");
    Ok(())
}
