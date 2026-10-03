#!/usr/bin/env python3
"""Tests for the pure helpers of tools/linuxapps (no network, no compiler).

    python tools/linuxapps/test_build.py

The expected generator outputs were produced by upstream's own scripts
(dash 0.5.12's ``mkbuiltins`` and ``mktokens``, run under Alpine's sh/awk) on
the same inputs, so they pin the Python reimplementations to the originals.
"""

from __future__ import annotations

import re
import stat
import struct
import sys
import tempfile
import unittest
import zipfile
from pathlib import Path

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))
import build  # noqa: E402
import dashgen  # noqa: E402
import elfcheck  # noqa: E402
import recipes  # noqa: E402
import rg  # noqa: E402
import sources  # noqa: E402
from toolchain import BuildError  # noqa: E402


def elf(e_type: int = 2, machine: int = 62, phdrs: tuple[int, ...] = (1,),
        entry: int = 0x401000, cls: int = 2) -> bytes:
    """A minimal ELF64 image: header then `phdrs` program headers of those types."""
    header = bytearray(64)
    header[:4] = b"\x7fELF"
    header[4], header[5], header[6] = cls, 1, 1
    struct.pack_into("<HHI", header, 16, e_type, machine, 1)
    struct.pack_into("<QQQ", header, 24, entry, 64, 0)
    struct.pack_into("<HHHH", header, 52, 64, 56, len(phdrs), 64)
    table = b"".join(struct.pack("<I", p_type) + bytes(52) for p_type in phdrs)
    return bytes(header) + table


class ElfCheckTest(unittest.TestCase):
    def test_static_executable_passes(self):
        self.assertIsNone(elfcheck.problem(elf()))

    def test_static_pie_passes(self):
        self.assertIsNone(elfcheck.problem(elf(e_type=3, phdrs=(6, 1, 2))))

    def test_interpreter_is_refused(self):
        self.assertIn("PT_INTERP", elfcheck.problem(elf(phdrs=(6, 3, 1))))

    def test_wrong_files_are_refused(self):
        self.assertEqual(elfcheck.problem(b"#!/bin/sh\n"), "not an ELF file")
        self.assertIn("ELF64", elfcheck.problem(elf(cls=1)))
        self.assertIn("x86-64", elfcheck.problem(elf(machine=183)))
        self.assertIn("not an executable", elfcheck.problem(elf(e_type=1)))
        self.assertIn("entry", elfcheck.problem(elf(entry=0)))
        self.assertIn("program headers", elfcheck.problem(elf(phdrs=())))

    def test_truncated_program_headers_are_refused(self):
        self.assertIn("past the end", elfcheck.problem(elf()[:80]))

    def test_check_reports_a_missing_file(self):
        self.assertEqual(elfcheck.check(HERE / "no-such-binary"), "missing")


class PinsTest(unittest.TestCase):
    def test_every_program_is_pinned(self):
        self.assertEqual(set(sources.PINS), set(build.NAMES))

    def test_pins_are_complete(self):
        for name, pin in sources.PINS.items():
            with self.subTest(name=name):
                self.assertRegex(pin.sha256, r"^[0-9a-f]{64}$")
                self.assertTrue(pin.url.startswith(("https://", "http://")))
                self.assertIn(pin.version.split(".")[0], pin.url)
                self.assertTrue(pin.archive.startswith(pin.topdir.split("-")[0]))
        digests = [pin.sha256 for pin in sources.PINS.values()]
        self.assertEqual(len(digests), len(set(digests)))

    def test_member_outside_the_destination_is_refused(self):
        with tempfile.TemporaryDirectory() as scratch:
            base = Path(scratch)
            self.assertEqual(sources._inside(base, "a/b.c"), (base / "a/b.c").resolve())
            with self.assertRaises(ValueError):
                sources._inside(base, "../evil")

    def test_zip_symlink_members_are_skipped(self):
        with tempfile.TemporaryDirectory() as scratch:
            base = Path(scratch)
            archive = base / "in.zip"
            with zipfile.ZipFile(archive, "w") as zf:
                zf.writestr("pkg/file.txt", "data")
                link = zipfile.ZipInfo("pkg/link")
                link.external_attr = (stat.S_IFLNK | 0o777) << 16
                zf.writestr(link, "/etc/passwd")
            out = base / "out"
            sources._unpack(archive, out)
            self.assertEqual((out / "pkg/file.txt").read_text(), "data")
            self.assertFalse((out / "pkg/link").exists())


class JqTest(unittest.TestCase):
    def test_builtin_inc_matches_the_makefile_sed(self):
        text = 'def f: "a\\\\b";\nx\n'
        self.assertEqual(recipes.c_string_lines(text, True),
                         '"def f: \\"a\\\\\\\\b\\";\\n"\n"x\\n"\n')

    def test_config_opts_has_no_newline_escape(self):
        self.assertEqual(recipes.c_string_lines("(unknown)\n", False), '"(unknown)"\n')

    def test_math_defines_are_unique_and_known(self):
        self.assertEqual(len(recipes.JQ_MATH), len(set(recipes.JQ_MATH)))
        self.assertNotIn("gamma", recipes.JQ_MATH)  # not in musl
        self.assertIn("-DHAVE_LGAMMA_R=1", recipes.jq_flags())


MKTOKENS = """\
: "${TMPDIR:=/tmp}"

cat > "${TMPDIR}"/ka$$ <<\\!
TEOF	1	end of file
TSEMI	0	";"
TNOT	0	"!"
TCASE	0	"case"
TEND	1	"}"
!
nl=`wc -l "${TMPDIR}"/ka$$`
"""

TOKEN_VARS_H = """
/* Array indicating which tokens mark the end of a list */
static const char tokendlist[] = {
\t1,
\t0,
\t0,
\t0,
\t1,
};

static const char *const tokname[] = {
\t"end of file",
\t"\\";\\"",
\t"\\"!\\"",
\t"\\"case\\"",
\t"\\"}\\"",
};

#define KWDOFFSET 2

static const char *const parsekwd[] = {
\t"!",
\t"case",
\t"}"
};
"""

BUILTINS_DEF = ("bgcmd\t-u bg\nbreakcmd\t-s break -s continue\n\n# comment\n"
                "echocmd\techo\nevalcmd\t-ns eval\ntruecmd\t-s : -u true\n"
                "testcmd\ttest [\n")

BUILTINS_C = """/*
 * This file was generated by the mkbuiltins program.
 */

#include "shell.h"
#include "builtins.h"

int bgcmd(int, char **);
int breakcmd(int, char **);
int echocmd(int, char **);
int evalcmd(int, char **);
int truecmd(int, char **);
int testcmd(int, char **);

const struct builtincmd builtincmd[] = {
\t{ ":", truecmd, 3 },
\t{ "[", testcmd, 0 },
\t{ "bg", bgcmd, 2 },
\t{ "break", breakcmd, 3 },
\t{ "continue", breakcmd, 3 },
\t{ "echo", echocmd, 0 },
\t{ "eval", NULL, 3 },
\t{ "test", testcmd, 0 },
\t{ "true", truecmd, 2 },
};
"""

BUILTINS_H_HEAD = """/*
 * This file was generated by the mkbuiltins program.
 */

#define BGCMD (builtincmd + 2)
#define BREAKCMD (builtincmd + 3)
#define ECHOCMD (builtincmd + 5)
#define EVALCMD (builtincmd + 6)
#define TESTCMD (builtincmd + 1)
#define TRUECMD (builtincmd + 0)

#define NUMBUILTINS 9
"""


class DashGenTest(unittest.TestCase):
    def test_token_table_is_read_from_the_script(self):
        rows = dashgen.token_table(MKTOKENS)
        self.assertEqual(rows[0], ("TEOF", "1", "end of file"))
        self.assertEqual(rows[1], ("TSEMI", "0", '";"'))

    def test_token_headers_match_mktokens(self):
        rows = dashgen.token_table(MKTOKENS)
        self.assertEqual(dashgen.token_h(rows),
                         "#define TEOF 0\n#define TSEMI 1\n#define TNOT 2\n"
                         "#define TCASE 3\n#define TEND 4\n")
        self.assertEqual(dashgen.token_vars_h(rows), TOKEN_VARS_H)

    def test_builtins_match_mkbuiltins(self):
        self.assertEqual(dashgen.builtins_c(BUILTINS_DEF), BUILTINS_C)
        header = dashgen.builtins_h(BUILTINS_DEF)
        self.assertTrue(header.startswith(BUILTINS_H_HEAD), header)
        self.assertTrue(header.endswith("extern const struct builtincmd builtincmd[];\n"))

    def test_musl_signal_names(self):
        names = dashgen.signal_names()
        self.assertEqual(len(names), 65)
        expected = {0: "EXIT", 6: "ABRT", 9: "KILL", 16: "16", 17: "CHLD", 29: "IO",
                    31: "SYS", 32: "32", 34: "34", 35: "RTMIN", 36: "RTMIN+1",
                    49: "RTMIN+14", 50: "RTMAX-14", 63: "RTMAX-1", 64: "RTMAX"}
        for number, name in expected.items():
            self.assertEqual(names[number], name, number)

    def test_odd_realtime_range_gets_an_extra_rtmin(self):
        names = dashgen.signal_names(nsig=11, rtmin=4, rtmax=10, named={})
        self.assertEqual(names[4:11], ["RTMIN", "RTMIN+1", "RTMIN+2", "RTMIN+3",
                                       "RTMAX-2", "RTMAX-1", "RTMAX"])

    def test_signames_c_is_null_terminated(self):
        text = dashgen.signames_c(["EXIT", "HUP"])
        self.assertIn('    "EXIT",\n    "HUP",\n    (char *)0x0\n};\n', text)
        self.assertIn("signal_names[NSIG + 1]", text)

    def test_config_h_disables_line_editing(self):
        self.assertRegex(dashgen.CONFIG_H, re.compile(r"^#define SMALL 1$", re.M))
        self.assertNotIn("HAVE_GLOB ", dashgen.CONFIG_H)


class RgEditTest(unittest.TestCase):
    def test_edits_apply_once_or_fail(self):
        with tempfile.TemporaryDirectory() as scratch:
            tree = Path(scratch)
            (tree / "f.txt").write_text("keep\ndrop\nkeep\n", encoding="utf-8")
            rg.apply_edits(tree, [("f.txt", "drop\n", "")])
            self.assertEqual((tree / "f.txt").read_text(encoding="utf-8"), "keep\nkeep\n")
            with self.assertRaises(BuildError):
                rg.apply_edits(tree, [("f.txt", "keep\n", "")])  # twice

    def test_jemalloc_edits_cover_manifest_source_and_lock(self):
        files = [name for name, _, _ in rg.JEMALLOC_EDITS]
        self.assertEqual(sorted(set(files)),
                         ["Cargo.lock", "Cargo.toml", "crates/core/main.rs"])


if __name__ == "__main__":
    unittest.main()
