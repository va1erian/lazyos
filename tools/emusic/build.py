#!/usr/bin/env python3
"""Build emusic for LazyOS: the program and the `emusic.lzp` package.

Steps (``docs/media-plan.md`` P3):

1. build ``emusic/`` (Rust, static musl) with the zig toolchain, which also
   compiles SQLite; cargo fetches va1erian/emusic at the revision
   ``emusic/Cargo.toml`` pins (``EMUSIC_REV``) -> ``target/emusic/emusic.elf``;
2. assemble the package tree (``emusic/package/`` + ``bin/emusic.elf`` + the
   licence files) and build it with ``tools/pkg/build.py`` ->
   ``target/pkg/emusic.lzp``, which the image embeds when ``LAZYOS_EMUSIC=1``
   as ``/system/share/samples/emusic.lzp``. Install it on LazyOS as a user
   package: copy it to your home and run ``pkgctl install`` on the copy.

Usage::

    python tools/emusic/build.py                          # program + package
    python tools/emusic/build.py --emusic-src ../emusic   # a local emusic checkout
    python tools/emusic/build.py --require                # a missing toolchain fails

``--emusic-src`` builds the pinned revision from a local clone instead of
GitHub (it must contain that commit): git's ``insteadOf`` points cargo's
fetch there. Prints a JSON map of what it built on stdout. Without zig or the
musl target it reports what is missing and exits 0 unless ``--require``.
"""

from __future__ import annotations

import argparse
import json
import os
import re
import shutil
import subprocess
import sys
import tempfile
from pathlib import Path

HERE = Path(__file__).resolve().parent
ROOT = HERE.parent.parent
sys.path.insert(0, str(ROOT / "tools" / "xui"))
sys.path.insert(0, str(ROOT / "tools" / "pkg"))
import zig  # noqa: E402

TARGET = "x86_64-unknown-linux-musl"
CRATE = ROOT / "emusic"
OUT_DIR = ROOT / "target" / "emusic"
#: Its own cargo target directory: the zig-linked build uses a different
#: RUSTFLAGS environment from `tools/xui/build.py`'s plain builds.
CARGO_TARGET_DIR = OUT_DIR / "cargo"
ELF = OUT_DIR / "emusic.elf"
PACKAGE_DIR = ROOT / "target" / "pkg"
#: The name the image embeds the package under (`build_support`).
PACKAGE_NAME = "emusic.lzp"
EMUSIC_REPO = "https://github.com/va1erian/emusic"


def log(message: str) -> None:
    print(f"emusic: {message}", file=sys.stderr)


def pinned_rev() -> str:
    """The emusic revision `emusic/Cargo.toml` pins (one rev for every crate)."""
    text = (CRATE / "Cargo.toml").read_text(encoding="utf-8")
    revs = set(re.findall(r'git = "' + re.escape(EMUSIC_REPO) + r'", rev = "([0-9a-f]{40})"', text))
    if len(revs) != 1:
        raise SystemExit(f"emusic/Cargo.toml must pin one emusic revision, found {sorted(revs)}")
    return revs.pop()


def source_env(local: Path | None) -> dict[str, str]:
    """The environment that makes cargo fetch emusic from `local` instead of
    GitHub (git's `insteadOf`, through the git CLI)."""
    if local is None:
        return {}
    url = local.resolve().as_uri()
    return {
        "CARGO_NET_GIT_FETCH_WITH_CLI": "true",
        "GIT_CONFIG_COUNT": "1",
        "GIT_CONFIG_KEY_0": f"url.{url}.insteadOf",
        "GIT_CONFIG_VALUE_0": EMUSIC_REPO,
    }


def build_program(debug: bool, local: Path | None) -> Path | None:
    """`target/emusic/emusic.elf`, or None when a prerequisite is missing. A
    compile error is fatal: CI must never ship a stale binary."""
    installed = subprocess.run(["rustup", "target", "list", "--installed"],
                               capture_output=True, text=True, cwd=CRATE)
    if TARGET not in installed.stdout:
        added = subprocess.run(["rustup", "target", "add", TARGET], capture_output=True,
                               text=True, cwd=CRATE)
        if added.returncode != 0:
            log(f"cannot add {TARGET}: {added.stderr.strip()}")
            return None
    command = zig.find_zig()
    if command is None:
        log(f"zig not found (install: {zig.INSTALL_HINT})")
        return None
    found = zig.version(command)
    if found != zig.ZIG_VERSION:
        log(f"zig {found} found, {zig.ZIG_VERSION} is the tested version")
    wrappers = zig.write_wrappers(command, OUT_DIR / "zig")
    env = dict(os.environ)
    env.update(zig.cargo_env(TARGET, wrappers))
    env.update(source_env(local))
    cargo = ["cargo", "build", "--manifest-path", str(CRATE / "Cargo.toml"),
             "--target", TARGET, "--target-dir", str(CARGO_TARGET_DIR)]
    if not debug:
        cargo.append("--release")
    log(f"building lazyemusic at emusic {pinned_rev()[:10]} (zig cc, static musl)")
    built = subprocess.run(cargo, cwd=CRATE, env=env, capture_output=True, text=True)
    if built.returncode != 0:
        log("lazyemusic build failed")
        print(built.stderr[-4000:], file=sys.stderr)
        raise SystemExit(1)
    output = CARGO_TARGET_DIR / TARGET / ("debug" if debug else "release") / "lazyemusic"
    OUT_DIR.mkdir(parents=True, exist_ok=True)
    shutil.copyfile(output, ELF)
    return ELF


def emusic_licence(local: Path | None) -> Path:
    """emusic's LICENSE in the checkout cargo built from."""
    env = dict(os.environ)
    env.update(source_env(local))
    meta = subprocess.run(["cargo", "metadata", "--format-version", "1",
                           "--manifest-path", str(CRATE / "Cargo.toml")],
                          cwd=CRATE, env=env, capture_output=True, text=True, check=True)
    for package in json.loads(meta.stdout)["packages"]:
        if package["name"] == "emusic-ui":
            licence = Path(package["manifest_path"]).parents[2] / "LICENSE"
            if licence.is_file():
                return licence
    raise SystemExit("emusic's LICENSE not found in the cargo checkout")


def build_package(elf: Path, local: Path | None) -> Path:
    """`target/pkg/emusic.lzp` from the checked-in tree plus the program and
    the licence files."""
    import build as pkg_build  # tools/pkg/build.py

    with tempfile.TemporaryDirectory() as scratch:
        tree = Path(scratch) / "emusic"
        shutil.copytree(CRATE / "package", tree)
        (tree / "bin").mkdir()
        shutil.copyfile(elf, tree / "bin" / "emusic.elf")
        resources = tree / "resources"
        shutil.copyfile(CRATE / "NOTICE", resources / "NOTICE.txt")
        shutil.copyfile(emusic_licence(local), resources / "LICENSE-emusic.txt")
        try:
            archive = pkg_build.build(tree, Path(scratch) / "dist")
        except pkg_build.BuildError as error:
            log(f"the package does not validate:\n{error}")
            raise SystemExit(1)
        PACKAGE_DIR.mkdir(parents=True, exist_ok=True)
        final = PACKAGE_DIR / PACKAGE_NAME
        shutil.copyfile(archive, final)
    log(f"{final} ({final.stat().st_size // 1024} KiB)")
    return final


def main() -> int:
    """Parse arguments, build the program and package, print what was built;
    return 1 only when `--require` is set and something is missing."""
    parser = argparse.ArgumentParser(description=__doc__,
                                     formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--debug", action="store_true", help="build the debug profile")
    parser.add_argument("--program-only", action="store_true", help="skip the package")
    parser.add_argument("--emusic-src", type=Path,
                        help="a local emusic clone holding the pinned revision")
    parser.add_argument("--require", action="store_true",
                        help="fail (exit 1) when a toolchain is unavailable")
    args = parser.parse_args()
    local = args.emusic_src or (Path(os.environ["EMUSIC_SRC"]) if os.environ.get("EMUSIC_SRC") else None)

    built: dict[str, str] = {}
    elf = build_program(args.debug, local)
    if elf is not None:
        built["lazyemusic"] = str(elf)
        if not args.program_only:
            built["package"] = str(build_package(elf, local))
    print(json.dumps(built, indent=2))
    wanted = {"lazyemusic"} if args.program_only else {"lazyemusic", "package"}
    if args.require and not wanted <= built.keys():
        log(f"unavailable: {', '.join(sorted(wanted - built.keys()))}")
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
