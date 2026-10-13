"""A misbehaving SNTP server for the NTP harness (docs/ntp-plan.md section 5).

One UDP socket on the loopback (the guest reaches it as 10.0.2.2 through
QEMU's user network). Every request is logged with the host's monotonic time,
and answered by the *behaviour* picked for its index: a script of behaviours
consumed one per request, the last one repeating. The clock it serves is the
host's, plus the behaviour's skew, so the guest's step can be judged against
a time the guest does not own.

Behaviours (`Behaviour(kind, ...)`):

    honest   a well-formed reply; `skew` seconds ahead, `hold` seconds of
             server-side delay before it is sent, `stratum`
    rate     stratum 0, kiss `RATE`      deny    stratum 0, kiss `DENY`
    silent   no answer
    nonce    a valid reply that echoes another request's transmit timestamp
    alarm    leap indicator 3            stratum16  stratum 16
    ancient  honest, but the time is 1970 (a hostile or broken server)
    zero     zero receive and transmit timestamps
    short    a 20-byte packet            garbage  48 random bytes
    mode3    a reply that is a client-mode packet
    wrongport  an honest reply from a second socket (a different source port)
"""

from __future__ import annotations

import os
import socket
import struct
import threading
import time
from dataclasses import dataclass, field

NTP_UNIX = 2_208_988_800


@dataclass(frozen=True)
class Behaviour:
    kind: str = "honest"
    skew: float = 0.0
    hold: float = 0.0
    stratum: int = 2


def honest(skew: float = 0.0, hold: float = 0.0, stratum: int = 2) -> Behaviour:
    return Behaviour("honest", skew, hold, stratum)


@dataclass
class Request:
    index: int
    at: float          # time.monotonic() on the host
    unix: float        # time.time() on the host
    source: tuple
    length: int
    first_byte: int
    transmit: bytes    # the client's transmit timestamp (the nonce)
    other_nonzero: bool  # any header field other than byte 0 and the transmit timestamp set
    behaviour: str = ""

    @property
    def version(self) -> int:
        return (self.first_byte >> 3) & 7

    @property
    def mode(self) -> int:
        return self.first_byte & 7


def ntp_timestamp(unix: float) -> bytes:
    seconds = int(unix)
    fraction = int((unix - seconds) * (1 << 32))
    return struct.pack(">II", (seconds + NTP_UNIX) & 0xFFFFFFFF, fraction)


def reply_packet(request: bytes, now: float, *, stratum: int = 2, li: int = 0, mode: int = 4,
                 version: int = 4, refid: bytes = b"LAZY", originate: bytes | None = None,
                 timestamps: bool = True) -> bytes:
    """A reply to `request` (RFC 4330 section 4), `now` the server's time."""
    echo = originate if originate is not None else request[40:48]
    receive = ntp_timestamp(now) if timestamps else bytes(8)
    transmit = ntp_timestamp(now + 0.0005) if timestamps else bytes(8)
    head = bytes([(li << 6) | (version << 3) | mode, stratum, 6, 0xEC])
    return (head + struct.pack(">II", 0x0000_0200, 0x0000_0400) + refid
            + ntp_timestamp(now - 5) + echo + receive + transmit)


class FakeNtpd:
    def __init__(self, port: int, script: list[Behaviour], host: str = "127.0.0.1"):
        self.port = port
        self.script = script or [honest()]
        self.requests: list[Request] = []
        self._lock = threading.Lock()
        self._previous_nonce = b"\x01" * 8
        self._sock = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
        self._sock.bind((host, port))
        self._sock.settimeout(0.2)
        self._other = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
        self._other.bind((host, 0))
        self._stop = threading.Event()
        self._thread = threading.Thread(target=self._serve, daemon=True)
        self._thread.start()

    def close(self) -> None:
        self._stop.set()
        self._thread.join(timeout=2)
        self._sock.close()
        self._other.close()

    def snapshot(self) -> list[Request]:
        with self._lock:
            return list(self.requests)

    def _behaviour(self, index: int) -> Behaviour:
        return self.script[min(index, len(self.script) - 1)]

    def _serve(self) -> None:
        while not self._stop.is_set():
            try:
                data, source = self._sock.recvfrom(2048)
            except (socket.timeout, OSError):
                continue
            at, unix = time.monotonic(), time.time()
            with self._lock:
                index = len(self.requests)
                behaviour = self._behaviour(index)
                padded = data.ljust(48, b"\0")
                self.requests.append(Request(
                    index=index, at=at, unix=unix, source=source, length=len(data),
                    first_byte=data[0] if data else 0, transmit=padded[40:48],
                    other_nonzero=any(padded[1:40]), behaviour=behaviour.kind))
            self._answer(behaviour, padded, source, unix)

    def _answer(self, b: Behaviour, request: bytes, source: tuple, unix: float) -> None:
        now = unix + b.skew
        sock = self._sock
        if b.kind == "silent":
            return
        if b.kind == "honest":
            packet = reply_packet(request, now, stratum=b.stratum)
        elif b.kind == "rate":
            packet = reply_packet(request, now, stratum=0, refid=b"RATE")
        elif b.kind == "deny":
            packet = reply_packet(request, now, stratum=0, refid=b"DENY")
        elif b.kind == "nonce":
            packet = reply_packet(request, now, originate=self._previous_nonce)
        elif b.kind == "alarm":
            packet = reply_packet(request, now, li=3)
        elif b.kind == "stratum16":
            packet = reply_packet(request, now, stratum=16)
        elif b.kind == "ancient":
            packet = reply_packet(request, 0.0)
        elif b.kind == "zero":
            packet = reply_packet(request, now, timestamps=False)
        elif b.kind == "short":
            packet = reply_packet(request, now)[:20]
        elif b.kind == "garbage":
            packet = os.urandom(48)
        elif b.kind == "mode3":
            packet = reply_packet(request, now, mode=3)
        elif b.kind == "wrongport":
            packet = reply_packet(request, now)
            sock = self._other
        else:
            raise ValueError(f"unknown behaviour {b.kind!r}")
        self._previous_nonce = request[40:48]
        if b.hold:
            time.sleep(b.hold)
        try:
            sock.sendto(packet, source)
        except OSError:
            pass


if __name__ == "__main__":  # a quick manual server: python fake_ntpd.py [port] [skew]
    import sys

    server = FakeNtpd(int(sys.argv[1]) if len(sys.argv) > 1 else 47123,
                      [honest(float(sys.argv[2]) if len(sys.argv) > 2 else 0.0)])
    print(f"fake ntpd on 127.0.0.1:{server.port}; Ctrl+C to stop", flush=True)
    try:
        while True:
            time.sleep(1)
    except KeyboardInterrupt:
        server.close()
