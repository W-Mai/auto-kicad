# Geometry library context

Geometry previews retain the complete effective native footprint table, including aliases, unavailable libraries, disabled and hidden leaf rows, plugin types, options and descriptions. Project rows retain precedence over global rows. `${KIPRJMOD}` URIs bind to the original canonical project directory; other native variables remain intact. Table provenance is revalidated before and after rendering. Rendered tables are bounded to 1 MiB. Unclassified project-variable syntax and paths that would introduce another native expansion reject.

Disabled nested table containers contribute no library names. Disabled leaf rows still hide same-named global libraries. Read-only nested tables apply the `libraries.overrides` flags from the native `kicad.json`. Effective flag changes, native flag-setting changes and table changes invalidate the catalog. The server uses `KICAD_CONFIG_HOME` when present and otherwise selects the platform's versioned KiCad configuration directory. Source resolution keeps its canonical-root and bounded-file constraints. These behaviors are exercised against KiCad 10.0.7 and its [library manager](https://raw.githubusercontent.com/KiCad/kicad-source-mirror/10.0.7/common/libraries/library_manager.cpp) and [settings implementation](https://raw.githubusercontent.com/KiCad/kicad-source-mirror/10.0.7/common/settings/kicad_settings.cpp).

## Reproduction

```sh
env CARGO_TARGET_DIR=/private/tmp/auto-kicad-m4-target CARGO_INCREMENTAL=0 CARGO_PROFILE_DEV_DEBUG=0 CARGO_PROFILE_TEST_DEBUG=0 CARGO_PROFILE_RELEASE_DEBUG=0 CARGO_BUILD_JOBS=1 cargo build --release -p auto-kicad -p kicad-adapter --bin auto-kicad --example check-geometry-mcp --example check-library-context
/private/tmp/auto-kicad-m4-target/release/examples/check-library-context SOURCE_PCB NATIVE_CONFIG_DIRECTORY/fp-lib-table OFFICIAL_FOOTPRINT_ROOT NEW_DRC_OUTPUT
/private/tmp/auto-kicad-m4-target/release/examples/check-geometry-mcp /private/tmp/auto-kicad-m4-target/release/auto-kicad SETTLED_CM5_INPUT scripts/fixtures/geometry-cm5-candidate-request.json NEW_MCP_OUTPUT
```

The closed check reads native flag overrides beside the supplied global table. Use the same native configuration for the CLI and checker; `KICAD_CONFIG_HOME` selects a configuration base whose `10.0` directory contains both files. The checker retains exact raw reports, table evidence and input hashes, then compares every diagnostic field except source/date and diagnostic/item ordering. Source board/project/rules remain unchanged. The MCP runner verifies approval, native editing, saved reload, independent saved intent, complete report correspondence, durable records and owned-editor cleanup.

## Evidence

Seven closed DRC pairs passed: missing project override, disabled project override, hidden alias, disabled nested container, disabled unsupported-format library, read-only settings override under an isolated native configuration, and populated CM5. Every pair matched complete reports in Rust and independently in Python. CM5 retained 171 violations. The missing-library control retained two library errors; deliberately omitting its project table removed those errors by exposing the global library, proving the previous preview behavior changed validation context.

The initial disabled-container control failed with two extra library errors. Its source/private reports remain archived. The final implementation passed that control with two matching dangling-track diagnostics. A separate context probe found that copying CM5's board/project/table without its libraries added 112 errors and that `--define-var KIPRJMOD=…` did not fix library resolution; relocating the URI restored full report equality.

Production stdio acceptance passed a missing-library clearance edit and a CM5 clearance edit. The missing-library profile retained 7 violations, including 2 library errors, and 2 unconnected diagnostics. CM5 retained 171 violations and zero unconnected diagnostics. Both verified exact complete preview/source DRC correspondence, saved reload, protected inputs, rule-drift approval revocation, replay refusal and durable raw reports. Independent native readers verified the saved edits. All recorded source and preview editor PIDs were absent after cleanup.

Full quality passed: 469 tests, zero failures, 15 environment-dependent tests ignored, formatting, warning-denied workspace/all-target clippy and the capability matrix. Exact profile paths, binary hash, counts, process identities, rejected reports and artifact hashes are recorded in [the evidence manifest](geometry-library-context.json).

This evidence covers library-table preservation and the existing ordinary geometry MCP path. It does not establish complete private generated DRC source/environment provenance or public generated-zone MCP execution. Those gates, GUI cancellation, zoned routing, combined geometry checks and the remaining product-delivery requirements remain open.
