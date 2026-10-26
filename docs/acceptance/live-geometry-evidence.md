# Live geometry and zone lock evidence

`kicad_adapter::live_geometry::observe_live_geometry` binds repeated native geometry and full live board serialization to the previously established editor token and document. It verifies exact KiCad 10.0.7, including token continuity during the version query, and requires identical native records and live text across repeated reads. The caller still binds source files, rules, source revision and approvals; this observation grants no write permission.

`LiveBoardText` retains the complete CST and original source text, including unknown fields and filled copper. Board and per-zone structural SHA-256 digests ignore only whitespace between lexical tokens. Quoting, escapes, numeric spelling, child order, coordinates, derived fields and unknown data remain significant. These digests are observation evidence, not a normalized fill comparator or a definition-only comparison. Exact zone source spans remain available through `zone_text`.

Lock parsing requires native board format `20260206`. A missing zone `locked` field means unlocked in that verified format; explicit values must be `yes` or `no`. Duplicate or missing identities, duplicate/unknown locks and unsupported format versions reject. The complete live/native zone UUID sets must agree. Known native lock values must agree with live text; `LS_UNKNOWN` is accepted only with the independent live-text lock evidence. The live text reader must be used to bind this evidence to the editor; a stale saved file alone cannot establish its current locks.

Before CST allocation, input is bounded to 32 MiB, depth 64 and 3,000,000 lexical/list nodes. Up to 256 zone records are accepted. Empty text, NUL, unbalanced forms and unterminated strings reject. Unknown forms remain retained, but parsing them is not a claim that they can be edited.

## Reproduction and evidence

```sh
cargo build -p kicad-adapter --example check-native-geometry
target/debug/examples/check-native-geometry "/Library/Application Support/kicad/demos/cm5_minima/CM5_MINIMA_3.kicad_pcb" /absolute/new-output
```

The extended single-editor probe compares every parsed zone lock with KiCad's native parser, rejects an incomplete text zone set, and injects two faults into real IPC responses: a changed version-response instance token and a changed zone lock in one live-text response. Each must reject with its exact expected error. Injection changes only received bytes; it does not change the editor or simulate an actual user lock operation. A fresh observation after injection must reproduce the original live text. Original project files and saved copy bytes remain unchanged, and the owned editor is closed.

Actual KiCad 10.0.7 acceptance passed on CM5 Minima: all 14 zone locks matched the native parser, including the locked rule area omitted by IPC. The existing 70 graphic identities and 31 filled polygons also matched. Both injected drifts and incomplete text identities were rejected. The complete 3,276,590-byte live text remained stable; see [compact evidence](live-geometry-evidence.json). The bundled Python emitted the known wxApp initialization diagnostic and exited successfully.

Offline quality passed 408 tests with 15 environment-dependent tests ignored, format validation, warnings-denied Clippy and capability validation. Architecture tests verify lossless/unknown-field retention, whitespace-only structural equivalence, lock/identity ambiguity rejection, format restrictions and parser allocation bounds. General outlines, zone writes, cancellation/timeout handling, full definition/fill separation and production geometry MCP acceptance remain outside this evidence.

[Refill definition preservation](refill-definition-preservation.md) provides separate immutable fingerprints for strict live/native definitions and closed CLI input/output comparison. The original complete structure digest still includes every fill polygon and display field.
