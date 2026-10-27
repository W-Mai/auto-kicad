"""Independently verify saved native content, generated identities and complete MCP reports."""
from pathlib import Path
import hashlib
import json
import re
import sys
import pcbnew

root = Path(sys.argv[1]).resolve()
result = json.loads((root / 'result.json').read_text())
records = json.loads((root / 'mcp.json').read_text())
metadata = json.loads((root / 'snapshot.json').read_text())
saved, bound = root / 'result.kicad_pcb', root / 'bound.kicad_pcb'
inputs = {p: p.read_bytes() for p in [saved, bound]}
assert hashlib.sha256(inputs[saved]).hexdigest() == metadata['input_sha256']
assert hashlib.sha256(inputs[bound]).hexdigest() == metadata['snapshot_sha256']
identities = {i['native_uuid'] for i in metadata['identities']}
assert identities
for name, path in [('saved', saved), ('bound', bound)]:
    board = pcbnew.LoadBoard(str(path))
    generated = [z for z in board.Zones() if z.IsTeardropArea()]
    assert len(generated) == len(identities)
    if name == 'bound':
        assert {str(z.m_Uuid.AsString()) for z in generated} == identities
    pcbnew.SaveBoard(str(root / f'independent-native-{name}.kicad_pcb'), board)

def native_content(path):
    text = path.read_text()
    pattern = r'(?ms)^\t\(zone\n.*?^\t\)'
    return sorted(re.findall(pattern, text)), re.sub(pattern, '', text)

assert native_content(root / 'independent-native-saved.kicad_pcb') == native_content(root / 'independent-native-bound.kicad_pcb')
assert all(p.read_bytes() == data for p, data in inputs.items())

def full_report(evidence, mapping):
    value = json.loads(evidence['raw_json'])
    value.pop('source'); value.pop('date')
    for section in ['violations', 'unconnected_items', 'schematic_parity']:
        for diagnostic in value.get(section, []):
            for item in diagnostic['items']:
                item['uuid'] = mapping.get(item['uuid'], item['uuid'])
            diagnostic['items'].sort(key=lambda item: json.dumps(item, sort_keys=True))
        if section in value:
            value[section].sort(key=lambda item: json.dumps(item, sort_keys=True))
    return value

checks = []
for record in records:
    data = record.get('response', {}).get('structuredContent', {}).get('data', {})
    if record['name'] != 'kicad_commit_change' or data.get('state') != 'committed':
        continue
    preview = next(r['response']['structuredContent']['data']['shadow_validation']['geometry']
                   for r in records if r['name'] == 'kicad_preview_change'
                   and r['arguments']['plan_id'] == record['arguments']['plan_id']
                   and r['response'].get('structuredContent', {}).get('data', {}).get('state') == 'awaiting_approval')
    actual = data['outcome']['geometry']
    for phase, report, field in [('before', 'baseline', 'baseline_map'), ('after', 'after', 'diagnostic_map')]:
        left = preview['candidate']['generated_' + phase]['identities']
        right = actual['candidate']['generated_' + phase]['identities']
        mapping = {a['native_uuid']: b['native_uuid'] for a, b in zip(left, right)}
        if phase == 'after':
            mapping.update(actual['creation_map'])
        assert mapping == actual[field]
        assert full_report(preview[report], mapping) == full_report(actual[report], {})
        unmapped_differs = full_report(preview[report], {}) != full_report(actual[report], {})
        assert unmapped_differs, 'this control must contain generated UUID diagnostics'
        fields = ['native_uuid', 'correspondence_key', 'kind', 'layer', 'net', 'text_definition_digest', 'text_content_digest', 'native_definition_digest', 'native_content_digest']
        for evidence in [preview, actual]:
            items = evidence['candidate']['generated_' + phase]['identities']
            assert all(set(item) == set(fields) for item in items)
            projected = [{key: ('' if key == 'native_uuid' else item[key]) for key in fields} for item in items]
            digest = hashlib.sha256(json.dumps(projected, separators=(',', ':'), ensure_ascii=False).encode()).hexdigest()
            assert digest == evidence['candidate'][phase]['generated_digest']
        checks.append(dict(phase=phase, generated=len(left), full_report_equal=True, unmapped_reports_differ=True))
assert checks
evidence = dict(passed=True, native_serialization_equal_except_zone_order=True, read_only_inputs=True,
                generated_count=len(identities), reports=checks,
                library_drift=result['library_drift_permanently_revokes_approval'])
(root / 'independent-generated.json').write_text(json.dumps(evidence, indent=2) + '\n')
print(json.dumps(evidence))
