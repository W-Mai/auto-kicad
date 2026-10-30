#!/usr/bin/env python3
"""Run native schematic intent acceptance through a real Codex app-server MCP client."""
import argparse
import importlib.util
from pathlib import Path
import sys
from codex_rpc import CodexRpc

drivers = {
    'general': 'check-schematic-general.py',
    'component': 'check-schematic-component-edits.py',
    'multiunit': 'check-schematic-multiunit.py',
    'inheritance': 'check-schematic-inheritance.py',
    'sheets': 'check-schematic-sheets.py',
    'children': 'check-schematic-children.py',
    'removal': 'check-schematic-sheet-removal.py',
}
parser = argparse.ArgumentParser(description=__doc__, usage='%(prog)s FAMILY SERVER OWNED_SOURCE OWNED_CONFIG NEW_OUTPUT [driver options]')
parser.add_argument('family', choices=drivers)
parser.add_argument('driver_arguments', nargs=argparse.REMAINDER)
args = parser.parse_args()
if len(args.driver_arguments) < 4:
    parser.error('SERVER, OWNED_SOURCE, OWNED_CONFIG and NEW_OUTPUT are required')
driver = Path(__file__).with_name(drivers[args.family])
spec = importlib.util.spec_from_file_location('codex_schematic_intent_driver', driver)
module = importlib.util.module_from_spec(spec); spec.loader.exec_module(module)
module.Rpc = CodexRpc
sys.argv = [str(driver), *args.driver_arguments]
module.main()
