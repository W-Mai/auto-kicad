# Logical schematic pins

`SchematicPinIndex` resolves exported component pins from exact schematic bytes, embedded symbol definitions, instance identities and native netlist unit metadata. Active pin selection includes the placed unit, common pins, selected graphical body style and stored alternate functions. Bracketed pin lists and bounded alphanumeric ranges expand into logical numbers. Complete native node membership, pin function and electrical type must agree with the active source pins.

Stored pin UUID lists can include inactive units and styles. The index retains those records as provenance without treating each record as active. Shared immutable records avoid copying an entire UUID inventory for every occurrence. Logical keys contain source unit identities and pin numbers; reference text is separate, so reference renaming preserves connectivity partitions. Common pins retain all active source occurrences. Native net codes, names, classes and explicit no-connect markers remain distinct fields.

Native XML unit metadata preserves slot order and custom display names, including unplaced units. The general index requires that metadata; legacy netlist callers can still parse documents without it. Source parsing accepts `body_style` and legacy `convert` as aliases and rejects repeated declarations. `SchematicAudit` exposes the index alongside its fixed native netlist/ERC evidence. Private initialization can create only the exact initially absent `.kicad_prl`; that file is bound before repeated validation, and unrelated namespace additions remain forbidden.

## Native acceptance

KiCad 10.0.7 verified ECC83 with 33 logical component pins, 4 explicit no-connect pins and 9 unexported source power pins. Multichannel verified 280 logical pins, 10 common-pin groups, 50 unexported source pins and five sheet instances. A real ECC83 copy with R1 renamed to R1001 preserved the complete source-identity connectivity partition.

A native test schematic combines a common pin, stacked pin range, alternate function, selected graphical body styles and an unplaced unit. Its 5 logical pins include 1 common group and 4 explicit no-connect pins. Repeating it with format 20250114 and legacy `convert` fields produced the same connectivity as format 20260306. Each native case rejects removed nodes, duplicate membership, changed pin functions and changed unit metadata. Original examples, retained baselines and owned input bytes remain unchanged. Private directories are removed; no editor was launched.

Architecture checks cover source revision binding, active/common/unplaced pin membership, reference-independent connectivity, malformed or excessive ranges, missing/extra/conflicting native evidence, unit ordering, shared metadata/node bounds, body-style alias ambiguity and bounded private preference initialization. Full quality passed formatting, capability matrix, warning-denied Clippy on all targets and 509 tests with zero failures and 15 environment-dependent tests ignored. The existing actual fixed-circuit CLI/MCP regression passed separately in 4.17 seconds.

## Scope

Power and virtual source pins omitted from native component export remain explicitly unexported. Their electrical connectivity is unproven; absence does not mean disconnection. This index does not prove physical wire geometry, authorize general schematic writes, or implement PCB synchronization. Dynamic pin names/numbers, ambiguous stored alternates, unresolved embedded inheritance and inconsistent native metadata reject explicitly. Public MCP capabilities are unchanged.

`check-schematic-pins` takes an owned schematic, owned configuration root and a new output directory. Both roots require `.auto-kicad-audit-owned` containing `schematic-audit-acceptance` followed by a newline. It records complete pin observations and connectivity, runs negative controls and verifies input/private-directory cleanup. [schematic-pins.json](schematic-pins.json) records native results, quality gates and artifact hashes. Failed fixture attempts without the required body-style declaration and before private preference initialization support are retained with native diagnostic XML.
