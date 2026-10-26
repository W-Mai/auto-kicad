# Native routing rule observation

`KiCadClient::get_effective_netclass_rules` reads the exact requested effective netclass set from the connected PCB editor. It checks the instance token and the complete matching board/project identity before and after the headerless query. Unknown/missing/extra net names, token drift, document changes, malformed values and out-of-bound requests reject without adopting a different instance. Requests are bounded to 64 unique nonempty net names of at most 4096 bytes each. Complete native class payloads remain available as evidence, including unsupported and unknown fields.

The returned class name, implicit/explicit status and constituents describe the effective class, which can combine several configured classes. Clearance and preferred track width remain optional when the corresponding native settings are absent. Uniform via diameter/drill preferences are exposed when both dimensions can be established. These preferred dimensions are not hard global manufacturing limits, and this reader does not evaluate custom DRC rules.

`kicad_adapter::routing_rules::observe_routing_rules` combines saved global minima, complete project/custom-rule content digests, observed stackup and effective native classes. Each adjacent source is limited to 8 MiB and must be a normal file; Unix reads reject symlink/FIFO replacement and compare file identity before and after reading. Project JSON rejects duplicate keys anywhere, limits depth to 32 and nodes to 100,000, and converts decimal millimetres exactly to nanometres. Sub-nanometre rounding, negative or oversized constraints reject. Missing files/settings remain explicit absence rather than assumed defaults.

Observation checks the bound instance, full source document identity, repeated native class/stackup values and repeated saved-file evidence. The digest includes source identity; `equivalent_preview_rules` compares content and native rule context while excluding relocated paths/document identity. This comparison does not itself load a native preview or reconcile unsaved settings.

## Reproduction

```sh
cargo build -p kicad-adapter --example check-routing-rules
target/debug/examples/check-routing-rules /absolute/new-output
```

The macOS probe opens one owned four-layer PCB fixture, reads `GND` and `VCC` rules repeatedly, checks native evidence stability and refuses an unknown net. A transport wrapper changes owned saved files during real IPC reads to verify project-content drift and custom-rule presence drift rejection. The probe restores exact original bytes, verifies stable restored evidence, closes the owned editor and removes working copies. The original fixture, combined JSON evidence and native protobuf payloads remain in the output directory.

On KiCad 10.0.7, `GND` returned the explicit `Default` class with 0.25 mm preferred width and 0.2 mm clearance. `VCC` returned an implicit class containing `Power` and `Default`, with 0.4 mm preferred width, 0.3 mm clearance and 0.8/0.4 mm preferred via/drill sizes. 4 enabled copper layers, stable instance, unknown-net refusal, repeated native evidence, source preservation and owned-editor cleanup passed. See [compact evidence](routing-rule-observation.json).

Saved minimum clearance, track width, via diameter, annular width and through-hole diameter were 0.1, 0.2, 0.5, 0.1 and 0.2 mm respectively. Both injected mid-observation file changes were rejected; each changed stable snapshot produced a different digest and failed content equivalence. Restoring bytes reproduced the original combined evidence. The relocation check uses copied saved files and relocated in-memory native evidence; it does not establish native preview equivalence.

This evidence establishes read-only rule observation and saved-file drift detection. Public MCP inspection is covered by [its stdio acceptance](routing-rule-inspection.md). Plan/approval binding, native preview reconciliation and configurable-layer route integration require separate implementation and acceptance. Custom rules remain opaque content evidence for KiCad DRC evaluation.
