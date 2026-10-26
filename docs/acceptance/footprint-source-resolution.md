# Local footprint source resolution

`kicad_adapter::library_catalog` resolves exact footprint identifiers from bounded local library tables and server-owned policy. Project nicknames override global nicknames. Nested-table cycles and effective-name collisions reject. Disabled parent tables reserve their child names through read-only metadata decoding, preventing a fallback from bypassing disabled configuration; their footprint files are not loaded. Unknown selected formats, options or variables reject. Hidden entries may resolve by exact name.

Project table includes are confined to canonical project/extra roots before reading. Project footprint directories must stay within project, installed official, usable global-library or explicitly approved roots. Final footprint symlinks reject; canonical directory aliases retain their proven target. `${KIPRJMOD}` is derived from the saved source project, named path variables come from server policy and shell/environment fallback is unavailable. Legacy explicit mappings and missing-nickname fallbacks remain separate; a disabled or invalid selected nickname cannot trigger fallback.

Resolution evidence binds policy, table presence/content, inclusion chain, identifier, selected canonical root/path and bounded footprint bytes. The catalog is rechecked before and after source reading. File contents have an 8 MiB limit; table traversal has 8 inclusion levels, 64 file visits, 8 MiB aggregate bytes and 4096 total rows. Unavailable unrelated native library directories grant no root access; a requested unavailable source rejects.

On 2026-10-26 the installed global/nested configuration resolved 157 effective libraries. An absent project table was included in the evidence. `Package_SO:SOIC-8_3.9x4.9mm_P1.27mm` and `Package_DFN_QFN:QFN-32-1EP_5x5mm_P0.5mm_EP3.65x3.65mm_ThermalVias` resolved through that configuration with their previously inventoried hashes. The [result record](footprint-source-resolution-result.json) contains source sizes and digests. No editor was opened and no configuration or PCB was changed.

Retained architecture tests cover project precedence, same-byte source identity changes, table appearance, changed contents, aliases, disabled parents, cycles, duplicate inclusion names, confinement, explicit extra roots, variables, legacy fallback, source-size/depth limits and symlinks. Reproduce a read-only source check with explicit operator paths:

```sh
env CARGO_TARGET_DIR=/tmp/auto-kicad-library-check CARGO_INCREMENTAL=0 CARGO_PROFILE_DEV_DEBUG=0 cargo run -p kicad-adapter --example check-footprint-source -- PROJECT_DIR GLOBAL_FP_LIB_TABLE OFFICIAL_FOOTPRINT_ROOT 10 Package_SO:SOIC-8_3.9x4.9mm_P1.27mm
```

The source check verifies resolution only. The public placement pipeline still uses its existing registry; generalized MCP placement and complete native pad preservation are not established by this result. Remove the temporary Cargo target after verification exits.

The complete quality gate passed formatting, warnings-denied Clippy and 377 tests, with 15 environment-dependent tests ignored.
