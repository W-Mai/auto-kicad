# Component unit creation and insertion

`create_symbol` supports multiple placements belonging to one new component. Placements sharing an instance reference require identical source/catalog definitions, value, footprint, effective BOM/board/DNP flags and instance annotations, with unique unit numbers. Body style is selected per placement. New component references cannot join existing components or collide with reference changes in the same plan.

Creation on a reused physical sheet requires an explicit `instance_references` object mapping every observed sheet path to its component reference. Missing, additional or repeated-instance references reject. All units in one component use the same mapping. The displayed reference equals the reference of the lexically first observed path. Existing single-instance requests can omit the mapping.

`add_symbol_unit` inserts a missing unit from an observed source placement. Its action contains `key`, `uuid`, `unit` and `transform`, and requires `file_all_instances` scope. The server resolves the anchor and complete existing component from immutable observation evidence. It rejects occupied or duplicate slots, inconsistent existing member properties and concurrent edits to any member of that component.

Insertion clones the complete observed source symbol, preserves its embedded library alias, value, footprint, effective flags, custom properties and other metadata, transforms property positions, allocates new source/pin UUIDs, and rebuilds the exact current-project instance annotations. Original source symbols and embedded cache bytes remain unchanged. Insertion uses the observed cache rather than replacing it with an installed catalog definition. Units need supported standalone cache definitions and unambiguous selected pins; stacked/variable pin-number creation remains unsupported.

Common pins represented by several unit contacts must belong to one native net. Explicit wires can connect those contacts in the same batch. Unwired contacts on distinct native nets reject before approval. Adding a source unit changes the complete unit membership of every existing logical pin in that component. Expectations must therefore declare the old pin/network keys removed and the expanded keys added, including actual ERC changes. Original source UUIDs and unrelated component connectivity remain exact.

The workflow uses the existing observation, planning, preview and commit tools. It retains source/catalog/configuration checks, exact native expectations, session-bound approval, durable publication and actual-source native verification. This contract covers saved-file CLI authoring; GUI/Codex qualification, hierarchy creation and PCB synchronization remain separate requirements.

## Reproducible native qualification

```text
python3 scripts/check-schematic-multiunit.py SERVER OWNED_SCHEMATIC OWNED_CONFIG NEW_OUTPUT
python3 scripts/check-schematic-multiunit.py SERVER OWNED_ROOT_SCHEMATIC OWNED_CONFIG NEW_OUTPUT --file channel_strip.kicad_sch
```

Inputs require marked project/configuration copies, prepared `Inherited_A:Leaf` and `Triple:Triple` libraries with common pins, and an incomplete `U9901` carrying its assigned five-pad footprint and custom field for the single-sheet profile. The reused-sheet profile creates its own incomplete component with explicit annotations. The driver exercises complete two-/three-unit creation, missing-unit insertion, duplicate/property/library/unit/map rejection, ambiguous common-net rejection, exact approval and replay refusal, untouched original source/cache records, configuration preservation and private-directory cleanup. A separate native export uses its own configuration and temporary directory, verifies saved unit grouping and unique pins, and preserves raw netlist/ERC output. Native ERC deduplicates diagnostics for a physical source-pin UUID reused across instances; connectivity remains verified separately for every instance.

Actual KiCad 10.0.7 qualification passed for schematic formats 20250114 and 20260306 plus a native child reused in 4 instances: 10 committed production-stdio executions and 40 independently rehashed evidence artifacts. Complete quality passed 551 tests with zero failures and 16 environment-dependent tests ignored; the existing actual CLI/MCP regression passed in 3.96 seconds. Each accepted profile exited successfully, retained the complete native output, preserved input configuration and removed private snapshots. Failed controls remain marked separately. [Machine-readable acceptance](schematic-multiunit.json) records the scope.

A separate actual stdio boundary probe confirms that complete multi-unit reference annotation and footprint assignment still reject without source changes. Those operations, sheet/hierarchical-pin/child-file authoring, GUI/Codex/full-family qualification and the remaining product milestones are not completed by this delivery. The complete product goal remains active.
