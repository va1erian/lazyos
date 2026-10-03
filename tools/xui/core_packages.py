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
boot. Every archive is checked against `pkgd`'s 8 MiB package limit.

`tools/xui/build.py` runs this after building the apps; by hand:

    python tools/xui/core_packages.py [--xui-dir target/xui] [--lazyrad-dir target/lazyrad] \
        [--out target/pkg/core]
"""

from __future__ import annotations

import argparse
import hashlib
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
import build as pkgbuild  # noqa: E402

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
#: callers (`lazyrad-os/src/handoff`); its Play runs still inherit its label
#: until development labels land (`docs/lazyrad-package-plan.md`, phase B).
CORE_APPS: dict[str, CoreApp] = {
    "sysmon": xui_app("xui-sysmon.elf", "sysmon"),
    "fabricmon": xui_app("xui-fabricmon.elf", "fabricmon"),
    "widget": xui_app("xui-widget.elf", "widget"),
    "counter": xui_app("xui-counter.elf", "counter"),
    "editor": xui_app("xui-editor.elf", "editor"),
    "files": xui_app("xui-files.elf", "files"),
    "paint": xui_app("xui-paint.elf", "paint"),
    "writer": xui_app("xui-writer.elf", "writer"),
    "settings": xui_app("xui-settings.elf", "settings"),
    "confd": xui_app("xui-confd.elf", "confd"),
    # The network apps. Only a desktop image with the network stack ships them
    # (`LAZYOS_NETD=1`, `build_support/xui_embed.rs` NETWORK_XUI_APPS, which
    # fails that build when they are missing), so others need not build them.
    "network": xui_app("xui-network.elf", "network", optional=True),
    "nettools": xui_app("xui-nettools.elf", "nettools", optional=True),
    # C++ (litehtml), built only where zig is installed.
    "docs": xui_app("xui-docs.elf", "docs", optional=True),
    # The IDE and its player, built by `tools/lazyrad/build.py`.
    "lazyrad": CoreApp(
        {"lazyrad.elf": "bin/lazyrad.elf", "lrplay.elf": "bin/lrplay.elf"},
        build_dir="lazyrad",
        optional=True,
        built_by="python tools/lazyrad/build.py",
    ),
}

#: `pkgd`'s largest package file (`user/src/bin/pkgd/store.rs` MAX_PACKAGE_FILE).
MAX_PACKAGE_FILE = 8 * 1024 * 1024

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


def build_one(short: str, programs: dict[Path, str], version: str, autostart: bool,
              out_dir: Path) -> Path:
    """Build one variant of one package into `out_dir`; returns the archive.
    `programs` maps each built program to its path inside the package."""
    with tempfile.TemporaryDirectory() as scratch:
        tree = Path(scratch) / short
        shutil.copytree(SOURCES / short, tree)
        manifest = tree / "manifest.toml"
        manifest.write_text(render_manifest(manifest.read_text(encoding="utf-8"), version, autostart),
                            encoding="utf-8", newline="\n")
        for program, inside in programs.items():
            binary = tree / inside
            binary.parent.mkdir(parents=True, exist_ok=True)
            shutil.copyfile(program, binary)
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
        shutil.copyfile(archive, final)
        return final


def digest(path: Path) -> str:
    return hashlib.sha256(path.read_bytes()).hexdigest()


def build_core_packages(xui_dir: Path, out_dir: Path, version: str | None = None,
                        lazyrad_dir: Path | None = None) -> list[Path]:
    """Build every core package whose programs exist; returns the archives.

    A missing optional program (Docs without zig, LazyRAD not built) is skipped
    with a note; a missing mandatory one is an error. Stale archives are
    removed first, so an app dropped from the set leaves the image at the next
    build.
    """
    version = version or workspace_version()
    build_dirs = {"xui": xui_dir, "lazyrad": lazyrad_dir or ROOT / "target" / "lazyrad"}
    for stale in list(out_dir.glob("*.lzp")) + list((out_dir / AUTOSTART_DIR).glob("*.lzp")):
        stale.unlink()
    lines = ["# short system_name version file digest autostart_file autostart_digest"]
    written = []
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
        plain = build_one(short, programs, version, False, out_dir)
        auto = build_one(short, programs, version, True, out_dir / AUTOSTART_DIR)
        system_name = plain.name[: -len(f"-{version}.lzp")]
        lines.append(" ".join([short, system_name, version,
                               plain.relative_to(out_dir).as_posix(), digest(plain),
                               auto.relative_to(out_dir).as_posix(), digest(auto)]))
        written.append(plain)
    (out_dir / LIST).write_text("\n".join(lines) + "\n", encoding="utf-8", newline="\n")
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
