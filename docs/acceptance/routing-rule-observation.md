# Native routing rule observation

`KiCadClient::get_effective_netclass_rules` reads the exact requested effective netclass set from the connected PCB editor. It checks the instance token and the complete matching board/project identity before and after the headerless query. Unknown/missing/extra net names, token drift, document changes, malformed values and out-of-bound requests reject without adopting a different instance. Requests are bounded to 64 unique nonempty net names of at most 4096 bytes each. Complete native class payloads remain available as evidence, including unsupported and unknown fields.

The returned class name, implicit/explicit status and constituents describe the effective class, which can combine several configured classes. Clearance and preferred track width remain optional when the corresponding native settings are absent. Uniform via diameter/drill preferences are exposed when both dimensions can be established. These preferred dimensions are not hard global manufacturing limits, and this reader does not evaluate custom DRC rules.

## Reproduction

```sh
cargo build -p kicad-adapter --example check-routing-rules
target/debug/examples/check-routing-rules /absolute/new-output
```

The macOS probe opens one owned four-layer PCB fixture, reads `GND` and `VCC` rules twice, checks native evidence stability, refuses an unknown net, verifies unchanged board/project bytes and closes the owned editor. The original fixture and native protobuf evidence are retained in the output directory.

On KiCad 10.0.7, `GND` returned the explicit `Default` class with 0.25 mm preferred width and 0.2 mm clearance. `VCC` returned an implicit class containing `Power` and `Default`, with 0.4 mm preferred width, 0.3 mm clearance and 0.8/0.4 mm preferred via/drill sizes. 4 enabled copper layers, stable instance, unknown-net refusal, repeated native evidence, source preservation and owned-editor cleanup passed. See [compact evidence](routing-rule-observation.json).

This evidence establishes native read-only observation. Saved global constraint decoding, public MCP rule inspection, approval-bound rule evidence and configurable-layer route creation require separate implementation and acceptance. No additional routing capability is advertised by this reader.
