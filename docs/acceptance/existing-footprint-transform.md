# Existing footprint movement and rotation

`move_footprint` moves or rotates one unlocked existing footprint through the PCB inspect, plan, preview and commit tools. KiCad 10.0.7 is the qualified native-text backend. The operation preserves the footprint side, native identities, reference/value, schematic association, pad definitions and networks. Connected tracks/vias, zones, unsupported copper, unsupported target geometry and selected native unit metadata reject the operation. Flip, deletion and copper rerouting are separate unsupported operations.

## Request and approval

Read `data.pcb.snapshot` and `data.pcb.revision` from `kicad_inspect_project`. Send the current placement identity, native UUID and revision with the requested absolute transform:

```json
{
  "endpoint": "ipc:///tmp/kicad/api.sock",
  "move_footprint": {
    "placement_id": "<observed placement identity>",
    "source_uuid": "<observed native UUID>",
    "observed_revision": "sha256:<observed revision>",
    "x_nm": 6000000,
    "y_nm": 5000000,
    "orientation_mdeg": 90000,
    "copper_tolerance_nm": 250000
  }
}
```

Supply both coordinates together or omit both. Position and orientation are independently optional, with at least one transform required. Orientation is normalized into `[0, 360000)`. The copper tolerance defaults to 250000 nm and accepts integers from 1 through 250000. Physical contact rejects regardless of net assignment. Conservative pad envelopes include corners and offsets; arc checks use the curved span; through vias include inner copper. This can reject nearby disconnected copper conservatively.

The plan binds complete native footprint payload digests as well as the normalized revision. The server derives these digests; callers cannot override them. Preview uses an isolated project and returns an approval only after save/reload, independent expected-state comparison, DRC/connectivity checks and cleanup. Commit requires the exact plan, validation evidence and approval hashes. Wrong approvals, stale observations and consumed approvals cannot write. Source execution repeats the native payload and normalized checks before mutation, then verifies full native and normalized data before and after reload. Project, custom rules and same-named schematic bytes are protected.

## Native preservation and limits

The native payload applies a rigid transform to parent and supported child coordinates/angles. Pads, fields, text and supported non-copper shape children retain their remaining protobuf fields. 3D model data remains in local coordinates. Unknown children, text boxes, copper fields/graphics and non-quarter-turn rotation of rectangle primitives reject before `UpdateItems`. Board graphics outside the existing segment observer can prevent observation. Boards containing zones or copper graphics/text reject this operation. This is a bounded editing capability, not general KiCad object coverage.

Full saved-template comparison excludes only the transient parent board-container KIID, which changes on reload. Footprint and child KIIDs, document path, instance token and all other template data remain checked. The expected result comes from the pre-edit state and approved intent, independently of the observed result. DRC must introduce no violation, and unconnected-item count must not increase. Existing violations may remain. GUI/external writers must not edit concurrently; auto-kicad's writer lock coordinates its own writers only.

Pre-mutation failures reject. A confirmed dropped transaction rejects without claiming success. Mutation, commit, save, readback or cleanup ambiguity reports `unknown`, retains durable evidence and requires inspection or guarded recovery. A failed shadow cannot authorize a source write, including when DRC fails after a shadow mutation. Automatic source restoration and automatic retry are disabled.

## Reproduce

The ignored live test requires installed KiCad PCB Editor and `kicad-cli`. It launches one source editor, reuses that process across all edits and reloads, and closes it afterward. Each serialized preview creates and cleans its own isolated editor. Original fixture bytes and protected source files are checked. Use one temporary target and remove it after the commands finish:

```sh
env CARGO_TARGET_DIR=/tmp/auto-kicad-move-target CARGO_INCREMENTAL=0 CARGO_PROFILE_DEV_DEBUG=0 CARGO_PROFILE_TEST_DEBUG=0 cargo build -p auto-kicad --bin auto-kicad
env CARGO_TARGET_DIR=/tmp/auto-kicad-move-target CARGO_INCREMENTAL=0 CARGO_PROFILE_DEV_DEBUG=0 CARGO_PROFILE_TEST_DEBUG=0 AUTO_KICAD_MOVE_SERVER=/tmp/auto-kicad-move-target/debug/auto-kicad AUTO_KICAD_MOVE_EVIDENCE=/tmp/auto-kicad-move-result.json cargo test -p auto-kicad --lib tests::existing_footprint_acceptance::live_existing_footprint_stdio_lifecycle -- --ignored --exact --nocapture
```

The [result record](existing-footprint-transform-result.json) summarizes the 2026-10-26 successful run through the actual stdio executable and native IPC. 4 footprints, 5 pads, a track and a through via exercise front translation, front rotation and back translation/rotation. Locked and connected targets reject both movement and rotation. Stale revisions, wrong approvals and repeated commits reject. A foreign-net collision produces a failed DRC preview with no approval and unchanged source. Full native payloads, normalized snapshots, saved reload and protected files pass. Six baseline DRC violations and three unconnected items remain, with zero introduced violations; the fixture is not a manufacturing-clean board. Owned editors and files were cleaned. This synthetic fixture does not count as a real-project delivery profile.

Offline workspace contracts independently cover circular arc contact, offset/corner envelopes, foreign-net contact, through-via layer spans, native item statuses and child preservation, update/commit/save/readback failures, failed cleanup and denial of approval for uncertain shadows. Transport/process failures are injected at the adapter boundary; the live run does not deliberately crash KiCad. Run `python3 scripts/quality.py` for matrix validation, format, warnings-denied Clippy and the offline workspace suite. Other ignored GUI tests are not implied to have run by this acceptance.


## Native unit metadata support gate

[Native unit metadata protection](footprint-unit-metadata.md) verifies a complete live-text support gate at planning and before execution begins a transaction. A selected direct `units` form rejects because it is absent from the full-item IPC schema. An unrelated footprint may retain units while supported ordinary movement proceeds. Actual 10.0.7 stdio rejection, ordinary neighbor commits and the extended complete front/back lifecycle pass; exact native forms, snapshots, DRC and source protection are checked. The older 10.0.6 record above remains historical evidence, not a qualification of omitted unit metadata.
