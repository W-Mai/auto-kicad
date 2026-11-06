# License and third-party notices

auto-kicad is licensed under GPL-3.0-or-later. Copyright 2026 W-Mai. The license permits use, modification and redistribution under GNU GPL version 3 or any later version. See [LICENSE](LICENSE).

The KiCad protocol files under `crates/kicad-ipc/proto/` retain their KiCad developer copyright notices and GPL-3.0-or-later headers. Rust protocol bindings are generated from those files during the build. KiCad itself and its symbol, footprint and 3D-model libraries are installed separately and are not included in the binary package.

Cargo dependencies retain their respective licenses. CI packages include `sbom.json` with locked package versions and license expressions, and `dependency-licenses/` with the notice files shipped by those packages, including the bundled NNG C library. The accompanying source archive includes auto-kicad source and vendored dependency sources with a Cargo configuration for offline builds.

Keep the license, third-party notices and corresponding source available when redistributing a build. Build artifacts and source archives from one CI run share the same source revision and are listed together in `SHA256SUMS`.
