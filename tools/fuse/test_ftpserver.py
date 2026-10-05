#!/usr/bin/env python3
"""The harness's FTP server against Python's own client (`ftplib`): every
command `ftpfuse` uses, the `MLSD` refusal, and no way out of the served tree.

    python tools/fuse/test_ftpserver.py
"""

from __future__ import annotations

import ftplib
import io
import sys
import tempfile
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
import ftpserver  # noqa: E402

# The server announces QEMU's gateway for passive connections; a host client
# must use the control peer instead, as `ftpfuse` does.
ftpserver.GATEWAY = (127, 0, 0, 1)


class ServerTest(unittest.TestCase):
    def setUp(self) -> None:
        self.dir = tempfile.TemporaryDirectory()
        self.root = Path(self.dir.name)
        (self.root / "a.txt").write_bytes(b"alpha")
        (self.root / "sub").mkdir()
        self.server = ftpserver.FtpServer(self.root)
        self.ftp = ftplib.FTP()
        self.ftp.connect("127.0.0.1", self.server.port, timeout=10)
        self.ftp.login("lazy", "os")

    def tearDown(self) -> None:
        try:
            self.ftp.quit()
        except (OSError, EOFError, ftplib.Error):
            pass
        self.server.close()
        self.dir.cleanup()

    def test_listing_and_transfers(self) -> None:
        facts = dict(self.ftp.mlsd("/"))
        self.assertEqual(facts["a.txt"]["type"], "file")
        self.assertEqual(facts["a.txt"]["size"], "5")
        self.assertEqual(facts["sub"]["type"], "dir")
        out = io.BytesIO()
        self.ftp.retrbinary("RETR /a.txt", out.write)
        self.assertEqual(out.getvalue(), b"alpha")
        self.ftp.storbinary("STOR /sub/b.bin", io.BytesIO(b"12"))
        self.ftp.storbinary("APPE /sub/b.bin", io.BytesIO(b"34"))
        self.assertEqual((self.root / "sub/b.bin").read_bytes(), b"1234")
        self.ftp.rename("/sub/b.bin", "/c.bin")
        self.ftp.mkd("/d")
        self.ftp.rmd("/d")
        self.ftp.delete("/c.bin")
        self.assertEqual(sorted(p.name for p in self.root.iterdir()), ["a.txt", "sub"])
        lines: list[str] = []
        self.ftp.retrlines("LIST /", lines.append)
        self.assertTrue(any(line.startswith("-") and line.endswith(" a.txt") for line in lines))

    def test_refusals(self) -> None:
        with self.assertRaises(ftplib.error_perm):
            self.ftp.retrbinary("RETR /../../etc/passwd", lambda _: None)
        with self.assertRaises(ftplib.error_perm):
            self.ftp.rmd("/sub/missing")
        self.server.mlsd = False
        with self.assertRaises(ftplib.error_perm):
            list(self.ftp.mlsd("/"))

    def test_wrong_password(self) -> None:
        other = ftplib.FTP()
        other.connect("127.0.0.1", self.server.port, timeout=10)
        with self.assertRaises(ftplib.error_perm):
            other.login("lazy", "nope")
        other.close()


if __name__ == "__main__":
    unittest.main()
