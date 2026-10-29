# Bounded schematic editing over MCP

`schematic.bounded_edit` exposes saved-file observation, typed plans, exact native expectations and approved publication through the existing schematic tools. KiCad 10.0.7 is the qualified CLI backend. Close project editors and stop external writers before using this workflow. PCB synchronization is separate.

## Native resources and limits

The server selects the native configuration through `KICAD_CONFIG_HOME` or the platform KiCad configuration directory. Its `10.0` directory must contain `kicad_common.json`, `sym-lib-table` and `fp-lib-table`. Installed symbol/footprint roots are detected on macOS and Linux. Operators may select absolute roots with `AUTO_KICAD_OFFICIAL_SYMBOL_ROOT` and `AUTO_KICAD_OFFICIAL_FOOTPRINT_ROOT`; additional permitted roots come from the existing library configuration. MCP requests cannot provide resource roots, executable paths, library definitions, catalog digests or footprint compatibility evidence. The adapter resolves and verifies these inputs itself.

Runtime availability requires durable execution state and native resources. Observation performs the complete project/context/hierarchy/netlist/ERC/pin audit and may reject unsupported or ambiguous sources. Each server permits 4 live observations and 4 live plans, expiring after 900 seconds. An in-flight operation retains its slot until completion. Terminal metadata is bounded to 32 entries. Release unused handles explicitly; successful or rejected commit attempts discard obsolete observations of that project. Native work uses blocking workers without holding the registry mutex across CLI execution.

## Workflow

Observe canonical absolute paths:

```json
{
  "observe": {
    "project_root": "/absolute/project",
    "root_schematic": "/absolute/project/design.kicad_sch"
  }
}
```

Send this to `kicad_schematic_audit`. The result contains `observation_id`, `revision`, units with source UUIDs and sheet paths, physical pin contacts in nanometres, editable object inventory, full source-pin connectivity, ERC and source/context evidence. Retain the returned identities when constructing a plan:

```json
{
  "project_root": "/absolute/project",
  "root_schematic": "/absolute/project/design.kicad_sch",
  "source_run_id": "resistor-value-edit",
  "request_id": "set-r1-value",
  "general": {
    "schema_version": 1,
    "observation_id": "RETURNED_OBSERVATION_ID",
    "observed_revision": "RETURNED_REVISION",
    "edits": [{
      "file": "design.kicad_sch",
      "scope": {"kind": "file_all_instances"},
      "action": {"kind": "set_property", "uuid": "OBSERVED_SYMBOL_UUID", "name": "Value", "value": "10k"}
    }]
  }
}
```

Send this to `kicad_plan_schematic_change`. A plan supports 1–128 edits and returns server-minted `plan_id`, `plan_hash`, exact changed source identities/instance paths, source/candidate artifacts and complete native preview evidence. Planning and preview do not change CAD files. Omitting `general` retains the fixed two-resistor request behavior.

| Intent | Supported actions |
| --- | --- |
| Existing symbols | `set_property`, `set_reference`, `set_flags`, `transform_symbol`, `assign_footprint`, `delete` |
| Catalog symbols | `create_symbol` with server-derived definitions/UUIDs and [bounded native inheritance](schematic-symbol-inheritance.md) |
| Wires | `create_wire`, `update_wire`, `delete` |
| Markers | `create_junction`, `create_no_connect`, `move_marker`, `delete` |
| Local/global/hierarchical labels | `create_label`, `update_label`, `delete` |

Coordinates are exact native-grid nanometres; rotations are `zero`, `ccw90`, `half`, `ccw270`, with `none`, `x` or `y` reflection axes for symbols. File-wide edits must explicitly use `file_all_instances`. Reference annotation can select `single_instance` with its observed sheet `path`. Shared physical source changes affect every instance and report those paths. Footprint assignment derives the actual distinct numbered library pads and requires a complete bijection with observed symbol pin numbers; duplicated physical pads with the same number are supported. An omitted mapping requests equal pin/pad numbers.

Call `kicad_preview_schematic_change` with `plan_id` and `plan_hash`. Its optional `expectations` contains typed `connectivity`, `removed_markers`, `added_markers` and `erc` deltas. Omission means every one of these remains unchanged. Electrical changes require exact declared removed/added networks and pin births/deaths, physical no-connect changes require explicit marker identities/positions/digests, and ERC changes require complete diagnostic identities including exact `diagnostic_json`. Review the [expectation contract](schematic-expectations.md) and returned native evidence. A mismatch produces no approval; it permits correcting expectations against the same intact plan. Re-preview revokes the previous approval. Legacy plans reject the new expectations field.

After reviewing and authorizing the preview, call `kicad_commit_schematic_change` with the exact `plan_id`, `plan_hash` and returned `approval_hash`. These identify server-owned matching evidence; client-supplied candidate proofs cannot substitute for it. The CLI validates actual published sources, complete connectivity/ERC, hierarchy and protected inputs before `committed`. The durable execution record retains rehashable preview, matching, verification and candidate sidecars. Observed source/configuration/library drift invalidates approval permanently, even if the bytes are restored. Plans and approvals expire on process restart; completed or uncertain attempts cannot replay.

Release handles with `kicad_schematic_audit` using `{"release":{"observation_ids":["..."],"plan_ids":["..."]}}`. It refuses releasing an in-flight plan. Observation/release requests are mutually exclusive with legacy ledger filters. General sessions appear separately in `general_sessions`; the existing ledger response remains compatible.

## Interrupted publication

Publication journals candidate/original fingerprints and each file operation before mutation. An interrupted or uncertain operation retains a project-local pending marker and blocks further project writes, including from another state directory. Start a fresh guarded recovery plan, preview and approval through the recovery MCP tools. Files with later unrelated user bytes are preserved and rejected. Recovery restores exact original bytes/new-file absence, removes matching staged files and verifies the resulting file set. Multi-file publication and recovery use per-file operations rather than a filesystem-wide transaction. See [publication recovery](schematic-publication-recovery.md).

## Actual acceptance and remaining scope

`scripts/check-schematic-general.py` drives real initialize/discovery/tools calls against the production stdio executable and actual KiCad CLI on explicitly owned project/configuration copies. ECC83 acceptance covers properties with Unicode/quoted/control text, flags and restoration, reference rename, footprint assignment, catalog resistor creation/transform/delete, local/global/hierarchical label create/update/delete, wire create/update/delete, junction/no-connect create/move/delete and an independently declared GND merge. It verifies native results, exact changed source files, protected configuration, wrong approvals, replay, process-restart expiry, drift rejection and rehashed retained sidecars. Multichannel acceptance covers two physical source files affecting five instances; `--controls` adds shared-child single-instance annotation, request authority rejection, observation limits/release and rejection of undeclared pin/network/marker/ERC changes.

`scripts/check-schematic-general-interruption.py` kills its own real stdio writer after publication and before terminal evidence. A fresh process rejects replay and new writes, preserves later user bytes, performs fresh approved production MCP recovery, compares exact original files/native observation, and accepts a subsequent normal commit. Owned native children stop before orphaned private snapshots are removed. These checks launch no GUI and do not establish editor undo, GUI cancellation or Codex model qualification.

Full `schematic.general_edit` remains deferred. Sheet geometry/hierarchical pins, hierarchy/child-file creation, GUI/Codex qualification, schematic-to-PCB synchronization and the final three-project product gate remain incomplete. Inherited symbol creation requires a bounded same-file chain; inherited embedded-file/font resources reject. Source examples and user projects are not mutated by these acceptance scripts; only marked copies are accepted.
