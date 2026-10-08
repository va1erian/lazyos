#!/usr/bin/env python3
"""The SMB harness's judges must fail when they should.

`smb_pcap.check_smb_flows` gets a synthetic SMB 2.1 conversation on a TCP
flow from the guest (NEGOTIATE, the logon, TREE_CONNECT, a WRITE and a READ)
and must pass it, then fail each damage: a password in the clear, the wrong
dialect, missing or unwanted signatures, an upload that never crossed whole
or leaks outside WRITE data, a missing download, the wrong share, too few
connections. `judge.judge_markers` and `judge.leaks` get serial logs and
files the same way.

Run: python tools/smb/test_judge.py
"""

from __future__ import annotations

import struct
import sys
import tempfile
import unittest
from pathlib import Path

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))
sys.path.insert(0, str(HERE.parent / "net"))
import checks  # noqa: E402
import judge  # noqa: E402
import smb_pcap as sp  # noqa: E402
from sockets_fixtures import GATEWAY_IP, GUEST_IP, frames_of, stream_flow  # noqa: E402

PORT = 1445
SECRET = b"hunter2secret"
UPLOAD = bytes((i * 7 + 3) & 0xFF for i in range(5000))
DOWNLOAD = bytes((i * 13 + 1) & 0xFF for i in range(4000))


def message(command: int, mid: int, body: bytes, *, response: bool = False, status: int = 0,
            signed: bool = False) -> bytes:
    flags = (sp.FLAG_RESPONSE if response else 0) | (sp.FLAG_SIGNED if signed else 0)
    header = b"\xfeSMB" + struct.pack("<HHIHHIIQIIQ", 64, 1, status, command, 32, flags, 0, mid, 0xFEFF, 1,
                                      0x11 if mid else 0) + (b"\x5a" * 16 if signed else bytes(16))
    data = header + body
    return struct.pack(">I", len(data)) + data


def conversation(*, dialect: int = 0x0210, sign: bool = False, share: str = "share",
                 extra_up: bytes = b"", upload: bytes = UPLOAD) -> tuple[bytes, bytes]:
    """(client bytes, server bytes) of one session."""
    negotiate = struct.pack("<HHHHI", 36, 2, 1, 0, 0) + bytes(24) + struct.pack("<HH", 0x0210, 0x0202)
    path = f"\\\\10.0.2.2\\{share}".encode("utf-16-le")
    tree = struct.pack("<HHHH", 9, 0, 72, len(path)) + path
    write = struct.pack("<HHIQQQ", 49, 112, len(upload), 0, 5, 5) + bytes(16) + upload
    read = struct.pack("<HBBIQQQ", 49, 0x50, 0, len(DOWNLOAD), 0, 6, 6) + bytes(17)
    up = (message(sp.NEGOTIATE, 0, negotiate) + message(sp.SESSION_SETUP, 1, b"\x19" + bytes(23))
          + message(sp.SESSION_SETUP, 2, b"\x19" + bytes(23) + extra_up)
          + message(sp.TREE_CONNECT, 3, tree, signed=sign) + message(sp.WRITE, 4, write, signed=sign)
          + message(sp.READ, 5, read, signed=sign))
    neg_resp = struct.pack("<HHH", 65, 1, dialect) + bytes(58)
    read_resp = struct.pack("<HBBIII", 17, 80, 0, len(DOWNLOAD), 0, 0) + DOWNLOAD
    down = (message(sp.NEGOTIATE, 0, neg_resp, response=True)
            + message(sp.SESSION_SETUP, 1, struct.pack("<HHHH", 9, 0, 0, 0), response=True,
                      status=0xC0000016)
            + message(sp.SESSION_SETUP, 2, struct.pack("<HHHH", 9, 0, 0, 0), response=True)
            + message(sp.TREE_CONNECT, 3, struct.pack("<HBBIII", 16, 1, 0, 0, 0, 0), response=True, signed=sign)
            + message(sp.WRITE, 4, struct.pack("<HHII", 17, 0, len(UPLOAD), 0) + bytes(8), response=True,
                      signed=sign)
            + message(sp.READ, 5, read_resp, response=True, signed=sign))
    return up, down


def check(up: bytes, down: bytes, expect: sp.PortExpect, flows: int = 1) -> list[str]:
    raw = []
    for n in range(flows):
        raw += stream_flow(50000 + n, PORT, up, down)
    _, problems = sp.check_smb_flows(frames_of(raw), GUEST_IP, GATEWAY_IP, {PORT: expect}, [SECRET])
    return problems


def expect(**kw) -> sp.PortExpect:
    base = {"signed": False, "uploads": [UPLOAD], "downloads": [DOWNLOAD]}
    return sp.PortExpect(**{**base, **kw})


class WireTests(unittest.TestCase):
    def test_a_clean_session_passes(self) -> None:
        self.assertEqual(check(*conversation(), expect()), [])
        self.assertEqual(check(*conversation(sign=True), expect(signed=True)), [])

    def test_each_damage_is_caught(self) -> None:
        cases = {
            "secret": (conversation(extra_up=SECRET), expect(), "secret"),
            "utf16": (conversation(extra_up=b"x" + SECRET), expect(), "secret"),
            "dialect": (conversation(dialect=0x0202), expect(), "dialect"),
            "unsigned": (conversation(), expect(signed=True), "unsigned"),
            "signed": (conversation(sign=True), expect(), "signed though"),
            "share": (conversation(share="other"), expect(), "TREE_CONNECT"),
            "upload": (conversation(upload=UPLOAD[:-1] + b"\x00"), expect(), "uploads"),
            "outside": (conversation(extra_up=UPLOAD[:64]), expect(), "outside WRITE"),
            "download": (conversation(), expect(downloads=[DOWNLOAD[::-1]]), "downloads"),
        }
        for name, ((up, down), want, fragment) in cases.items():
            problems = check(up, down, want)
            self.assertTrue(any(fragment in p for p in problems), f"{name}: {problems}")

    def test_too_few_connections_fail(self) -> None:
        self.assertTrue(any("connections" in p for p in check(*conversation(), expect(min_flows=2))))
        self.assertEqual(check(*conversation(), expect(min_flows=2), flows=2), [])

    def test_a_cut_stream_is_reported(self) -> None:
        up, down = conversation()
        self.assertTrue(check(up[:-10], down, expect()))


def serial(overrides: dict[str, str] | None = None) -> str:
    """A serial log in which every check behaves as it should."""
    lines = []
    for c in checks.CHECKS:
        text = (overrides or {}).get(c.name)
        if text is None:
            text = "SMB:PASS commands=1" if c.passes else f"SMB:FAIL reason=x {c.reason} y"
        status = 0 if "SMB:PASS" in text else 1
        lines += [text, f"SMBCHECK:{c.name}:{status}"]
    return "\n".join(lines) + "\n"


class MarkerTests(unittest.TestCase):
    def test_the_expected_outcomes_pass(self) -> None:
        self.assertEqual(judge.judge_markers(serial()), [])

    def test_a_negative_that_succeeds_fails(self) -> None:
        self.assertTrue(judge.judge_markers(serial({"guest": "SMB:PASS commands=1"})))

    def test_a_refusal_for_another_reason_fails(self) -> None:
        self.assertTrue(judge.judge_markers(serial({"tamper": "SMB:FAIL reason=connection reset"})))

    def test_a_failed_transfer_or_a_missing_check_fails(self) -> None:
        self.assertTrue(judge.judge_markers(serial({"transfer": "SMB:FAIL reason=boom"})))
        text = serial().replace("SMBCHECK:smb3:1", "")
        self.assertTrue(judge.judge_markers(text))


class LeakTests(unittest.TestCase):
    def test_a_password_in_any_form_is_found(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            clean, raw, wide = (Path(tmp) / n for n in ("clean", "raw", "wide"))
            clean.write_bytes(b"nothing here")
            raw.write_bytes(b"..." + SECRET + b"...")
            wide.write_bytes(SECRET.decode().encode("utf-16-le"))
            self.assertEqual(judge.leaks([clean], [SECRET]), [])
            self.assertEqual(len(judge.leaks([raw, wide, clean], [SECRET])), 2)


if __name__ == "__main__":
    unittest.main()
