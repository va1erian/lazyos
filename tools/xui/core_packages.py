#!/usr/bin/env python3
"""Build the core packages: every desktop xui app as an `.lzp` (issue #509).

Each app has a package source tree under `xui-app/packages/<short>/` (manifest,
icons, docs). This copies the built `target/xui/<elf>` into a scratch copy of
the tree as `bin/<short>.elf` (the LazyRAD IDE copies its two programs from
`target/lazyrad/`, see `CORE_APPS`), sets the manifest's `version` to the
workspace version, and builds the archive with `tools/pkg/build.py`, twice: once with
`autostart = false` (`target/pkg/core/<sn>-<version>.lzp`) and once with
`autostart = true` (`target/pkg/core/autostart/<sn>-<version>.lzp`). The root
`build.rs` (`build_support/core_packages.rs`) embeds one of the two per app as
`/system/packages/<sn>.lzp`, as `LAZYOS_XUI_AUTOSTART` says at `cargo build`
time, and writes the index `pkgd` reads at boot from `target/pkg/core/core.lst`.

The archives are reproducible (fixed zip timestamps and order), so rebuilding
an unchanged app gives the same digest and `pkgd` does nothing at the next
boot. Every archive is checked against `pkgd`'s 256 MiB package limit.

`tools/xui/build.py` runs this after building the apps; by hand:

    python tools/xui/core_packages.py [--xui-dir target/xui] [--lazyrad-dir target/lazyrad] \
        [--out target/pkg/core]
"""

from __future__ import annotations

import argparse
import hashlib
import json
import re
import shutil
import sys
import tempfile
from dataclasses import dataclass
from pathlib import Path

HERE = Path(__file__).resolve().parent
ROOT = HERE.parent.parent
SOURCES = ROOT / "xui-app" / "packages"
sys.path.insert(0, str(ROOT / "tools" / "pkg"))
sys.path.insert(0, str(ROOT / "tools"))
import build as pkgbuild  # noqa: E402
from outfile import copy_file, copy_if_changed, copy_tree, write_if_changed  # noqa: E402

@dataclass(frozen=True)
class CoreApp:
    """One core package: which built programs go where in the archive."""

    #: built file name -> path inside the package (the manifest's `binary` first).
    programs: dict[str, str]
    #: the directory the programs are built into: "xui" (`target/xui`) or
    #: "lazyrad" (`target/lazyrad`).
    build_dir: str = "xui"
    #: a missing program skips the package (with a note) instead of failing.
    optional: bool = False
    #: how to build it, for the skip note.
    built_by: str = "python tools/xui/build.py"
    #: a LazyRAD project (relative to the root) copied to `resources/project`,
    #: the layout File -> Make LazyOS App gives a LazyRAD app.
    project: str | None = None
    #: extra files for the project: a glob relative to the root -> the folder
    #: inside the project they are copied to.
    project_files: tuple[tuple[str, str], ...] = ()


def xui_app(elf: str, short: str, optional: bool = False) -> CoreApp:
    """A desktop xui app: `target/xui/<elf>` as `bin/<short>.elf`."""
    return CoreApp({elf: f"bin/{short}.elf"}, optional=optional)


#: short id -> the package. The xui apps are `build_support/xui_embed.rs`'s
#: `DESKTOP_XUI_APPS`, `DOCUMENT_XUI_APPS` and `OPTIONAL_XUI_APPS`, minus the
#: xui programs that stay unlabelled system programs in `/system/bin`
#: (`docs/packages.md`, core packages): LazyShell, the Installer (`pkgd`'s
#: trusted UI), the Terminal (its shell and every command typed in it would
#: inherit a package label) and Devices (it reads the kernel's device
#: inspection calls, `os.kernel.dev`, which no package permission can name).
#: The LazyRAD IDE (`lazyrad-os/`, not an xui app) is a core package too, built
#: from `target/lazyrad/` with its player beside it (`lrplay` is found next to
#: `lazyrad` in the install directory) and shipped only in images built with
#: `LAZYOS_LAZYRAD=1` (`build_support/lazyrad_embed.rs`). It installs the apps
#: it builds through the Installer, never `pkgd`, which refuses labelled
#: callers (`lazyrad-os/src/handoff`), and declares `develop = true` so Play
#: runs the project under its own `dev:<system_name>` label
#: (`docs/lazyrad-package-plan.md`, phase B).
CORE_APPS: dict[str, CoreApp] = {
    "sysmon": xui_app("xui-sysmon.elf", "sysmon"),
    "fabricmon": xui_app("xui-fabricmon.elf", "fabricmon"),
    "widget": xui_app("xui-widget.elf", "widget"),
    "counter": xui_app("xui-counter.elf", "counter"),
    "editor": xui_app("xui-editor.elf", "editor"),
    "files": xui_app("xui-files.elf", "files"),
    "paint": xui_app("xui-paint.elf", "paint"),
    "writer": xui_app("xui-writer.elf", "writer"),
    # The Archiver (docs/archiver-plan.md).
    "archiver": xui_app("xui-archiver.elf", "archiver"),
    "settings": xui_app("xui-settings.elf", "settings"),
    "confd": xui_app("xui-confd.elf", "confd"),
    # The network apps. Only a desktop image with the network stack ships them
    # (`LAZYOS_NETD=1`, `build_support/xui_embed.rs` NETWORK_XUI_APPS, which
    # fails that build when they are missing), so others need not build them.
    "network": xui_app("xui-network.elf", "network", optional=True),
    "nettools": xui_app("xui-nettools.elf", "nettools", optional=True),
    # Network Drives: FTP servers as folders under /mnt, through `mountd`.
    "netdrives": xui_app("xui-netdrives.elf", "netdrives", optional=True),
    # Blitz (pure Rust), built with the other apps.
    "docs": xui_app("xui-docs.elf", "docs", optional=True),
    # The web browser (Blitz, pure Rust; docs/lazyweb.md). Only a
    # `LAZYOS_LAZYWEB=1` image ships it (`build_support/lazyweb_embed.rs`,
    # which fails that build when it is missing).
    "lazyweb": xui_app("xui-lazyweb.elf", "lazyweb", optional=True),
    # WebKit, the browser (docs/webkit-port-plan.md W5): one static program linked in the WebKit
    # build container by `tools/webkit/app.py`. Only a `LAZYOS_WEBKIT=1` image ships it
    # (`build_support/webkit_embed.rs`, which fails that build when it is missing).
    "webkit": CoreApp({"xui-webkit.elf": "bin/webkit.elf"}, optional=True,
                      built_by="python tools/webkit/app.py"),
    # LazyWeb+ (docs/lazywebplus.md): LazyWeb's chrome on the same WebKit build, one static program
    # linked by `tools/webkit/app.py --plus`. Only a `LAZYOS_LAZYWEBPLUS=1` image ships it
    # (`build_support/lazywebplus_embed.rs`, which fails that build when it is missing).
    "lazywebplus": CoreApp({"xui-lazywebplus.elf": "bin/lazywebplus.elf"}, optional=True,
                           built_by="python tools/webkit/app.py --plus"),
    # The Bluesky client (docs/lazysky.md). Only a `LAZYOS_LAZYSKY=1` image
    # ships it (`build_support/lazysky_embed.rs`, which fails that build when
    # it is missing).
    "lazysky": xui_app("xui-lazysky.elf", "lazysky", optional=True),
    # esMail (docs/mail.md): links SQLite's C (zig), built only on request
    # (`tools/xui/build.py --mail`); shipped by `LAZYOS_MAIL=1` images.
    "mail": xui_app("xui-mail.elf", "mail", optional=True),
    # The IDE and its player, built by `tools/lazyrad/build.py`.
    "lazyrad": CoreApp(
        {"lazyrad.elf": "bin/lazyrad.elf", "lrplay.elf": "bin/lrplay.elf"},
        build_dir="lazyrad",
        optional=True,
        built_by="python tools/lazyrad/build.py",
    ),
    # The Picture Viewer (docs/lazyrad-pictures.md): a LazyRAD project on the
    # player, with the wallpapers as its sample pictures. Only
    # `LAZYOS_PICTURES=1` images ship it (`build_support/pictures_embed.rs`).
    "pictures": CoreApp(
        {"lrplay.elf": "bin/pictures.elf"},
        build_dir="lazyrad",
        optional=True,
        built_by="python tools/lazyrad/build.py",
        project="lazyrad-os/samples/pictures",
        project_files=(("assets/wallpapers/*.jpg", "samples"),),
    ),
    # Calculator: A basic calculator.
    "calc": xui_app("xui-calc.elf", "calc"),
    # PDF Viewer: Read PDF documents.
    "pdf": xui_app("xui-pdf.elf", "pdf"),
    # Tray Demo, the tray sample app (docs/tray-plan.md T1): only
    # `LAZYOS_TRAYDEMO=1` images ship it, so a desktop build without its ELF
    # skips it here; such an image fails on it in `build_support/xui_embed.rs`.
    "traydemo": xui_app("xui-traydemo.elf", "traydemo", optional=True),
    # Volume, the tray applet over the mixer (docs/tray-plan.md T3).
    "volume": xui_app("xui-volume.elf", "volume"),
    # Network Status, the tray applet over `netd` (docs/tray-plan.md T3):
    # shipped with the other network apps (`LAZYOS_NETD=1`).
    "netstatus": xui_app("xui-netstatus.elf", "netstatus", optional=True),
    # LazyGolf: Fly over a procedurally generated golf course.
    "golf": xui_app("xui-golf.elf", "golf"),
    # Video Player: Play video files.
    "video": xui_app("xui-video.elf", "video"),
    # About LazyOS: About LazyOS, with a real-time 3D renderer.
    "about": xui_app("xui-about.elf", "about"),
}

#: `pkgd`'s largest package file (`user/src/bin/pkgd/store.rs` MAX_PACKAGE_FILE).
MAX_PACKAGE_FILE = 256 * 1024 * 1024

#: The list `build.rs` reads (one line per package).
LIST = "core.lst"
AUTOSTART_DIR = "autostart"


class CoreError(Exception):
    """A core package could not be built."""


def workspace_version(root: Path = ROOT) -> str:
    """The `[package] version` of the root `Cargo.toml`."""
    text = (root / "Cargo.toml").read_text(encoding="utf-8")
    match = re.search(r'^\[package\][^\[]*?^version\s*=\s*"([^"]+)"', text, re.M | re.S)
    if not match:
        raise CoreError("the root Cargo.toml has no [package] version")
    return match.group(1)


def render_manifest(text: str, version: str, autostart: bool) -> str:
    """The source manifest with `version` and `entry.autostart` set."""
    text, versions = re.subn(r'^version\s*=\s*"[^"]*"', f'version = "{version}"', text,
                             count=1, flags=re.M)
    text, autostarts = re.subn(r"^autostart\s*=\s*(true|false)",
                               f"autostart = {'true' if autostart else 'false'}", text,
                               count=1, flags=re.M)
    if versions != 1 or autostarts != 1:
        raise CoreError("a core manifest must have one `version` and one `autostart` line")
    return text


#: Where a LazyRAD app's project sits inside its package.
PROJECT_DIR = "resources/project"


def copy_project(app: CoreApp, tree: Path, root: Path = ROOT) -> None:
    """Copy `app`'s LazyRAD project, and its extra files, into `tree`."""
    if app.project is None:
        return
    project = tree / PROJECT_DIR
    copy_tree(root / app.project, project)
    for pattern, folder in app.project_files:
        matches = sorted(root.glob(pattern))
        if not matches:
            raise CoreError(f"{app.project}: no file matches {pattern}")
        (project / folder).mkdir(parents=True, exist_ok=True)
        for path in matches:
            copy_file(path, project / folder / path.name)


#: Beside the archives of one directory: for each package the digest of
#: everything its archive is made of, and the archive's name.
INPUTS_FILE = ".inputs.json"


#: (path, size, modified time) -> digest, so the two variants of a package, and
#: the same program in two packages, are read once per run.
_DIGESTS: dict[tuple[str, int, int], bytes] = {}


def file_digest(path: Path) -> bytes:
    """A digest of a file, read a mebibyte at a time."""
    stat = path.stat()
    key = (str(path), stat.st_size, stat.st_mtime_ns)
    if key not in _DIGESTS:
        h = hashlib.blake2b(digest_size=24)
        with path.open("rb") as handle:
            while chunk := handle.read(1 << 20):
                h.update(chunk)
        _DIGESTS[key] = h.digest()
    return _DIGESTS[key]


def tree_files(base: Path) -> list[Path]:
    """The files below `base`, in a fixed order."""
    return sorted(path for path in base.rglob("*") if path.is_file())


def inputs_key(short: str, programs: dict[Path, str], version: str, autostart: bool,
               app: CoreApp | None, root: Path = ROOT) -> str:
    """A digest of everything the archive is made of: the built programs, the
    package's source tree (and the LazyRAD project and its extra files), the
    version, the autostart flag and the code that renders and packs them. Two
    runs with the same key make the same archive, so the second need not."""
    h = hashlib.blake2b(digest_size=24)
    h.update(f"{short}\0{version}\0{int(autostart)}\0".encode())
    for program, inside in sorted(programs.items(), key=lambda item: item[1]):
        h.update(inside.encode() + b"\0" + file_digest(program))
    trees = [SOURCES / short]
    extra: list[Path] = []
    if app is not None and app.project is not None:
        trees.append(root / app.project)
        for pattern, folder in app.project_files:
            h.update(f"{pattern}\0{folder}\0".encode())
            extra += sorted(root.glob(pattern))
    for tree in trees:
        for path in tree_files(tree):
            h.update(path.relative_to(tree).as_posix().encode() + b"\0" + file_digest(path))
    for path in extra:
        h.update(path.name.encode() + b"\0" + file_digest(path))
    for code in (Path(__file__), Path(pkgbuild.__file__), HERE.parent / "pkg" / "pkgmanifest.py"):
        h.update(code.read_bytes())
    return h.hexdigest()


def load_inputs(out_dir: Path) -> dict:
    try:
        data = json.loads((out_dir / INPUTS_FILE).read_text(encoding="utf-8"))
    except (OSError, ValueError):
        return {}
    return data if isinstance(data, dict) else {}


def save_inputs(out_dir: Path, inputs: dict) -> None:
    out_dir.mkdir(parents=True, exist_ok=True)
    (out_dir / INPUTS_FILE).write_text(json.dumps(inputs, sort_keys=True), encoding="utf-8")


def build_one(short: str, programs: dict[Path, str], version: str, autostart: bool,
              out_dir: Path, app: CoreApp | None = None) -> Path:
    """Build one variant of one package into `out_dir`; returns the archive.
    `programs` maps each built program to its path inside the package.

    An archive whose inputs are the ones it was last made from is kept as it is
    (compressing a 100 MiB browser program takes seconds, and the launcher
    makes every package at least twice per run); one made again with the same
    bytes is not written, so its time does not change either."""
    key = inputs_key(short, programs, version, autostart, app)
    inputs = load_inputs(out_dir)
    made = inputs.get(short)
    if made and made.get("key") == key and (out_dir / made.get("archive", "")).is_file():
        return out_dir / made["archive"]
    with tempfile.TemporaryDirectory() as scratch:
        tree = Path(scratch) / short
        copy_tree(SOURCES / short, tree)
        if app is not None:
            copy_project(app, tree)
        manifest = tree / "manifest.toml"
        manifest.write_text(render_manifest(manifest.read_text(encoding="utf-8"), version, autostart),
                            encoding="utf-8", newline="\n")
        for program, inside in programs.items():
            binary = tree / inside
            binary.parent.mkdir(parents=True, exist_ok=True)
            copy_file(program, binary)
        try:
            archive = pkgbuild.build(tree, Path(scratch) / "dist")
        except pkgbuild.BuildError as error:
            raise CoreError(f"{short}: {error}") from error
        size = archive.stat().st_size
        if size > MAX_PACKAGE_FILE:
            raise CoreError(f"{short}: {archive.name} is {size} bytes, over pkgd's "
                            f"{MAX_PACKAGE_FILE}-byte package limit")
        out_dir.mkdir(parents=True, exist_ok=True)
        final = out_dir / archive.name
        # The record goes first: a copy that is cut short must not be taken
        # for the archive of the old inputs by a later run.
        if short in inputs:
            del inputs[short]
            save_inputs(out_dir, inputs)
        copy_if_changed(archive, final)
    inputs[short] = {"key": key, "archive": final.name}
    save_inputs(out_dir, inputs)
    return final


def digest(path: Path) -> str:
    return hashlib.sha256(path.read_bytes()).hexdigest()


def build_core_packages(xui_dir: Path, out_dir: Path, version: str | None = None,
                        lazyrad_dir: Path | None = None) -> list[Path]:
    """Build every core package whose programs exist; returns the archives.

    A missing optional program (Docs not built, LazyRAD not built) is skipped
    with a note; a missing mandatory one is an error. An archive whose inputs
    did not change is kept (see `build_one`); stale archives are removed at
    the end, so an app dropped from the set leaves the image at the next
    build.
    """
    version = version or workspace_version()
    build_dirs = {"xui": xui_dir, "lazyrad": lazyrad_dir or ROOT / "target" / "lazyrad"}
    lines = ["# short system_name version file digest autostart_file autostart_digest"]
    written = []
    keep: set[Path] = set()
    for short, app in CORE_APPS.items():
        programs = {build_dirs[app.build_dir] / name: inside
                    for name, inside in app.programs.items()}
        missing = [path for path in programs if not path.is_file()]
        if missing:
            if app.optional:
                print(f"note: core package {short} skipped: {missing[0]} is not built "
                      f"(`{app.built_by}`)", file=sys.stderr)
                continue
            raise CoreError(f"core package {short}: {missing[0]} is not built")
        plain = build_one(short, programs, version, False, out_dir, app)
        auto = build_one(short, programs, version, True, out_dir / AUTOSTART_DIR, app)
        keep.update((plain, auto))
        system_name = plain.name[: -len(f"-{version}.lzp")]
        lines.append(" ".join([short, system_name, version,
                               plain.relative_to(out_dir).as_posix(), digest(plain),
                               auto.relative_to(out_dir).as_posix(), digest(auto)]))
        written.append(plain)
    for directory in (out_dir, out_dir / AUTOSTART_DIR):
        for stale in directory.glob("*.lzp"):
            if stale not in keep:
                stale.unlink()
        inputs = load_inputs(directory)
        kept = {short: made for short, made in inputs.items()
                if (directory / made.get("archive", "")) in keep}
        if kept != inputs:
            save_inputs(directory, kept)
    # Only when it changed: the image build watches this file's time.
    write_if_changed(out_dir / LIST, ("\n".join(lines) + "\n").encode("utf-8"))
    return written


def main(argv=None) -> int:
    parser = argparse.ArgumentParser(description=__doc__,
                                     formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--xui-dir", type=Path, default=ROOT / "target" / "xui")
    parser.add_argument("--lazyrad-dir", type=Path, default=ROOT / "target" / "lazyrad")
    parser.add_argument("--out", type=Path, default=ROOT / "target" / "pkg" / "core")
    args = parser.parse_args(argv)
    try:
        for archive in build_core_packages(args.xui_dir, args.out, lazyrad_dir=args.lazyrad_dir):
            print(archive)
    except CoreError as error:
        print(f"error: {error}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
