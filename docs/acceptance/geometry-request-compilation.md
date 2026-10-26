# Bound geometry requests

`kicad-model::geometry` defines strict serializable outline and solid-zone requests. `kicad-engine::geometry` checks the observed revision, board-scoped net identity and name, selected enabled copper layers, target identity and lock state, polygon topology and mutation bounds. Native preparation accepts an immutable adapter observation containing complete geometry, named nets, stackup, document and editor token. `verify_current` reobserves that complete binding immediately before a transaction. Compilation and native preparation do not grant source approval.

The request contains `observed_revision`, optional `outline`, explicit `zones` and `refill`. Zone mutations are tagged `create`, `update` or `delete`; patches change only supplied fields. Rings use integer nanometres and implicit closure. Creation supplies every authored field and fixed inactive native settings, without borrowing another zone. Existing patches retain complete native data. The adapter rejects inactive island thresholds that KiCad would discard on save: `always` and `never` require 10,000,000,000,000 nm²; custom thresholds require `below_area`. Changing an existing nondefault threshold to an inactive mode requires an explicit threshold patch to the saved default.

Whole-board refill is mandatory for zone edits and outline edits on boards with refillable zones. `none` permits only an outline edit on a board without refillable zones. Refill-only requests use an empty zone list, no outline and `all`. Limits include 2 MiB JSON input, 32 mutations, 256 observed zones, 512 polygon vertices, 16 holes, 32 selected layers, 32 outline loops, 512 outline primitives and 100 explicit outline removals. Unknown fields, duplicate mutation targets, invalid source kinds, missing or mismatched nets, disabled layers, locked targets, stale revisions and ineffective native patches reject.

## Actual acceptance

The connected synthetic fixture completed three JSON requests in one owned KiCad 10.0.7 editor: create a concave two-hole GND zone on F.Cu/In1.Cu, update its network to VCC plus F.Cu/B.Cu and connection/fill parameters, then delete it. Each request passed native receipts, complete unedited definition checks, save/reload, native fill, independent closed-CLI copper comparison, exact original tracks/graphics/footprints, unchanged pad partitions and zero introduced DRC. Deletion restored the full baseline definitions and copper. Existing locked and hatched regions remained unchanged. The original fixture retains five baseline DRC warnings and two unconnected items; this is preservation acceptance, not a clean manufacturing board.

CM5 Minima completed explicit updates to zone `2a1e1815-500f-4664-a2ef-ed3623186aa5`, changing clearance from 150,000 to 160,000 nm and restoring 150,000 nm in the same editor. Both stages preserved all 633 pad identities and connection partitions, complete original footprints/tracks/graphics, 14 zones including the locked rule area, protected project bytes and original source files. Each stage verified 31 copper polygons and 90,184 points against an independently refilled copy. Copper changed after the first edit and returned to its original digest after restoration. There were zero introduced DRC items, 171 unchanged baseline violations and zero unconnected items. The separate native Python reader compared saved fields to each original JSON request for both profiles.

The first synthetic attempt rejected an incorrectly prefixed revision in the acceptance template binder. A subsequent unconnected fixture correctly failed DRC for newly isolated copper. The connected fixture initially rejected an unsuitable container-pad query; the acceptance caller uses the existing production footprint-definition reader. These failures and owned-editor cleanup records remain separate from accepted results. Native Python emitted wx initialization diagnostics but completed successfully.

Full quality passed 442 tests with 15 environment-dependent tests ignored, formatting, warnings-denied Clippy and the capability matrix. Architecture checks cover strict bounded parsing, identity/lock/refill constraints and native saved-parameter representability. See [compact evidence](geometry-request-compilation.json).

## Reproduction

Use KiCad 10.0.7 on macOS and new output directories. Each example copies the source, owns one editor, reuses it for every stage and closes it on exit. Run the independent reader after a successful result.

```sh
env CARGO_TARGET_DIR=/private/tmp/auto-kicad-m4-target CARGO_INCREMENTAL=0 CARGO_PROFILE_DEV_DEBUG=0 CARGO_PROFILE_TEST_DEBUG=0 cargo run -p kicad-adapter --example check-geometry-request -- scripts/fixtures/geometry-zones.kicad_pcb scripts/fixtures/geometry-zone-requests.json /absolute/new-synthetic-output
env CARGO_TARGET_DIR=/private/tmp/auto-kicad-m4-target CARGO_INCREMENTAL=0 CARGO_PROFILE_DEV_DEBUG=0 CARGO_PROFILE_TEST_DEBUG=0 cargo run -p kicad-adapter --example check-geometry-request -- '/Library/Application Support/kicad/demos/cm5_minima/CM5_MINIMA_3.kicad_pcb' scripts/fixtures/geometry-cm5-requests.json /absolute/new-cm5-output
/Applications/KiCad/KiCad.app/Contents/Frameworks/Python.framework/Versions/3.9/bin/python3.9 scripts/check-geometry-request.py /absolute/new-synthetic-output
/Applications/KiCad/KiCad.app/Contents/Frameworks/Python.framework/Versions/3.9/bin/python3.9 scripts/check-geometry-request.py /absolute/new-cm5-output
```

The example's `${revision}`, `${net:NAME}` and `${created:ID}` placeholders bind fixture inputs to actual observations and creation receipts. They are acceptance-template syntax, not public API fields. The typed request digest serializes as 64 hexadecimal characters. Saved requests retain exact values for independent verification.

This evidence covers typed compilation and zone-only native mutation/refill. General outline requests reuse the separately accepted outline compiler; mixed outline/zone execution is not established here. Public MCP geometry inspection, plan/preview/approval/recovery, teardrop regeneration, GUI cancellation and committed maximum combined acceptance are not implemented or accepted by this record. No complete milestone or product-delivery claim is made.

[Production MCP geometry inspection](geometry-inspection.md) provides the observed zone identities, locks, definitions and geometry revision for authoring requests. Mutation planning and approval require separate acceptance.
