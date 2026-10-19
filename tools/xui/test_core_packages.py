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
import zipfile
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

#: The windowless tray applets (docs/tray-plan.md T3,
#: `build_support/core_packages.rs` APPLETS).
WINDOWLESS = ("volume", "netstatus", "removable")


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
            if short in WINDOWLESS:
                # A tray applet never opens a window: no display, but resident.
                self.assertNotIn("os.lazy.display.v1", manifest["permissions"]["interfaces"])
                self.assertTrue(manifest["entry"]["resident"], short)
            else:
                self.assertIn("os.lazy.display.v1", manifest["permissions"]["interfaces"])
                # `SetCursor` (docs/cursor-plan.md): every client window asks
                # for its pointer shape, and a label without the scope has the
                # call refused once per run (a traced run reports it).
                self.assertIn("os.lazy.display.cursor.v1", manifest["permissions"]["interfaces"], short)
            self.assertTrue((core_packages.SOURCES / short / "docs" / "README.md").is_file())

    @unittest.skipIf(tomllib is None, "needs Python 3.11+")
    def test_packages_outside_the_core_set_that_ask_for_a_cursor_declare_it(self) -> None:
        # The same rule for the packages that are not core apps: emusic and the
        # Counter sample draw through `LazyOSBackend`, SRB2Kart calls
        # `SetCursor` itself, and Doom has declared the scope since the cursor
        # API. Quake is not here: it never asks for a shape.
        root = core_packages.ROOT
        for tree in ("games/doom/package", "games/srb2kart/package", "emusic/package", "tools/pkg/samples/counter"):
            manifest = tomllib.loads((root / tree / "manifest.toml").read_text(encoding="utf-8"))
            self.assertIn("os.lazy.display.cursor.v1", manifest["permissions"]["interfaces"], tree)

    @unittest.skipIf(tomllib is None, "needs Python 3.11+")
    def test_lazyrad_ships_its_player_beside_the_ide_and_is_not_an_exception(self) -> None:
        # The IDE is a core package like the other desktop apps (it was an
        # unlabelled system program before docs/lazyrad-package-plan.md A).
        app = core_packages.CORE_APPS["lazyrad"]
        self.assertEqual(app.programs, {"lazyrad.elf": "bin/lazyrad.elf",
                                        "lrplay.elf": "bin/lrplay.elf"})
        self.assertEqual(app.build_dir, "lazyrad")
        self.assertTrue(app.optional, "built and listed only for LAZYOS_LAZYRAD=1 images")
        # The tray demo is opt-in too (LAZYOS_TRAYDEMO=1): a default desktop
        # build must not fail for want of its ELF.
        self.assertTrue(core_packages.CORE_APPS["traydemo"].optional)
        # So are the games (LAZYOS_GAMES=1).
        self.assertTrue(core_packages.CORE_APPS["golf"].optional)
        self.assertTrue(core_packages.CORE_APPS["nes"].optional)
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
                         ["os.lazy.display.v1", "os.lazy.display.cursor.v1", "os.lazy.input.v1",
                          "os.lazy.clipboard.v1", "os.lazy.confd.v1", "os.lazy.print.v1"])
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
        # (minus the LazyRAD IDE and the Picture Viewer, LazyRAD programs that
        # `xui_embed` adds on their own switches).
        source = (core_packages.ROOT / "build_support" / "core_packages.rs").read_text(encoding="utf-8")
        block = re.search(r"const CORE: &\[&str\] = &\[(.*?)\];", source, re.S)
        self.assertIsNotNone(block)
        stems = set(re.findall(r'"([a-z]+)"', block.group(1)))
        self.assertEqual(stems, set(core_packages.CORE_APPS) - {"lazyrad", "pictures"})

    def test_the_package_carries_both_programs(self) -> None:
        import zipfile
        archive = next(p for p in self.build("lr") if p.name.startswith("os.lazy.lazyrad-"))
        with zipfile.ZipFile(archive) as zf:
            names = set(zf.namelist())
            self.assertLessEqual({"bin/lazyrad.elf", "bin/lrplay.elf", "manifest.toml"}, names)
            self.assertNotEqual(zf.read("bin/lazyrad.elf"), zf.read("bin/lrplay.elf"))

    @unittest.skipIf(tomllib is None, "needs Python 3.11+")
    def test_the_picture_viewer_is_a_lazyrad_project_with_sample_pictures(self) -> None:
        import zipfile
        app = core_packages.CORE_APPS["pictures"]
        self.assertTrue(app.optional, "shipped only by LAZYOS_PICTURES=1 images")
        archive = next(p for p in self.build("pv") if p.name.startswith("os.lazy.pictures-"))
        with zipfile.ZipFile(archive) as zf:
            names = set(zf.namelist())
            project = core_packages.PROJECT_DIR
            self.assertLessEqual({"bin/pictures.elf", f"{project}/Pictures.lrp",
                                  f"{project}/main_form.lfm", f"{project}/main_form.rhai",
                                  f"{project}/paths.rhai", f"{project}/samples/Aurora.jpg"},
                                 names)
            manifest = tomllib.loads(zf.read("manifest.toml").decode())
        self.assertEqual(manifest["app"]["category"], "graphics")
        opened = {mime["type"] for mime in manifest["mime"] if "open" in mime["verbs"]}
        self.assertEqual(opened, {"image/png", "image/jpeg", "image/bmp", "image/gif"})
        for mime in manifest["mime"]:
            self.assertNotIn("edit", mime["verbs"], "Edit stays with Paint")
        self.assertIn("os.lazy.mimed.v1", manifest["permissions"]["interfaces"])

    def test_a_project_file_glob_that_matches_nothing_is_an_error(self) -> None:
        app = core_packages.CoreApp({"lrplay.elf": "bin/lrplay.elf"}, build_dir="lazyrad",
                                    project="lazyrad-os/samples/pictures",
                                    project_files=(("assets/no-such/*.jpg", "samples"),))
        with self.assertRaises(core_packages.CoreError):
            core_packages.copy_project(app, self.dir / "tree")

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

    def test_an_unchanged_package_is_not_made_again(self) -> None:
        out = self.dir / "g"
        first = self.build("g")
        times = {path: path.stat().st_mtime_ns for path in first}
        autostart = {path: path.stat().st_mtime_ns
                     for path in (out / core_packages.AUTOSTART_DIR).glob("*.lzp")}
        again = self.build("g")
        self.assertEqual([p.name for p in again], [p.name for p in first])
        for path, mtime in times.items():
            self.assertEqual(path.stat().st_mtime_ns, mtime, f"{path.name} was written again")
        for path, mtime in autostart.items():
            self.assertEqual(path.stat().st_mtime_ns, mtime, f"{path.name} was written again")

    def test_a_changed_program_makes_only_its_package(self) -> None:
        first = {p.name: p.stat().st_mtime_ns for p in self.build("h")}
        (self.xui / "xui-paint.elf").write_bytes(b"\x7fELF a different paint")
        again = {p.name: p.stat().st_mtime_ns for p in self.build("h")}
        changed = [name for name in first if first[name] != again[name]]
        self.assertEqual(changed, ["os.lazy.paint-0.1.0.lzp"])
        # What it holds is the new program.
        with zipfile.ZipFile(self.dir / "h" / "os.lazy.paint-0.1.0.lzp") as archive:
            self.assertEqual(archive.read("bin/paint.elf"), b"\x7fELF a different paint")

    def test_a_changed_version_makes_every_package_again(self) -> None:
        self.build("i")
        names = {p.name for p in core_packages.build_core_packages(
            self.xui, self.dir / "i", "0.2.0", lazyrad_dir=self.lazyrad)}
        self.assertTrue(names and all(name.endswith("-0.2.0.lzp") for name in names))
        # The 0.1.0 archives are stale and gone, not left beside them.
        self.assertEqual({p.name for p in (self.dir / "i").glob("*.lzp")}, names)

    def test_an_oversized_package_is_an_error(self) -> None:
        # Noise does not deflate, so the archive is as large as the program.
        # Written a mebibyte at a time: `randbytes` of the whole limit at once
        # asks `getrandbits` for more bits than a C int holds.
        rng = random.Random(7)
        with (self.xui / "xui-paint.elf").open("wb") as program:
            for _ in range(core_packages.MAX_PACKAGE_FILE // (1 << 20) + 1):
                program.write(rng.randbytes(1 << 20))
        with self.assertRaises(core_packages.CoreError):
            self.build("f")


if __name__ == "__main__":
    unittest.main()
