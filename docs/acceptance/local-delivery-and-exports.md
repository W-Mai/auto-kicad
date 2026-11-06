# Local delivery and native exports

The macOS arm64 runtime supports KiCad 10.0.7 and local installation. No public binary distribution is configured. `auto-kicad --version`, `--build-info` and `--help` report runtime metadata; invocation without arguments serves MCP on standard input/output.

## Installation

```sh
python3 scripts/install-local.py /path/to/auto-kicad
python3 scripts/install-local.py --status
python3 scripts/install-local.py --verify
python3 scripts/install-local.py --rollback
python3 scripts/install-local.py --uninstall
```

`--prefix` selects an isolated installation root. Existing launcher paths remain stable. Immutable releases under `lib/auto-kicad/versions/` contain the executable, checksums, build information and dependency/license inventory. The atomic `current` symlink selects a release. Installation retains the active and previous release by default; rollback validates checksums before switching. Install with `--no-rollback` to retain only the active verified release and prune older owned releases. This flag accepts an installation source and rejects lifecycle actions; rollback is unavailable until a later installation retains a previous release. The known legacy layout is adopted after exact layout and launcher verification. Optional `--replay-binary` installs the separate replay utility and an existing replay executable is carried forward during upgrade.

Verification rejects changed binaries, launchers, manifests and unrecognized release files. Uninstall preserves modified or unrecognized files, all CAD projects, execution/recovery state and Codex registration. Installation records dependency licenses and a local-only distribution decision; this does not grant public redistribution rights. Interrupted activation uses the atomic pointer as its commit point. Incomplete unregistered installer staging directories are retained for inspection.

An isolated-prefix acceptance installed the accepted region runtime, upgraded to the immutable-owner runtime with `--no-rollback`, verified one retained release and exact executable hashes, repeated the installation idempotently, rejected incompatible lifecycle action combinations, and uninstalled. Project, recovery, user configuration and unrecognized-file sentinels remained byte-identical; the global installation was untouched.

## Export requests

Close project editors and use saved documents. `kicad_inspect_project` accepts an exclusive `artifact_export` observation:

```json
{"artifact_export":{"kind":"observe","project_root":"/absolute/project","root_schematic":"/absolute/project/design.kicad_sch"}}
```

Use the returned observation identity and revision to generate private native artifacts:

```json
{"source_run_id":"export-review","request_id":"fabrication","artifact_export":{"schema_version":1,"observation_id":"<observation_id>","observed_revision":"<revision>","output_dir":"manufacturing","formats":["gerber","drill","bom","schematic_pdf"]}}
```

This request is passed to `kicad_plan_change`. `output_dir` must be a new relative project directory whose parent already exists. Traversal, symlinks, occupied destinations and overwrites reject. Planning runs the actual native exporters and returns the complete candidate `files` inventory with relative paths, formats, byte counts and SHA256 digests. `native_preview.evidence` refers to complete private native/source evidence. Planning does not publish artifacts.

Call `kicad_preview_change` with `plan_id`, `plan_hash` and `artifact_export: {"files": <the complete returned inventory>}`. Commit uses the returned `approval_hash` and `validation_evidence_hash` with the same plan identity. Source/resource drift revokes approval. Approvals expire on restart. Commit fsyncs private durable candidates and a sibling stage, then exclusively renames the complete directory into place. Published bytes must exactly match the approved inventory; CAD inputs remain unchanged.

Gerber exports enabled copper layers plus front/back mask, silkscreen and Edge.Cuts with X2 attributes and six-digit precision. Drill exports separate plated and non-plated Excellon files in millimetres using absolute origin. BOM exports ungrouped references, values, footprint identifiers, quantity and DNP fields, excluding BOM-disabled and DNP symbols; captured source flags and component membership are verified. The ascending reference order uses the native default because explicit `--sort-asc true` triggers a KiCad 10.0.7 parameter exception. PDF exports the complete hierarchy in monochrome with no background or author/subject metadata. Custom external font faces reject. Export commands use their own captured private configuration because KiCad persists export preferences.

Version 1 supports 4 observations and 4 live plans per server with a 900-second lifetime. Limits are 512 native files, 64 MiB per artifact and 256 MiB total including the publication manifest; the saved PCB uses the existing 32 MiB source-input bound. Output path depth is bounded. Native exports describe saved design contents and do not certify electrical correctness, zone-fill freshness or manufacturing readiness.

## Recovery and resource ownership

`artifact_export: {"kind":"history","project_root":"/absolute/project"}` reads export records. `kind: "release"` accepts `observation_ids` and `plan_ids` to release private sessions. An uncertain publication requires `kicad_plan_recovery` with its `artifact-export-attempt:` identity and `action: "finalize_export"` or `"retain_artifacts"`, followed by the existing preview/commit recovery tools and fresh approval. Finalization requires exact source/resource/environment authority and the recorded artifact inventory. Retention preserves partial artifacts and later user bytes. Recovery errors after dispatch report an uncertain outcome and require fresh recovery; they cannot be reported as a pre-write rejection. Export recovery never restores CAD files. An independent export journal domain preserves existing CAD recovery encodings.

Registered native temporary directories carry an ownership token, inode/device identity and inherited open-file-description leases. Registration publishes a complete fsynced owner record exclusively. Its JSON payload remains immutable; release records an exact token in an atomic extended attribute. Native workers inherit read-only descriptors with shared leases, and reclamation requires an exclusive lease. Legacy released records remain readable; a foreign release marker or malformed record refuses reclamation. Startup and shutdown cleanup preserve live workers, changed directory identities, symlinks and unrecognized resources. Only registered released or dead-owner resources without live leases can be reclaimed. Ownership does not change inherited environment variables or older CAD recovery fingerprints. Cleanup status is exposed in `kicad_capabilities.native_temporary_cleanup`.

## Verification evidence

Owned-copy MCP acceptance generated all 4 formats and verified exact inventory approval, wrong-approval rejection, replay rejection, occupied-output preservation, source-drift revocation and unchanged original CAD/configuration. Actual writer SIGKILL at a staged publication passed fresh restarted approval, later-user-file protection, exact candidate finalization and terminal-marker recovery. A live KiCad CLI worker retained two leased input directories across parent SIGKILL; those directories were reclaimed after the worker exited. The harness keeps the process group from becoming orphaned while the worker is stopped. A separate actual bundled Python/pcbnew 10.0.7 worker verified its inherited read-only descriptor, loaded three footprints and one copper object, and saved after its Rust owner released the directory. Cleanup retained the live worker lease, then reclaimed the directory and record after worker exit; owner JSON stayed byte-identical. Full workspace quality passed 628 tests with 18 environment-dependent tests ignored, plus strict all-target Clippy. Earlier malformed owner records remain archived; their exact triggering condition is unproven, and automatic repair is not claimed.

Actual Codex app-server acceptance published 13 files from the intact RoyalBlue54L-NFC-Antenna copy. Independent output inspection verified one PDF page, the two copper layers, one BOM component and both physical drill hits against native positions, diameters and plating. Original CAD and configuration hashes remained unchanged. These are export checks; the broader product milestones and three-project final gate remain separate.

```sh
python3 scripts/check-artifact-exports.py --binary /path/to/auto-kicad --project /path/to/source-project --schematic design.kicad_sch --config /path/to/config-home --output /path/to/new-acceptance-directory
```

Add `--codex` to run the same export through the Codex app-server transport. That transport owns child-process cleanup and can remove abandoned private trees; it does not establish automatic server cleanup. Automatic ownership cleanup is qualified separately by the actual native-worker crash test described above.

[Native export isolation and multi-unit BOM qualification](native-export-isolation.md) cover an installed local candidate, actual Codex exports of the complete ECC83 pair, a reused-sheet BOM/PDF, sealed native preferences and persistent source dependency checks after private cleanup. Source-bound exports require a consistent complete component definition; mixed per-unit cached library definitions reject.

[Native GUI and active local runtime qualification](native-gui-qualification.md) bind the export-qualified executable to actual schematic/PCB saves, production fill cancellation/recovery and a fresh registered Codex connection. Release `0.1.0-d15c5bd75e557d1b` retains one verified payload without rollback storage.
