# Native GUI qualification

KiCad 10.0.7 qualifies actual schematic load/save, configured nested page labels, the saved ECC83 PCB and production zone-fill cancellation with guarded recovery. The qualified runtime is `0e71326ecbd4868cb5c6ea9e11c7f8901580ec458a06940ca142cf062f5c801c`; the active local release is `0.1.0-d15c5bd75e557d1b`. A fresh actual Codex connection discovers all 13 tools through the registered installation without a server override. Local installation retains one release with no rollback payload.

| Gate | Actual native evidence |
| --- | --- |
| Complete component result | GUI File Save, 26 source units, one page, complete native netlist/ERC and explicit source-format conversion comparison |
| General authoring result | GUI File Save, 26 source units and 38 wires; complete native netlist/ERC and source geometry/fields preserve the accepted electrical edits |
| Repeated and nested hierarchy | GUI File Save across 4 physical files, 285 logical units and seven pages; actual Edit Sheet Page Number dialogs read `99` at `/AK_CHILD/NESTED/` and `100` at `/AK_CHILD/` |
| Saved source API compatibility | Current production MCP re-observes all three results; complete connectivity, contact, unit and configured-sheet inventories agree |
| Real ECC83 PCB result | GUI File Save preserves all 15 footprints and 59 copper segments; complete parsed PCB data agrees after explicit field ordering and ten native 3D-model path migrations |
| Actual fill cancellation | The source editor's native `Fill All Zones` Cancel button receives AXPress successfully during an approved production commit; generated copper differs and MCP records `unknown` |
| Cancellation recovery | Open-editor recovery rejects; after owned closure, fresh approval restores exact pre-commit PCB bytes, removes the pending marker and retains the original unknown execution history; wrong, restarted and replayed approvals reject |

## Native serialization boundaries

The GUI converts source format 20250114 to 20260306 and project net settings 4 to 5. Existing board design settings, network-class values and assignments remain intact. Current-project inactive annotations may be pruned; visible fallback references refresh from the complete active instance maps. Exact active maps, source geometry, existing fields, connectivity and native diagnostics remain verified. Added empty standard fields, default sheet/simulation flags and native-generated missing pin UUID records have explicit checks. Numeric spelling and direct record ordering are serialization differences; point sequences remain significant.

Legacy library caches undergo additional native migration: a tilde Datasheet becomes empty, hidden cached graphical text can disappear, body-style metadata and empty synthetic field records can appear, and duplicate empty native export fields gain disambiguated labels. Complete raw differences remain sealed. The native netlist comparison permits only timestamp-set ordering, field ordering and the exact empty `Field42` disambiguation in `TL072CD`. Exact legacy cache or inactive-annotation byte preservation is not claimed. Production publication preserves its original source authority and integrity guards; GUI checks operate on separate owned copies.

PCB GUI saving converts ten `${KICAD6_3DMODEL_DIR}` WRL references to matching `${KICAD10_3DMODEL_DIR}` STEP references and reorders footprint unit metadata. Every other parsed source node remains equal. The original accepted real-project files stay untouched. These path migrations are recorded rather than hidden behind a broad model-field exclusion.

## Failures and cleanup

An unsettled CM5 source first rejects preview because saved/live states differ. Same-process native Revert settles that owned copy; a fresh plan passes. The first AppleScript progress observer fails when windows change during enumeration and its commit succeeds normally; that attempt is retained and is not counted as cancellation. A separate approved commit uses native Accessibility observation and the real Cancel button; it records the expected unknown outcome and passes fresh guarded recovery.

One schematic process is reused for the general and nested cases. PCB document-open events do not switch the existing owned editor, so the saved-pair editor closes before the separate cancellation editor starts. Every owned process is terminal. Two stale owned document locks are quarantined only after exact editor closure; six native instance registry files are removed only after their recorded PIDs are absent. These are acceptance-owner interventions. Private GUI scratch is empty, the single Cargo target is absent, and no build is performed for this qualification batch.

The unchanged executable source retains its complete 636 passed / 0 failed / 18 ignored quality receipt, formatting, capability matrix and warnings-denied workspace/all-target Clippy. Documentation checks reuse that receipt. Historical environment-drift recovery controls remain preserved; they are not relabeled as successful recovery. Screenshot evidence is unavailable because Screen Recording is not granted; actual native controls, saved files and independent native/MCP reads establish the recorded results.

[Machine-readable evidence](native-gui-qualification.json) binds native receipts, complete saved comparisons, cleanup, installation and Codex discovery to retained hashes. [Three real-project functional profiles](final-real-projects.md) retain their separate actual Codex/native/recovery evidence. Unsupported operations and compatibility bounds remain those documented by each API family.
