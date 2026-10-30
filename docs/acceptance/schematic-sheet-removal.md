# Exact schematic placement removal

`remove_sheet` removes an observed hierarchical sheet placement by its physical parent file and source UUID. It retains the child file, its embedded definitions, instance annotations and every other source byte outside the removed placement form. Shared child files remain available to surviving instances. Removing the last placement leaves an orphan file on disk; the tool does not delete it.

```json
{
  "file": "design.kicad_sch",
  "scope": {"kind": "file_all_instances"},
  "action": {"kind": "remove_sheet", "uuid": "OBSERVED_PLACEMENT_UUID"}
}
```

Place this edit inside the versioned `general` request in [bounded schematic MCP](schematic-general-mcp.md). Multiple explicit placements can be removed together. A reused physical parent requires `file_all_instances`, and every affected parent instance is listed in the plan. Generic object deletion does not grant placement removal authority. Repeated/overlapping edits reject. A surviving sibling can take a removed sibling's name through an explicit `update_sheet` in the same final projection.

The candidate retains complete physical source bytes while its parsed hierarchy and artifact revision describe only files reachable from the final root. One shared bounded source graph expander handles observed identities and compiled candidates, retaining cycle, depth, physical-file and expanded-instance checks. A batch that would also change a detached file rejects because native observation cannot verify that edit's semantics. Subsequent observations omit orphan objects while retaining the physical file on disk.

Removal can disconnect surviving parent wiring, remove component/virtual pin instances and resolve or introduce diagnostics. Default preservation rejects these changes. Declare complete network, pin and isolated-state transitions plus exact ERC diagnostics and removed/added header strings through [explicit expectations](schematic-expectations.md). Physical markers in retained child files stay unchanged. Preview and publication continue to verify the originally captured child bytes even if the candidate no longer reaches them; child drift revokes approval permanently. Actual-source native hierarchy, netlist, full ERC and source-pin connectivity must match before durable completion.

## Native qualification

```text
python3 scripts/check-schematic-sheet-removal.py SERVER OWNED_ROOT_SCHEMATIC OWNED_CONFIG NEW_OUTPUT
python3 scripts/check-schematic-sheet-removal.py SERVER OWNED_ROOT_SCHEMATIC OWNED_CONFIG NEW_OUTPUT --file channel_strip.kicad_sch --shared
python3 scripts/check-schematic-general-interruption.py SERVER OWNED_ROOT_SCHEMATIC OWNED_CONFIG NEW_OUTPUT --sheet-name CH2 --remove-sheet
```

Actual KiCad 10.0.7 production stdio passed formats 20250114 and 20260306 plus a physical parent reused in 4 instances. The root profiles remove one shared-child placement and then the remaining placements. The nested profile removes 4 expanded empty-sheet instances through one physical placement edit. All 5 commits pass independent saved native component-network and complete ERC comparison, exact whole-file subtraction of only the requested source forms, retained child/annotation/configuration bytes, source-drift permanence, wrong approval, replay and cleanup. All 20 publication artifacts were rehashed.

Real removal-publication interruption leaves missing terminal/post-attempt evidence and blocks new writes. Later unrelated bytes reject recovery. A fresh recovery approval restores the exact original source/native observation, and a subsequent normal removal commits. Its 7 retained artifacts were rehashed. Complete quality passed 562 tests with zero failures and 16 environment-dependent tests ignored. The fixed CLI/MCP regression passed in 5.93 seconds. No GUI was launched. [Machine-readable acceptance](schematic-sheet-removal.json) records the evidence.

[Child creation/attachment/retargeting](schematic-children.md) use explicit page/reference maps and exact source/absence authority. Full GUI/Codex/family qualification and the remaining product gates remain incomplete. Native research candidates remain separate from public API acceptance.
