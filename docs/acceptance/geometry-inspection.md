# MCP geometry inspection

`kicad_inspect_project` accepts `geometry: true` to return bounded native zone and board-level Edge.Cuts observations from KiCad 10.0.7. Requests without this selector retain their existing response shape. Inspection rechecks complete native/live geometry, editor identity, named nets, stackup, normalized PCB state and saved project/rule files. A changing, unsupported or oversized observation returns `status: "unavailable"` with a reason; it never returns a partial success.

```json
{
  "endpoint": "ipc:///tmp/kicad/api.sock",
  "geometry": true
}
```

`data.geometry.observation` contains the board identity, canonical copper layers, named net IDs, every observed zone, every board-level outline shape, complete definition and copper digests, saved-rule evidence and `evidence_digest`. `observed_revision` is the geometry request revision; `pcb_revision` matches the normalized `data.pcb.revision`. Both use the `sha256:` prefix. The evidence digest additionally covers saved rules and the complete returned observation. Saved-rule drift can change this evidence digest independently of the geometry-only revision. A read does not approve writes or permanently revoke a plan; plan approval requires its own drift handling.

Each zone retains its native UUID, actual live-text lock, net, layers, fill state/count and complete persistent native definition digest. Ordinary solid zones include authored polygon/holes and all editable parameters in the [typed geometry request format](geometry-request-compilation.md). Locked zones remain visible; `editable_solid` classifies the native form and does not override `lock_state`. Hatched fills, rule areas, curved/oversized polygons and unknown forms retain explicit unsupported reasons and native evidence. Full copper vertices remain internal; only counts and digests are returned. Unknown fields affect persistent digests even when they have no authoring representation.

Outline summaries describe segments, arcs, circles, rectangles and bounded straight polygons in exact nanometres. Unsupported forms remain explicit. The standard PCB snapshot includes `opaque_graphics` for non-segment objects on Dwgs.User and Edge.Cuts: source identity, layer and complete persistent native digest. These records participate in canonical source revisions and collision checks. Empty records are omitted for legacy serialization compatibility. Strict segment mutation/readback APIs still reject unsupported forms; reading a native shape does not make it editable through a legacy operation.

## Actual acceptance

Production stdio MCP acceptance passed on a CM5 Minima copy and the connected zone fixture. An independent KiCad Python reader verified all zone and outline identities, locks, net/layer mappings, fill flags/counts, every exposed solid-zone definition and every exposed outline coordinate/width against the saved native board. CM5 exposed 14 zones, 30 outline objects, 13 complete ordinary-solid definitions and one locked rule area. Its normalized snapshot retained all 14 non-segment Edge.Cuts objects as explicit opaque records, independently matched to native identities and layers. The fixture exposed three zones, 4 outlines, two solid definitions, one locked region and an explicitly unsupported hatched region.

Both runs verified opt-in schema behavior, stable repeated evidence, missing-endpoint/type refusal, saved-rule digest changes and exact evidence restoration after restoring the owned rule file. Original PCB/project/rule/schematic files and owned copy PCB/project bytes were unchanged. Each run reused one editor, closed its stdio server and editor, and removed the disposable working copy. The first CM5 attempt rejected because the legacy normalized graphics reader only accepted segments; the shared reader now retains non-segment objects explicitly. The failed attempt and cleanup record remain separate.

Full quality passed 446 tests with 15 environment-dependent tests ignored, formatting, warnings-denied Clippy and capability validation. Architecture tests cover opaque native retention, unknown fields, strict legacy rejection, duplicate identities, revision/legacy serialization behavior and unavailable-evidence responses. [Compact evidence](geometry-inspection.json) records actual profiles. This read capability does not establish geometry plan/preview/approval/commit, mixed geometry execution or the final product acceptance gate.

## Reproduction

```sh
env CARGO_TARGET_DIR=/private/tmp/auto-kicad-m4-target CARGO_INCREMENTAL=0 CARGO_PROFILE_DEV_DEBUG=0 CARGO_PROFILE_TEST_DEBUG=0 cargo build -p auto-kicad --bin auto-kicad --example check-geometry-inspection
/private/tmp/auto-kicad-m4-target/debug/examples/check-geometry-inspection /private/tmp/auto-kicad-m4-target/debug/auto-kicad '/Library/Application Support/kicad/demos/cm5_minima/CM5_MINIMA_3.kicad_pcb' /absolute/new-cm5-output
/private/tmp/auto-kicad-m4-target/debug/examples/check-geometry-inspection /private/tmp/auto-kicad-m4-target/debug/auto-kicad scripts/fixtures/geometry-zones.kicad_pcb /absolute/new-fixture-output
```

The example runs the stdio verifier and independent native reader automatically. Use fresh output directories. JSON observations and comparison records remain available after owned-editor cleanup. Native Python wx initialization diagnostics did not prevent the checks from completing.
