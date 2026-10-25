#!/usr/bin/env python3
"""Run the repository's offline quality gates."""

import os
from pathlib import Path
import subprocess
import sys


ROOT = Path(__file__).resolve().parents[1]
ENV = dict(os.environ, CARGO_INCREMENTAL="0")


def run(command):
    print("$", *command, flush=True)
    subprocess.run(command, cwd=ROOT, env=ENV, check=True)


def main():
    run([sys.executable, str(ROOT / "scripts/check-capability-matrix.py")])
    run(["cargo", "fmt", "--all", "--", "--check"])
    run(["cargo", "clippy", "--workspace", "--all-targets", "--", "-D", "warnings"])
    run(["cargo", "test", "--workspace", "--", "--test-threads=1"])


if __name__ == "__main__":
    main()
