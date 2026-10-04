#!/usr/bin/env python3
"""Tests of the Windows xui-checkout fixup (`tools/xui/git_checkout.py`).

These do not need the network. They build a throwaway git repo that looks like
cargo's ``git/db`` layout, then drive the helper's seeding against it, including
submodules whose trees have colon-named test files Windows cannot check out.

    python tools/xui/test_git_checkout.py
"""

from __future__ import annotations

import os
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))
import git_checkout  # noqa: E402

# Two submodule paths in the shape the real checkout uses.
LIB1 = "crates/xui-netsurf/netsurf-sys/vendor/libnsbmp"
LIB2 = "crates/xui-netsurf/netsurf-sys/vendor/libnsgif"


def git(*args: str, cwd: Path | None = None, check: bool = True) -> str:
    return subprocess.run(
        ["git", *args], cwd=cwd, check=check, capture_output=True, text=True
    ).stdout.strip()


class GitCheckoutTests(unittest.TestCase):
    def setUp(self) -> None:
        tmp = tempfile.TemporaryDirectory()
        self.addCleanup(tmp.cleanup)
        self.home = Path(tmp.name)
        self.cargo_home = self.home / "cargo"
        git("init", "--bare", "-q", str(self.cargo_home / "git" / "db" / "xui-abc"))
        self.git = self.cargo_home / "git"
        self.db = self.git / "db" / "xui-abc"
        self.libs = {}
        self._seed_db_with_xui_tree()
        self._saved_rev = git_checkout.XUI_REV
        self.addCleanup(setattr, git_checkout, "XUI_REV", self._saved_rev)

    def _library(self, name: str, colon_file: bool) -> str:
        """A local library repo with `src` and a colon-named test file."""
        repo = self.home / name
        repo.mkdir()
        git("init", "-q", str(repo))
        (repo / "src").mkdir()
        (repo / "src" / "lib.c").write_text("")
        (repo / "include").mkdir()
        (repo / "include" / "lib.h").write_text("")
        (repo / "test" / "afl").mkdir(parents=True)
        (repo / "test" / "afl" / "id:1.bmp").write_text("")
        if not colon_file:
            (repo / "test" / "afl" / "plain.bmp").write_text("")
        git("add", "-A", cwd=repo)
        git("-c", "user.email=t@t", "-c", "user.name=t", "commit", "-qm", "x", cwd=repo)
        return git("rev-parse", "HEAD", cwd=repo)

    def _seed_db_with_xui_tree(self) -> None:
        """A commit with xui's shape (`crates/`, `Cargo.toml`) and two
        submodules (both carrying colon-named test files)."""
        lib1 = self.home / "libnsbmp-src"
        lib2 = self.home / "libnsgif-src"
        self.libs[LIB1] = self._library("libnsbmp-src", colon_file=True)
        self.libs[LIB2] = self._library("libnsgif-src", colon_file=True)

        work = self.home / "xui-work"
        work.mkdir()
        git("init", "-q", str(work))
        (work / "Cargo.toml").write_text("[workspace]\n")
        (work / "crates" / "xui-core" / "src").mkdir(parents=True)
        (work / "crates" / "xui-core" / "src" / "lib.rs").write_text("")
        for path, repo in ((LIB1, lib1), (LIB2, lib2)):
            (work / path).parent.mkdir(parents=True, exist_ok=True)
            git("-c", "user.email=t@t", "-c", "user.name=t",
                "-c", "protocol.file.allow=always",
                "submodule", "add", "-q", str(repo), path, cwd=work)
        git("add", "-A", cwd=work)
        git("-c", "user.email=t@t", "-c", "user.name=t", "commit", "-qm", "x", cwd=work)
        git("push", "-q", str(self.db), "HEAD:refs/heads/main", cwd=work)
        self.rev = git("rev-parse", "HEAD", cwd=work)
        git_checkout.XUI_REV = self.rev

    def checkout(self) -> Path:
        return self.git / "checkouts" / "xui-abc" / self.rev[:7]

    def test_db_for_points_at_the_bare_repo(self) -> None:
        self.assertEqual(git_checkout._db_for(self.checkout()), self.db)

    def test_find_checkouts_matches_xui_by_shape(self) -> None:
        self.assertEqual(git_checkout.find_checkouts(self.git, self.rev), [self.checkout()])

    def test_find_checkouts_ignores_unknown_revision(self) -> None:
        self.assertEqual(git_checkout.find_checkouts(self.git, "0" * 40), [])

    def test_find_checkouts_handles_two_url_spellings(self) -> None:
        # A second bare repo, the `www.github.com` spelling cargo treats as a
        # different source but the same tree.
        second = self.git / "db" / "xui-www"
        git("init", "--bare", "-q", str(second))
        git("push", "-q", str(second), f"{self.rev}:refs/heads/main", cwd=self.home / "xui-work")
        found = git_checkout.find_checkouts(self.git, self.rev)
        self.assertEqual(
            {c.parent.name for c in found}, {"xui-abc", "xui-www"}
        )

    def test_submodules_lists_both(self) -> None:
        checkout = self.checkout()
        checkout.parent.mkdir(parents=True, exist_ok=True)
        git("clone", "--quiet", str(self.db), str(checkout))
        git("-C", str(checkout), "checkout", "--quiet", "--force", self.rev)
        self.assertEqual(
            [p for _n, p, _u in git_checkout._submodules(checkout)],
            [LIB1, LIB2],
        )

    def test_seed_populates_every_submodule_without_the_colon_paths(self) -> None:
        if os.name != "nt":
            # The colon file checks out fine elsewhere; the skip logic only
            # matters on Windows, where the file cannot exist at all.
            self.skipTest("Windows-only fixup")
        checkout = self.checkout()
        git_checkout.seed(checkout)
        self.assertTrue(git_checkout.checkout_seeded(checkout))
        for path in (LIB1, LIB2):
            self.assertTrue((checkout / path / "src").is_dir())
            self.assertTrue((checkout / path / "include").is_dir())
            self.assertFalse((checkout / path / "test").exists())

    def test_seeded_checkout_is_not_reseeded(self) -> None:
        if os.name != "nt":
            self.skipTest("Windows-only fixup")
        checkout = self.checkout()
        git_checkout.seed(checkout)
        before = sorted(p.name for p in (checkout / LIB1).iterdir())
        git_checkout.seed(checkout)
        self.assertEqual(before, sorted(p.name for p in (checkout / LIB1).iterdir()))


if __name__ == "__main__":
    unittest.main()
