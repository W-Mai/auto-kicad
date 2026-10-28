# Symbol library context

`SymbolCatalog` loads native symbol library tables using the shared bounded table parser, inclusion traversal, native flag overrides and canonical path policy. Project rows override global rows. A project alias may reuse an approved global symbol file; that permission does not include neighbouring files. Used non-native plugins, plugin options, unresolved variables, escaping project paths and symlinked source files are rejected.

`SymbolLibraryContext` retains effective table provenance and the exact bytes' digests of requested used library files. Missing nicknames, disabled rows and missing files remain distinct observations. The context is constructed from filesystem reads and cannot be deserialized into authority. Capture rechecks table state and reads used files again; verification rejects changed content, presence, path or catalog state. File reads are limited to 32 MiB per symbol library and 128 MiB per capture.

## Native acceptance

KiCad 10.0.7 checked owned ECC83 and multichannel project copies. Both used a project symbol table plus the installed global table and its included default table. A separate ECC83 copy used an isolated native configuration with its library row moved into the global table. Native ERC baselines were 0 diagnostics for both ECC83 profiles and 4 active errors plus 32 warnings for multichannel.

Each profile changed an owned used symbol library by one newline. Native ERC remained equivalent while context verification rejected the changed file bytes. Changing a project symbol table by one newline also invalidated the captured context. Restoring the original inputs restored verification and native ERC equivalence. Original examples and retained baseline copies remained unchanged; modified acceptance inputs were restored byte-for-byte. No editor was launched.

Architecture tests cover explicit absence, file appearance, global-table appearance, byte changes, nested tables, project overrides, exact-file global permissions, disabled rows, unsupported plugins, table-family mismatches, dangling links and source-size bounds. Shared footprint-catalog tests retain the existing PCB lookup behavior.

## Scope

This API records requested symbol library files. It does not determine the complete set of used symbols, resolve symbol inheritance, verify pin connectivity, prove native lookup equivalence for arbitrary configurations, or bind the complete project/configuration/locale context. It grants no schematic write authority. The footprint catalog API and public MCP capabilities are unchanged.

The `check-symbol-context` example accepts an owned schematic, global symbol table, official symbol directory and a new output directory. It requires the exact `.auto-kicad-context-test-owned` marker and modifies only a used library and table inside that marked project, retaining reports before and after restoration. Use the retained preparation scripts for project and isolated-configuration setup. Machine-readable evidence and artifact hashes are in [symbol-library-context.json](symbol-library-context.json).
