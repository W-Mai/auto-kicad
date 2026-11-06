# Schematic acceptance through the Codex MCP client

`scripts/check-codex-schematic.py` runs the existing native schematic intent drivers through Codex app-server. Each run initializes Codex, creates an ephemeral context, discovers the selected server and calls `mcpServer/tool/call` against that context's actual MCP connection. No model turn is started. The original drivers retain exact plan/preview/approval checks, native expectations, publication evidence and source/configuration preservation assertions.

```text
python3 scripts/check-codex-schematic.py general SERVER OWNED_SOURCE OWNED_CONFIG NEW_OUTPUT --extended
python3 scripts/check-codex-schematic.py component SERVER OWNED_SOURCE OWNED_CONFIG NEW_OUTPUT
python3 scripts/check-codex-schematic.py component SERVER OWNED_SOURCE OWNED_CONFIG NEW_OUTPUT --file channel_strip.kicad_sch
python3 scripts/check-codex-schematic.py children SERVER OWNED_SOURCE OWNED_CONFIG NEW_OUTPUT
```

Supported driver selectors are `general`, `component`, `multiunit`, `inheritance`, `sheets`, `children` and `removal`. Each selected driver validates its explicitly marked owned fixture and accepts its existing options. The portable entry point's actual read-only child probe is separately verified. The recorded mutating client scope covers single-sheet properties/wires/labels/markers/catalog symbols, complete multi-unit component edits, repeated-sheet annotation/assignment and child creation/retargeting; exposing another driver selector does not establish additional Codex acceptance by itself.

## Configuration and lifecycle

The selected server executable and its configuration are temporary app-server overrides. `KICAD_CONFIG_HOME`, `AUTO_KICAD_STATE_DIR` and `TMPDIR` are passed explicitly in the MCP server environment. Parent custom environment inheritance must not be assumed. Runtime overrides do not modify the user's registered server configuration. `scripts/check-codex.py` separately verifies discovery through the registered local installation.

The transport writes compressed complete app-server request/response traces alongside the existing intent records. Its contexts are ephemeral. Closing the client can terminate an MCP process that still owns private snapshots. The acceptance owner records descendant PIDs/start times, confirms those processes stop, removes only its exclusive temporary snapshots, and retains a lifecycle record. This cleanup is a recorded test intervention; it does not qualify automatic server cleanup after arbitrary client termination. No process is selected for termination by application name.

## Native client qualification

Actual Codex CLI 0.165.0 and KiCad 10.0.7 accepted 53 committed executions through real app-server MCP connections. The selected profiles contain 489 actual MCP calls and 220 independently rehashed publication artifacts. Single-sheet acceptance covers all supported basic property/flag/reference/footprint/symbol/wire/marker/label operations, an explicit electrical merge, source/library/configuration drift, wrong approval, replay and process restart. Complete component and four-instance shared-sheet cases verify consistent unit fields, per-instance references, footprint assignments and independent saved native grouping. Child cases retain complete network/ERC comparisons after creation, populated attachment, retargeting, annotation reuse and removal.

An actual Codex-owned MCP writer was killed during new-child publication. Fresh Codex connections reject replay and ordinary writes, preserve later user bytes, perform newly approved guarded recovery, restore exact original content/new-file absence and accept a subsequent normal commit. The recorded descendant set is confirmed stopped before its exclusive orphaned snapshots are removed.

An earlier environment-selection failure remains excluded. Two intent runners completed their mutation steps but stopped at teardown assertions; their original nonzero outcomes are retained. Separate postcondition continuations verified terminal service identity, completed owner cleanup, performed fresh Codex observation and independent saved native checks, and rehashed all artifacts. These continuations are identified explicitly in [machine-readable evidence](schematic-codex-client.json). Full quality passes 566 tests with zero failures and 16 environment-dependent tests ignored. The temporary transport boundary test passed and was removed.

## GUI gate

[Native GUI qualification](native-gui-qualification.md) records actual component/general/nested loading and saving, configured page labels 99/100, complete native network/ERC and current MCP compatibility. The original permission-denied attempt remains retained separately. Native legacy cache/default/annotation migrations are explicit; exact legacy source/cache byte preservation is not claimed. Every owned editor closes and original accepted inputs remain unchanged.

The client protocol is documented in [Codex App Server](https://learn.chatgpt.com/docs/app-server). The generated schema from the tested local Codex version is retained with the private acceptance evidence.
