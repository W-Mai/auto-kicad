# Populated PCB copy acceptance

An owned copy of the saved PawChrona PCB passed an existing-track edit through production stdio MCP and KiCad 10.0.6 on 2026-10-26. The source is a populated, partially routed prototype with 38 footprints, 201 pads, 80 track segments, no vias and 4 copper layers. This acceptance uses the saved native PCB; it does not rerun the placement/routing demonstration.

One existing 3 mm front-copper GND segment, from `(91.5, 96.5)` mm to `(91.5, 99.5)` mm, was widened from 0.20 to 0.25 mm. The persisted PCB diff contains exactly that width change. Every footprint, pad, net, other track, board graphic and stackup field remains unchanged. Complete native payload checks, pad connectivity, independent normalized expectations, protected project files and saved reload passed. One source editor was reused; the isolated preview and owned source editor were closed and their working copies cleaned. Before/after native project files and small evidence records were retained outside the repository.

The [result record](populated-existing-edit-result.json) contains source/result hashes, counts and verification outcomes. DRC retained its baseline of 49 violations and 43 unconnected items, with no introduced violations or increased unconnected count. Those pre-existing findings remain unresolved. The result verifies a bounded editing workflow; it is not a completed board, electrical approval or the final three-project delivery gate. The installed Codex MCP executable was not replaced; the test used the built production stdio executable.

## Reproduction

`check-existing-edit-copy` requires a saved PCB and its matching project file, a built server executable and a new output directory. It opens only an owned copy and preserves the original PCB/project/rules/schematic bytes. The acceptance script chooses one unlocked straight track at least 1 mm long and increases its width by 0.05 mm. Its conservative geometry screening requires at least 0.4 mm estimated clearance from foreign-net copper. Native isolated preview and CLI DRC remain authoritative; the heuristic does not approve changes. No candidate or a rejected preview fails without automatically trying another edit.

```sh
env CARGO_TARGET_DIR=/tmp/auto-kicad-copy-target CARGO_INCREMENTAL=0 CARGO_PROFILE_DEV_DEBUG=0 CARGO_PROFILE_TEST_DEBUG=0 cargo build -p auto-kicad --bin auto-kicad --example check-existing-edit-copy
/tmp/auto-kicad-copy-target/debug/examples/check-existing-edit-copy SOURCE.kicad_pcb /tmp/auto-kicad-copy-target/debug/auto-kicad NEW_OUTPUT_DIR
```

Successful output contains `before/board.kicad_pcb`, `result/board.kicad_pcb`, matching project files, `selection.json`, `result.json`, `snapshot.json`, `cleanup.json`, the MCP transcript and durable execution records. The source editor is reloaded in the same process before the result is retained. Do not reuse an existing output directory. Remove the temporary Cargo target after the command and quality checks finish; retain the resulting native PCB.

The complete quality gate passed 367 tests, with 15 environment-dependent tests ignored, plus formatting and warnings-denied Clippy. The populated-copy command ran separately against the native editor.
