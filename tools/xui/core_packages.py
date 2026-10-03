#!/usr/bin/env python3
"""Build the core packages: every desktop xui app as an `.lzp` (issue #509).

Each app has a package source tree under `xui-app/packages/<short>/` (manifest,
icons, docs). This copies the built `target/xui/<elf>` into a scratch copy of
the tree as `bin/<short>.elf`, sets the manifest's `version` to the workspace
version, and builds the archive with `tools/pkg/build.py`, twice: once with
`autostart = false` (`target/pkg/core/<sn>-<version>.lzp`) and once with
`autostart = true` (`target/pkg/core/autostart/<sn>-<version>.lzp`). The root
`build.rs` (`build_support/core_packages.rs`) embeds one of the two per app as
`/system/packages/<sn>.lzp`, as `LAZYOS_XUI_AUTOSTART` says at `cargo build`
time, and writes the index `pkgd` reads at boot from `target/pkg/core/core.lst`.

The archives are reproducible (fixed zip timestamps and order), so rebuilding
an unchanged app gives the same digest and `pkgd` does nothing at the next
boot. Every archive is checked against `pkgd`'s 8 MiB package limit.

`tools/xui/build.py` runs this after building the apps; by hand:

    python tools/xui/core_packages.py [--xui-dir target/xui] [--out target/pkg/core]
"""

from __future__ import annotations

import argparse
import hashlib
import re
import shutil
import sys
import tempfile
from pathlib import Path

HERE = Path(__file__).resolve().parent
ROOT = HERE.parent.parent
SOURCES = ROOT / "xui-app" / "packages"
sys.path.insert(0, str(ROOT / "tools" / "pkg"))
import build as pkgbuild  # noqa: E402

#: short id -> (the built program under target/xui, optional). The order is
#: `build_support/xui_embed.rs`'s `DESKTOP_XUI_APPS`, `DOCUMENT_XUI_APPS` and
#: `OPTIONAL_XUI_APPS`, minus the xui programs that stay unlabelled system
#: programs in `/system/bin` (`docs/packages.md`, core packages): LazyShell,
#: the Installer (`pkgd`'s trusted UI), the Terminal (its shell and every
#: command typed in it would inherit a package label) and Devices (it reads
#: the kernel's device inspection calls, `os.kernel.dev`, which no package
#: permission can name).
CORE_APPS: dict[str, tuple[str, bool]] = {
    "sysmon": ("xui-sysmon.elf", False),
    "fabricmon": ("xui-fabricmon.elf", False),
    "widget": ("xui-widget.elf", False),
    "counter": ("xui-counter.elf", False),
    "editor": ("xui-editor.elf", False),
    "files": ("xui-files.elf", False),
    "paint": ("xui-paint.elf", False),
    "settings": ("xui-settings.elf", False),
    "confd": ("xui-confd.elf", False),
    # The network apps. Only a desktop image with the network stack ships them
    # (`LAZYOS_NETD=1`, `build_support/xui_embed.rs` NETWORK_XUI_APPS, which
    # fails that build when they are missing), so others need not build them.
    "network": ("xui-network.elf", True),
    "nettools": ("xui-nettools.elf", True),
    # C++ (litehtml), built only where zig is installed.
    "docs": ("xui-docs.elf", True),
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


def build_one(short: str, program: Path, version: str, autostart: bool, out_dir: Path) -> Path:
    """Build one variant of one package into `out_dir`; returns the archive."""
    with tempfile.TemporaryDirectory() as scratch:
        tree = Path(scratch) / short
        shutil.copytree(SOURCES / short, tree)
        manifest = tree / "manifest.toml"
        manifest.write_text(render_manifest(manifest.read_text(encoding="utf-8"), version, autostart),
                            encoding="utf-8", newline="\n")
        binary = tree / "bin" / f"{short}.elf"
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


def build_core_packages(xui_dir: Path, out_dir: Path, version: str | None = None) -> list[Path]:
    """Build every core package whose program exists; returns the archives.

    A missing optional program (Docs without zig) is skipped with a warning; a
    missing mandatory one is an error. Stale archives are removed first, so an
    app dropped from the set leaves the image at the next build.
    """
    version = version or workspace_version()
    for stale in list(out_dir.glob("*.lzp")) + list((out_dir / AUTOSTART_DIR).glob("*.lzp")):
        stale.unlink()
    lines = ["# short system_name version file digest autostart_file autostart_digest"]
    written = []
    for short, (elf, optional) in CORE_APPS.items():
        program = xui_dir / elf
        if not program.is_file():
            if optional:
                print(f"warning: core package {short} skipped: {program} is not built",
                      file=sys.stderr)
                continue
            raise CoreError(f"core package {short}: {program} is not built")
        plain = build_one(short, program, version, False, out_dir)
        auto = build_one(short, program, version, True, out_dir / AUTOSTART_DIR)
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
    parser.add_argument("--out", type=Path, default=ROOT / "target" / "pkg" / "core")
    args = parser.parse_args(argv)
    try:
        for archive in build_core_packages(args.xui_dir, args.out):
            print(archive)
    except CoreError as error:
        print(f"error: {error}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
