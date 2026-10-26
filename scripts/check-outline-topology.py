"""Verify native outline DRC controls on closed copies of the accepted result."""
from pathlib import Path
import hashlib
import json
import shutil
import subprocess
import sys

CASES={'open':'invalid_outline','self-intersection':'invalid_outline','copper-collision':'copper_edge_clearance'}

def prepare(output,name,work):
    # Each native parser owns one board in its own process to avoid SWIG lifetime reuse.
    import pcbnew
    source=output/'result/board.kicad_pcb'
    ids=json.loads((output/'result.json').read_text())['created']
    board=pcbnew.LoadBoard(str(source))
    drawings={str(item.m_Uuid.AsString()):item for item in board.GetDrawings()}
    if name=='open':
        board.Remove(drawings[ids[0]])
    else:
        points=[(40,30),(60,50),(60,30),(40,50)] if name=='self-intersection' else [(12,9),(14,9),(14,11),(12,11)]
        for index,(x,y) in enumerate(points):
            shape=drawings[ids[4+index]]
            nx,ny=points[(index+1)%4]
            shape.SetStart(pcbnew.VECTOR2I(pcbnew.FromMM(x),pcbnew.FromMM(y)))
            shape.SetEnd(pcbnew.VECTOR2I(pcbnew.FromMM(nx),pcbnew.FromMM(ny)))
    candidate=work/'board.kicad_pcb'
    pcbnew.SaveBoard(str(candidate),board)
    shutil.copyfile(source.with_suffix('.kicad_pro'),candidate.with_suffix('.kicad_pro'))

def verify_intent(output):
    import pcbnew
    board=pcbnew.LoadBoard(str(output/'result/board.kicad_pcb'))
    drawings={str(item.m_Uuid.AsString()):item for item in board.GetDrawings()}
    intent=json.loads((output/'intent.json').read_text())
    ids=json.loads((output/'result.json').read_text())['created']
    expected=[(edge,loop['width_nm']) for loop in intent['loops'] for edge in loop['edges']]
    assert len(expected)==len(ids)
    for uuid,(edge,width) in zip(ids,expected):
        shape=drawings[uuid]
        assert shape.GetLayer()==pcbnew.Edge_Cuts and shape.GetWidth()==width
        assert shape.GetShape()==(pcbnew.SHAPE_T_ARC if edge['kind']=='arc' else pcbnew.SHAPE_T_SEGMENT)
        points=[('start',shape.GetStart()),('end',shape.GetEnd())]
        if edge['kind']=='arc': points.append(('mid',shape.GetArcMid()))
        for key,actual in points:
            assert (actual.x,actual.y)==(edge[key]['x_nm'],edge[key]['y_nm']),(uuid,key)
    result=dict(passed=True,primitives=len(expected),coordinates_widths_layers_match_serialized_intent=True)
    (output/'intent-native-check.json').write_text(json.dumps(result,indent=2)+'\n')
    print(json.dumps(result))

def main(output):
    source=output/'result/board.kicad_pcb'
    project=source.with_suffix('.kicad_pro')
    baseline={path:hashlib.sha256(path.read_bytes()).hexdigest() for path in [source,project]}
    settings=json.loads(project.read_text())['board']['design_settings']
    assert settings['rule_severities']['invalid_outline']=='error'
    assert settings['rule_severities']['copper_edge_clearance']=='error'
    assert settings['rules']['min_copper_edge_clearance']>0
    if (output/'intent.json').is_file():
        witness=subprocess.run([sys.executable,__file__,str(output),'verify-intent'],capture_output=True,text=True,timeout=30)
        (output/'intent-native-check.log').write_text(witness.stdout+witness.stderr)
        assert witness.returncode==0,witness.stderr
    checks=output/'topology-checks'
    checks.mkdir()
    records=[]
    for name,expected in CASES.items():
        work=checks/name
        work.mkdir()
        try:
            worker=subprocess.run([sys.executable,__file__,str(output),'prepare',name],capture_output=True,text=True,timeout=30)
            (checks/(name+'-prepare.log')).write_text(worker.stdout+worker.stderr)
            assert worker.returncode==0,worker.stderr
            report=checks/(name+'.json')
            command=['/Applications/KiCad/KiCad.app/Contents/MacOS/kicad-cli','pcb','drc','--format','json','--output',str(report),str(work/'board.kicad_pcb')]
            process=subprocess.run(command,capture_output=True,text=True,timeout=60)
            (checks/(name+'.log')).write_text(process.stdout+process.stderr)
            assert process.returncode==0,process.stderr
            result=json.loads(report.read_text())
            types=sorted({item['type'] for item in result['violations']})
            records.append(dict(case=name,expected=expected,observed_types=types,detected=expected in types))
            assert expected in types,(name,types)
        finally:
            shutil.rmtree(work)
    assert all(hashlib.sha256(path.read_bytes()).hexdigest()==digest for path,digest in baseline.items())
    summary=dict(cases=records,original_result_unchanged=True,required_drc_checks_enabled=True,scope='native CLI negative controls only; not MCP planning refusal')
    (checks/'result.json').write_text(json.dumps(summary,indent=2)+'\n')
    print(json.dumps(summary,indent=2))

if __name__=='__main__':
    output=Path(sys.argv[1]).resolve()
    if len(sys.argv)==3:
        assert sys.argv[2]=='verify-intent'
        verify_intent(output)
    elif len(sys.argv)==4:
        assert sys.argv[2]=='prepare' and sys.argv[3] in CASES
        prepare(output,sys.argv[3],output/'topology-checks'/sys.argv[3])
    else:
        assert len(sys.argv)==2
        main(output)
