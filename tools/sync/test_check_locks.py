#!/usr/bin/env python3
"""Self-test of tools/sync/check_locks.py: the scan finds what it should,
ignores what it should, and `--ratchet` fails when it should."""

from __future__ import annotations

import json
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))

import check_locks  # noqa: E402


class Scan(unittest.TestCase):
    def lines(self, source: str) -> list[int]:
        return check_locks.scan_text(source)

    def test_every_lock_form_is_found(self):
        source = "\n".join(
            [
                "use spin::Mutex;",
                "static TASKS: Mutex<[Option<Task>; N]> = Mutex::empty();",
                "static FRAMES: spin::Mutex<Option<Frames>> = spin::Mutex::new(None);",
                "use spin::mutex::Mutex;",
                "type YieldMutex<T> = spin::mutex::Mutex<T, Yield>;",
                "static R: RwLock<u32> = RwLock::new(0);",
                "use spin::RwLock;",
            ]
        )
        # Line 3 holds two uses (the type and the constructor path), both
        # counted on that line.
        self.assertEqual(self.lines(source), [1, 2, 3, 3, 4, 5, 6, 7])

    def test_the_kernel_lock_type_and_ordinary_text_are_not_hits(self):
        source = "\n".join(
            [
                "use crate::sync::Lock;",
                "static T: Lock<Table> = Lock::new(Table::new());",
                "// The lock above replaced the old raw lock (docs/smp-plan.md).",
                'let name = "not a lock";',
                "struct Mutex;",
            ]
        )
        self.assertEqual(self.lines(source), [])

    def test_a_generic_use_with_a_space_is_found(self):
        self.assertEqual(self.lines("static T: Mutex <[u8; 4]> = todo!();"), [1])


class InScope(unittest.TestCase):
    def test_only_the_kernel_trees_are_scanned(self):
        self.assertTrue(check_locks.in_scope("kernel/src/task/mod.rs"))
        self.assertTrue(check_locks.in_scope("user/src/bin/init/sessions.rs"))
        self.assertTrue(check_locks.in_scope("libs/ext2fs/src/io.rs"))
        for out in ("xui-app/src/net/web.rs",          # a host-crate std::sync::Mutex
                    "nettls/src/tls/mod.rs",           # a standalone workspace
                    "vendor/bootloader/src/lib.rs",    # upstream
                    "libs/ext2fs/examples/osread.rs",  # a host example program
                    "libs/ext2fs/src/tests/cache_crash.rs",
                    "libs/ext2fs/src/memio.rs"):       # cfg(test/fuzz)-only
            self.assertFalse(check_locks.in_scope(out), out)


class Ratchet(unittest.TestCase):
    def test_a_new_file_with_a_lock_fails(self):
        problems = check_locks.ratchet({"new.rs": [3]}, {})
        self.assertEqual(len(problems), 1)
        self.assertIn("new.rs: 1 raw spin lock(s), baseline 0", problems[0])

    def test_a_grown_file_fails_and_a_shrinking_one_must_lower_its_entry(self):
        problems = check_locks.ratchet({"a.rs": [1, 2, 3]}, {"a.rs": 2})
        self.assertEqual(len(problems), 1)
        self.assertIn("a.rs: 3 raw spin lock(s), baseline 2", problems[0])
        problems = check_locks.ratchet({"a.rs": [1]}, {"a.rs": 2})
        self.assertEqual(len(problems), 1)
        self.assertIn("--update-baseline", problems[0])

    def test_an_unchanged_file_passes(self):
        self.assertEqual(check_locks.ratchet({"a.rs": [1, 2]}, {"a.rs": 2}), [])
        self.assertEqual(check_locks.ratchet({}, {"a.rs": 0}), [])


class EndToEnd(unittest.TestCase):
    def test_the_ratchet_judges_a_tree(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            (root / "kernel" / "src").mkdir(parents=True)
            (root / "kernel" / "src" / "a.rs").write_text(
                "use spin::Mutex;\nstatic M: Mutex<u8> = Mutex::new(0);\n", encoding="utf-8"
            )
            baseline = root / "baseline.json"
            out = subprocess.run(  # noqa: S603 (fixed argv, no shell)
                [sys.executable, str(HERE / "check_locks.py"), "--update-baseline",
                 "--root", str(root), "--baseline", str(baseline)],
                capture_output=True, text=True, check=True,
            )
            self.assertIn("2 locks in 1 files", out.stdout)
            saved = json.loads(baseline.read_text(encoding="utf-8"))
            self.assertEqual(saved, {"kernel/src/a.rs": 2})
            # Grown: fail. Lowered: fail until the entry moves. Unchanged: pass.
            (root / "kernel" / "src" / "a.rs").write_text(
                "use spin::Mutex;\nstatic M: Mutex<u8> = Mutex::new(0);\nstatic N: Mutex<u8> = Mutex::new(0);\n",
                encoding="utf-8",
            )
            grown = subprocess.run(  # noqa: S603
                [sys.executable, str(HERE / "check_locks.py"), "--ratchet",
                 "--root", str(root), "--baseline", str(baseline)],
                capture_output=True, text=True,
            )
            self.assertEqual(grown.returncode, 1)
            (root / "kernel" / "src" / "a.rs").write_text("use spin::Mutex;\n", encoding="utf-8")
            lowered = subprocess.run(  # noqa: S603
                [sys.executable, str(HERE / "check_locks.py"), "--ratchet",
                 "--root", str(root), "--baseline", str(baseline)],
                capture_output=True, text=True,
            )
            self.assertEqual(lowered.returncode, 1)
            self.assertIn("--update-baseline", lowered.stderr)
            subprocess.run(  # noqa: S603
                [sys.executable, str(HERE / "check_locks.py"), "--update-baseline",
                 "--root", str(root), "--baseline", str(baseline)],
                capture_output=True, text=True, check=True,
            )
            ok = subprocess.run(  # noqa: S603
                [sys.executable, str(HERE / "check_locks.py"), "--ratchet",
                 "--root", str(root), "--baseline", str(baseline)],
                capture_output=True, text=True, check=True,
            )
            self.assertEqual(ok.returncode, 0)


if __name__ == "__main__":
    unittest.main()
