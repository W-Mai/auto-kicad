# Exact existing-object deletion

`delete_items` in `kicad_plan_change` removes 1–100 explicit supported top-level PCB objects. Each target binds its kind, current observed identity and native UUID. The operation requires the current observed revision, an isolated successful preview and exact approval. Deletion is classified as high risk in the engine.

```json
{
  "endpoint": "ipc:///tmp/kicad/api.sock",
  "source_run_id": "pcb-edit",
  "request_id": "remove-unused-route",
  "delete_items": {
    "observed_revision": "sha256:<current revision>",
    "targets": [
      {
        "kind": "track",
        "target_id": "<observed track_id>",
        "source_uuid": "<native UUID>"
      }
    ]
  }
}
```

Supported kinds are `track` (straight or arc), `via` (through), `graphic_segment` (Dwgs.User), and `footprint` (free and without schematic association). Use `track_id`, `via_id`, `graphic_id`, or `placement_id` from `kicad_inspect_project` → `data.pcb.snapshot` as the target identity. For a drawing, `graphic_id` also supplies its native UUID. A delete request cannot be combined with other operation forms.

Duplicates, missing/foreign identities, wrong kinds, locked or unknown-lock objects, unsupported via types, Edge.Cuts deletion, isolated pads and schematic-associated footprint removal reject. Footprint deletion checks existing copper contact against the original board, so a batch cannot bypass protection by deleting its connecting track first. Zones and unsupported copper prevent this operation. Fully observed arc tracks can be removed; this request does not change arc geometry.

## Native verification

The server derives exact normalized and complete native removal sets before writing. Survivors retain all unrelated fields, unknown protobuf data, IDs, nets and associations. Removing a footprint includes its owned children; individual child targets are not accepted. Complete saved/reloaded object sets and protected project/rule/schematic bytes must match the approved intent. All previously connected pairs of surviving pads must remain connected. Independent CLI DRC must introduce no violations or increased unconnected count.

Exact successful per-item deletion receipts are checked when present. KiCad 10.0.6 returns an empty per-item result list; this exact-version compatibility case remains `NeedsSavedVerification` until complete saved-state checks pass. It never fabricates individual success statuses. Unknown versions with incomplete results reject. A confirmed native discard cancels a rejected batch; uncertain commit/save, failed discard, mismatched saved state or uncertain cleanup reports `unknown` with durable recovery evidence. Source restoration is not automatic.

## Acceptance

The [result record](item-deletion-result.json) records production stdio MCP and KiCad 10.0.6 acceptance. One source editor was reused with serialized owned previews; all owned editors and temporary fixtures were cleaned. A mixed batch removed a dangling straight track, dangling arc, disconnected through via, Dwgs.User segment and free footprint. The exact remaining native content, independent normalized snapshot, protected files and same-process saved reload matched.

Deleting a required track or via failed isolated preview because surviving pad connectivity broke; no approval or source mutation occurred. Locked tracks/drawings/footprints, associated or connected footprints, Edge.Cuts, duplicate/empty/mismatched targets and the batch connection-policy bypass rejected. Incorrect approval, repeated commit and replay after restarting the MCP server also rejected. The final DRC introduced no violations, and unconnected items decreased from 7 to 3. The baseline had 10 violations; this synthetic fixture is not a real-project delivery profile or manufacturing-clean board.

Offline contracts cover immutable survivor payloads, partial invalid batches, high-risk classification, original-board connection policy and malformed deletion receipts. Transaction and transport failure classifications use existing injected contracts; live editor crashes were not injected. The complete quality gate passed 367 tests, with 15 environment-dependent tests ignored. The live stdio test passed separately in 34.08 seconds.

```sh
env CARGO_TARGET_DIR=/tmp/auto-kicad-delete-target CARGO_INCREMENTAL=0 CARGO_PROFILE_DEV_DEBUG=0 CARGO_PROFILE_TEST_DEBUG=0 cargo build -p auto-kicad --bin auto-kicad
env CARGO_TARGET_DIR=/tmp/auto-kicad-delete-target CARGO_INCREMENTAL=0 CARGO_PROFILE_DEV_DEBUG=0 CARGO_PROFILE_TEST_DEBUG=0 AUTO_KICAD_DELETE_SERVER=/tmp/auto-kicad-delete-target/debug/auto-kicad AUTO_KICAD_DELETE_EVIDENCE=/tmp/auto-kicad-delete-result.json cargo test -p auto-kicad --lib tests::item_deletion_acceptance::live_item_deletion_stdio_lifecycle -- --ignored --exact --nocapture
```

Run `python3 scripts/quality.py` for the complete offline gate. Remove the temporary build target after all build/test processes exit. These commands do not replace the installed user MCP executable.
