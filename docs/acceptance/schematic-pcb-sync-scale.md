# Synchronization size and batch limits

Closed schematic-to-PCB synchronization supports a 128 MiB saved PCB, 128 component actions and 4096 explicit copper net assignments in one request on KiCad 10.0.7. Source, project, rule, response, native deadline and durable publication limits still apply together. A smaller input can reject when its complete public differences or expectations exceed the 4 MiB message bound.

| Reader | PCB input | Endpoint cap | Native report |
| --- | --- | --- | --- |
| Existing live connectivity | 32 MiB | 4096 | 2 MiB |
| Closed synchronization connectivity | 128 MiB | 100000 | 64 MiB |

Project inputs retain the 32 MiB bound. Private execution captures cap retained originals, candidates and native copies at 1 GiB; the source-input and 256 MiB publication budgets remain separate and stricter. These internal caps do not qualify arbitrary public endpoint counts or every combination of source hierarchy, template and geometry maxima.

## Complete proof outside large responses

Small plan responses retain their existing fields. When native proof detail makes a plan exceed 4 MiB, `native_preview` retains scalar summary fields and returns `evidence_externalized: true` plus `complete_evidence: {path, bytes, sha256}`. The local JSON file contains the complete original native proof, bounded to 64 MiB and bound to the preview digest. Public differences and DRC, region and class expectations remain complete.

The plan owns this artifact. Preview and commit verify its exact path, regular-file type, size and digest; drift terminally revokes the plan. Consumption, release and expiry reclaim the owned artifact after inherited native leases end. Reading the file grants no approval, source authority or write scope. Approval still uses the existing exact expectations and session-bound hashes.

## Native preservation

Private input capture, generated/authored geometry parsing, region ownership, refill, saved readback and recovery use the same fixed closed-board profile. Original legacy worker bytes and smaller live-reader bounds remain unchanged. Large region capture selects a fixed wrapper around the unchanged native region reader; recovery accepts only the known exact worker identities.

Native SaveBoard can reorder independent direct segments, arcs and vias after a net change. A comparison-only UUID ordering retains each complete record, nested geometry order and unknown token; the approved candidate keeps original source spans. Every non-route source record remains protected. Same-input publication and recovery DRC verification reuse the complete closed physical-component comparison already used for repeated native captures. Source-versus-candidate diagnostic expectations remain exact.

## Qualified controls

Actual Codex app-server acceptance committed a 128 MiB synthetic input with 128 component updates and 4096 copper assignments: 4602 exact differences, 4355 native electrical endpoints and 4097 retained copper objects. Wrong approval, replay, action/copper overflow and a 128 MiB plus one byte input reject. Complete externalized proof hashes match, and the consumed proof file is reclaimed. Original schematic, settings, configuration, manual geometry and unselected source bytes remain unchanged.

An independent source reader verifies every approved field value, 14 unselected root records, 23176 retained selected-item child pairs and their order, complete trailing source bytes, six durable artifacts and 11 archived native reports. These are preservation controls on a synthetic maximum-batch fixture, not a final real-project profile.

A 64 MiB populated region control passes actual writer SIGKILL after publication, fresh cold recovery, later-user-byte protection, complete native baseline restoration and a subsequent commit. A separate accepted 018f283 writer hands its interrupted region journal to this runtime for actual native recovery and subsequent publication. Both controls use stable native execution environments. Recovery rejects environment drift even with fresh approval; unknown outcomes and their backups must be retained when original authority cannot be re-established.

Full workspace quality passes 633 tests with 18 environment-dependent tests ignored, plus strict all-target Clippy, formatting and capability-matrix checks. Machine-readable receipts are recorded in [schematic-pcb-sync-scale.json](schematic-pcb-sync-scale.json). Broader source/template maxima, custom rules, remaining families, GUI and the complete real-project gates remain unqualified.
