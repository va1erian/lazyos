#!/usr/bin/env python3
"""Tests for the MIDL browser. Run: python tools/midlc/test_midl_browser.py.

The model tests run headless. The GUI smoke test builds the real window and
is skipped where Tk has no display (CI runners)."""

from __future__ import annotations

import sys
import tempfile
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
import midl_browser_model as model  # noqa: E402

TWO = """
/// First.
interface os.lazy.first.v1 {
    method Attach(slots: U32) -> ()
        transfers (rings: Ring<Rx>, notify: Channel<os.lazy.first.v1>);
    method Notify() -> () oneway;
    /// Frames in.
    ring Rx : frames producer=server doorbell=Notify;
}
interface os.lazy.second.v1 {
    method Ping() -> ();
}
"""


class ModelTests(unittest.TestCase):
    def setUp(self) -> None:
        self.dir = tempfile.TemporaryDirectory()
        self.path = Path(self.dir.name) / "two.midl"
        self.path.write_text(TWO, encoding="utf-8")
        self.loaded = model.load([self.path])

    def tearDown(self) -> None:
        self.dir.cleanup()

    def test_every_interface_of_a_file_is_loaded(self) -> None:
        names = [entry.interface.name for entry in self.loaded]
        self.assertEqual(names, ["os.lazy.first.v1", "os.lazy.second.v1"])

    def test_signature_shows_transfers(self) -> None:
        attach = self.loaded[0].interface.methods[0]
        self.assertEqual(
            model.signature(attach),
            "(slots: U32) -> () transfers (rings: Ring<Rx>, notify: Channel<os.lazy.first.v1>)",
        )

    def test_filter_finds_rings_and_transfers(self) -> None:
        first = self.loaded[0].interface
        by_ring = model.filtered(first, "doorbell notify")
        self.assertEqual([r.name for r in by_ring.rings], ["Rx"])
        by_transfer = model.filtered(first, "channel<")
        self.assertEqual([m.name for m in by_transfer.methods], ["Attach"])
        self.assertIsNone(model.filtered(first, "nothing-like-this"))
        self.assertIs(model.filtered(first, ""), first)

    def test_counts(self) -> None:
        total = model.counts(self.loaded)
        self.assertEqual(
            {k: total[k] for k in ("interfaces", "files", "failed", "methods", "transferring", "rings")},
            {"interfaces": 2, "files": 1, "failed": 0, "methods": 3, "transferring": 1, "rings": 1},
        )

    def test_parse_error_is_one_entry(self) -> None:
        bad = Path(self.dir.name) / "bad.midl"
        bad.write_text("interface nope {", encoding="utf-8")
        loaded = model.load([bad])
        self.assertEqual(len(loaded), 1)
        self.assertIsNone(loaded[0].interface)
        self.assertEqual(model.counts(loaded)["failed"], 1)

    def test_a_root_under_a_skipped_directory_is_still_scanned(self) -> None:
        # A git worktree lives in `.claude/worktrees/`; only directories
        # below the root are skipped.
        root = Path(self.dir.name) / ".claude" / "tree"
        (root / "target").mkdir(parents=True)
        (root / "a.midl").write_text(TWO, encoding="utf-8")
        (root / "target" / "skip.midl").write_text(TWO, encoding="utf-8")
        self.assertEqual([p.name for p in model.discover([root])], ["a.midl"])

    def test_repository_files_all_load(self) -> None:
        loaded = model.load(model.discover([model.REPO_ROOT / "idl"]))
        self.assertTrue(all(entry.interface is not None for entry in loaded), loaded)
        names = {entry.interface.name for entry in loaded}
        # Second interfaces of their files, invisible before.
        self.assertIn("os.lazy.input.shell.v1", names)
        self.assertIn("os.lazy.audio.mixer.v1", names)
        self.assertGreaterEqual(model.counts(loaded)["rings"], 3)


class GuiSmokeTests(unittest.TestCase):
    def test_tree_and_ring_detail(self) -> None:
        try:
            import tkinter as tk

            root = tk.Tk()
        except Exception as error:  # no display (CI)
            self.skipTest(f"Tk unavailable: {error}")
        import midl_browser

        try:
            with tempfile.TemporaryDirectory() as tmp:
                path = Path(tmp) / "two.midl"
                path.write_text(TWO, encoding="utf-8")
                browser = midl_browser.MidlBrowser(root, [path])
                kinds = [node.kind for node in browser.nodes.values()]
                self.assertEqual(kinds.count("file"), 1)
                self.assertEqual(kinds.count("interface"), 2)
                ring = next(iid for iid, node in browser.nodes.items() if node.kind == "ring")
                browser.tree.selection_set(ring)
                browser._render(browser.nodes[ring])
                text = browser.detail.get("1.0", "end")
                self.assertIn("ring Rx", text)
                self.assertIn("doorbell    Notify (oneway)", text)
                self.assertIn("Attach: `buffers[0]`, a shared buffer holding the rings `Rx`", text)
                self.assertIn("1 rings", browser.status.cget("text"))
                browser.query.set("second")
                kinds = [node.kind for node in browser.nodes.values()]
                self.assertEqual(kinds.count("interface"), 1)
        finally:
            root.destroy()


if __name__ == "__main__":
    unittest.main()
