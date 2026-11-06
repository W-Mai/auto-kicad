# Downloads and builds

[GitHub Releases](https://github.com/W-Mai/auto-kicad/releases/latest) provides MCP executables built by cargo-dist. KiCad 10.0.7, its libraries and any native Python dependencies are installed separately. Builds are unsigned. Native KiCad acceptance covers macOS arm64; the additional platform builds are published without runtime acceptance and may fail at startup or when invoking native operations.

## Platforms

| Platform | Rust target | Archive |
| --- | --- | --- |
| macOS Apple Silicon | `aarch64-apple-darwin` | `.tar.xz` |
| macOS Intel | `x86_64-apple-darwin` | `.tar.xz` |
| Linux ARM64 | `aarch64-unknown-linux-gnu` | `.tar.xz` |
| Linux x86_64 | `x86_64-unknown-linux-gnu` | `.tar.xz` |
| Windows x86_64 | `x86_64-pc-windows-msvc` | `.zip` |

Archives contain `auto-kicad` or `auto-kicad.exe` and license notices. The separate replay utility is built from source and is not part of MCP release archives. Linux builds use GNU libc; the release manifest records platform and linkage requirements. Windows native backends still contain unsupported operations. A successful build does not establish platform feature parity.

## Install

macOS and Linux:

```sh
curl --proto '=https' --tlsv1.2 -LsSf https://github.com/W-Mai/auto-kicad/releases/latest/download/auto-kicad-installer.sh | sh
codex mcp add auto-kicad -- "$HOME/.cargo/bin/auto-kicad"
```

Windows PowerShell:

```powershell
powershell -ExecutionPolicy Bypass -c "irm https://github.com/W-Mai/auto-kicad/releases/latest/download/auto-kicad-installer.ps1 | iex"
codex mcp add auto-kicad -- "$env:USERPROFILE\.cargo\bin\auto-kicad.exe"
```

The generated installers select the host platform and install under Cargo home. Adjust the MCP executable path when using a custom `CARGO_HOME`. Alternatively, extract the matching archive and configure the client to launch its executable directly. Set `tool_timeout_sec = 600` in the existing Codex MCP configuration section, then start a new session.

The cargo-dist installer installs the executable directly. The separate `scripts/install-local.py` retains its existing macOS arm64 release layout, rollback and verification behavior; it is not the cross-platform release installer.

## Checksums and source

| Asset | Contents |
| --- | --- |
| `auto-kicad-TARGET.tar.xz` / `.zip` | MCP executable and license notices |
| `auto-kicad-installer.sh`, `auto-kicad-installer.ps1` | Generated platform-selecting installers |
| `sha256.sum`, `*.sha256` | Release checksums |
| `auto-kicad-source.tar.gz` | Corresponding tracked source and vendored Cargo dependencies |
| `auto-kicad-dependency-notices.tar.gz` | Dependency notice files, project license and inventory |
| `dependency-inventory.json` | Locked package versions, licenses, source revision and lockfile digest |
| `dist-manifest.json` | Release, artifact and linkage metadata |

Verify a downloaded archive against its checksum. On macOS, `shasum -a 256 ARCHIVE`; on Linux, `sha256sum ARCHIVE`; on Windows, `Get-FileHash ARCHIVE -Algorithm SHA256`. Compare the output with the matching `.sha256` asset or `sha256.sum`.

Keep the matching source and dependency notices available when redistributing a build. Source and inventory records identify the release commit. To rebuild the source archive, install Rust stable, Cargo, CMake and the platform C/C++ build tools, extract it, then run:

```sh
cargo build --release --locked --offline --no-default-features -p auto-kicad --bin auto-kicad
```

The included `.cargo/config.toml` resolves dependencies from `vendor/`.

## Publication

A version tag matching the Cargo package version triggers the generated [release workflow](../.github/workflows/release.yml). cargo-dist builds each target on a native runner, creates archives and installers, builds source/license assets and publishes the GitHub Release. Runtime smoke checks, installer execution and native KiCad acceptance do not gate publication. Branch and pull-request quality checks run separately.
