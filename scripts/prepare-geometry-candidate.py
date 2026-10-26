#!/usr/bin/env python3
"""Copy one GUI-saved baseline and its library context for candidate-pair acceptance."""
from collections import Counter
import hashlib
import json
from pathlib import Path
import shutil
import sys

import pcbnew


def sha(data):
    return hashlib.sha256(data).hexdigest()


def pads(path):
    board = pcbnew.LoadBoard(str(path))
    assert board is not None
    return Counter(str(pad.m_Uuid.AsString()) for footprint in board.GetFootprints()
                   for pad in footprint.Pads())


def main():
    if len(sys.argv) != 5:
        raise SystemExit('usage: prepare-geometry-candidate.py ORIGINAL GUI_BASELINE BASELINE_PROJECT NEW_DIRECTORY')
    source, saved, project = [Path(p).resolve(strict=True) for p in sys.argv[1:4]]
    output = Path(sys.argv[4]).resolve()
    if source.suffix != '.kicad_pcb' or saved.suffix != '.kicad_pcb' or project.suffix != '.kicad_pro':
        raise ValueError('expected native PCB and project paths')
    output.mkdir()
    original = {source.with_suffix(ext): source.with_suffix(ext).read_bytes()
                for ext in ['.kicad_pcb', '.kicad_pro', '.kicad_dru']
                if source.with_suffix(ext).is_file()}
    saved_bytes, project_bytes = saved.read_bytes(), project.read_bytes()
    source_pads, saved_pads = pads(source), pads(saved)
    assert sum(source_pads.values()) == sum(saved_pads.values())
    assert all(count == 1 for count in saved_pads.values()), 'saved baseline has duplicate pad identities'
    board = output / source.name
    board.write_bytes(saved_bytes)
    board.with_suffix('.kicad_pro').write_bytes(project_bytes)
    rules = source.with_suffix('.kicad_dru')
    if rules.is_file():
        shutil.copyfile(rules, board.with_suffix('.kicad_dru'))
    table = source.parent / 'fp-lib-table'
    if table.is_file():
        shutil.copyfile(table, output / table.name)
    for library in source.parent.glob('*.pretty'):
        shutil.copytree(library, output / library.name)
    for path, data in original.items():
        assert path.read_bytes() == data, 'original input changed'
    assert saved.read_bytes() == saved_bytes and project.read_bytes() == project_bytes
    record = dict(source=str(source), saved_baseline=str(saved), settled=str(board),
                  source_sha256=sha(original[source]), settled_sha256=sha(saved_bytes),
                  duplicate_source_pad_ids={key: count for key, count in source_pads.items() if count > 1},
                  pad_count=sum(saved_pads.values()), settled_pad_ids_unique=True,
                  original_files_unchanged=True, saved_baseline_unchanged=True,
                  scope='Owned acceptance input copied from a GUI-saved baseline; no production source repair')
    (output / 'preparation.json').write_text(json.dumps(record, indent=2) + '\n')
    print(json.dumps(record))


if __name__ == '__main__':
    main()
