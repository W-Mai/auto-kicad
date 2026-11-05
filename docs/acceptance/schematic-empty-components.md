# Pinless symbols and footprints

`SchematicPinIndex::component_pin_numbers` returns the complete active pin-number set for an exact ordered component-unit identity. A verified empty set is retained alongside ordinary nonempty sets. Missing or partial identity groups return no authority. The source/native join checks every unit's pin metadata against the embedded definition and accounts for every native net node before returning the index.

Catalog symbol creation supports a selected pinless body when that body exists and the entire captured definition has no pins. A missing body, invalid unit/style, or an empty active body in an otherwise pinful definition rejects. Native export, complete component identity, exact connectivity/ERC expectations and source approval remain required. No library-name or reference-prefix convention grants pin authority.

Footprint assignment compares complete component pin sets with distinct numbered library pads. Empty sets form an exact empty bijection. Every reused instance must have the same complete pin universe. Unnumbered physical pads remain physical objects; an empty numbered-pad set does not authorize dropping them. Missing identity, nonempty caller maps for empty sets, and numbered `MP`, `SH` or switch-pad mismatches reject.

Explicit PCB `add`, `replace` and `remove` use the complete source-bound native template and publication workflow. Both footprints without physical pads and mounting holes with unnumbered NPTH pads are supported. Replacement requires exact physical pad correspondence; a one-pad mounting hole cannot silently become a zero-pad graphic. Source paths, allocated child identities, drill apertures, placement, side, native serialization, surviving copper partitions and full DRC/ERC evidence remain checked. Removal retains its existing lock, zone, external-reference and physical-contact restrictions.

KiCad 10.0.7 acceptance covers actual source creation and assignment, PCB addition, disconnected replacement, source deletion and exact PCB removal on native 20250114 and 20260306 source pairs. Front and oblique back-side mounting holes preserve complete physical pads; zero-pad 5mm-to-6mm logo replacement preserves complete native graphics. Independent native physical readers and complete English DRC reports match the approved candidates. Every original PCB root node, original copper object and unselected source node remains protected.

Actual Codex app-server calls exercise the oblique back-side lifecycle. Actual writer SIGKILL during addition and replacement requires fresh recovery approval, preserves later user bytes, verifies the original native baseline and permits a subsequent approved commit. Source/library drift permanently revokes assignment approval. Ordinary two-pin creation and nonempty assignment also pass. The intact NFC antenna copy retains a new M4 installation hole and every original copper object; that useful operation does not complete the three-project final gate.

```text
python3 scripts/check-schematic-pinless-lifecycle.py SERVER OWNED_SOURCE OWNED_CONFIG NEW_OUTPUT
python3 scripts/check-schematic-pinless-lifecycle.py SERVER OWNED_SOURCE OWNED_CONFIG NEW_OUTPUT --profile back-oblique
python3 scripts/check-schematic-pinless-lifecycle.py SERVER OWNED_SOURCE OWNED_CONFIG NEW_OUTPUT --profile no-pad
python3 scripts/check-codex-schematic.py pinless SERVER OWNED_SOURCE OWNED_CONFIG NEW_OUTPUT --profile back-oblique
```

Inputs require ownership markers and an already synchronized source/PCB pair. The driver copies inputs, opens no PCB Editor and retains the native results. `--profile legacy-front` requires an actual 20250114 source pair; changing only a modern source version token does not produce a synchronized legacy fixture. `--position-nm X Y` selects the explicit fixture placement. `--profile real` retains its newly authored mounting hole instead of deleting it. Installed macOS KiCad 10.0.7 libraries and its bundled Python runtime are required.

`scripts/check-schematic-pcb-sync-zero-pin.py` retains a native-prepared mounting-hole metadata/relink control. It checks duplicate-reference Add rejection, native replacement preview, source Value publication and matching PCB Value publication. It is separate from public source creation acceptance.

Historical PIC, ECC83 and multichannel mixer records retain their original runtime hashes and scope. PIC's numbered shield pads remain a full-synchronization conflict; its read-only empty-component observation is not a complete PCB publication. Earlier driver failures and their independently resumed postcondition seals are recorded explicitly. Owner cleanup after Codex termination is separate from automatic server cleanup. [Machine-readable evidence](schematic-empty-components.json) retains historical and current workflow receipts. M5a, M5b and the complete product remain unfinished.
