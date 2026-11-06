# Placement and routing replay

`auto-kicad-replay` places components one at a time on an owned PCB copy, then adds simple deterministic routes. It keeps one PCB Editor process for placement, routing and saved reload, and closes it afterward. Replay is a local utility with its own input and routing limits; it does not use MCP plans or approvals.

## Build and run

Run from the repository root with KiCad and its bundled Python runtime installed on macOS:

```sh
env CARGO_INCREMENTAL=0 cargo build --release --locked -p auto-kicad --bins
python3 scripts/install-local.py target/release/auto-kicad --replay-binary target/release/auto-kicad-replay --no-rollback
~/.local/bin/auto-kicad-replay /absolute/input.kicad_pcb /absolute/new-output-directory 250
```

The input must be an unrouted front-side SMT board without zones, with matching `.kicad_pro` settings. The output directory must be new. The last argument is the pause between steps in milliseconds: 0–5000, default 250. Ctrl+C cancels playback and closes the owned editor. Input files and placement coordinates are preserved.

## Routing and output

Routing sorts same-net pad pairs by distance and tries straight, orthogonal and 45-degree paths. It rejects foreign-net pad and track collisions. Candidates use 0.2 mm track width and clearance plus a geometric margin, then pass native DRC before creation. This algorithm does not handle zones, vias, differential pairs, impedance or power-current requirements, and does not optimize placement.

Final checks compare pad geometry and nets, saved/reloaded observations, route counts, source hashes and introduced DRC violations. Board drawings outside Edge.Cuts are hidden in the replay copy; DRC comparisons use that prepared baseline.

| Output | Contents |
| --- | --- |
| `replay.html` | Interactive playback and seeking, with executed geometry embedded |
| `events.jsonl`, `route-plan.json` | Executed events and planned routes |
| DRC reports, `summary.json` | Baseline, candidate and final diagnostics, counts and verification results |
| `result/` | Saved native PCB and project |

The HTML opens directly without a server. Working copies are removed after success; retained results and the installed utility do not depend on build output.

## Recorded pet-collar run

Two replays produced the same route geometry: 38 components, 201 preserved pads, 58 connections and 80 copper segments. Unconnected items decreased from 101 to 43 with no introduced DRC violations. Existing violations remained. The result is partially routed and requires further electrical and layout review before manufacturing.
