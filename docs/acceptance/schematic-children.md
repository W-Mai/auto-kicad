# Schematic child creation and attachment

`create_sheet` creates a hierarchical placement with a bounded empty child or an explicitly inspected existing child. `retarget_sheet` changes an observed placement's child association. Both require `file_all_instances`, exact page/reference maps and the ordinary [approved schematic MCP workflow](schematic-general-mcp.md). Retargeting and placement removal retain detached child files and their inactive annotations.

## Inspect an existing child

Call `kicad_schematic_audit` with an exclusive read-only request bound to the current observation:

```json
{
  "inspect_child": {
    "observation_id": "OBSERVED_ID",
    "observed_revision": "OBSERVED_REVISION",
    "file": "children/channel.kicad_sch"
  }
}
```

The result contains `source_revision`, a transitive `sheets` inventory with local placement paths and source symbol UUIDs/metadata, and captured source context. Inspection grants no publication authority. `attach_existing` requires this revision; planning recaptures the complete child graph and rejects changed source bytes. Child paths are normalized project-relative `.kicad_sch` paths. Existing files and parents must be canonical and confined; symlinks, editor locks, cycles and missing parents reject. Directory creation and file cloning are outside this operation.

## Create a placement

```json
{
  "file": "design.kicad_sch",
  "scope": {"kind": "file_all_instances"},
  "action": {
    "kind": "create_sheet",
    "key": "sensor-channel",
    "name": "Sensor",
    "rectangle": {
      "position": {"x_nm": 100000000, "y_nm": 100000000},
      "width_nm": 20000000,
      "height_nm": 20000000
    },
    "child": {"kind": "create_empty", "file": "children/sensor.kicad_sch"},
    "pins": [],
    "pages": [{"parent_instance": "OBSERVED_PARENT_PATH", "relative_path": "/", "page": "10"}],
    "references": []
  }
}
```

Creation allocates placement/file/pin UUIDs server-side and inherits the parent's supported source format. New hierarchical pins use `{key,pin:{name,shape,position,rotation}}` with explicit absolute contacts under the same [edge constraints](schematic-sheets.md) as existing pins. Empty creation requires an absent destination in an existing directory. To attach instead, use `{"kind":"attach_existing","file":"children/existing.kicad_sch","source_revision":"INSPECTED_REVISION"}` and supply complete references. `retarget_sheet` takes an observed placement `uuid`, `child`, `pages` and `references`; it preserves the placement name, rectangle, pins and UUID.

Every born sheet instance requires one page assignment. `parent_instance` identifies the observed host instance; `relative_path` is `/` for the selected child's root or `/PLACEMENT_UUID/...` for nested placements. It excludes the child file's own UUID. Reused physical hosts require all parent instances. Page labels are bounded ASCII alphanumeric strings; wholly numeric labels require canonical positive `u32` text. Changed/new labels cannot collide with active pages. Missing final page records require explicit initialization in the same edit batch.

Every born source unit requires one `{parent_instance,relative_path,symbol_uuid,reference}` assignment, including non-board and power units. Compatible units of one component share their reference within that instance. Distinct component instances retain globally distinct references. Missing/extra/duplicate/conflicting maps reject before publication. Unit numbers and metadata derive from the captured source. Existing foreign and inactive annotation rows remain unchanged; identical prior rows can be reused, while conflicting inactive rows reject.

## Configured pages

Observation returns `sheets` entries containing `file`, `sheet_path`, `page` and `native_ordinal`, including empty pages. `page` is the exact captured configuration label, or null when missing. Native XML ordinals describe export ordering and cannot verify configured labels. Plans retain both source/candidate inventories. `set_sheet_page` takes `uuid: null` for the observed root, or an observed placement UUID, and complete per-parent `pages` entries with `relative_path: "/"`. It changes only explicit page records and preserves unrelated records. Root displayed labels are independently qualified through SVG; nested displayed labels remain subject to GUI qualification.

## Publication and recovery

The preview retains immutable additional authority for selected child sources, absent destinations, editor locks, existing parent identities and any library resources used only by attached children. This authority is separate from the original observation and shares its byte/file limits. Complete native candidate validation includes component grouping, pins, netlist, ERC and explicit sheet-header changes. Attaching an existing orphan does not create its existing physical no-connect markers; logical instance pin births/deaths remain explicit.

Publication writes new child files before existing references and journals exact present/absent before-state through the durable per-file writer. It verifies the actual saved hierarchy and complete native results against the preview. Later user bytes never become replacement/deletion authority; source/resource/absence drift revokes approval permanently even after restoration. Interrupted publication blocks ordinary writes until a fresh guarded recovery restores exact original bytes and new-file absence. Filesystem-wide atomicity is not provided.

## Native qualification

```text
python3 scripts/check-schematic-children.py SERVER OWNED_ROOT_SCHEMATIC OWNED_CONFIG NEW_OUTPUT
python3 scripts/check-schematic-children.py SERVER OWNED_ROOT_SCHEMATIC OWNED_CONFIG NEW_OUTPUT --file channel_strip.kicad_sch --shared
```

The marked fixture must contain an independent `orphan.kicad_sch`, complete embedded definitions and approved library context, plus absent `created.kicad_sch` and `second.kicad_sch` destinations. The driver creates a child, adds a catalog resistor, retargets to populated multi-unit content, retargets to an empty file, reattaches using existing annotations and removes the placement while retaining child files. Every commit has independent saved native network/ERC comparison and wrong-approval/replay checks. Detailed evidence and additional transitive, authority, page-label and real interruption cases are recorded in [machine-readable acceptance](schematic-children.json). GUI/Codex/full-family qualification, PCB synchronization and the remaining product gates stay separate.

Actual KiCad 10.0.7 production stdio passed both source formats and a physical parent reused in 4 instances. Creation, symbol insertion, populated multi-unit retargeting, empty retargeting, annotation reuse and retained-file removal pass complete independent saved native network/ERC checks. A separate populated two-level attachment creates an explicit hierarchical pin and rejects a source cycle. Root labels 1→10→1 pass independent SVG readback; nested displayed labels remain unqualified. Exact original parent forms, protected configuration and source originals remain unchanged.

The accepted runs contain 23 committed native stdio executions, 108 independently rehashed artifacts and 29 verified backups. The actual mixed-existence writer interruption preserves later user bytes, blocks ordinary writes, restores exact original content and new-file absence through fresh MCP approval, and permits a subsequent normal commit. Source/library/absent-destination drift permanently revokes approval. Complete quality passes 566 tests with zero failures and 16 environment-dependent tests ignored; actual fixed two-resistor stdio/native compatibility also passes. Failed ENOSPC and acceptance-fixture/symlink-snapshot attempts remain separate. No GUI was launched.
