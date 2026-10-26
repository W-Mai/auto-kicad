use super::*;

#[test]
#[ignore = "requires the built stdio server, KiCad PCB Editor and CLI"]
fn live_existing_footprint_stdio_lifecycle() {
    let binary = std::env::var_os("AUTO_KICAD_MOVE_SERVER")
        .expect("set AUTO_KICAD_MOVE_SERVER to the built auto-kicad executable");
    let fixture = TempBoard::new();
    let input = fixture.root.join("board.kicad_pcb");
    let board = routing_fixture_board()
        .replace("(at 5 5)", "(at 5 5) (path \"/50000000-0000-4000-8000-000000000001/50000000-0000-4000-8000-000000000002\")")
        .replacen("(pad \"1\" smd rect (at 0 0)", "(pad \"1\" smd rect (at 0.5 0.2 30)", 1)
        .replace("(at 10 5)", "(locked yes) (at 10 5)");
    let board = board.replace("          (segment (start 7.5 5)", r#"
          (footprint "Test:BackPair" (layer "B.Cu") (uuid "10000000-0000-4000-8000-000000000004") (at 13 10)
            (property "Reference" "J4" (at 0 -1) (layer "B.Fab") (effects (font (size 1 1) (thickness 0.15))))
            (property "Value" "BackPair" (at 0 1) (layer "B.Fab") (effects (font (size 1 1) (thickness 0.15))))
            (attr smd)
            (pad "1" smd rect (at 0.5 0.2 30) (size 0.5 0.3) (layers "B.Cu" "B.Paste" "B.Mask") (net 1 "GND") (uuid "20000000-0000-4000-8000-000000000004"))
            (pad "2" smd rect (at -0.5 -0.2 30) (size 0.5 0.3) (layers "B.Cu" "B.Paste" "B.Mask") (net 1 "GND") (uuid "20000000-0000-4000-8000-000000000005")))
          (via (at 7.5 8) (size 0.6) (drill 0.3) (layers "F.Cu" "B.Cu") (net 1) (uuid "40000000-0000-4000-8000-000000000002"))
          (segment (start 7.5 5)"#);
    let board = board
        .replace(
            "(setup (pad_to_mask_clearance 0)) (net 1 \"GND\")",
            "(setup (pad_to_mask_clearance 0)) (net 1 \"GND\") (net 2 \"OTHER\")",
        )
        .replace(
            "(net 1 \"GND\") (uuid \"20000000-0000-4000-8000-000000000002\")",
            "(net 2 \"OTHER\") (uuid \"20000000-0000-4000-8000-000000000002\")",
        );
    std::fs::write(
        input.with_extension("kicad_dru"),
        r#"(version 1) (rule "Fixture clearance" (constraint clearance (min 0.25mm)))"#,
    )
    .unwrap();
    std::fs::write(&input, &board).unwrap();
    std::fs::write(input.with_extension("kicad_pro"), r#"{"schematic":{"top_level_sheets":[{"filename":"board.kicad_sch","name":"board","uuid":"00000000-0000-0000-0000-000000000000"}]}}"#).unwrap();
    let mut editor = OwnedPcbnewLauncher::new(&fixture.root.join("source")).unwrap();
    let copy = editor.create_shadow(&input).unwrap();
    let schematic = r#"(kicad_sch (version 20250114) (generator "eeschema") (uuid "00000000-0000-0000-0000-000000000000") (paper "A4") (lib_symbols))"#;
    std::fs::write(copy.board_path().with_extension("kicad_sch"), schematic).unwrap();
    let identity = editor.launch(&copy).unwrap();
    editor
        .connect_owned()
        .unwrap()
        .save_board(&identity.board_filename)
        .unwrap();
    editor.reload_owned().unwrap();
    // Settle project metadata migrations in the hand-written fixture before acceptance.
    editor
        .connect_owned()
        .unwrap()
        .save_board(&identity.board_filename)
        .unwrap();
    editor.reload_owned().unwrap();
    let before = observe_owned_shadow(&mut editor).unwrap();
    let evidence_path = fixture.root.join("move-stdio.json");
    let status = std::process::Command::new("python3")
        .env("PYTHONDONTWRITEBYTECODE", "1")
        .arg(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../../scripts/check-footprint-move.py"),
        )
        .arg(binary)
        .arg(fixture.root.join("state"))
        .arg(&identity.endpoint)
        .arg(copy.board_path())
        .arg(&evidence_path)
        .status()
        .unwrap();
    if !status.success() {
        if let Some(path) = std::env::var_os("AUTO_KICAD_MOVE_EVIDENCE") {
            std::fs::copy(&evidence_path, path).unwrap();
        }
        panic!("stdio movement acceptance failed");
    }
    let mut evidence: Value =
        serde_json::from_slice(&std::fs::read(evidence_path).unwrap()).unwrap();
    editor.reload_owned().unwrap();
    let after = observe_owned_shadow(&mut editor).unwrap();
    assert_eq!(
        serde_json::to_value(&after.snapshot).unwrap(),
        evidence["after"]
    );
    assert_ne!(before.snapshot, after.snapshot);
    assert_eq!(std::fs::read_to_string(&input).unwrap(), board);
    editor.close_owned(identity.pid).unwrap();
    editor.cleanup().unwrap();
    assert!(!copy.root().exists());
    evidence["source_pid"] = json!(identity.pid);
    evidence["source_editor_reused"] = json!(true);
    evidence["same_process_reload_verified"] = json!(true);
    evidence["owned_editors_closed"] = json!(true);
    evidence["fixture_cleaned"] = json!(true);
    if let Some(path) = std::env::var_os("AUTO_KICAD_MOVE_EVIDENCE") {
        std::fs::write(path, serde_json::to_vec_pretty(&evidence).unwrap()).unwrap();
    }
}
