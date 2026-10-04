#!/usr/bin/env python3
"""Build `fetch` (also run as `curl` and `wget`) for LazyOS (docs/tls-plan.md §7).

`fetch` is the static-musl HTTPS client in ``nettls/``: rustls with the
in-tree pure-Rust provider ``nettls/crypto`` (RustCrypto crates; no ring or
aws-lc, whose licences a GPL-2.0 program cannot include; see
``tools/nettls/licenses.py``), and ``ureq`` for HTTP/1.1. It is not part of
the OS workspace. Everything is Rust, so the recipe is ``tools/rhai/build.py``'s:
the default linker, or the toolchain's ``rust-lld`` on hosts without a C
compiler. The image build embeds the result as /system/bin/fetch,
/system/bin/curl and /system/bin/wget when ``LAZYOS_TLS=1``.

SIMD and LazyOS: AES, GHASH/POLYVAL, ChaCha20, SHA-2 and curve25519 select
their backends at run time through the ``cpufeatures`` crate, whose AVX and
AVX2 checks need CPUID.1:ECX.XSAVE+OSXSAVE and then XCR0's XMM|YMM bits
(``cpufeatures`` 0.2.17 ``src/x86.rs``, ``__xgetbv!``). LazyOS saves FPU
state with FXSAVE and leaves CR4.OSXSAVE clear, so no AVX/AVX2 path is ever
chosen; AES-NI, PCLMULQDQ and SSSE3 (XMM only) remain. Nothing is compiled
for AVX statically (the baseline ``x86_64`` target). The ABI fixture
``tlsfix`` re-checks this on LazyOS.

Usage::

    python tools/nettls/build.py             # target/nettls/fetch.elf
    python tools/nettls/build.py --require   # an unavailable target or a failed build exits 1
    python tools/nettls/build.py --debug

Prints a JSON map of what it built on stdout. Without the musl target it
warns and exits 0 (unless ``--require``); a compile error always exits 1.
"""

from __future__ import annotations

import argparse
import json
import os
import shutil
import subprocess
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent.parent
CRATE = ROOT / "nettls"
TARGET = "x86_64-unknown-linux-musl"
OUT_DIR = ROOT / "target" / "nettls"
ELF = OUT_DIR / "fetch.elf"
BIN = "fetch"


def log(message: str) -> None:
    print(f"nettls: {message}", file=sys.stderr)


def run(cmd: list[str], env: dict[str, str] | None = None) -> subprocess.CompletedProcess:
    return subprocess.run(cmd, cwd=ROOT, capture_output=True, text=True, env=env)


def ensure_target() -> bool:
    """True when the musl target is installed (adding it if needed)."""
    try:
        installed = run(["rustup", "target", "list", "--installed"])
    except FileNotFoundError:
        log("rustup not found; cannot check for the musl target")
        return False
    if TARGET in installed.stdout:
        return True
    added = run(["rustup", "target", "add", TARGET])
    if added.returncode != 0:
        log(f"cannot add {TARGET}: {added.stderr.strip()}")
        return False
    return True


def build_env() -> dict[str, str]:
    """The cargo environment: the bundled lld where there is no `cc`."""
    env = dict(os.environ)
    if shutil.which("cc") is None:
        env[f"CARGO_TARGET_{TARGET.upper().replace('-', '_')}_LINKER"] = "rust-lld"
        flags = "-C linker-flavor=ld.lld -C link-self-contained=yes"
        env["RUSTFLAGS"] = (env.get("RUSTFLAGS", "") + " " + flags).strip()
    return env


def no_linker(stderr: str) -> bool:
    """True when cargo failed only because no linker could be run."""
    lowered = stderr.lower()
    return "linker" in lowered and (
        "not found" in lowered or "could not exec" in lowered or "no such file" in lowered
    )


def build(debug: bool) -> Path | None:
    """`target/nettls/fetch.elf`, or None when the toolchain is unavailable.
    A compile error is fatal: an image must never ship a stale client."""
    if not ensure_target():
        return None
    cargo = ["cargo", "build", "--manifest-path", str(CRATE / "Cargo.toml"),
             "--target", TARGET, "--locked"]
    if not debug:
        cargo.append("--release")
    log("building fetch (rustls + nettls-crypto, static musl)")
    built = run(cargo, env=build_env())
    if built.returncode != 0:
        if no_linker(built.stderr):
            log("no linker for the musl target; fetch/curl/wget unavailable")
            return None
        log("fetch build failed")
        print(built.stderr[-4000:], file=sys.stderr)
        raise SystemExit(1)
    output = CRATE / "target" / TARGET / ("debug" if debug else "release") / BIN
    OUT_DIR.mkdir(parents=True, exist_ok=True)
    shutil.copy(output, ELF)  # keeps the executable bit for host runs
    log(f"{ELF} ({ELF.stat().st_size} bytes)")
    return ELF


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__,
                                     formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--debug", action="store_true", help="build the debug profile")
    parser.add_argument("--require", action="store_true",
                        help="fail (exit 1) when the musl target or a linker is unavailable")
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
