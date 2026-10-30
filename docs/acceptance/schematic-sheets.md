# Existing sheet and hierarchical pin edits

`update_sheet` edits an observed placement's name, rectangle and hierarchical pins through the bounded schematic MCP workflow. The compiler projects the complete resulting pin set before checking geometry. Existing pin UUIDs, child-file associations, instance annotations, effects and untouched source intervals remain exact. It accepts one complete update per placement per batch.

`update.name` changes the `Sheetname` property. Names must be nonempty single-line path segments and distinct among sibling placements. Explicit name swaps are checked against the complete final sibling set. `update.rectangle` contains `position`, `width_nm` and `height_nm`. Coordinates and dimensions use the exact 100 nm native grid, dimensions must be positive and extents must fit native coordinates. Moving the rectangle translates existing sheet property positions by the origin delta while preserving field rotation and effects. Resizing preserves their offset from the origin.

Pin positions are absolute. Moving or resizing a rectangle does not drag pins or wires. Include explicit pin updates where needed to keep the final pin set on its edges. A contact must lie strictly between the corners, with direction `zero` on the right, `half` on the left, `ccw90` on top or `ccw270` below. Final pin names and contacts must be unique. Pin creation allocates UUIDs once against all observed and allocated identities. Creation keys share the ordinary creation namespace. The combined outer and nested operation count cannot exceed 128. Bus/variable pin names and unsupported electrical forms reject.

```json
{
  "file": "design.kicad_sch",
  "scope": {"kind": "file_all_instances"},
  "action": {
    "kind": "update_sheet",
    "uuid": "OBSERVED_SHEET_UUID",
    "update": {
      "name": "Analog",
      "rectangle": {"position":{"x_nm":100000000,"y_nm":50000000},"width_nm":40000000,"height_nm":20000000},
      "pins": [{
        "kind": "update",
        "uuid": "OBSERVED_SHEET_PIN_UUID",
        "pin": {"name":"IN","shape":"input","position":{"x_nm":100000000,"y_nm":60000000},"rotation":"half"}
      }]
    }
  }
}
```

Place this edit inside the existing versioned `general` request. Nested pin operations are `create` with `key`/`pin`, `update` with `uuid`/`pin`, or `delete` with `uuid`. Unlisted pins retain their exact source and contact. Reused physical parents require `file_all_instances`; the preview lists every affected parent instance. [Bounded schematic MCP](schematic-general-mcp.md) retains full native hierarchy/connectivity/ERC evidence, explicit expectations, exact approval, durable publication and actual-source verification.

Renaming a sheet changes ERC sheet headers and may rename networks. `native_preview.erc_delta` contains `removed_sheet_headers` and `added_sheet_headers` as canonical JSON strings retaining complete header fields and exact numeric tokens. Declare these exact arrays in `expectations.erc`, alongside any changed diagnostics. Missing, extra or duplicate declarations reject. ERC metadata changes remain rejected. [Explicit expectations](schematic-expectations.md) retain preservation by default; header declarations grant no diagnostic or electrical exception.

## Native qualification

```text
python3 scripts/check-schematic-sheets.py SERVER OWNED_ROOT_SCHEMATIC OWNED_CONFIG NEW_OUTPUT
python3 scripts/check-schematic-sheets.py SERVER OWNED_ROOT_SCHEMATIC OWNED_CONFIG NEW_OUTPUT --file channel_strip.kicad_sch --sheet-name AK_NESTED
python3 scripts/check-schematic-general-interruption.py SERVER OWNED_ROOT_SCHEMATIC OWNED_CONFIG NEW_OUTPUT --sheet-name CH2
```

Inputs require the owned-copy marker. Profiles use the installed multichannel example with qualified component fixtures in formats 20250114 and 20260306. A nested placement in a physical parent reused in 4 instances checks shared-file scope. Its empty child and initial placement are manual fixture setup before observation; they are not evidence of child-file authoring.

The driver checks names, explicit connected-pin movement, resizing, pin creation on all edges, pin mutation/deletion and connected-port renaming/restoration. Independent saved native exports compare complete component networks and ERC after each commit. Source checks preserve unrelated placements, symbols, wires, child-file associations, annotations, effects, UUIDs and configuration. Rehashed immutable before/candidate records additionally verify every requested rectangle and translated property coordinate. Wrong approvals, replay, source drift, partial ERC header declarations, invalid pin ownership/direction, duplicate names, implicit pin dragging and operation bounds reject. No GUI is launched.

Placement creation/removal, child-file creation/attachment, full GUI/Codex qualification and schematic-to-PCB synchronization remain unaccepted. These example-based profiles do not complete the final distinct-real-project gate.

Actual KiCad 10.0.7 production stdio passed 31 sheet commits across both formats and the reused-parent profile, with 124 publication artifacts rehashed. Every commit passed independent saved native network/ERC comparison; immutable before/candidate evidence verified all rectangles and translated fields. Real sheet-publication interruption, fresh guarded recovery, later-byte protection, exact original restoration and a subsequent normal commit passed; 7 additional artifacts were rehashed. Complete quality passed 561 tests with zero failures and 16 environment-dependent tests ignored. The fixed CLI/MCP regression passed in 4.60 seconds. [Machine-readable acceptance](schematic-sheets.json) records scope and retained failed controls.
