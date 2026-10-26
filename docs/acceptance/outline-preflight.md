# Complete outline bounds before native creation

`kicad-geometry` provides one integer segment/arc envelope implementation for both outline request compilation and typed native creation. Complete selected sweeps must remain within ±2,000 mm, and the union of all requested primitives must fit a 1,000 mm span. Control-point bounds alone are insufficient for major or nearly collinear arcs. Invalid envelopes reject before any transport request. Retained boundaries still require the complete saved topology check.

The calculation uses bounded inputs, exact arbitrary-integer circumcenters and sweep predicates, and integer square-root intervals. Envelope limits round outward to integer nanometres; authored coordinates remain unchanged. The compiler and IPC depend on the pure geometry crate independently. The saved native Python checker retains its separate calculation and native/CLI topology checks.

KiCad 10.0.7 can swap an arc's start/end during winding conversion. Creation receipts accept only the exact reversed endpoint pair with an unchanged midpoint and unchanged requested attributes. Saved/reloaded native payloads then compare exactly, omitting only the transient board parent. Midpoint changes reject even if the native constructor reports success. The independent saved-intent witness separately checks all control coordinates and reports reversed endpoint pairs. No positional tolerance, snapping or midpoint substitution is permitted.

## Reproduction

```sh
cargo build -p kicad-adapter --example check-native-outline
target/debug/examples/check-native-outline /absolute/new-output
```

The example launches one owned editor. It rejects three oversized complete-envelope batches while proving native geometry, live serialization and saved PCB bytes unchanged. It also rejects native midpoint rewriting and cancels that provisional transaction. A valid major-arc boundary is created, saved, reloaded and verified against complete native topology and DRC. Its complete board bounds are `[0, 0, 215000000, 104000000]` nm. An independent parser confirms the original midpoint and exactly one reversed arc endpoint pair.

Deleting the test boundary restores exact persistent native items, zones, PCB bytes and project bytes. Same-process reload assigns a different transient board parent; subsequent observation/deletion checks bind to the freshly read document. The example then cancels 512 native creations, refuses 513, replaces 4 original edges with 14 primitives, verifies independent original-intent correspondence, preserves unrelated content and checks open/crossing/copper-collision DRC controls. Owned-window cleanup and original input preservation are mandatory. Failed runs retain `failure.json`; restoration failures additionally retain complete before/after diagnostics.

## Acceptance

Actual KiCad 10.0.7 passed the complete example with zero introduced DRC violations and zero unconnected items. All saved receipts, independent intent checks, restoration checks and cleanup passed. Full offline quality passed 422 tests with 15 environment-dependent tests ignored, format validation, warnings-denied Clippy and capability validation. Geometry architecture tests cover 3,960 known-circle sweeps plus reversed directions at different scales/translations. A temporary independent Python/Rust comparison passed 1,200 seeded cases, including 200 near-collinear inputs, then its scaffolding was removed. See [compact evidence](outline-preflight.json).

Failed attempts exposed an incorrect extremum-direction assertion and a comparison of transient board parents across reload; both acceptance assertions were corrected after inspecting exact native/file evidence. The accepted result retains both complete saved files and independent intent/topology records. Bundled native Python emits a wx application-initialization diagnostic during read-only parsing; both intent checks exit successfully and their exact comparisons pass.

This acceptance establishes backend envelope preflight and native outline verification. General-geometry MCP planning, approval/recovery, zone editing/refill and final real-project profiles remain unfinished. Arcs whose native constructor changes the midpoint remain rejected. The 512-item test proves provisional creation/cancellation, not committed maximum-size MCP behavior.
