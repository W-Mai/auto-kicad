use super::*;

#[test]
#[ignore = "requires KiCad PCB Editor, kicad-cli and built stdio server"]
fn live_item_deletion_stdio_lifecycle() {
    let binary =
        std::env::var_os("AUTO_KICAD_DELETE_SERVER").expect("AUTO_KICAD_DELETE_SERVER is required");
    let fixture = TempBoard::new();
    let input = fixture.root.join("board.kicad_pcb");
    let board = format!(
        r#"{}
        (segment (start 5 5) (end 10 5) (width 0.25) (layer "F.Cu") (net 1) (uuid "40000000-0000-4000-8000-000000000002"))
        (segment (start 2 10) (end 3 10) (width 0.25) (layer "F.Cu") (locked yes) (net 1) (uuid "40000000-0000-4000-8000-000000000003"))
        (via (at 7.5 5) (size 0.6) (drill 0.3) (layers "F.Cu" "B.Cu") (net 1) (uuid "50000000-0000-4000-8000-000000000001"))
        (gr_line (start 2 12) (end 4 12) (stroke (width 0.15) (type default)) (layer "Dwgs.User") (uuid "30000000-0000-4000-8000-000000000005"))
    )"#,
        routing_fixture_board()
            .trim_end()
            .strip_suffix(')')
            .unwrap()
    );
    let board = format!(
        r#"{}
      (segment (start 2 11) (end 3 11) (width 0.25) (layer "F.Cu") (net 1) (uuid "40000000-0000-4000-8000-000000000004"))
      (arc (start 2 8) (mid 2.5 7.5) (end 3 8) (width 0.25) (layer "F.Cu") (net 1) (uuid "40000000-0000-4000-8000-000000000005"))
      (via (at 12 12) (size 0.6) (drill 0.3) (layers "F.Cu" "B.Cu") (net 1) (uuid "50000000-0000-4000-8000-000000000002"))
      (gr_line (start 2 13) (end 4 13) (stroke (width 0.15) (type default)) (layer "Dwgs.User") (locked yes) (uuid "30000000-0000-4000-8000-000000000006"))
      (footprint "Test:Associated" (layer "F.Cu") (uuid "10000000-0000-4000-8000-000000000004") (at 15 8) (path "/60000000-0000-4000-8000-000000000001/60000000-0000-4000-8000-000000000002")
        (property "Reference" "J4" (at 0 -1) (layer "F.Fab") (effects (font (size 1 1) (thickness 0.15))))
        (property "Value" "Associated" (at 0 1) (layer "F.Fab") (effects (font (size 1 1) (thickness 0.15))))
        (attr smd) (pad "1" smd rect (at 0 0) (size 0.5 0.5) (layers "F.Cu" "F.Paste" "F.Mask") (net 1 "GND") (uuid "20000000-0000-4000-8000-000000000004")))
      (footprint "Test:Free" (layer "F.Cu") (uuid "10000000-0000-4000-8000-000000000005") (at 13 10)
        (property "Reference" "J5" (at 0 -1) (layer "F.Fab") (effects (font (size 1 1) (thickness 0.15))))
        (property "Value" "Free" (at 0 1) (layer "F.Fab") (effects (font (size 1 1) (thickness 0.15))))
        (attr smd) (pad "1" smd rect (at 0 0) (size 0.5 0.5) (layers "F.Cu" "F.Paste" "F.Mask") (net 1 "GND") (uuid "20000000-0000-4000-8000-000000000005")))
      (footprint "Test:Locked" (layer "F.Cu") (uuid "10000000-0000-4000-8000-000000000006") (at 17 8) (locked yes)
        (property "Reference" "J6" (at 0 -1) (layer "F.Fab") (effects (font (size 1 1) (thickness 0.15))))
        (property "Value" "Locked" (at 0 1) (layer "F.Fab") (effects (font (size 1 1) (thickness 0.15))))
        (attr smd) (pad "1" smd rect (at 0 0) (size 0.5 0.5) (layers "F.Cu" "F.Paste" "F.Mask") (net 1 "GND") (uuid "20000000-0000-4000-8000-000000000006")))
    )"#,
        board.trim_end().strip_suffix(')').unwrap()
    );
    std::fs::write(&input, &board).unwrap();
    std::fs::write(input.with_extension("kicad_pro"), r#"{"schematic":{"top_level_sheets":[{"filename":"board.kicad_sch","name":"board","uuid":"00000000-0000-0000-0000-000000000000"}]}}"#).unwrap();
    let mut editor = OwnedPcbnewLauncher::new(&fixture.root.join("source")).unwrap();
    let copy = editor.create_shadow(&input).unwrap();
    std::fs::write(copy.board_path().with_extension("kicad_sch"),r#"(kicad_sch (version 20250114) (generator "eeschema") (uuid "00000000-0000-0000-0000-000000000000") (paper "A4") (lib_symbols))"#).unwrap();
    let identity = editor.launch(&copy).unwrap();
    for _ in 0..2 {
        editor
            .connect_owned()
            .unwrap()
            .save_board(&identity.board_filename)
            .unwrap();
        editor.reload_owned().unwrap();
    }
    let before = observe_owned_shadow(&mut editor).unwrap();
    let path = fixture.root.join("delete-stdio.json");
    let status = std::process::Command::new("python3")
        .env("PYTHONDONTWRITEBYTECODE", "1")
        .arg(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../../scripts/check-item-deletion.py"),
        )
        .arg(binary)
        .arg(fixture.root.join("state"))
        .arg(&identity.endpoint)
        .arg(copy.board_path())
        .arg(&path)
        .status()
        .unwrap();
    if !status.success() {
        if let Some(output) = std::env::var_os("AUTO_KICAD_DELETE_EVIDENCE") {
            std::fs::copy(&path, output).unwrap();
        }
        panic!("stdio deletion acceptance failed");
    }
    let mut evidence: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
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
    if let Some(output) = std::env::var_os("AUTO_KICAD_DELETE_EVIDENCE") {
        std::fs::write(output, serde_json::to_vec_pretty(&evidence).unwrap()).unwrap();
    }
}
