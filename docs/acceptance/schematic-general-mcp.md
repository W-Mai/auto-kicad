# Schematic editing over MCP

`kicad_schematic_audit` reads a saved schematic. The schematic plan, preview and commit tools apply selected edits after approval. The `schematic.bounded_edit` capability uses KiCad 10.0.7 to check native netlist and ERC results. Close project editors and stop external writers before editing. Schematic-to-PCB synchronization uses a separate request.

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

Send this to `kicad_schematic_audit`. The result contains `observation_id`, `revision`, units with source UUIDs and sheet paths, sheet inventory with configured page labels and separate native ordinals, physical pin contacts in nanometres, editable object inventory, full source-pin connectivity, ERC and source/context evidence. Retain the returned identities when constructing a plan:

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
| Existing component units | `add_symbol_unit` from observed source metadata and exact instance annotations |
| Existing sheets and hierarchical pins | `update_sheet` with explicit name, rectangle and nested pin create/update/delete; `remove_sheet` retaining child files |
| Child placements and configured pages | `create_sheet`, `retarget_sheet`, `set_sheet_page` with exact page/reference maps and inspected child revisions |
| Wires | `create_wire`, `update_wire`, `delete` |
| Markers | `create_junction`, `create_no_connect`, `move_marker`, `delete` |
| Local/global/hierarchical labels | `create_label`, `update_label`, `delete` |

Coordinates are exact native-grid nanometres; rotations are `zero`, `ccw90`, `half`, `ccw270`, with `none`, `x` or `y` reflection axes for symbols. File-wide edits must explicitly use `file_all_instances`. Reference annotation can select `single_instance` with its observed sheet `path`. Shared physical source changes affect every instance and report those paths. Footprint assignment derives the actual distinct numbered library pads and requires a complete bijection with observed symbol pin numbers; duplicated physical pads with the same number are supported. An omitted mapping requests equal pin/pad numbers. Native assignment preserves literal numbers; an explicit map must also retain each number because a Footprint field cannot persist pin renumbering.

[Component unit creation and insertion](schematic-multiunit.md) permit compatible new units to share a new component reference. Reused-sheet creation requires a complete `instance_references` mapping; insertion inherits existing annotations. Native common-pin nets must be unambiguous, and unit insertion requires explicit old-to-expanded logical-pin membership deltas.

[Complete component reference/footprint edits](schematic-component-edits.md) require consistent final shared fields across all units, retain immutable membership and use canonical active component pins for compatibility. Per-instance annotation preserves foreign/inactive rows and other instances; explicit reference swaps retain distinct component ownership.

[Existing sheet and hierarchical pin edits](schematic-sheets.md) validate complete final edge contacts and require explicit pin movement. Sheet names use complete explicit removed/added ERC header declarations; metadata changes remain rejected. Outer and nested operations share the 128-operation bound.

Call `kicad_preview_schematic_change` with `plan_id` and `plan_hash`. Its optional `expectations` contains typed `connectivity`, `removed_markers`, `added_markers` and `erc` deltas. Omission means every one of these remains unchanged. Electrical changes require exact declared removed/added networks and pin births/deaths, physical no-connect changes require explicit marker identities/positions/digests, and ERC changes require complete diagnostic identities including exact `diagnostic_json`. Review the [expectation contract](schematic-expectations.md) and returned native evidence. A mismatch produces no approval; it permits correcting expectations against the same intact plan. Re-preview revokes the previous approval. Legacy plans reject the new expectations field.

After reviewing and authorizing the preview, call `kicad_commit_schematic_change` with the exact `plan_id`, `plan_hash` and returned `approval_hash`. These identify server-owned matching evidence; client-supplied candidate proofs cannot substitute for it. The CLI validates actual published sources, complete connectivity/ERC, hierarchy and protected inputs before `committed`. The durable execution record retains rehashable preview, matching, verification and candidate sidecars. Observed source/configuration/library drift invalidates approval permanently, even if the bytes are restored. Plans and approvals expire on process restart; completed or uncertain attempts cannot replay.

Release handles with `kicad_schematic_audit` using `{"release":{"observation_ids":["..."],"plan_ids":["..."]}}`. It refuses releasing an in-flight plan. Observation, read-only `inspect_child` and release requests are mutually exclusive with each other and legacy ledger filters. [Child inspection and authoring](schematic-children.md) bind detached source inventories to the current observation without granting publication authority. General sessions appear separately in `general_sessions`; the existing ledger response remains compatible.

## Interrupted publication

Publication journals candidate/original fingerprints and each file operation before mutation. An interrupted or uncertain operation retains a project-local pending marker and blocks further project writes, including from another state directory. Start a fresh guarded recovery plan, preview and approval through the recovery MCP tools. Files with later unrelated user bytes are preserved and rejected. Recovery restores exact original bytes/new-file absence, removes matching staged files and verifies the resulting file set. Multi-file publication and recovery use per-file operations rather than a filesystem-wide transaction. See [publication recovery](schematic-publication-recovery.md).

## Actual acceptance and remaining scope

`scripts/check-schematic-general.py` drives real initialize/discovery/tools calls against the production stdio executable and actual KiCad CLI on explicitly owned project/configuration copies. ECC83 acceptance covers properties with Unicode/quoted/control text, flags and restoration, reference rename, footprint assignment, catalog resistor creation/transform/delete, local/global/hierarchical label create/update/delete, wire create/update/delete, junction/no-connect create/move/delete and an independently declared GND merge. It verifies native results, exact changed source files, protected configuration, wrong approvals, replay, process-restart expiry, drift rejection and rehashed retained sidecars. Multichannel acceptance covers two physical source files affecting five instances; `--controls` adds shared-child single-instance annotation, request authority rejection, observation limits/release and rejection of undeclared pin/network/marker/ERC changes.

`scripts/check-schematic-general-interruption.py` kills its own real stdio writer after publication and before terminal evidence. A fresh process rejects replay and new writes, preserves later user bytes, performs fresh approved production MCP recovery, compares exact original files/native observation, and accepts a subsequent normal commit. Owned native children stop before orphaned private snapshots are removed. These checks launch no GUI and do not establish editor undo, GUI cancellation or Codex model qualification.

Full `schematic.general_edit` remains deferred. Complete GUI/Codex/family qualification, schematic-to-PCB synchronization and the final three-project product gate remain incomplete. Inherited symbol creation requires a bounded same-file chain; inherited embedded-file/font resources reject. Source examples and user projects are not mutated by these acceptance scripts; only marked copies are accepted.

[Existing sheet qualification](schematic-sheets.md) passed both formats and a parent reused in 4 instances through production stdio, independent saved native network/ERC comparison, exact rectangle/field checks and actual interruption/recovery. [Exact placement removal](schematic-sheet-removal.md) passed both formats, reused parents and actual interruption/recovery while retaining shared/orphan child files. [Child creation/attachment/retargeting](schematic-children.md) bind complete page/reference maps, retained inactive sources and exact new-file authority. Full GUI/Codex/family qualification remains incomplete.

[Actual Codex client qualification](schematic-codex-client.md) passes deterministic native intent workflows and guarded recovery through real ephemeral app-server MCP connections. Explicit server environment selection is required; client termination snapshot cleanup is recorded as an acceptance-owner intervention. GUI load/save and nested displayed-page qualification remain incomplete after a verified assistive-access denial.
