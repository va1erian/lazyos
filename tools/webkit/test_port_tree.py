#!/usr/bin/env python3
"""``port_tree``: the upstream files are fetched before the patches, and a file an applied patch
changed is not fetched again (patch 0023 changes the curl port's WebSocket scheduler, which the
tarball lacks). Runs on throwaway folders with a stand-in for the download; no network needed.

Run: python tools/webkit/test_port_tree.py
"""

from __future__ import annotations

import io
import sys
import tempfile
import unittest
from pathlib import Path
from unittest import mock

sys.path.insert(0, str(Path(__file__).resolve().parent))
import wkpins  # noqa: E402,F401  (puts tools/jsc, where toolenv lives, on sys.path)
import port_tree  # noqa: E402

PRISTINE = b"upstream bytes\n"
PATH = "Source/WebCore/platform/network/curl/CurlStream.cpp"


class PortTreeTests(unittest.TestCase):
    def setUp(self) -> None:
        tmp = tempfile.TemporaryDirectory()
        self.addCleanup(tmp.cleanup)
        self.root = Path(tmp.name)
        self.tree = self.root / "tree"
        self.tree.mkdir()
        self.patches = self.root / "patches"
        self.patches.mkdir()
        (self.patches / "0023-x.patch").write_text(f"why\n\n--- a/{PATH}\n+++ b/{PATH}\n@@ -1 +1 @@\n")
        upstream = self.root / "upstream.txt"
        upstream.write_text(f"# comment\n{port_tree.git_blob_sha1(PRISTINE)} {PATH}\n")
        self.fetches: list[str] = []
        for name, value in (("PATCHES", self.patches), ("JSC_PATCHES", self.root / "none"),
                            ("UPSTREAM", upstream)):
            patcher = mock.patch.object(port_tree, name, value)
            patcher.start()
            self.addCleanup(patcher.stop)
        opener = mock.patch.object(port_tree.urllib.request, "urlopen", self.urlopen)
        opener.start()
        self.addCleanup(opener.stop)

    def urlopen(self, url: str, timeout: float) -> io.BytesIO:
        self.fetches.append(url)
        return io.BytesIO(PRISTINE)

    def test_patched_paths_reads_the_new_side(self) -> None:
        self.assertEqual(port_tree.patched_paths(list(self.patches.glob("*.patch"))), {PATH})

    def test_a_missing_file_is_fetched_and_checked(self) -> None:
        port_tree.fetch_upstream(self.tree)
        self.assertEqual((self.tree / PATH).read_bytes(), PRISTINE)
        self.assertEqual(len(self.fetches), 1)

    def test_an_unpatched_changed_file_is_fetched_again(self) -> None:
        (self.tree / PATH).parent.mkdir(parents=True)
        (self.tree / PATH).write_bytes(b"stale\n")
        port_tree.fetch_upstream(self.tree)
        self.assertEqual((self.tree / PATH).read_bytes(), PRISTINE)

    def test_a_file_an_applied_patch_changed_is_kept(self) -> None:
        (self.tree / PATH).parent.mkdir(parents=True)
        (self.tree / PATH).write_bytes(b"patched\n")
        (self.tree / ".lazyos-patched").write_text("0023-x.patch\n")
        port_tree.fetch_upstream(self.tree)
        self.assertEqual((self.tree / PATH).read_bytes(), b"patched\n")
        self.assertEqual(self.fetches, [])

    def test_prepare_fetches_before_it_patches(self) -> None:
        order: list[str] = []
        with mock.patch.object(port_tree.wkpins, "fetch_webkit", return_value=self.root / "w.tar.xz"), \
             mock.patch.object(port_tree.toolenv, "extract", return_value=self.tree), \
             mock.patch.object(port_tree, "TREE", self.root / "src"), \
             mock.patch.object(port_tree, "fetch_upstream", lambda tree: order.append("fetch")), \
             mock.patch.object(port_tree, "apply_patches", lambda tree: order.append("patch")), \
             mock.patch.object(port_tree, "lay_overlay", lambda tree: order.append("overlay") or 0):
            (self.root / "src" / port_tree.wkpins.jsc_pins.WEBKIT.topdir).mkdir(parents=True)
            port_tree.prepare()
        self.assertEqual(order, ["fetch", "patch", "overlay"])


if __name__ == "__main__":
    unittest.main()
