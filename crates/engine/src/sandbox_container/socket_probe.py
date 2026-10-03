"""Synthetic socket proof; runtime failures must never become access denials."""

import errno
import ipaddress
from pathlib import Path
import socket
import sys
import time


def probe(mode, address, port, witness, nonce, receipt):
    if mode not in ("allowed", "denied"):
        raise ValueError("unknown probe mode")
    address = str(ipaddress.IPv4Address(address))
    port = int(port)
    if not 0 < port < 65536:
        raise ValueError("invalid target port")
    with socket.socket(socket.AF_INET, socket.SOCK_STREAM) as connection:
        connection.settimeout(3)
        Path(witness).write_text(nonce + "\n", encoding="utf-8")
        try:
            connection.connect((address, port))
        except OSError as error:
            # Refused/reset connections and arbitrary OS errors do not prove
            # routing isolation. A socket timeout is distinct from the host
            # supervisor timing out: the guest must finish and emit a receipt.
            expected = isinstance(error, TimeoutError) or error.errno in (
                errno.ENETUNREACH, errno.EHOSTUNREACH, errno.ETIMEDOUT
            )
            if mode != "denied" or not expected:
                raise
            print("SOCKET-DENIED:" + (errno.errorcode.get(error.errno) or "TIMEOUT"))
        else:
            if mode != "allowed":
                raise RuntimeError("direct route unexpectedly connected")
            expected = b"external\n"
            data = b""
            while len(data) < len(expected):
                chunk = connection.recv(len(expected) - len(data))
                if not chunk:
                    break
                data += chunk
            if data != expected:
                raise RuntimeError("positive target did not return its witness")
            print("SOCKET-ALLOWED")
    print(receipt, flush=True)


def serve():
    deadline = time.monotonic() + 120
    with socket.socket(socket.AF_INET, socket.SOCK_STREAM) as server:
        server.bind(("0.0.0.0", 18080))
        server.listen()
        print("SOCKET-TARGET-READY", flush=True)
        while time.monotonic() < deadline:
            server.settimeout(max(0.01, deadline - time.monotonic()))
            try:
                connection, _ = server.accept()
            except TimeoutError:
                break
            with connection:
                connection.settimeout(3)
                try:
                    connection.sendall(b"external\n")
                except OSError:
                    pass


if __name__ == "__main__":
    if sys.argv[1:] == ["serve"]:
        serve()
    else:
        probe(*sys.argv[1:])
