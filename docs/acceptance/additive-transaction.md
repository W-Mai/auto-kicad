# Caller-owned additive transactions

`BoardGraphicsExecutor::create_native_batch_in_transaction` validates and creates bounded graphics, tracks and through vias within a transaction owned by its caller. It checks creation receipts without beginning, committing, cancelling, saving, reloading or retrying. Errors can follow partial creation. The caller controls cancellation or uncertain-outcome handling and verifies the complete saved result. `execute_native_batch` delegates creation to the same method while retaining its transaction, saved readback and error handling.

## Native acceptance

KiCad 10.0.7 accepted three paths containing 9 tracks and 2 vias on F.Cu, In1.Cu and B.Cu. The creation trace contains exactly 11 CreateItems requests and no lifecycle requests. Saved PCB/project bytes remain unchanged until the caller commits and saves. Same-process reload, complete original-item and footprint preservation, native connectivity and DRC checks passed. Unconnected items decreased from 3 to 0, with no introduced DRC violations.

An injected failure before dispatch of the second creation leaves cancellation to the caller. Its attempted-request trace contains two CreateItems entries and no lifecycle requests; the first request reached KiCad. After explicit cancellation, complete original items and saved files match the baseline. This injection tests transaction ownership; it does not represent native zone-fill cancellation.

## Production regression

The production stdio route acceptance created 16 tracks and 4 vias through plan, preview, approval, commit and saved reload. Original native items and footprints survived. Custom-rule, saved-minimum and netclass drift invalidated approval; restoring rules did not restore approval. Invalid approval, replay and DRC-invalid route controls passed. No DRC violations were introduced by the accepted route batch. Owned editor cleanup and original-file isolation passed.

```sh
cargo build -p auto-kicad --bin auto-kicad --example check-native-route --example check-copper-route-mcp
target/debug/examples/check-native-route /absolute/new-native-output
target/debug/examples/check-copper-route-mcp target/debug/auto-kicad /absolute/new-mcp-output
```

Format, all-target workspace Clippy with warnings denied, the capability matrix and the full workspace suite passed: 475 tests, 0 failures, 15 environment-dependent tests ignored. The architecture test verifies the ownership boundary for success, invalid batches, partial writes, transport errors and mismatched receipts. [Evidence manifest](additive-transaction.json) binds saved boards, native payloads, diagnostic reports, transaction traces, source hashes, binaries and cleanup records.

This acceptance covers shared transaction creation and the existing zone-free route lifecycle. It does not establish additive routing on zoned boards, whole-board refill after routing, or completion of the product-delivery goal.
