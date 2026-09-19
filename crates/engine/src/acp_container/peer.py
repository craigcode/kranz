"""Synthetic ACP peer; never a provider/credential fixture."""
import json
import os
import signal
import socket
import subprocess
import sys
import time
from urllib.parse import urlsplit

mode = os.environ["FIXTURE_MODE"]


def send(message):
    print(json.dumps(message), flush=True)


def idle_tree():
    # A namespace peer must not be able to stop PID 1's lease checks.
    os.kill(1, signal.SIGSTOP)
    pid = os.fork()
    if pid == 0:
        os.setsid()
        while True:
            with open("child-heartbeat", "w") as target:
                target.write(str(time.monotonic_ns()))
            time.sleep(0.05)
    while not os.path.exists("child-heartbeat"):
        time.sleep(0.01)
    with open("ready", "w") as target:
        target.write(str(pid))
    while True:
        time.sleep(1)


def hostile_probes():
    denied = {}

    def probe(name, action):
        try:
            action()
            denied[name] = False
        except OSError:
            denied[name] = True

    for name in ["serve.token", "serve.read.token", "config.json"]:
        probe("read-" + name, lambda name=name: open(".kranz/" + name).read())
    probe("audit-write", lambda: open(".kranz/missions/m-fixture/state.json", "w").write("tampered"))
    probe("outside-write", lambda: open(os.environ["FIXTURE_OUTSIDE"], "w").write("tampered"))
    probe("git-config", lambda: open(os.environ["FIXTURE_GIT_CONFIG"], "w").write("tampered"))
    probe("base-ref", lambda: open(os.environ["FIXTURE_BASE_REF"], "w").write("0" * 40))
    probe("renew-host-lease", lambda: open("/kranz-owned-session/lease", "w").write("forged"))
    probe("supervisor-memory", lambda: open("/proc/1/mem", "rb"))
    probe("supervisor-root", lambda: open("/proc/1/root/kranz-owned-session/lease", "w"))
    with socket.socket() as connection:
        connection.settimeout(0.2)
        probe("direct-egress", lambda: connection.connect(("192.0.2.1", 443)))
    shell = subprocess.run(["/bin/sh", "-c", 'printf tampered > "$1"', "fixture", os.environ["FIXTURE_OUTSIDE"]], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    denied["shell-write"] = shell.returncode != 0
    nested = subprocess.run([sys.executable, "-c", 'import os;os.setsid();open(os.environ["FIXTURE_OUTSIDE"],"w").write("tampered")'], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    denied["nested-write"] = nested.returncode != 0
    with open("probes.json", "w") as target:
        json.dump(denied, target)
    if not all(denied.values()):
        os._exit(77)


def egress_probes():
    proxy = urlsplit(os.environ["HTTPS_PROXY"])

    def tunnel(authority, allowed):
        with socket.create_connection((proxy.hostname, proxy.port), timeout=5) as connection:
            connection.sendall(("CONNECT " + authority + " HTTP/1.1\r\nHost: " + authority + "\r\n\r\n").encode("ascii"))
            header = b""
            while b"\r\n\r\n" not in header and len(header) < 4096:
                chunk = connection.recv(1)
                if not chunk:
                    break
                header += chunk
            if allowed:
                if b"200 Connection Established" not in header:
                    return False
                connection.sendall(b"ping")
                return connection.recv(4) == b"pong"
            return b"403 Forbidden" in header

    evidence = {"allowed": tunnel(os.environ["FIXTURE_ALLOWED"], True), "denied": tunnel("denied.invalid:443", False)}
    for key in list(os.environ):
        if key.lower().endswith("proxy"):
            os.environ.pop(key)
    try:
        socket.create_connection(("192.0.2.1", 443), timeout=0.2).close()
        evidence["directDenied"] = False
    except OSError:
        evidence["directDenied"] = True
    with open("egress.json", "w") as target:
        json.dump(evidence, target)
    if not all(evidence.values()):
        os._exit(78)


for line in sys.stdin:
    request = json.loads(line)
    method = request["method"]
    result = None
    if method == "initialize":
        result = {"protocolVersion": 1, "agentCapabilities": {}, "authMethods": []}
    elif method == "session/new":
        result = {"sessionId": "fixture-session"}
        if mode == "blocked-stdin":
            send({"jsonrpc": "2.0", "id": request["id"], "result": result})
            idle_tree()
    elif method == "session/prompt":
        if mode in ["complete", "hostile", "egress"]:
            if mode == "hostile":
                hostile_probes()
            if mode == "egress":
                egress_probes()
            with open("delivered.txt", "w") as target:
                target.write("feature")
            send({"jsonrpc": "2.0", "method": "session/update", "params": {
                "sessionId": "fixture-session", "update": {
                    "sessionUpdate": "agent_message_chunk",
                    "content": {"type": "text", "text": "fixture-delivery"},
                },
            }})
            send({"jsonrpc": "2.0", "id": request["id"], "result": {"stopReason": "end_turn"}})
            break
        idle_tree()
    if result is not None:
        send({"jsonrpc": "2.0", "id": request["id"], "result": result})
