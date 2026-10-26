# Mixed native geometry transactions

`PreparedGeometryChange::apply_in_transaction` applies solid-zone creation, updates and deletion together with explicit outline replacement inside a caller-owned native commit. It revalidates the complete immutable geometry observation before mutation. `GeometryMutationReceipts` binds private native receipts to the exact compiled request. Definition verification checks every resulting native zone and shape, then compares all unedited live-text fields. Existing zone-only verification continues to reject any graphics change.

The combined text comparison excludes only the exact removed and created direct Edge.Cuts object identities after complete native receipt verification. Unedited outlines, other layers, nested footprint content, unknown board fields and unedited fields inside patched zones remain significant. Receipt identity, deletion-set and count mismatches reject. A successful definition commit still requires separate topology, refill, electrical and approval checks; it is not atomic with native zone filling.

## Actual acceptance

One owned KiCad 10.0.7 editor completed a cancelled mixed transaction followed by 4 committed requests. The requests combined curved outline replacement with concave two-hole multilayer zone creation, rectangular outline replacement with network/layer/thermal/island parameter changes, curved outline replacement with zone deletion, and a final outline-only replacement on the zoned board. Cancellation restored the complete geometry observation and original board items. Every committed request passed exact native definition verification before and after saved reload, native boundary construction with absolute invalid-outline DRC, full live/saved/independent-CLI copper comparison, complete footprint and unrelated item preservation, unchanged pad partitions and zero introduced DRC.

The fixture retains 5 baseline DRC warnings and 2 unconnected items; this is preservation acceptance. Locked and hatched zones remained unchanged. An independent native Python reader matched every authored zone field and all 16 created outline primitives, including exact arc midpoints and undirected endpoint pairs. The final rectangle has newly assigned native identities, so the fixture does not claim restoration of the original whole-board definition digest.

CM5 Minima separately exercised the same executor with zone-only clearance changes from 150,000 to 160,000 nm and back. Both saved results passed independent authored-field comparison, complete definition and copper verification, preservation of 633 pad identities and connection partitions, 171 unchanged DRC items and zero unconnected items. Each stage verified all 14 zones, 31 copper polygons and 90,184 points. The final CM5 definition and copper evidence exactly matched its baseline. Both original sources and protected projects remained unchanged; both owned editors closed.

Full quality passed 447 tests, with 15 environment-dependent tests ignored, formatting, warnings-denied Clippy and the capability matrix. Projection architecture checks verify that exact direct outline exclusions cannot hide unknown fields, other layers, duplicate identities, footprint content, tracks or unrequested text. See [compact evidence](mixed-geometry-transactions.json).

## Reproduction

Run from the repository on macOS with KiCad 10.0.7 and new output directories. Each runner uses one editor for all requests and closes it at the end.

```sh
env CARGO_TARGET_DIR=/private/tmp/auto-kicad-m4-target CARGO_INCREMENTAL=0 CARGO_PROFILE_DEV_DEBUG=0 CARGO_PROFILE_TEST_DEBUG=0 cargo run -p kicad-adapter --example check-geometry-request -- scripts/fixtures/geometry-zones.kicad_pcb scripts/fixtures/geometry-mixed-requests.json /absolute/new-mixed-output
env CARGO_TARGET_DIR=/private/tmp/auto-kicad-m4-target CARGO_INCREMENTAL=0 CARGO_PROFILE_DEV_DEBUG=0 CARGO_PROFILE_TEST_DEBUG=0 cargo run -p kicad-adapter --example check-geometry-request -- '/Library/Application Support/kicad/demos/cm5_minima/CM5_MINIMA_3.kicad_pcb' scripts/fixtures/geometry-cm5-requests.json /absolute/new-cm5-output
/Applications/KiCad/KiCad.app/Contents/Frameworks/Python.framework/Versions/3.9/bin/python3.9 scripts/check-geometry-request.py /absolute/new-mixed-output
/Applications/KiCad/KiCad.app/Contents/Frameworks/Python.framework/Versions/3.9/bin/python3.9 scripts/check-geometry-request.py /absolute/new-cm5-output
```

`${outline_ids}` is an acceptance-template placeholder for actual observed outline identities. The saved request files contain exact UUID arrays. Request binding and creation identity placeholders remain confined to the runner.

This record establishes combined backend definition execution. Geometry mutation through MCP, preview approval and source reconciliation, durable uncertain-state recovery, GUI fill cancellation, derived teardrop regeneration, zoned additive routing and committed maximum combined batches require separate acceptance. The cancelled definition transaction is not evidence of GUI fill cancellation. CM5 is a zone-only regression here; populated mixed MCP acceptance is not claimed.
