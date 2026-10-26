# Native solid-zone definition editing

The backend creates, patches and explicitly deletes ordinary solid copper zones on exact KiCad 10.0.7. `PreparedSolidZoneEdits` binds the request to complete live/native evidence and independently observed text locks. Mixed operations run inside a caller-owned native transaction. Locked targets, repeated targets, unsupported zone types, missing or ambiguous nets, disabled copper layers, stale observations and ineffective patches reject. Native net names bind identity; deprecated numeric net codes are not written.

`SolidZonePayload` retains complete native definitions. Typed patches change only listed fields and invalidate filled copper. Exact receipts preserve all remaining protobuf data, including unknown fields; only assigned creation identity/parent and classified empty per-layer fill containers are accepted as native additions. Saved verification compares complete native sets, checks receipts against the request, and hashes all unedited live board fields, including fields absent from IPC. Whole-board text is retained; patched fields are omitted individually from the preservation comparison, while their expected native values remain mandatory. A successful definition edit does not establish completed refill, electrical connectivity, DRC acceptance or source approval.

`kicad-geometry::polygon_bounds` validates one straight exterior with disjoint holes using exact bounded integer predicates. Both windings and forward collinear subdivisions are accepted without changing coordinates. Crossings, backtracking, duplicate/touching vertices, zero area and outside/nested/overlapping/touching holes reject. Limits are 32 mutations, 256 observed zones, 512 vertices and 16 holes per edited zone, 32 copper layers, coordinates within ±2,000 mm, 1,000 mm span and 32 MiB aggregate definition data. Curved and hatched authoring are unsupported; retained hatched zones remain unchanged.

## Actual acceptance

One owned PCB Editor created a multilayer concave zone with two holes, patched its name/priority/clearance, then changed its net, layer set, connection mode, thermal settings, minimum thickness and island-removal parameters. A mixed create/update/delete transaction and a restoring transaction passed complete live and saved verification. Every stage reused the same process. Final deletion/restoration returned the saved PCB and protected project to their original bytes. Existing tracks, board graphics, a locked copper zone and a hatched zone remained unchanged. The owned editor exited and the original fixture files were preserved.

The maximum 32-creation transaction included one zone with 512 vertices and 16 holes; exact native receipts passed, and cancellation restored the complete native/live state. A 33-item batch rejected. Invalid geometry, unknown network and disabled copper layer requests rejected without board changes. These maximum creations were cancelled, not committed or passed through MCP. A separate KiCad Python reader checked all coordinates, both holes, layers, networks and edited parameters in three saved files against the original test intent. Its read-only checks passed; wx initialization/image-handler diagnostics did not prevent the helper from completing.

Full quality passed 432 tests with 15 environment-dependent tests ignored, warnings-denied Clippy, formatting and capability validation. Architecture tests cover unknown-field preservation, strict receipt comparison, patch-specific text preservation and polygon topology/resource constraints. See [compact evidence](native-solid-zone-edits.json).

The first live attempt rejected before mutation because it incorrectly required a numeric network code. The pinned protocol and native serialization showed name-only identities; the native request was corrected accordingly. That failed attempt and its cleanup record remain separate from accepted evidence.

## Reproduction

Run on macOS with KiCad 10.0.7 installed. Choose a new output directory for each run; the example owns one disposable editor and closes it on exit.

```sh
env CARGO_TARGET_DIR=/private/tmp/auto-kicad-m4-target CARGO_INCREMENTAL=0 CARGO_PROFILE_DEV_DEBUG=0 CARGO_PROFILE_TEST_DEBUG=0 cargo run -p kicad-adapter --example check-native-zones -- /absolute/new-output
/Applications/KiCad/KiCad.app/Contents/Frameworks/Python.framework/Versions/3.9/bin/python3.9 scripts/check-native-zones.py /absolute/new-output
```

The saved boards, request/response records and compact results remain in the output directory. This acceptance covers native definition editing on a synthetic fixture. Public geometry schemas/planning, whole-board fill completion/cancellation, copper equivalence, teardrop regeneration, populated-board zone editing, production MCP approval/recovery and committed maximum-batch acceptance remain required. No combined milestone or full product completion is asserted.
