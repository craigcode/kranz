"""One trusted terminal supervisor inside the already-owned worker namespace.

Started only by the host over Docker exec, never a host execution bridge. The
read-only script consumes a bounded request on stdin; output is structured and
bounded by the host. A non-dumpable subreaper owns detached descendants too.
"""
import codecs
import ctypes
import json
import os
import select
import signal
import subprocess
import sys
import time

MAX_REQUEST = 32768 + 4096
CLEANUP_SECONDS = 3


def send(value):
    data = (json.dumps(value, ensure_ascii=True) + "\n").encode("ascii")
    while data:
        written = os.write(1, data)
        if written <= 0:
            raise OSError("short output")
        data = data[written:]


def children():
    # Linux supplies the kernel's direct-child list for this single-threaded
    # subreaper. Reparented descendants appear here even after setsid/double-fork.
    with open("/proc/self/task/" + str(os.getpid()) + "/children") as source:
        return [int(pid) for pid in source.read(32768).split()]


def reap(peer):
    for _ in range(512):
        try:
            pid, status = os.waitpid(-1, os.WNOHANG)
        except ChildProcessError:
            return True
        if pid == 0:
            return False
        if pid == peer.pid:
            peer.returncode = os.waitstatus_to_exitcode(status)
    return False


def stop_children(peer):
    deadline = time.monotonic() + CLEANUP_SECONDS
    while time.monotonic() < deadline:
        peer_was_reaped = peer.returncode is not None
        if reap(peer):
            return
        before = set(children())
        try:
            os.killpg(peer.pid, signal.SIGKILL)
        except (ProcessLookupError, PermissionError):
            pass
        for pid in before:
            try:
                fd = os.pidfd_open(pid)
                try:
                    # Verify ownership after acquiring the non-reusable handle.
                    # An exited pidfd cannot signal a newly reused numeric PID.
                    if pid in children():
                        signal.pidfd_send_signal(fd, signal.SIGKILL)
                finally:
                    os.close(fd)
            except ProcessLookupError:
                pass
        if reap(peer):
            return
        after = set(children())
        peer_reaped_now = (not peer_was_reaped) and (peer.returncode is not None)
        if not peer_reaped_now and after == before:
            time.sleep(0.01)
    raise TimeoutError("descendant cleanup")


def open_directory(path):
    if not path.startswith("/") or "\x00" in path:
        raise ValueError("cwd")
    components = path.split("/")[1:]
    if any(part in ("", ".", "..") for part in components):
        raise ValueError("cwd components")
    fd = os.open("/", os.O_RDONLY | os.O_DIRECTORY | os.O_CLOEXEC)
    try:
        for component in components:
            child = os.open(component, os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW | os.O_CLOEXEC, dir_fd=fd)
            os.close(fd)
            fd = child
        return fd
    except BaseException:
        os.close(fd)
        raise


def main():
    libc = ctypes.CDLL(None, use_errno=True)
    libc.prctl.argtypes = [ctypes.c_int, ctypes.c_ulong, ctypes.c_ulong, ctypes.c_ulong, ctypes.c_ulong]
    libc.prctl.restype = ctypes.c_int
    for operation, value in [(4, 0), (36, 1)]:  # non-dumpable, child subreaper
        if libc.prctl(operation, value, 0, 0, 0) != 0:
            raise OSError("supervision primitive")
    if not hasattr(os, "pidfd_open") or not hasattr(signal, "pidfd_send_signal"):
        raise RuntimeError("pidfd required")

    deadline = time.monotonic() + 5
    data = bytearray()
    while not data.endswith(b"\n"):
        if len(data) >= MAX_REQUEST or time.monotonic() >= deadline:
            raise ValueError("launch bound")
        if select.select([0], [], [], 0.05)[0]:
            byte = os.read(0, 1)
            if not byte:
                raise EOFError("launch")
            data.extend(byte)
    launch = json.loads(data)
    workspace = launch["workspace"]
    action = launch["action"]
    budgets = launch["budgets"]
    command_ms = budgets["commandMs"]
    retention_ms = budgets["retentionMs"]
    session_ms = launch["sessionRemainingMs"]
    for value, maximum in [(command_ms, 600000), (retention_ms, 300000), (session_ms, 600000)]:
        if type(value) is not int or not 0 < value <= maximum:
            raise ValueError("budget")
    session_deadline = time.monotonic() + session_ms / 1000
    # The initial fixture contract accepts only the bound mount root. Nested
    # cwd support needs a separate pinned-directory admission path.
    if action["cwd"] != workspace:
        raise ValueError("foreign cwd")
    cwd = open_directory(workspace)
    executable = action["command"]
    if executable not in ("/bin/sh", "/usr/local/bin/python3"):
        raise ValueError("unqualified executable")
    resolved = os.path.realpath(executable)
    if not (resolved.startswith("/usr/bin/") or resolved.startswith("/usr/local/bin/")):
        raise ValueError("executable escaped immutable image")
    program = os.open(resolved, os.O_RDONLY | os.O_NOFOLLOW | os.O_CLOEXEC)
    env = {"PATH": "/usr/local/bin:/usr/bin:/bin", "LANG": "C.UTF-8",
           "HOME": launch["scratch"], "TMPDIR": launch["scratch"]}
    for item in action["env"]:
        if item["name"] not in ("LANG", "LC_ALL", "TERM", "NO_COLOR", "CI"):
            raise ValueError("environment policy")
        env[item["name"]] = item["value"]
    # This supervisor is single-threaded. Set the hint in the child before
    # untrusted code runs and read it back through a private, close-before-exec
    # pipe. Failure leaves an explicit unavailable observation, not an OOM claim.
    hint_read, hint_write = os.pipe()

    def child_hint():
        try:
            os.fchdir(cwd)
            os.close(cwd)
            os.set_inheritable(program, False)
        except BaseException:
            os._exit(125)
        observed = None
        hint_fd = None
        try:
            # Fork inherits the non-dumpable supervisor posture, which makes
            # procfs ownership root-only. Only this command child temporarily
            # uses the ordinary exec posture to open its own hint file. Restore
            # protection before readback; the supervisor itself stays protected.
            if libc.prctl(4, 1, 0, 0, 0) != 0:
                raise OSError("child procfs access")
            try:
                hint_fd = os.open("/proc/self/oom_score_adj", os.O_RDWR | os.O_CLOEXEC)
            finally:
                if libc.prctl(4, 0, 0, 0, 0) != 0:
                    os._exit(125)
            os.write(hint_fd, b"500")
            os.lseek(hint_fd, 0, os.SEEK_SET)
            observed = int(os.read(hint_fd, 32))
        except (OSError, ValueError):
            pass
        finally:
            if hint_fd is not None:
                os.close(hint_fd)
        os.write(hint_write, json.dumps(observed).encode("ascii"))
        os.close(hint_write)

    peer = subprocess.Popen([executable, *action["args"]],
                            executable="/proc/self/fd/" + str(program),
                            cwd=None, env=env,
                            stdin=subprocess.DEVNULL, stdout=subprocess.PIPE,
                            stderr=subprocess.STDOUT, close_fds=True,
                            pass_fds=(program, hint_write), start_new_session=True,
                            preexec_fn=child_hint)
    os.close(hint_write)
    observed_hint = json.loads(os.read(hint_read, 32))
    os.close(hint_read)
    os.close(program)
    os.close(cwd)
    send({"event": "started", "oomScoreAdj": {"requested": 500, "observed": observed_hint}})
    os.set_blocking(peer.stdout.fileno(), False)
    decoder = codecs.getincrementaldecoder("utf-8")("replace")
    output_eof = False
    exit_sent = False
    releasing = False
    control = bytearray()
    command_deadline = min(session_deadline, time.monotonic() + command_ms / 1000)
    retention_deadline = None
    budget = None
    retention_expired = False
    expired_budget = None
    drain_deadline = None
    while True:
        reap(peer)
        if peer.returncode is not None and drain_deadline is None:
            stop_children(peer)
            drain_deadline = time.monotonic() + CLEANUP_SECONDS
        now = time.monotonic()
        if peer.returncode is None and now >= command_deadline:
            budget = "sessionLifetime" if now >= session_deadline else "commandLifetime"
            stop_children(peer)
            drain_deadline = time.monotonic() + CLEANUP_SECONDS
        now = time.monotonic()
        if exit_sent and not retention_expired and now >= retention_deadline:
            retention_expired = True
            expired_budget = "sessionLifetime" if now >= session_deadline else "retention"
            releasing = True
        if releasing and exit_sent:
            send({"event": "released", "retentionExpired": retention_expired, "budget": expired_budget})
            return
        if drain_deadline is not None and not output_eof and time.monotonic() >= drain_deadline:
            raise TimeoutError("output drain")
        readers = [0] + ([] if output_eof else [peer.stdout.fileno()])
        ready, _, _ = select.select(readers, [], [], 0.01)
        if peer.stdout.fileno() in ready:
            chunk = os.read(peer.stdout.fileno(), 4096)
            text = decoder.decode(chunk, final=not chunk)
            if text:
                send({"event": "output", "text": text})
            if not chunk:
                output_eof = True
        if 0 in ready:
            chunk = os.read(0, 4096)
            if not chunk:
                stop_children(peer)
                raise EOFError("owner disconnected")
            control.extend(chunk)
            if len(control) > 4096:
                raise ValueError("control bound")
            while b"\n" in control:
                line, _, control = control.partition(b"\n")
                request = json.loads(line)
                if request == {"op": "kill"} or request == {"op": "release"}:
                    stop_children(peer)
                    if drain_deadline is None:
                        drain_deadline = time.monotonic() + CLEANUP_SECONDS
                    releasing = releasing or request["op"] == "release"
                else:
                    raise ValueError("control operation")
        if peer.returncode is not None and output_eof and not exit_sent:
            stop_children(peer)
            code = peer.returncode
            send({"event": "exit", "status": {
                "exitCode": code if code >= 0 else None,
                "signal": signal.Signals(-code).name if code < 0 else None},
                "descendantsReaped": True, "outputDrained": True, "budget": budget})
            exit_sent = True
            retention_deadline = min(session_deadline, time.monotonic() + retention_ms / 1000)
        if releasing and exit_sent:
            send({"event": "released", "retentionExpired": retention_expired, "budget": expired_budget})
            return


try:
    main()
except BaseException:
    # The host treats any missing/failed receipt as uncertain and ends the
    # entire owned namespace. Never expose launch data or exception arguments.
    try:
        send({"event": "failed"})
    finally:
        os._exit(125)
