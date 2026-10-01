"""The host's side of the socket evidence (stage N3): echo servers the guest
connects to through the gateway, and a client that connects into the guest.

QEMU's user networking maps the gateway address (10.0.2.2) to the host's
loopback, so a guest `nc 10.0.2.2 47771` reaches `EchoServers.tcp_port` here.
Everything a server receives is recorded, so the harness can compare it with
what the capture shows crossed the wire. The guest's listener is reached the
other way, through the `hostfwd` rule `tools/net/run.py` adds to the netdev.
"""

from __future__ import annotations

import socket
import threading
import time

TCP_PORT = 47771
UDP_PORT = 47772
#: The port the guest's `nc -l` listens on.
GUEST_LISTEN_PORT = 47773


def pattern(n: int, seed: int = 0x5EED) -> bytes:
    """`n` deterministic, non-repeating-looking bytes (an LCG)."""
    out = bytearray(n)
    state = seed
    for i in range(n):
        state = (state * 1103515245 + 12345) & 0x7FFFFFFF
        out[i] = (state >> 16) & 0xFF
    return bytes(out)


class EchoServers:
    """A TCP and a UDP echo server on the loopback, recording what they get."""

    def __init__(self, tcp_port: int = TCP_PORT, udp_port: int = UDP_PORT) -> None:
        self.tcp_streams: list[bytes] = []
        self.udp_datagrams: list[bytes] = []
        self._lock = threading.Lock()
        self._stop = threading.Event()
        self._tcp = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
        self._tcp.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
        self._tcp.bind(("127.0.0.1", tcp_port))
        self._tcp.listen(16)
        self._tcp.settimeout(0.2)
        self._udp = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
        self._udp.bind(("127.0.0.1", udp_port))
        self._udp.settimeout(0.2)
        self._threads = [
            threading.Thread(target=self._accept_loop, daemon=True),
            threading.Thread(target=self._udp_loop, daemon=True),
        ]
        for thread in self._threads:
            thread.start()

    def _accept_loop(self) -> None:
        while not self._stop.is_set():
            try:
                conn, _ = self._tcp.accept()
            except socket.timeout:
                continue
            except OSError:
                return
            threading.Thread(target=self._serve, args=(conn,), daemon=True).start()

    def _serve(self, conn: socket.socket) -> None:
        received = bytearray()
        conn.settimeout(30)
        try:
            while True:
                chunk = conn.recv(65536)
                if not chunk:
                    break
                received += chunk
                conn.sendall(chunk)
        except OSError:
            pass
        finally:
            conn.close()
            with self._lock:
                self.tcp_streams.append(bytes(received))

    def _udp_loop(self) -> None:
        while not self._stop.is_set():
            try:
                data, peer = self._udp.recvfrom(65536)
            except socket.timeout:
                continue
            except OSError:
                return
            with self._lock:
                self.udp_datagrams.append(data)
            try:
                self._udp.sendto(data, peer)
            except OSError:
                pass

    def snapshot(self) -> tuple[list[bytes], list[bytes]]:
        with self._lock:
            return list(self.tcp_streams), list(self.udp_datagrams)

    def close(self) -> None:
        self._stop.set()
        for sock in (self._tcp, self._udp):
            try:
                sock.close()
            except OSError:
                pass


class InboundProbe:
    """Connect into the guest's listener once it says it is listening, send a
    payload and expect the same bytes back."""

    def __init__(self, forwarded_port: int, payload: bytes) -> None:
        self.port = forwarded_port
        self.payload = payload
        self.echoed = b""
        self.error: str | None = None
        self.done = threading.Event()
        self._thread: threading.Thread | None = None

    def start(self) -> None:
        if self._thread is None:
            self._thread = threading.Thread(target=self._run, daemon=True)
            self._thread.start()

    def _run(self) -> None:
        try:
            deadline = time.time() + 15
            conn = None
            while conn is None:
                try:
                    conn = socket.create_connection(("127.0.0.1", self.port), timeout=10)
                except OSError as exc:
                    if time.time() > deadline:
                        raise
                    time.sleep(0.2)
                    del exc
            conn.settimeout(30)
            sender = threading.Thread(target=lambda: self._send(conn), daemon=True)
            sender.start()
            got = bytearray()
            while len(got) < len(self.payload):
                chunk = conn.recv(65536)
                if not chunk:
                    break
                got += chunk
            self.echoed = bytes(got)
            sender.join(timeout=30)
            conn.shutdown(socket.SHUT_WR)
            while conn.recv(65536):
                pass
            conn.close()
        except OSError as exc:
            self.error = f"{type(exc).__name__}: {exc}"
        finally:
            self.done.set()

    def _send(self, conn: socket.socket) -> None:
        try:
            conn.sendall(self.payload)
        except OSError as exc:
            self.error = f"send: {exc}"

    def verdict(self) -> list[str]:
        problems = []
        if self.error:
            problems.append(self.error)
        if self.echoed != self.payload:
            problems.append(f"the guest echoed {len(self.echoed)} of {len(self.payload)} bytes"
                            + ("" if len(self.echoed) != len(self.payload) else ", and they differ"))
        return problems


# ---- the FTP server (stage N4) -------------------------------------------------

FTP_PORT = 47780
FTP_USER, FTP_PASS = "lazy", "os"
#: The address the guest uses for the host (QEMU maps it to the loopback).
GATEWAY = (10, 0, 2, 2)


def xorshift_pattern(n: int) -> bytes:
    """The stream `nc -g` and `ftp put -g` send (`ftpwire::Pattern`): an
    xorshift64 generator, one byte from bits 24..32 of each step."""
    mask = (1 << 64) - 1
    state = 0x9E3779B97F4A7C15
    out = bytearray(n)
    for i in range(n):
        state ^= (state << 13) & mask
        state ^= state >> 7
        state ^= (state << 17) & mask
        out[i] = (state >> 24) & 0xFF
    return bytes(out)


class FtpServer:
    """A small passive-mode FTP server on the loopback that records everything:
    each command, each upload, and each data transfer with the port it used, so
    the harness can find that connection in the capture."""

    def __init__(self, port: int = FTP_PORT, files: dict[str, bytes] | None = None) -> None:
        self.files: dict[str, bytes] = dict(files or {})
        self.commands: list[tuple[str, str]] = []
        #: (passive port, "down" | "up", bytes) per data transfer.
        self.transfers: list[tuple[int, str, bytes]] = []
        self.uploads: dict[str, bytes] = {}
        self._lock = threading.Lock()
        self._stop = threading.Event()
        self._listener = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
        self._listener.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
        self._listener.bind(("127.0.0.1", port))
        self._listener.listen(8)
        self._listener.settimeout(0.2)
        threading.Thread(target=self._accept_loop, daemon=True).start()

    def _accept_loop(self) -> None:
        while not self._stop.is_set():
            try:
                conn, _ = self._listener.accept()
            except socket.timeout:
                continue
            except OSError:
                return
            threading.Thread(target=self._session, args=(conn,), daemon=True).start()

    def _record(self, verb: str, arg: str) -> None:
        with self._lock:
            self.commands.append((verb, arg))

    def _session(self, conn: socket.socket) -> None:
        conn.settimeout(30)
        reader = conn.makefile("rb")

        def say(text: str) -> None:
            conn.sendall(text.encode() + b"\r\n")

        passive: socket.socket | None = None
        cwd = "/"
        user_ok = logged_in = False
        try:
            say("220 lazyos test ftp server")
            while True:
                raw = reader.readline(2048)
                if not raw:
                    break
                line = raw.decode("latin-1").rstrip("\r\n")
                verb, _, arg = line.partition(" ")
                verb = verb.upper()
                self._record(verb, arg)
                if verb == "USER":
                    user_ok = arg == FTP_USER
                    say("331 password please")
                elif verb == "PASS":
                    logged_in = user_ok and arg == FTP_PASS
                    say("230 welcome" if logged_in else "530 login incorrect")
                elif verb == "QUIT":
                    say("221 bye")
                    break
                elif not logged_in:
                    say("530 log in first")
                elif verb == "SYST":
                    say("215 UNIX Type: L8")
                elif verb == "TYPE":
                    say("200 type set")
                elif verb == "PWD":
                    say(f'257 "{cwd}" is the current directory')
                elif verb == "CWD":
                    target = arg.strip("/")
                    if target in ("", "pub", ".."):
                        cwd = "/" if target in ("", "..") else "/pub"
                        say("250 directory changed")
                    else:
                        say("550 no such directory")
                elif verb == "SIZE":
                    data = self.files.get(arg.lstrip("/"))
                    say(f"213 {len(data)}" if data is not None else "550 no such file")
                elif verb == "PASV":
                    if passive is not None:
                        passive.close()
                    passive = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
                    passive.bind(("127.0.0.1", 0))
                    passive.listen(1)
                    passive.settimeout(20)
                    port = passive.getsockname()[1]
                    h = ",".join(str(o) for o in GATEWAY)
                    say(f"227 Entering Passive Mode ({h},{port >> 8},{port & 255}).")
                elif verb in ("LIST", "NLST", "RETR", "STOR"):
                    if passive is None:
                        say("425 use PASV first")
                        continue
                    self._transfer(conn, say, passive, verb, arg)
                    passive = None
                else:
                    say("502 not implemented")
        except (OSError, ValueError):
            pass
        finally:
            if passive is not None:
                passive.close()
            conn.close()

    def _transfer(self, conn, say, passive, verb: str, arg: str) -> None:
        name = arg.lstrip("/")
        if verb == "RETR" and (".." in name or name not in self.files):
            say("550 no such file")
            passive.close()
            return
        if verb == "STOR" and (".." in name or not name):
            say("553 bad file name")
            passive.close()
            return
        port = passive.getsockname()[1]
        say("150 opening data connection")
        try:
            data_conn, _ = passive.accept()
        except OSError:
            say("425 no data connection")
            return
        finally:
            passive.close()
        data_conn.settimeout(30)
        try:
            if verb in ("LIST", "NLST"):
                body = "".join(f"-rw-r--r-- 1 lazy lazy {len(v):8d} Jan  1 00:00 {k}\r\n"
                               for k, v in sorted(self.files.items())).encode()
                data_conn.sendall(body)
                data_conn.shutdown(socket.SHUT_WR)
                with self._lock:
                    self.transfers.append((port, "down", body))
            elif verb == "RETR":
                body = self.files[name]
                data_conn.sendall(body)
                data_conn.shutdown(socket.SHUT_WR)
                with self._lock:
                    self.transfers.append((port, "down", body))
            else:
                got = bytearray()
                while True:
                    chunk = data_conn.recv(65536)
                    if not chunk:
                        break
                    got += chunk
                with self._lock:
                    self.files[name] = bytes(got)
                    self.uploads[name] = bytes(got)
                    self.transfers.append((port, "up", bytes(got)))
        finally:
            data_conn.close()
        say("226 transfer complete")

    def close(self) -> None:
        self._stop.set()
        try:
            self._listener.close()
        except OSError:
            pass
