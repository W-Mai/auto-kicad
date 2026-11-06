# Native export project isolation

Native exports execute on an independent project copy and retain complete immutable source/audit evidence. Only the exact root schematic `.kicad_prl` in that worker copy may initialize or change during generation. All other worker CAD bytes and namespace entries remain exact. The completed worker project, local preferences, export configuration and artifact inventory are sealed before approval and rechecked during publication. Private workspace evidence is separate from persistent source dependencies.

BOM validation uses resolved physical components and every file-bound source unit. Complete multi-unit components produce one quantity-1 row, and all source units must agree on BOM/DNP flags. Native concatenated UUID lists are resolved by the shared identity index.

| Actual Codex profile | Published outputs | Independent checks |
| --- | --- | --- |
| Complete accepted ECC83 pair | 13 files: Gerber, plated/non-plated Excellon, BOM, PDF and manifest | 11 eligible BOM rows; U1 represents 3 source units with Value `ECC83 / 12AX7`; 1 PDF page; 33 physical holes; exact source/configuration preservation |
| Reused child fixture | BOM, PDF and manifest | 4 quantity-1 BOM rows across 2 child instances; 3 PDF pages match the native sheet inventory; exact original source/configuration preservation |

Both profiles use the installed candidate launcher through actual Codex app-server connections. Wrong complete-inventory approval and commit replay reject. All 25 durable files, including 16 published outputs, are independently rehashed. Persistent source dependencies still exist with their exact expected digests after private-workspace cleanup; they contain no transient worker/snapshot paths. PDF page counts are read independently with `pdfinfo`. These checks do not claim schematic GUI page-display qualification.

The isolated macOS arm64 candidate supports KiCad 10.0.7, contains 140 locked dependency/license records and retains one release with `--no-rollback`. Its executable matches the source-provenance digest, and its separate replay executable matches the existing installation. Global activation is unchanged during this candidate qualification; public distribution is not performed.

## Recorded refusal and failed controls

The complete Sonde source export attempt rejects before planning because U1 combines cached definitions `74LS125_1`, `74LS125_6`, `74LS125_7` and `sonde_xilinx:74LS125`; native XML exposes only `74LS125_1` as its component library part. This exceeds the existing complete-component definition consistency boundary. Its original and copied source/configuration remain exact, no export journal is created, and the attempt is not counted as a passing export. Its separately accepted PCB-only edit remains valid. Mixed per-unit library definitions remain unsupported; no source metadata or identity guard is relaxed.

Retained failed attempts record the source-preview preference mutation and the concatenated multi-unit BOM UUID lookup. Both reject before publication. An nng-sys native build exits without a detailed diagnostic; one cached quality retry passes without a source change. All logs remain separate from passing results.

## Cleanup and remaining gates

Final quality passes 636 tests with 0 failures and 18 environment-dependent skips, formatting, capability-matrix validation and warnings-denied workspace/all-target Clippy. The sole Cargo target is absent; batch receipts record 5825122304 allocated build bytes removed. Failed isolated installations are uninstalled through their ownership-checked installer; their manifests and failure evidence remain. The isolated final candidate remains available.

Client lifecycle records distinguish private directories removed by the acceptance owner from normal server cleanup. Subsequent read-only production startup reclaims 2 stale owner records with no remaining private files; no registry or lease guard is bypassed. Recorded owned descendants are terminal. GUI cancellation, schematic load/save and configured displayed pages, saved-pair GUI qualification and final global activation remain separate open gates.

[Machine-readable evidence](native-export-isolation.json) records exact source/runtime/package identities, outputs, dependencies, source refusal, failures, cleanup and lifecycle scope.
