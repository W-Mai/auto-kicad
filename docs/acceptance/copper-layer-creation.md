# Native copper-layer track creation

`kicad_model::CopperLayer` accepts canonical `F.Cu`, `B.Cu` and `In1.Cu` through `In30.Cu` strings. Invalid indices, aliases and non-copper layers cannot construct the type. `require_enabled_on` checks a valid observed stackup before mutation. `kicad_adapter::native_copper_layer` converts the domain identity to the copper-only IPC type without exposing raw protocol enum values.

`CreateTrack.layer` selects the native copper layer. Creation receipts must return that exact layer; adapter saved readback also checks the requested layer. All 32 layer mappings and wrong-layer receipt rejection have offline contract coverage. Existing callers explicitly select F.Cu, preserving current MCP single-track, assembly and replay behavior. The native primitive requires its caller to validate enabled layers before starting a transaction; it does not perform an additional stackup query for every segment.

The complete offline quality suite passed 387 tests with 15 environment-dependent tests ignored, alongside format, Clippy warnings-as-errors and capability-matrix checks. The separate actual KiCad 10.0.7 stdio MCP mixed-routing regression passed in 13.51 seconds, covering existing front-layer track/via preview, approval, commit, saved reload and editor cleanup.

## Reproduction

```sh
cargo build -p kicad-adapter --example check-copper-layers
target/debug/examples/check-copper-layers /absolute/new-output
```

The macOS probe creates an owned four-layer fixture with two existing through vias and opens one PCB Editor. It settles the disposable saved fixture before capturing baseline evidence, then creates 4 tracks between the vias in one native commit, one on each enabled copper layer. In3.Cu is absent and rejected during preflight. The probe saves and reloads in the same editor process, checks exact creation/readback identities and layers, compares complete persistent payloads of every original board item, checks unchanged settled project bytes, runs DRC and closes the owned editor. The untouched input fixture, retained result PCB/project, native payloads and DRC reports remain in the output directory.

KiCad 10.0.7 acceptance passed on F.Cu, In1.Cu, In2.Cu and B.Cu. No DRC violations were introduced; unconnected items decreased from 2 to 1. Native preservation, exact selected-layer receipts, saved reload, disabled-layer refusal, source isolation and editor cleanup passed. See [compact evidence](copper-layer-creation.json).

This acceptance covers native selected-layer track primitives. Public MCP layer selection, persisted selected-layer intents, route anchors and polyline/via transactions require separate integration. The fixture is synthetic and does not count toward the three-real-project gate. Remaining unconnected items are retained baseline evidence rather than a completed circuit claim.
