"""TLS fronts for esMail's mock mail server, under the harness's test CA.

`mail-mock-server` (va1erian/esmail) serves IMAP over implicit TLS with a
certificate only valid for `localhost`, and SMTP in plaintext. The guest
reaches the host as `tls.test` and trusts the harness's test CA
(`tools/net/tlscerts.py`), so this puts two TLS listeners in front of it:

- IMAPS: TLS with the `tls.test` leaf, re-encrypted to the mock's IMAP port
  (verified against the CA the mock writes with `CA_OUT`).
- SMTPS: TLS with the same leaf, forwarded in the clear to the mock's SMTP.

Each finished handshake is recorded (port, SNI, protocol), so the judge can
check the guest really spoke TLS to both.
"""

from __future__ import annotations

import asyncio
import ssl
import threading
from dataclasses import dataclass, field
from pathlib import Path


@dataclass
class Handshake:
    port: int
    sni: str | None
    version: str | None


@dataclass
class Record:
    handshakes: list[Handshake] = field(default_factory=list)
    lock: threading.Lock = field(default_factory=threading.Lock)

    def add(self, handshake: Handshake) -> None:
        with self.lock:
            self.handshakes.append(handshake)

    def ports(self) -> set[int]:
        with self.lock:
            return {h.port for h in self.handshakes}


async def _pipe(reader: asyncio.StreamReader, writer: asyncio.StreamWriter) -> None:
    try:
        while data := await reader.read(65536):
            writer.write(data)
            await writer.drain()
    except (ConnectionError, ssl.SSLError):
        pass
    finally:
        writer.close()


class Fronts:
    """Runs both listeners on a background event loop until `close`."""

    def __init__(self, leaf_cert: Path, leaf_key: Path, mock_ca: Path, imap: tuple[int, int],
                 smtp: tuple[int, int], bind: str = "127.0.0.1") -> None:
        self.record = Record()
        self._server = ssl.create_default_context(ssl.Purpose.CLIENT_AUTH)
        self._server.load_cert_chain(leaf_cert, leaf_key)
        self._upstream = ssl.create_default_context(cafile=str(mock_ca))
        self._sni: dict[int, str | None] = {}
        self._server.sni_callback = self._remember_sni
        self._routes = [(imap[0], imap[1], True), (smtp[0], smtp[1], False)]
        self._bind = bind
        self._loop = asyncio.new_event_loop()
        self._ready = threading.Event()
        self._error: BaseException | None = None
        self._thread = threading.Thread(target=self._run, daemon=True)
        self._thread.start()
        self._ready.wait(10)
        if self._error:
            raise self._error

    def _remember_sni(self, sock: ssl.SSLObject, name: str | None, _ctx) -> None:
        self._sni[id(sock)] = name

    def _run(self) -> None:
        asyncio.set_event_loop(self._loop)
        try:
            for listen, upstream, tls_upstream in self._routes:
                self._loop.run_until_complete(asyncio.start_server(
                    lambda r, w, l=listen, u=upstream, t=tls_upstream: self._serve(r, w, l, u, t),
                    self._bind, listen, ssl=self._server))
        except BaseException as error:  # reported by __init__
            self._error = error
            self._ready.set()
            return
        self._ready.set()
        self._loop.run_forever()

    async def _serve(self, reader, writer, listen: int, upstream: int, tls_upstream: bool) -> None:
        obj = writer.get_extra_info("ssl_object")
        self.record.add(Handshake(listen, self._sni.pop(id(obj), None), obj.version() if obj else None))
        try:
            up_reader, up_writer = await asyncio.open_connection(
                "127.0.0.1", upstream, ssl=self._upstream if tls_upstream else None,
                server_hostname="localhost" if tls_upstream else None)
        except OSError:
            writer.close()
            return
        await asyncio.gather(_pipe(reader, up_writer), _pipe(up_reader, writer))

    def close(self) -> None:
        self._loop.call_soon_threadsafe(self._loop.stop)
        self._thread.join(5)
