#!/usr/bin/env python3
"""Build SRB2Kart for LazyOS: the engine, the program and the `srb2kart.lzp` package.

Steps (``docs/srb2kart-port-plan.md``, ``srb2kart/README.md``):

1. fetch the Kart-Public v1.6 source at its pinned digest and the official
   v1.6 game data (``tools/srb2kart/fetch.py``);
2. build ``srb2kart/`` (Rust, static musl) with the zig toolchain, which also
   compiles the C engine and our platform layer (``srb2kart/build.rs``)
   -> ``target/srb2kart/srb2kart.elf``;
3. assemble the package tree (``srb2kart/package/`` + ``bin/srb2kart.elf`` +
   the licence files) and build it with ``tools/pkg/build.py``
   -> ``target/pkg/srb2kart.lzp``. The package is the engine only: the game
   data (``target/srb2kart/data/``) has no licence and is never packaged;
   the image build embeds it at ``/system/share/srb2kart/`` instead.

Usage::

    python tools/srb2kart/build.py                # engine + data + package
    python tools/srb2kart/build.py --engine-only  # just target/srb2kart/srb2kart.elf
    python tools/srb2kart/build.py --require      # a missing toolchain or download fails

Prints a JSON map of what it built on stdout (``srb2kart``, ``data``,
``package``). Without zig, the musl target or network access it reports what
is missing and exits 0 (CI passes ``--require``, so a missing input never
looks like a pass there).
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
sys.path.insert(0, str(ROOT / "tools"))
from outfile import copy_if_changed  # noqa: E402
sys.path.insert(0, str(HERE))
sys.path.insert(0, str(ROOT / "tools" / "xui"))
sys.path.insert(0, str(ROOT / "tools" / "pkg"))
import fetch  # noqa: E402
import zig  # noqa: E402

TARGET = "x86_64-unknown-linux-musl"
CRATE = ROOT / "srb2kart"
OUT_DIR = ROOT / "target" / "srb2kart"
#: Its own cargo target directory: the zig-linked build uses a different
#: RUSTFLAGS environment from `tools/xui/build.py`'s plain builds.
CARGO_TARGET_DIR = OUT_DIR / "cargo"
ELF = OUT_DIR / "srb2kart.elf"
PACKAGE_DIR = ROOT / "target" / "pkg"
#: The 8.3 name the image embeds the package under (`build_support`).
PACKAGE_NAME = "srb2kart.lzp"


def build_engine(debug: bool) -> Path | None:
    """`target/srb2kart/srb2kart.elf`, or None when a prerequisite is missing.
    A compile error is fatal: CI must never ship a stale binary."""
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
    source = fetch.source()
    if source is None:
        return None
    wrappers = zig.write_wrappers(command, OUT_DIR / "zig")
    env = dict(os.environ)
    env.update(zig.cargo_env(TARGET, wrappers))
    env["SRB2KART_SRC"] = str(source)
    cargo = ["cargo", "build", "--manifest-path", str(CRATE / "Cargo.toml"),
             "--target", TARGET, "--target-dir", str(CARGO_TARGET_DIR)]
    if not debug:
        cargo.append("--release")
    fetch.log("building the engine and lazysrb2kart (zig cc, static musl)")
    built = subprocess.run(cargo, cwd=ROOT, env=env, capture_output=True, text=True)
    if built.returncode != 0:
        fetch.log("lazysrb2kart build failed")
        print(built.stdout[-2000:], file=sys.stderr)
        print(built.stderr[-8000:], file=sys.stderr)
        raise SystemExit(1)
    output = CARGO_TARGET_DIR / TARGET / ("debug" if debug else "release") / "lazysrb2kart"
    OUT_DIR.mkdir(parents=True, exist_ok=True)
    copy_if_changed(output, ELF)
    return ELF


def build_package(elf: Path) -> Path | None:
    """`target/pkg/srb2kart.lzp` from the checked-in package tree plus the built
    program and the licence files, or None when the tree is not there yet
    (it is written by the packaging work, not by this script)."""
    template = CRATE / "package"
    if not (template / "manifest.toml").is_file():
        fetch.log(f"{template} has no manifest.toml yet; skipping the package")
        return None
    import build as pkg_build  # tools/pkg/build.py

    with tempfile.TemporaryDirectory() as scratch:
        tree = Path(scratch) / "srb2kart"
        shutil.copytree(template, tree)
        (tree / "bin").mkdir(exist_ok=True)
        shutil.copyfile(elf, tree / "bin" / "srb2kart.elf")
        resources = tree / "resources"
        resources.mkdir(exist_ok=True)
        for name, dest in (("NOTICE", "NOTICE.txt"), ("CREDITS.txt", "CREDITS.txt")):
            licence = CRATE / name
            if licence.is_file():
                shutil.copyfile(licence, resources / dest)
            else:
                fetch.log(f"{licence} is missing; the package goes without it")
        try:
            archive = pkg_build.build(tree, Path(scratch) / "dist")
        except pkg_build.BuildError as error:
            fetch.log(f"the package does not validate:\n{error}")
            raise SystemExit(1)
        PACKAGE_DIR.mkdir(parents=True, exist_ok=True)
        final = PACKAGE_DIR / PACKAGE_NAME
        copy_if_changed(archive, final)
    fetch.log(f"{final} ({final.stat().st_size // 1024} KiB)")
    return final


def main() -> int:
    """Parse arguments, build the engine and package, and print what was built;
    return 1 only when `--require` is set and a wanted artifact is missing."""
    parser = argparse.ArgumentParser(description=__doc__,
                                     formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--debug", action="store_true", help="build the debug profile")
    parser.add_argument("--engine-only", action="store_true",
                        help="skip the game data and the package")
    parser.add_argument("--require", action="store_true",
                        help="fail (exit 1) when a toolchain or download is unavailable")
    args = parser.parse_args()

    built: dict[str, str] = {}
    elf = build_engine(args.debug)
    if elf is not None:
        built["srb2kart"] = str(elf)
        if not args.engine_only:
            data = fetch.data()
            if data is not None:
                built["data"] = str(data)
            package = build_package(elf)
            if package is not None:
                built["package"] = str(package)
    print(json.dumps(built, indent=2))
    wanted = {"srb2kart"} if args.engine_only else {"srb2kart", "data", "package"}
    if args.require and not wanted <= built.keys():
        fetch.log(f"unavailable: {', '.join(sorted(wanted - built.keys()))}")
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
