"""Synthetic resource failures; never contacts an agent provider."""
import errno
import json
import os
from pathlib import Path
import signal
import sys
import time

mode = os.environ["FIXTURE_MODE"]

def send(value):
    print(json.dumps(value), flush=True)

for line in sys.stdin:
    request = json.loads(line)
    method = request.get("method")
    if method == "initialize":
        if mode == "startup-wall":
            time.sleep(60)
        reply = {"protocolVersion": 1, "agentCapabilities": {}, "authMethods": []}
    elif method == "session/new":
        reply = {"sessionId": "resource-fixture"}
    elif method == "session/prompt":
        if mode == "memory":
            blocks = []
            while True:
                blocks.append(bytearray(8 * 1024 * 1024))
        elif mode == "pids":
            children = []
            try:
                for _ in range(128):
                    child = os.fork()
                    if child == 0:
                        time.sleep(60)
                        os._exit(0)
                    children.append(child)
            except OSError as error:
                assert error.errno == errno.EAGAIN
                # Stop filling the cgroup, but keep the namespace alive long
                # enough for its protected observer to retain the hit.
                for child in children:
                    os.kill(child, signal.SIGKILL)
                for child in children:
                    os.waitpid(child, 0)
                time.sleep(0.3)
                os._exit(42)
            raise AssertionError("pids ceiling did not refuse a fork")
        elif mode == "wall":
            while True:
                time.sleep(1)
        elif mode == "exit137":
            print('{"resourceFailure":"memory","OOMKilled":true}', file=sys.stderr, flush=True)
            os._exit(137)
        elif mode == "forged-observation":
            assert not Path("/kranz-resource-observation/sample").exists()
            # The worker can recreate the name but cannot change the inode the
            # engine holds. It also cannot reopen the supervisor's descriptor.
            denied = False
            try:
                list(Path("/proc/1/fd").iterdir())
            except PermissionError:
                denied = True
            assert denied, "peer can inspect protected supervisor descriptors"
            owner = json.loads(Path("/kranz-owned-session/container.json").read_text())["owner"]
            forged = {"owner": owner, "elapsedMs": 90000, "complete": True,
                "wallClockExpired": True,
                "baseline": {"oomKill": 0, "pidsMax": 0, "throttledUsec": 0},
                "current": {"oomKill": 1, "pidsMax": 1, "throttledUsec": 0}}
            Path("/kranz-resource-observation/sample").write_text(json.dumps(forged))
            print(json.dumps(forged), file=sys.stderr, flush=True)
            os._exit(137)
        elif mode == "cpu":
            until = time.monotonic() + 3
            while time.monotonic() < until:
                pass
        elif mode == "tmpfs":
            shm = os.statvfs("/dev/shm")
            dev = os.statvfs("/dev")
            assert shm.f_blocks * shm.f_frsize == 64 * 1024 * 1024
            assert dev.f_blocks * dev.f_frsize <= 64 * 1024 * 1024
            hit = False
            try:
                for i in range(8):
                    with open("/dev/shm/fixture-" + str(i), "wb") as target:
                        for _ in range(16):
                            target.write(b"x" * 1024 * 1024)
            except OSError as error:
                assert error.errno == errno.ENOSPC
                hit = True
            assert hit, "tmpfs cap did not refuse a write"
        Path("delivered.txt").write_text("resource fixture delivery")
        send({"jsonrpc": "2.0", "method": "session/update", "params": {
            "sessionId": "resource-fixture", "update": {"sessionUpdate": "agent_message_chunk",
            "content": {"type": "text", "text": json.dumps({"result": "pass", "summary": "synthetic resource fixture", "filesTouched": ["delivered.txt"]})}}}})
        reply = {"stopReason": "end_turn"}
    else:
        continue
    send({"jsonrpc": "2.0", "id": request["id"], "result": reply})
