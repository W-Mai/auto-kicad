# Fixed schematic CLI execution inputs

`NetlistRunner::freeze_context` captures the native executable's canonical path and byte digest, the inherited process environment, and an optional canonical configuration root. Netlist export and ERC receive the captured environment through an explicit cleared environment and run from the canonical project directory. Each successful command checks executable and inherited-environment stability before dispatch and after completion. Existing unfrozen callers retain their execution behavior.

`NativeExecutionContext` exposes digests and server-side variable lookup. Debug output omits environment names and values. Environment capture rejects duplicate or invalid entries and limits aggregate size to 1 MiB and entry count to 4096. Executable hashing uses a bounded streaming read. A changed executable prevents dispatch or invalidates a command's returned output.

## Native acceptance

KiCad 10.0.7 ran netlist export and complete ERC on prepared owned configurations for ECC83, multichannel with the default language, and multichannel with English selected. The resulting netlists matched retained baselines: 15 components/13 nets for ECC83 and 114 components/80 nets for multichannel. Expanded source identities matched 26 and 169 units across 1 and 5 sheet instances. Complete ERC matched the corresponding language-specific baselines. Original examples, baseline copies and prepared project/configuration inputs remained unchanged during the captured runs. No editor was launched.

Configuration probes establish initialization requirements. The first native CLI run created `cvpcb.json` and `symbol_editor.json` and rewrote `kicad_common.json`; repeating the same command left configuration bytes stable. Conflicting external environment and stored configuration variables changed native symbol lookup. Project text variables also affected library URI expansion. A conflicting external `KIPRJMOD` did not replace the loaded project's directory. Language selection retained diagnostic counts while changing exact report descriptions. Raw reports, initial configuration hashes and the failed initial immutability assertion remain in the recorded evidence directories.

Architecture tests check bounded, order-independent and unambiguous environment hashing; executable and configuration-directory changes; redacted debugging; explicit command environment/directory; and refusal of executable changes before and after dispatch. The existing native fixed-circuit CLI/MCP regression checks compatibility.

## Scope

This context fixes process inputs. It does not snapshot configuration-file contents, native dependencies, locale resources, project files or library files, and it does not authorize schematic writes. Private configuration initialization, complete resource selection, effective-variable resolution and source/library/configuration snapshots must be composed with this API for general schematic validation. Public MCP capabilities are unchanged.

The `check-schematic-execution` example takes an owned schematic, prepared configuration root, retained baseline XML, retained baseline ERC JSON and a new output directory. It records context digests, performs both native commands, resolves source identities and compares the results. [native-execution.json](native-execution.json) records quality gates and artifact hashes; retained scripts reproduce configuration preparation and native probes.
