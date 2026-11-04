#!/usr/bin/env python3
"""Qualify closed schematic-to-PCB synchronization through the production MCP stdio server."""
import argparse
import copy
import hashlib
import importlib.util
import json
import os
from pathlib import Path
import shutil
import signal
import threading
import time

from stdio_rpc import Rpc


def snapshot(root):
    return {str(p.relative_to(root)): hashlib.sha256(p.read_bytes()).hexdigest()
            for p in root.rglob('*') if p.is_file() and p.name != '.auto-kicad-writer.lock'}


def load_support(name, filename):
    spec = importlib.util.spec_from_file_location(name, Path(__file__).with_name(filename))
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    for name in ('binary', 'source', 'config', 'output'):
        parser.add_argument(name, type=Path)
    parser.add_argument('--interruption', action='store_true')
    args = parser.parse_args()
    binary, source, config = (p.resolve(strict=True) for p in (args.binary, args.source, args.config))
    project = source.parent
    for root in (project, config):
        assert (root / '.auto-kicad-audit-owned').read_text() == 'schematic-audit-acceptance\n'
    args.output.mkdir()
    output = args.output.resolve(strict=True)
    assert not output.is_relative_to(project) and not output.is_relative_to(config)
    temporary, state = output / 'runtime-tmp', output / 'state'
    temporary.mkdir()
    env = dict(os.environ, AUTO_KICAD_STATE_DIR=str(state), KICAD_CONFIG_HOME=str(config),
               TMPDIR=str(temporary), PYTHONDONTWRITEBYTECODE='1')
    before, configuration = snapshot(project), snapshot(config)
    board = source.with_suffix('.kicad_pcb')
    original_board = board.read_bytes()
    (output / 'original.kicad_pcb').write_bytes(original_board)
    transcript, cases = [], []
    rpc, launches = None, 0

    def start():
        nonlocal rpc, launches
        launches += 1
        rpc = Rpc([str(binary)], env=env, log=output / f'server-{launches}.log')
        rpc.call('initialize', {'protocolVersion': '2025-03-26', 'capabilities': {}, 'clientInfo': {'name': 'schematic-pcb-sync-acceptance', 'version': '1'}})
        rpc.send({'jsonrpc': '2.0', 'method': 'notifications/initialized'})

    def call(name, request, ok=True):
        response = rpc.call('tools/call', {'name': name, 'arguments': request}, timeout=600)
        assert not response.get('isError'), (name, response)
        envelope = response['structuredContent']
        transcript.append(dict(tool=name, request=request, response=envelope))
        assert envelope['ok'] == ok, (name, envelope)
        return envelope['data']

    def inspect(request):
        return call('kicad_inspect_project', {'schematic_pcb_sync': request})

    def observe():
        baseline = snapshot(project)
        data = inspect(dict(kind='observe', project_root=str(project), root_schematic=str(source)))
        assert snapshot(project) == baseline and snapshot(config) == configuration
        return data

    def intent(observed):
        return dict(schema_version=1, actions=[dict(kind='update', component_id=c['id'], fields=[], update_nets=True)
                    for c in observed['components'] if c['on_board'] and not c['reference'].startswith('#')],
                    copper_nets=[change for change in json.loads((project / '.auto-kicad-sync-copper.json').read_text())
                                 if next((c['net'] for c in observed['copper'] if c['uuid'] == change['uuid']), None) != change['net']])

    def plan(observed, ok=True):
        baseline = snapshot(project)
        result = call('kicad_plan_change', dict(source_run_id='sync-stdio', request_id=f'sync-plan-{len(transcript)}',
                      schematic_pcb_sync=dict(schema_version=1, observation_id=observed['observation_id'], observed_revision=observed['revision'], intent=intent(observed))), ok=ok)
        assert snapshot(project) == baseline and snapshot(config) == configuration
        return result

    def expected(planned):
        assert not planned['drc_delta']['metadata_changed']
        return dict(differences=planned['differences'], drc={key: planned['drc_delta'][key] for key in ('introduced', 'resolved')})

    def identity(planned):
        return dict(plan_id=planned['plan_id'], plan_hash=planned['plan_hash'])

    def approve(planned):
        return call('kicad_preview_change', dict(identity(planned), schematic_pcb_sync=expected(planned)))

    def commit_request(planned, approved):
        return dict(identity(planned), approval_hash=approved['approval_hash'], validation_evidence_hash=approved['validation_evidence_hash'])

    def release(observed=None, planned=None):
        inspect(dict(kind='release', observation_ids=[observed['observation_id']] if observed else [], plan_ids=[planned['plan_id']] if planned else []))

    def verify_inputs_after_commit(planned):
        changed = snapshot(project)
        assert set(changed) == set(before)
        assert {name for name in before if changed[name] != before[name]} == {board.name}
        assert snapshot(config) == configuration
        assert hashlib.sha256(board.read_bytes()).hexdigest() == planned['native_preview']['candidate_board_sha256']
        assert not (project / '.auto-kicad-sync-pending').exists()

    try:
        start()
        tools = {tool['name']: tool for tool in rpc.call('tools/list', {})['tools']}
        assert len(tools) == 13
        for tool in ('kicad_inspect_project', 'kicad_plan_change', 'kicad_preview_change'):
            assert 'schematic_pcb_sync' in tools[tool]['inputSchema']['properties']
        capability = call('kicad_capabilities', {})['schematic_pcb_sync']
        assert capability['available'] and capability['requires_complete_difference_and_drc_expectation']
        call('kicad_inspect_project', dict(endpoint='must-not-connect', schematic_pcb_sync=dict(kind='history')), ok=False)
        call('kicad_plan_change', dict(source_run_id='mixed', request_id='mixed', segments=[], schematic_pcb_sync=dict(schema_version=1, observation_id='missing', observed_revision='missing', intent=dict(schema_version=1, actions=[], copper_nets=[]))), ok=False)
        if not args.interruption:
            for path in (source, board, source.with_suffix('.kicad_pro'), config / '10.0/kicad_common.json'):
                observed = observe()
                original = path.read_bytes()
                path.write_bytes(original + b'\n')
                altered = snapshot(project), snapshot(config)
                request = dict(source_run_id='drift', request_id='drift', schematic_pcb_sync=dict(schema_version=1,
                               observation_id=observed['observation_id'], observed_revision=observed['revision'], intent=intent(observed)))
                call('kicad_plan_change', request, ok=False)
                assert altered == (snapshot(project), snapshot(config))
                path.write_bytes(original)
                release(observed=observed)
                cases.append(dict(name='source-drift', path=str(path), rejected_before_write=True))
        observed = observe()
        planned = plan(observed)
        assert planned['state'] == 'planned' and planned['has_changes']
        call('kicad_preview_change', identity(planned), ok=False)
        corrupt = copy.deepcopy(expected(planned));corrupt['differences'][0]['after'] = 'unapproved-value'
        call('kicad_preview_change', dict(identity(planned), schematic_pcb_sync=corrupt), ok=False)
        approved = approve(planned)
        call('kicad_commit_change', dict(commit_request(planned, approved), approval_hash='wrong'), ok=False)
        call('kicad_commit_change', dict(commit_request(planned, approved), validation_evidence_hash='wrong'), ok=False)
        assert snapshot(project) == before and snapshot(config) == configuration

        if args.interruption:
            support = load_support('sync_owned_processes', 'check-schematic-general-interruption.py')
            completed, replies = threading.Event(), []
            def publish():
                try: replies.append(call('kicad_commit_change', commit_request(planned, approved)))
                except BaseException as error: replies.append(dict(connection_error=str(error)))
                finally: completed.set()
            worker = threading.Thread(target=publish);worker.start()
            deadline, record = time.monotonic() + 180, None
            while time.monotonic() < deadline and not completed.is_set():
                for path in state.rglob('record.json'):
                    try: current = json.loads(path.read_text())
                    except (FileNotFoundError, json.JSONDecodeError): continue
                    file = current.get('sync_publication', {}).get('file')
                    if current.get('domain') == 'pcb_sync' and current.get('state') == 'in_flight' and file and board.is_file() and hashlib.sha256(board.read_bytes()).hexdigest() == file['candidate_sha256']:
                        record = current;break
                if record: break
                time.sleep(.002)
            assert record is not None, ('no interruptible actual PCB publication', replies)
            os.kill(rpc.process.pid, signal.SIGSTOP)
            children = support.descendants(rpc.process.pid)
            rpc.process.kill();rpc.process.wait(timeout=10);worker.join(timeout=10)
            assert completed.is_set()
            support.settle_owned_children(children);rpc.close();rpc = None
            (output / 'interrupted-record.json').write_text(json.dumps(record, indent=2) + '\n')
            (output / 'owned-children.json').write_text(json.dumps(children, indent=2) + '\n')
            for path in temporary.iterdir():
                if path.is_dir() and not path.is_symlink(): shutil.rmtree(path)
                else: path.unlink()
            start()
            call('kicad_commit_change', commit_request(planned, approved), ok=False)
            blocked = plan(observe(), ok=False);assert 'recovery' in blocked['reason']
            candidate = board.read_bytes();board.write_bytes(candidate + b'\n')
            recovery_request = dict(project_dir=str(project), attempt_id=record['attempt_id'], action='restore_backups')
            call('kicad_plan_recovery', recovery_request, ok=False)
            assert board.read_bytes() == candidate + b'\n';board.write_bytes(candidate)
            recovery = call('kicad_plan_recovery', recovery_request)
            recovery_identity = dict(project_dir=str(project), recovery_id=recovery['recovery_id'], recovery_plan_hash=recovery['recovery_plan_hash'])
            approved_recovery = call('kicad_preview_recovery', recovery_identity)
            rpc.close();rpc = None;start()
            call('kicad_commit_recovery', dict(recovery_identity, approval_hash=approved_recovery['approval_hash']), ok=False)
            recovery = call('kicad_plan_recovery', recovery_request)
            recovery_identity = dict(project_dir=str(project), recovery_id=recovery['recovery_id'], recovery_plan_hash=recovery['recovery_plan_hash'])
            approved_recovery = call('kicad_preview_recovery', recovery_identity)
            call('kicad_commit_recovery', dict(recovery_identity, approval_hash='wrong'), ok=False)
            restored = call('kicad_commit_recovery', dict(recovery_identity, approval_hash=approved_recovery['approval_hash']))
            assert restored['state'] == 'restored' and restored['outcome']['actual_native_baseline_verified']
            assert snapshot(project) == before and snapshot(config) == configuration
            cases.append(dict(name='actual-killed-stdio-recovery', later_user_bytes_preserved=True, fresh_restart_approval_required=True, actual_native_restoration=True))
        else:
            rpc.close();rpc = None;start()
            call('kicad_commit_change', commit_request(planned, approved), ok=False)
            assert snapshot(project) == before and snapshot(config) == configuration
            cases.append(dict(name='restart-approval', rejected_before_write=True))

        observed = observe();planned = plan(observed);approved = approve(planned)
        committed = call('kicad_commit_change', commit_request(planned, approved))
        assert committed['state'] == 'committed';verify_inputs_after_commit(planned)
        call('kicad_commit_change', commit_request(planned, approved), ok=False)
        release(planned=planned)
        (output / 'result.kicad_pcb').write_bytes(board.read_bytes())
        cases.append(dict(name='connected-sync', native_publication_committed=True))
        if not args.interruption:
            unchanged = snapshot(project)
            observed = observe()
            redundant = intent(observed)
            redundant['copper_nets'] = json.loads((project / '.auto-kicad-sync-copper.json').read_text())
            call('kicad_plan_change', dict(source_run_id='redundant', request_id='redundant', schematic_pcb_sync=dict(schema_version=1,
                 observation_id=observed['observation_id'], observed_revision=observed['revision'], intent=redundant)), ok=False)
            assert snapshot(project) == unchanged
            no_op = plan(observed)
            assert no_op['state'] == 'unchanged' and not no_op['has_changes']
            call('kicad_preview_change', dict(identity(no_op), schematic_pcb_sync=expected(no_op)), ok=False)
            assert snapshot(project) == unchanged
            release(observed=observed, planned=no_op)
            cases.append(dict(name='unchanged', mutation_approval_refused=True))
        ledger = call('kicad_inspect_project', dict(endpoint='', project_dir=str(project)))
        executions = [r for r in ledger['execution_history'] if r['domain'] == 'pcb_sync']
        assert len(executions) == (2 if args.interruption else 1)
        assert all(r['schema_version'] == 4 and r['state'] in ('restored', 'committed') for r in executions)
        (output / 'durable-history.json').write_text(json.dumps(ledger, indent=2) + '\n')
    finally:
        if rpc: rpc.close()
        (output / 'transcript.json').write_text(json.dumps(transcript, indent=2) + '\n')
    load_support('sync_cache_cleanup', 'check-schematic-general.py').cleanup_native_cache(temporary)
    result = dict(passed=True, production_stdio=True, server_sha256=hashlib.sha256(binary.read_bytes()).hexdigest(),
                  cases=cases, actual_stdio_writer_killed=args.interruption, private_snapshots_removed=True,
                  configuration_unchanged=True, gui_launched=False, product_complete=False)
    (output / 'result.json').write_text(json.dumps(result, indent=2) + '\n')
    print(json.dumps(result), flush=True)


if __name__ == '__main__':
    main()
