# Development

The Rust 2024 workspace separates native IPC, design observations, edit planning and MCP publication. The verified native runtime is KiCad 10.0.7 on macOS arm64.

| Crate | Responsibility |
| --- | --- |
| `auto-kicad` | MCP tools, operation orchestration, execution records and recovery |
| `kicad-ipc` | KiCad protocol, transport, native commands and receipts |
| `kicad-model` | Design observations and source identities |
| `kicad-engine` | Typed plans and expected design changes |
| `kicad-adapter` | Native editor lifecycle, library resolution and verification |
| `kicad-schematic` | Schematic source parsing and CLI operations |
| `kicad-checks` | Native checks and report comparison |
| `kicad-geometry` | Geometry calculations |
| `runtime` | Shared process and temporary-resource handling |

## Quality checks

Run from the repository root:

```sh
env CARGO_TARGET_DIR=/tmp/auto-kicad-target CARGO_INCREMENTAL=0 CARGO_PROFILE_DEV_DEBUG=0 CARGO_PROFILE_TEST_DEBUG=0 python3 scripts/quality.py
```

This runs the capability-matrix validator, Rust formatting, warnings-denied workspace/all-target Clippy and default workspace tests. It opens no KiCad window. Native environment-dependent tests are ignored unless selected explicitly. The same checks run in [CI](../.github/workflows/quality.yml).

Use one build directory while developing. Remove its contents after build/test processes have exited and any required installed binaries or results have been retained. Installed releases do not depend on the Cargo target directory.

The [capability matrix](../schemas/capability-matrix.json) records advertised operations, constraints and evidence. `scripts/check-capability-matrix.py` checks it against the MCP router; verified entries must point to tracked records. Source generation or compilation alone does not verify a native operation.

## Native tests

Live tests require KiCad and its libraries. PCB tests launch owned editor processes on disposable copies; the CLI-only source-binding test opens no editor. Run tests individually and read the fixture requirements in their linked records.

```sh
cargo test -p auto-kicad tests::production_cli_binds_fixed_circuit_footprints_without_editor -- --ignored --exact
cargo test -p auto-kicad tests::source_bound_circuit_and_routing_reuse_one_owned_editor -- --ignored --exact
cargo test -p auto-kicad tests::disposable_graphics_shadow_verifies_server_assigned_ids -- --ignored --exact
cargo test -p auto-kicad tests::live_mcp_saved_routing_lifecycle_uses_two_editors -- --ignored --exact
cargo test -p auto-kicad tests::live_mcp_source_bound_footprint_lifecycle_uses_two_editors -- --ignored --exact
cargo test -p auto-kicad tests::live_mcp_connected_assembly_lifecycle_uses_two_editors -- --ignored --exact
cargo test -p auto-kicad tests::recovery_acceptance::live_recovery_restores_native_board_and_allows_next_mcp_commit --lib -- --ignored --exact
```

Use the same `CARGO_TARGET_DIR` and profile environment as the quality command to reuse build output. The mixed-routing, footprint and assembly tests use a source editor and a separate preview editor. Preview requests are serialized. Tests close only their owned processes, and failed post-commit checks can retain diagnostic files whose paths are reported. Existing-footprint move acceptance also needs `AUTO_KICAD_MOVE_SERVER`; see its [contract](acceptance/existing-footprint-transform.md).

## Installed-server checks

After registering the local installation with Codex, verify tool discovery without starting a model turn:

```sh
env PYTHONDONTWRITEBYTECODE=1 python3 scripts/check-codex.py --output /tmp/auto-kicad-codex.json
```

The board-copy check performs a real edit through the installed server. Supply a saved PCB, its same-named project settings and a new output directory:

```sh
env CARGO_TARGET_DIR=/tmp/auto-kicad-target CARGO_INCREMENTAL=0 CARGO_PROFILE_DEV_DEBUG=0 cargo run --locked -p auto-kicad --example check-board-copy -- /absolute/project/board.kicad_pcb "$HOME/.local/bin/auto-kicad" /absolute/new-acceptance-directory
```

It adds a 3 mm Dwgs.User marker at (80, 78) mm on an owned copy, checks approval and replay rejection, verifies saved reload, DRC, backups and existing objects, then closes the editors. Saved results and evidence remain in the output directory. The input files stay unchanged.

[Real-project records](acceptance/final-real-projects.md) cover NFC, routed Sonde and ECC83 copies. [GUI records](acceptance/native-gui-qualification.md) cover native saves, nested page labels and actual fill cancellation/recovery. Individual backend and failure tests are listed in the [reference index](README.md); their reported scope applies to that recorded run.

## CI packages

The CI workflow runs workspace quality and a release build on macOS 15 arm64. The build job calls `scripts/package-ci.py` to exercise CLI metadata, MCP discovery/capabilities and install/verify/uninstall outside the source checkout. It packages the executable, license notices and a matching source archive with vendored dependencies, then uploads both with checksums. Only a version-tag push can publish release assets, and that job requires quality and build success. See [downloads and builds](releases.md).
