# Native zone refill and complete copper verification

`KiCadClient::request_zone_refill` dispatches the fixed whole-board fill action on exact KiCad 10.0.7. It consumes the client, checks the bound editor/document before and after dispatch, and refuses an active native transaction. `Returned` proves only an action response; complete result verification is separate. `Uncertain` retains a read-only client with no retry, save, reload or rollback methods. Production NNG transport disables automatic request retransmission. The transport deadline is bounded by 60 seconds.

`FilledCopperEvidence` fingerprints every direct-zone UUID, fill flag, copper layer, island marker and point/arc coordinate. Decimal coordinates convert exactly to integer nanometres; polygon ordering, cyclic start and exact reversed winding normalize. All coordinates, repeated vertices, polygons and arc midpoints remain significant. Parsing rejects unknown derived fields and enforces 500,000 control points within the complete 32 MiB board limit. Least cyclic rotation uses linear time even for repeated vertices.

`refill_closed_copy` owns a private bounded PCB/project/rules snapshot, invokes the fixed CLI refill/save/DRC command, checks its version/source and protects original file bytes. Its worker has a process deadline and output limit; timeout terminates the owned process group. `VerifiedRefillContent` combines complete live/native definition preservation with equality of live, saved and independently refilled copper. Saved CLI definitions use the separate automatic-dimension comparison described in [definition preservation](refill-definition-preservation.md). The independent file never replaces the editor's source.

## Verified serialization difference

Hatched fill added an empty direct `setup/zone_defaults` container. The [pinned serializer](https://raw.githubusercontent.com/KiCad/kicad-source-mirror/10.0.7/pcbnew/pcb_io/kicad_sexpr/pcb_io_kicad_sexpr.cpp) emits the container for an internal property map but omits entries without a configured offset. The [pinned parser](https://raw.githubusercontent.com/KiCad/kicad-source-mirror/10.0.7/pcbnew/pcb_io/kicad_sexpr/pcb_io_kicad_sexpr_parser.cpp) applies only contained properties. Refill definition comparisons therefore treat exactly one empty direct container as absent. Nonempty defaults, unknown properties, nested occurrences and all actual settings remain exact; duplicate setup/default containers reject. Full structural revision and zone-edit preservation do not apply this projection. The rejected initial snapshots remain in the local evidence directory.

## Actual acceptance

| Profile | Zones | Filled polygons | Control points | Result |
| --- | ---: | ---: | ---: | --- |
| CM5 Minima | 14 | 31 | 90,184 | Complete native/live/saved/CLI equivalence, including locked rule area |
| Disconnected multilayer islands | 2 | 3 | 48 | Same two expected two-pad clusters before and after reload |
| Solid, locked and hatched zones | 3 | 3 | 924 | Complete definitions and hatched copper preserved |
| No zones | 0 | 0 | 0 | Explicit unchanged empty result |
| Copper zones covered by a keepout | 4 | 0 | 0 | Three filled flags with no resulting copper; no connection claim |
| Fully isolated zones with island removal enabled | 3 | 3 | 924 | Actual retained copper compared exactly |

Each profile used one owned editor, two native fills and same-process saved reload. Complete definitions, settled project bytes and original source files passed preservation checks. The independent CLI matched all saved copper. Unfilled observations rejected where their fill evidence differed; the no-zone profile correctly had no difference. Every owned editor closed. The isolated-zone test initially expected empty copper, but native 10.0.7 retains initially fully isolated layers; a covering keepout established the separate zero-polygon case. No empty-fill or connectivity claim is inferred from island-removal settings alone.

A separate actual transport test sent one accepted native fill request with a 1 ms receive timeout. It returned `Uncertain`; read-only observation subsequently saw 13 filled CM5 zones and preserved definitions. There was no additional action, save, reload or rollback, and saved files remained unchanged. An attempted dispatch during an open native transaction was refused before sending the action. This proves timeout isolation, not GUI cancellation or completed recovery.

Full quality passed 439 tests with 15 environment-dependent tests ignored, formatting, warnings-denied Clippy and capability validation. Architecture checks cover exact geometry canonicalization, unknown-field preservation, version/transaction binding, no automatic resend, private-copy source protection, worker deadlines, and narrowly scoped empty-container normalization. See [compact evidence](complete-zone-refill.json).

## Reproduction and scope

Run on macOS with KiCad 10.0.7 installed and `kicad-cli` on PATH. Each output directory must be new. Original source files remain unchanged; native results, complete snapshots, hashes and cleanup records are retained.

```sh
env CARGO_TARGET_DIR=/private/tmp/auto-kicad-m4-target CARGO_INCREMENTAL=0 CARGO_PROFILE_DEV_DEBUG=0 CARGO_PROFILE_TEST_DEBUG=0 cargo build -p kicad-adapter --example check-zone-refill --example check-zone-refill-timeout
/private/tmp/auto-kicad-m4-target/debug/examples/check-zone-refill /absolute/source.kicad_pcb /absolute/new-output
/private/tmp/auto-kicad-m4-target/debug/examples/check-zone-refill-timeout /absolute/cm5-source.kicad_pcb /absolute/new-timeout-output
```

The hatched fixture is generated by [native solid-zone acceptance](native-solid-zone-edits.md). Local empty-copper and isolated-zone source directories retain exact generation records and native PCB/project files. Native Python preparation emits a wx initialization diagnostic but completes successfully.

This evidence covers backend dispatch and content verification. It does not establish public MCP geometry approval/recovery, populated-board zone editing, teardrop regeneration, GUI cancellation, complete DRC-delta acceptance or manufacturing approval. The private refill checker does not import local footprint libraries; its DRC report is not a substitute for the caller's complete project-context DRC check. Callers must independently bind live/saved identity, verify connectivity and validate approval before reporting a successful source edit.
