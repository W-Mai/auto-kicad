# Existing footprint flips

`flip_footprint` in `kicad_plan_change` flips one unlocked footprint about its current position. The request selects an explicit target side and axis. Current revision, observed placement identity and native UUID are required. A footprint already on the requested side rejects; an approved flip cannot be replayed as a toggle.

```json
{
  "endpoint": "ipc:///tmp/kicad/api.sock",
  "source_run_id": "pcb-edit",
  "request_id": "flip-connector",
  "flip_footprint": {
    "observed_revision": "sha256:<current revision>",
    "placement_id": "<observed placement_id>",
    "source_uuid": "<native UUID>",
    "target_side": "back",
    "axis": "top_bottom"
  }
}
```

`target_side` accepts `front` or `back`; `axis` accepts `top_bottom` or `left_right`. Each flip requires its own plan. The workflow is inspect → plan → isolated preview → exact approved commit → save and reload. IDs, reference/value, schematic association, pad numbers/nets, unrelated native objects and protected project/rule/schematic files are preserved.

## Geometry and limits

The native expectation reflects board-space positions, swaps paired front/back layers, reflects side-specific text and applies KiCad's left/right text-upright adjustment. Normalized pad positions remain footprint-local; relative angles and pad-local shape offsets/chamfers reflect. Complete native receipts and saved payloads are compared independently with source-derived expectations. Comparison canonicalizes cyclic finite angles without rounding, pad layer-set ordering, the direction of the primary full-span drill and the transient board-container ID. Other fields, unknown protobuf data and all object IDs remain exact. Normalized orientations and content hashes use millidegrees in `[0, 360000)`.

Supported targets use normal pad stacks, front/back pad layers, fixed paired technical layers and line/circle/arc/rectangle/Bezier footprint graphics. Custom pad stacks/primitives, inner-layer pad definitions, polygon children, custom user-layer pairings and unknown children reject. Locked footprints, existing copper contact, zones or unsupported copper also reject. Native pad-cluster evidence must preserve all existing pad connections. CLI DRC must introduce no violations or increased unconnected count. No rerouting or automatic copper movement occurs.

A confirmed native transaction discard rejects the operation. Uncertain commit/save, failed discard, mismatched saved data or cleanup failures return `unknown` with durable recovery evidence. Failed previews issue no approval and leave the source unchanged. External writers must remain stopped; the server coordinates its own writers only.

## Acceptance

The [result record](footprint-flip-result.json) records production stdio MCP and KiCad 10.0.6 acceptance. One source editor was reused, with serialized disposable previews and owned-editor cleanup. Asymmetric footprints starting on both sides, with nonzero orientations, passed both axes and complete round trips. The fixture includes rotated pads, native fields, line/circle/arc children, schematic association and unrelated routing. Full native payloads, independent normalized state, source isolation, protected files and saved reload were checked for every successful operation.

Locked/connected targets, wrong native identity, stale revisions, same-side requests, incorrect approval and approval replay rejected. A flip into foreign-net copper failed isolated preview without approval or source mutation. Existing baseline violations remain; the final DRC introduces no violations and preserves the unconnected count. Offline contracts separately check malformed native receipts, unknown-field preservation, sub-millidegree drift, unsupported children, cyclic orientation hashing and transaction failure classification. Transport crashes were not injected into the live editor. This synthetic fixture is not a real-project delivery profile. The full quality gate passed 365 tests, with 14 environment-dependent tests ignored; live acceptance passed separately in 137.81 seconds. Existing field-edit and footprint-movement stdio regressions also passed on the shared implementation in 39.68 and 81.65 seconds.

```sh
env CARGO_TARGET_DIR=/tmp/auto-kicad-flip-target CARGO_INCREMENTAL=0 CARGO_PROFILE_DEV_DEBUG=0 CARGO_PROFILE_TEST_DEBUG=0 cargo build -p auto-kicad --bin auto-kicad
env CARGO_TARGET_DIR=/tmp/auto-kicad-flip-target CARGO_INCREMENTAL=0 CARGO_PROFILE_DEV_DEBUG=0 CARGO_PROFILE_TEST_DEBUG=0 AUTO_KICAD_FLIP_SERVER=/tmp/auto-kicad-flip-target/debug/auto-kicad AUTO_KICAD_FLIP_EVIDENCE=/tmp/auto-kicad-flip-result.json cargo test -p auto-kicad --lib tests::footprint_flip_acceptance::live_existing_footprint_flip_stdio_lifecycle -- --ignored --exact --nocapture
```

Run `python3 scripts/quality.py` for the complete offline gate. Remove the temporary target after all build/test processes exit. Acceptance does not replace the installed user MCP executable. Item deletion has a separate contract and remains unavailable through MCP in this delivery.
