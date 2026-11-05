#!/usr/bin/env python3
"""Verify source-bound native exports on an owned copy through MCP or Codex."""
import argparse
import hashlib
import json
import os
from pathlib import Path
import shutil
import sys

sys.dont_write_bytecode = True
from stdio_rpc import Rpc


def snapshot(root):
    return {str(path.relative_to(root)): hashlib.sha256(path.read_bytes()).hexdigest()
            for path in root.rglob('*') if path.is_file() and path.name != '.auto-kicad-writer.lock'}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--binary', type=Path, required=True)
    parser.add_argument('--project', type=Path, required=True)
    parser.add_argument('--schematic', type=Path, required=True, help='Path relative to the source project')
    parser.add_argument('--config', type=Path, required=True)
    parser.add_argument('--output', type=Path, required=True, help='New owned acceptance directory')
    parser.add_argument('--codex', action='store_true')
    args = parser.parse_args()
    if args.schematic.is_absolute() or '..' in args.schematic.parts:
        parser.error('schematic must be confined to the source project')
    original, configuration = snapshot(args.project), snapshot(args.config)
    args.output.mkdir()
    case = args.output.resolve()
    project, config, temporary = case / 'project', case / 'config', case / 'runtime-tmp'
    shutil.copytree(args.project, project)
    shutil.copytree(args.config, config)
    temporary.mkdir()
    source = project / args.schematic
    env = dict(os.environ, AUTO_KICAD_STATE_DIR=str(case / 'state'), KICAD_CONFIG_HOME=str(config),
               TMPDIR=str(temporary), PYTHONDONTWRITEBYTECODE='1')
    transport = Rpc
    if args.codex:
        from codex_rpc import CodexRpc
        transport = CodexRpc
    rpc = transport([str(args.binary.resolve(strict=True))], env=env, log=case / 'server.log')
    transcript = []

    def call(name, request, ok=True):
        response = rpc.call('tools/call', dict(name=name, arguments=request), timeout=900)
        assert not response.get('isError'), response
        envelope = response['structuredContent']
        transcript.append(dict(tool=name, request=request, response=envelope))
        assert envelope['ok'] == ok, envelope
        return envelope['data']

    try:
        rpc.call('initialize', dict(protocolVersion='2025-03-26', capabilities={},
                                   clientInfo=dict(name='artifact-export-acceptance', version='1')))
        rpc.send(dict(jsonrpc='2.0', method='notifications/initialized'))
        observed = call('kicad_inspect_project', dict(artifact_export=dict(
            kind='observe', project_root=str(project), root_schematic=str(source))))
        planned = call('kicad_plan_change', dict(source_run_id='artifact-export-acceptance', request_id='all-formats',
            artifact_export=dict(schema_version=1, observation_id=observed['observation_id'],
            observed_revision=observed['revision'], output_dir='manufacturing',
            formats=['gerber', 'drill', 'bom', 'schematic_pdf'])))
        identity = dict(plan_id=planned['plan_id'], plan_hash=planned['plan_hash'])
        call('kicad_preview_change', dict(identity, artifact_export=dict(files=[])), ok=False)
        approved = call('kicad_preview_change', dict(identity, artifact_export=dict(files=planned['files'])))
        request = dict(identity, approval_hash=approved['approval_hash'],
                       validation_evidence_hash=approved['validation_evidence_hash'])
        committed = call('kicad_commit_change', request)
        assert committed['state'] == 'committed', committed
        call('kicad_commit_change', request, ok=False)
        for item in planned['files']:
            data = (project / 'manufacturing' / item['path']).read_bytes()
            assert len(data) == item['bytes'] and hashlib.sha256(data).hexdigest() == item['sha256']
        assert {item['format'] for item in planned['files']} == {
            'gerber', 'drill', 'bom', 'schematic_pdf', 'manifest'}
        saved = snapshot(project)
        assert all(saved[name] == digest for name, digest in original.items())
        assert snapshot(config) == configuration
        assert snapshot(args.project) == original and snapshot(args.config) == configuration
        result = dict(passed=True, transport='codex-app-server' if args.codex else 'mcp-stdio',
                      native_version='10.0.7', files=planned['files'],
                      output_dir=str(project / 'manufacturing'), original_inputs_unchanged=True)
        (case / 'result.json').write_text(json.dumps(result, indent=2) + '\n')
        print(json.dumps(result))
    finally:
        rpc.close()
        (case / 'transcript.json').write_text(json.dumps(transcript, indent=2) + '\n')


if __name__ == '__main__':
    main()
