# Restoring an interrupted edit

Recovery restores recorded byte backups after an uncertain PCB or schematic execution. Close project editors and stop external writers before restoration. KiCad lock files block recovery; these tools never remove them.

Recovery does not reconcile unsaved editor memory, reconstruct missing backups, reroute copper or synchronize a schematic and PCB. Recovery refuses to overwrite source changes made after the recorded attempt.

## Contract

`kicad_plan_recovery` accepts `project_dir`, `attempt_id` and `action: "restore_backups"`. It returns `recovery_id`, `recovery_plan_hash`, exact source/backup digests and `state: "prepared"`. `kicad_preview_recovery` accepts that identity and hash and returns `approval_hash`. `kicad_commit_recovery` requires all three values and returns `restored`, `rejected` or `unknown`. Failed calls have `ok: false`. These tools are advertised only with a durable state directory.

Execution completion records source fingerprints while holding an exclusive writer lock. Recovery rejects files changed after that evidence, including edits made before recovery planning. An interrupted or legacy execution without fingerprints or publication intent can recover only if every source still equals its backup. [Durable publication intent](schematic-publication-recovery.md) permits exact recorded before/candidate states after interrupted schematic writes and includes exact staging cleanup. Missing, redirected or altered sources/backups are rejected. Paths must identify canonical regular files inside its source directory and the originating attempt's backup directory. Source and backup checks occur during preparation, preview and commit; source files are checked again before each rename, and every final file is hashed after the last write.

Existing-file recovery records use schema 2 in a separate `recoveries` namespace. Execution schema 1 remains readable; legacy recovery schema 1 remains inspectable but cannot execute. Mixed schematic executions use schema 2 with `created_sources`; their recovery records use schema 3. Schematic execution schema 3 adds complete publication intent; recovery schema 4 adds `staging_files`. Absent publication fields and empty file lists are omitted, preserving existing record encoding and plan hashes. Original execution records are never rewritten by recovery. Inspection returns execution and recovery histories separately. Plans are bound to both their immutable contents and the live server session. Restarted services require new plans and approvals. Consumed approvals cannot execute again.

Mixed schematic recovery restores existing backups and removes schematic files proven absent before the attempt. Each new path has an explicit `absent` or `present` state with a digest, a confined canonical existing parent and no overlap with existing sources. Plans expose every removal through `created_sources` and `created_sources_restore_to: {"kind": "absent"}`. Changed files, symlinks, collisions and missing parents reject. Missing post-attempt evidence permits continued absence only; recovery refuses to infer ownership of a file created during an unobserved interruption. [Native and stdio acceptance](schematic-recovery.md) qualifies this recovery foundation separately from general source publication.

Schematic attempts persist `.auto-kicad-schematic-pending` before mutation. Pending schematic or geometry recovery blocks ordinary project writes across state directories. Only the owning execution can release its marker after a terminal outcome or successful guarded recovery. The persistent writer lock and pending marker serve different purposes: releasing the process lock does not release uncertain execution state.

Each file replacement is atomic. The set of replacements is not atomic. A partial restore, uncertain readback or journal failure reports `unknown`, consumes the approval and leaves the original backup available. An interrupted journal is also reported as `unknown` on inspection. No automatic rollback or retry occurs. The writer lock excludes other auto-kicad writers; it does not lock out arbitrary filesystem writers.

When KiCad is closed or unavailable, `kicad_inspect_project` accepts `endpoint: ""` with `project_dir` and returns `observation: "durable_history_only"`, execution history and recovery history. It returns no live revision or normalized CAD snapshot in this mode. This allows inspection after a crash without reopening a board before recovery.

## Reproduce

Run the offline recovery contracts:

```sh
cargo test -p auto-kicad recovery --lib
```

The live test requires the installed KiCad PCB Editor and CLI. It uses a disposable PCB containing footprints/pads and runs a real native mutation through the MCP router. The uncertain outcome is a test-only lost-receipt injection after the native commit and save. Production code has no environment-controlled failure switch.

```sh
cargo build -p auto-kicad --bin auto-kicad
AUTO_KICAD_RECOVERY_SERVER="$PWD/target/debug/auto-kicad" \
AUTO_KICAD_RECOVERY_EVIDENCE=/tmp/auto-kicad-recovery-evidence.json \
cargo test -p auto-kicad tests::recovery_acceptance::live_recovery_restores_native_board_and_allows_next_mcp_commit --lib -- --ignored --exact --nocapture
```

Set `AUTO_KICAD_RECOVERY_SOURCE` to a supported existing `.kicad_pcb` with a matching `.kicad_pro` to test an owned copy. Source files are compared before and after and remain unchanged. With `AUTO_KICAD_RECOVERY_SERVER`, restoration uses the actual stdio executable via `scripts/check-recovery.py`; that script verifies wrong approvals, process restart, fresh approval, exact restored bytes and replay rejection. Without it, the same recovery tools run through an in-process MCP transport. The live test then opens the restored board in KiCad, compares observed objects and DRC, completes another normal MCP commit, reloads it and checks DRC again. Only owned editor processes and fixtures are cleaned.

## Evidence scope

On 2026-10-26, KiCad 10.0.6 passed the complete native/MCP recovery test on the disposable fixture and on a populated pet-collar PCB copy. The populated-board run used the actual stdio executable for recovery, including server restart and closed-document history inspection. Exact restored bytes, observed footprints/routing/stackup, restored DRC, subsequent normal MCP commit and owned-resource cleanup passed. The board retained 49 baseline DRC violations and 43 unconnected items, with zero introduced violations. These are preserved pre-existing issues, not a clean manufacturing result. The [machine-readable result](guarded-recovery-result.json) records scope, checks and tested source hashes. Passing recovery contracts does not establish support for all KiCad shapes or completion of the complete editing toolset. The xNAS trial on KiCad 10.0.6 failed before recovery because the existing graphics observer accepts segments only (`invalid_observed_board_graphic: board graphic must be a segment`). That project is not a passing acceptance case.

[Geometry recovery acceptance](geometry-recovery.md) on KiCad 10.0.7 exercises the project-local pending marker with an actual saved-response fault and an interrupted MCP process. Both originating and alternate state-directory writes reject until matching approved recovery. Complete native content, copper and DRC are checked after reopening. The record distinguishes native execution evidence from the separately completed read-only artifact check.
