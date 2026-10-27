# Schematic instance and component identities

`kicad_schematic::identities::SchematicIdentityIndex` expands physical schematic files into sheet instances and matches native netlist component UUID sets to all source symbol units. Repeated-sheet annotations are read from the matching project and instance path. A multi-unit component retains every member UUID and unit number. Source power and non-board symbols remain in the index even when omitted from the board netlist.

The source parser retains `lib_name` separately from `lib_id`. Local embedded symbol overrides can produce an empty native netlist library name; correspondence uses the exact embedded name when present. Literal value, footprint and effective library identifiers must match. Variable-expanded properties require separate context-aware validation and can reject this index.

The resolver validates the observed artifact set, expands at most 4096 instances from 128 physical files, bounds depth to 32 and expanded symbol units to 100000, and rejects cycles, path escapes, duplicate object/instance/native identities, missing annotations, inconsistent unit numbers and mismatched native sheet/component membership. Component lookup uses the instance path and annotation index. Existing single-unit placement and fixed-circuit authoring contracts remain unchanged.

This is an identity model, not source-write authorization. Its caller must establish that parsed source fields and the native XML came from the exact bound project bytes and environment. It does not prove pin connectivity, full ERC, embedded library contents, equality of all source properties, complete inclusion of every source unit in the board netlist, or safe publication. No general schematic-edit or PCB-sync MCP capability is enabled by this module.

## Native evidence

Installed KiCad 10.0.7 exported XML and complete ERC from owned copies of the installed ECC83 and multichannel examples. The resolver matched all native component identities:

| Project | Physical files | Sheet instances | Expanded source units | Native components |
| --- | --- | --- | --- | --- |
| ECC83 | 1 | 1 | 26 | 15 |
| Multichannel mixer | 2 | 5 | 169 | 114 |

ECC83 U1 has three units. The multichannel mixer has repeated CH1–CH4 child instances, different per-instance references and multi-unit ICs. The older single-unit join rejected both inputs; its separate conservative contract is retained. Full identity outputs, original hashes, native XML/ERC, scripts and source snapshots are retained under the roots recorded in [the evidence manifest](schematic-identities.json).

The initial ECC83 research copy omitted its local footprint files and reported 11 ERC warnings. The complete local-library copy reports zero; multichannel retains 36 baseline ERC items. These native reports demonstrate context sensitivity and remain research evidence, not an implemented general ERC comparison feature. No GUI was opened, and all original/copied input hashes remained unchanged.

Architecture checks cover deterministic ordering, repeated-sheet instance references, complete multi-unit UUID sets, source/native mismatches, embedded library overrides, hierarchy cycles/path escapes and expanded-instance bounds.

## Reproduction

Create an owned copy retaining project, schematic hierarchy, symbol libraries, footprint libraries and both library tables. Export its native netlist using the installed CLI, then pass that same schematic and XML to the read-only example:

```sh
kicad-cli sch export netlist --format kicadxml -o /absolute/output/netlist.xml /absolute/copy/root.kicad_sch
cargo run -p kicad-schematic --example check-schematic-identities -- /absolute/copy/root.kicad_sch /absolute/output/netlist.xml
cargo test -p kicad-schematic identities::tests
```

The example reads an existing XML file and does not establish its provenance by itself. General authoring, standalone complete ERC/connectivity and schematic-to-PCB synchronization remain unimplemented.

Full workspace quality passed: 487 tests, zero failures and 15 explicitly ignored environment tests; formatting, capability-matrix checks and warnings-denied all-target Clippy passed. The existing production CLI schematic MCP and footprint-binding regression also passed on 10.0.7 in 4.82 seconds without opening an editor.
