# Zoned additive PCB changes

Explicit routes, single pad-bound tracks, named-net through vias and mixed additive graphics/copper batches use complete whole-board refill verification when the board contains zones or generated teardrops. The operation prefix is bound to one internal refill-only geometry tail. Public geometry definition edits remain exclusive, and existing-item move/flip/delete copper restrictions remain unchanged.

## Approval and preservation

Planning binds complete native and saved source state, effective routing rules, project/library provenance and the physical additive batch. New track/via UUIDs derive from the original source identity/revision and batch, and the preview and source use the same allocation. Complete original/nested/generated namespaces are checked before dispatch; exact receipt identities, counts and physical content are required. Existing identities are preserved. Native KiCad uses generator UUIDs to order equal-area teardrops; shared creation inputs retain strict generated priority and content comparison.

Native CommitChanges may generate teardrops when copper is added. Intermediate checks retain all authored definitions/settings and ordinary fills; final approval requires complete generated/native/text correspondence, same-process saved reload, independent CLI refill, protected file checks, retained pad partitions and no introduced DRC violations. Graphics-only additions retain strict intermediate generated preservation.

Complete DRC comparison uses immutable repeated live connectivity and independently loaded closed-board connectivity bound to exact saved/private DRC inputs and raw report hashes. The bounded comparator permits equivalent unconnected endpoints only within proven physical components. Shared endpoint records, unknown fields, diagnostic multiplicity, net identity and exact reporting coordinates remain significant. Other complete DRC callers retain strict comparison.

## Verification

KiCad 10.0.7 production stdio acceptance passed 4 operation families on both ordinary and teardrop-enabled populated synthetic zoned copies. The generated fixture produced two regions after routing and 4 after the mixed batch. Each family verified plan/preview/approval/source commit, exact creation IDs/counts, complete candidates, connectivity and DRC, durable evidence, saved reload, invalid approval and replay refusal. Custom-rule changes revoked approval, including after restoration. Both original fixtures remained unchanged.

Actual recovery acceptance covered a successful native save with a corrupted reply and MCP termination during a held read before source writes. The outcomes were unknown and in-flight respectively; same-state and separate-state commits were blocked before native preflight, recovery while the editor remained open was refused, and no further IPC followed the fault. Explicit closed-document recovery restored saved/native content and DRC. A subsequent ordinary geometry commit and same-process reload passed.

Offline quality passed 481 tests, zero failures and 15 explicitly ignored environment-dependent tests, with formatting, warning-denied workspace/all-target Clippy and capability validation. The evidence manifest records exact binaries, outputs, raw reports, source backups and editor cleanup. A rejected mixed execution remains preserved separately: its generated priorities differed because preview/source used independent creation UUIDs. The accepted implementation preserves priority equality.

These fixtures are synthetic and do not count toward the three-real-project product gate. GUI refill cancellation, combined maximum-size zoned MCP qualification, remaining populated-profile coverage, general schematic editing/synchronization, packaging and manufacturing exports remain outside this acceptance.

## Reproduction

Use the bundled KiCad 10.0.7 Python environment and a new output directory for each command. The fixture generator settles native property identities before copies are opened.

```sh
/Applications/KiCad/KiCad.app/Contents/Frameworks/Python.framework/Versions/3.9/bin/python3.9 scripts/prepare-zoned-additive-fixture.py /absolute/new-generated-fixture --teardrops
/Applications/KiCad/KiCad.app/Contents/Frameworks/Python.framework/Versions/3.9/bin/python3.9 scripts/prepare-zoned-additive-fixture.py /absolute/new-ordinary-fixture
env CARGO_TARGET_DIR=/private/tmp/auto-kicad-m4-target CARGO_INCREMENTAL=0 CARGO_PROFILE_DEV_DEBUG=0 CARGO_PROFILE_TEST_DEBUG=0 CARGO_BUILD_JOBS=1 python3 scripts/quality.py
env CARGO_TARGET_DIR=/private/tmp/auto-kicad-m4-target CARGO_INCREMENTAL=0 CARGO_PROFILE_DEV_DEBUG=0 CARGO_BUILD_JOBS=1 cargo build -p auto-kicad --bin auto-kicad --example check-copper-route-mcp --example check-geometry-recovery
env CARGO_TARGET_DIR=/private/tmp/auto-kicad-m4-target CARGO_INCREMENTAL=0 CARGO_PROFILE_DEV_DEBUG=0 CARGO_BUILD_JOBS=1 cargo build -p kicad-ipc --example geometry-fault-relay
env AUTO_KICAD_ACCEPTANCE_REQUIRE_GENERATED=1 /private/tmp/auto-kicad-m4-target/debug/examples/check-copper-route-mcp /private/tmp/auto-kicad-m4-target/debug/auto-kicad /absolute/new-generated-result --zoned /absolute/new-generated-fixture/board.kicad_pcb
/private/tmp/auto-kicad-m4-target/debug/examples/check-copper-route-mcp /private/tmp/auto-kicad-m4-target/debug/auto-kicad /absolute/new-ordinary-result --zoned /absolute/new-ordinary-fixture/board.kicad_pcb
/private/tmp/auto-kicad-m4-target/debug/examples/check-geometry-recovery /private/tmp/auto-kicad-m4-target/debug/auto-kicad /private/tmp/auto-kicad-m4-target/debug/examples/geometry-fault-relay /absolute/new-generated-fixture/board.kicad_pcb /absolute/new-recovery-result --additive
```

The runners own and close their source/preview editors. Recovery deliberately requires closing the owned document before restoration and reopening it afterward. Retain the result boards, JSON evidence and small logs when removing the task-owned Cargo target. See [the acceptance manifest](zoned-additive-mcp.json).
