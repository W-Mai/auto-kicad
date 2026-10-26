# Native geometry observation

`KiCadClient::capture_board_geometry` reads complete board-level zones and graphic shapes, retaining native geometry, settings, filled polygons and unknown protobuf fields. Every record requires a unique bounded identity; unrequested item types reject. The request binds the instance token and complete board/project identity before and after the query. `NativeBoardGeometry::evidence_bytes` encodes complete records in deterministic kind/identity order; it does not discard fields or normalize saved state.

`KiCadClient::live_board_text` reads the current board through `SaveDocumentToString` without saving it. The response document must match the observed document exactly, and the instance/document must remain stable. This supplies live native evidence for fields absent from IPC records. KiCad 10.0.7 omits zone lock state: `LS_UNKNOWN` must never be interpreted as unlocked. The adapter provides [bound live-text lock evidence](live-geometry-evidence.md); this IPC reader alone grants no mutation permission.

Observation bounds are 256 zones, 4,096 board graphic shapes, 500,000 zone boundary/fill control points and 32 MiB of native geometry payload or live board text. Arc nodes count as three control points. The NNG transport limits individual incoming messages to 64 MiB before protobuf decoding. Unsupported geometry/settings remain complete raw records; successful observation does not declare them editable.

## Reproduction

```sh
cargo build -p kicad-adapter --example check-native-geometry
target/debug/examples/check-native-geometry "/Library/Application Support/kicad/demos/cm5_minima/CM5_MINIMA_3.kicad_pcb" /absolute/new-output
```

The macOS probe opens one owned board copy, captures geometry around live serialization, verifies stable native records and unchanged saved PCB bytes, then loads the live string with KiCad's bundled Python parser. Exact zone/graphic UUID sets, copper layers, filled state, outline counts and per-layer filled polygon/vertex counts must match. It retains native JSON, complete protobuf evidence and live PCB text, closes its editor, removes the working copy and verifies unchanged original PCB/project/rules/schematic files.

Actual KiCad 10.0.7 acceptance on CM5 Minima observed 14 zones, 70 graphics and 31 filled polygons across a six-layer board. The locked rule area `f9c2f127-515b-4e2f-b007-625a8e1a09e6` retained its lock in live text while IPC reported enum value 0 (`LS_UNKNOWN`). Live serialization contained 3,276,590 bytes. Native/text comparisons, stable repeated reads, original-file isolation and owned-editor cleanup passed. The bundled Python emitted a wxApp initialization diagnostic but exited successfully after all comparisons. See [compact evidence](native-geometry-observation.json).

Offline quality passed with 405 tests and 15 environment-dependent tests ignored, format checks, warnings-denied Clippy and capability validation. Contract tests cover complete payload retention, unknown fields, duplicate/missing/foreign IDs, bounds and document/token drift. This is read-only backend acceptance; it does not establish zone editing, native refill, disconnected-island connectivity or production geometry MCP support.
