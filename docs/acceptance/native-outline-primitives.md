# Native outline primitives and exact replacement

`OutlinePrimitive` represents a segment or a circular arc defined by start, middle and end points, with an explicit stroke width. `KiCadClient::create_outline_primitives` creates 1–512 primitives on Edge.Cuts inside the caller's native transaction. Coordinates are bounded to ±2,000 mm, width to 1–2,000,000,000 nm and the batch control-point span to 1,000 mm. Repeated points, collinear arc controls and oversized batches reject before creation. Whole-arc extrema, closed contours, intersections, holes and copper clearances require planning/native validation; these primitive bounds alone do not establish a valid board outline.

Requests explicitly select solid strokes, unfilled shapes and unlocked state. Every successful item receipt must have a distinct bounded UUID, exact requested geometry, width, layer, stroke, fill and lock values, with no assigned copper net. Complete native receipt payloads retain unknown and additional fields. Saved comparison removes only the transient board parent and compares every remaining field exactly. No coordinate tolerance or arc tessellation is introduced.

`delete_outline_items` accepts 1–100 exact observed board-level Edge.Cuts IDs. It requires unlocked segment, arc, circle, rectangle or polygon records and identical fresh native geometry before deletion. The native deletion receipt remains provisional until saved verification. `ExistingBoardItemSet::matches_after_outline_replacement` verifies explicit removed IDs, receipt-backed disjoint new IDs and exact surviving routing/graphic payloads. It does not validate new payloads independently of their receipts. The existing general item-deletion policy remains unchanged.

## Reproduction and evidence

```sh
cargo build -p kicad-adapter --example check-native-outline
target/debug/examples/check-native-outline /absolute/new-output
```

The native probe uses one owned editor and a synthetic board containing two copper tracks, a rectangular outline, a locked circular inner boundary and unrelated rectangle/polygon graphics. It rejects locked, wrong-layer, missing and duplicate removal targets, and rejects 513 requested primitives. A 512-item native creation is then cancelled; complete geometry returns to the initial observation. This establishes maximum creation/cancellation behavior, not maximum-batch committed MCP acceptance.

The committed transaction replaces 4 original boundary segments with 14 objects: a curved outer boundary containing two semicircular arcs, a rectangular internal cutout and a separate concave outer boundary. The original locked circular boundary and unrelated graphics/tracks remain exact. All new complete receipts match saved and same-process reloaded native records. Original input files and settled project bytes remain unchanged. Native DRC introduces zero violations, retains the same two dangling-track warnings and reports zero unconnected items. See [compact evidence](native-outline-primitives.json).

After editor cleanup, `scripts/check-outline-topology.py` checks three closed copies with native CLI DRC. Removing an outer segment and crossing cutout edges each produces `invalid_outline`; moving the cutout through a track produces `copper_edge_clearance`. Both checks are enabled as errors and the copper-edge minimum is positive. The accepted result remains byte-identical. Each negative case uses a separate bundled-Python parser process; reusing multiple native boards in one process exposed a SWIG object-lifetime failure, so that failed attempt is retained separately. These are native DRC controls, not production planning refusals.

Offline quality passed 411 tests with 15 environment-dependent tests ignored, format validation, warnings-denied Clippy and capability validation. Architecture tests cover primitive bounds, exact receipts, retained unknown fields, disjoint identities and preservation without broadening existing deletion policy.

This native interface is an implementation foundation. [Outline request compilation and content revision binding](outline-request-compilation.md) have separate acceptance. Preview/approval, protected-file and footprint evidence integration, zoned-board refill, general curve/topology acceptance and production geometry MCP lifecycle are unfinished. The synthetic board does not count toward the three-real-project gate.
