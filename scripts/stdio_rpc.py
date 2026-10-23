import json
import queue
import subprocess
import threading
import time


class Rpc:
    def __init__(self, command, env=None, log=None):
        self.log = open(log, "w") if log else subprocess.DEVNULL
        self.process = subprocess.Popen(
            command,
            stdin=subprocess.PIPE,
            stdout=subprocess.PIPE,
            stderr=self.log,
            env=env,
            text=True,
            bufsize=1,
        )
        self.messages = queue.Queue()
        self.sequence = 0

        def read():
            try:
                for line in self.process.stdout:
                    self.messages.put(json.loads(line))
            except Exception as error:
                self.messages.put(error)
            self.messages.put(EOFError("RPC process exited"))

        threading.Thread(target=read, daemon=True).start()

    def send(self, value):
        self.process.stdin.write(json.dumps(value) + "\n")
        self.process.stdin.flush()

    def call(self, method, params, timeout=180):
        self.sequence += 1
        self.send(
            {"jsonrpc": "2.0", "id": self.sequence, "method": method, "params": params}
        )
        deadline = time.monotonic() + timeout
        while True:
            remaining = deadline - time.monotonic()
            if remaining <= 0:
                raise TimeoutError(f"RPC deadline exceeded: {method}")
            try:
                response = self.messages.get(timeout=remaining)
            except queue.Empty as error:
                raise TimeoutError(f"RPC deadline exceeded: {method}") from error
            if isinstance(response, Exception):
                raise response
            if response.get("id") != self.sequence:
                continue
            if "error" in response:
                raise RuntimeError(json.dumps(response["error"]))
            return response["result"]

    def close(self):
        self.process.stdin.close()
        try:
            self.process.wait(timeout=10)
        except subprocess.TimeoutExpired:
            self.process.terminate()
            try:
                self.process.wait(timeout=10)
            except subprocess.TimeoutExpired:
                self.process.kill()
                self.process.wait(timeout=10)
        if hasattr(self.log, "close"):
            self.log.close()
