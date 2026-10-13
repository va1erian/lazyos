#!/usr/bin/env python3
"""``port_cmake.collect``: a program ninja did not link again is not stripped
again (the image build and the launcher watch the stripped file's time), one it
did is, and the stripped file is replaced whole. Runs on throwaway folders with
a stand-in for ``strip``; no toolchain needed."""

from __future__ import annotations

import json
import os
import sys
import tempfile
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
import port_cmake  # noqa: E402
import wkpins  # noqa: E402


class CollectTests(unittest.TestCase):
    def setUp(self) -> None:
        tmp = tempfile.TemporaryDirectory()
        self.addCleanup(tmp.cleanup)
        root = Path(tmp.name)
        self.saved = (port_cmake.BUILD, port_cmake.STRIPPED, port_cmake.FONTS_CONF,
                      wkpins.OUT, wkpins.BIN, port_cmake.run, port_cmake.write_fonts_conf)
        port_cmake.BUILD = root / "build"
        wkpins.OUT = root / "out"
        wkpins.BIN = wkpins.OUT / "bin"
        port_cmake.STRIPPED = wkpins.OUT / "stripped.json"
        (port_cmake.BUILD / "bin").mkdir(parents=True)
        self.strips: list[str] = []

        def strip(command, cwd, what, env=None):
            # strip --strip-all -o OUT BUILT: the output is the built bytes, shortened.
            self.strips.append(what)
            built = Path(command[-1]).read_bytes()
            Path(command[-2]).write_bytes(built[:-1])

        port_cmake.run = strip
        port_cmake.write_fonts_conf = lambda: None
        self.addCleanup(self.restore)

    def restore(self) -> None:
        (port_cmake.BUILD, port_cmake.STRIPPED, port_cmake.FONTS_CONF,
         wkpins.OUT, wkpins.BIN, port_cmake.run, port_cmake.write_fonts_conf) = self.saved

    def link(self, name: str, content: bytes) -> None:
        """What ninja leaves: a program, with a time of its own."""
        path = port_cmake.BUILD / "bin" / name
        path.write_bytes(content)
        bump = 10_000_000_000 * (len(self.strips) + len(content))
        os.utime(path, ns=(bump, bump))

    def test_the_first_collect_strips_each_program(self) -> None:
        self.link("WebProcess", b"web-1")
        self.link("NetworkProcess", b"net-1")
        found = port_cmake.collect(["WebProcess", "NetworkProcess"])
        self.assertEqual(sorted(self.strips), ["strip NetworkProcess", "strip WebProcess"])
        self.assertEqual(Path(found["WebProcess"]).read_bytes(), b"web-")
        self.assertEqual(sorted(json.loads(port_cmake.STRIPPED.read_text())),
                         ["NetworkProcess", "WebProcess"])

    def test_a_program_that_was_not_linked_again_is_left_alone(self) -> None:
        self.link("WebProcess", b"web-1")
        first = Path(port_cmake.collect(["WebProcess"])["WebProcess"])
        old = 1_000_000_000_000_000_000
        os.utime(first, ns=(old, old))
        self.strips.clear()
        port_cmake.collect(["WebProcess"])
        self.assertEqual(self.strips, [])
        self.assertEqual(first.stat().st_mtime_ns, old, "its time is what the build watches")

    def test_a_program_that_was_linked_again_is_stripped_again(self) -> None:
        self.link("WebProcess", b"web-1")
        port_cmake.collect(["WebProcess"])
        self.link("WebProcess", b"web-22")
        self.strips.clear()
        found = port_cmake.collect(["WebProcess"])
        self.assertEqual(self.strips, ["strip WebProcess"])
        self.assertEqual(Path(found["WebProcess"]).read_bytes(), b"web-2")

    def test_a_cut_short_strip_leaves_no_program_and_no_record(self) -> None:
        self.link("WebProcess", b"web-1")

        def cancelled(command, cwd, what, env=None):
            Path(command[-2]).write_bytes(b"we")  # half of it, then the launcher stops
            raise KeyboardInterrupt

        port_cmake.run = cancelled
        with self.assertRaises(KeyboardInterrupt):
            port_cmake.collect(["WebProcess"])
        self.assertFalse((wkpins.BIN / "WebProcess").exists(), "a short program must not stand in")
        self.assertFalse(port_cmake.STRIPPED.exists())

    def test_a_missing_program_is_an_error(self) -> None:
        with self.assertRaises(port_cmake.BuildError):
            port_cmake.collect(["WebProcess"])


if __name__ == "__main__":
    unittest.main()
