# Footprint library table decoding

`kicad_adapter::library_table` reads one native footprint library table and records its requested/canonical path, presence, byte size and SHA-256. It retains nickname aliases, literal URIs, disabled/hidden flags, nested-table entries and unsupported format names. Decoding does not expand variables, follow table inclusions, grant source access or change a board. The existing footprint placement registry remains in use.

The decoder accepts the version-7 and unversioned `fp_lib_table` syntax. It rejects duplicate nicknames or fields, unknown structure, invalid identity components and malformed scalar values. Limits apply before recursive CST parsing: 1 MiB input, 32 nesting levels and 4096 entries. File reads require an absolute path and a bounded regular file, distinguish absence from dangling links, cap the actual read and capture the content digest. Source-root confinement and catalog precedence must be established by a resolver before entries can authorize loading.

On 2026-10-26 the decoder read the installed KiCad 10 configuration without changing it: the 452-byte global table contained three entries including one nested table; the 25,235-byte installed default table contained 155 entries. Their SHA-256 values were `02511f4212d34080642667e02cdce26c665d629af776a57af341e49e7cd1e03e` and `4042e1c92709d4f872d4cdeaddfa7ec0f6bd49be240e0d9668b76e89888b1e77`. A temporary filesystem probe passed absence/presence, changed-content digest, oversized-file, directory and dangling-link checks and was removed afterward. No editor was launched.

Retained architecture tests verify representation preservation, ambiguity rejection and resource bounds:

```sh
cargo test -p kicad-adapter library_table -- --test-threads=1
```

This evidence establishes table decoding only. It does not establish generalized library resolution, source approval, footprint placement or installed Codex runtime behavior.

The full repository gate passed formatting, warnings-denied Clippy and 370 tests, with 15 environment-dependent tests ignored.
