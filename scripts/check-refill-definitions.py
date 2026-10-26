"""Prepare an owned unfilled PCB and independently refill a closed verification copy."""
from pathlib import Path
import json
import shutil
import subprocess
import sys

mode, board_name, source_name, output_name = sys.argv[1:]
board, source, output = map(Path, [board_name, source_name, output_name])
assert board.resolve().is_relative_to((output / 'work').resolve())

def libraries(destination):
    table = source.parent / 'fp-lib-table'
    if table.is_file():
        shutil.copyfile(table, destination / table.name)
    for library in source.parent.glob('*.pretty'):
        shutil.copytree(library, destination / library.name)

if mode == 'prepare':
    import pcbnew
    native = pcbnew.LoadBoard(str(board))
    for zone in native.Zones():
        zone.UnFill()
    pcbnew.SaveBoard(str(board), native)
    libraries(board.parent)
elif mode == 'verify':
    independent = output / 'independent'
    independent.mkdir()
    for suffix in ['.kicad_pcb', '.kicad_pro', '.kicad_dru']:
        path = board.with_suffix(suffix)
        if path.is_file():
            shutil.copyfile(path, independent / path.name)
    libraries(independent)
    process = subprocess.run([
        '/Applications/KiCad/KiCad.app/Contents/MacOS/kicad-cli',
        'pcb', 'drc', '--refill-zones', '--save-board', '--format', 'json',
        '--output', str(output / 'drc.json'), str(independent / board.name),
    ], capture_output=True, text=True, timeout=60)
    (output / 'cli.log').write_text(process.stdout + process.stderr)
    assert process.returncode == 0, process.stderr
    report = json.loads((output / 'drc.json').read_text())
    assert report['kicad_version'] == '10.0.7'
    assert isinstance(report['violations'], list)
    print(json.dumps(dict(violations=len(report['violations']), unconnected=len(report['unconnected_items']))))
else:
    raise ValueError(mode)
