# Complete generated-zone refill verification

`TeardropRefillObservation` captures repeated native geometry and full live text on one exact KiCad 10.0.7 editor/document. `VerifiedTeardropRefill` checks authored definitions, generated copper and mask content, saved reload and an independent closed CLI refill. This immutable evidence does not authorize edits or establish electrical correctness.

## Preservation contract

Refill preserves all authored board definitions and complete ordinary native zone/graphic definitions, including generating pad/via/track settings. Generated regions may rebuild. Their complete text, including every fill, island flag, mask region and unknown field, must equal both saved input and independent CLI output by exact type/layer/net/priority/outline correspondence. Generated record order is immaterial; internal content remains strict. Ordinary copper retains its existing exact geometry normalization. Closed-file automatic-dimension handling remains confined to saved authored definitions.

Same-editor reload preserves complete ordinary native content and authored definitions/copper. The existing empty `setup/zone_defaults` rule applies to refill definitions. Generated native comparison removes only ephemeral UUIDs and transient parents, with one directional border conversion: `ZBS_INVISIBLE` to `ZBS_SOLID`, requiring exact `hatch none 0.1` in both texts and 100000 nm native pitch. Every other border field, setting, unknown field and native filled polygon remains exact. [KiCad's pinned border setter](https://raw.githubusercontent.com/KiCad/kicad-source-mirror/10.0.7/pcbnew/zone.cpp) clamps the zero pitch requested during generation; actual captures confirm this behavior. This private generated reload rule does not weaken ordinary or same-document equality.

## Actual acceptance

One owned editor completed two native refills, each with independent CLI comparison and same-process saved reload. The fixture contains 3 ordinary zones, including locked and hatched content, and 10 generated regions: straight pad, curved via, front/back track-end transitions and front/back solder-mask openings. All complete generated text/native checks passed. Every generated UUID and directional border conversion is retained for each refill.

An independent native Python reader compared every generated outline/fill coordinate, hole, island, layer, net, priority, lock/fill flag and supported zone setting between saved and CLI boards. SWIG does not expose the teardrop subtype enum as an integer; subtype coverage is established by the complete IPC records. Original files and settled project bytes stayed unchanged. PID 92599 closed and was verified absent.

The existing ordinary refill runner passed on 3 zones with one locked zone: repeated fill, full definition/copper equality, independent CLI, saved reload and an unfilled negative control. PID 97951 closed. Final quality passed 463 tests, 15 environment tests ignored, formatting and warning-denied workspace/all-target clippy. [Machine-readable evidence](teardrop-refill.json) retains hashes, correspondence records, reader scope and artifact paths.

Two rejected runs are retained. They exposed the already classified empty `zone_defaults` serialization; captured data also corrected the initial zero-pitch assumption to 0.1 mm. A temporary offline regression verified the retained generated records before the successful native rerun, then was removed from the suite. The independent reader initially interpreted an unbound SWIG enum as a pointer; its corrected field checks passed on retained files without another GUI launch.

## Reproduction

```sh
KICAD_PYTHON=/Applications/KiCad/KiCad.app/Contents/Frameworks/Python.framework/Versions/3.9/bin/python3.9
"$KICAD_PYTHON" scripts/prepare-teardrop-fixture.py scripts/fixtures/geometry-zones.kicad_pcb BASE_FIXTURE
"$KICAD_PYTHON" scripts/prepare-teardrop-refill-fixture.py BASE_FIXTURE/teardrops.kicad_pcb FULL_FIXTURE
kicad-cli pcb drc --refill-zones --save-board --format json --output FULL_FIXTURE/drc.json FULL_FIXTURE/teardrops.kicad_pcb
cargo build -p kicad-adapter --example check-teardrop-refill --example check-zone-refill
target/debug/examples/check-teardrop-refill FULL_FIXTURE/teardrops.kicad_pcb NEW_OUTPUT 10
"$KICAD_PYTHON" scripts/check-teardrop-refill-saved.py NEW_OUTPUT
target/debug/examples/check-zone-refill scripts/fixtures/geometry-zones.kicad_pcb ORDINARY_OUTPUT
```

The fixture reports existing DRC violations and unconnected items. This proves refill content, not electrical acceptance. Saved-source provenance, pad connectivity, preview/source generated correspondence, DRC UUID projection, public MCP approval/commit, GUI cancellation and complete product qualification remain separate requirements. Public geometry operations still reject UUID-free generated zones until those integration gates are implemented.
