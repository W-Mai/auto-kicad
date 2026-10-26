# Copper route batch preservation

Route batches preserve complete original native payloads while adding up to 256 copper objects and 100 graphics. `ExistingBoardItemSet::matches_after_additions` requires unique added identities disjoint from the original set, an exact resulting identity set and unchanged original persistent fields. Creation receipts and saved readback independently validate the new payloads. The comparison ignores only the existing transient board-parent field. Existing-object deletion retains its separate 100-target, unlocked-object and layer/type restrictions.

The route executor previously reused the deletion helper for additive preservation. That incorrectly rejected more than 100 additions or added Edge.Cuts after native creation in the disposable preview. The dedicated comparison permits the documented additive scope without extending deletion permissions or weakening original-payload checks.

## Reproduction

```sh
cargo build -p auto-kicad --bin auto-kicad --example check-copper-route-mcp
target/debug/examples/check-copper-route-mcp /absolute/auto-kicad /absolute/new-output-directory --boundary
```

The production stdio acceptance uses the existing 4-layer fixture. One approved batch adds two 128-segment F.Cu routes, 96 Dwgs.User segments and a rectangular cutout formed by 4 Edge.Cuts segments. The 96 drawing segments remain inside the existing 10 mm graphic-span constraint. The routes connect existing GND vias and front-side pads. A 257-copper batch rejects during semantic planning with `batch copper count exceeds 256`, before any source mutation.

Actual KiCad 10.0.7 preview, approval, commit and same-process source reload passed. The independent final native comparison verified all 356 added UUIDs and every original native record; complete footprint payloads and unrelated normalized content remained unchanged. Unconnected items fell from 7 to 5 with zero introduced DRC findings. Preview left the source unchanged, protected files were identical, the original fixture remained untouched and all owned editors and disposable copies were cleaned. See [compact evidence](copper-route-boundary.json).

The architectural contract exercises 356 additions including Edge.Cuts, rejects duplicate/missing/overlapping identities, detects omitted or altered original records including unknown fields, and retains deletion restrictions. Full offline quality passed 401 tests with 15 environment-dependent tests ignored, formatting, Clippy warnings-as-errors and capability-matrix validation. This synthetic limit test supplements [route lifecycle acceptance](copper-route-mcp.md) and [populated-copy acceptance](populated-copper-route.md); it is not an additional real-project profile.
