# Existing PCB item fields

`edit_existing` in `kicad_plan_change` modifies 1–100 distinct existing objects through inspect, plan, isolated preview and exact approved commit. It supports straight-track endpoints/width, uniform circular through-via position/copper diameter/primary drill diameter, and Dwgs.User segment endpoints/width. KiCad 10.0.6 is the verified backend. [Footprint movement and rotation](existing-footprint-transform.md) use the separate `move_footprint` request. Footprint flipping and item deletion are unavailable through MCP.

## Request

Read the current revision and native object identities from `kicad_inspect_project` → `data.pcb`. Track and via targets use their observed `track_id` or `via_id` plus native `source_uuid`. Drawing segments use `graphic_id` for both fields. Coordinates are integral board-space nanometres. Omit fields that should remain unchanged; every target must have an effective change. Each form is exclusive with other top-level operation fields.

```json
{
  "endpoint": "ipc:///tmp/kicad/api.sock",
  "source_run_id": "pcb-edit",
  "request_id": "widen-track",
  "edit_existing": {
    "observed_revision": "sha256:<current revision>",
    "changes": [
      {
        "kind": "track",
        "target_id": "<observed track_id>",
        "source_uuid": "<native UUID>",
        "width_nm": 300000
      }
    ]
  }
}
```

| Kind | Optional fields |
|---|---|
| `track` | `start_nm: {x,y}`, `end_nm: {x,y}`, `width_nm` |
| `via` | `position_nm: {x,y}`, `diameter_nm`, `drill_nm` |
| `graphic_segment` | `start_nm: {x,y}`, `end_nm: {x,y}`, `width_nm` |

Positions are bounded to ±2,000,000,000 nm. Dimensions must be positive and no greater than 2,000,000,000 nm. Segment endpoints must differ; via drill must be smaller than copper diameter. Native locks, unknown lock states, arc editing, blind/buried/micro vias, nonuniform or offset via copper, noncircular drills, non-Dwgs.User drawing edits and unsupported copper/zones reject. Reference/value, nets, layer spans, pad definitions and library assignments cannot be modified by these fields.

## Preservation and failure handling

The server binds the plan to normalized revision, complete native routing/shape and footprint payloads, and canonical native pad connectivity clusters. Callers cannot supply the native evidence digest. Both preview and source execution derive independent normalized and native expectations from the original data. Native updates retain every unrelated field, including unknown protobuf fields. Full saved/reloaded comparisons exclude only transient board-container IDs. Project, custom-rule and same-named schematic bytes are protected.

Every previously connected pair of surviving pads must remain connected, including when a global unconnected count would stay the same. Native pad-cluster comparison enforces this condition. Independent CLI DRC must introduce no violations, and unconnected count must not increase. Existing violations may remain. External writers must remain stopped; the server's writer lock coordinates its own writers only.

Stale revisions, wrong identities, duplicates, no-ops, unavailable evidence and unsupported edits reject before mutation. A confirmed transaction discard rejects the entire update. Uncertain begin/commit/save, failed discard, postcondition failures or uncertain cleanup report `unknown`; durable backups and evidence remain available. Failed previews never issue approval. Wrong approvals, replay and stale source evidence cannot authorize a source write. No automatic source restore or retry occurs.

## Acceptance and reproduction

The [result record](existing-item-edit-result.json) records actual KiCad 10.0.6 + production stdio MCP acceptance on 2026-10-26. One source editor was reused; serialized disposable previews used separate owned editors and cleaned them. A batch changed a connected front track's width and endpoint, a connected through via's position/copper/drill sizes, and a Dwgs.User segment endpoint/width. Complete native and independent normalized state, same-process reload, protected files, source isolation, exact approval, replay rejection and stale/locked/invalid-target rejection passed. Track endpoint and via movements that disconnected pads produced failed previews with no approval or source change.

The baseline contained 4 DRC violations and 1 unconnected item; the final report introduced no violations and retained that unconnected count. This synthetic fixture is not a manufacturing-clean board or a real-project delivery profile. Offline contracts separately test equal-count rewiring, preservation of unrelated/unknown native fields, malformed receipts and partial-update/transaction failure classification; native transport crashes were not injected. The full quality gate passed 361 tests, with 13 environment-dependent tests ignored. The live stdio acceptance passed separately in 36.98 seconds.

```sh
env CARGO_TARGET_DIR=/tmp/auto-kicad-edit-target CARGO_INCREMENTAL=0 CARGO_PROFILE_DEV_DEBUG=0 CARGO_PROFILE_TEST_DEBUG=0 cargo build -p auto-kicad --bin auto-kicad
env CARGO_TARGET_DIR=/tmp/auto-kicad-edit-target CARGO_INCREMENTAL=0 CARGO_PROFILE_DEV_DEBUG=0 CARGO_PROFILE_TEST_DEBUG=0 AUTO_KICAD_EDIT_SERVER=/tmp/auto-kicad-edit-target/debug/auto-kicad AUTO_KICAD_EDIT_EVIDENCE=/tmp/auto-kicad-edit-result.json cargo test -p auto-kicad --lib tests::existing_item_edit_acceptance::live_existing_item_stdio_lifecycle -- --ignored --exact --nocapture
```

Run `python3 scripts/quality.py` for matrix validation, formatting, warnings-denied Clippy and the offline workspace suite. Remove the temporary target after all build/test processes exit. The installed user MCP binary is not changed by this acceptance command.
