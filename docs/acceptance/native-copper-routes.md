# Native copper paths

`CopperRouteIntent` describes bounded explicit polylines, selected copper layers and through-via transitions. Anchors resolve physical pads, existing straight-track endpoints or existing through vias against one board revision. Compilation rejects stale observations, wrong nets, unavailable layers, mismatched endpoints, incomplete transitions and identity collisions across additive operations. Route limits are 16 legs, 128 nonzero segments and 15 vias; a batch permits at most 256 new copper objects. No intermediate pad identities are fabricated.

`StraightTrackIntent.layer` defaults to F.Cu when reading legacy serialized plans. Native through-via requests carry the complete enabled copper set from the observed stackup. Creation receipts and saved readback compare exact layers, dimensions, networks and identities. The route compiler prepares all native copper before the existing atomic transaction begins.

`connected_copper_ids` reads bounded native same-net components containing pads, tracks, arcs and vias. Every created route object and both anchors must share that component. Foreign-net shorts require the native DRC check because KiCad's component query follows assigned net codes.

## Reproduction and evidence

```sh
cargo build -p auto-kicad --example check-native-route
target/debug/examples/check-native-route /absolute/new-output-directory
```

The probe uses KiCad 10.0.7 on one owned 4-layer board copy. Three explicit paths create 9 tracks on F.Cu, In1.Cu and B.Cu plus 2 full-span vias. All three anchor kinds participate in the connected result. A transport fault before the second native creation proves cancellation of the first pending creation, with unchanged original objects and saved files. The successful transaction saves and reloads in the same process, preserves complete original board-item and footprint payloads, checks settled project bytes and confirms every route member in the native component. DRC introduces zero violations; unconnected items decrease from 3 to 0. The output retains the result PCB/project, plan, independent expected snapshot, native evidence and DRC reports. Owned editor and temporary working copy cleanup passed. See [compact evidence](native-copper-routes.json).

The initial native runs exposed two two-layer assumptions in via receipt/readback validation. Requests bind the enabled layer set explicitly. Reload comparison accepts only the native conversion of an inner circle's zero-size representative to its actual diameter and the unused custom-anchor field of circular pads; other observed fields remain compared. Existing objects use complete persistent payload comparisons without these creation-specific exceptions.

The offline workspace suite passed 395 tests with 15 environment-dependent tests ignored, plus format, Clippy warnings-as-errors and capability-matrix checks. Architecture tests cover bounded route structure, current anchor/net/layer binding, shared identities, complete via spans, native component evidence and reload preservation.

The actual KiCad 10.0.7 production MCP mixed-routing regression passed in 13.44 seconds, including existing single-track/via planning, preview, approval, commit, reload and editor cleanup. This regression does not exercise a public route request.

## Scope

This evidence covers native route compilation and execution. Public route requests, rule-evidence reconciliation, approval binding and the complete MCP route lifecycle are not implemented by this acceptance. The probe explicitly uses unbound evidence digests and does not grant approval. Its synthetic fixture does not count toward the three-real-project gate. General board outlines, zones/refill, general schematic editing, synchronization and manufacturing delivery remain outside this record.
