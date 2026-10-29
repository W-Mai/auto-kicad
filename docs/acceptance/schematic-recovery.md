# Mixed schematic recovery

The recovery store records both backed-up existing schematics and new schematic paths proven absent before execution. An explicit existence type binds the observed post-attempt state; absence has no placeholder digest. Recovery restores each existing file and removes each recorded new file after checking its exact current state. Closed editors, confined canonical parents, fresh approval, directory synchronization and complete final readback are required. Uncertain results retain a pending marker in the source directory and block writers using other state directories.

On KiCad 10.0.7, an owned ECC83 copy passed native audit and a controlled uncertain edit changed R2's value and created an empty child schematic. A second native audit observed the changed value. Actual stdio MCP rejected a wrong approval and an approval from a restarted process, accepted a fresh recovery approval, restored exact original bytes, removed the new file and rejected replay. A final native audit matched the complete baseline netlist, ERC and source-pin connectivity. Configuration bytes remained unchanged; private snapshots were removed and no GUI opened.

The child is intentionally unreferenced: this verifies file-existence recovery semantics, not hierarchy editing. The controlled writes originate in the acceptance example, not a general publisher. [Durable per-file intent and process-exit recovery](schematic-publication-recovery.md) have separate acceptance. Hierarchy edits, complete native publication, general editing through MCP and GUI/Codex acceptance remain unimplemented or unqualified. Without durable publication intent, missing post-attempt evidence permits unchanged original state only and never authorizes deleting an unobserved new file.

Architectural contracts cover path collisions, symlinks, missing parents, duplicate/overlapping sources, later user edits, partial restoration, approval consumption, marker ownership, terminal-record interruption and legacy JSON compatibility. These injections are distinct from native process crash evidence. Full offline quality passed with 535 tests, zero failures and 15 environment-dependent tests ignored; the existing actual CLI/MCP schematic-to-footprint regression passed separately.

Build the executable and acceptance example, then run against an owned ECC83/configuration copy marked `.auto-kicad-audit-owned` containing `schematic-audit-acceptance` followed by a newline:

```sh
cargo build -p auto-kicad --bin auto-kicad --example check-schematic-recovery
target/debug/examples/check-schematic-recovery target/debug/auto-kicad /absolute/owned/project/ecc83-pp.kicad_sch /absolute/owned/config /absolute/new-output
```

The example invokes `scripts/check-recovery.py` against the production server. [Machine-readable acceptance](schematic-recovery.json) records its exact scope. Retained native reports, preview evidence and stdio transcript are under `/Users/w-mai/Projects/PawChrona/.local/auto-kicad-acceptance/m5-schematic-recovery`.
