# Owned schematic preview evidence

`SchematicAudit` owns shared immutable state containing captured inputs, native observations and private snapshots. `SchematicAudit::preview_edits` returns `Arc<SchematicEditPreview>`; `MatchedSchematicPreview` retains the same preview and its atomic validity state. Handles require `Send + Sync + 'static`, so server sessions can store them after planning returns. Cloning an audit shares captured bytes. Candidate audits also retain their generated source tree, including when cloned independently from a preview.

The owned ECC83 acceptance retains two matching handles, drops the original audit/preview/planning handle and verifies retained evidence on another thread. Native private directories remain present. Changing the original source invalidates a matching handle; restoring exact bytes leaves both handles invalid. Dropping one handle preserves the snapshots. Dropping the final matching handle removes the baseline audit and releases the preview. An independently cloned candidate audit still verifies against its retained source tree, then removes its private files when dropped.

The same executable continues with controlled uncertain writes and actual stdio MCP recovery, verifying exact original bytes, new-file removal and restored native netlist/ERC/connectivity. It opens no GUI. This qualifies evidence ownership and cleanup; user approval, general publication and general schematic MCP remain separate unfinished contracts.

```sh
cargo build -p auto-kicad --bin auto-kicad --example check-schematic-recovery
target/debug/examples/check-schematic-recovery target/debug/auto-kicad /absolute/owned/project/ecc83-pp.kicad_sch /absolute/owned/config /absolute/new-output
```

Inputs require the owned-copy acceptance marker described in [mixed schematic recovery](schematic-recovery.md). Full quality passed 536 tests with zero failures and 15 environment-dependent tests ignored. The multichannel native preview regression also passed across 4 reused-sheet instances, retaining its 4 baseline errors and 32 warnings plus source/configuration/library drift controls. [Machine-readable acceptance](schematic-preview-ownership.json) records scope. Native evidence is retained under `/Users/w-mai/Projects/PawChrona/.local/auto-kicad-acceptance/m5-owned-preview`.
