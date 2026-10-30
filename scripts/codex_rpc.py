"""Run accepted MCP intent drivers through the installed Codex app-server transport."""
import gzip
import json
from pathlib import Path
import sys
import importlib.util
import shutil

SCRIPTS = Path(__file__).resolve().parent
sys.path.insert(0, str(SCRIPTS))
from stdio_rpc import Rpc
spec = importlib.util.spec_from_file_location('interruption_processes', SCRIPTS / 'check-schematic-general-interruption.py')
processes = importlib.util.module_from_spec(spec); spec.loader.exec_module(processes)


class CodexRpc:
    def __init__(self, command, env=None, log=None):
        binary = Path(command[0]).resolve(strict=True)
        selected = {key: env[key] for key in ('KICAD_CONFIG_HOME', 'AUTO_KICAD_STATE_DIR', 'TMPDIR')}
        override = 'mcp_servers.auto-kicad.env={' + ','.join(key + '=' + json.dumps(value) for key, value in selected.items()) + '}'
        self.temporary = Path(selected['TMPDIR']).resolve()
        assert self.temporary == Path(log).parent.resolve() / 'runtime-tmp'
        self.rpc = Rpc(['codex', 'app-server', '--stdio', '-c', 'mcp_servers.auto-kicad.command=' + json.dumps(str(binary)), '-c', 'mcp_servers.auto-kicad.args=[]', '-c', override], env=env, log=log)
        self.process = self.rpc.process
        self.log = Path(str(log) + '.codex.jsonl.gz')
        self.cwd = str(Path(log).parent.resolve())
        self.thread_id = None
        self.tools = None
        self.closed = False

    def request(self, method, parameters, timeout=600):
        response = self.rpc.call(method, parameters, timeout=timeout)
        with gzip.open(self.log, 'at') as stream:
            stream.write(json.dumps({'method': method, 'params': parameters, 'result': response}) + '\n')
        return response

    def call(self, method, parameters, timeout=600):
        if method == 'initialize':
            result = self.request('initialize', {'clientInfo': {'name': 'auto_kicad_acceptance', 'version': '1.0'}, 'capabilities': {'experimentalApi': True, 'requestAttestation': False}})
            self.rpc.send({'jsonrpc': '2.0', 'method': 'initialized', 'params': {}})
            thread = self.request('thread/start', {'cwd': self.cwd, 'ephemeral': True})
            self.thread_id = thread['thread']['id']
            status = self.request('mcpServerStatus/list', {'serverName': 'auto-kicad', 'detail': 'toolsAndAuthOnly', 'threadId': self.thread_id})
            server = next(s for s in status['data'] if s['name'] == 'auto-kicad')
            assert not server['toolsError'], server['toolsError']
            self.tools = list(server['tools'].values())
            return result
        if method == 'tools/list':
            assert self.thread_id and self.tools
            return {'tools': self.tools}
        assert method == 'tools/call', method
        assert self.thread_id is not None, 'Codex context has not been initialized'
        return self.request('mcpServer/tool/call', {'threadId': self.thread_id, 'server': 'auto-kicad', 'tool': parameters['name'], 'arguments': parameters['arguments']}, timeout)

    def send(self, message):
        assert message['method'] == 'notifications/initialized'

    def close(self):
        if not self.closed:
            owned = processes.descendants(self.process.pid) if self.process.poll() is None else {}
            self.rpc.close()
            processes.settle_owned_children(owned)
            removed = []
            if self.temporary:
                for path in self.temporary.iterdir():
                    if path.name.startswith(('auto-kicad-schematic-', '.tmp')):
                        assert path.is_dir() and not path.is_symlink()
                        removed.append(path.name)
                        shutil.rmtree(path)
            Path(str(self.log) + '.lifecycle.json').write_text(json.dumps({'owned_descendants': owned, 'children_stopped': True, 'orphaned_private_directories_removed_by_acceptance_owner': removed, 'automatic_server_cleanup_not_claimed': bool(removed)}, indent=2) + '\n')
            self.closed = True
