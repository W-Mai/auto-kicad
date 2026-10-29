# Native publication of matched schematic candidates

`MatchedSchematicPreview::prepare_publication` performs a one-way transition from ready to publishing and returns a non-cloneable `SchematicPublicationCheck`. Every retained preview or matching handle refuses reuse. Before mutation, the check requires original source bytes, private snapshots, configuration, selected catalog evidence and executable context to remain exact. After mutation, only compiled candidate source bytes are permitted; all other bound inputs remain unchanged, and the complete parsed hierarchy must equal the candidate.

`SchematicPublicationCheck::verify_published` executes a complete audit on the actual published sources under the original server-owned resource policy. Native component/net records, full ERC and source-pin connectivity must equal the matched native candidate. Inputs are checked again after native observation. Only this path constructs `VerifiedSchematicPublication`; its private fields prevent request payloads from supplying a verification proof. The proof retains the actual-source audit and rechecks candidate/context evidence before completion. Source or protected-input drift permanently invalidates it.

`ProjectWriter::publish_matched_schematic` composes the check with durable per-file publication. It requires the writer's canonical project to match, persists candidate source copies and complete preview/matching/verification JSON, then records `committed` only after exact file readback and native proof validation. Artifact references retain paths, sizes and digests; artifacts are bounded to 128 MiB each and 256 MiB total. Failures before durable preparation reject. Preparation ambiguity, source-write failure, failed final validation or terminal persistence ambiguity remains `unknown` with recoverable evidence. Later user source bytes cannot gain recovery authority from an uncertain completion.

KiCad 10.0.7 native acceptance passed these owned-copy cases:

| Case | Actual published changes | Native result |
|---|---|---|
| Multichannel properties | 2 physical files, 5 affected instances | 80 source-pin networks; baseline 4 errors/32 warnings preserved |
| ECC83 explicit wire | Resistor pin contact connection | Independently expected GND merge, 13 to 12 networks; zero ERC diagnostics |
| ECC83 catalog symbol | `Device:R` inserted as R900 | 2 declared pin births and named disconnected networks, 13 to 15 networks; exactly 2 explicitly declared unconnected-pin errors |

Wrong-project writers and stale source/configuration/project-library bytes reject before durable preparation. Consumed previews and replayed publication calls reject without further changes. Retained artifacts were rehashed from disk, and an independent final audit matched the preview in every case. Exact candidate bytes, unchanged protected configuration and final-owner private-directory cleanup passed. No GUI opened. The created resistor intentionally remains disconnected; this case does not establish a clean manufacturing result.

An explicitly run native failure test covers lost final-verification acknowledgement, changed configuration after writes and later user source edits. Every case retains Unknown, blocks ordinary writes and rejects consumed preview reuse. Recovery refuses the later user bytes; the owned control restores the exact candidate before requesting a fresh approved recovery. The complete original native baseline and private-directory cleanup pass afterward. This uses private test callbacks and the backend recovery API; it does not simulate a power failure or qualify public general-edit stdio transport.

```sh
cargo build -p auto-kicad --example check-native-schematic-publication
target/debug/examples/check-native-schematic-publication OWNED_SCHEMATIC OWNED_CONFIG NEW_OUTPUT properties
target/debug/examples/check-native-schematic-publication OWNED_ECC83 OWNED_CONFIG NEW_OUTPUT wire
target/debug/examples/check-native-schematic-publication OWNED_ECC83 OWNED_CONFIG NEW_OUTPUT symbol
```

The ignored failure test is `recovery::native_publication::tests::native_final_failures_remain_unknown_and_preserve_later_user_content`. It requires `AUTO_KICAD_NATIVE_PUBLICATION_SOURCE`, `AUTO_KICAD_NATIVE_PUBLICATION_CONFIG` and a new `AUTO_KICAD_NATIVE_PUBLICATION_OUTPUT` directory, using owned acceptance markers. Full offline quality passed 544 tests with zero failures and 16 environment-dependent tests ignored; the native failure test and existing real CLI/MCP regression run separately. [Machine-readable acceptance](schematic-native-publication.json) records scope and retained evidence.

This backend supports matched candidates changing existing schematic files, including symbol insertion. [Bounded schematic MCP](schematic-general-mcp.md) binds these server-owned candidates to observation sessions, typed intent and exact approval, with [source-bound inherited symbol creation](schematic-symbol-inheritance.md). Hierarchy/child-file creation, GUI/Codex lifecycle and full M5a acceptance remain incomplete. Arbitrary hashes supplied to backend orchestration do not constitute human approval.
