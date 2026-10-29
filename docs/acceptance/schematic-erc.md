# Complete schematic ERC reports

`kicad_checks::erc::CompleteErcReport` retains complete native ERC JSON, exact numeric tokens, unknown fields, sheet identities, duplicate diagnostics, ignored-check metadata and exclusion comments. `NetlistRunner::run_complete_erc` captures KiCad 10.0.7 JSON with all severities and millimetre coordinates, validates the reported source basename, and applies bounded output capture and process-group timeout cleanup.

Report comparison ignores only top-level source/date and the ordering of sheets, diagnostics and diagnostic items. Every other value remains significant. Differences distinguish report metadata, sheet headers, introduced diagnostics, resolved diagnostics and unchanged multiplicities. KiCad retains an excluded diagnostic's original severity and sets `excluded: true`; summary counts separate active errors, active warnings and exclusions. Serialization retains the complete raw report as a string to prevent numeric rounding through intermediate JSON values.

## Native acceptance

Owned copies of installed KiCad examples were checked with their project files, symbol libraries, footprint libraries and library tables. ECC83 produced zero diagnostics. The multichannel mixer produced 4 active errors and 32 warnings across 5 sheet instances. Both reports matched retained native baselines with exact diagnostic correspondence.

Three additional multichannel copies exercised native project settings. Ignoring `power_pin_not_driven` removed 3 diagnostics and changed ignored-check metadata; changing its severity replaced 3 errors with 3 warnings; excluding one error retained its identity, original severity, exclusion flag and comment. Every changed report failed baseline equivalence. The exclusion case contained 3 active errors, 32 warnings and 1 exclusion. Input hashes remained unchanged during each CLI capture, and the original examples and baseline copies remained unchanged.

Native exclusion testing exposed an incorrect initial summary assumption that exclusions used a separate severity string. The implementation and architecture tests use the observed native boolean field. Failed attempts and corrected reports are retained separately. Tests also cover missing fields, incomplete severity coverage, duplicate keys, repeated sheet identities, exact number tokens, unknown-field changes, fixed CLI arguments, incorrect source/version, nonzero exit, timeout and output limits. The existing fixed-circuit CLI/MCP regression checks compatibility with the error-only ERC path.

## Scope and reproduction

This backend API does not establish immutable source/library/configuration binding, pin-level connectivity proof, general schematic editing, GUI load/save acceptance or PCB synchronization. Parsing an imported report grants no source authority. [Bounded schematic MCP](schematic-general-mcp.md) composes complete project-context capture and native correspondence before accepting exact source-bound ERC expectations.

Run `cargo run -p kicad-schematic --example check-schematic-erc -- /absolute/owned/project/root.kicad_sch /absolute/new-output-directory /absolute/baseline.json` with KiCad 10.0.7 installed. The output directory must not exist. Omit the baseline argument to capture only. A changed report is retained before the example exits unsuccessfully. Use `python3 scripts/quality.py` for the repository quality gates.

Machine-readable acceptance and retained artifact hashes are recorded in [schematic-erc.json](schematic-erc.json). Native reports and scripts remain under the recorded evidence directory. No editor was launched for this acceptance.
