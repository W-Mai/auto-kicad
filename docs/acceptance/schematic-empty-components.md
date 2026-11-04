# Verified empty component pins

`SchematicPinIndex::component_pin_numbers` returns the complete active pin-number set for an exact ordered component-unit identity. A verified empty set is retained alongside ordinary nonempty sets. Missing or partial identity groups return no authority. The source/native join verifies every unit's pin metadata against the embedded definition and accounts for every native net node before returning the index.

Closed schematic-to-PCB synchronization can inspect zero-pin components and update or explicitly relink their existing footprints. Every native unit pin list and the footprint's distinct numbered-pad set must be empty. Unnumbered physical pads, dimensions, field placement, geometry, IDs, configuration, physical partitions and full native serialization remain protected. A pinless active body with nonempty native unit metadata rejects. No reference-name convention supplies pin authority.

Zero-pin `add` and `replace` reject at template capture before allocation. Footprint assignment retains its separate complete pin/pad compatibility guard. Unmapped numbered mechanical, shield and unused switch pads remain conflicts, including `MP`, `SH` and `SN/RN/TN`; their names do not grant an exception.

The intact PIC programmer hierarchy passes public observation with 63 components, six verified empty mounting-hole components and 376 copper objects. This is read-only real-project evidence. Its D-sub shield pads remain a separate full-synchronization conflict. An owned native control verifies actual public relink/native publication, source Value authoring and subsequent PCB Value synchronization with exact complete expectations. Creation and replacement reject; independent retained-source verification covers all 4 footprints, every unnumbered-pad/geometry token and five approved combined metadata differences. Native fixture preparation is recorded separately from MCP creation.

The ECC83 real pair separately passes complete Value editing of its three units, explicit source-library repair and relinking of 15 footprints through production MCP. All 59 copper objects and the complete solid zone remain unchanged. Independent source sealing checks every unselected root byte/order and all retained footprint/pad geometry and unknown tokens outside 67 exact approved metadata differences. Its evidence retains its distinct runtime hash. The multichannel_mixer passes source editing through actual Codex, while incompatible numbered pads reject source assignment before PCB mutation. Generated-teardrop preparation in complex_hierarchy fails its bounded exact-serialization control and preserves original board bytes.

```text
python3 scripts/check-schematic-pcb-sync-zero-pin.py SERVER OWNED_SYNCED_SOURCE OWNED_CONFIG NEW_OUTPUT
```

The command copies owned synchronized `sync.kicad_sch` two-resistor inputs into a new output directory and prepares one native mounting-hole control. It requires the fixture's fixed root UUID, ownership markers, installed macOS KiCad 10.0.7 libraries and its bundled Python runtime. Inputs remain unchanged. The test performs actual MCP observation, Add/Replace rejection, Relink/native commit, source Value commit and a matching PCB Update/native commit in one server process. It opens no PCB Editor.

Full quality and electrical stdio killed-writer/native recovery plus actual Codex drift/approval/commit/no-op regressions pass on the empty-component runtime. [Machine-readable evidence](schematic-empty-components.json) distinguishes positive controls, actual real-project observations and remaining conflicts. The complete product and unrestricted synchronization remain unfinished.
