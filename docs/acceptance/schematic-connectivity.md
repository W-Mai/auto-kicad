# Schematic pin connectivity

`SchematicAudit::observe_pin_connectivity` observes exported component pins and omitted power/virtual pins in a private native witness. `PinProbePlan` binds exact source bytes, retains every original byte, and inserts passive single-pin symbols excluded from board and BOM at omitted contacts. Repeated sheets receive instance-specific references; stacked logical numbers share a physical probe. Returned `ConnectivityPin` identities contain original source identities and pin numbers, never auxiliary probe references.

The witness must preserve original component metadata, pin functions and electrical types, net names/classes, and all exported-pin connectivity partitions. Every probe has exactly one validated passive node. Missing, duplicate or unexpected nodes, changed original connectivity and names derived from private probe identifiers reject. Electrical `no_connect` pins remain separately isolated because they do not propagate contact connectivity; ambiguous duplicate contacts and electrical-no-connect isolation with jumper definitions reject. Omitted non-virtual symbols require separate evidence and reject.

The adapter checks its source/context audit before and after observation, copies the captured private project and configuration, and shares only verified immutable captured resources. Native configuration settles before a second identical export. Project/configuration bytes and namespace must remain unchanged, and the witness directory is removed before returning. Projects requiring no probes reuse their verified native baseline. Original complete ERC and explicit no-connect marker evidence remain authoritative: auxiliary pins can change no-connect representation and diagnostics, so probe reports never replace that baseline.

## Native acceptance

Actual KiCad 10.0.7 passed eight cases with 407 connected source-pin identities and one separately isolated electrical-no-connect identity. All original projects, retained baselines and owned input copies remained byte-identical. No editor was launched.

| Case | Exported pins | Omitted source pins | Connected identities | Isolated electrical NC | Nets |
| --- | ---: | ---: | ---: | ---: | ---: |
| ECC83 | 33 | 9 | 42 | 0 | 13 |
| Multichannel | 280 | 50 | 330 | 0 | 80 |
| Common/stacked/alternate body | 5 | 0 | 5 | 0 | 4 |
| Legacy convert | 5 | 0 | 5 | 0 | 4 |
| Passive interference controls | 5 | 8 | 12 | 1 | 8 |
| Dynamic power values | 1 | 2 | 3 | 0 | 2 |
| Legacy dynamic power values | 1 | 2 | 3 | 0 | 2 |
| Virtual common/stacked pins | 1 | 6 | 7 | 0 | 5 |

Controls cover `${REFERENCE}` power values, virtual-only wired and separate nets, an unsplit wire crossing, explicit no-connect markers, electrical-no-connect pins and a label matching an automatically generated name. Board-excluded probes preserve baseline names where board-enabled probes can rename ordinary anonymous nets. Each case requiring probes rejects removed/duplicate probe nodes and changed original net names. Existing pin metadata negative controls also pass. The virtual common/stacked case uses five probes to observe six omitted source identities.

The native current/legacy controls exposed pin-name compatibility: base name `~` becomes empty only before format 20250318; current formats and alternate names retain it literally. Architecture checks cover both forms, exact source binding, unchanged original objects, full pin coverage, native metadata drift and separate electrical-no-connect isolation. Full quality passed formatting, capability matrix, warning-denied all-target Clippy and 516 tests with zero failures and 15 environment-dependent tests ignored. The actual fixed-circuit CLI/MCP regression passed separately in 3.81 seconds.

## Reproduction and limits

Run `check-schematic-pins OWNED_SCHEMATIC OWNED_CONFIG NEW_OUTPUT --connectivity`. Both input roots require `.auto-kicad-audit-owned` containing `schematic-audit-acceptance` followed by a newline. The example retains complete connectivity, probe netlist/evidence, baseline ERC, negative controls and cleanup results. The retained `accept-pin-connectivity.py` reproduces the native cases; [schematic-connectivity.json](schematic-connectivity.json) contains source/artifact hashes and results. Failed pin-name normalization and malformed fixture attempts remain available with their diagnostics.

This API observes supported source-pin connectivity. It does not describe complete graphical wire topology, publish schematic edits, grant write approval or synchronize PCB objects. Dynamic pin names/numbers and unsupported source/library forms retain their explicit rejection behavior. [Bounded schematic MCP](schematic-general-mcp.md) composes this proof with typed edit compilation, exact connectivity/ERC expectations and approved multi-file publication/recovery.
