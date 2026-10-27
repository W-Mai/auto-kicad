# Generated teardrop observations

`kicad_adapter::live_geometry::TeardropTextPartition` parses a complete bounded KiCad 10.0.7 board and separates explicitly classified generated zones from authored content. It retains the original CST. `bind` pairs each generated text region with exactly one complete native zone record. `TeardropIdentity` records native UUID, correspondence key and separate complete text/native definition/content digests.

KiCad saves generated copper and mask zones without UUID or name. Its full refill rebuilds them with identities derived from the generating items; reloading assigns different transient identities. The pinned [serializer](https://raw.githubusercontent.com/KiCad/kicad-source-mirror/10.0.7/pcbnew/pcb_io/kicad_sexpr/pcb_io_kicad_sexpr.cpp) and [teardrop manager](https://raw.githubusercontent.com/KiCad/kicad-source-mirror/10.0.7/pcbnew/teardrop/teardrop.cpp) describe those paths. Native and saved observations below verify the behavior independently of those source comments.

## Binding contract

Only direct zones with the exact `attr/teardrop/type` classification can omit identity. Supported classification values are `padvia` and `track_end`. The matching signature includes type, one verified copper or front/back mask layer, network name, priority and every outline point/arc at exact nanometre precision. Exact cyclic rotation and reversed winding normalize for correspondence only. Missing, additional or ambiguous matches reject, as do unclassified types, duplicate attributes and unexpected generated UUID/name fields. Combined zones and geometry remain bounded by the existing observation limits.

Complete generated text/native digests preserve settings and unknown fields. Native digest projection removes only UUID and transient parent; definition digests additionally exclude derived fill. Matching correspondence keys does not imply complete content equality. The authored remainder goes through the unchanged strict ordinary-zone parser and preserves every non-generated board, pad, via and track field. Ordinary zone identities and native/live lock checks remain exact. Existing ordinary parsing still rejects UUID-free zones; this separate observation type grants no editing or refill permission.

## Actual acceptance

A native-written fixture enables straight pad and curved via teardrops and the connected track's solder-mask opening. KiCad produced 2 copper and 2 mask generated zones alongside 3 existing ordinary zones, including locked and hatched content. One owned editor observed the initial load and 2 full native refills. Every stage passed bijective live/native binding, saved-text binding and same-process reload correspondence. Altering text classification rejected against the real native records.

All 4 generated UUIDs changed on every reload. Correspondence keys and complete generated text remained equal; the authored definition digest remained equal across all stages. Full native definition/content digests detected a border-style difference: regeneration produces `ZBS_INVISIBLE`, while reloading produces `ZBS_SOLID`; both serialize as `hatch none`. That difference is retained and has not been waived by this implementation.

An independent KiCad Python reader checked each saved generated region's layer, network, priority and every outline coordinate in all 3 saved stages, without modifying the files. Original source files were unchanged and owned editor PIDs 27819 and 43080 were verified absent. Full quality passed 461 tests, 15 environment tests ignored, formatting and warning-denied workspace/all-target clippy. [The machine-readable evidence](teardrop-observation.json) retains source hashes, identity mappings and separate digest differences; full protobuf/text/native boards remain at its recorded artifact root.

## Reproduction

```sh
KICAD_PYTHON=/Applications/KiCad/KiCad.app/Contents/Frameworks/Python.framework/Versions/3.9/bin/python3.9
"$KICAD_PYTHON" scripts/prepare-teardrop-fixture.py scripts/fixtures/geometry-zones.kicad_pcb NEW_FIXTURE
kicad-cli pcb drc --refill-zones --save-board --format json --output NEW_FIXTURE/drc.json NEW_FIXTURE/teardrops.kicad_pcb
cargo build --release -p kicad-adapter --example check-teardrop-observation
target/release/examples/check-teardrop-observation NEW_FIXTURE/teardrops.kicad_pcb NEW_OUTPUT
"$KICAD_PYTHON" scripts/check-teardrop-saved.py NEW_OUTPUT
```

This is observation acceptance. Full generated fill/copper/mask equivalence, result correspondence across preview/source, DRC identity projection and public MCP handling of boards containing teardrops are not established here. Track-end and back-mask forms have parser contracts but no native acceptance in this fixture. GUI cancellation, zoned routing and complete product qualification remain separate requirements.
