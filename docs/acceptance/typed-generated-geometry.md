# Typed geometry with generated regions

`observe_generated_context` binds complete native/text observations to one editor, document, net set and layer stack. A separate authored view supports ordinary zone and outline edits. Native extraction requires the exact classified generated UUID set and preserves all remaining records, including unknown fields. The complete observation remains available for revisions and preservation checks.

Generated zones are visible as noneditable refill targets. Native writes revalidate the complete object set. `verify_generated_definitions` requires unchanged generated identities and complete content during definition edits. A separate reload verifier permits only complete generated UUID correspondence after a caller-established saved reload; observations from another editor or document reject. The ordinary definition and candidate entry points reject attempts to omit a nonempty generated partition.

`GeometryCandidateEvidence::capture_generated` verifies typed definition receipts, whole-board refill, saved reload and independent CLI copper/mask content. Baseline and result digests retain every generated field except native UUIDs. `generated_mapping` returns a separate generated identity map only after complete candidate equivalence. Explicit creation receipts retain their own map. These adapter APIs do not grant source approval or establish a complete electrical regression gate.

## Reproduction

Use the extended fixture described in [generated refill acceptance](teardrop-refill.md): ordinary solid copper plus 10 pad/via and track-end regions on front/back copper and mask layers.

```sh
env CARGO_TARGET_DIR=/private/tmp/auto-kicad-m4-target CARGO_INCREMENTAL=0 CARGO_PROFILE_DEV_DEBUG=0 CARGO_PROFILE_TEST_DEBUG=0 CARGO_BUILD_JOBS=1 cargo build -p kicad-adapter --example check-generated-correspondence --example check-geometry-request
/private/tmp/auto-kicad-m4-target/debug/examples/check-generated-correspondence INPUT/teardrops.kicad_pcb NEW_GENERATED_OUTPUT
/Applications/KiCad/KiCad.app/Contents/Frameworks/Python.framework/Versions/3.9/bin/python3.9 scripts/check-generated-correspondence.py NEW_GENERATED_OUTPUT
/private/tmp/auto-kicad-m4-target/debug/examples/check-geometry-request scripts/fixtures/geometry-zones.kicad_pcb scripts/fixtures/geometry-mixed-requests.json NEW_ORDINARY_OUTPUT
```

The generated runner uses two sequential owned editors, reusing each through definition editing, refill, reload and repeated DRC. The independent Python reader checks saved clearance, assigned generated UUIDs, native serialization and complete raw DRC mapping. The ordinary runner exercises mixed outline/zone creation, updates and deletion in one editor.

## Evidence

Actual KiCad 10.0.7 acceptance passed on two owned copies with 10 generated regions each. Both applied the typed 200000-to-250000 nm ordinary-zone clearance change, verified exact mutation identities, saved/reloaded through the dedicated generated correspondence gate, refilled and produced equal complete candidates. All 10 generated UUIDs differed across copies. Complete raw DRC matched after the generated identity map; unmapped reports differed. Both reports contained 12 violations and 4 unconnected diagnostics. This is candidate correspondence evidence, not a DRC-clean board or a baseline electrical regression claim.

Direct generated-region editing, omission through ordinary definition/candidate verifiers and use of the mutation verifier for reloaded identities were refused. Independent checks recomputed generated content digests, compared complete candidates and mappings, checked noneditable refill targets, and loaded both saved results through native Python. Original inputs stayed unchanged.

Ordinary regression passed 4 mixed outline/zone stages with cancellation, exact saved intent, complete refill, preserved pad clusters and zero introduced DRC. The final authored outline intentionally differs from the original baseline; the runner reports restored_baseline=false. Independent Python verified all 4 saved results.

Full quality passed: 467 tests, zero failures, 15 environment-dependent tests ignored, formatting, warning-denied workspace/all-target clippy and the capability matrix. All 7 source editor PIDs from preliminary/final generated checks and ordinary regression were verified absent. Exact paths, digests and artifact hashes are in [the machine-readable evidence](typed-generated-geometry.json).

Public generated-zone MCP writes remain unavailable until private DRC project/library context and approval/recovery integration are complete. Full product qualification remains open.
