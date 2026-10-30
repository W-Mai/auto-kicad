# Catalog-bound schematic symbol creation

`SymbolCatalog::resolve_definition` resolves one approved `nickname:entry` from a native `.kicad_sym` file and retains the complete source definition, source digest and catalog digest. The resolver validates native table rows, canonical source confinement, duplicate definitions, inheritance targets and inheritance cycles before producing evidence.

`EditAction::CreateSymbol` compiles that evidence into an immutable candidate. The compiler normalizes entry-only library names to the schematic-qualified identifier, preserves the complete embedded definition, allocates deterministic symbol and pin UUIDs, writes the selected unit and body style, creates reference/value/footprint properties, and records every expanded instance path in the native `instances` block. Creation is file-scoped and requires complete physical-sheet coverage; isolated edits of a reused physical file reject.

The compiler binds catalog and source digests in the typed request, rejects malformed identifiers, conflicting component references or unit slots, unsupported stacked pin numbers, missing pins, stale embedded definitions and UUID collisions, and keeps unrelated source spans byte-identical. [Bounded schematic MCP](schematic-general-mcp.md) resolves this catalog evidence server-side and exposes observation, planning, exact native expectations and approved publication for supported definitions. [Inherited definition creation](schematic-symbol-inheritance.md) resolves complete source-file chains into standalone cache definitions while retaining original catalog provenance.

The KiCad 10.0.7 acceptance example creates `Device:R` as `R900` in an owned ECC83 copy, observes the new component and two pins in native netlist data, verifies zero new ERC diagnostics, preserves the source and configuration, and removes all private audit directories. No editor GUI is launched.

The reproducible command is:

```text
cargo run -p kicad-adapter --example check-schematic-edits -- OWNED_SCHEMATIC OWNED_CONFIG OUTPUT_DIR R1
```

The machine-readable result is [schematic-symbol-creation.json](schematic-symbol-creation.json). The retained native evidence directory is `/Users/w-mai/Projects/PawChrona/.local/auto-kicad-acceptance/m5-symbol-create-native`.

[Component unit creation and insertion](schematic-multiunit.md) define same-reference unit groups, complete per-instance annotations and insertion from existing source metadata.
