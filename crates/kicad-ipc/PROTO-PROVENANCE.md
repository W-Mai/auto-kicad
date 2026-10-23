# KiCad IPC protobuf provenance

- Upstream repository: `https://gitlab.com/kicad/code/kicad.git`
- Release tag: `10.0.6`
- Resolved commit: `caf7377e9cb6fa1535ec3596dcb8c99bf44a996e`
- Generator: `protobuf-codegen 3.7.2`
- Source license: KiCad source distribution; each vendored file retains its upstream license header.
- Distribution note: the vendored KiCad proto files are GPL-3.0-or-later source artifacts. Release packaging and generated-code distribution require an explicit license review before publication.

Vendored files and SHA-256:

| File | SHA-256 |
|---|---|
| `proto/common/envelope.proto` | `fc2d727416a2e79d131d0ae56f0965ef2b7441b365df9e718828ac355e2cc8c6` |
| `proto/common/commands/base_commands.proto` | `44ee37af7b9533653d6d5a28fee89d37b1e42207baecd1a6f2e97287d9d79bff` |
| `proto/common/commands/editor_commands.proto` | `efdfdf569c867b4e9dbef5edb0da22ca5546bb09bf5908825219057d8fe198f7` |
| `proto/common/types/base_types.proto` | `875d10d36a49aee52bfd16234e553b34a92f68240cecc087ce6721827046e4f2` |
| `proto/common/types/enums.proto` | `ee9c1ac5fdc61d2f012a1f8bfc358358115423b79c05023903021f7665523f47` |
| `proto/common/types/project_settings.proto` | `4552846e15cc07645a681034026018b4fb1186f39ac0b9a8cb0c2c893226b0ca` |
| `proto/board/board.proto` | `128e9429cbd5446d71cf3b69943754c051f7337ee9b3986c92aedc2ee686f4d3` |
| `proto/board/board_types.proto` | `66b2559955c447094ecec307d3cc9f894ee2e33bfc11cbf1ca7e6bc14f416998` |
| `proto/board/board_commands.proto` | `6ca42dd8c6531419b5e206e35b8c5c4422803fef5233aa2227596e790b9a0c7b` |

KiCad 10.0.6 adds read-only parent identities to PCB objects. Headerless board graphic responses require a stable instance token and a fresh matching open-document query. Pad observation reads the exact footprint definition by UUID. Unknown fields and explicit document or parent mismatches remain errors.
