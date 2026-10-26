# Explicit copper routes through MCP

`kicad_plan_change` accepts `route` either at the top level or as an item in `batch`. It is exclusive with the other operation fields in that item. A route specifies `route_id`, the current `data.pcb.revision` as `observed_revision`, explicit observed `net`, two anchors, `legs` and `vias`. Anchors use `kind: "pad"` with physical `pad_id` and `footprint_placement_id`, `kind: "track_end"` with native `source_uuid` and `end: "start" | "end"`, or `kind: "via"` with an existing through-via UUID. Both anchors must resolve to the supplied network in the observed board.

Each leg specifies canonical `layer`, `width_nm` and at least two explicit `{x_nm, y_nm}` points. Each transition via specifies `via_id`, `position`, `pad_diameter_nm` and `drill_diameter_nm`. Layer changes require one explicit full-span through via at the shared leg endpoint. Bounds are 16 legs, 128 nonzero segments and 15 vias per route, and 256 new copper objects per batch. Route batches may include existing track, via, graphic and rectangular-outline additions. Unsupported anchors, disabled layers, discontinuities, wrong networks, stale revisions and conflicting identities reject before native mutation.

The existing pad-bound `track` request accepts an optional canonical `layer`; omission retains F.Cu and the existing `AddStraightTrack` operation family. The selected layer must be enabled and present at both physical endpoint pads. B.Cu authoring passed actual production MCP acceptance. Routing does not move anchors or choose paths automatically.

Route plans return `routing_evidence` containing complete saved project/custom-rule digests, observed stackup, effective native netclasses and a digest of complete native items, footprints and pad connectivity. These server-derived digests enter the plan hash. Preview requires equivalent rule content in the disposable editor and identical original native evidence. Source preview/commit checks re-read the evidence; detected drift revokes the stored approval. Restoring original files does not restore that approval. Saved minima are enforced without treating preferred track/via sizes as hard limits. Real KiCad DRC evaluates custom rules.

Execution prepares all new copper before one native transaction. Exact receipts and saved/reloaded native data establish new UUIDs, geometry, layers and networks. Complete preexisting objects and footprints must remain unchanged. Original pad clusters may merge within a requested net but cannot split or merge across nets. Both anchors and every created route object must share the intended native connected component. Project, custom-rule and schematic files remain protected. Live execution also compares baseline/final DRC; unexpected post-commit verification failures remain uncertain and retain normal recovery evidence.

## Reproduction

```sh
cargo build -p auto-kicad --bin auto-kicad --example check-copper-route-mcp
target/debug/examples/check-copper-route-mcp /absolute/auto-kicad /absolute/new-output-directory
```

The KiCad 10.0.7 fixture contains 4 copper layers, front/back SMD pads, existing straight tracks and through vias. Production stdio MCP creates 5 routes with 15 tracks and 4 full-span vias, followed by one pad-bound B.Cu single track. Coverage includes all three anchor kinds, F.Cu/In1.Cu/B.Cu legs, front SMD → inner layer → front SMD through two vias, and VCC routing at an explicit width different from its Power+Default class preference. Each successful plan passes disposable preview, exact approval, native commit, save and reload. Source reload uses the same editor process. Complete original footprint preservation is independently checked by the example; production execution also checks original board items and pad connectivity.

Invalid paths, missing transition vias, absent/non-copper layers, wrong pad layers, wrong nets, stale observations and below-minimum dimensions reject. Three approved plans are invalidated by custom-rule presence, saved-minimum changes or saved-netclass changes; restoring the original bytes cannot replay those approvals. A custom GND width constraint rejects preview with concrete `track_width` DRC findings and no source change. Invalid approval hashes and repeated committed requests reject. The fixture root-sheet index is initialized explicitly so KiCad does not add project metadata during preview; complete project-byte checks remain active.

The output retains every MCP request/response, native before/after evidence, DRC reports, durable execution records, result PCB/project and editor cleanup evidence. See [compact evidence](copper-route-mcp.json). Synthetic acceptance is independent of the populated real-project and three-project delivery gates. Native partial-creation cancellation is covered separately by [the native path probe](native-copper-routes.md).

The complete offline quality gate passed 400 tests with 15 environment-dependent tests ignored, format, Clippy warnings-as-errors and the capability matrix. Seven real KiCad regressions passed: legacy track/via creation, single-footprint placement, connected assembly, footprint move/rotate, field editing, flipping and deletion. The assembly regression initially timed out during editor startup before mutation; cleanup succeeded and a single retry passed. Both attempt logs are retained. A separate fresh stdio process refused a prior route approval with `unknown plan_id`, advertised the route schema/capability and retained complete rule/native evidence in durable execution records.

The full 256-copper plus 100-graphic batch, including Edge.Cuts, is verified separately by [maximum-batch preservation acceptance](copper-route-boundary.md).

## Boundaries

Zones and unsupported native copper reject because complete preservation cannot yet be established for them. Curved route creation, blind/buried/microvias, differential pairs, length tuning and automatic routing are unsupported. Populated-board routing and live stackup-drift acceptance are recorded in [the populated-copy evidence](populated-copper-route.md). General outlines, zone refill, schematic editing/synchronization and manufacturing delivery remain outside this capability.
