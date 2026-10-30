# Footprint unit metadata protection

Existing footprint movement and rotation reject a selected footprint containing native `units` metadata before mutation. KiCad 10.0.7 full `FootprintInstance` updates cannot represent that metadata. Complete live board text supplies the support check, bound to repeated native observations, editor identity, document and format. Planning returns `footprint_transform_unavailable` with a precise units detail. Execution repeats the same gate before `BeginCommit`; adapters without native metadata support reject by default.

Only the selected footprint's direct metadata determines this restriction. A property named `units` is ordinary field content. Unit metadata on another footprint remains untouched and does not prevent supported moves. Missing/duplicate target identities and malformed footprint identity fields reject. Existing copper, lock, approval, complete native payload, source preservation, DRC and saved reload checks remain required. The qualified native-text backend requires KiCad 10.0.7. This gate does not add movement support for unit-bearing footprints or general schematic-to-PCB synchronization.

## Actual native and MCP evidence

An isolated two-footprint fixture exercises the installed runtime and the corrected executable through real stdio MCP. The installed executable accepts and commits an R1 movement while the saved `units` form disappears. The corrected executable refuses both R1 translation and rotation with byte-identical source. Two ordinary R2 translation/rotation commits preserve R1's complete two-unit form and introduce no DRC item or unconnected-item increase. One owned source editor is reused for both executable phases through native reload, then closed.

The reproducible complete movement fixture includes five footprints, a unit-bearing unlocked target, locked and connected controls, asymmetric front/back pads, a track and a through via. Three accepted transforms pass complete expected snapshots, exact approval, wrong-approval and replay rejection, source protection, DRC and same-process reload. Selected unit-bearing movement/rotation refuse without writes; ordinary transforms retain the exact unrelated unit form. The full native test passes in 58.42 seconds and closes its owned source/preview editors. These synthetic fixtures do not count as a real-project delivery profile.

Workspace quality passes 567 tests with zero failures and 16 environment-dependent tests ignored by default. The live movement test is executed separately and passes. Format, capability validation and all-target warnings-denied Clippy pass. Architectural contracts require exact target identity, native metadata qualification and rejection before beginning a mutation transaction. The [result record](footprint-unit-metadata.json) retains source/binary/evidence hashes and full DRC summaries. Full transcripts, native before/after boards, durable records and quality logs remain in local acceptance evidence.

## Reproduce

Use the existing ignored native lifecycle; it starts one owned source editor and serializes disposable previews. KiCad 10.0.7 and official native tools must be installed.

```sh
env CARGO_TARGET_DIR=/tmp/auto-kicad-move-target CARGO_INCREMENTAL=0 CARGO_PROFILE_DEV_DEBUG=0 CARGO_PROFILE_TEST_DEBUG=0 cargo build --locked -p auto-kicad --bin auto-kicad
env CARGO_TARGET_DIR=/tmp/auto-kicad-move-target CARGO_INCREMENTAL=0 CARGO_PROFILE_DEV_DEBUG=0 CARGO_PROFILE_TEST_DEBUG=0 AUTO_KICAD_MOVE_SERVER=/tmp/auto-kicad-move-target/debug/auto-kicad AUTO_KICAD_MOVE_EVIDENCE=/tmp/auto-kicad-move-result.json cargo test -p auto-kicad --lib tests::existing_footprint_acceptance::live_existing_footprint_stdio_lifecycle -- --ignored --exact --nocapture
```

Remove the task-owned target after its build/test processes exit. Preserve useful result evidence. The source target and supported-operation restrictions are unchanged by compiling or installing an executable; an existing long-lived MCP process must reconnect to use a replaced executable.
