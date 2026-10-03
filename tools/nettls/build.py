#!/usr/bin/env python3
"""Build `fetch` (also run as `curl` and `wget`) for LazyOS (docs/tls-plan.md §7).

`fetch` is the static-musl HTTPS client in ``nettls/``: rustls with the
``ring`` crypto provider, ``ureq`` for HTTP/1.1. It is not part of the OS
workspace. ``ring`` contains C and assembly, so the C compiler and the final
link are zig's (``tools/xui/zig.py``, as for Doom and the Docs app): zig ships
musl, so the same recipe works on Linux and Windows hosts without a sysroot.
The image build embeds the result as /system/bin/fetch, /system/bin/curl and
/system/bin/wget when ``LAZYOS_TLS=1``.

ring and AVX (why this binary is safe on LazyOS): LazyOS saves FPU state with
FXSAVE (x87 + SSE) and leaves CR4.OSXSAVE clear, so YMM state is not
preserved across a context switch. ring 0.17.14 decides its AVX paths at run
time: ``OPENSSL_cpuid_setup`` (``crypto/cpu_intel.c``) reads XCR0 only when
CPUID.1:ECX.OSXSAVE[bit 27] is set, and when ``(XCR0 & 6) != 6`` it clears
AVX, FMA, XOP, AVX2, VAES and VPCLMULQDQ from the CPUID words before
``cpuid_to_caps_and_set_c_flags`` (``src/cpu/intel.rs``) turns them into
capabilities. With OSXSAVE clear XCR0 reads as 0, so no AVX/AVX2/VAES code
(ChaCha20 AVX2, AES-GCM VAES+VPCLMULQDQ) is ever chosen; AES-NI, PCLMULQDQ,
SSSE3 (XMM only), ADX/BMI (general registers) remain. Nothing selects AVX at
compile time either: Rust builds for the baseline ``x86_64`` target and zig's
``-target x86_64-linux-musl`` defaults to the baseline CPU (no ``__AVX__``).
The ABI fixture ``tlsfix`` (``tools/abi/fixtures/tlsfix``) checks the same
condition on LazyOS and runs real handshakes.

Usage::

    python tools/nettls/build.py             # target/nettls/fetch.elf
    python tools/nettls/build.py --require   # a missing toolchain or a failed build exits 1
    python tools/nettls/build.py --debug

Prints a JSON map of what it built on stdout. Without zig or the musl target
it warns and exits 0 (unless ``--require``); a compile error always exits 1.
"""

from __future__ import annotations

import argparse
import json
import os
import shutil
import subprocess
import sys
from pathlib import Path

HERE = Path(__file__).resolve().parent
ROOT = HERE.parent.parent
sys.path.insert(0, str(ROOT / "tools" / "xui"))
import zig  # noqa: E402

TARGET = "x86_64-unknown-linux-musl"
CRATE = ROOT / "nettls"
OUT_DIR = ROOT / "target" / "nettls"
#: Its own cargo target directory: the zig-linked build uses a different
#: RUSTFLAGS environment from the crate's host (test) builds.
CARGO_TARGET_DIR = OUT_DIR / "cargo"
ELF = OUT_DIR / "fetch.elf"
BIN = "fetch"


def log(message: str) -> None:
    print(f"nettls: {message}", file=sys.stderr)


def ensure_target() -> bool:
    """True when the musl target is installed (adding it if needed)."""
    try:
        installed = subprocess.run(["rustup", "target", "list", "--installed"],
                                   capture_output=True, text=True)
    except FileNotFoundError:
        log("rustup not found; cannot check for the musl target")
        return False
    if TARGET in installed.stdout:
        return True
    added = subprocess.run(["rustup", "target", "add", TARGET], capture_output=True, text=True)
    if added.returncode != 0:
        log(f"cannot add {TARGET}: {added.stderr.strip()}")
        return False
    return True


def build_env(command: list[str]) -> dict[str, str]:
    """The cargo environment: zig compiles ring's C and links the program."""
    wrappers = zig.write_wrappers(command, OUT_DIR / "zig")
    env = dict(os.environ)
    env.update(zig.cargo_env(TARGET, wrappers))
    return env


def build(debug: bool) -> Path | None:
    """`target/nettls/fetch.elf`, or None when a prerequisite is missing.
    A compile error is fatal: an image must never ship a stale client."""
    if not ensure_target():
        return None
    command = zig.find_zig()
    if command is None:
        log(f"zig not found; fetch/curl/wget unavailable (install: {zig.INSTALL_HINT})")
        return None
    found = zig.version(command)
    if found != zig.ZIG_VERSION:
        log(f"zig {found} found, {zig.ZIG_VERSION} is the tested version")
    cargo = ["cargo", "build", "--manifest-path", str(CRATE / "Cargo.toml"),
             "--target", TARGET, "--target-dir", str(CARGO_TARGET_DIR), "--locked"]
    if not debug:
        cargo.append("--release")
    log("building fetch (rustls + ring, zig cc, static musl)")
    built = subprocess.run(cargo, cwd=ROOT, env=build_env(command), capture_output=True, text=True)
    if built.returncode != 0:
        log("fetch build failed")
        print(built.stderr[-4000:], file=sys.stderr)
        raise SystemExit(1)
    output = CARGO_TARGET_DIR / TARGET / ("debug" if debug else "release") / BIN
    OUT_DIR.mkdir(parents=True, exist_ok=True)
    shutil.copy(output, ELF)  # keeps the executable bit for host runs
    log(f"{ELF} ({ELF.stat().st_size} bytes)")
    return ELF


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__,
                                     formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--debug", action="store_true", help="build the debug profile")
    parser.add_argument("--require", action="store_true",
                        help="fail (exit 1) when zig or the musl target is unavailable")
    args = parser.parse_args()
    built: dict[str, str] = {}
    elf = build(args.debug)
    if elf is not None:
        built[BIN] = str(elf)
    print(json.dumps(built, indent=2))
    if args.require and elf is None:
        log("unavailable: fetch")
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
