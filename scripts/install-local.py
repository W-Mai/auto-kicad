#!/usr/bin/env python3
"""Install a built auto-kicad executable and its macOS launcher."""

import argparse
import os
from pathlib import Path
import shlex
import shutil
import tempfile


def atomic_install(destination, write):
    destination.parent.mkdir(parents=True, exist_ok=True)
    fd, temporary = tempfile.mkstemp(prefix=".auto-kicad-", dir=destination.parent)
    os.close(fd)
    try:
        write(Path(temporary))
        os.chmod(temporary, 0o755)
        os.replace(temporary, destination)
    finally:
        Path(temporary).unlink(missing_ok=True)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("binary", type=Path)
    parser.add_argument("--prefix", type=Path, default=Path.home() / ".local")
    args = parser.parse_args()
    binary = args.binary.resolve(strict=True)
    if not binary.is_file() or not os.access(binary, os.X_OK):
        parser.error("binary must be an executable file")
    prefix = args.prefix.expanduser().resolve()
    installed = prefix / "lib/auto-kicad/auto-kicad"
    launcher = prefix / "bin/auto-kicad"
    atomic_install(installed, lambda path: shutil.copyfile(binary, path))
    script = (
        "#!/bin/sh\n"
        'PATH="/Applications/KiCad/KiCad.app/Contents/MacOS:${PATH:-/usr/bin:/bin}"\n'
        "export PATH\n" + "exec " + shlex.quote(str(installed)) + ' "$@"\n'
    )
    atomic_install(launcher, lambda path: path.write_text(script))
    print(launcher)


if __name__ == "__main__":
    main()
