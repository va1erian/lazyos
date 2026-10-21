"""Tests for the pure parts of repro_check: flags, digests and the diff."""

from __future__ import annotations

import sys
import tempfile
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))

import repro_check as rc  # noqa: E402


class FlagTests(unittest.TestCase):
    def test_remap_covers_checkout_toolchain_and_cargo_home(self) -> None:
        flags = rc.remap_flags(Path("/w/a"), Path("/tc"), Path("/home/u/.cargo"), Path("/w/a-target"))
        self.assertEqual(
            [f.rsplit("=", 1)[1] for f in flags], ["/src", "/cargo", "/rustc", "/target"]
        )
        self.assertTrue(all(f.startswith("--remap-path-prefix=") for f in flags))
        self.assertIn(f"--remap-path-prefix={Path('/w/a-target')}=/target", flags)

    def test_encoded_flags_are_unit_separated(self) -> None:
        self.assertEqual(rc.encoded_flags(["--cfg", "x"]), "--cfg\x1fx")

    def test_the_repository_config_flags_survive(self) -> None:
        # An env var replaces .cargo/config.toml's rustflags, so they must be read back.
        self.assertIn("aes_force_soft", rc.config_rustflags(rc.ROOT))


class SanityTests(unittest.TestCase):
    def test_an_empty_or_partial_build_is_a_problem(self) -> None:
        self.assertTrue(rc.missing({}, ["kernel"]))
        self.assertTrue(rc.missing({"kernel": "x"}, ["user"]))
        user = {name: "x" for name in rc.EXPECTED["user"]} | {f"p{i}": "x" for i in range(40)}
        self.assertEqual(rc.missing(user, ["user"]), [])
        self.assertEqual(rc.missing({"kernel": "x"}, ["kernel"]), [])
        self.assertTrue(rc.missing({"kernel": "x"}, ["user", "kernel"]))


class DigestTests(unittest.TestCase):
    def test_only_extensionless_files_count_and_differences_are_named(self) -> None:
        with tempfile.TemporaryDirectory() as a, tempfile.TemporaryDirectory() as b:
            for root, text in ((Path(a), b"one"), (Path(b), b"two")):
                (root / "init").write_bytes(b"same")
                (root / "init.d").write_bytes(text)
                (root / "kernel").write_bytes(text)
            (Path(b) / "extra").write_bytes(b"x")
            da, db = rc.digests(Path(a)), rc.digests(Path(b))
            self.assertEqual(sorted(da), ["init", "kernel"])
            self.assertEqual(rc.differences(da, db), ["extra", "kernel"])


if __name__ == "__main__":
    unittest.main()
