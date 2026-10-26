# Native existing-item protocol

`kicad-ipc` exposes instance-bound `FlipItems`, `DeleteItems` and pad connectivity queries. These primitives require a previously identified editor and reject response tokens that differ from that editor. They validate document bindings, item types, identities, statuses and response cardinality. Callers must supply their own edit policy, native transaction, independent saved-state verification and recovery handling. These primitives do not expose new MCP editing operations.

Deletion accepts 1–100 unique explicit identities. A complete successful native item-status set produces `ExactItemStatuses`. KiCad 10.0.6 returns an empty item-result list even for successful deletion; only that exact tested version permits `NeedsSavedVerification`. Neither receipt proves saved removal. Other versions with missing results reject. Unknown, duplicate, extra, missing or failed results reject. A failed or ambiguous response after mutation still requires transaction/recovery handling by the caller.

Connectivity queries partition the supplied complete set of electrical pad identities into canonical native copper clusters. Unknown, duplicate, overlapping or inconsistent members reject. The observation is bounded to 4096 pads and a 30-second query-loop deadline, with transport timeouts bounding individual requests. Connectivity preservation must compare the connected pad pairs before and after an edit; a global unconnected-item count alone cannot establish preservation.

## Native verification

The [result record](existing-item-protocol-result.json) records the 2026-10-26 KiCad 10.0.6 run. One owned editor was reused for four flips, native transaction discard, committed deletion and saved reloads, then closed and cleaned. Both flip axes completed a round trip on a back-side footprint with an asymmetric pad. Each flip preserved footprint/pad identities and nets; round trips preserved complete native footprint payloads. Pad clusters remained one connected pair and one isolated pad. Discarding deletion of the connected track restored the complete normalized snapshot; committing deletion of the drawing segment removed exactly that segment from the normalized snapshot. Input fixture bytes remained unchanged.

This probe uses native IPC directly. It does not establish MCP plan/preview/approval/commit behavior, independent single-flip geometry preservation, general object deletion safety, DRC preservation or real-project acceptance. The offline workspace quality gate passed with 355 tests and 12 environment-dependent tests ignored; protocol contracts cover malformed replies and foreign identities. The ignored native probe passed separately in 5.32 seconds.

## Reproduce

Installed KiCad PCB Editor is required. The test creates and owns its editor and temporary copy. Remove the temporary target after the command exits:

```sh
env CARGO_TARGET_DIR=/tmp/auto-kicad-protocol-target CARGO_INCREMENTAL=0 CARGO_PROFILE_DEV_DEBUG=0 CARGO_PROFILE_TEST_DEBUG=0 AUTO_KICAD_EXISTING_PROTOCOL_EVIDENCE=/tmp/auto-kicad-protocol-result.json cargo test -p auto-kicad --lib tests::existing_item_protocol::live_existing_item_protocol_uses_one_editor -- --ignored --exact --nocapture
```
