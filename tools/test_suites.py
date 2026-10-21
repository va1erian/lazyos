#!/usr/bin/env python3
"""Tests for the software suites (`tools/suites.toml`, `tools/suites.py`) and
what reads them: `run_demo.py --suite` and `--dry-run` (no QEMU, no build).

Run: python tools/test_suites.py
"""

from __future__ import annotations

import io
import os
import re
import sys
import unittest
from contextlib import redirect_stderr, redirect_stdout
from pathlib import Path
from unittest import mock

sys.path.insert(0, str(Path(__file__).resolve().parent))
import demo_args  # noqa: E402
import run_demo  # noqa: E402
import suites  # noqa: E402
from lazygui import osvolume  # noqa: E402

DOCS = Path(__file__).resolve().parent.parent / "docs" / "build-switches.md"
#: Profile flags with no build switch of their own.
NO_SWITCH = {"big-app"}


def switch_rows() -> dict[str, list[str]]:
    """`flag -> switches` from the Front end column of every table row of build-switches.md."""
    rows: dict[str, list[str]] = {}
    for line in DOCS.read_text(encoding="utf-8").splitlines():
        cells = [cell.strip() for cell in line.strip().strip("|").split("|")]
        if not line.startswith("| `LAZYOS_") or len(cells) < 3:
            continue
        for flag in re.findall(r"`--([a-z0-9-]+)", cells[-1]):
            rows.setdefault(flag, []).append(cells[0].strip("`"))
    return rows


def optional_table_flags() -> set[str]:
    """The flags of the rows in "Optional apps and packages"."""
    text = DOCS.read_text(encoding="utf-8")
    section = text.split("## Optional apps and packages", 1)[1].split("\n## ", 1)[0]
    return set(re.findall(r"`--([a-z0-9-]+)", "\n".join(
        line.rsplit("|", 2)[-2] for line in section.splitlines() if line.startswith("| `LAZYOS_"))))


class DefinitionTests(unittest.TestCase):
    def setUp(self) -> None:
        self.data = suites.load()
        self.parser = demo_args.make_parser("", Path("x.img"))
        self.known = {option[2:] for action in self.parser._actions
                      for option in action.option_strings if option.startswith("--")}

    def named(self) -> set[str]:
        return {flag for name in self.data["suites"] for flag in suites.suite_flags(name, self.data)} \
            | {flag for entry in self.data["options"].values() for flag in entry["flags"]}

    def test_every_flag_exists_in_run_demo(self) -> None:
        for flag in self.named() | set(self.data["test_only"]):
            self.assertIn(flag, self.known, f"--{flag} is not a run_demo.py flag")

    def test_every_suite_flag_has_its_switch_documented(self) -> None:
        rows = switch_rows()
        for flag in self.named() - NO_SWITCH:
            self.assertIn(flag, rows, f"--{flag} has no row in docs/build-switches.md")

    def test_no_test_only_flag_is_in_a_suite(self) -> None:
        self.assertFalse(self.named() & set(self.data["test_only"]))

    def test_every_optional_app_chooses_a_place(self) -> None:
        """A new app switch in the "Optional apps" table must join `full`, an option or `test_only`."""
        placed = self.named() | set(self.data["test_only"])
        missing = optional_table_flags() - placed
        self.assertFalse(missing, f"place these in tools/suites.toml: {sorted(missing)}")

    def test_full_extends_base_and_srb2kart_is_an_option_not_in_full(self) -> None:
        base, full = suites.suite_flags("base", self.data), suites.suite_flags("full", self.data)
        self.assertEqual(base, ["desktop"])
        self.assertEqual(full[:len(base)], base)
        self.assertIn("srb2kart", suites.option_names(self.data))
        self.assertNotIn("srb2kart", full)
        for flag in ("webkit", "lazywebplus", "lazyweb", "mail", "lazysky", "games", "doom",
                     "quake", "emusic", "linuxapps", "lazyrad", "modplayer", "pictures", "tls",
                     "net", "big-app", "devtools"):
            self.assertIn(flag, full)

    def test_resolve_adds_options_without_duplicates_and_refuses_unknown_names(self) -> None:
        flags = suites.resolve("full", ["srb2kart"], self.data)
        self.assertEqual(len(flags), len(set(flags)))
        self.assertEqual(flags[-1], "srb2kart")
        with self.assertRaises(ValueError):
            suites.resolve("huge", (), self.data)
        with self.assertRaises(ValueError):
            suites.resolve("base", ["nope"], self.data)

    def test_os_size_is_osvolumes(self) -> None:
        full = suites.resolve("full")
        self.assertEqual(suites.os_size_bytes(suites.resolve("base")), osvolume.required_bytes(False, False))
        self.assertEqual(suites.os_size_bytes(full), osvolume.required_bytes(False, True))
        self.assertEqual(suites.os_size_bytes(suites.resolve("full", ["srb2kart"])),
                         osvolume.required_bytes(True, True))
        self.assertEqual(suites.os_size_bytes(full) >> 20, 1024)

    def test_a_suite_naming_a_test_only_flag_is_refused(self) -> None:
        import tempfile
        with tempfile.TemporaryDirectory() as tmp:
            path = Path(tmp) / "suites.toml"
            path.write_text('[suites.base]\nflags=["desktop"]\n[suites.full]\nflags=["jsc"]\n'
                            '[test_only]\nflags=["jsc"]\n', encoding="utf-8")
            with self.assertRaises(ValueError):
                suites.load(path)


class RunDemoTests(unittest.TestCase):
    def setUp(self) -> None:
        ambient = {k: v for k, v in os.environ.items() if not k.startswith("LAZYOS_")}
        patch = mock.patch.dict(os.environ, ambient, clear=True)
        patch.start()
        self.addCleanup(patch.stop)

    def parse(self, *argv: str):
        parser = demo_args.make_parser("", Path("x.img"))
        return demo_args.parse_args(parser, list(argv))[0]

    def test_suite_turns_on_its_flags_and_their_implications(self) -> None:
        args = self.parse("--suite", "full")
        for name in ("desktop", "net", "tls", "webkit", "lazywebplus", "games", "doom", "quake",
                     "emusic", "linuxapps", "mail", "lazysky", "lazyweb", "lazyrad", "modplayer", "pictures",
                     "devtools"):
            self.assertTrue(getattr(args, name), name)
        self.assertFalse(args.srb2kart)
        self.assertFalse(args.jsc)
        self.assertEqual(args.memory, "4G")  # the big-app profile
        self.assertIn("quota_user_memory=8G", args.limit)

    def test_with_adds_the_option_and_needs_a_suite(self) -> None:
        self.assertTrue(self.parse("--suite", "full", "--with", "srb2kart").srb2kart)
        with redirect_stderr(io.StringIO()), self.assertRaises(SystemExit):
            self.parse("--with", "srb2kart")

    def test_base_is_the_desktop_alone_and_explicit_flags_add(self) -> None:
        args = self.parse("--suite", "base", "--traydemo")
        self.assertTrue(args.desktop and args.traydemo)
        self.assertFalse(args.net or args.webkit or args.doom)

    def test_dry_run_prints_the_plan_and_builds_nothing(self) -> None:
        out = io.StringIO()
        with mock.patch("subprocess.Popen", side_effect=AssertionError("started a process")), \
                redirect_stdout(out):
            code = run_demo.main(["--suite", "full", "--with", "srb2kart", "--dry-run", "--yes"])
        text = out.getvalue()
        self.assertEqual(code, 0)
        self.assertIn("suite full +srb2kart", text)
        self.assertIn("OS volume 1536 MiB", text)
        for step in ("  webkit\n", "  lazywebplus\n", "  srb2kart\n", "  doom\n", "  cargo build"):
            self.assertIn(step.strip(), text)
        for switch in ("LAZYOS_DESKTOP=1", "LAZYOS_WEBKIT=1", "LAZYOS_SRB2KART=1", "LAZYOS_TLS=1",
                       "LAZYOS_LIMIT_QUOTA_USER_MEMORY=8G"):
            self.assertIn(switch, text)
        self.assertNotIn("LAZYOS_JSC", text)


if __name__ == "__main__":
    unittest.main()
