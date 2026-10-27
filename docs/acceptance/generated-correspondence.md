# Generated-zone correspondence and DRC snapshots

`GeneratedZoneCorrespondence` returns a bijective generated UUID map only when every correspondence key and complete text/native definition/content digest agrees. It covers generated zones only. Ordinary identities, whole-board intent and approval remain separate checks. Different lifecycle states do not receive the reload-only border exception.

`GeneratedDrcSnapshot` compiles actual saved text against a bound observation. Complete generated saved content must agree with the observation, and text/native matching must be exact and unambiguous. The compiler inserts one explicit UUID into each classified generated zone using CST spans. All original bytes remain unchanged. UUIDs must be canonical, unique and distinct from every authored UUID, including nested footprint objects. Input/output SHA-256 values and bindings are retained. The output is exclusively for an owned closed DRC copy; it must never replace or reload the live source.

## Native acceptance

Two independent copies each changed an existing ordinary zone's clearance from 0.20 to 0.25 mm, refilled natively, passed complete independent refill verification, saved and reloaded. Their 10 generated copper/mask UUIDs all differed. Complete authored content, ordinary native records and generated content agreed. Each copy's compiled DRC snapshot produced identical repeated raw reports. Mapping only the verified generated UUIDs made the two copies' complete raw DRC reports equal, including unconnected items, schematic parity and report metadata other than date/source. Without that map the reports differed, so the fixture exercised the identity problem.

An independent KiCad Python reader verified every explicit generated UUID survived native loading, confirmed the 0.25 mm clearance, and compared complete native serialization of bound/saved boards modulo direct zone ordering. Input files remained unchanged. It independently repeated the raw-report comparison. Source files were preserved; editors 94623 and 94964 ran sequentially, closed and were verified absent. The fixture contains 12 preexisting violations and 4 unconnected items; this is correspondence acceptance, not a clean electrical design.

Full quality passed 465 tests, 15 environment tests ignored, formatting and warning-denied workspace/all-target clippy. Architecture tests reject content/unknown-digest changes, missing/duplicate mapping, malformed UUIDs and collisions with nested authored identities. [Machine-readable evidence](generated-correspondence.json) retains mappings, hashes, records and artifact paths.

## Reproduction

Use the 10-region fixture from [complete teardrop refill verification](teardrop-refill.md), then run:

```sh
cargo build -p kicad-adapter --example check-generated-correspondence
target/debug/examples/check-generated-correspondence FULL_FIXTURE/teardrops.kicad_pcb NEW_OUTPUT
/Applications/KiCad/KiCad.app/Contents/Frameworks/Python.framework/Versions/3.9/bin/python3.9 scripts/check-generated-correspondence.py NEW_OUTPUT
```

The feasibility probe retained an initial byte-comparison failure caused by direct zone ordering; a read-only continuation proved full content equivalence under that ordering and exact repeated bound DRC. Two build attempts failed from disk exhaustion, were retained as failed logs and were not counted as validation. The accepted build and full suite ran after space recovered.

## Integration boundary

Public geometry MCP operations still reject UUID-free generated zones. Runtime composition must preserve saved-source provenance and complete project/rule/library context, then integrate generated bindings with inspection, receipt verification, candidate approval, DRC, uncertainty and recovery. The existing typed DRC model retains unconnected-item counts but not their full diagnostics; production correspondence must preserve those details before using this mapping. These requirements and the remaining product milestones stay open.
