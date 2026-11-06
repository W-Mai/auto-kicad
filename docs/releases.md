# Downloads and builds

CI builds the MCP executable for macOS arm64. KiCad 10.0.7 and its libraries must be installed separately. Build archives are unsigned; the CI smoke check verifies the CLI and MCP tool handshake without starting a native editor. Tool discovery is checked against the capability matrix and runtime backend availability; runners without KiCad omit schematic editing tools. The native CAD qualification is recorded separately in [real-project acceptance](acceptance/final-real-projects.md).

## Download

Open a successful [CI run](https://github.com/W-Mai/auto-kicad/actions/workflows/quality.yml) and download the `auto-kicad-macos-arm64` artifact. GitHub requires sign-in for Actions artifact downloads. Artifacts are retained for 14 days. A pushed `vVERSION` tag publishes the same assets to [Releases](https://github.com/W-Mai/auto-kicad/releases) after quality and build jobs pass; the tag must match the Cargo package version.

| File | Contents |
| --- | --- |
| `auto-kicad-VERSION-aarch64-apple-darwin.tar.gz` | Executable, local installer, GPL license, dependency notices, SBOM, build metadata and smoke result |
| `auto-kicad-VERSION-source.tar.gz` | Corresponding tracked source, vendored Cargo dependencies and offline source configuration |
| `SHA256SUMS` | SHA-256 checksums for the archives and accompanying records |
| `build-info.json`, `smoke.json` | Version, source revision, platform and executed CLI/MCP checks |

Keep both archives when redistributing a build. They identify the same source revision. The dependency inventory includes locked versions and license expressions; copied notice files and vendored sources preserve dependency notices.

## Verify and install

Extract the downloaded Actions artifact, then check its contents:

```sh
shasum -a 256 -c SHA256SUMS
```

For version 0.1.0, extract and install the executable:

```sh
tar -xzf auto-kicad-0.1.0-aarch64-apple-darwin.tar.gz
cd auto-kicad-0.1.0-aarch64-apple-darwin
python3 scripts/install-local.py ./auto-kicad --no-rollback
python3 scripts/install-local.py --verify
codex mcp add auto-kicad -- "$HOME/.local/bin/auto-kicad"
```

Add `tool_timeout_sec = 600` to the existing `[mcp_servers.auto-kicad]` section in `~/.codex/config.toml`, keeping its command entry, then start a new session. The installer adds the macOS KiCad CLI directory to PATH and verifies the packaged inventory against the executable. It needs no Cargo checkout. `--no-rollback` keeps one release; omit it to retain the previous release.

Local installation retains its existing manifest layout for compatibility with older installed releases. New executable metadata and package notices declare GPL-3.0-or-later; the legacy local-installation layout identifier does not restrict the GPL grant. Existing installed payloads and historical records are not rewritten by a source upgrade.

## Rebuild the source archive

Install Rust stable, Cargo, CMake and the Xcode command-line tools. Extract the source archive and run from its root:

```sh
env CARGO_INCREMENTAL=0 cargo build --release --locked --offline -p auto-kicad --bin auto-kicad
```

The included `.cargo/config.toml` resolves dependencies from `vendor/`. CI verifies the source archive's dependency graph with a fresh Cargo home in offline mode. This checks source completeness; it does not promise byte-identical compiler output across toolchains.
