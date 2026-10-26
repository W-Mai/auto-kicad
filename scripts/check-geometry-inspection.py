"""Verify complete read-only native geometry through the production stdio tool."""
import json
import os
from pathlib import Path
import subprocess
import sys

from stdio_rpc import Rpc

endpoint, board_arg, output_arg, binary = sys.argv[1:]
board, output = Path(board_arg), Path(output_arg)
records = {}
rpc = Rpc([binary], env=dict(os.environ, AUTO_KICAD_STATE_DIR=str(output/'state')), log=str(output/'stdio.log'))


def inspect(label, **args):
    result = rpc.call('tools/call', {'name':'kicad_inspect_project', 'arguments':dict(endpoint=endpoint, **args)})
    records[label] = result
    (output/'mcp.json').write_text(json.dumps(records,indent=2))
    assert not result.get('isError'), result
    envelope = result['structuredContent']
    assert envelope['ok'], envelope
    return envelope['data']


try:
    rpc.call('initialize', {'protocolVersion':'2025-03-26','capabilities':{},'clientInfo':{'name':'geometry-inspection-acceptance','version':'1'}})
    rpc.send({'jsonrpc':'2.0','method':'notifications/initialized'})
    tools=rpc.call('tools/list',{})['tools']
    schema=next(t for t in tools if t['name']=='kicad_inspect_project')['inputSchema']
    assert 'geometry' in schema['properties']
    base=inspect('baseline')
    assert 'geometry' not in base
    assert 'geometry' not in inspect('disabled',geometry=False)
    result=inspect('observed',geometry=True)
    observed=result['geometry']
    assert observed['status']=='observed', observed
    summary=observed['observation']
    assert summary['pcb_revision']==result['pcb']['revision']
    assert summary['observed_revision'].startswith('sha256:') and len(summary['observed_revision'])==71
    assert len(json.dumps(summary))<=2*1024*1024
    assert 'filled_polygons' not in json.dumps(summary)
    assert inspect('repeat',geometry=True)['geometry']==observed
    (output/'geometry.json').write_text(json.dumps(summary,indent=2))
    witness=Path(__file__).with_name('check-geometry-inspection-native.py')
    subprocess.run(['/Applications/KiCad/KiCad.app/Contents/Frameworks/Python.framework/Versions/3.9/bin/python3.9',str(witness),str(board),str(output)],check=True)
    for label, arguments in [('no_endpoint',dict(endpoint='',geometry=True)),('wrong_type',dict(endpoint=endpoint,geometry={'write':True}))]:
        try:
            reply=rpc.call('tools/call',{'name':'kicad_inspect_project','arguments':arguments})
        except RuntimeError as e:
            records[label]={'rpc_error':str(e)}
        else:
            records[label]=reply
            assert reply.get('isError') or not reply['structuredContent']['ok']
    # Saved-rule drift is explicit evidence and cannot alter the geometry-only revision.
    custom=board.with_suffix('.kicad_dru')
    original=custom.read_bytes() if custom.exists() else None
    try:
        custom.write_bytes((original or b'(version 1)\n')+b'\n# geometry evidence drift\n')
        changed=inspect('rules_changed',geometry=True)['geometry']
        assert changed['status']=='observed',changed
        assert changed['observation']['evidence_digest']!=summary['evidence_digest']
        assert changed['observation']['saved_rules']!=summary['saved_rules']
    finally:
        if original is None: custom.unlink()
        else: custom.write_bytes(original)
    assert inspect('restored',geometry=True)['geometry']==observed
    (output/'result.json').write_text(json.dumps(dict(production_stdio=True,zone_count=len(summary['zones']),outline_count=len(summary['outlines']),source_unchanged=True,repeat_equal=True,saved_rule_drift_bound=True,independent_native_intent=True),indent=2)+'\n')
finally:
    (output/'mcp.json').write_text(json.dumps(records,indent=2))
    rpc.close()
