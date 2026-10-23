# auto-kicad

A Rust MCP server for inspecting KiCad PCB state and applying bounded, approval-bound changes over IPC.

## Build and run

```sh
cargo build --release -p auto-kicad
./target/release/auto-kicad
```

The server uses MCP over standard input/output. Configure an MCP client to launch the built executable. KiCad 10.0.6 is the verified protocol baseline. Enable the KiCad IPC API and open the target PCB before using board tools. The endpoint identifies the editor instance; on macOS it is normally under `/tmp/kicad/`.

## Board workflow

1. Save the PCB and project settings in KiCad.
2. Call `kicad_inspect_project` for the target editor endpoint.
3. Call `kicad_plan_change` to prepare board graphics, a rectangular Edge.Cuts outline, pad-bound front-copper tracks net-bound through vias or one source-bound footprint.
4. Call `kicad_preview_change` to validate a disposable copy. Review its result before approving.
5. Call `kicad_commit_change` with the exact plan, validation evidence and approval hashes returned by the preview.

Preview copies the saved PCB, project and optional custom design rules. It verifies that the copy matches the editor state, applies the proposal in an owned editor, saves and reloads it in the same editor process, and compares the result and DRC changes. Source locks stay in place. Unsaved board differences are rejected. Source file, editor identity or observed content changes invalidate approval.

## Scope

PCB observation covers supported graphics, footprint placements and pads, routing and stackup metadata. The public authoring path supports board graphics, rectangular outlines, pad-bound straight tracks on F.Cu and net-bound through vias, including mixed batches. Routing previews remap pad identities into the disposable board and verify server-assigned creation IDs, geometry and networks. Routing has offline contract coverage; live routing verification remains pending. Source-bound footprint placement is integrated with planning, disposable preview and approved IPC commit; live end-to-end verification remains pending. It binds a closed saved schematic, its CLI netlist and the allowlisted library file to the plan hash, verifies symbol paths and pin-to-pad network assignments, and rejects source changes. Template preparation reloads a confined staging copy in the existing owned editor. Each footprint requires a separate plan. Unsupported objects or lossy observations can reject inspection.

Schematic authoring is restricted to the fixed two-resistor operation exposed by the schematic tools when its backend is available. Both resistor instances specify `Resistor_SMD:R_0603_1608Metric`. General schematic editing, schematic-to-PCB synchronization, zone authoring, automatic placement/routing and manufacturing release are not implemented. Sessions and approvals expire on restart. The stdio server persists execution evidence, outcomes and pre-write source backups under `$HOME/.local/state/auto-kicad`; `AUTO_KICAD_STATE_DIR` selects a different private directory. `kicad_inspect_project` returns recent execution records and backup paths. An interrupted commit remains uncertain until its board/files are inspected; recovery requires a fresh plan, validation and approval. Source files are never restored automatically. Keep backups until the corresponding changes have been reviewed.

PCB and schematic commits acquire an OS writer lock in `.auto-kicad-writer.lock` at the canonical project directory. Multiple server instances share this lock even with different state directories. The file stays in place after release; it is separate from KiCad editor locks. Durable recording must succeed before mutation. An approval with a recorded completed or uncertain attempt cannot be replayed.

## Footprint sources

The placement allowlist contains `Resistor_SMD:R_0603_1608Metric`, `LED_SMD:LED_1210_3225Metric_Pad1.42x2.65mm_HandSolder` and `Type-C:USB_C_GCT_USB4085`. Save and close the same-named `.kicad_sch` before planning. Its reference, value and footprint field must match the request. The preview returns the source-bound operations for review.

`AUTO_KICAD_OFFICIAL_FOOTPRINT_ROOT` selects the installed `footprints` directory; the server resolves the corresponding `.pretty` subdirectory. `AUTO_KICAD_PROJECT_FOOTPRINT_ROOT` selects a USB-C `.pretty` directory, defaulting to `Type-C.pretty` inside its project directory. MCP requests cannot supply executable, script or library paths. The fixed template helper currently requires the source checkout and KiCad's bundled macOS Python runtime; copying only the server executable does not provide this backend.

`kicad_capabilities` reports enabled tool names directly from the MCP router. Authoring marked `live_verification_pending` has not completed live acceptance; each request still requires its normal source, shadow, DRC and approval checks.

## Verification

```sh
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

Environment-dependent tests are ignored by default. The CLI-only source-binding test requires KiCad CLI and official libraries and opens no editor. PCB live tests require the installed editor and CLI. Run each explicitly; they create disposable projects and close only their owned editor processes. Preview requests are serialized to avoid concurrent preview windows.

```sh
cargo test -p auto-kicad tests::production_cli_binds_fixed_circuit_footprints_without_editor -- --ignored --exact
cargo test -p auto-kicad tests::source_bound_circuit_and_routing_reuse_one_owned_editor -- --ignored --exact
cargo test -p auto-kicad tests::disposable_graphics_shadow_verifies_server_assigned_ids -- --ignored --exact
cargo test -p auto-kicad tests::live_mcp_preview_rejects_unsaved_state_and_commits_saved_graphics -- --ignored --exact
```

The CLI-only source-binding test passed with installed KiCad. The open-editor MCP workflow and same-process reload have offline regression coverage; live end-to-end acceptance remains pending. A single-editor circuit trial verified both resistor placements and their saved/reloaded schematic bindings, then exposed a through-via observation error after routing reload. Cleanup and source isolation passed. The model accepts the serialized inner-layer representative without adding a physical copper layer, and shadow routing reads its saved defaults after reloading the owned editor. Full circuit DRC and MCP live acceptance remain pending.
