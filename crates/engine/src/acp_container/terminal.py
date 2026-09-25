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

MAX_REQUEST = 32768
WALL_SECONDS = 30
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
        if reap(peer):
            return
        for pid in children():
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
        if len(data) > MAX_REQUEST or time.monotonic() >= deadline:
            raise ValueError("launch bound")
        if select.select([0], [], [], 0.05)[0]:
            byte = os.read(0, 1)
            if not byte:
                raise EOFError("launch")
            data.extend(byte)
    launch = json.loads(data)
    workspace = launch["workspace"]
    action = launch["action"]
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
    peer = subprocess.Popen([executable, *action["args"]],
                            executable="/proc/self/fd/" + str(program),
                            cwd="/proc/self/fd/" + str(cwd), env=env,
                            stdin=subprocess.DEVNULL, stdout=subprocess.PIPE,
                            stderr=subprocess.STDOUT, close_fds=True,
                            pass_fds=(cwd, program), start_new_session=True)
    os.close(program)
    os.close(cwd)
    send({"event": "started"})
    os.set_blocking(peer.stdout.fileno(), False)
    decoder = codecs.getincrementaldecoder("utf-8")("replace")
    output_eof = False
    exit_sent = False
    releasing = False
    control = bytearray()
    deadline = time.monotonic() + WALL_SECONDS
    drain_deadline = None
    while True:
        reap(peer)
        if peer.returncode is not None and drain_deadline is None:
            stop_children(peer)
            drain_deadline = time.monotonic() + CLEANUP_SECONDS
        if time.monotonic() >= deadline:
            stop_children(peer)
            raise TimeoutError("terminal wall budget")
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
                "descendantsReaped": True, "outputDrained": True})
            exit_sent = True
        if releasing and exit_sent:
            send({"event": "released"})
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
