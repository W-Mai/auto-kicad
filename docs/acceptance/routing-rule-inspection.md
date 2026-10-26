# MCP routing-rule inspection

`kicad_inspect_project` accepts an optional `routing_rules` selector containing `net_ids`. Select 1 through 64 unique IDs from the current board observation. The server resolves IDs against actual named board nets and checks the editor token, document, native stackup, repeated net identity, complete live board observation and saved rule files. No rule-file path is accepted from the caller.

```json
{
  "endpoint": "ipc:///tmp/kicad/api.sock",
  "routing_rules": {
    "net_ids": ["<net_id from data.pcb.snapshot>"]
  }
}
```

Successful `data.routing_rules` has `status: "observed"` and an `observation` containing the ID-to-name map, `evidence`, `evidence_digest` and `observed_revision`. Both outer digest fields use the `sha256:` prefix; the revision matches `data.pcb.revision`. Evidence includes effective netclass preferences, saved global minima, complete project/custom-rule content digests and the observed stackup. Netclass track/via dimensions are preferences, while saved minima describe persisted constraints. Custom rules remain opaque evidence for KiCad DRC evaluation.

Malformed selectors return an MCP parameter error. An unknown/foreign net ID, malformed source file, unsupported backend or changing observation returns `data.routing_rules.status: "unavailable"` with a reason and no fabricated observation. Missing saved settings remain null; missing files have explicit presence evidence. Existing inspection requests without the selector retain their behavior. Inspection never saves editor settings or rewrites project files. It reports live preferences and saved minima separately and does not claim to reconcile unsaved rule settings.

## Reproduction

```sh
cargo build -p auto-kicad --bin auto-kicad --example check-routing-rule-inspection
target/debug/examples/check-routing-rule-inspection target/debug/auto-kicad /absolute/new-output
```

The probe opens one owned four-layer KiCad editor and launches the production server over stdio. It queries GND and VCC, checks five exact saved minima, distinguishes the 0.4 mm VCC preference from the 0.2 mm minimum, verifies stable repeated evidence, and rejects empty/duplicate/oversized/NUL-containing selections and missing endpoints. Unknown nets and duplicate project JSON keys return unavailable. Removing the owned project file reports absent minima; adding an owned custom-rule file changes the digest. Restoring exact bytes reproduces the original evidence. Source PCB/project bytes remain unchanged after the checks, and the owned server/editor and working copy are closed and cleaned.

Actual KiCad 10.0.7 stdio acceptance passed. Full workspace quality passed 388 tests with 15 environment-dependent tests ignored, plus format, Clippy warnings-as-errors and capability-matrix checks. The final binary passed a separate headless stdio capability/schema check. [Compact evidence](routing-rule-inspection.json) records the results. The synthetic fixture does not count toward the three-real-project gate. Route creation, plan/approval evidence binding and native preview reconciliation require separate acceptance.
