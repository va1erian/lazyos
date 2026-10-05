#!/usr/bin/env python3
"""Tests of the core package build (`tools/xui/core_packages.py`, issue #509).

    python tools/xui/test_core_packages.py
"""

from __future__ import annotations

import hashlib
import random
import re
import sys
import tempfile
import unittest
from pathlib import Path

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))
import core_packages  # noqa: E402

sys.path.insert(0, str(core_packages.ROOT / "tools" / "pkg"))
import pkgmanifest  # noqa: E402

try:
    import tomllib
except ModuleNotFoundError:  # pragma: no cover - Python < 3.11
    tomllib = None


def sha(path: Path) -> str:
    return hashlib.sha256(path.read_bytes()).hexdigest()


class CorePackageTests(unittest.TestCase):
    def setUp(self) -> None:
        tmp = tempfile.TemporaryDirectory()
        self.addCleanup(tmp.cleanup)
        self.dir = Path(tmp.name)
        self.xui = self.dir / "xui"
        self.xui.mkdir()
        self.lazyrad = self.dir / "lazyrad"
        self.lazyrad.mkdir()
        # Stand-ins for the built programs: different bytes per program.
        for short, app in core_packages.CORE_APPS.items():
            for name in app.programs:
                built = {"xui": self.xui, "lazyrad": self.lazyrad}[app.build_dir]
                (built / name).write_bytes(b"\x7fELF " + (short + name).encode() * 1000)

    def build(self, out: str):
        return core_packages.build_core_packages(self.xui, self.dir / out, "0.1.0",
                                                 lazyrad_dir=self.lazyrad)

    def test_every_core_app_has_a_source_tree_and_nothing_else_does(self) -> None:
        trees = sorted(p.name for p in core_packages.SOURCES.iterdir() if p.is_dir())
        self.assertEqual(trees, sorted(core_packages.CORE_APPS))
        for kept_out in ("terminal", "installer", "devices", "shell"):
            self.assertNotIn(kept_out, core_packages.CORE_APPS)

    @unittest.skipIf(tomllib is None, "needs Python 3.11+")
    def test_every_source_manifest_is_valid_and_names_its_app(self) -> None:
        for short in core_packages.CORE_APPS:
            text = (core_packages.SOURCES / short / "manifest.toml").read_text(encoding="utf-8")
            manifest = tomllib.loads(text)
            self.assertEqual(pkgmanifest.validate_manifest(manifest), [], short)
            self.assertEqual(manifest["app"]["system_name"], f"os.lazy.{short}")
            self.assertEqual(manifest["entry"]["binary"], f"bin/{short}.elf")
            self.assertIn(manifest["entry"]["binary"], core_packages.CORE_APPS[short].programs.values())
            self.assertEqual(manifest["entry"]["args"], ["--client"])
            self.assertEqual(manifest["entry"]["abi"], "linux")
            self.assertIn("os.lazy.display.v1", manifest["permissions"]["interfaces"])
            self.assertTrue((core_packages.SOURCES / short / "docs" / "README.md").is_file())

    @unittest.skipIf(tomllib is None, "needs Python 3.11+")
    def test_lazyrad_ships_its_player_beside_the_ide_and_is_not_an_exception(self) -> None:
        # The IDE is a core package like the other desktop apps (it was an
        # unlabelled system program before docs/lazyrad-package-plan.md A).
        app = core_packages.CORE_APPS["lazyrad"]
        self.assertEqual(app.programs, {"lazyrad.elf": "bin/lazyrad.elf",
                                        "lrplay.elf": "bin/lrplay.elf"})
        self.assertEqual(app.build_dir, "lazyrad")
        self.assertTrue(app.optional, "built and listed only for LAZYOS_LAZYRAD=1 images")
        text = (core_packages.SOURCES / "lazyrad" / "manifest.toml").read_text(encoding="utf-8")
        manifest = tomllib.loads(text)
        self.assertEqual(manifest["app"]["category"], "development")
        # The install handoff (lazyrad-os/src/handoff): never `pkgd`.
        permissions = manifest["permissions"]
        self.assertIn("os.lazy.mimed.v1", permissions["interfaces"])
        self.assertIn("os.lazy.init.v1", permissions["interfaces"])
        self.assertIn("subscribe:system/events/pkg/+", permissions["topics"])
        self.assertNotIn("os.lazy.pkgd.v1", permissions["interfaces"])

    @unittest.skipIf(tomllib is None, "needs Python 3.11+")
    def test_lazywriter_is_an_office_app_for_its_own_documents(self) -> None:
        # Issue #533: cargo bin `writer`, built as `xui-writer.elf`, packaged
        # as `bin/writer.elf`; it takes `.lzw` (mimed maps the extension) and
        # leaves text/plain and text/markdown to the Editor and Docs.
        # `build` is tools/pkg/build.py here (core_packages imports it), so
        # load tools/xui/build.py under its own name.
        import importlib.util
        spec = importlib.util.spec_from_file_location("xui_build", HERE / "build.py")
        xui_build = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(xui_build)
        self.assertEqual(xui_build.BINS["writer"], "xui-writer.elf")
        self.assertEqual(core_packages.CORE_APPS["writer"].programs,
                         {"xui-writer.elf": "bin/writer.elf"})
        self.assertFalse(core_packages.CORE_APPS["writer"].optional)
        text = (core_packages.SOURCES / "writer" / "manifest.toml").read_text(encoding="utf-8")
        manifest = tomllib.loads(text)
        self.assertEqual(manifest["app"]["name"], "LazyWriter")
        self.assertEqual(manifest["app"]["category"], "office")
        self.assertEqual(manifest["mime"], [{"type": "application/x-lazywriter",
                                             "verbs": ["open", "edit"]}])
        self.assertEqual(manifest["permissions"]["interfaces"],
                         ["os.lazy.display.v1", "os.lazy.input.v1", "os.lazy.clipboard.v1",
                          "os.lazy.confd.v1", "os.lazy.print.v1"])
        # Printing (docs/xui-writer.md): pages go to the print spooler, which
        # talks to the printer, so LazyWriter needs no network access itself.
        self.assertNotIn("network", manifest["permissions"])
        for size in (16, 32, 128):
            self.assertTrue((core_packages.SOURCES / "writer" / "icons" / f"app-{size}.png").is_file())

    @unittest.skipIf(tomllib is None, "needs Python 3.11+")
    def test_archiver_opens_every_archive_type_it_reads(self) -> None:
        # docs/archiver-plan.md: cargo bin `xui-archiver`, packaged as
        # `bin/archiver.elf`; first in the start menu's installed section
        # (accessories) so no other row moves; the clipboard carries drags.
        import importlib.util
        spec = importlib.util.spec_from_file_location("xui_build", HERE / "build.py")
        xui_build = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(xui_build)
        self.assertEqual(xui_build.BINS["xui-archiver"], "xui-archiver.elf")
        self.assertEqual(core_packages.CORE_APPS["archiver"].programs,
                         {"xui-archiver.elf": "bin/archiver.elf"})
        self.assertFalse(core_packages.CORE_APPS["archiver"].optional)
        text = (core_packages.SOURCES / "archiver" / "manifest.toml").read_text(encoding="utf-8")
        manifest = tomllib.loads(text)
        self.assertEqual(manifest["app"]["system_name"], "os.lazy.archiver")
        self.assertEqual(manifest["app"]["category"], "accessories")
        types = {entry["type"] for entry in manifest["mime"]}
        self.assertEqual(types, {"application/zip", "application/x-tar", "application/gzip",
                                 "application/x-xz", "application/zstd",
                                 "application/x-7z-compressed"})
        self.assertIn("os.lazy.clipboard.v1", manifest["permissions"]["interfaces"])
        self.assertIn("os.lazy.mimed.v1", manifest["permissions"]["interfaces"])
        for size in (16, 32, 128):
            self.assertTrue((core_packages.SOURCES / "archiver" / "icons" / f"app-{size}.png").is_file())

    def test_the_image_build_lists_the_same_core_apps(self) -> None:
        # `build_support/core_packages.rs` `is_core_stem` mirrors CORE_APPS
        # (minus the LazyRAD IDE, which `lazyrad_embed` adds).
        source = (core_packages.ROOT / "build_support" / "core_packages.rs").read_text(encoding="utf-8")
        block = re.search(r"const CORE: &\[&str\] = &\[(.*?)\];", source, re.S)
        self.assertIsNotNone(block)
        stems = set(re.findall(r'"([a-z]+)"', block.group(1)))
        self.assertEqual(stems, set(core_packages.CORE_APPS) - {"lazyrad"})

    def test_the_package_carries_both_programs(self) -> None:
        import zipfile
        archive = next(p for p in self.build("lr") if p.name.startswith("os.lazy.lazyrad-"))
        with zipfile.ZipFile(archive) as zf:
            names = set(zf.namelist())
            self.assertLessEqual({"bin/lazyrad.elf", "bin/lrplay.elf", "manifest.toml"}, names)
            self.assertNotEqual(zf.read("bin/lazyrad.elf"), zf.read("bin/lrplay.elf"))

    def test_the_manifest_takes_the_workspace_version_and_the_autostart_flag(self) -> None:
        text = 'version = "0.1.0"\nautostart = false\n'
        self.assertEqual(core_packages.render_manifest(text, "1.2.3", True),
                         'version = "1.2.3"\nautostart = true\n')
        with self.assertRaises(core_packages.CoreError):
            core_packages.render_manifest('version = "0.1.0"\n', "1.0.0", False)
        self.assertRegex(core_packages.workspace_version(), r"^\d+\.\d+")

    def test_builds_are_reproducible_and_autostart_changes_the_digest(self) -> None:
        first = self.build("a")
        second = self.build("b")
        self.assertEqual(len(first), len(core_packages.CORE_APPS))
        for one, two in zip(first, second):
            self.assertEqual(one.name, two.name)
            self.assertEqual(sha(one), sha(two), f"{one.name} is not reproducible")
            auto = one.parent / core_packages.AUTOSTART_DIR / one.name
            self.assertNotEqual(sha(one), sha(auto))
            self.assertLessEqual(one.stat().st_size, core_packages.MAX_PACKAGE_FILE)
        listing = (self.dir / "a" / core_packages.LIST).read_text(encoding="utf-8")
        line = next(l for l in listing.splitlines() if l.startswith("editor "))
        short, name, version, plain, digest, auto, auto_digest = line.split()
        self.assertEqual((name, version), ("os.lazy.editor", "0.1.0"))
        self.assertEqual(digest, sha(self.dir / "a" / plain))
        self.assertEqual(auto_digest, sha(self.dir / "a" / auto))

    def test_a_missing_optional_app_is_skipped_and_a_mandatory_one_fails(self) -> None:
        (self.xui / "xui-docs.elf").unlink()
        (self.lazyrad / "lrplay.elf").unlink()  # the IDE without its player is not built either
        built = self.build("c")
        names = [p.name for p in built]
        self.assertNotIn("os.lazy.docs-0.1.0.lzp", names)
        self.assertNotIn("os.lazy.lazyrad-0.1.0.lzp", names)
        (self.xui / "xui-paint.elf").unlink()
        with self.assertRaises(core_packages.CoreError):
            self.build("d")

    def test_stale_archives_are_removed(self) -> None:
        out = self.dir / "e"
        (out / core_packages.AUTOSTART_DIR).mkdir(parents=True)
        stale = out / "os.lazy.gone-0.0.1.lzp"
        stale.write_bytes(b"old")
        core_packages.build_core_packages(self.xui, out, "0.1.0", lazyrad_dir=self.lazyrad)
        self.assertFalse(stale.exists())

    def test_an_oversized_package_is_an_error(self) -> None:
        # Noise does not deflate, so the archive is as large as the program.
        noise = random.Random(7).randbytes(core_packages.MAX_PACKAGE_FILE + 4096)
        (self.xui / "xui-paint.elf").write_bytes(noise)
        with self.assertRaises(core_packages.CoreError):
            self.build("f")


if __name__ == "__main__":
    unittest.main()
