# General footprint source contract

Source-bound placement resolves exact `nickname:entry` identifiers from local KiCad project/global library tables, including nested tables and project aliases. Saved schematic reference, value, footprint field, sheet path and exported pin nets must match the request. Creation remains front-side at zero rotation with at most 128 physical pads. Native preview, exact approval, no introduced DRC and no increased unconnected count remain mandatory.

## Source configuration

`AUTO_KICAD_LIBRARY_CONFIG` optionally names an absolute JSON configuration file. Settings and their digest are captured once at server startup. Changes require a server restart. Project/global table contents, their presence, canonical paths and footprint bytes are checked again during planning, preview and execution. A rejected source-bound execution invalidates its previous approval.

```json
{
  "version": 1,
  "global_table": "/absolute/config/fp-lib-table",
  "extra_roots": ["/absolute/approved-libraries"],
  "path_variables": {"COMPANY_PARTS": "/absolute/approved-libraries"}
}
```

All fields except `version` are optional. The default global table location uses the connected KiCad major version and its platform configuration directory. Only `KIPRJMOD`, the matching `KICAD<major>_FOOTPRINT_DIR`, and explicitly configured variables expand. Configuration rejects duplicate/unknown keys, relative paths and `KIPRJMOD` overrides. An extra root grants local path access; a variable alone does not. Global native library entries and the installed official library directory also define allowed roots. Project entries take precedence. Disabled entries reserve names and never fall back. Hidden entries allow exact lookup.

Existing `AUTO_KICAD_OFFICIAL_FOOTPRINT_ROOT` and `AUTO_KICAD_PROJECT_FOOTPRINT_ROOT` overrides retain their three legacy mappings. Without explicit overrides, those mappings are fallbacks only for absent nicknames. Source resolution is bounded to 1 MiB per table, 8 MiB total table content, 64 visits, 8 inclusion levels, 4096 rows and 8 MiB per footprint. Final footprint symlinks, escaped paths, ambiguous tables, unknown formats/options and unresolved variables reject.

## Inspection and approval

```json
{
  "endpoint": "ipc:///absolute/kicad/api.sock",
  "footprint_source": {
    "library_nickname": "Package_SO",
    "library_entry_name": "SOIC-8_3.9x4.9mm_P1.27mm"
  }
}
```

Pass this request to `kicad_inspect_project`. The response adds exact source evidence or an unavailable reason; `geometry_verified: false` explicitly separates successful lookup from placement support. Requests accept no arbitrary library path. Existing `footprint` and `assembly` request shapes are unchanged. Plan responses include source provenance, and plan hashes bind resolution evidence. Old audit records remain readable; records without resolution evidence cannot authorize a new placement.

Preview writes a disposable library table pointing to approved canonical directories so KiCad DRC sees project aliases. User configuration remains unchanged and libraries are not copied. The embedded helper accepts alias names independently of directory basenames, verifies exact source size/digest and preserves numbered and unnumbered physical pads.

## Native preservation

KiCad regenerates child UUIDs during footprint creation. The client proves a unique correspondence from complete native child payloads, omitting assigned identity, validated read-only parent, deprecated net codes and an exactly empty symbol-pin message. It rejects ambiguous identical payloads or duplicate IDs rather than pairing by pad number/position. Other unknown fields remain exact. Repeated physical pad numbers map to one unambiguous schematic pin; unnumbered apertures carry no net. Full native data and returned identities are checked after save and preview reload.

Source-bound creation requires KiCad 10.0.7 fabrication-property support. The fixed helper records every physical pad UUID and its exact manufacturing attribute, verifies the staged save/reload, and compares the complete map with the native IPC template. Missing, unknown or altered properties reject. The native creation receipt and saved reload must preserve the full template. The pinned protocol includes fabrication properties, teardrops, zone overrides, solder-mask fields and embedded files; this does not advertise editing support for those additional object types. See [protocol provenance](../../crates/kicad-ipc/PROTO-PROVENANCE.md).

## Reproduction and evidence

```sh
cargo build -p auto-kicad --bin auto-kicad --example check-general-footprints
target/debug/examples/check-general-footprints target/debug/auto-kicad /absolute/new-output
```

The macOS acceptance requires installed KiCad libraries, bundled Python and CLI. It creates an owned fixture, reuses one source editor, serializes preview editors and closes owned editors on completion. The fixture declares a 0.2 mm minimum through-hole diameter before testing. It checks installed NE555D SOIC-8 placement, a project alias whose directory has another name, an atomic IC/resistor connection, source/table drift, wrong approval, invalidated approval reuse, saved/reloaded native payloads and protected project/schematic/table bytes. The result retains the native project, its fixture libraries and JSON evidence.

The official `Package_DFN_QFN:QFN-32-1EP_5x5mm_P0.5mm_EP3.65x3.65mm_ThermalVias` source is loaded without modification. Its 52 physical pads include 9 unnumbered apertures, 11 pad-33 instances and 11 heatsink properties. All survive native creation and full saved reload. A separately named property-free `LocalQFN` fixture remains an additional correspondence check.

Recorded acceptance on KiCad 10.0.7 created 6 footprints, 130 pads and one connected track, with zero introduced DRC violations and zero unconnected items. Exact table/source drift and stale approval rejection passed. Complete native reload, protected files and owned-editor cleanup passed. See [compact evidence](general-footprint-sources.json).

Offline validation passed the capability matrix, formatting, warnings-denied Clippy and 379 tests with 15 environment-dependent tests ignored. Seven explicit real KiCad regressions passed: connected assembly with rollback, source-bound resistor placement, mixed graphics/routing/via creation, footprint movement/rotation, footprint flips, existing item field edits and exact-set deletion. These native fixtures do not complete the separate three-real-project acceptance gate.
