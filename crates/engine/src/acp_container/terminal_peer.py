"""Provider-free adversarial ACP terminal peer; never used by a real profile."""
import json
import os
import sys
import time


def send(value):
    print(json.dumps(dict(jsonrpc="2.0", **value)), flush=True)


def receive():
    return json.loads(sys.stdin.readline())


def request(number, method, **params):
    send(dict(id=number, method=method, params=dict(sessionId="terminal-peer", **params)))


def response(number):
    value = receive()
    assert value["id"] == number, value
    return value


def target(number, method, handle):
    request(number, "terminal/" + method, terminalId=handle)


def create(number, script):
    request(number, "terminal/create", command="/usr/local/bin/python3",
            args=["-I", "-S", "-c", script], outputByteLimit=7)


def run():
    mode = os.environ["FIXTURE_MODE"]
    init = receive()
    assert init["params"]["clientCapabilities"]["terminal"] == (mode != "disabled")
    send(dict(id=init["id"], result=dict(protocolVersion=1, agentCapabilities={})))
    session = receive()
    send(dict(id=session["id"], result=dict(sessionId="terminal-peer")))
    prompt = receive()
    if mode == "disabled":
        create(100, 'open("unapproved", "w").write("bad")')
        assert response(100)["error"]["code"] == -32601
    elif mode == "paths":
        for number, extra in enumerate([
                dict(cwd="/"), dict(cwd=os.getcwd() + "/../workspace"),
                dict(cwd=os.getcwd() + "/escape"),
                dict(env=[dict(name="HOME", value="/tmp")]),
                dict(env=[dict(name="LD_PRELOAD", value="/tmp/evil")]),
                dict(command=os.getcwd() + "/fake-executable"),
                dict(outputByteLimit=1048577), dict(sessionId="foreign")], 200):
            params = dict(sessionId="terminal-peer", command="/bin/sh", args=["-c", "touch unapproved"])
            params.update(extra)
            send(dict(id=number, method="terminal/create", params=params))
            assert "error" in response(number)
        assert not os.path.exists("unapproved")
    elif mode == "budget-expiry":
        adapter_hint = int(open("/proc/self/oom_score_adj").read())
        create(100, r'''
import os, time
fds = {int(n) for n in os.listdir("/proc/self/fd")}
extra = fds - {0, 1, 2}
assert len(extra) == 1, fds
for fd in extra:
    try:
        os.fstat(fd)
    except OSError:
        pass
    else:
        raise AssertionError(f"leaked fd {fd}")
assert int(open("/proc/self/oom_score_adj").read()) == 500
if os.fork() == 0:
    os.setsid()
    assert int(open("/proc/self/oom_score_adj").read()) == 500
    open("descendant-pid", "w").write(str(os.getpid()))
    while True:
        open("heartbeat", "w").write(str(time.monotonic()))
        time.sleep(0.02)
print("ready", flush=True)
time.sleep(120)
''')
        handle = response(100)["result"]["terminalId"]
        target(101, "wait_for_exit", handle)
        error = response(101)["error"]["data"]
        assert error["kind"] == "terminalBudget" and error["budget"] == "commandLifetime", error
        assert error["exitStatus"]["signal"] == "SIGKILL", error
        assert error["descendantsReaped"] and error["outputDrained"], error
        assert int(open("/proc/self/oom_score_adj").read()) == adapter_hint
        assert not os.path.exists("/proc/" + open("descendant-pid").read())
        target(102, "output", handle)
        assert response(102)["result"]["output"] == "ready\n"
        beat = open("heartbeat").read()
        time.sleep(3)
        assert open("heartbeat").read() == beat
        target(103, "output", handle)
        assert response(103)["error"]["data"]["budget"] == "retention"
        create(104, 'print("done")')
        handle = response(104)["result"]["terminalId"]
        target(105, "wait_for_exit", handle)
        assert response(105)["result"]["exitCode"] == 0
        target(106, "release", handle)
        assert response(106)["result"] == {}
    elif mode == "budget-capacity":
        for number in range(100, 109):
            create(number, 'open("unapproved", "w").write("bad")')
        assert response(108)["error"]["data"]["budget"] == "capacity"
        denied = [receive() for _ in range(8)]
        assert {value["id"] for value in denied} == set(range(100, 108))
        assert all("error" in value for value in denied)
        assert not os.path.exists("unapproved")
        create(200, 'print("done")')
        handle = response(200)["result"]["terminalId"]
        target(201, "wait_for_exit", handle)
        assert response(201)["result"]["exitCode"] == 0
        target(202, "release", handle)
        assert response(202)["result"] == {}
    elif mode == "budget-controls":
        create(100, 'import time; print("ready", flush=True); time.sleep(120)')
        handle = response(100)["result"]["terminalId"]
        refused = 0
        # Distinct polls cannot spend the 1,024 consent identity slots,
        # including recoverable rate-limit refusals. Subsequent waits and the
        # next create reuse some of these completed control IDs.
        for number in range(101, 1201):
            target(number, "output", handle)
            value = response(number)
            if "error" in value:
                assert value["error"]["data"]["budget"] == "outputRate", value
                refused += 1
        assert refused > 0
        for number in range(1000, 1033):
            target(number, "wait_for_exit", handle)
        assert response(1032)["error"]["data"]["budget"] == "pendingControl"
        # All 32 control slots are full, but human consent and create still work.
        create(200, 'print("done")')
        second = response(200)["result"]["terminalId"]
        values = [receive() for _ in range(32)]
        assert {value["id"] for value in values} == set(range(1000, 1032))
        assert all(value["error"]["data"]["budget"] == "commandLifetime" for value in values)
        target(201, "release", handle)
        assert response(201)["result"] == {}
        # Retention (60s) exceeds the first command's lifetime (15s), so release succeeds deterministically.
        target(202, "release", second)
        assert response(202)["result"] == {}
    else:
        script = r'''
import os, time
if os.fork() == 0:
    os.setsid()
    if os.fork() != 0:
        os._exit(0)
    open("descendant-pid", "w").write(str(os.getpid()))
    while True:
        open("heartbeat", "w").write(str(time.monotonic()))
        time.sleep(0.02)
while not os.path.exists("heartbeat"):
    time.sleep(0.01)
os.write(1, ("🦀" * 100000 + "雪é").encode())
open("command-ready", "w").close()
time.sleep(120)
'''
        create(100, script)
        if mode == "late-create":
            send(dict(id=prompt["id"], result=dict(stopReason="end_turn")))
            time.sleep(120)
        handle = response(100)["result"]["terminalId"]
        while not os.path.exists("command-ready"):
            time.sleep(0.01)
        if mode == "provider-death":
            # The parent of our terminal command is its trusted subreaper.
            # Same-uid signals are possible; losing that driver must fail closed.
            for name in os.listdir("/proc"):
                if name.isdigit():
                    try:
                        cmd = open("/proc/" + name + "/cmdline", "rb").read()
                        if b"/kranz-owned-session/terminal.py" in cmd:
                            os.kill(int(name), 9)
                    except (FileNotFoundError, PermissionError, ProcessLookupError):
                        pass
            time.sleep(120)
        if mode == "peer-death":
            os._exit(3)
        if mode == "drop":
            time.sleep(120)
        target(101, "wait_for_exit", handle)
        create(102, 'open("unapproved", "w").write("bad")')
        target(103, "output", handle)
        target(104, "kill", handle)
        received = {}
        while not {101, 103, 104}.issubset(received):
            value = receive()
            received[value["id"]] = value
        assert received[104]["result"] == {}
        assert received[101]["result"]["signal"] == "SIGKILL"
        if 102 not in received:
            received[102] = response(102)
        assert "error" in received[102]
        assert not os.path.exists("unapproved")
        target(105, "output", handle)
        output = response(105)["result"]
        assert output["truncated"] is True and output["output"] == "雪é", output
        assert len(output["output"].encode()) <= 7
        assert output["exitStatus"]["signal"] == "SIGKILL"
        beat = open("heartbeat").read()
        time.sleep(0.1)
        assert open("heartbeat").read() == beat
        assert not os.path.exists("/proc/" + open("descendant-pid").read())
        target(106, "release", handle)
        assert response(106)["result"] == {}
        target(107, "output", handle)
        assert "error" in response(107)
        target(108, "kill", "forged")
        assert "error" in response(108)
        send(dict(id=109, method="terminal/output", params=dict(sessionId="foreign", terminalId=handle)))
        assert "error" in response(109)
        create(110, r'''
import os
assert "FIXTURE_MODE" not in os.environ
assert os.path.exists("terminal_peer.py")
fds = {int(n) for n in os.listdir("/proc/self/fd")}
extra = fds - {0, 1, 2}
assert len(extra) == 1, fds
for fd in extra:
    try:
        os.fstat(fd)
    except OSError:
        pass
    else:
        raise AssertionError(f"leaked fd {fd}")
print("done")
''')
        second = response(110)["result"]["terminalId"]
        target(111, "wait_for_exit", second)
        assert response(111)["result"]["exitCode"] == 0
        target(112, "release", second)
        assert response(112)["result"] == {}
    send(dict(method="session/update", params=dict(sessionId="terminal-peer", update=dict(
        sessionUpdate="agent_message_chunk", content=dict(type="text", text="terminal-proof-ok")))))
    send(dict(id=prompt["id"], result=dict(stopReason="end_turn")))
    for _ in sys.stdin:
        pass


run()
