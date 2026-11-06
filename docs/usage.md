# Using auto-kicad

The MCP server provides 13 tools. Board edits use a running PCB Editor; schematic edits, synchronization and exports use closed, saved projects. Every write requires a plan, preview and approval.

## Tools

| Purpose | Tools |
| --- | --- |
| Discover capabilities and inspect designs | `kicad_capabilities`, `kicad_inspect_project` |
| Plan, preview and commit PCB edits, synchronization or exports | `kicad_plan_change`, `kicad_preview_change`, `kicad_commit_change` |
| Validate a board | `kicad_validate_project` |
| Observe and edit a schematic | `kicad_schematic_audit`, `kicad_plan_schematic_change`, `kicad_preview_schematic_change`, `kicad_commit_schematic_change` |
| Plan, preview and commit recovery | `kicad_plan_recovery`, `kicad_preview_recovery`, `kicad_commit_recovery` |

Start with `kicad_capabilities` to check backend availability and operation limits. Tool discovery alone does not establish that KiCad is ready for a particular operation.

## PCB edits

Save the board and its same-named `.kicad_pro`, enable the KiCad IPC API and keep the board open in PCB Editor. Use the endpoint for that editor instance; macOS sockets are normally under `/tmp/kicad/`. Inspect it with `kicad_inspect_project` before planning.

This request to `kicad_plan_change` adds a 3 mm line on Dwgs.User. Replace the endpoint with the inspected editor's endpoint:

```json
{
  "endpoint": "ipc:///tmp/kicad/api.sock",
  "source_run_id": "board-review",
  "request_id": "add-review-mark",
  "segments": [{
    "graphic_id": "review-mark",
    "start": {"x": 80000000, "y": 78000000, "unit": "Nanometres", "space": "Board"},
    "end": {"x": 83000000, "y": 78000000, "unit": "Nanometres", "space": "Board"},
    "width_nm": 100000,
    "layer": "DwgsUser"
  }]
}
```

Pass the returned `plan_id` and `plan_hash` to `kicad_preview_change`. Review the preview, then pass the same plan identity and its `approval_hash` and `validation_evidence_hash` to `kicad_commit_change`. Preview checks a saved copy in an owned editor, including save/reload, object preservation and DRC changes. Unsaved differences between the source editor and saved files reject the preview.

Select operation fields for the intended edit:

| Operation | Request field and reference |
| --- | --- |
| Move or rotate a free footprint | [`move_footprint`](acceptance/existing-footprint-transform.md), with the observed placement, native UUID and revision |
| Edit existing tracks, through vias or Dwgs.User segments | [`edit_existing`](acceptance/existing-item-edit.md), with explicit target identities and field patches |
| Flip a free footprint | [`flip_footprint`](acceptance/footprint-flip.md), with an explicit side and axis |
| Delete supported objects | [`delete_items`](acceptance/item-deletion.md), with an exact target set |
| Place a source-bound footprint or connected assembly | [`footprint` or `assembly`](acceptance/general-footprint-sources.md), with saved schematic and library bindings |
| Add a path across selected copper layers | [`route`](acceptance/copper-route-mcp.md), with explicit anchors, path and via transitions |
| Edit board outlines or solid zones | [`geometry`](acceptance/geometry-mcp.md), with a separate geometry observation and refill policy |

Footprint transforms reject connected or locked targets and unsupported native metadata. Placement requires an unambiguous saved source symbol, compatible pins/pads and an approved library definition. An assembly is exclusive with other top-level operation fields, supports at most 100 placements plus connections, and connects unambiguous front-copper pads on the same nonempty net. Its [connected-resistor example](#connected-assembly) is below.

Geometry observation uses `geometry: true`. Use `data.geometry.observation.observed_revision` for geometry requests; it differs from the ordinary PCB revision. Zone edits require `refill: "all"`. Explicit routing on supported zoned boards also verifies the resulting fill and connectivity. See [zoned routing](acceptance/zoned-additive-mcp.md) and [geometry limits](acceptance/geometry-boundaries.md).


### Connected assembly

The saved, closed schematic must already contain R1 and R2 with the specified values and footprint fields, and R1.2 connected to R2.1. This request places both footprints and their connecting track in one approved board operation:

```json
{
  "endpoint": "ipc:///tmp/kicad/api.sock",
  "source_run_id": "resistor-circuit",
  "request_id": "place-connected-resistors",
  "assembly": {
    "placements": [
      {"target_id": "r1", "library_nickname": "Resistor_SMD", "library_entry_name": "R_0603_1608Metric", "x_nm": 5000000, "y_nm": 5000000, "reference": "R1", "value": "1k"},
      {"target_id": "r2", "library_nickname": "Resistor_SMD", "library_entry_name": "R_0603_1608Metric", "x_nm": 10000000, "y_nm": 5000000, "reference": "R2", "value": "1k"}
    ],
    "connections": [
      {"track_id": "r1-r2", "start": {"target_id": "r1", "pad_number": "2"}, "end": {"target_id": "r2", "pad_number": "1"}, "width_nm": 250000}
    ]
  }
}
```

Template digests, source identities and pin-to-pad assignments are bound to the preview. A failure before native commit discards the transaction; an uncertain commit or save requires inspection and fresh recovery approval.

## Closed-file operations

Save and close project editors before observing a schematic, synchronizing it or exporting files. Stop other writers to those files. Observation captures source, configuration and library inputs; changes to those inputs invalidate approval.

| Task | Entry point | Request reference |
| --- | --- | --- |
| Edit symbols, properties, wires, labels or sheets | `kicad_schematic_audit` → schematic plan/preview/commit tools | [Schematic editing](acceptance/schematic-general-mcp.md) |
| Update a PCB from its schematic | `kicad_inspect_project.schematic_pcb_sync` → board plan/preview/commit tools | [Synchronization](acceptance/schematic-pcb-sync.md) |
| Export Gerber, drill, BOM or PDF | `kicad_inspect_project.artifact_export` → board plan/preview/commit tools | [Exports](acceptance/local-delivery-and-exports.md#export-requests) |

Schematic previews require explicit expected connectivity and ERC changes. Synchronization previews require the exact selected differences and DRC changes; project-netclass edits have additional expectations. Export previews require the complete output-file inventory. Copy these structures from the corresponding plan rather than replacing them with counts or an empty list.

Synchronization preserves manual geometry and copper while applying selected source changes. Replacement requires compatible physical pads and retained connected contacts. Net reassignment requires explicit selection of affected copper. Zone changes use the [region profile](acceptance/schematic-pcb-sync-regions.md); custom-rule evaluation is unsupported. The [family](acceptance/schematic-pcb-sync-families.md) and [size-limit](acceptance/schematic-pcb-sync-scale.md) records describe tested inputs.

Exports publish into a new relative directory with an existing parent. Existing destinations, traversal and symlinks reject. Publication preserves the approved file inventory and leaves CAD inputs unchanged.

## Recovery

An `unknown` result means execution did not establish a verified final state. Inspect the execution history and saved files before issuing another write. Backups remain under `~/.local/state/auto-kicad`, or the configured `AUTO_KICAD_STATE_DIR`.

Close editors before recovery. For PCB and schematic attempts, call `kicad_plan_recovery` with the canonical `project_dir`, recorded `attempt_id` and `action: "restore_backups"`. Review `kicad_preview_recovery`, then commit with its new approval. Recovery rejects changed source files and missing or altered backups; it never removes KiCad lock files. Each replacement is atomic, but a multi-file restore is not one filesystem transaction. See the [recovery contract](acceptance/guarded-recovery.md).

When no editor is available, `kicad_inspect_project` with `endpoint: ""` and `project_dir` reads durable execution and recovery history without returning a live board snapshot. Pending schematic or geometry recovery blocks ordinary writes across server state directories. A server restart requires fresh plans and approvals.

Export recovery uses `finalize_export` or `retain_artifacts` and never restores CAD files. Its authority and retained-file rules are described in the [export recovery contract](acceptance/local-delivery-and-exports.md#recovery-and-resource-ownership).

## Configuration

| Setting | Purpose |
| --- | --- |
| `AUTO_KICAD_STATE_DIR` | Execution records, backups and recovery state |
| `KICAD_CONFIG_HOME` | KiCad configuration captured for native schematic operations |
| `AUTO_KICAD_LIBRARY_CONFIG` | Server-side JSON configuration for library tables, permitted roots and path variables |
| `AUTO_KICAD_OFFICIAL_SYMBOL_ROOT` | Installed official symbol-library root |
| `AUTO_KICAD_OFFICIAL_FOOTPRINT_ROOT` | Installed official footprint-library root |
| `AUTO_KICAD_PROJECT_FOOTPRINT_ROOT` | USB-C footprint directory used by the legacy source mapping |

The installed launcher adds the macOS KiCad CLI directory to `PATH`. Fixed helpers are embedded in the executable and use KiCad's bundled Python runtime. MCP requests cannot select executable paths or grant access to arbitrary library directories. Library configuration is captured at server startup; restart after changing it. See [library configuration](acceptance/general-footprint-sources.md#source-configuration).
