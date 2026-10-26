# Approved PCB geometry over MCP

`kicad_plan_change` accepts an exclusive `geometry` request on KiCad 10.0.7. Segment/arc Edge.Cuts replacement and ordinary solid-zone create/update/delete use one native definition transaction, followed by separate whole-board refill, saved reload, independent complete copper verification, topology, pad-connectivity and DRC checks.

## Request and approval

Inspect with `kicad_inspect_project` and `geometry: true`. Use `data.geometry.observation.observed_revision` for the request revision; `data.pcb.revision` describes a different observation. Supply exact unlocked source UUIDs, named board-scoped net identities, enabled copper layers and explicit authored parameters. `geometry` cannot be combined with another top-level operation family. Unknown fields reject. Limits are 32 zone edits, 512 vertices per ring, 16 holes and 512 outline primitives; requests are bounded to 2 MiB.

```json
{
  "endpoint": "ipc:///tmp/kicad/api-EDITOR.sock",
  "source_run_id": "geometry-review",
  "request_id": "zone-clearance",
  "geometry": {
    "observed_revision": "sha256:OBSERVED_GEOMETRY_DIGEST",
    "outline": null,
    "zones": [{"kind": "update", "source_uuid": "OBSERVED_ZONE_UUID", "patch": {"clearance_nm": 160000}}],
    "refill": "all"
  }
}
```

Preview reconciles complete saved/native source content before mutation. Candidate evidence binds authored intent, complete native definitions, board text, filled copper, rules and the normalized observation revision. Only receipt-proven new UUIDs map between preview and source. Review the geometry and DRC evidence, then supply the exact plan, validation and approval hashes to `kicad_commit_change`. Repeated preview, stale source, wrong approval and commit replay reject. Detected source/rule changes permanently revoke approval, even after restoring the changed file.

Geometry commits require durable storage. Records retain complete candidate evidence and the canonical normalized observation revision; full observations remain in memory and in the ordinary preview response. PCB/project/rule backups remain byte exact. This avoids duplicating megabytes of snapshot data within the 4 MiB record bound. DRC correspondence compares every diagnostic field and counter, allowing only source paths, ordering and receipt-bound creation UUID differences.

## Uncertain outcomes

An unverified error after attempting native begin produces `unknown`; no retry, cancellation, reload or rollback follows that error. A project-local `.auto-kicad-geometry-pending` marker blocks further PCB and schematic writes, including another server with a different state directory. Verified commit or definite pre-write rejection clears only its matching marker. Unknown or interrupted execution retains it. Close project editors and use guarded recovery with fresh approval; only verified restoration of the matching attempt clears the marker. Do not remove the marker to retry an uncertain operation.

Cross-state-directory blocking, interrupted/unknown records, wrong-attempt refusal, matching recovery and terminal-record marker cleanup are architecture tests. Live accepted-send refill timeout evidence is recorded in [complete zone refill](complete-zone-refill.md). Actual GUI cancellation and interrupted geometry recovery are not claimed by this acceptance.

## Reproduction

```sh
cargo build --release -p auto-kicad --bin auto-kicad --example check-geometry-mcp
target/release/examples/check-geometry-mcp target/release/auto-kicad scripts/fixtures/geometry-zones.kicad_pcb scripts/fixtures/geometry-mixed-requests.json NEW_OUTPUT
```

The runner reuses one owned source editor and closes every disposable preview editor. It retains native boards, requests, production stdio calls, durable journals, independent Python field checks and cleanup records. Original project files stay unchanged. CM5 uses the unique-pad GUI-saved input documented in [candidate correspondence](geometry-candidate-correspondence.md); it does not repair production sources. Use the release build for populated verification. CM5 previews exceeded 180 seconds in both debug and release builds. Configure Codex `tool_timeout_sec = 600` for populated geometry verification.

## Evidence

The mixed fixture passed 4 stages covering curved/straight outline replacement, zone create/update/delete and outline-only changes. Complete candidate correspondence, no introduced DRC, preserved pad connectivity, wrong/stale approval rejection, rule-drift revocation, saved reload, successful pending-marker cleanup and independent authored-field checks passed.

CM5 passed production stdio preview and source execution with 112 footprints and 633 pads. Complete content/copper correspondence, 14 zone identities, 171 unchanged baseline DRC violations and zero unconnected items were verified. Retained failed attempts document the corrected recovery-record size limit and the asymmetric DRC UUID-list ordering comparison. Independent diagnosis found all 171 diagnostic payloads identical and 57 nonlexically ordered UUID identity lists.

Exact counts, digests, timings, source/preview PIDs, output locations and record sizes are in [the machine-readable record](geometry-mcp.json). This acceptance covers the exclusive geometry family. Teardrop regeneration, GUI cancellation, zoned additive routing, combined maximum-size MCP requests, general schematic editing, manufacturing release and complete product qualification remain unverified or unavailable.

Offline quality passed: 457 tests, 15 environment-dependent tests ignored, warning-denied workspace/all-target clippy, formatting and the 17-capability/13-tool matrix. The final release executable exposed the geometry schema and durable write capability over actual stdio. All recorded source and preview PIDs were verified absent after cleanup.
