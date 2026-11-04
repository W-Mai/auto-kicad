# Closed schematic-to-PCB synchronization

The existing MCP tools synchronize a saved schematic hierarchy to its same-basename saved PCB. KiCad10.0.7 validates private candidates and the actual published board. Close the target schematic and PCB editors before observation, planning, publication and recovery. Publication writes only the PCB; schematic authoring is a separate approved operation. Project settings, rules, libraries and configuration remain unchanged.

## Observe and select changes

Call `kicad_inspect_project`:

```json
{
  "schematic_pcb_sync": {
    "kind": "observe",
    "project_root": "/absolute/project",
    "root_schematic": "/absolute/project/design.kicad_sch"
  }
}
```

The response contains `observation_id`, `revision`, complete eligible components and ineligible source units, footprint/pad records, copper UUID/net/lock records, zone presence and source context. Match components through full source UUID paths and unit aliases. A displayed reference cannot supply missing linkage. A unique complete saved instance path remains authoritative when its descriptive project label is empty or differs from the filename; repeated active paths reject.

Call `kicad_plan_change` with an exclusive synchronization request:

```json
{
  "source_run_id": "antenna-review",
  "request_id": "connector-update",
  "schematic_pcb_sync": {
    "schema_version": 1,
    "observation_id": "OBSERVED_ID",
    "observed_revision": "OBSERVED_REVISION",
    "intent": {
      "schema_version": 1,
      "actions": [{"kind": "update", "component_id": "OBSERVED_COMPONENT_ID", "fields": [], "update_nets": false}],
      "copper_nets": []
    }
  }
}
```

`update` refreshes source reference/value/flags/unit/pin metadata on the existing authoritative link. `fields` selects extra source properties; it is not a list of built-in metadata fields. Pad-net changes require `update_nets: true`. `relink` additionally requires an observed `footprint_uuid`. `add` requires the exact assigned source footprint and explicit `placement: {position_nm:[x,y], orientation_mdeg:angle, side:"front"}` or `"back"`. `replace` names the existing footprint and uses the source assignment while retaining placement. `keep`, `detach` and `remove` name exact observed footprint UUIDs. These are typed actions with conflict checks, not arbitrary board edits.

Manual unmatched objects remain unchanged. Current-source excluded/unexported links require explicit disposition. A linked eligible component cannot be implicitly removed. Removal rejects locked, grouped, board-only, zoned or physically connected targets. Replacement requires complete physical pad correspondence and preserves existing connected contacts. Unsupported source/template families, new custom-field geometry and unverifiable native forms reject. Every eligible component still requires exact final pin/pad/net parity.

## Preview and commit

The plan contains all typed `differences` and a complete `drc_delta`. Call `kicad_preview_change` with its `plan_id`, `plan_hash` and `schematic_pcb_sync: {differences:[...], drc:{introduced:[...], resolved:[...]}}`. Declare every exact difference and complete diagnostic record. Missing, extra, duplicate or same-count-but-different expectations reject. Review the actual diagnostic changes, including unconnected items and schematic parity; a successful preview does not imply zero DRC violations.

Commit with `kicad_commit_change` using that preview's `approval_hash` and `validation_evidence_hash` plus the same plan identity. Approval is session-bound and cannot survive restart or replay. A verified no-change plan returns `state: "unchanged"`, creates no mutation approval and writes no execution journal. Mixed live-editor/synchronization operations and unknown request fields reject.

Complete native source/candidate/saved-board evidence binds all retained objects, UUIDs, source resources, physical copper partitions, effective netclasses and raw DRC diagnostic multisets. Full native re-observation of the actual published PCB must match the approved candidate. Manual geometry, dimensions, field placement, unknown protected data and copper geometry remain unchanged. Native dimension-owned text shares its owner's UUID only under the qualified typed ownership rule; ordinary global UUID collisions reject.

## Copper and scope

Changing a connected pad's net requires explicit net assignments for every affected observed copper object. `copper_nets` contains `{uuid,net}` entries. Track, arc, via and supported root copper graphics participate in complete physical partitions; a partition requiring incompatible source nets rejects. Unknown returned endpoints cannot be skipped. Supported root copper graphics are `gr_line`, `gr_arc`, `gr_circle`, `gr_rect` and `gr_poly` on canonical copper layers. Their geometry/layers remain protected; nested or other forms without matching source/native evidence reject.

This workflow preserves project settings and effective rules. Explicit class/project/custom-rule changes, affected zones/generated copper, zero-pin additions and unqualified source/template families are outside this accepted workflow. The composed connectivity reader limits inputs to32MiB and4096 endpoints; larger parser limits do not grant a larger supported workflow. Synchronization messages are bounded to4MiB, plans to128 actions and4096 explicit copper assignments. Sessions retain at most4 observations and4 live plans for900seconds, with32 terminal summaries. Release retained resources through `kicad_inspect_project.schematic_pcb_sync` using `kind: "release"`, `observation_ids` and `plan_ids`; `kind: "history"` returns bounded session history.

## Durable publication and recovery

PCB-only executions use domain `pcb_sync`, execution schema4 and `.auto-kicad-sync-pending`. The initial durable record binds exact backups, candidate, expected differences/DRC, native baseline and absent-before sibling staging path. The OS project writer lock and pending record prevent conflicting writes. An interrupted or ambiguous write remains uncertain and blocks ordinary writers.

Use `kicad_plan_recovery` with `project_dir`, `attempt_id` and `action: "restore_backups"`, then the existing recovery preview/commit tools with fresh approval. Recovery reconstructs source authority through the current server policy and validates retained evidence; it never accepts arbitrary resource roots from the journal. Later user bytes, altered candidates/configuration and arbitrary staging content reject restoration. Only an exact approved candidate prefix in a known absent-before staging file can be removed. Exact original bytes, complete fresh native serialization/connectivity/DRC and terminal journal records are required before the marker clears. Recovery does not run automatically.

## Qualification

```text
python3 scripts/check-schematic-pcb-sync.py SERVER OWNED_SOURCE OWNED_CONFIG NEW_OUTPUT
python3 scripts/check-schematic-pcb-sync.py SERVER OWNED_SOURCE OWNED_CONFIG NEW_OUTPUT --interruption
python3 scripts/check-codex-schematic.py synchronization SERVER OWNED_SOURCE OWNED_CONFIG NEW_OUTPUT
```

The driver requires ownership markers and a prepared source/PCB fixture with explicit `.auto-kicad-sync-copper.json` assignments. It tests complete expectations, source/configuration drift, wrong/restart/replayed approvals, actual publication, redundant copper declarations and no-change behavior. The interruption profile kills the actual stdio writer, checks later-user-byte protection, requires new approval, performs fresh native restoration and commits again. The Codex profile uses an actual app-server MCP client. Owner cleanup of interrupted private snapshots is recorded separately from automatic server cleanup.

Machine-readable qualification, real-project results and remaining scope are recorded in [schematic-pcb-sync.json](schematic-pcb-sync.json). General unrestricted synchronization remains deferred in the capability matrix; runtime availability reports this bounded interface separately.
