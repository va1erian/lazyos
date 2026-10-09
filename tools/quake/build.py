#!/usr/bin/env python3
"""Build Quake for LazyOS: the engine, the program and the `quake.lzp`
package.

Steps (``docs/quake-port-plan.md``, ``quake/README.md``):

1. fetch the ``quake-srp`` port at its pinned revision
   (``tools/quake/qfetch.py``), then id's shareware 1.06 pak;
2. assemble the crate at ``target/quake/quake-srp-<rev>/lazyos/``: the
   fetched ``quake-wasm`` platform layer with this tree's overlay files
   (``quake/src/`` — the root, the patched ``common.rs`` and the
   ``lazy/`` platform) copied over it, and ``Cargo.toml`` written from
   the template with the LazyOS workspace's paths baked in;
3. build it (Rust, static musl, the zig toolchain links) ->
   ``target/quake/quake.elf``;
4. assemble the package tree (``quake/package/`` + ``bin/quake.elf`` +
   ``resources/id1/pak0.pak`` + the licence files) and build it with
   ``tools/pkg/build.py`` -> ``target/pkg/quake.lzp``, which the image
   embeds when ``LAZYOS_QUAKE=1`` as ``/system/share/samples/quake.lzp``.
   Install it on LazyOS as a user package: copy it to your home and run
   ``pkgctl install`` on the copy (or open it in Files).

Usage::

    python tools/quake/build.py                # engine + package
    python tools/quake/build.py --engine-only  # just target/quake/quake.elf
    python tools/quake/build.py --test         # the crate's tests on the host
    python tools/quake/build.py --require      # a missing download fails

Prints a JSON map of what it built on stdout. Without zig, the musl
target or network access the build reports what is missing and exits 0
(CI's Quake job passes ``--require``, so a missing input never looks like
a pass there).
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
import zig  # noqa: E402
import qfetch  # noqa: E402

TARGET = "x86_64-unknown-linux-musl"
OVERLAY = ROOT / "quake"
OUT_DIR = ROOT / "target" / "quake"
#: Its own cargo target directory: the zig-linked build uses a different
#: RUSTFLAGS environment from `tools/xui/build.py`'s plain builds.
CARGO_TARGET_DIR = ROOT / "target" / "quake" / "cargo"
ELF = OUT_DIR / "quake.elf"
PACKAGE_DIR = ROOT / "target" / "pkg"
#: The file name the image embeds the package under (`build_support`).
PACKAGE_NAME = "quake.lzp"
#: The assembled crate's directory inside the fetched tree: beside
#: `../quake-rs` (its path dependency) and `../quake-data` (the upstream
#: tests' shareware lies there, as quake-srp's own CI keeps it).
ASSEMBLED_NAME = "lazyos"


def log(message: str) -> None:
    qfetch.log(message)


def this_file(path: str) -> str:
    """This script's own digest (`Path`), the fingerprint's second input."""
    import hashlib
    return hashlib.sha256(Path(path).read_bytes()).hexdigest()


def assemble(tree: Path) -> Path | None:
    """The crate `tree/lazyos/`: `quake-wasm/` with the overlay on top.

    Rebuilt only when the overlay's fingerprint (the tree plus this
    script's file list) changes, so an incremental build keeps cargo's
    cache. Returns the crate's directory."""
    crate = tree / ASSEMBLED_NAME
    fingerprint = crate / ".lazyos-overlay"
    # The overlay's contents and this script both go into the fingerprint:
    # a build.py change that alters what is assembled (paths, more sources)
    # must trigger the rebuild too.
    script = this_file(__file__)
    wanted = f"{qfetch.QUAKE_SRP_REV}\n{qfetch.sha256_paths(OVERLAY)}\n{script}\n"
    if fingerprint.is_file() and fingerprint.read_text(encoding="utf-8") == wanted:
        return crate
    wasm = tree / "quake-wasm"
    if not (wasm / "src" / "sys.rs").is_file():
        log("quake-wasm/src is missing in the fetched tree")
        return None
    shutil.rmtree(crate, ignore_errors=True)
    crate.mkdir(parents=True)
    shutil.copyfile(wasm / "build.rs", crate / "build.rs")
    shutil.copytree(wasm / "src", crate / "src")
    # The overlay, last: the port's root, its patched `common.rs` and the
    # `lazy/` platform layer.
    for name, source in (("src/main.rs", OVERLAY / "src" / "main.rs"),
                         ("src/common.rs", OVERLAY / "src" / "common.rs")):
        shutil.copyfile(source, crate / name)
    shutil.copytree(OVERLAY / "src" / "lazy", crate / "src" / "lazy")
    cargo = (OVERLAY / "Cargo.toml.in").read_text(encoding="utf-8")
    root = ROOT.as_posix()
    crate.joinpath("Cargo.toml").write_text(cargo.replace("{ROOT}", root), encoding="utf-8")
    fingerprint.write_text(wanted, encoding="utf-8")
    return crate


def musl_installed() -> bool:
    installed = subprocess.run(
        ["rustup", "target", "list", "--installed"], capture_output=True, text=True
    )
    if TARGET not in installed.stdout:
        added = subprocess.run(
            ["rustup", "target", "add", TARGET], capture_output=True, text=True
        )
        if added.returncode != 0:
            log(f"cannot add {TARGET}: {added.stderr.strip()}")
            return False
    return True


def build_engine(debug: bool) -> Path | None:
    """``target/quake/quake.elf``, or None when a prerequisite is missing.
    A compile error is fatal: CI must never ship a stale binary."""
    if not musl_installed():
        return None
    command = zig.find_zig()
    if command is None:
        log(f"zig not found (install: {zig.INSTALL_HINT})")
        return None
    found = zig.version(command)
    if found != zig.ZIG_VERSION:
        log(f"zig {found} found, {zig.ZIG_VERSION} is the tested version")
    tree = qfetch.quake_srp()
    if tree is None:
        return None
    if qfetch.shareware(tree) is None:
        return None
    crate = assemble(tree)
    if crate is None:
        return None
    wrappers = zig.write_wrappers(command, OUT_DIR / "zig")
    env = dict(os.environ)
    env.update(zig.cargo_env(TARGET, wrappers))
    env.setdefault("CARGO_TARGET_DIR", str(CARGO_TARGET_DIR))
    cargo = ["cargo", "build", "--manifest-path", str(crate / "Cargo.toml"),
             "--target", TARGET]
    if not debug:
        cargo.append("--release")
    log("building the engine and lazyquake (zig cc, static musl)")
    built = subprocess.run(cargo, cwd=ROOT, env=env, capture_output=True, text=True)
    if built.returncode != 0:
        log("lazyquake build failed")
        print(built.stderr[-4000:], file=sys.stderr)
        raise SystemExit(1)
    output = CARGO_TARGET_DIR / TARGET / ("debug" if debug else "release") / "lazyquake"
    OUT_DIR.mkdir(parents=True, exist_ok=True)
    shutil.copyfile(output, ELF)
    return ELF


def build_package(elf: Path) -> Path | None:
    """``target/pkg/quake.lzp`` from the checked-in tree plus the built and
    fetched files, or None when the shareware pak is unavailable."""
    import build as pkg_build  # tools/pkg/build.py

    tree_root = qfetch.quake_srp()
    if tree_root is None:
        return None
    data = qfetch.shareware(tree_root)
    if data is None:
        return None
    with tempfile.TemporaryDirectory() as scratch:
        tree = Path(scratch) / "quake"
        shutil.copytree(OVERLAY / "package", tree)
        (tree / "bin").mkdir()
        shutil.copyfile(elf, tree / "bin" / "quake.elf")
        resources = tree / "resources"
        (resources / "id1").mkdir(parents=True)
        shutil.copyfile(data / "ID1" / "PAK0.PAK", resources / "id1" / "pak0.pak")
        shutil.copyfile(data / "SLICNSE.TXT", resources / "SLICNSE.TXT")
        shutil.copyfile(data / "quake106.zip", resources / "quake106.zip")
        shutil.copyfile(OVERLAY / "NOTICE", resources / "NOTICE.txt")
        shutil.copyfile(data / "SLICNSE.TXT", resources / "COPYING-shareware.txt")
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


def test_crate() -> bool:
    """``cargo test`` on the host: the upstream suite over the record
    protocol (`zsys.rs`'s run, the census) and the port's own platform
    tests, with the shareware pak beside the crate as quake-srp's CI keeps
    it. A Linux host with no zig: the OS target is never built."""
    if sys.platform not in ("linux", "linux2"):
        log("the assembled tests run on a Linux host (the OS target is not built here)")
        return False
    tree = qfetch.quake_srp()
    if tree is None:
        return False
    if qfetch.shareware(tree) is None:
        return False
    crate = assemble(tree)
    if crate is None:
        return False
    env = dict(os.environ)
    env.setdefault("CARGO_TARGET_DIR", str(OUT_DIR / "cargo-host"))
    host = subprocess.run(
        ["cargo", "test", "--manifest-path", str(crate / "Cargo.toml")],
        cwd=ROOT, env=env, capture_output=True, text=True,
    )
    if host.returncode != 0:
        log("cargo test failed")
        print(host.stdout[-4000:], file=sys.stderr)
        print(host.stderr[-4000:], file=sys.stderr)
        return False
    log("cargo test passed")
    return True


def main() -> int:
    """Parse arguments and build; return 1 only when `--require` is set
    and a wanted artifact is missing."""
    parser = argparse.ArgumentParser(
        description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter
    )
    parser.add_argument("--debug", action="store_true", help="build the debug profile")
    parser.add_argument("--engine-only", action="store_true",
                        help="skip the shareware and the package")
    parser.add_argument("--test", action="store_true",
                        help="cargo test the assembled crate on the host")
    parser.add_argument("--require", action="store_true",
                        help="fail (exit 1) when a toolchain or download is unavailable")
    args = parser.parse_args()

    if args.test:
        # The tests are their own deliverable: pass or fail, and the
        # runner's output is the report.
        ok = test_crate()
        return 1 if (not ok and args.require) else 0

    elf = build_engine(args.debug)
    built: dict[str, str] = {}
    if elf is not None:
        built["lazyquake"] = str(elf)
        if not args.engine_only:
            package = build_package(elf)
            if package is not None:
                built["package"] = str(package)
    print(json.dumps(built, indent=2))
    wanted = {"lazyquake"} if args.engine_only else {"lazyquake", "package"}
    if args.require and not wanted <= built.keys():
        log(f"unavailable: {', '.join(sorted(wanted - built.keys()))}")
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
