"""The harness's one host listener: 127.0.0.1:18080, reachable from the guest as 10.0.2.2:18080 (QEMU's
user network). The demo page's picture points at it; a light view has no network, so the listener must
see nothing. It answers every request with a 404 and records it (`judge.judge_requests`).

It binds nothing the web harnesses use (80, 81, 443), so a run does not collide with
`tools/webplus/run.py`.
"""

from __future__ import annotations

import socket
import threading
import time
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

HOST = "127.0.0.1"
PORT = 18080


def busy(port: int = PORT, host: str = HOST) -> bool:
    """Something already listens there. A bind test would not do: Windows lets a second process bind a
    port that is listening, so ask by connecting."""
    with socket.socket() as probe:
        probe.settimeout(0.5)
        return probe.connect_ex((host, port)) == 0


class _Server(ThreadingHTTPServer):
    """Counts the connections it accepts, so a connection that sends nothing still shows."""

    connections = 0

    def get_request(self):
        accepted = super().get_request()
        self.connections += 1
        return accepted


class Listener:
    """Counts every request, and every connection, that reaches it, from any client."""

    def __init__(self, port: int = PORT, host: str = HOST) -> None:
        self.requests: list[dict] = []
        self.lock = lock = threading.Lock()
        requests = self.requests

        class Handler(BaseHTTPRequestHandler):
            def _answer(self) -> None:
                with lock:
                    requests.append({"method": self.command, "path": self.path, "client": self.client_address[0],
                                     "host": self.headers.get("Host", ""), "at": time.time()})
                self.send_response(404)
                self.send_header("Content-Length", "0")
                self.send_header("Connection", "close")
                self.end_headers()

            do_GET = do_HEAD = do_POST = do_PUT = do_OPTIONS = _answer  # noqa: N815

            def log_message(self, *args) -> None:  # silence the default stderr log
                pass

        self.server = _Server((host, port), Handler)
        self.server.daemon_threads = True
        self.thread = threading.Thread(target=self.server.serve_forever, daemon=True)
        self.thread.start()

    def snapshot(self) -> dict:
        """What the listener saw: `{"requests": [...], "connections": n}`."""
        with self.lock:
            return {"requests": list(self.requests), "connections": self.server.connections}

    def close(self) -> None:
        self.server.shutdown()
        self.server.server_close()
