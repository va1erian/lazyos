#!/usr/bin/env python3
"""Build the `lazyrad` runtime for LazyOS.

`lazyrad-os/` is a standalone Rust workspace (bins `lrplay` and `lazyrad`) built
for ``x86_64-unknown-linux-musl`` (static, ``std``): it is *not* part of the OS
workspace. The two ELFs are the contents of the core package ``os.lazy.lazyrad``
(``xui-app/packages/lazyrad``, ``bin/lazyrad.elf`` + ``bin/lrplay.elf``, the
player beside the IDE): after a full build this script repackages the core
packages (``tools/xui/core_packages.py``; skipped with a note when the xui apps
are not built yet, ``tools/xui/build.py`` packages it then). The root
``build.rs`` embeds the package as ``/system/packages/os.lazy.lazyrad.lzp``
whenever ``LAZYOS_LAZYRAD=1`` is set, ``pkgd`` installs it at boot like every
desktop app, and the build copies the sample projects named by
``LAZYRAD_SAMPLES`` under ``/system/share/lazyrad/``. Nothing of LazyRAD is in
``/system/bin`` and it is not an unlabelled exception.

The recipe mirrors ``tools/rhai/build.py`` and ``tools/xui/build.py``: on
Windows the musl target has no host linker, so cargo is pointed at the
toolchain's bundled ``rust-lld`` through target-specific variables (so the host
build scripts and proc macros keep the default linker); elsewhere the default
linker is used. The environment is set only for the cargo subprocess.

Usage::

    python tools/lazyrad/build.py
    python tools/lazyrad/build.py --bin lrplay
    python tools/lazyrad/build.py --bin all --debug

Output: ``target/lazyrad/lrplay.elf`` and/or ``target/lazyrad/lazyrad.elf``
(and, after a full build, ``target/pkg/core/os.lazy.lazyrad-<version>.lzp``),
plus a JSON map on stdout. If ``lazyrad-os/`` is not checked out the script
says so and exits non-zero. If the musl target or a linker is unavailable the
script warns and exits 0 with an empty map, so a host without them (and CI's
optional jobs) skips the run instead of failing; a real compile error exits 1.
"""

from __future__ import annotations

import argparse
import json
import os
import re
import subprocess
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent.parent
APP = ROOT / "lazyrad-os"
MANIFEST = APP / "Cargo.toml"
TARGET = "x86_64-unknown-linux-musl"
OUT_DIR = ROOT / "target" / "lazyrad"
# bin name -> 8.3 on-disk name (the kernel's FAT reader resolves short names).
BINS = {
    "lrplay": "lrplay.elf",
    "lazyrad": "lazyrad.elf",
}
ALL = "all"
#: Builds the core packages from the built programs.
CORE_PACKAGES = ROOT / "tools" / "xui" / "core_packages.py"


def run(cmd: list[str], env: dict[str, str] | None = None) -> subprocess.CompletedProcess:
    return subprocess.run(cmd, cwd=ROOT, capture_output=True, text=True, env=env)


def ensure_target() -> bool:
    """Return True if the musl target is installed (adding it if needed)."""
    try:
        installed = run(["rustup", "target", "list", "--installed"])
    except FileNotFoundError:
        print("warning: rustup not found; cannot check for the musl target", file=sys.stderr)
        return False
    if TARGET in installed.stdout:
        return True
    added = run(["rustup", "target", "add", TARGET])
    if added.returncode != 0:
        print(f"warning: cannot add {TARGET}: {added.stderr.strip()}", file=sys.stderr)
        return False
    return True


def target_env_prefix(target: str) -> str:
    """The ``CARGO_TARGET_...`` prefix for a Rust target triple."""
    return f"CARGO_TARGET_{target.upper().replace('-', '_')}"


def rust_lld_path(sysroot: str, host: str) -> Path:
    """The bundled `rust-lld` for `host` inside a rustc `sysroot`."""
    return Path(sysroot) / "lib" / "rustlib" / host / "bin" / "rust-lld.exe"


def host_triple(rustc_version: str) -> str:
    """The `host:` value from `rustc -vV` output, or ``""``."""
    return next(
        (line.split(":", 1)[1].strip() for line in rustc_version.splitlines() if line.startswith("host:")),
        "",
    )


def apply_linker_env(env: dict[str, str], linker: Path, target: str) -> dict[str, str]:
    """Point cargo at `linker` for `target`, without touching the host build.

    Target-specific variables are used (not a bare ``RUSTFLAGS``) so the host
    build scripts and proc macros keep the default linker. Existing values win,
    like the sibling scripts.
    """
    prefix = target_env_prefix(target)
    env.setdefault(f"{prefix}_LINKER", str(linker))
    env.setdefault(f"{prefix}_RUSTFLAGS", "-C linker-flavor=ld.lld")
    return env


def build_env(
    os_name: str | None = None,
    sysroot: str | None = None,
    host: str | None = None,
) -> dict[str, str]:
    """The cargo environment, with the bundled lld on hosts without a musl cc.

    `os_name`, `sysroot` and `host` are injectable so the unit tests exercise
    the Windows path without running rustc.
    """
    env = dict(os.environ)
    if os_name is None:
        os_name = os.name
    if os_name != "nt":
        return env
    if sysroot is None:
        sysroot = run(["rustc", "--print", "sysroot"]).stdout.strip()
    if host is None:
        host = host_triple(run(["rustc", "-vV"]).stdout)
    linker = rust_lld_path(sysroot, host)
    if linker.is_file():
        apply_linker_env(env, linker, TARGET)
    return env


def no_linker(stderr: str) -> bool:
    """True when cargo failed only because no linker could be run.

    Matches cargo's own diagnostic rather than any stderr that mentions a linker
    next to an unrelated "not found", so a real compile or link error still fails.
    """
    return (
        re.search(r"error: linker `[^`]+` not found", stderr) is not None
        or "could not exec the linker" in stderr.lower()
    )


def select_bins(requested: str) -> list[str]:
    """The bin names to build: both for ``all``, else the one requested."""
    return list(BINS) if requested == ALL else [requested]


def profile(debug: bool) -> str:
    """The cargo profile directory name."""
    return "debug" if debug else "release"


def source_path(profile_name: str, bin: str) -> Path:
    """Where cargo writes the `bin` artifact inside the `lazyrad-os` workspace."""
    return APP / "target" / TARGET / profile_name / bin


def dest_path(out_name: str) -> Path:
    """Where the artifact is copied for the root `build.rs` to embed."""
    return OUT_DIR / out_name


def package_core() -> None:
    """Repack the core packages so `os.lazy.lazyrad` carries the new programs.

    Best effort: the other packages need the xui apps, which may not be built
    yet; `tools/xui/build.py` (or `tools/run_demo.py`) packages it then.
    """
    result = run([sys.executable, str(CORE_PACKAGES), "--lazyrad-dir", str(OUT_DIR)])
    if result.returncode != 0:
        print("note: os.lazy.lazyrad was not packaged (build the xui apps with "
              "`python tools/xui/build.py`, which packages it)", file=sys.stderr)


def parse_args(argv: list[str] | None = None) -> argparse.Namespace:
    parser = argparse.ArgumentParser(
        description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter
    )
    parser.add_argument(
        "--bin",
        choices=[*BINS, ALL],
        default=ALL,
        help="which binary to build (default: all)",
    )
    parser.add_argument("--debug", action="store_true", help="build the debug profile")
    return parser.parse_args(argv)


def main(argv: list[str] | None = None) -> int:
    args = parse_args(argv)

    if not MANIFEST.is_file():
        print(
            f"error: {MANIFEST} not found; `lazyrad-os/` is not checked out",
            file=sys.stderr,
        )
        return 1

    built: dict[str, str] = {}
    if not ensure_target():
        print(json.dumps(built))
        return 0

    bins = select_bins(args.bin)
    profile_name = profile(args.debug)
    command = ["cargo", "build", "--manifest-path", str(MANIFEST), "--target", TARGET]
    for bin in bins:
        command += ["--bin", bin]
    if not args.debug:
        command.append("--release")
    build = run(command, env=build_env())
    if build.returncode != 0:
        if no_linker(build.stderr):
            # Rust cannot link for musl on this host (no `cc`, no rust-lld):
            # the command is optional, so report it unavailable like a
            # missing target instead of failing the whole build.
            print("warning: no linker for the musl target; lazyrad unavailable", file=sys.stderr)
            print(json.dumps(built))
            return 0
        # A real compile error must fail CI instead of silently skipping.
        print("error: lazyrad build failed", file=sys.stderr)
        print(build.stderr[-2000:], file=sys.stderr)
        print(json.dumps(built))
        return 1

    sources: list[tuple[str, Path]] = []
    for bin in bins:
        source = source_path(profile_name, bin)
        if not source.is_file():
            print(f"error: {source} was not produced", file=sys.stderr)
            print(json.dumps(built))
            return 1
        sources.append((bin, source))
    # All artifacts exist: copy them only now, so a partial build never leaves a
    # half-populated target/lazyrad/ behind.
    OUT_DIR.mkdir(parents=True, exist_ok=True)
    for bin, source in sources:
        dest = dest_path(BINS[bin])
        dest.write_bytes(source.read_bytes())
        built[bin] = str(dest)
        print(f"{bin}: {dest} ({dest.stat().st_size} bytes)", file=sys.stderr)
    if set(bins) == set(BINS):
        package_core()
    print(json.dumps(built, indent=2))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
