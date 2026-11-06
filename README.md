# auto-kicad

Edit KiCad schematics and PCBs from an MCP client. Changes are previewed, checked with KiCad, and applied after approval. Execution records and source backups support recovery when an operation is interrupted.

`auto-kicad` is written in Rust and uses MCP over standard input/output. The verified runtime is **KiCad 10.0.7 on macOS arm64**.

## What it does

| Area | Supported work |
| --- | --- |
| Inspection | Read components, nets, pads, routing, board geometry and supported design rules; run ERC and DRC |
| PCB editing | Place footprints, move or rotate free footprints, edit tracks and vias, flip or delete selected objects, draw outlines and edit solid zones |
| Routing | Add explicit tracks, multilayer paths and through vias; refill zones affected by supported changes |
| Schematic editing | Create symbols, edit properties and annotations, draw wires and labels, and manage sheets, hierarchical pins and child files |
| Synchronization | Apply selected schematic changes to a saved PCB while preserving unrelated layout and routing |
| Exports | Generate Gerber, Excellon drill, BOM and schematic PDF files into a new output directory |

Each operation has limits. Connected-copper edits, replacement compatibility and custom rules need particular care; see [usage and restrictions](docs/usage.md) before editing an existing design.

## Install

Install KiCad with its libraries and bundled Python runtime. Building from source also requires Rust stable, Cargo and Python 3. Run these commands from the repository root:

```sh
env CARGO_INCREMENTAL=0 cargo build --release --locked -p auto-kicad --bin auto-kicad
python3 scripts/install-local.py target/release/auto-kicad --no-rollback
python3 scripts/install-local.py --verify
codex mcp add auto-kicad -- "$HOME/.local/bin/auto-kicad"
codex mcp get auto-kicad
```

The installer writes a launcher to `~/.local/bin/auto-kicad` and checksum-verified releases to `~/.local/lib/auto-kicad/versions/`. The example keeps one release. Omit `--no-rollback` to retain the previous release for rollback. The installed runtime works without the source checkout or Cargo build directory; KiCad must remain installed.

Add `tool_timeout_sec = 600` to the existing `[mcp_servers.auto-kicad]` section in `~/.codex/config.toml`, keeping its `command` entry. Large previews can take several minutes. Start a new Codex session after registering the server. Other MCP clients can launch the same installed executable.

Installation status, upgrade, rollback and removal are described in the [installation reference](docs/acceptance/local-delivery-and-exports.md#installation).

## Use

For live PCB editing, enable KiCad's IPC API and open the saved board in PCB Editor. Schematic editing, schematic-to-PCB synchronization and exports work on saved files with project editors closed.

1. Inspect the design and select objects from the returned identities.
2. Plan a specific change.
3. Preview it on a disposable copy and review the design and diagnostic differences.
4. Approve and commit that preview.
5. Read the execution result. An `unknown` outcome requires inspection and a new recovery approval before further writes.

Source changes invalidate approval. Plans and approvals expire when the server restarts. Backups and execution history are stored in `~/.local/state/auto-kicad`; `AUTO_KICAD_STATE_DIR` selects another directory.

The [usage guide](docs/usage.md) lists the MCP tools, request examples and recovery steps.

## Limits

Movement and rotation reject footprints with attached copper. Explicit routing needs caller-supplied paths; general automatic placement and routing are outside the MCP API. Synchronization supports selected replacement and netclass changes, with conflicts rejected before publication. Custom-rule evaluation and unrestricted KiCad GUI parity are outside the supported scope.

Native GUI saves can migrate older schematic caches, annotations and 3D-model references. Exported files reflect the saved design; they do not establish manufacturing readiness. See the [native GUI record](docs/acceptance/native-gui-qualification.md) and [export contract](docs/acceptance/local-delivery-and-exports.md) for details.

## Documentation

- [Usage](docs/usage.md): tools, examples, configuration and recovery.
- [Development](docs/development.md): workspace layout, quality checks and live tests.
- [Placement and routing replay](docs/replay.md): a separate local playback utility.
- [Reference index](docs/README.md): API contracts and native implementation records.
- [Real-project acceptance](docs/acceptance/final-real-projects.md): NFC, routed Sonde and ECC83 project copies.
- [Capability matrix](schemas/capability-matrix.json): operation status, limits and verification records. The same matrix is returned by `kicad_capabilities`.
