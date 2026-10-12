"""The server's connection limits: an idle connection is dropped, a connection
past the bound is closed at accept (docs/netboot-plan.md section 5; the
registry listens on a LAN port, so a client that never finishes a request must
not hold a thread for good)."""

from __future__ import annotations

import socket
import sys
import tempfile
import threading
import time
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))

import registry  # noqa: E402


def connect(port: int) -> socket.socket:
    return socket.create_connection(("127.0.0.1", port), timeout=5)


def closed_by_peer(sock: socket.socket, within: float) -> bool:
    """True when the server closes the connection within `within` seconds."""
    sock.settimeout(within)
    try:
        return sock.recv(1) == b""
    except (TimeoutError, socket.timeout):
        return False
    except OSError:
        return True


class LimitTests(unittest.TestCase):
    def start(self, **kwargs) -> int:
        root = Path(tempfile.mkdtemp())
        server = registry.serve(root, "127.0.0.1:0", None, 10, lambda line: None, None, time.time, **kwargs)
        thread = threading.Thread(target=server.serve_forever, daemon=True)
        thread.start()
        self.addCleanup(server.server_close)
        self.addCleanup(server.shutdown)
        return server.server_address[1]

    def test_idle_connection_is_closed(self):
        port = self.start(idle_timeout=0.5)
        sock = connect(port)
        self.addCleanup(sock.close)
        self.assertTrue(closed_by_peer(sock, 5.0), "an idle connection stayed open past the timeout")

    def test_a_half_request_is_closed(self):
        port = self.start(idle_timeout=0.5)
        sock = connect(port)
        self.addCleanup(sock.close)
        sock.sendall(b"GET /healthz HTTP/1.1\r\nHost: x\r\n")  # never finished
        self.assertTrue(closed_by_peer(sock, 5.0))

    def test_connections_past_the_bound_are_refused(self):
        port = self.start(max_connections=2, idle_timeout=30)
        held = [connect(port) for _ in range(2)]
        for sock in held:
            self.addCleanup(sock.close)
        time.sleep(0.2)  # let both take their slots
        extra = connect(port)
        self.addCleanup(extra.close)
        self.assertTrue(closed_by_peer(extra, 5.0), "a connection past the bound was not closed")
        # a slot comes back once a held connection ends
        held[0].close()
        time.sleep(0.2)
        again = connect(port)
        self.addCleanup(again.close)
        again.sendall(b"GET /healthz HTTP/1.1\r\nHost: x\r\nConnection: close\r\n\r\n")
        again.settimeout(5)
        self.assertIn(b"200", again.recv(64))

    def test_defaults_are_sane(self):
        self.assertGreaterEqual(registry.MAX_CONNECTIONS, 16)
        self.assertGreaterEqual(registry.IDLE_TIMEOUT_S, 10)


if __name__ == "__main__":
    unittest.main()
