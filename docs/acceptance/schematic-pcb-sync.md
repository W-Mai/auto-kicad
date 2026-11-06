# Schematic-to-PCB synchronization

Synchronize a saved schematic hierarchy to its same-basename PCB through the MCP inspect, plan, preview and commit tools. KiCad 10.0.7 validates private candidates and the published board. Close schematic and PCB editors before observation, planning, publication and recovery.

Publication writes the PCB and selected project netclass settings. Schematic edits require their own approval. Rules, libraries, source schematics, configuration and unselected project tokens remain unchanged. A project-only change preserves the PCB bytes and modification time.

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

Manual unmatched objects remain unchanged. Current-source excluded/unexported links require explicit disposition. A linked eligible component cannot be implicitly removed. Default-profile removal rejects locked, grouped, board-only, zoned or physically connected targets. The explicit [region profile](schematic-pcb-sync-regions.md) adds native zone ownership and `remove_manual` policy without authorizing contact loss. Replacement requires complete physical pad correspondence and preserves existing connected contacts. Unsupported source/template families, new custom-field geometry and unverifiable native forms reject. Every eligible component still requires exact final pin/pad/net parity.

## Preview and commit

The plan contains all typed `differences` and a complete `drc_delta`. Large native proof detail uses a digest-bound `native_preview.complete_evidence` JSON artifact while retaining complete public expectations; smaller responses remain unchanged. Call `kicad_preview_change` with its `plan_id`, `plan_hash` and `schematic_pcb_sync: {differences:[...], drc:{introduced:[...], resolved:[...]}}`. Declare every exact difference and complete diagnostic record. Missing, extra, duplicate or same-count-but-different expectations reject. Review the actual diagnostic changes, including unconnected items and schematic parity; a successful preview does not imply zero DRC violations.

Commit with `kicad_commit_change` using that preview's `approval_hash` and `validation_evidence_hash` plus the same plan identity. Approval is session-bound and cannot survive restart or replay. A verified no-change plan returns `state: "unchanged"`, creates no mutation approval and writes no execution journal. Mixed live-editor/synchronization operations and unknown request fields reject.

Complete native source/candidate/saved-board evidence binds all retained objects, UUIDs, source resources, physical copper partitions, effective netclasses and raw DRC diagnostic multisets. Full native re-observation of the actual published PCB must match the approved candidate. Manual geometry, dimensions, field placement, unknown protected data and copper geometry remain unchanged. Native dimension-owned text shares its owner's UUID only under the qualified typed ownership rule; ordinary global UUID collisions reject.

[Connected same-geometry replacement](schematic-connected-replacement.md) passes actual public source assignment, PCB publication, killed-writer native recovery and a subsequent replacement commit. Complete child-identity mapping preserves full DRC meaning while retaining the exact declared raw diagnostic changes.

[Pinless symbol and footprint lifecycles](schematic-empty-components.md) use verified complete empty pin authority for source creation, compatible assignment, explicit PCB addition/replacement/removal and existing metadata/relink. Every native unit pin list and numbered-pad set must agree. Unnumbered physical pads remain complete; replacement cannot silently change physical pad cardinality.

## Copper and scope

Changing a connected pad's net requires explicit net assignments for every affected observed copper object. `copper_nets` contains `{uuid,net}` entries. Track, arc, via and supported root copper graphics participate in complete physical partitions; a partition requiring incompatible source nets rejects. Unknown returned endpoints cannot be skipped. Supported root copper graphics are `gr_line`, `gr_arc`, `gr_circle`, `gr_rect` and `gr_poly` on canonical copper layers. Their geometry/layers remain protected; nested or other forms without matching source/native evidence reject.

Requests without project actions preserve project settings and effective rules. Typed project netclass transitions require their separate complete expectations below. Other project fields, custom-rule interactions and unqualified source/template families remain unsupported. The explicit [region observation profile](schematic-pcb-sync-regions.md) qualifies authored/generated fill and zone-net changes. Default-profile project netclass changes reject when a zone is present; [custom-rule mutations](schematic-pcb-sync-rules.md) reject in every profile, including ordinary board-only edits; a byte-identical project no-op retains the ordinary PCB workflow and its existing checks. The [composed size and batch profile](schematic-pcb-sync-scale.md) qualifies128MiB PCB input with128 component actions and4096 selected copper items in one actual Codex request; larger internal endpoint caps do not establish unrestricted public admission. Synchronization messages are bounded to4MiB, plans to128 actions and4096 explicit copper assignments. Sessions retain at most4 observations and4 live plans for900seconds, with32 terminal summaries. Release retained resources through `kicad_inspect_project.schematic_pcb_sync` using `kind: "release"`, `observation_ids` and `plan_ids`; `kind: "history"` returns bounded session history.

## Typed project netclass changes

Set `schematic_pcb_sync.project_netclasses: true` on the observation request to inspect complete configured and effective native classes. The response also contains `selections`: a project digest, actual `net_settings_version`, named assignments with their raw JSON tokens, and ordered patterns with observed indices and exact raw tokens. Legacy scalar and empty assignments are preserved. Observation grants no write authority, accepts no caller scope sets and returns no complete project file.

Add `intent.project_netclasses` to the existing synchronization plan. Empty or omitted project actions retain the existing intent serialization and PCB-only behavior. Supported settings use `net_settings.meta.version: 5`; legacy project formats require an explicit native save on an owned copy before editing. The server does not migrate the source project automatically.

| Action | Selected fields |
| --- | --- |
| `assign` | Observed `net`, complete `classes` list |
| `remove_assignment` | Existing observed `net` |
| `add_pattern` | Observed insertion `position`, `class`, `pattern` |
| `update_pattern` | Observed `index`, selected `class` and/or `pattern` |
| `remove_pattern` | Observed `index` |
| `define` | `name`, optional `priority`, selected `dimensions` |
| `remove_class` | Existing unreferenced `name` |

Each dimension edit is `{field, change:{kind:"set", nm:INTEGER}}` or `{field, change:{kind:"inherit"}}`. Fields are `clearance`, `track_width`, `via_diameter`, `via_drill`, `microvia_diameter`, `microvia_drill`, `diff_pair_width`, `diff_pair_gap` and `diff_pair_via_gap`. Values use integer nanometres within 0–2,000,000,000. New definitions require an explicit nonnegative priority; `Default` retains its native priority and cannot be removed. Complete configured/effective class records bind definition state, values, parents, priorities, membership and tuning profile.

Assignments combine with native pattern behavior. An empty assignment or removal can fall through to a matching pattern; it does not disable that pattern. Select pattern changes explicitly and review the complete effective result. Original exported class constituents remain protected references, so removing a matching pattern may require a separate approved edit before class removal. Duplicate edits, foreign nets, missing definitions, invalid indices and arbitrary JSON fields reject. Unknown unselected tokens, including large numeric tokens, remain exact.

For a project-only change, `actions: []` and `copper_nets: []` are permitted with selected project actions. Complete existing source linkage, explicit orphan/exclusion disposition, metadata, pin/pad/net parity and physical preservation checks still run. No implicit `keep` action is synthesized. A verified project no-op produces no project write; if the PCB is also unchanged, it produces no approval or execution journal.

A project-changing plan reports `publication_kind: "project_and_optional_board"`, `project_differences` and `netclass_delta` in addition to the PCB differences and DRC delta. Preview requires `schematic_pcb_sync: {differences, project_differences, netclasses, drc}`. `netclasses` is the complete planned `netclass_delta`, including `source_nets`, `configured` and `effective`; `drc` contains complete `introduced` and `resolved` records. A PCB-only expectation cannot approve a project-changing plan. A requested project no-op uses `publication_kind: "board_only"` and the existing PCB expectation.

## Source-declared classes

Existing `directive_label`, its native `netclass_flag` alias, and ordinary labels with canonical `Netclass` properties are read as immutable source declarations. Both schematic formats 20250114 and 20260306 are qualified. Complete native source records determine each eligible pin's required classes; configured names and exact captured declaration names resolve the native expression. Ambiguous literal/composite names and unresolved expressions reject.

Every non-default source constituent must be present in the final native PCB net. A missing `Power` assignment therefore rejects an otherwise unchanged PCB plan. An unrelated project edit or selected project no-op cannot waive this requirement. `Default` is fallback; existing project patterns and other retained members continue to participate in the native result. Excluded-only source contacts create no eligible PCB class target, while existing linked excluded footprints still require explicit disposition.

Use explicit `assign` actions to satisfy declarations and explicit `define` actions for missing configured classes. Source bytes, labels, UUIDs, source membership and all unrelated data remain exact during synchronization. The plan exposes the complete configured/effective/source class changes and `erc_delta` before approval.

Defining a source-declared missing class can resolve its native `undefined_netclass` error. Preview then requires `erc` alongside the complete project/PCB/netclass/DRC expectation. Copy `removed_sheet_headers`, `added_sheet_headers`, `introduced` and `resolved` from the planned `erc_delta`. Each diagnostic record contains its complete `sheet_path`, `sheet_uuid_path` and `diagnostic_json`; retain the full diagnostic JSON string, unknown fields and multiplicity. Omitted `erc` authorizes no ERC difference, preserving existing unchanged-ERC requests and serialized hashes.

Project netclass changes reject ERC metadata/header changes and introduced diagnostics. A resolved diagnostic must identify a captured declaring label in its exact expanded sheet instance and belong to a newly selected explicit class definition. All other source ERC remains unchanged. Missing or same-count altered expectations reject. Actual publication independently reproduces the complete approved source/netlist/pins/ERC and native PCB/class/connectivity/DRC state.

## Durable publication and recovery

PCB-only executions use domain `pcb_sync`, execution schema4 and `.auto-kicad-sync-pending`. The initial durable record binds exact backups, candidate, expected differences/DRC, native baseline and absent-before sibling staging path. The OS project writer lock and pending record prevent conflicting writes. An interrupted or ambiguous write remains uncertain and blocks ordinary writers.

Project transitions use execution schema5 and recovery schema6, retaining PCB-only execution4/recovery5 encoding. Internal project baselines use version2 for declaration-aware original scope; version1 retains its original configured-only reconstruction and JSON encoding. Recovery validates the original frozen execution inputs and restores exact originals, including an originally unsynchronized source class or undefined-class error. A subsequent plan must satisfy the final source-class and ERC contract. Publish project settings before PCB bytes; omit the PCB write when its bytes are unchanged. Each destination has an exact candidate and owned staging prefix; fsync and source revalidation precede replacement. Restore in reverse order and reproduce the complete original native source, classes, PCB, connectivity and DRC before clearing the marker. Per-file atomic replacement does not provide filesystem-wide two-file atomicity.

Use `kicad_plan_recovery` with `project_dir`, `attempt_id` and `action: "restore_backups"`, then the existing recovery preview/commit tools with fresh approval. Recovery reconstructs source authority through the current server policy and validates retained evidence; it never accepts arbitrary resource roots from the journal. Later user bytes, altered candidates/configuration and arbitrary staging content reject restoration. Only an exact approved candidate prefix in a known absent-before staging file can be removed. Exact original bytes, complete fresh native serialization/connectivity/DRC and terminal journal records are required before the marker clears. Recovery does not run automatically.

## Qualification

```text
python3 scripts/check-schematic-pcb-sync.py SERVER OWNED_SOURCE OWNED_CONFIG NEW_OUTPUT
python3 scripts/check-schematic-pcb-sync.py SERVER OWNED_SOURCE OWNED_CONFIG NEW_OUTPUT --interruption
python3 scripts/check-codex-schematic.py synchronization SERVER OWNED_SOURCE OWNED_CONFIG NEW_OUTPUT
```

The driver requires ownership markers and a prepared source/PCB fixture with explicit `.auto-kicad-sync-copper.json` assignments. It tests complete expectations, source/configuration drift, wrong/restart/replayed approvals, actual publication, redundant copper declarations and no-change behavior. The interruption profile kills the actual stdio writer, checks later-user-byte protection, requires new approval, performs fresh native restoration and commits again. The Codex profile uses an actual app-server MCP client. Owner cleanup of interrupted private snapshots is recorded separately from automatic server cleanup.

Machine-readable qualification, real-project results and remaining scope are recorded in [schematic-pcb-sync.json](schematic-pcb-sync.json). General unrestricted synchronization remains deferred in the capability matrix; runtime availability reports this bounded interface separately.

[Native synchronization families](schematic-pcb-sync-families.md) qualify actual Codex net split/merge, repeated child aliases, complete common-pin components, official thermal-pad addition/replacement and a 128-template hierarchical batch. Explicit native refusal limits and GUI/final-project gates remain documented separately.
