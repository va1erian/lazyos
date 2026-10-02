#!/usr/bin/env python3
"""Tests for check_literals.py: python tools/fhs/test_check_literals.py"""
import tempfile
import unittest
from pathlib import Path

import check_literals as chk


class Tree:
    """A throwaway source tree with `write(rel, text)`."""

    def __init__(self):
        self._dir = tempfile.TemporaryDirectory()
        self.root = Path(self._dir.name)

    def write(self, rel, text):
        path = self.root / rel
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(text, encoding="utf-8")

    def check(self, allow=""):
        (self.root / "allow.txt").write_text(allow, encoding="utf-8")
        return chk.scan(self.root, chk.load_allowlist(self.root / "allow.txt"))

    def cleanup(self):
        self._dir.cleanup()


class CheckLiterals(unittest.TestCase):
    def setUp(self):
        self.tree = Tree()
        self.addCleanup(self.tree.cleanup)

    def found(self, source, rel="user/src/a.rs", allow=""):
        self.tree.write(rel, source)
        return self.tree.check(allow)

    def test_offending_literals_are_caught(self):
        for text in ['"/data/apps"', '"/tmp/x"', '"/docs"', '"/home/u"', '"/conf"',
                     '"/apps"', '"/logs"', '"INIT.ELF"', '"XAPPS.LST"', '"MIME.TYP"',
                     '"PASSWD"', '"BUSYBOX"', 'r#"/data"#', '"cannot spawn TOP.ELF"',
                     '"/system/bin/top"', '"/system"', '"/etc/mime.types"',
                     '"/transient/x"', '"PKGDEMO.LZP"', '"/tmp"']:
            with self.subTest(text=text):
                self.assertEqual(len(self.found(f"const A: &str = {text};\n")), 1)

    def test_the_line_is_reported(self):
        hits = self.found('fn a() {}\n\nconst A: &str = "/data";\n')
        self.assertEqual(hits, [("user/src/a.rs", 3, "/data")])

    def test_clean_literals_pass(self):
        self.assertEqual(self.found('const A: &str = "/dev/null"; // "/data"\n'), [])
        self.assertEqual(self.found("const A: &str = fhs::mount::DATA;\n"), [])
        self.assertEqual(self.found('const A: &str = "/tmpfs";\n'), [])
        self.assertEqual(self.found('const A: &str = "/tmp2/x";\n'), [])

    def test_comments_are_ignored(self):
        source = '// "/data/apps"\n/// "/data" and INIT.ELF\n/* "/home" /* "/docs" */ "/apps" */\nfn a() {}\n'
        self.assertEqual(self.found(source), [])

    def test_a_char_literal_does_not_swallow_the_line(self):
        self.assertEqual(len(self.found("""let q = '"'; let p = "/data";\n""")), 1)

    def test_byte_strings_are_checked_too(self):
        for text in [r'b"TOP.ELF arg\0"', r'b"/system/bin/beep role=intruder\0"',
                     r'b"linux:NETFIX.ELF\0"', r'br"/etc/passwd"']:
            with self.subTest(text=text):
                self.assertEqual(len(self.found(f"const A: &[u8] = {text};\n")), 1)
        self.assertEqual(self.found('const A: &[u8] = b"demo=1\\0";\n'), [])

    def test_test_code_and_generated_files_are_skipped(self):
        bad = 'const A: &str = "/data";\n'
        for rel in ["kernel/src/tests/x.rs", "libs/x/tests/y.rs", "libs/x/src/tests.rs",
                    "libs/generated/src/lib.rs", "libs/fhs/src/state.rs", "target/x.rs"]:
            with self.subTest(rel=rel):
                self.assertEqual(self.found(bad, rel), [])

    def test_an_inline_test_module_is_skipped(self):
        source = 'const A: &str = "/home";\n#[cfg(test)]\nmod tests {\n const B: &str = "/data";\n}\n'
        self.assertEqual([hit[1] for hit in self.found(source)], [1])

    def test_escaped_char_literals_do_not_swallow_the_line(self):
        for char in [r"'\"'", r"'\''", r"'\'", r"'\n'", r"'\x41'", r"'\u{1F600}'"]:
            with self.subTest(char=char):
                self.assertEqual(len(self.found(f'let q = {char}; let p = "/data";\n')), 1)
        # A lifetime has no closing quote and must not start a char literal.
        self.assertEqual(len(self.found("fn f<'a>(x: &'a str) { let p = \"/data\"; }\n")), 1)

    def test_production_code_after_a_test_module_is_scanned(self):
        source = ('#[cfg(test)]\nmod tests {\n    const B: &str = "/data}";\n'
                  '    fn t() { if true { } }\n}\nconst A: &str = "/data";\n')
        self.assertEqual([hit[1] for hit in self.found(source)], [6])

    def test_malformed_allowlist_entries_are_rejected(self):
        for entry in ["user/src/a.rs:2:", "user/src/a.rs:", "user/src/a.rs:2:   ", "user/src/a.rs"]:
            with self.subTest(entry=entry):
                with self.assertRaises(ValueError):
                    self.tree.check(entry + "\n")
        self.tree.write("user/src/a.rs", 'const A: &str = "/logs";\n')
        (self.tree.root / "bad.txt").write_text("user/src/a.rs:2:\n", encoding="utf-8")
        argv = ["--root", str(self.tree.root), "--allowlist", str(self.tree.root / "bad.txt")]
        self.assertEqual(chk.main(argv), 2)

    def test_allowlisted_file_and_line_pass(self):
        source = 'const A: &str = "/data";\nconst B: &str = "/docs";\n'
        self.assertEqual(len(self.found(source)), 2)
        self.assertEqual(self.tree.check("user/src/a.rs:the whole file\n"), [])
        left = self.tree.check("# note\nuser/src/a.rs:2:a fixture\n")
        self.assertEqual([hit[1] for hit in left], [1])

    def test_main_reports_and_fails(self):
        self.tree.write("user/src/a.rs", 'const A: &str = "/logs";\n')
        self.assertEqual(chk.main(["--root", str(self.tree.root), "--allowlist", str(self.tree.root / "none")]), 1)
        self.tree.write("user/src/a.rs", "const A: u8 = 1;\n")
        self.assertEqual(chk.main(["--root", str(self.tree.root), "--allowlist", str(self.tree.root / "none")]), 0)


if __name__ == "__main__":
    unittest.main()
