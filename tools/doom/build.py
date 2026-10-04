#!/usr/bin/env python3
"""Build Doom for LazyOS: the engine, the program and the `doom.lzp` package.

Steps (``docs/doom-port-plan.md``, ``doom/README.md``):

1. fetch doomgeneric at its pinned revision (``tools/doom/fetch.py``);
2. build ``doom/`` (Rust, static musl) with the zig toolchain, which also
   compiles the C engine (``doom/build.rs``) -> ``target/doom/doom.elf``;
3. fetch Freedoom's release zip (pinned SHA-256) for ``freedoom1.wad``;
4. assemble the package tree (``doom/package/`` + ``bin/doom.elf`` +
   ``resources/freedoom1.wad`` + the licence files) and build it with
   ``tools/pkg/build.py`` -> ``target/pkg/doom.lzp``, which the image embeds
   when ``LAZYOS_DOOM=1`` as ``/system/share/samples/doom.lzp``. Install it on
   LazyOS as a user package: copy it to your home and run ``pkgctl install``
   on the copy (or open it in Files).

Usage::

    python tools/doom/build.py                # engine + package
    python tools/doom/build.py --engine-only  # just target/doom/doom.elf
    python tools/doom/build.py --require      # a missing toolchain or download fails

Prints a JSON map of what it built on stdout. Without zig, the musl target or
network access it reports what is missing and exits 0 (CI's Doom job passes
``--require``, so a missing input never looks like a pass there).
"""

from __future__ import annotations

import argparse
import json
import os
import shutil
import subprocess
import sys
import tempfile
from pathlib import Path

HERE = Path(__file__).resolve().parent
ROOT = HERE.parent.parent
sys.path.insert(0, str(HERE))
sys.path.insert(0, str(ROOT / "tools" / "xui"))
sys.path.insert(0, str(ROOT / "tools" / "pkg"))
import fetch  # noqa: E402
import git_checkout  # noqa: E402
import zig  # noqa: E402

TARGET = "x86_64-unknown-linux-musl"
CRATE = ROOT / "doom"
OUT_DIR = ROOT / "target" / "doom"
#: Its own cargo target directory: the zig-linked build uses a different
#: RUSTFLAGS environment from `tools/xui/build.py`'s plain builds.
CARGO_TARGET_DIR = OUT_DIR / "cargo"
ELF = OUT_DIR / "doom.elf"
PACKAGE_DIR = ROOT / "target" / "pkg"
#: The 8.3 name the image embeds the package under (`build_support`).
PACKAGE_NAME = "doom.lzp"


def build_engine(debug: bool) -> Path | None:
    """`target/doom/doom.elf`, or None when a prerequisite is missing. A
    compile error is fatal: CI must never ship a stale binary."""
    installed = subprocess.run(["rustup", "target", "list", "--installed"],
                               capture_output=True, text=True)
    if TARGET not in installed.stdout:
        added = subprocess.run(["rustup", "target", "add", TARGET], capture_output=True, text=True)
        if added.returncode != 0:
            fetch.log(f"cannot add {TARGET}: {added.stderr.strip()}")
            return None
    command = zig.find_zig()
    if command is None:
        fetch.log(f"zig not found (install: {zig.INSTALL_HINT})")
        return None
    found = zig.version(command)
    if found != zig.ZIG_VERSION:
        fetch.log(f"zig {found} found, {zig.ZIG_VERSION} is the tested version")
    source = fetch.doomgeneric()
    if source is None:
        return None
    wrappers = zig.write_wrappers(command, OUT_DIR / "zig")
    env = dict(os.environ)
    env.update(zig.cargo_env(TARGET, wrappers))
    env["DOOMGENERIC_SRC"] = str(source)
    # Windows: doom pulls the pinned xui, whose NetSurf submodules name files
    # Windows cannot check out; resolve (and seed cargo's checkout) first so the
    # real build does not trip on the submodule.
    if os.name == "nt" and not git_checkout.resolve(CRATE / "Cargo.toml", env=env):
        raise SystemExit(1)
    cargo = ["cargo", "build", "--manifest-path", str(CRATE / "Cargo.toml"),
             "--target", TARGET, "--target-dir", str(CARGO_TARGET_DIR)]
    if not debug:
        cargo.append("--release")
    fetch.log("building the engine and lazydoom (zig cc, static musl)")
    built = subprocess.run(cargo, cwd=ROOT, env=env, capture_output=True, text=True)
    if built.returncode != 0:
        fetch.log("lazydoom build failed")
        print(built.stderr[-4000:], file=sys.stderr)
        raise SystemExit(1)
    output = CARGO_TARGET_DIR / TARGET / ("debug" if debug else "release") / "lazydoom"
    OUT_DIR.mkdir(parents=True, exist_ok=True)
    shutil.copyfile(output, ELF)
    return ELF


def build_package(elf: Path) -> Path | None:
    """`target/pkg/doom.lzp` from the checked-in tree plus the built and
    fetched files, or None when Freedoom is unavailable."""
    import build as pkg_build  # tools/pkg/build.py

    wad_dir = fetch.freedoom()
    if wad_dir is None:
        return None
    with tempfile.TemporaryDirectory() as scratch:
        tree = Path(scratch) / "doom"
        shutil.copytree(CRATE / "package", tree)
        (tree / "bin").mkdir()
        shutil.copyfile(elf, tree / "bin" / "doom.elf")
        resources = tree / "resources"
        resources.mkdir()
        shutil.copyfile(wad_dir / "freedoom1.wad", resources / "freedoom1.wad")
        shutil.copyfile(wad_dir / "COPYING.txt", resources / "COPYING-freedoom.txt")
        shutil.copyfile(CRATE / "NOTICE", resources / "NOTICE.txt")
        try:
            archive = pkg_build.build(tree, Path(scratch) / "dist")
        except pkg_build.BuildError as error:
            fetch.log(f"the package does not validate:\n{error}")
            raise SystemExit(1)
        PACKAGE_DIR.mkdir(parents=True, exist_ok=True)
        final = PACKAGE_DIR / PACKAGE_NAME
        shutil.copyfile(archive, final)
    fetch.log(f"{final} ({final.stat().st_size // 1024} KiB)")
    return final


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__,
                                     formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--debug", action="store_true", help="build the debug profile")
    parser.add_argument("--engine-only", action="store_true", help="skip Freedoom and the package")
    parser.add_argument("--require", action="store_true",
                        help="fail (exit 1) when a toolchain or download is unavailable")
    args = parser.parse_args()

    built: dict[str, str] = {}
    elf = build_engine(args.debug)
    if elf is not None:
        built["lazydoom"] = str(elf)
        if not args.engine_only:
            package = build_package(elf)
            if package is not None:
                built["package"] = str(package)
    print(json.dumps(built, indent=2))
    wanted = {"lazydoom"} if args.engine_only else {"lazydoom", "package"}
    if args.require and not wanted <= built.keys():
        fetch.log(f"unavailable: {', '.join(sorted(wanted - built.keys()))}")
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
