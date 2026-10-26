# Saved outline verification

`kicad_checks::outline::check_saved_outline` checks the complete saved boundary, including retained board and footprint graphics, on a private copy of PCB/project bytes. Its immutable result binds both input SHA-256 values, native boundary counts, holes, areas, complete curve bounds and an independent topology DRC report. It requires KiCad 10.0.7 and the saved project's `invalid_outline` severity explicitly set to `error`.

The embedded native worker requires exact degree-two endpoint chains and disables inferred rectangular fallback. The native neighbor search uses a 1 nm radius after exact endpoint validation; zero radius finds no neighbors, including coincident endpoints. Native default polygon interpretation verifies curves and cutouts without rewriting authored geometry. Empty/zero-area boundaries, unsupported Edge.Cuts items and exceeded bounds reject. The checker covers up to 4,096 edge objects, 4,096 native outer boundaries/holes and 500,000 native polygon points; inputs are each bounded to 32 MiB, and native output to 64 KiB.

Circular-arc bounds use integer circumcenter numerators, exact sweep membership and integer square-root intervals. Bounds round outward to integer nanometres, retaining the complete long or short sweep. Original coordinates remain unchanged. This avoids the observed native display bounding box that covered only the short sweep for a valid three-point major arc. Native board construction and CLI DRC independently interpret the authored curve.

Native construction alone is insufficient: a crossing between a closed straight boundary and a curved boundary can be normalized into a result even when DRC reports it invalid. The checker therefore also runs `kicad-cli pcb drc --severity-all` on the same private snapshot and rejects every final `invalid_outline`, including exclusions. Missing/disabled severity rejects before spawning. This is an absolute final-state check, independent of a baseline-difference policy. The shared DRC reader limits reports to 32 MiB. A common deadline bounds the native worker and CLI to at most 60 seconds; owned process groups terminate on timeout, and private snapshots are removed on success or failure.

Native `GetBoardPolygonOutlines` with the direct curved-polygon option triggered an unsupported Boolean-operation assertion, and `IsSelfIntersecting()` reported true for the accepted simplified result. Neither is used as the checker’s pass/fail oracle. The accepted implementation uses exact endpoint checks, default native construction, integer arc bounds and absolute topology DRC together.

## Reproduction

```sh
cargo build -p kicad-checks --example check-outline-geometry
target/debug/examples/check-outline-geometry /absolute/board.kicad_pcb

cargo build -p kicad-adapter --example check-native-outline
target/debug/examples/check-native-outline /absolute/new-live-output
```

The saved-board checker opens no editor. The native outline example opens one owned editor, replaces the requested 4 edges with 14 exact segment/arc primitives, saves and reloads in the same process, invokes this checker, verifies all creation receipts and unrelated native content, checks complete DRC deltas, then closes its editor. All saved coordinates, widths, layers and primitive types are independently compared with original JSON intent through bundled native Python.

`scripts/check-native-outline-geometry.py` accepts an output directory from `check-native-outline`, the checker executable and a new evidence directory. Run it with KiCad's bundled Python. Its 13 isolated cases accept curved multiple-boundary/cutout geometry, retained native circles/rectangles/polygons and a valid major arc. They reject disabled topology rules, open chains, a 1 nm gap, straight crossing, arc/line crossing, overlapping circles, tangent circles, empty boundaries, doubled zero-area arcs and a major-arc span exceeding 1,000 mm despite bounded control points. Every original/candidate PCB and project file remains byte-identical during verification; temporary snapshots are removed.

## Acceptance

Actual KiCad 10.0.7 passed all 13 saved-copy controls. The integrated editor run accepted two outer boundaries and two holes, preserved the locked circle and unrelated graphics/tracks, matched saved/reloaded native receipts and original intent, and introduced zero DRC violations. Unconnected items remained zero; two existing dangling-track warnings remained unchanged. The editor exited after one launch. The 512-creation cancellation/513-refusal probe also passed; it does not establish committed maximum-size MCP acceptance.

Full offline quality passed 417 tests with 15 environment-dependent tests ignored, format validation, warnings-denied Clippy and capability validation. A temporary arithmetic test independently covered 3,960 known-circle sweeps in both directions at different scales/translations plus the major-arc regression; that temporary script was removed. Retained architecture tests cover immutable evidence binding, version/bounds requirements, disabled rules, nonregular/symlink inputs, timeout cleanup and malformed worker responses. See [compact evidence](saved-outline-verification.json).

This checker establishes saved-candidate topology evidence. It does not establish all DRC rules, source approval/recovery, complete unrelated-object preservation, zone refill or production MCP execution. The independent checker’s private copy does not replace the source file. Analytic curve-envelope validation before IPC creation remains required; a saved-result check cannot prevent an unsafe request from reaching native creation. Public geometry MCP integration, zone operations and final real-project profile acceptance remain unfinished.
