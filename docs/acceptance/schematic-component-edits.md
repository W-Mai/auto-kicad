# Complete component reference and footprint edits

Existing `set_reference`, `assign_footprint`, Value `set_property` and `set_flags` intents operate on explicitly listed source units. An immutable index groups observed units by sheet path and component reference. The compiler projects the requested changes onto those complete groups and requires each touched shared field to have one consistent final value. Partial or conflicting edits reject before native dispatch. Unrelated custom properties and untouched shared fields remain exact.

Reference ownership belongs to the complete observed component. Every unit in a renamed component instance must retain one resulting reference; separate components cannot merge. Explicit complete-component swaps are supported. Global reservations preserve unrelated references across sheets. Reference changes retain source UUIDs, pin UUIDs and complete logical-pin membership; declared connectivity differences contain the actual network-name changes.

Use `single_instance` scope with the observed sheet path to rename one instance of a reused physical child. Supply the same new reference for every source unit in that component instance. Other instances remain unchanged. File-wide annotations update the displayed Reference and every observed current-project row. Instance-scoped annotations preserve the displayed field. Foreign-project and inactive annotations remain unchanged. Path matching uses the identity index's trailing-slash normalization without rewriting the source path tokens.

Footprint compatibility uses canonical active pins from the exact observed embedded definitions. It includes common pins once per component and expands supported stacked pin numbers. Inactive body styles and inactive stored pin records do not grant mapping authority. Each affected instance of a physical target must have the same complete component pin universe. The mapping must bijectively match that universe and the actual distinct numbered library pads. Native assignment preserves literal pin/pad numbers; nonidentity mappings reject because a Footprint field cannot persist pin renumbering. Catalog definitions, pad numbers and digests remain server-derived. Component membership insertion/deletion and shared-field changes require separate plans.

The Rust `allocate_references` helper resolves an anchor to complete observed components, reserves globally distinct references and emits explicit member edits. Reused sources receive instance-scoped annotations. Compatible repeated requests for one component coalesce; conflicting parameters and expansions beyond 128 edits reject. The current MCP intents accept explicit reference assignments.

```json
{
  "edits": [
    {"file":"design.kicad_sch","scope":{"kind":"file_all_instances"},"action":{"kind":"set_reference","uuid":"OBSERVED_UNIT_A_UUID","reference":"U20"}},
    {"file":"design.kicad_sch","scope":{"kind":"file_all_instances"},"action":{"kind":"set_reference","uuid":"OBSERVED_UNIT_B_UUID","reference":"U20"}}
  ]
}
```

Place this explicit edit array inside the existing versioned `general` request. [Bounded schematic MCP](schematic-general-mcp.md) retains observation, exact native expectations, approval, durable publication and actual-source verification. These edits preserve component membership; [unit creation/insertion](schematic-multiunit.md) describes membership-changing operations separately.

## Reproducible native qualification

```text
python3 scripts/check-schematic-component-edits.py SERVER OWNED_SCHEMATIC OWNED_CONFIG NEW_OUTPUT
python3 scripts/check-schematic-component-edits.py SERVER OWNED_ROOT_SCHEMATIC OWNED_CONFIG NEW_OUTPUT --file channel_strip.kicad_sch
```

The driver requires marked copies with prepared complete `U9910` and `U9930` component groups, common-pin wiring and the matching installed pin-header libraries. Fixtures include foreign-project and inactive annotation records, custom fields and path normalization controls. It checks explicit two-/three-unit annotation, complete footprint and Value changes, consistent flags/restoration, reference swaps, partial/conflicting/colliding requests, common-pin mapping, source drift/replay, unchanged source identities and protected metadata, full native network/ERC correspondence, independent saved native grouping and configuration/private-directory cleanup. Independent exports use separate configuration and temporary directories. No GUI is launched.

Hierarchy/child-file authoring, complete GUI/Codex qualification, PCB synchronization and the remaining product gates remain separate requirements. Automatic annotation exposure through MCP requires separate qualification from the Rust helper.

Actual KiCad 10.0.7 production-stdio qualification passed for both schematic formats and the child reused in 4 instances: 27 committed component edits, 108 rehashed publication artifacts and independent saved native grouping/ERC. Every profile used the same service binary hash. All changed ERC items were independently bound to the explicitly targeted physical pin UUIDs; unrelated diagnostics remain unchanged. Complete quality passed 556 tests with zero failures and 16 environment-dependent tests ignored. The actual fixed CLI/MCP regression passed in 4.23 seconds.

A separate real stdio interruption/recovery regression killed only its owned writer after source publication, retained in-flight/missing post-attempt evidence, blocked fresh writes, protected later user bytes, restored the exact original files/native observation and accepted a subsequent normal commit. Its 7 publication artifacts were rehashed. A prior CLI planning timeout and a recovery-test sentinel collision remain marked separately; neither is counted as passing acceptance. [Machine-readable acceptance](schematic-component-edits.json) records the scope and evidence.
