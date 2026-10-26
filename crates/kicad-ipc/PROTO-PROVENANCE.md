# KiCad IPC protobuf provenance

- Upstream repository: `https://gitlab.com/kicad/code/kicad.git`
- Release tag: `10.0.7`
- Resolved commit: `69cafb907ebc09e7722a2a464b1d466fc2e3608d`
- Generator: `protobuf-codegen 3.7.2`
- Source license: KiCad source distribution; each vendored file retains its upstream license header.
- Distribution note: the vendored KiCad proto files are GPL-3.0-or-later source artifacts. Release packaging and generated-code distribution require an explicit license review before publication.

Vendored files and SHA-256:

| File | SHA-256 |
|---|---|
| `proto/board/board.proto` | `128e9429cbd5446d71cf3b69943754c051f7337ee9b3986c92aedc2ee686f4d3` |
| `proto/board/board_commands.proto` | `662c14f4a701fa542c906e75e0a3ab4f2b5d4cdc116c178c6b0e39c5a9acb716` |
| `proto/board/board_types.proto` | `04e160be8f90a1267fc58792982838d012b058ebc452ee703cd9a2db871369ed` |
| `proto/common/commands/base_commands.proto` | `44ee37af7b9533653d6d5a28fee89d37b1e42207baecd1a6f2e97287d9d79bff` |
| `proto/common/commands/editor_commands.proto` | `194a0028c3be57a79f1b57da702856d910c9c20537de013c43e9967785642f3f` |
| `proto/common/envelope.proto` | `fc2d727416a2e79d131d0ae56f0965ef2b7441b365df9e718828ac355e2cc8c6` |
| `proto/common/types/base_types.proto` | `a4b1cc365cce89ee4b9823f985ae41d1e677dbe34636243ee53f365ffd47462e` |
| `proto/common/types/embedded_files.proto` | `595d9c2a2be963709f8302a7cce877f0756bc71da6736b0cfb91e8d02d606868` |
| `proto/common/types/enums.proto` | `18338434b703f96844cc40ea964382ebf5a4d949d8678324768557d64e6bdacd` |
| `proto/common/types/project_settings.proto` | `4552846e15cc07645a681034026018b4fb1186f39ac0b9a8cb0c2c893226b0ca` |

KiCad 10.0.6 adds read-only parent identities to PCB objects. Headerless board graphic responses require a stable instance token and a fresh matching open-document query. Pad observation reads the exact footprint definition by UUID. Unknown fields and explicit document or parent mismatches remain errors.

KiCad 10.0.7 adds pad fabrication properties, pad/via teardrops and zone overrides, solder-mask overrides, footprint embedded files and variants. Complete native payload comparisons preserve these fields. The embedded-files dependency is included in the pinned import closure.
