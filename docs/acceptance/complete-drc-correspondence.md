# Complete geometry DRC evidence

Geometry preview and source execution retain the complete KiCad JSON report. Candidate correspondence checks violations, unconnected diagnostics, schematic parity, ignored checks, coordinates, severities and unknown fields. Only top-level source/date, diagnostic/item ordering and receipt-proven created-item UUIDs may differ. Array multiplicity remains significant; unknown arrays retain their order.

The immutable `CompleteDrcReport` contains a normalized summary and the exact `raw_json` string. The string survives intermediate MCP JSON values and durable journal serialization without rounding numeric tokens. The normalized summary remains the input for violation deltas and the existing preview summary. It cannot prove candidate correspondence by itself: an unconnected endpoint can change without changing the count.

Parsing rejects duplicate keys at every depth, malformed known diagnostic sections, more than 128 levels of nesting and input larger than 32 MiB. Identity maps must be bounded and bijective and cannot merge a reported original identity with a created item. Callers must establish the map from native receipts before comparison. The shared CLI runner retains path confinement, fixed arguments, bounded reports and timeouts. Geometry checks the report's source filename and KiCad version against its bound observation.

## Reproduction

Build with one temporary target, disabled incremental compilation and debug information:

```sh
env CARGO_TARGET_DIR=/private/tmp/auto-kicad-m4-target CARGO_INCREMENTAL=0 CARGO_PROFILE_DEV_DEBUG=0 CARGO_PROFILE_TEST_DEBUG=0 CARGO_PROFILE_RELEASE_DEBUG=0 CARGO_BUILD_JOBS=1 cargo build --release -p auto-kicad --bin auto-kicad --example check-geometry-mcp
/private/tmp/auto-kicad-m4-target/release/examples/check-geometry-mcp /private/tmp/auto-kicad-m4-target/release/auto-kicad scripts/fixtures/geometry-zones.kicad_pcb scripts/fixtures/geometry-mixed-requests.json NEW_MIXED_OUTPUT
/private/tmp/auto-kicad-m4-target/release/examples/check-geometry-mcp /private/tmp/auto-kicad-m4-target/release/auto-kicad SETTLED_CM5_INPUT scripts/fixtures/geometry-cm5-candidate-request.json NEW_CM5_OUTPUT
```

The existing [CM5 preparation](geometry-candidate-correspondence.md) supplies a settled owned input with unique pad identities and original library context. Acceptance independently compares every raw diagnostic field in Python, checks matching complete reports in durable journals, enforces the 4 MiB record bound, verifies actual native edits and reload, and closes its owned editors.

## Evidence

KiCad 10.0.7 production stdio acceptance passed 4 mixed outline/zone commits and one populated CM5 clearance edit. The mixed profile retained 5 baseline violations and 2 unconnected diagnostics at every stage. CM5 retained 171 baseline violations and zero unconnected diagnostics. Independent Python comparison covered complete raw reports and receipt mappings, and durable records retained the exact report strings. The largest record was 1,903,085 bytes, below 4 MiB. Saved native intent, complete copper, same-process reload, source isolation and owned-editor cleanup passed.

A retained real-report probe compared 4 actual CLI reports with 10 generated-region identity mappings. Changing an unconnected endpoint while keeping the count fixed left the legacy normalized summary equal but made complete correspondence fail. Repeated reports matched; reports with unmapped generated identities differed. The probe source and results are archived in the evidence directory; the temporary Cargo example was removed.

Full quality passed: 466 tests, zero failures, 15 environment-dependent tests ignored, formatting, warning-denied workspace/all-target clippy and the capability matrix. All 9 recorded source/preview editor PIDs were verified absent. Native acceptance preceded two behavior-preserving lint rewrites; full quality covers the final source. Exact paths, candidate digests, report counts, binary hash, record sizes and artifact hashes are in [the machine-readable evidence](complete-drc-correspondence.json).

This acceptance establishes complete DRC comparison for the existing geometry pipeline. Generated-zone MCP composition, private DRC project/library context, GUI cancellation, zoned routing and the remaining product delivery gates remain open.
