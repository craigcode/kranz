"""Trusted PID 1. The host lease, not agent cooperation, owns this namespace.

Run only as /usr/local/bin/python3 -I -S -u with an engine-owned read-only
control directory. No model output, workspace module or shell is evaluated here.
"""
import ctypes
import json
import os
import subprocess
import sys
import threading
import time

LEASE_SECONDS = 5.0
POLL_SECONDS = 0.1
COMPLETION_SECONDS = 1.0
stage = "initialization"


def stop(code):
    # Exiting PID 1 kills every namespace descendant, including setsid children.
    # _exit also prevents blocked daemon I/O threads from delaying termination.
    os._exit(code)


def reap_children(peer):
    # PID 1 also adopts exited background tools. Bound each sweep so even a
    # continuously forking peer cannot starve lease checks. This is the sole
    # waiter; preserve the peer status instead of racing Popen.poll().
    for _ in range(64):
        try:
            pid, status = os.waitpid(-1, os.WNOHANG)
        except ChildProcessError:
            break
        if pid == 0:
            break
        if pid == peer.pid:
            peer.returncode = os.waitstatus_to_exitcode(status)
    return peer.returncode


def main():
    global stage
    if os.getpid() != 1:
        raise RuntimeError("supervisor requires a private PID namespace")
    libc = ctypes.CDLL(None, use_errno=True)
    libc.prctl.argtypes = [ctypes.c_int, ctypes.c_ulong, ctypes.c_ulong, ctypes.c_ulong, ctypes.c_ulong]
    libc.prctl.restype = ctypes.c_int
    # The peer has the same uid. Deny ptrace and /proc/1/{mem,fd,root} access
    # before spawning it; Docker also drops every capability. Do not exec
    # after setting this flag (exec can reset dumpability).
    if libc.prctl(4, 0, 0, 0, 0) != 0:  # PR_SET_DUMPABLE
        raise OSError(ctypes.get_errno(), "cannot protect supervisor")
    root = sys.argv[1]
    stage = "first-lease"

    def read_lease():
        try:
            with open(root + "/lease", "rb") as source:
                value = source.read(9)
            return value if len(value) == 8 else None
        except OSError:
            return None  # Unreadable evidence never renews the deadline.

    # A delayed daemon start must not execute a payload under a dead owner.
    # Require a renewal observed *after* this supervisor started.
    previous = read_lease()
    changed_at = time.monotonic()
    stage = "initial-renewal"
    while True:
        current = read_lease()
        if current is not None and current != previous:
            break
        if time.monotonic() - changed_at >= LEASE_SECONDS:
            stop(124)
        time.sleep(POLL_SECONDS)
    previous = current
    changed_at = time.monotonic()
    stage = "launch-file"
    with open(root + "/launch.json", encoding="utf-8") as source:
        launch = json.load(source)
    stage = "peer-spawn"
    peer = subprocess.Popen(
        launch["argv"], cwd=launch["cwd"], env=launch["env"],
        stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.PIPE,
        close_fds=True, start_new_session=True, bufsize=0,
    )
    failed = threading.Event()
    input_eof = threading.Event()
    stage = "supervision"

    def forward(source, target, close_target=False):
        reached_eof = False
        try:
            while True:
                chunk = os.read(source, 65536)
                if not chunk:
                    reached_eof = True
                    break
                while chunk:
                    written = os.write(target, chunk)
                    if written <= 0:
                        raise OSError("short pipe write")
                    chunk = chunk[written:]
        except (BrokenPipeError, OSError):
            # Agent-stdin closure is normal after a single-shot response.
            # Lost output cannot be presented as a successful run.
            if not close_target:
                failed.set()
        finally:
            if close_target:
                os.close(target)
                if reached_eof:
                    input_eof.set()

    input_thread = threading.Thread(
        target=forward, args=(0, peer.stdin.fileno(), True), daemon=True,
    )
    outputs = [
        threading.Thread(target=forward, args=(peer.stdout.fileno(), 1), daemon=True),
        threading.Thread(target=forward, args=(peer.stderr.fileno(), 2), daemon=True),
    ]
    input_thread.start()
    for thread in outputs:
        thread.start()
    ended_at = None
    eof_at = None
    code = None
    while True:
        current = read_lease()
        now = time.monotonic()
        if current is not None and current != previous:
            previous, changed_at = current, now
        if now - changed_at >= LEASE_SECONDS:
            stop(124)
        if failed.is_set():
            stop(125)
        observed = reap_children(peer)
        if code is None:
            code = observed
            if code is not None:
                ended_at = now
        if ended_at is not None:
            if all(not thread.is_alive() for thread in outputs):
                stop(code if 0 <= code <= 255 else 125)
            # A detached child holding output open must not outlive the peer.
            if now - ended_at >= 1.0:
                stop(125)
        elif input_eof.is_set():
            if eof_at is None:
                eof_at = now
            if now - eof_at >= COMPLETION_SECONDS:
                # Host stdin EOF ends a completed single-shot session. The
                # trusted supervisor owns intentional shutdown, so the host
                # never mistakes a killed Docker client for successful work.
                # Check the peer directly before stopping: an orphan backlog
                # must not hide an already-earned nonzero peer exit.
                pid, status = os.waitpid(peer.pid, os.WNOHANG)
                if pid == peer.pid:
                    peer.returncode = os.waitstatus_to_exitcode(status)
                    code, ended_at = peer.returncode, now
                else:
                    stop(0)
        time.sleep(POLL_SECONDS)


try:
    main()
except BaseException as failure:
    # No credential-bearing launch data or exception text crosses stderr.
    os.write(2, ("ACP supervisor refused at " + stage + ": " + type(failure).__name__ + "\n").encode("ascii"))
    stop(125)
