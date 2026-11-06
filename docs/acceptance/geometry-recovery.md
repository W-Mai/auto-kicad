# Geometry uncertainty and guarded recovery

Production stdio on KiCad 10.0.7 rejects further writes after uncertain geometry execution and permits approved recovery of the recorded source bytes. An acceptance-only NNG relay forwards unchanged requests to an owned PCB Editor and introduces one explicitly armed fault. No production fault switch or replacement executor is used.

## Verified cases

| Fault | Native and durable outcome | Recovery |
| --- | --- | --- |
| Corrupt one successful `SaveDocument` response | KiCad applied the zone clearance edit and saved it; the server reported `unknown`, retained post-attempt fingerprints and left the pending marker | Exact recorded PCB/project backups restored after closing the owned editor |
| Hold a read-only `GetVersion` reply after the pending marker exists, then terminate the owned MCP process | No native mutation was dispatched after the marker; the journal remained `in_flight`, without post-attempt fingerprints; source bytes equalled backups | Approved restoration of unchanged bytes cleared the matching marker |

Both cases rejected new approved drawing commits from the originating state directory and from another state directory. Drawing probes deliberately avoid refilling the incomplete geometry: a refill can reintroduce an isolated-copper warning that disappeared when the definition edit invalidated the fill. The actual writer-marker refusal, unchanged source bytes and retained marker were checked separately from DRC rejection. No source IPC command followed the injected fault on the original connection.

Recovery planning rejected the open editor. After the runner closed and waited for its exact owned process, it removed only that disposable board/project's stale locks. The recovery tools never remove locks. Wrong approval, approval reuse across a server restart and replay after restoration all rejected. Fresh approval restored exact backup bytes, preserved execution history and cleared the matching marker. Reopening the restored copy passed complete normalized snapshot, native definitions, filled copper and full DRC comparisons. A normal approved geometry commit then passed and its saved result survived same-process reload.

## Reproduction

```sh
cargo build --release -p auto-kicad --bin auto-kicad --example check-geometry-recovery -p kicad-ipc --example geometry-fault-relay
target/release/examples/check-geometry-recovery target/release/auto-kicad target/release/examples/geometry-fault-relay scripts/fixtures/geometry-zones.kicad_pcb NEW_OUTPUT
```

The runner reuses the source editor until recovery requires closure, then reopens the same copy. Preview editors are disposable and closed by their existing lifecycle. It retains raw fault requests/native replies, command transcripts, journal/backup files, recovery approval transcripts, fault-state boards and the final native result. Only owned test processes are terminated. Original source files remain unchanged.

## Evidence and limits

The recorded run completed both fault/recovery cases and the normal commit/reload, then its final independent reader rejected a missing `kicad_version` field in the acceptance JSON. The runner's failed exit and original result files are retained. The field is included by the corrected script. A separate read-only native check verified the retained fault and normal PCB clearances at 210000 nm, the interrupted board's exact baseline bytes, backup digests, fault command sequence and native outcome version. It passed without reopening KiCad or rewriting the failed records. The complete evidence is the native run plus this retained-artifact check; the original runner is not reported as having exited successfully.

[The machine-readable record](geometry-recovery.json) contains artifact digests, actual outcomes, process identities and the retained read-only verification. Offline quality passed 457 tests with 15 environment tests ignored, formatting and warning-denied workspace/all-target clippy. All recorded owned processes were verified absent.

This covers controlled protocol corruption after a real native save and actual process termination before mutation. It does not establish GUI fill cancellation, arbitrary native crashes, or safe restoration of modified files without post-attempt fingerprints. Those files remain rejected by guarded recovery. Teardrop regeneration, zoned additive routing and combined maximum/populated geometry qualification are separate requirements.

[Complete routed-board qualification](final-real-projects.md) covers actual Codex transport, successful native SaveDocument response loss, approved native recovery and a subsequent normal plane edit on the installed Sonde Xilinx design. The shared dimension/text identity ownership check preserves its native dimension while independent collisions reject.
