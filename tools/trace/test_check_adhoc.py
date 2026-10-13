#!/usr/bin/env python3
"""Self-test of tools/trace/check_adhoc.py and tools/trace/catalog.py: the
lint finds what it should, ignores what it should, and `--enforce` fails
when it should."""

from __future__ import annotations

import json
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))

import catalog  # noqa: E402
import check_adhoc  # noqa: E402


class Scan(unittest.TestCase):
    def hits(self, source: str) -> list[str]:
        return [tag for _, tag, _ in check_adhoc.scan_text(source)]

    def test_every_print_form_is_found(self):
        source = "\n".join(
            [
                'serial_println!("DEV:ENUM:PASS");',
                'crate::serial_println!("FS:ROOT:{}", x);',
                'serial_print!("PERF:x");',
                'println!("CALC:UP:PASS");',
                'eprintln!("WEB:LOAD:FAIL");',
                'sys::write_str(&format!("INIT:AUTOSTART:PASS app={}\\n", a));',
                'sys::write_str(&format!(\n    "PKGD:READY\\n"));',
            ]
        )
        self.assertEqual(
            self.hits(source),
            ["DEV:", "FS:", "PERF:", "CALC:", "WEB:", "INIT:", "PKGD:"],
        )

    def test_diagnostics_and_ordinary_text_are_not_hits(self):
        source = "\n".join(
            [
                'serial_println!("HW:COM1:PRESENT");',
                'serial_println!("EXCEPTION:#PF at {}", a);',
                'serial_println!("IRQOFF:MAX syscall=1");',
                'serial_println!("DBGD:READY port=1");',
                'println!("hello {}", name);',
                'println!("lower:case");',
                'serial_println!("limits: ram=1");',
                'let s = "UP:PASS";',
            ]
        )
        self.assertEqual(self.hits(source), [])

    def test_line_numbers(self):
        hits = check_adhoc.scan_text('fn a() {}\n\nprintln!("UI:RECT x=1");\n')
        self.assertEqual([line for line, _, _ in hits], [3])

    def test_a_secret_in_the_arguments_is_reported(self):
        bad = 'sys::write_str(&format!("KEYD:PROVISION user={}\\n", password));'
        good = 'sys::write_str(&format!("KEYD:SECRETS:PASS file={}\\n", fhs::KEYD_SECRETS));'
        label = 'sys::write_str(&format!("ACCOUNTS:PASSWORD:PASS user={}\\n", name));'
        self.assertEqual(len(check_adhoc.secret_hits(bad)), 1)
        self.assertEqual(check_adhoc.secret_hits(good), [])
        self.assertEqual(check_adhoc.secret_hits(label), [])
        snake = 'sys::write_str(&format!("KEYD:PROVISION user={}\n", api_token));'
        self.assertEqual(len(check_adhoc.secret_hits(snake)), 1)
        captured = 'println!("KEYD:PROVISION password={password}");'
        self.assertEqual(len(check_adhoc.secret_hits(captured)), 1)
        marker = 'println!("KEYD:SECRETS:PASS {{token}}");'
        self.assertEqual(check_adhoc.secret_hits(marker), [])


class Enforce(unittest.TestCase):
    def run_lint(
        self, files: dict[str, str], *flags: str, baseline: dict[str, int] | None = None
    ) -> tuple[int, str]:
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp) / "repo"
            root.mkdir()
            saved = Path(tmp) / "baseline.json"
            if baseline is not None:
                saved.write_text(json.dumps(baseline), encoding="utf-8")
            flags = (*flags, "--baseline", str(saved))
            subprocess.run(["git", "init", "-q"], cwd=root, check=True)
            for name, text in files.items():
                path = root / name
                path.parent.mkdir(parents=True, exist_ok=True)
                path.write_text(text, encoding="utf-8")
            subprocess.run(["git", "add", "-A"], cwd=root, check=True)
            done = subprocess.run(
                [sys.executable, str(HERE / "check_adhoc.py"), "--root", str(root), *flags],
                capture_output=True,
                text=True,
            )
            return done.returncode, done.stdout + done.stderr

    def test_report_mode_passes_and_enforce_fails(self):
        files = {"user/src/a.rs": 'fn f() { println!("INIT:UP:PASS"); }\n'}
        code, out = self.run_lint(files)
        self.assertEqual(code, 0)
        self.assertIn("user/src/a.rs", out)
        self.assertIn("1 tree user", " ".join(out.split()))
        code, _ = self.run_lint(files, "--enforce")
        self.assertEqual(code, 1)

    def test_ratchet_refuses_a_new_print_and_a_stale_baseline(self):
        old = 'fn f() { println!("INIT:UP:PASS"); }\n'
        new = old + 'fn g() { println!("WRITER:SAVE:PASS"); }\n'
        fresh = 'fn h() { println!("CALC:UP:PASS"); }\n'
        base = {"user/src/a.rs": 1}
        # Unchanged: fine.
        code, out = self.run_lint({"user/src/a.rs": old}, "--ratchet", baseline=base)
        self.assertEqual(code, 0, out)
        # A second print in a file that had one.
        code, out = self.run_lint({"user/src/a.rs": new}, "--ratchet", baseline=base)
        self.assertEqual(code, 1)
        self.assertIn("user/src/a.rs: 2 ad hoc print(s), baseline 1", out)
        # A new app that prints.
        files = {"user/src/a.rs": old, "xui-app/src/bin/notes.rs": fresh}
        code, out = self.run_lint(files, "--ratchet", baseline=base)
        self.assertEqual(code, 1)
        self.assertIn("xui-app/src/bin/notes.rs: 1 ad hoc print(s), baseline 0", out)
        # A file that improved must lower its entry, or the room is spent later.
        code, out = self.run_lint({"user/src/a.rs": "fn f() {}\n"}, "--ratchet", baseline=base)
        self.assertEqual(code, 1)
        self.assertIn("--update-baseline", out)
        # A missing baseline means every print is new.
        code, _ = self.run_lint({"user/src/a.rs": old}, "--ratchet")
        self.assertEqual(code, 1)

    def test_update_baseline_writes_the_counts(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp) / "repo"
            (root / "user/src").mkdir(parents=True)
            (root / "user/src/a.rs").write_text(
                'fn f() { println!("INIT:A"); println!("INIT:B"); }\n', encoding="utf-8"
            )
            subprocess.run(["git", "init", "-q"], cwd=root, check=True)
            subprocess.run(["git", "add", "-A"], cwd=root, check=True)
            saved = Path(tmp) / "baseline.json"
            subprocess.run(
                [sys.executable, str(HERE / "check_adhoc.py"), "--root", str(root),
                 "--baseline", str(saved), "--update-baseline"],
                check=True, capture_output=True,
            )
            self.assertEqual(json.loads(saved.read_text()), {"user/src/a.rs": 2})

    def test_the_committed_baseline_is_current(self):
        found = check_adhoc.scan(check_adhoc.ROOT)
        saved = json.loads(check_adhoc.BASELINE.read_text(encoding="utf-8"))
        self.assertEqual(check_adhoc.ratchet(found, saved), [])

    def test_the_sinks_and_the_suite_are_excluded(self):
        files = {
            "libs/tracefmt/src/x.rs": 'println!("INIT:UP:PASS");\n',
            "kernel/src/trace.rs": 'serial_println!("DEV:X");\n',
            "kernel/src/tests/a.rs": 'serial_println!("DEV:X");\n',
            "tools/abi/fixtures/f.rs": 'println!("ABI:x:PASS");\n',
            "user/src/ok.rs": 'fn f() { serial_println!("HW:COM1:PRESENT"); }\n',
        }
        code, out = self.run_lint(files, "--enforce")
        self.assertEqual(code, 0, out)


class Catalogue(unittest.TestCase):
    def test_the_real_catalogue_parses_and_has_no_duplicates(self):
        text = (catalog.ROOT / catalog.CHANNEL_RS).read_text(encoding="utf-8")
        rows = catalog.parse_channels(text)
        catalog.check_rows(rows)
        self.assertIn(("Init", "INIT"), rows)

    def test_a_duplicate_prefix_is_refused(self):
        with self.assertRaises(ValueError):
            catalog.check_rows([("A", "X"), ("B", "X")])

    def test_a_malformed_row_is_refused(self):
        with self.assertRaises(ValueError):
            catalog.parse_channels('\nchannels! {\n    Init = "INIT",\n}\n')

    def test_the_committed_document_is_current(self):
        self.assertEqual(
            (catalog.ROOT / catalog.OUT).read_text(encoding="utf-8"),
            catalog.render(catalog.ROOT),
            "docs/trace/channels.md is stale: python tools/trace/catalog.py",
        )


if __name__ == "__main__":
    unittest.main()
