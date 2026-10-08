#!/usr/bin/env python3
"""A small SMB 2.1 file server over a real directory, for `tools/smb/run.py`.

Standard library only (impacket is flagged by antivirus on Windows hosts, and
Samba needs root): Direct TCP, dialects 2.0.2 and 2.1, NTLMv2 inside SPNEGO
(or raw), HMAC-SHA256 signing, one user and one share. The served directory
is what the harness judges: files the guest wrote are the server's own files.
Every request is recorded (`events`), with the logons and signature checks.

    python tools/smb/smbserver.py DIR --port 1445 --user chaton --password ...
    python tools/smb/smbserver.py DIR --require-signing
    python tools/smb/smbserver.py DIR --raw-ntlm        # no SPNEGO hint

Misbehaviour switches for the negative checks: `--guest` (log anyone on as a
guest), `--encrypt` (a session that requires encryption), `--truncate-challenge`,
`--tamper-read` (a bad signature on READ responses), `--smb3-only`.
"""

from __future__ import annotations

import argparse
import os
import socket
import struct
import threading
from dataclasses import dataclass, field
from pathlib import Path

import ntlm
import smbproto as p
from smbfiles import FileOps

SESSION_ID = 0x0000_4400_0000_0021
TREE_ID = 1


@dataclass
class Options:
    user: str = "chaton"
    password: str = "lazyos"
    share: str = "share"
    domain: str = "LAZYNAS"
    computer: str = "SMBHARNESS"
    require_signing: bool = False
    spnego: bool = True
    timestamp: bool = True
    guest: bool = False
    encrypt: bool = False
    truncate_challenge: bool = False
    tamper_read: bool = False
    dialects: tuple[int, ...] = (0x0210, 0x0202)


@dataclass
class Record:
    """What the server saw, for the judge."""
    events: list[tuple[str, str]] = field(default_factory=list)
    logons: list[tuple[str, str, bool]] = field(default_factory=list)
    signed: int = 0
    unsigned: int = 0
    bad_signatures: int = 0
    dialects: list[int] = field(default_factory=list)


class SmbServer:
    """Serve `root` on 127.0.0.1:`port` until `close()`."""

    def __init__(self, root: Path, port: int = 0, options: Options | None = None) -> None:
        self.root = root.resolve()
        self.options = options or Options()
        self.record = Record()
        self.lock = threading.Lock()
        self._listener = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
        self._listener.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
        self._listener.bind(("127.0.0.1", port))
        self._listener.listen(8)
        self.port = self._listener.getsockname()[1]
        self._stop = threading.Event()
        threading.Thread(target=self._accept, daemon=True).start()

    def close(self) -> None:
        self._stop.set()
        try:
            self._listener.close()
        except OSError:
            pass

    def event(self, command: str, detail: str = "") -> None:
        with self.lock:
            self.record.events.append((command, detail))

    def _accept(self) -> None:
        while not self._stop.is_set():
            try:
                conn, _ = self._listener.accept()
            except OSError:
                return
            threading.Thread(target=Connection(self, conn).run, daemon=True).start()


def _recv_exact(conn: socket.socket, n: int) -> bytes:
    data = b""
    while len(data) < n:
        chunk = conn.recv(n - len(data))
        if not chunk:
            raise ConnectionError("closed")
        data += chunk
    return data


class Connection:
    def __init__(self, server: SmbServer, conn: socket.socket) -> None:
        self.server, self.conn, self.o = server, conn, server.options
        self.key: bytes | None = None
        self.signing = False
        self.logged_on = False
        self.wrapped = True
        self.tree = False
        self.challenge = os.urandom(8)
        self.files = FileOps(server.root)

    def run(self) -> None:
        try:
            while True:
                head = _recv_exact(self.conn, 4)
                if head[0] != 0:
                    return
                size = struct.unpack(">I", head)[0] & 0xFFFFFF
                message = _recv_exact(self.conn, size)
                reply = self.handle(message)
                if reply is not None:
                    self.conn.sendall(p.frame(reply))
        except (ConnectionError, OSError, ValueError, struct.error):
            pass
        finally:
            self.files.close_all()
            self.conn.close()

    def respond(self, h: p.Header, status: int, body: bytes, sign: bool | None = None,
                tamper: bool = False) -> bytes:
        session = SESSION_ID if h.command != p.NEGOTIATE else 0
        tree = TREE_ID if h.command not in (p.NEGOTIATE, p.SESSION_SETUP, p.LOGOFF) else 0
        message = bytearray(h.response(status, session, tree) + (body or bytes([9, 0, 0, 0, 0, 0, 0, 0, 0])))
        if sign is None:
            sign = self.signing and self.key is not None
        if sign and self.key is not None:
            message[16:20] = struct.pack("<I", p.FLAG_RESPONSE | p.FLAG_SIGNED)
            signature = bytearray(ntlm.smb2_signature(self.key, bytes(message)))
            if tamper:
                signature[0] ^= 0x01
            message[48:64] = signature
        return bytes(message)

    def handle(self, message: bytes) -> bytes | None:
        h = p.Header.parse(message)
        name = p.NAMES.get(h.command, f"0x{h.command:02x}")
        if h.flags & p.FLAG_RESPONSE:
            return None
        if self.logged_on and h.command != p.SESSION_SETUP:
            if h.flags & p.FLAG_SIGNED:
                expected = ntlm.smb2_signature(self.key or b"", message)
                if expected != h.signature:
                    with self.server.lock:
                        self.server.record.bad_signatures += 1
                    self.server.event(name, "bad signature")
                    return self.respond(h, p.ACCESS_DENIED, b"", sign=False)
                with self.server.lock:
                    self.server.record.signed += 1
                self.signing = True
            else:
                with self.server.lock:
                    self.server.record.unsigned += 1
                if self.o.require_signing:
                    self.server.event(name, "unsigned")
                    return self.respond(h, p.ACCESS_DENIED, b"", sign=False)
        elif h.command not in (p.NEGOTIATE, p.SESSION_SETUP, p.ECHO):
            self.server.event(name, "before logon")
            return self.respond(h, p.USER_SESSION_DELETED, b"")
        body = message[p.HEADER:]
        if h.command == p.NEGOTIATE:
            return self.negotiate(h, body)
        if h.command == p.SESSION_SETUP:
            return self.session_setup(h, message, body)
        if h.command in (p.LOGOFF, p.TREE_DISCONNECT, p.ECHO):
            self.server.event(name)
            # Respond (signed, if the session signs) before forgetting the
            # state the response depends on.
            reply = self.respond(h, p.SUCCESS, bytes([4, 0, 0, 0]))
            if h.command in (p.LOGOFF, p.TREE_DISCONNECT):
                self.tree = False
                self.files.close_all()
            if h.command == p.LOGOFF:
                self.logged_on = False
            return reply
        if h.command == p.TREE_CONNECT:
            return self.tree_connect(h, message, body)
        if not self.tree:
            # File commands need a connected share, as on a real server.
            self.server.event(name, "no tree")
            return self.respond(h, p.NETWORK_NAME_DELETED, b"")
        status, reply, detail = self.files.dispatch(h.command, message, body)
        self.server.event(name, detail)
        tamper = self.o.tamper_read and h.command == p.READ
        return self.respond(h, status, reply, tamper=tamper)

    def negotiate(self, h: p.Header, body: bytes) -> bytes:
        count, mode = struct.unpack_from("<HH", body, 2)
        offered = list(struct.unpack_from(f"<{count}H", body, 36))
        common = [d for d in self.o.dialects if d in offered]
        with self.server.lock:
            self.server.record.dialects = offered
        if not common:
            self.server.event("NEGOTIATE", "no common dialect")
            return self.respond(h, p.NOT_SUPPORTED, b"")
        dialect = common[0]
        self.server.event("NEGOTIATE", f"dialect=0x{dialect:04x} client_mode={mode}")
        security = 0x1 | (0x2 if self.o.require_signing else 0)
        hint = p.negotiate_hint() if self.o.spnego else b""
        reply = struct.pack("<HHHH", 65, security, dialect, 0) + os.urandom(16)
        reply += struct.pack("<IIII", 0, 65536, 65536, 65536)
        reply += struct.pack("<QQHHI", p.filetime(), 0, p.HEADER + 64, len(hint), 0) + hint
        return self.respond(h, p.SUCCESS, reply)

    def session_setup(self, h: p.Header, message: bytes, body: bytes) -> bytes:
        offset, length = struct.unpack_from("<HH", body, 12)
        token = message[offset:offset + length]
        try:
            inner, self.wrapped = p.unwrap_client_token(token)
        except (ValueError, KeyError, IndexError) as error:
            self.server.event("SESSION_SETUP", f"bad token: {error}")
            return self.respond(h, p.INVALID_PARAMETER, b"")
        kind = struct.unpack_from("<I", inner, 8)[0] if len(inner) >= 12 else 0
        if kind == 1:
            stamp = p.filetime() if self.o.timestamp else None
            challenge = p.challenge_message(self.challenge, self.o.domain, self.o.computer, stamp)
            if self.o.truncate_challenge:
                challenge = challenge[:40]
            out = p.neg_token_resp(1, challenge) if self.wrapped else challenge
            self.server.event("SESSION_SETUP", "challenge")
            reply = struct.pack("<HHHH", 9, 0, p.HEADER + 8, len(out)) + out
            return self.respond(h, p.MORE_PROCESSING_REQUIRED, reply, sign=False)
        try:
            auth = p.parse_authenticate(inner)
        except (ValueError, struct.error, UnicodeDecodeError) as error:
            self.server.event("SESSION_SETUP", f"bad authenticate: {error}")
            return self.respond(h, p.INVALID_PARAMETER, b"")
        key = None
        if auth.user == self.o.user:
            key = ntlm.check_ntlmv2(self.o.password, auth.user, auth.domain, self.challenge, auth.nt)
        ok = key is not None or self.o.guest
        with self.server.lock:
            self.server.record.logons.append((auth.user, auth.domain, key is not None))
        self.server.event("SESSION_SETUP", f"user={auth.user} domain={auth.domain} ok={ok}")
        if not ok:
            return self.respond(h, p.LOGON_FAILURE, b"", sign=False)
        self.key = key or os.urandom(16)
        self.logged_on = True
        self.signing = self.o.require_signing
        flags = (0x1 if self.o.guest else 0) | (0x4 if self.o.encrypt else 0)
        out = p.neg_token_resp(0) if self.wrapped else b""
        reply = struct.pack("<HHHH", 9, flags, p.HEADER + 8 if out else 0, len(out)) + out
        return self.respond(h, p.SUCCESS, reply, sign=self.signing and key is not None)

    def tree_connect(self, h: p.Header, message: bytes, body: bytes) -> bytes:
        offset, length = struct.unpack_from("<HH", body, 4)
        path = message[offset:offset + length].decode("utf-16-le", "replace")
        share = path.rsplit("\\", 1)[-1]
        self.server.event("TREE_CONNECT", path)
        if share.lower() != self.o.share.lower():
            return self.respond(h, p.BAD_NETWORK_NAME, b"")
        self.tree = True
        return self.respond(h, p.SUCCESS, struct.pack("<HBBIII", 16, 1, 0, 0, 0, 0x001F01FF))


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("root", type=Path)
    parser.add_argument("--port", type=int, default=1445)
    parser.add_argument("--user", default="chaton")
    parser.add_argument("--password", default=os.environ.get("LAZYOS_SMB_PASSWORD", "lazyos"))
    parser.add_argument("--share", default="share")
    parser.add_argument("--require-signing", action="store_true")
    parser.add_argument("--raw-ntlm", action="store_true")
    parser.add_argument("--guest", action="store_true")
    parser.add_argument("--encrypt", action="store_true")
    parser.add_argument("--truncate-challenge", action="store_true")
    parser.add_argument("--tamper-read", action="store_true")
    parser.add_argument("--smb3-only", action="store_true")
    args = parser.parse_args()
    options = Options(user=args.user, password=args.password, share=args.share,
                      require_signing=args.require_signing, spnego=not args.raw_ntlm, guest=args.guest,
                      encrypt=args.encrypt, truncate_challenge=args.truncate_challenge,
                      tamper_read=args.tamper_read, dialects=(0x0311,) if args.smb3_only else (0x0210, 0x0202))
    server = SmbServer(args.root, args.port, options)
    print(f"smbserver: serving {server.root} as \\\\127.0.0.1\\{options.share} on port {server.port}", flush=True)
    try:
        threading.Event().wait()
    except KeyboardInterrupt:
        server.close()
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
