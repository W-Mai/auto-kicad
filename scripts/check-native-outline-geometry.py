"""Exercise saved native outline checks on isolated positive and negative copies."""
from pathlib import Path
import hashlib
import json
import shutil
import subprocess
import sys

CASES = {
    'valid': True,
    'closed-shapes': True,
    'valid-major-arc': True,
    'disabled-topology-rule': False,
    'open': False,
    'one-nm-gap': False,
    'straight-crossing': False,
    'arc-line-crossing': False,
    'circle-circle-crossing': False,
    'circle-circle-tangent': False,
    'empty': False,
    'zero-area-arcs': False,
    'major-arc-span': False,
}


def prepare(source, name, work):
    import pcbnew
    board = pcbnew.LoadBoard(str(source / 'result/board.kicad_pcb'))
    ids = json.loads((source / 'result.json').read_text())['created']
    drawings = {str(item.m_Uuid.AsString()): item for item in board.GetDrawings()}
    point = lambda x, y: pcbnew.VECTOR2I(round(x * 1_000_000), round(y * 1_000_000))
    if name == 'closed-shapes':
        for shape in board.GetDrawings():
            if shape.GetLayer() == pcbnew.Dwgs_User:
                shape.SetLayer(pcbnew.Edge_Cuts)
    elif name == 'open':
        board.Remove(drawings[ids[0]])
    elif name == 'one-nm-gap':
        shape = drawings[ids[0]]
        start = shape.GetStart()
        shape.SetStart(pcbnew.VECTOR2I(start.x + 1, start.y))
    elif name in ('straight-crossing', 'arc-line-crossing'):
        points = [(40,30), (60,50), (60,30), (40,50)] if name == 'straight-crossing' else [(100,30), (140,30), (140,50), (100,50)]
        for i, start in enumerate(points):
            drawings[ids[4+i]].SetStart(point(*start))
            drawings[ids[4+i]].SetEnd(point(*points[(i+1)%4]))
    elif name in ('circle-circle-crossing', 'circle-circle-tangent'):
        shape = pcbnew.PCB_SHAPE(board)
        shape.SetShape(pcbnew.SHAPE_T_CIRCLE)
        center = 78 if name == 'circle-circle-tangent' else 73
        shape.SetStart(point(center,60)); shape.SetEnd(point(center+4,60))
        shape.SetLayer(pcbnew.Edge_Cuts); shape.SetWidth(50_000)
        board.Add(shape)
    elif name in ('empty', 'zero-area-arcs', 'major-arc-span', 'valid-major-arc'):
        for shape in list(board.GetDrawings()):
            if shape.GetLayer() == pcbnew.Edge_Cuts:
                board.Remove(shape)
        if name != 'empty':
            a, m, b = ((0,0),(10,10),(20,0)) if name == 'zero-area-arcs' else ((0,0),(100,9),(1,0))
            if name == 'valid-major-arc':
                a, m, b = (0,0), (10,9), (1,0)
            shape = pcbnew.PCB_SHAPE(board)
            shape.SetShape(pcbnew.SHAPE_T_ARC)
            shape.SetArcGeometry(point(*a), point(*m), point(*b))
            shape.SetLayer(pcbnew.Edge_Cuts); shape.SetWidth(50_000)
            board.Add(shape)
            other = pcbnew.PCB_SHAPE(board)
            if name == 'zero-area-arcs':
                other.SetShape(pcbnew.SHAPE_T_ARC)
                other.SetArcGeometry(point(*b), point(*m), point(*a))
            else:
                other.SetShape(pcbnew.SHAPE_T_SEGMENT)
                other.SetStart(point(*b)); other.SetEnd(point(*a))
            other.SetLayer(pcbnew.Edge_Cuts); other.SetWidth(50_000)
            board.Add(other)
    pcbnew.SaveBoard(str(work / 'board.kicad_pcb'), board)
    shutil.copyfile(source / 'result/board.kicad_pro', work / 'board.kicad_pro')
    if name == 'disabled-topology-rule':
        project = work / 'board.kicad_pro'
        settings = json.loads(project.read_text())
        settings['board']['design_settings']['rule_severities']['invalid_outline'] = 'ignore'
        project.write_text(json.dumps(settings))


def main(source, executable, output):
    output.mkdir()
    inputs = [source / 'result/board.kicad_pcb', source / 'result/board.kicad_pro']
    original = {str(p): hashlib.sha256(p.read_bytes()).hexdigest() for p in inputs}
    records = []
    for name, accepted in CASES.items():
        work = output / name
        work.mkdir()
        worker = subprocess.run([sys.executable, __file__, '--prepare', str(source), name, str(work)], capture_output=True, text=True, timeout=30)
        (work / 'prepare.log').write_text(worker.stdout + worker.stderr)
        if worker.returncode:
            raise RuntimeError(worker.stderr)
        before = {str(p): hashlib.sha256(p.read_bytes()).hexdigest() for p in [work / 'board.kicad_pcb', work / 'board.kicad_pro']}
        checked = subprocess.run([str(executable), str(work / 'board.kicad_pcb')], capture_output=True, text=True, timeout=65)
        (work / 'check.json').write_text(checked.stdout)
        (work / 'check.log').write_text(checked.stderr)
        result = json.loads(checked.stdout)
        assert (checked.returncode == 0) == accepted, (name, result)
        assert ('Ok' in result) == accepted, (name, result)
        if accepted:
            native = result['Ok']['native']
            assert native['outer_count'] == (1 if name == 'valid-major-arc' else 2), native
            expected_holes = {'closed-shapes': 4, 'valid-major-arc': 0}.get(name, 2)
            assert sum(native['holes']) == expected_holes, native
        assert all(hashlib.sha256(Path(p).read_bytes()).hexdigest() == h for p,h in before.items())
        assert not list(work.glob('.auto-kicad-outline-*'))
        records.append(dict(case=name, accepted=accepted, result=result, source_preserved=True, temporary_snapshots_cleaned=True))
        print(name, 'accepted' if accepted else 'rejected', flush=True)
    assert all(hashlib.sha256(Path(p).read_bytes()).hexdigest() == h for p,h in original.items())
    summary = dict(kicad_version='10.0.7', cases=records, original_result_unchanged=True, editor_launches=0, scope='saved native topology, full curve bounds and absolute invalid-outline DRC; not complete DRC or MCP approval')
    (output / 'result.json').write_text(json.dumps(summary, indent=2) + '\n')


if __name__ == '__main__':
    if sys.argv[1] == '--prepare':
        assert len(sys.argv) == 5 and sys.argv[3] in CASES
        prepare(Path(sys.argv[2]), sys.argv[3], Path(sys.argv[4]))
    else:
        assert len(sys.argv) == 4
        main(*(Path(a).resolve() for a in sys.argv[1:]))
