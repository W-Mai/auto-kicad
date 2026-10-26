# Outline request compilation

`OutlineEdit` is a serializable request containing exact `remove_ids` and named loops. Each `OutlineLoop` contains ordered segment/arc edges and a stroke width; `OutlinePoint` uses integer nanometres. Unknown JSON fields reject. The [replayable fixture](../../scripts/fixtures/outline-edit.json) describes a curved boundary, rectangular hole and separate concave boundary.

`compile_outline_edit` checks a requested observation revision against the current revision, unique bounded loop/removal identities, supported existing outline types and unlocked removal targets. Loops require exact endpoint closure and distinct edge control points. Straight loops require nonzero signed area; exact integer predicates reject intersecting/overlapping straight edges and non-adjacent touches, including contacts between requested loops. Arcs require distinct noncollinear control points and retain their exact coordinates.

Requests are bounded to 32 loops, 512 total primitives and 100 removals; source inventories are bounded to 4,096 outline objects. Coordinates remain within ±2,000 mm, widths within 1–2,000,000,000 nm and the requested control-point span within 1,000 mm. No coordinates are snapped or inferred. Compiled target identities derive deterministically from loop identity and edge index. A removal-only request remains subject to native verification of the remaining board boundary.

`CompiledOutlineEdit` has private contents and exposes immutable removals/primitives. Every compiled result requires native topology verification. Compiler success does not establish arc intersections/extrema, curved-loop area, hole containment or the combined topology of new and retained boundaries. Those properties, copper clearances and resulting rule checks require the complete disposable preview gate before source execution. These primitive checks alone cannot authorize a commit.

The adapter binds compilation to complete observed native payloads and byte-exact live board text using length-framed SHA-256 evidence. It derives removal targets from actual Edge.Cuts records and translates compiled coordinates unchanged into native primitives. This content revision does not replace source file, editor-instance, rule, approval or recovery binding in the production pipeline.

## Reproduction and evidence

```sh
cargo build -p kicad-adapter --example check-native-outline
target/debug/examples/check-native-outline /absolute/new-output
```

The probe reads the JSON intent, observes its owned board, compiles against the actual evidence revision and executes the resulting native parameters. It rejects stale revision, open-loop and locked-removal requests before the successful transaction. Actual KiCad 10.0.7 replaced 4 old segments with 14 compiled primitives, including two semicircular arcs, and preserved the locked circle and all unrelated native routing/graphics. Complete receipts match saved and same-process reloaded objects; DRC introduces zero violations and leaves zero unconnected items. Original input files, settled project bytes and owned-editor cleanup passed.

An independent bundled-Python parser loads the saved result and compares every created primitive's type, layer, width, start/end and arc midpoint directly with the original JSON intent. All 14 objects match exactly, providing evidence independent of the compiler/native conversion. Closed-copy CLI controls also detect open boundaries, straight-edge self-intersection and copper-edge collision. The output retains intent, compiled primitives/revision, native saved PCB/project and all check records. See [compact evidence](outline-request-compilation.json).

Offline quality passed 414 tests with 15 environment-dependent tests ignored, format validation, warnings-denied Clippy and capability validation. Architecture tests cover request serialization, revision/lock/source checks, exact straight-edge geometry, valid 512-edge closure, excess counts and unchanged arc controls with mandatory native verification.

This acceptance covers backend request compilation and native execution on a synthetic fixture. General curve/retained-boundary validation, public geometry MCP requests, source approval/recovery, zoned-board refill and final real-project profile acceptance remain unfinished. The existing public rectangular-outline workflow is unchanged.
