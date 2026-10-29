# Complete native schematic edit previews

`SchematicAudit::preview_edits` compiles typed edits from the audit's captured schematic bytes and native baseline. Catalog-backed symbol creation and footprint assignment recheck their source and catalog evidence. `SchematicAudit::symbol_definition` resolves symbol creation evidence through that same effective catalog. Created symbols with footprint assignments require separate compatibility evidence and currently reject in this preview API.

The candidate retains its basename and physical hierarchy in an owned snapshot. Effective library tables retain the original resource authority. A complete candidate audit fixes configuration, executable and resource inputs, reads native netlist data, resolves active pins, and observes omitted virtual-pin connectivity through private witnesses. Both source and candidate inputs are checked before returning evidence.

`SchematicEditPreview` retains exact source/candidate artifact manifests, the typed candidate and changed source spans, complete baseline/result ERC JSON, the exact diagnostic multiset difference, native component/net records, and complete source-identity pin connectivity including isolated electrical no-connect pins. Its digest binds the retained evidence. A detected change to source hierarchy, configuration, libraries or private snapshots permanently invalidates the preview, even after the input is restored.

The KiCad 10.0.7 native profiles use ECC83 and a multichannel mixer with a physical child sheet reused 4 times. Property edits preserve complete ERC and connectivity; the multichannel baseline retains 4 errors and 32 warnings. Catalog-backed `Device:R` creation is observed in the candidate netlist. A floating-label candidate retains its newly introduced ERC diagnostic. Forged catalog evidence rejects, and source/configuration/library drift invalidates the corresponding preview. Original inputs remain unchanged and private audit trees are removed. No GUI is launched.

This API observes candidates and their differences. It does not approve electrical changes, publish source files, provide multi-file recovery or expose general-edit MCP tools. Reviewed connectivity/ERC expectations, publication, recovery and GUI/Codex lifecycle qualification remain required.

```text
cargo run -p kicad-adapter --example check-schematic-preview -- OWNED_SCHEMATIC OWNED_CONFIG NEW_OUTPUT REFERENCE
```

Owned project and configuration inputs require the `.auto-kicad-audit-owned` acceptance marker. The machine-readable record is [schematic-native-preview.json](schematic-native-preview.json).
