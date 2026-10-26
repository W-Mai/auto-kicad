use super::*;

#[test]
#[ignore = "requires the built stdio server, KiCad PCB Editor and CLI"]
fn live_existing_footprint_flip_stdio_lifecycle() {
    let binary = std::env::var_os("AUTO_KICAD_FLIP_SERVER")
        .expect("set AUTO_KICAD_FLIP_SERVER to the built auto-kicad executable");
    let fixture = TempBoard::new();
    let input = fixture.root.join("board.kicad_pcb");
    let board = routing_fixture_board()
        .replace("(at 5 5)", "(at 5 5 30) (path \"/50000000-0000-4000-8000-000000000001/50000000-0000-4000-8000-000000000002\")")
        .replacen("(pad \"1\" smd rect (at 0 0)", "(pad \"1\" smd rect (at 0.5 0.2 30)", 1)
        .replace("(at 10 5)", "(locked yes) (at 10 5)");
    let board = board.replacen("(attr smd)", r#"(fp_line (start -0.7 -0.6) (end 0.9 -0.4) (stroke (width 0.1) (type default)) (layer "F.Fab") (uuid "60000000-0000-4000-8000-000000000001"))
        (fp_circle (center 0.3 0.2) (end 0.5 0.2) (stroke (width 0.1) (type default)) (fill none) (layer "F.Fab") (uuid "60000000-0000-4000-8000-000000000002"))
        (fp_arc (start -0.6 0) (mid -0.4 -0.2) (end -0.2 0) (stroke (width 0.1) (type default)) (layer "F.Fab") (uuid "60000000-0000-4000-8000-000000000003"))
        (attr smd)"#, 1);
    let board = board.replace("          (segment (start 7.5 5)", r#"
          (footprint "Test:BackPair" (layer "B.Cu") (uuid "10000000-0000-4000-8000-000000000004") (at 13 10 90)
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
    let board = format!(
        r#"{}
      (footprint "Test:FlipCollision" (layer "F.Cu") (uuid "10000000-0000-4000-8000-000000000005") (at 16 5)
        (property "Reference" "J5" (at 0 -1) (layer "F.Fab") (effects (font (size 1 1) (thickness 0.15))))
        (property "Value" "FlipCollision" (at 0 2) (layer "F.Fab") (effects (font (size 1 1) (thickness 0.15))))
        (attr smd) (pad "1" smd rect (at 0 1) (size 1 1) (layers "F.Cu" "F.Paste" "F.Mask") (net 1 "GND") (uuid "20000000-0000-4000-8000-000000000006")))
      (footprint "Test:CollisionObstacle" (layer "B.Cu") (uuid "10000000-0000-4000-8000-000000000006") (at 16 4)
        (property "Reference" "J6" (at 0 -1) (layer "B.Fab") (effects (font (size 1 1) (thickness 0.15))))
        (property "Value" "CollisionObstacle" (at 0 2) (layer "B.Fab") (effects (font (size 1 1) (thickness 0.15))))
        (attr smd) (pad "1" smd rect (at 0 0) (size 1 1) (layers "B.Cu" "B.Paste" "B.Mask") (net 2 "OTHER") (uuid "20000000-0000-4000-8000-000000000007")))
    )"#,
        board.trim_end().strip_suffix(')').unwrap()
    );
    let board = board.replace(
        r#"(layer "B.Fab") (effects (font (size 1 1) (thickness 0.15))))"#,
        r#"(layer "B.Fab") (effects (font (size 1 1) (thickness 0.15)) (justify mirror)))"#,
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
    let evidence_path = fixture.root.join("flip-stdio.json");
    let status = std::process::Command::new("python3")
        .env("PYTHONDONTWRITEBYTECODE", "1")
        .arg(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../../scripts/check-footprint-flip.py"),
        )
        .arg(binary)
        .arg(fixture.root.join("state"))
        .arg(&identity.endpoint)
        .arg(copy.board_path())
        .arg(&evidence_path)
        .status()
        .unwrap();
    if !status.success() {
        if let Some(path) = std::env::var_os("AUTO_KICAD_FLIP_EVIDENCE") {
            std::fs::copy(&evidence_path, path).unwrap();
        }
        panic!("stdio flip acceptance failed");
    }
    let mut evidence: Value =
        serde_json::from_slice(&std::fs::read(evidence_path).unwrap()).unwrap();
    editor.reload_owned().unwrap();
    let after = observe_owned_shadow(&mut editor).unwrap();
    assert_eq!(
        serde_json::to_value(&after.snapshot).unwrap(),
        evidence["after"]
    );
    assert_eq!(before.snapshot, after.snapshot);
    assert_eq!(std::fs::read_to_string(&input).unwrap(), board);
    editor.close_owned(identity.pid).unwrap();
    editor.cleanup().unwrap();
    assert!(!copy.root().exists());
    evidence["source_pid"] = json!(identity.pid);
    evidence["source_editor_reused"] = json!(true);
    evidence["same_process_reload_verified"] = json!(true);
    evidence["owned_editors_closed"] = json!(true);
    evidence["fixture_cleaned"] = json!(true);
    if let Some(path) = std::env::var_os("AUTO_KICAD_FLIP_EVIDENCE") {
        std::fs::write(path, serde_json::to_vec_pretty(&evidence).unwrap()).unwrap();
    }
}
