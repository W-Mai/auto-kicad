# Populated copper-route acceptance

The saved PawChrona prototype passed explicit multilayer routing through production stdio MCP and KiCad 10.0.7. The original PCB has 38 footprints, 201 pads, 80 tracks and 4 copper layers. An owned copy connects the SIM_VDD decoupling capacitor C6 pad 1 to SIM socket J3 pad 1 with 3 new tracks and 2 through vias. The path runs from `(79.49, 99.5)` mm to `(78.9, 99.5)` mm on F.Cu, down to `(78.9, 102.5)` mm on In1.Cu, then to `(80, 102.5)` mm on F.Cu. Track width is 0.20 mm; via diameter/drill are 0.60/0.30 mm.

Unconnected items decrease from 43 to 42. The 49 baseline DRC violations remain, with zero introduced violations. The final board has 83 tracks and 2 vias. Complete native preservation checks in route execution, independent normalized comparison, protected project/rules/schematic bytes and same-process saved reload pass. The original source files remain unchanged. The result verifies one useful connection on a populated prototype; it does not establish complete board routing or manufacturing readiness. See [compact evidence](populated-copper-route.json) and the [route API contract](copper-route-mcp.md).

The acceptance also changes the owned live editor from 4 to 6 copper layers without saving. Every original enabled layer remains present; only empty inner layers are added. The public observation and revision change while saved PCB bytes remain identical. Commit rejects the previously approved plan. Reloading the saved copy restores the complete original observation in the same editor, but the revoked approval still rejects. A new plan then completes preview, approval, native commit and saved verification. The auxiliary `owned-stackup-drift` executable requires an acceptance-owned `work` directory, exact open-document path and editor-token evidence; it does not expose stackup mutation through MCP.

## Reproduction

```sh
cargo build -p auto-kicad -p kicad-ipc --bin auto-kicad --example check-route-copy --example owned-stackup-drift
target/debug/examples/check-route-copy SOURCE.kicad_pcb /absolute/auto-kicad NEW_OUTPUT_DIR /absolute/owned-stackup-drift scripts/fixtures/pawchrona-sim-vdd-route.json
```

The profile supplies explicit physical reference/pad selectors and exact geometry. The checker resolves current native pad identities, requires matching observed nets and fails on ambiguity. Use the retained original PawChrona source for this profile. Other boards need their own reviewed route profile; the checker does not search alternative paths or alter constraints. It requires the final unconnected count to decrease, preserves all unrelated normalized content and relies on full native route checks and real DRC for acceptance.

Successful output contains before/result PCB and project files, the route request, complete MCP transcript, baseline/final DRC, durable execution records, before/after snapshots, stackup-drift evidence and cleanup records. One owned source editor is reused throughout; disposable previews are serialized and closed. The result files remain available after all working copies and owned editors are removed. The installed Codex MCP executable is unchanged; the built production executable was exercised through stdio. The final three-profile Codex delivery gate remains separate.

## Verification and recorded failures

The initial candidate at x=78.5 mm failed native board-edge clearance: its via edge was 0.20 mm from the board edge against the existing 0.50 mm requirement. No live-source route was committed. Moving the explicit corridor to x=78.9 mm satisfied the same constraint. Another attempt correctly rejected unsaved stackup drift but its acceptance helper passed a full path to a basename-based reload command; the helper was corrected and complete restoration/reapproval acceptance passed. Both failed outputs retain their logs and successful owned-editor cleanup records.

The complete offline quality gate passes 400 tests, with 15 environment-dependent tests ignored, plus format, Clippy warnings-as-errors and capability-matrix checks. Route runtime code is unchanged from the [accepted synthetic MCP delivery](copper-route-mcp.md), which also passed seven real editing/creation regressions. The [native path acceptance](native-copper-routes.md) records actual partial-create cancellation; adapter architecture tests cover save failures and incomplete or mismatched receipt/readback outcomes. No live process crash is claimed.
