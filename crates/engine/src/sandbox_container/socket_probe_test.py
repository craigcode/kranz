"""Exercise the real guest probe's classification without Docker or a provider."""

import contextlib
import errno
import io
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch

import socket_probe


class SocketProofTest(unittest.TestCase):
    def attempt(self, mode, error=None, data=b"external\n", address="192.0.2.1"):
        with tempfile.TemporaryDirectory() as root:
            witness = Path(root) / "started"
            output = io.StringIO()
            with patch.object(socket_probe.socket, "socket") as factory:
                connection = factory.return_value.__enter__.return_value
                connection.connect.side_effect = error
                connection.recv.return_value = data
                caught = None
                with contextlib.redirect_stdout(output):
                    try:
                        socket_probe.probe(mode, address, "18080", witness, "nonce", "receipt")
                    except Exception as error:
                        caught = error
                return caught, output.getvalue(), witness.exists()

    def test_route_errors_require_completed_guest_receipts(self):
        for error in (OSError(errno.ENETUNREACH, "route"),
                      OSError(errno.EHOSTUNREACH, "host"),
                      OSError(errno.ETIMEDOUT, "timeout"), TimeoutError()):
            with self.subTest(error=error):
                caught, output, started = self.attempt("denied", error)
                self.assertIsNone(caught)
                self.assertTrue(started)
                self.assertIn("SOCKET-DENIED:", output)
                self.assertTrue(output.endswith("receipt\n"))

    def test_arbitrary_errors_are_not_denials(self):
        for code in (errno.ECONNREFUSED, errno.ECONNRESET, errno.EBADF, errno.EMFILE):
            with self.subTest(code=code):
                caught, output, started = self.attempt("denied", OSError(code, "unexpected"))
                self.assertIsInstance(caught, OSError)
                self.assertTrue(started)
                self.assertNotIn("receipt", output)

    def test_unexpected_connection_fails_denial(self):
        caught, output, started = self.attempt("denied")
        self.assertIsInstance(caught, RuntimeError)
        self.assertTrue(started)
        self.assertNotIn("receipt", output)

    def test_positive_control_requires_target_bytes(self):
        caught, output, started = self.attempt("allowed")
        self.assertIsNone(caught)
        self.assertTrue(started)
        self.assertEqual(output, "SOCKET-ALLOWED\nreceipt\n")
        for data in (b"", b"wrong!!!"):
            caught, output, _ = self.attempt("allowed", data=data)
            self.assertIsInstance(caught, RuntimeError)
            self.assertNotIn("receipt", output)

    def test_positive_control_cannot_accept_network_failure(self):
        caught, output, _ = self.attempt("allowed", TimeoutError())
        self.assertIsInstance(caught, TimeoutError)
        self.assertNotIn("receipt", output)

    def test_invalid_address_does_not_claim_guest_start(self):
        caught, output, started = self.attempt("denied", address="not-an-ip")
        self.assertIsInstance(caught, ValueError)
        self.assertFalse(started)
        self.assertNotIn("receipt", output)


if __name__ == "__main__":
    unittest.main()
