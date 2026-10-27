# Combined geometry bounds and compact recovery records

Execution and recovery records use compact JSON with the existing 4 MiB read/write bound. The schema, evidence fields, approval hashes, source backups, pending markers and atomic durable writes are unchanged. Existing JSON records remain readable. An architecture contract verifies dense evidence/outcome round trips and recovery without field loss, and rejects an actual oversized payload.

## Native acceptance

KiCad 10.0.7 production stdio acceptance replaces 100 existing outline objects with 32 closed loops containing 512 segments, and creates 32 ordinary copper zones in the same approved transaction. Every created zone has 512 polygon vertices, including 16 holes. The fixture contains existing footprints, tracks, vias and filled ground planes; 32 additional fixture ground vias connect the new copper to the existing ground network.

The complete lifecycle covers planning, isolated preview, wrong approval and replay refusal, permanent custom-rule drift revocation, source commit, full native/text/copper preservation, independent refill, complete DRC correspondence, protected files and same-process saved reload. Independent KiCad Python reading checks every requested outline/zone field and confirms all 32 new zones contain filled copper and belong to the ground component. Geometry bounds are not satisfied by empty fills.

Six controls reject 101 removals, 513 outline edges, 33 loops, 33 zone mutations, 513 zone vertices and 17 holes before source mutation. Saved bytes and the complete native geometry observation remain unchanged.

A separate recovery run on the accepted dense result board passed both corrupted successful SaveDocument replies and MCP termination during a held pre-write read. Unknown/in-flight records block writes from both state directories and refuse recovery while the editor is open. Closed-document recovery restores native content and DRC, followed by an accepted geometry commit and same-process reload. An earlier attempt rejected the second case during preview-editor startup before fault injection; its failed evidence remains retained separately.

## Evidence and limits

The initial request used an invalid inactive-island threshold and rejected during planning. After correction, isolated test zones were refused by preview DRC. Connecting fixture ground vias preserved the same geometric maxima and passed preview, but indented recovery evidence exceeded 4 MiB before any source mutation. The retained baseline/result PCB hashes were equal. Compact serialization allowed the same complete evidence to fit: the accepted 512-edge/32-zone execution record was 2432303 bytes, versus 8207485 bytes for an equivalent indented representation. Final profile sizes, binary hashes, process cleanup and artifact hashes are recorded in [the evidence manifest](geometry-boundaries.json).

This acceptance qualifies the listed mutation bounds on populated synthetic boards. It does not establish GUI fill cancellation, useful geometry/routing edits on a real populated zoned project, general schematic editing/synchronization, installation/release delivery or the final three-real-project KiCad/Codex gate.

## Reproduction

Create an ordinary zoned fixture first, then generate a request and its connected source fixture. All output paths must be new.

```sh
/Applications/KiCad/KiCad.app/Contents/Frameworks/Python.framework/Versions/3.9/bin/python3.9 scripts/prepare-zoned-additive-fixture.py /absolute/new-base-fixture
/Applications/KiCad/KiCad.app/Contents/Frameworks/Python.framework/Versions/3.9/bin/python3.9 scripts/prepare-maximum-geometry.py /absolute/new-request.json --fixture-source /absolute/new-base-fixture/board.kicad_pcb --fixture-output /absolute/new-maximum-fixture
env CARGO_TARGET_DIR=/private/tmp/auto-kicad-m4-target CARGO_INCREMENTAL=0 CARGO_PROFILE_DEV_DEBUG=0 CARGO_PROFILE_TEST_DEBUG=0 CARGO_BUILD_JOBS=1 python3 scripts/quality.py
env CARGO_TARGET_DIR=/private/tmp/auto-kicad-m4-target CARGO_INCREMENTAL=0 CARGO_PROFILE_DEV_DEBUG=0 CARGO_BUILD_JOBS=1 cargo build -p auto-kicad --bin auto-kicad --example check-geometry-mcp --example check-geometry-recovery
env CARGO_TARGET_DIR=/private/tmp/auto-kicad-m4-target CARGO_INCREMENTAL=0 CARGO_PROFILE_DEV_DEBUG=0 CARGO_BUILD_JOBS=1 cargo build -p kicad-ipc --example geometry-fault-relay
env AUTO_KICAD_GEOMETRY_BOUNDARY=1 /private/tmp/auto-kicad-m4-target/debug/examples/check-geometry-mcp /private/tmp/auto-kicad-m4-target/debug/auto-kicad /absolute/new-maximum-fixture/board.kicad_pcb /absolute/new-request.json /absolute/new-maximum-result
/private/tmp/auto-kicad-m4-target/debug/examples/check-geometry-recovery /private/tmp/auto-kicad-m4-target/debug/auto-kicad /private/tmp/auto-kicad-m4-target/debug/examples/geometry-fault-relay /absolute/new-maximum-result/result.kicad_pcb /absolute/new-recovery-result
```

Each runner closes its owned editors and preserves result files. Recovery closes and reopens only its owned document when required by the closed-document contract. Retain evidence and result boards when cleaning the temporary Cargo target.
