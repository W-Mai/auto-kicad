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
3. Call `kicad_plan_change` to prepare board graphics, a rectangular Edge.Cuts outline, pad-bound front-copper tracks or net-bound through vias.
4. Call `kicad_preview_change` to validate a disposable copy. Review its result before approving.
5. Call `kicad_commit_change` with the exact plan, validation evidence and approval hashes returned by the preview.

Preview copies the saved PCB, project and optional custom design rules. It verifies that the copy matches the editor state, applies the proposal in an owned editor, saves and reloads it in the same editor process, and compares the result and DRC changes. Source locks stay in place. Unsaved board differences are rejected. Source file, editor identity or observed content changes invalidate approval.

## Scope

PCB observation covers supported graphics, footprint placements and pads, routing and stackup metadata. The public authoring path supports board graphics, rectangular outlines, pad-bound straight tracks on F.Cu and net-bound through vias, including mixed batches. Routing previews remap pad identities into the disposable board and verify server-assigned creation IDs, geometry and networks. Routing has offline contract coverage; live routing verification remains pending. Footprint authoring is not yet enabled. Unsupported objects or lossy observations can reject inspection.

Schematic authoring is restricted to the fixed two-resistor operation exposed by the schematic tools when its backend is available. General schematic editing, schematic-to-PCB synchronization, zone authoring, automatic placement/routing and manufacturing release are not implemented. Sessions and approvals are held in memory; restart requires new planning and validation.

## Verification

```sh
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

Live tests are ignored by default and require the installed KiCad editor and CLI. Run each explicitly; they create disposable projects and close only their owned editor processes. Preview requests are serialized to avoid concurrent preview windows.

```sh
cargo test -p auto-kicad tests::disposable_graphics_shadow_verifies_server_assigned_ids -- --ignored --exact
cargo test -p auto-kicad tests::live_mcp_preview_rejects_unsaved_state_and_commits_saved_graphics -- --ignored --exact
```

The open-editor MCP workflow and same-process reload have offline regression coverage; their live end-to-end verification is pending.
