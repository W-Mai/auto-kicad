# auto-kicad

A Rust MCP server for inspecting KiCad PCB state and applying bounded, approval-bound changes over IPC.

## Build and run

```sh
CARGO_INCREMENTAL=0 cargo build --release --locked -p auto-kicad
python3 scripts/install-local.py target/release/auto-kicad
codex mcp add auto-kicad -- "$HOME/.local/bin/auto-kicad"
codex mcp get auto-kicad
```

The installer copies the executable into `~/.local/lib/auto-kicad` and writes a launcher into `~/.local/bin`. `--prefix` selects another installation root. The launcher adds the macOS KiCad CLI directory to `PATH`; the embedded fixed footprint helper needs no source checkout. KiCad and its bundled Python runtime must remain installed. The build directory can be removed after installation and verification. Restart the Codex session to load the registered server. Set `tool_timeout_sec = 180` under `[mcp_servers.auto-kicad]` in the Codex configuration to allow editor startup, reload and DRC.

The server uses MCP over standard input/output. Configure an MCP client to launch the built executable. KiCad 10.0.7 is the verified protocol baseline. `kicad_inspect_project` can query [routing rules for existing net IDs](docs/acceptance/routing-rule-inspection.md), including effective netclass preferences and saved minima; native evidence details remain available to backend callers; [its read-only probe and boundaries](docs/acceptance/routing-rule-observation.md) are documented separately from routing support. Native track construction supports explicit copper layers with [receipt and saved-reload evidence](docs/acceptance/copper-layer-creation.md); public single-track requests accept an optional canonical `layer`, defaulting to F.Cu. [Explicit route requests](docs/acceptance/copper-route-mcp.md) bind native/rule evidence to preview and approval and support pad, track-end and via anchors. [Populated four-layer copy acceptance](docs/acceptance/populated-copper-route.md) verifies a SIM_VDD connection and rejection of approvals after unsaved stackup changes. Enable the KiCad IPC API and open the target PCB before using board tools. The endpoint identifies the editor instance; on macOS it is normally under `/tmp/kicad/`.

Backend callers can read [complete native geometry and live board text](docs/acceptance/native-geometry-observation.md), including filled polygons and evidence of zone locks omitted by IPC. This read-only interface does not expose zone editing or refill through MCP.

## Board workflow

1. Save the PCB and project settings in KiCad.
2. Call `kicad_inspect_project` for the target editor endpoint.
3. Call `kicad_plan_change` to prepare board graphics, a rectangular Edge.Cuts outline, pad-bound tracks on selected copper layers, explicit multilayer routes, net-bound through vias, source-bound footprints or an atomic connected footprint assembly.
4. Call `kicad_preview_change` to validate a disposable copy. Review its result before approving.
5. Call `kicad_commit_change` with the exact plan, validation evidence and approval hashes returned by the preview.

Preview copies the saved PCB, project and optional custom design rules. It verifies that the copy matches the editor state, applies the proposal in an owned editor, saves and reloads it in the same editor process, and compares the result and DRC changes. Source locks stay in place. Unsaved board differences are rejected. Source file, editor identity or observed content changes invalidate approval.

## Scope

PCB observation covers supported graphics, footprint placements and pads, routing and stackup metadata. The public authoring path supports board graphics, rectangular outlines, pad-bound straight tracks on enabled copper layers, explicit multilayer routes and net-bound through vias, including mixed additive batches. Routing previews remap pad identities into the disposable board and verify server-assigned creation IDs, geometry and networks. Graphics, pad-bound straight tracks and through vias have passed native and production MCP plan/preview/commit acceptance. Source-bound single-footprint placement has passed production MCP planning, disposable preview and approved IPC commit with the official R0603 library. It binds a closed saved schematic, its CLI netlist and the resolved library file and table provenance to the plan hash, verifies symbol paths and pin-to-pad network assignments, and rejects source changes. Template preparation reloads a confined staging copy in the existing owned editor. An `assembly` plan places multiple source-bound footprints and connects their pins in one native transaction. Connections use placement target IDs and pad numbers; native pad IDs and coordinates are resolved from validated creation receipts. All placements must share one saved schematic revision. Separate `footprint` requests remain supported. Unsupported objects or lossy observations can reject inspection.

Schematic authoring is restricted to the fixed two-resistor operation exposed by the schematic tools when its backend is available. Both resistor instances specify `Resistor_SMD:R_0603_1608Metric`. General schematic editing, schematic-to-PCB synchronization, zone authoring, automatic placement/routing through MCP and manufacturing release are not implemented. Sessions and approvals expire on restart. The stdio server persists execution evidence, outcomes and pre-write source backups under `$HOME/.local/state/auto-kicad`; `AUTO_KICAD_STATE_DIR` selects a different private directory. `kicad_inspect_project` returns recent execution records and backup paths. An interrupted commit remains uncertain until its board/files are inspected; recovery requires a fresh plan, validation and approval. Source files are never restored automatically. Keep backups until the corresponding changes have been reviewed.

Guarded recovery uses `kicad_plan_recovery`, `kicad_preview_recovery` and `kicad_commit_recovery` when a durable state directory is configured. Close all project editors before recovery; editor locks are rejected and never removed by these tools. Source bytes must match the recorded post-execution fingerprints, and backups must match their original digests. Interrupted or legacy executions without post-execution evidence permit only unchanged source bytes. Plans and approvals expire on server restart. Recovery replaces each file atomically, verifies the complete restored file set, and reports any post-write or journal ambiguity as `unknown`. Multi-file restoration is not a filesystem-wide transaction; concurrent external writers must remain stopped. See [recovery contract and acceptance](docs/acceptance/guarded-recovery.md).

PCB and schematic commits acquire an OS writer lock in `.auto-kicad-writer.lock` at the canonical project directory. Multiple server instances share this lock even with different state directories. The file stays in place after release; it is separate from KiCad editor locks. Durable recording must succeed before mutation. An approval with a recorded completed or uncertain attempt cannot be replayed.

## Footprint sources

Placement resolves exact local KiCad project/global library identifiers and aliases. The legacy resistor, LED and Type-C mappings remain compatible. Sources with unsupported geometry or unverified native fabrication properties reject. KiCad 10.0.7 preserves the official thermal QFN’s physical pads and heatsink properties through creation and saved reload. See [source configuration, bounds and actual acceptance](docs/acceptance/general-footprint-sources.md). Save and close the same-named `.kicad_sch` before planning. Its reference, value and footprint field must match the request. The preview returns the source-bound operations for review.

Existing footprint movement and rotation use `move_footprint` in `kicad_plan_change`. Read `placement_id`, native `source_uuid` and `observed_revision` from `kicad_inspect_project` → `data.pcb`. Supply optional paired `x_nm`/`y_nm` absolute coordinates, optional `orientation_mdeg`, or both. At least one transform is required. Orientation is normalized modulo 360000; `copper_tolerance_nm` defaults to 250000 and is bounded to 1–250000. Connected tracks/vias, zones, unsupported copper and unsupported child geometry reject. Accepted changes preserve native identities, rigid child geometry, nets and associations, then pass full saved/reloaded state and DRC checks. Front and back transforms have passed actual KiCad 10.0.6 + stdio MCP acceptance. See [the request contract, limits and reproducible evidence](docs/acceptance/existing-footprint-transform.md).

Existing tracks, through vias and Dwgs.User segments use `edit_existing` with the current observed revision and 1–100 explicit target identities. Typed field patches preserve unrelated native data, nets and existing pad connections; isolated preview and saved source verification enforce DRC and protected-file checks. See [the field contract and native stdio acceptance](docs/acceptance/existing-item-edit.md). Existing free footprints use `flip_footprint` with an explicit target side and axis; connected/locked targets reject, and native reflection, saved state and DRC must pass. See [the flip contract and acceptance](docs/acceptance/footprint-flip.md). `delete_items` removes an explicit set of supported tracks, through vias, Dwgs.User segments or free unassociated footprints while preserving surviving pad connections; see [the deletion contract](docs/acceptance/item-deletion.md). [Populated-board copy acceptance](docs/acceptance/populated-existing-edit.md) verifies a saved route-width edit with complete preservation and DRC evidence.

An assembly request is exclusive with `footprint`, `batch` and other top-level operation fields. Placements plus connections are limited to 100 items. Each connection requires two unambiguous front-copper pads on the same nonempty schematic net. A failed operation before commit drops the complete native transaction; uncertain commit/save results require inspection and a fresh approval.

```json
{
  "endpoint": "ipc:///tmp/kicad/api.sock",
  "source_run_id": "resistor-circuit",
  "request_id": "place-connected-resistors",
  "assembly": {
    "placements": [
      {"target_id": "r1", "library_nickname": "Resistor_SMD", "library_entry_name": "R_0603_1608Metric", "x_nm": 5000000, "y_nm": 5000000, "reference": "R1", "value": "1k"},
      {"target_id": "r2", "library_nickname": "Resistor_SMD", "library_entry_name": "R_0603_1608Metric", "x_nm": 10000000, "y_nm": 5000000, "reference": "R2", "value": "1k"}
    ],
    "connections": [
      {"track_id": "r1-r2", "start": {"target_id": "r1", "pad_number": "2"}, "end": {"target_id": "r2", "pad_number": "1"}, "width_nm": 250000}
    ]
  }
}
```

Use the endpoint returned for the target editor. The matching saved schematic must already contain R1 and R2 with the specified values and footprint fields, with R1.2 and R2.1 connected. Preview evidence binds template byte digests and symbol/pin associations together with the plan, source fingerprints, saved snapshot and DRC result.

`AUTO_KICAD_OFFICIAL_FOOTPRINT_ROOT` selects the installed `footprints` directory; the server resolves the corresponding `.pretty` subdirectory. `AUTO_KICAD_PROJECT_FOOTPRINT_ROOT` selects a USB-C `.pretty` directory, defaulting to `Type-C.pretty` inside its project directory. MCP requests cannot supply executable, script or library paths. The fixed template helper is embedded in the executable and uses KiCad's bundled macOS Python runtime. MCP input cannot replace its program.

`kicad_capabilities` reports enabled tool names directly from the MCP router. Authoring marked `live_verification_pending` has not completed live acceptance; each request still requires its normal source, shadow, DRC and approval checks.

## Verification

The checked-in [capability matrix](schemas/capability-matrix.json) records each advertised capability's status, constraints, tested KiCad family, evidence command and record. `kicad_capabilities` returns this matrix together with the current runtime status, so an unavailable backend is not reported as verified. `scripts/check-capability-matrix.py` fails if router tool names drift from the matrix, verified entries lose evidence, or deferred entries are accidentally advertised. The same check runs in the repository quality workflow.

The repository quality workflow and `scripts/quality.py` are the offline gate: they run the matrix validator, format check, warnings-denied Clippy and default workspace tests without starting KiCad or using a user project. Environment-dependent live tests remain ignored unless explicitly selected; their commands and disposable-fixture requirements are listed below.

```sh
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

Environment-dependent tests are ignored by default. The CLI-only source-binding test exercises schematic plan, preview and commit through the MCP router with the production CLI, then verifies footprint source binding; it requires official libraries and opens no editor. PCB live tests require the installed editor and CLI. Run each explicitly; they create disposable projects and close only their owned editor processes. Preview requests are serialized to avoid concurrent preview windows. The combined circuit test launches one owned editor and reloads in the same process. The mixed-routing PCB MCP test starts one source editor and one isolated preview editor. It checks changed-source rejection before preview startup, then reuses each process for save/reload. Failed post-commit assertions retain a diagnostic JSON file in the system temporary directory; its path is reported with the failure.

```sh
cargo test -p auto-kicad tests::production_cli_binds_fixed_circuit_footprints_without_editor -- --ignored --exact
cargo test -p auto-kicad tests::source_bound_circuit_and_routing_reuse_one_owned_editor -- --ignored --exact
cargo test -p auto-kicad tests::disposable_graphics_shadow_verifies_server_assigned_ids -- --ignored --exact
cargo test -p auto-kicad tests::live_mcp_saved_routing_lifecycle_uses_two_editors -- --ignored --exact
cargo test -p auto-kicad tests::live_mcp_source_bound_footprint_lifecycle_uses_two_editors -- --ignored --exact
cargo test -p auto-kicad tests::live_mcp_connected_assembly_lifecycle_uses_two_editors -- --ignored --exact
cargo test -p auto-kicad tests::recovery_acceptance::live_recovery_restores_native_board_and_allows_next_mcp_commit --lib -- --ignored --exact
# Existing-footprint stdio acceptance requires AUTO_KICAD_MOVE_SERVER; see its linked contract.
```

The CLI-only MCP schematic lifecycle and source-binding test passed with installed KiCad, including unchanged source bytes during preview, exact candidate bytes after commit, idempotent replay, durable execution history and pre-write backups. Single-editor circuit acceptance passed with KiCad 10.0.6: both resistor placements and schematic pin/pad/net bindings, native graphics and connected-track creation, complete snapshot comparison after save/reload, and no introduced DRC violations. A separately added one-layer-connected via survived save/reload and produced only the expected dangling-via violation. The same editor PID was retained throughout; owned-editor cleanup and source board/project/schematic isolation passed. Offline contracts cover pad-coordinate transforms and serialized through-via inner shapes. Production MCP mixed graphics/track/via acceptance passed with one source editor and one preview editor: changed-source rejection, isolated save/reload and DRC, exact approval commit, replay rejection, committed-source DRC, preservation of existing objects, durable history and pre-write backups. Both owned editors and their disposable files were cleaned. The source-bound footprint MCP test also passed: it creates the fixed two-resistor schematic through MCP, places R1 through the PCB tools, and verifies saved symbol paths, pin/pad/net assignments, unchanged schematic bytes, DRC and both execution records. Atomic assembly MCP acceptance also passed from an empty PCB: both resistors and their connecting track were created through one approved plan, with zero unconnected items after save/reload and no introduced DRC violations. Injecting a verification failure into the second placement discarded the complete transaction and preserved the source board. Source isolation, exact approval, replay rejection, durable backups and owned-editor cleanup passed.

Native explicit copper paths have passed separate 4-layer primitive acceptance with pad, track-end and via anchors, atomic rollback, saved reload and connectivity checks. Public route requests also pass the MCP approval lifecycle, including [256-copper and 100-graphic mixed-batch acceptance](docs/acceptance/copper-route-boundary.md). See [native route contracts and evidence](docs/acceptance/native-copper-routes.md).

## Native placement and routing replay

`auto-kicad-replay` rebuilds an owned PCB copy one component at a time through native IPC, then creates deterministic simple routes. It uses one PCB Editor process for placement, routing and saved reload, and closes that process afterward. This local utility is separate from the MCP approval workflow.

```sh
CARGO_INCREMENTAL=0 cargo build --release --locked -p auto-kicad --bins
python3 scripts/install-local.py target/release/auto-kicad --replay-binary target/release/auto-kicad-replay
~/.local/bin/auto-kicad-replay /absolute/input.kicad_pcb /absolute/new-output-directory 250
```

The input must be an unrouted front-side SMT PCB without zones and with matching `.kicad_pro` settings. macOS KiCad and its bundled Python runtime are required. The last argument controls the pause between steps in milliseconds (0–5000; default 250). The output directory must not exist. The utility preserves input files and placement coordinates; it does not optimize component placement. Ctrl+C cancels playback and closes the owned editor.

Routing sorts same-net pad pairs by distance and tries straight, orthogonal and 45-degree paths, rejecting foreign-net pad and track collisions. Candidate paths use 0.2 mm width and clearance with an additional geometric margin, then undergo native DRC before IPC creation. Final checks compare every pad's geometry and net assignment, saved/reloaded observations, route counts, source hashes and introduced DRC violations. Board-level drawings other than Edge.Cuts are hidden in the replay copy; DRC deltas use that same prepared baseline.

Each result contains `replay.html` with interactive playback and seeking, `events.jsonl`, `route-plan.json`, baseline/candidate/final DRC reports, `summary.json`, and the saved native PCB/project under `result/`. The HTML embeds the executed geometry and can be opened directly. Working copies are removed after success; the installed executable and results do not depend on Cargo build outputs.

Two PawChrona pet-collar native replays produced identical route geometry: 38 components, 201 preserved pads, 58 connections and 80 copper segments. Unconnected items decreased from 101 to 43 with zero introduced DRC violations. This is partial routing: zones, vias, differential pairs, impedance and power-current requirements remain outside the algorithm. Existing input violations are retained; the result is not a manufacturing release.

Native IPC field updates, flip, exact-set deletion receipts and pad connectivity primitives have a separate [protocol acceptance record](docs/acceptance/existing-item-protocol.md). Public field editing, footprint flipping and exact-set deletion have separate MCP acceptance contracts.

## Installed-server acceptance

`scripts/check-codex.py` uses the local Codex app-server protocol to initialize the registered `auto-kicad` server and verify tool discovery. It starts no model turn. This check was verified with Codex CLI 0.162.1.

```sh
PYTHONDONTWRITEBYTECODE=1 python3 scripts/check-codex.py --output /tmp/auto-kicad-codex.json
cargo run --locked -p auto-kicad --example check-board-copy -- /absolute/project/board.kicad_pcb "$HOME/.local/bin/auto-kicad" /absolute/new-acceptance-directory
```

The board-copy check requires a saved same-named `.kicad_pro`, Python 3 and KiCad. The output directory must not exist. It opens an owned copy, launches the installed stdio server, validates a 3 mm marker on Dwgs.User at (80, 78) mm, commits with exact approval evidence, rejects invalid approval and replay, verifies backups, compares DRC, and reloads the saved PCB in the same editor. It compares existing footprints, pads, routing and stackup, closes owned editors and removes working copies. The output retains JSON evidence, private backups and a saved result PCB/project. The input project is not edited.

PawChrona Air8000A copy acceptance passed with 38 components, 201 pads, 36 named nets and 4 copper layers. The Circuit JSON input was converted offline to an isolated KiCad PCB and all pad positions, sizes and nets were checked after load, including 17 U1 ground pads. Installed stdio plan/preview/commit, saved reload, source isolation, exact approval, replay rejection, durable backups and editor cleanup passed. The baseline and final DRC both contained 155 report items and 101 unconnected items, with zero introduced violations. This is an unrouted evaluation board; this acceptance does not establish manufacturing readiness or general schematic conversion support.
