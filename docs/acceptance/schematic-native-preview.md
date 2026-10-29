# Complete native schematic edit previews

`SchematicAudit::preview_edits` compiles typed edits from the audit's captured schematic bytes and native baseline. Catalog-backed symbol creation and footprint assignment recheck their source and catalog evidence. `SchematicAudit::symbol_definition` resolves symbol creation evidence through that same effective catalog. Created symbols with footprint assignments require separate compatibility evidence and currently reject in this preview API.

The candidate retains its basename and physical hierarchy in an owned snapshot. Effective library tables retain the original resource authority. A complete candidate audit fixes configuration, executable and resource inputs, reads native netlist data, resolves active pins, and observes omitted virtual-pin connectivity through private witnesses. Both source and candidate inputs are checked before returning evidence.

`SchematicEditPreview` retains exact source/candidate artifact manifests, the typed candidate and changed source spans, complete baseline/result ERC JSON, the exact diagnostic multiset difference, native component/net records, and complete source-identity pin connectivity including isolated electrical no-connect pins. Its digest binds the retained evidence. A detected change to source hierarchy, configuration, libraries or private snapshots permanently invalidates the preview, even after the input is restored.

`SchematicAudit` clones share immutable captured inputs. `preview_edits` returns `Arc<SchematicEditPreview>`, and matching evidence retains that same preview after the planning call returns. Handles can move between worker threads without copying captured source/library bytes. Cloned candidate audits retain their internally generated source tree independently; private directories are removed after their final owner drops. All matching handles share permanent preview invalidation. [Native ownership acceptance](schematic-preview-ownership.md) exercises these lifetime and cleanup guarantees.

The KiCad 10.0.7 native profiles use ECC83 and a multichannel mixer with a physical child sheet reused 4 times. Property edits preserve complete ERC and connectivity; the multichannel baseline retains 4 errors and 32 warnings. Catalog-backed `Device:R` creation is observed in the candidate netlist. A floating-label candidate retains its newly introduced ERC diagnostic. Forged catalog evidence rejects, and source/configuration/library drift invalidates the corresponding preview. Original inputs remain unchanged and private audit trees are removed. No GUI is launched.

This API observes candidates and their differences. [Explicit electrical/ERC expectations](schematic-expectations.md) compare declared changes against complete native observations and bind the result to the preview digest. [Native source publication](schematic-native-publication.md) consumes matching evidence and validates the actual result before completion; [durable recovery](schematic-publication-recovery.md) covers supported file writes. [Bounded schematic MCP editing](schematic-general-mcp.md) binds these objects to server-owned sessions and exact approval, including [supported inherited symbol definitions](schematic-symbol-inheritance.md). Hierarchy authoring and GUI/Codex lifecycle qualification remain incomplete.

```text
cargo run -p kicad-adapter --example check-schematic-preview -- OWNED_SCHEMATIC OWNED_CONFIG NEW_OUTPUT REFERENCE
```

Owned project and configuration inputs require the `.auto-kicad-audit-owned` acceptance marker. The machine-readable record is [schematic-native-preview.json](schematic-native-preview.json).
