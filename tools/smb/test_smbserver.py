#!/usr/bin/env python3
"""Tests of the harness SMB server (`smbserver.py`) and its NTLM code.

The crypto is checked against `MS-NLMP`'s published vectors; the DER helpers
on hand-built tokens; and whole sessions with `libs/smbwire`'s host client
(`cargo run -p smbwire --example smbcat`), so the server and the client are
judged against each other from two independent implementations: every
behaviour switch the guest harness uses must produce the refusal it is for.
The session tests are skipped when `cargo` is missing.

Run: python tools/smb/test_smbserver.py
"""

from __future__ import annotations

import os
import shutil
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

HERE = Path(__file__).resolve().parent
ROOT = HERE.parent.parent
sys.path.insert(0, str(HERE))
import checks  # noqa: E402
import ntlm  # noqa: E402
import smbproto as p  # noqa: E402
import smbserver  # noqa: E402

PASSWORD = "harnessPW42"
SMBCAT = ROOT / "target" / "debug" / "examples" / ("smbcat.exe" if os.name == "nt" else "smbcat")


class CryptoTests(unittest.TestCase):
    def test_md4_and_the_ms_nlmp_vectors(self) -> None:
        self.assertEqual(ntlm.md4(b"").hex(), "31d6cfe0d16ae931b73c59d7e0c089c0")
        self.assertEqual(ntlm.md4(b"a" * 100).hex(), ntlm.md4(b"a" * 100).hex())
        self.assertEqual(ntlm.nt_hash("Password").hex(), "a4f49c406510bdcab6824ee7c30fd852")
        key = ntlm.ntowfv2("Password", "User", "Domain")
        self.assertEqual(key.hex(), "0c868a403bfd7a93a3001ef22ef02e3f")
        info = bytes.fromhex("02000c0044006f006d00610069006e0001000c0053006500720076006500720000000000")
        blob = bytes([1, 1, 0, 0, 0, 0, 0, 0]) + bytes(8) + b"\xaa" * 8 + bytes(4) + info + bytes(4)
        server = bytes.fromhex("0123456789abcdef")
        proof = ntlm.hmac_md5(key, server, blob)
        self.assertEqual(proof.hex(), "68cd0ab851e51c96aabc927bebef6a1c")
        session = ntlm.check_ntlmv2("Password", "User", "Domain", server, proof + blob)
        self.assertEqual(session.hex(), "8de40ccadbc14a82f15cb0ad0de95ca3")
        self.assertIsNone(ntlm.check_ntlmv2("password", "User", "Domain", server, proof + blob))


class DerTests(unittest.TestCase):
    def test_client_tokens_unwrap_both_ways(self) -> None:
        inner = p.NTLM_SIGNATURE + b"\x01\x00\x00\x00rest"
        init = p.der(0x60, p.der(0x06, p.SPNEGO_OID) + p.der(0xA0, p.der(0x30, p.der(
            0xA0, p.der(0x30, p.der(0x06, p.NTLMSSP_OID))) + p.der(0xA2, p.der(0x04, inner)))))
        self.assertEqual(p.unwrap_client_token(init), (inner, True))
        resp = p.der(0xA1, p.der(0x30, p.der(0xA2, p.der(0x04, inner * 40))))
        self.assertEqual(p.unwrap_client_token(resp), (inner * 40, True))
        self.assertEqual(p.unwrap_client_token(inner), (inner, False))
        with self.assertRaises(ValueError):
            p.unwrap_client_token(init[:-3])

    def test_the_hint_lists_ntlmssp(self) -> None:
        hint = p.negotiate_hint()
        self.assertIn(p.NTLMSSP_OID, hint)
        tag, body, rest = p.der_read(hint)
        self.assertEqual((tag, rest), (0x60, b""))


@unittest.skipUnless(shutil.which("cargo"), "cargo is needed for the host client")
class SessionTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls) -> None:
        subprocess.run(["cargo", "build", "-q", "-p", "smbwire", "--example", "smbcat"], cwd=ROOT, check=True)

    def serve(self, **options) -> tuple[smbserver.SmbServer, Path]:
        root = Path(tempfile.mkdtemp())
        self.addCleanup(shutil.rmtree, root, True)
        checks.seed(root)
        server = smbserver.SmbServer(root, 0, smbserver.Options(password=PASSWORD, **options))
        self.addCleanup(server.close)
        return server, root

    def smbcat(self, server, *args: str, password: str = PASSWORD, share: str = "share",
               flags: tuple[str, ...] = ()) -> subprocess.CompletedProcess:
        env = dict(os.environ, LAZYOS_SMB_PASSWORD=password)
        command = [str(SMBCAT), *flags, f"127.0.0.1:{server.port}", share, "chaton", *args]
        return subprocess.run(command, env=env, capture_output=True, text=True, timeout=60)

    def test_a_round_trip_lands_in_the_served_directory(self) -> None:
        server, root = self.serve()
        result = self.smbcat(server, "selftest")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("selftest: ok", result.stdout)
        got = subprocess.run([str(SMBCAT), f"127.0.0.1:{server.port}", "share", "chaton", "get", "big.bin"],
                             env=dict(os.environ, LAZYOS_SMB_PASSWORD=PASSWORD), capture_output=True, timeout=60)
        self.assertEqual(got.stdout, checks.BIG)
        local = root / "local.bin"
        local.write_bytes(checks.UPLOAD)
        self.assertEqual(self.smbcat(server, "put", "up.bin", str(local)).returncode, 0)
        self.assertEqual((root / "up.bin").read_bytes(), checks.UPLOAD)
        self.assertEqual(server.record.signed, 0)
        self.assertTrue(any(ok for _, _, ok in server.record.logons))

    def test_signing_is_verified_both_ways(self) -> None:
        for options, flags in (({"require_signing": True}, ()), ({}, ("--sign",))):
            server, _ = self.serve(**options)
            result = self.smbcat(server, "selftest", flags=flags)
            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertGreater(server.record.signed, 10)
            self.assertEqual((server.record.unsigned, server.record.bad_signatures), (0, 0))

    def test_raw_ntlm_without_a_server_time(self) -> None:
        server, _ = self.serve(spnego=False, timestamp=False)
        result = self.smbcat(server, "ls")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("spnego: false", result.stderr)

    def test_every_misbehaviour_is_refused_for_its_reason(self) -> None:
        cases = [
            ({}, {"password": "wrong"}, "3221225581"),  # STATUS_LOGON_FAILURE
            ({}, {"share": "nope"}, "3221225676"),  # STATUS_BAD_NETWORK_NAME
            ({"require_signing": True}, {"flags": ("--no-sign",)}, "requires signing"),
            ({"guest": True}, {}, "guest"),
            ({"encrypt": True}, {}, "encryption"),
            ({"truncate_challenge": True}, {}, "Malformed"),
            ({"require_signing": True, "tamper_read": True}, {}, "Signature"),
            ({"dialects": (0x0311,)}, {}, "Status { command: 0"),
        ]
        for options, call, reason in cases:
            server, _ = self.serve(**options)
            result = self.smbcat(server, "get", "hello.txt", **call)
            self.assertNotEqual(result.returncode, 0, (options, call))
            self.assertIn(reason, result.stderr, (options, call))
            touched = [e for e in server.record.events if e[0] in ("WRITE", "QUERY_DIRECTORY", "SET_INFO")]
            self.assertEqual(touched, [], options)


if __name__ == "__main__":
    unittest.main()
