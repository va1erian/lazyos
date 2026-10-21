#!/usr/bin/env python3
"""Build the same revision twice, in two checkouts at different paths, and
compare the programs bit for bit (docs/update-plan.md decision 24).

Package-level updates only help if an unchanged crate gives an unchanged
binary; this is the check that says so. It builds the `user` programs and the
kernel (release, `x86_64-unknown-none`) from two `git worktree` checkouts of
one revision with fresh target directories, with absolute paths remapped the
way the official build must (cargo home, the toolchain and the checkout), and
prints every program whose digest differs.

    python tools/pkg/repro_check.py                  # HEAD, user programs and kernel
    python tools/pkg/repro_check.py --rev v0.4.0 --sets kernel
    python tools/pkg/repro_check.py --no-remap       # show what the remap fixes

Exit status is 1 when any program differs. The build takes about two minutes
per checkout on a warm machine. Only the Linux container's output is ever
signed; a Windows run is a development check.
"""

from __future__ import annotations

import argparse
import hashlib
import os
import shutil
import subprocess
import sys
import tempfile
import tomllib
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
TARGET = "x86_64-unknown-none"
SETS = {
    "user": ["-p", "user", "--bins"],
    "kernel": ["-p", "kernel"],
}


def run(cmd: list[str], cwd: Path, env: dict[str, str] | None = None) -> str:
    done = subprocess.run(cmd, cwd=cwd, env=env, capture_output=True, text=True, encoding="utf-8", errors="replace")
    if done.returncode != 0:
        sys.exit(f"{' '.join(cmd)} failed in {cwd}:\n{done.stdout[-2000:]}\n{done.stderr[-3000:]}")
    return done.stdout.strip()


def config_rustflags(checkout: Path) -> list[str]:
    """The target's `rustflags` from `.cargo/config.toml`: an env var replaces
    them, so the remap has to carry them along."""
    config = tomllib.loads((checkout / ".cargo" / "config.toml").read_text(encoding="utf-8"))
    return list(config.get("target", {}).get(TARGET, {}).get("rustflags", []))


def remap_flags(checkout: Path, sysroot: Path, cargo_home: Path) -> list[str]:
    """`--remap-path-prefix` for every absolute prefix a build embeds."""
    pairs = [(checkout, "/src"), (cargo_home, "/cargo"), (sysroot, "/rustc")]
    return [f"--remap-path-prefix={src}={dst}" for src, dst in pairs]


def encoded_flags(flags: list[str]) -> str:
    return "\x1f".join(flags)


def digests(directory: Path) -> dict[str, str]:
    """Program name to SHA-256 for the extension-less files of a release dir."""
    return {
        entry.name: hashlib.sha256(entry.read_bytes()).hexdigest()
        for entry in sorted(directory.iterdir())
        if entry.is_file() and "." not in entry.name
    }


def differences(a: dict[str, str], b: dict[str, str]) -> list[str]:
    names = sorted(set(a) | set(b))
    return [name for name in names if a.get(name) != b.get(name)]


def build(checkout: Path, target_dir: Path, sets: list[str], remap: bool, env: dict[str, str]) -> dict[str, str]:
    flags = config_rustflags(checkout)
    if remap:
        flags += remap_flags(checkout, Path(env["_SYSROOT"]), Path(env["_CARGO_HOME"]))
    build_env = {k: v for k, v in env.items() if not k.startswith("_")}
    build_env["CARGO_ENCODED_RUSTFLAGS"] = encoded_flags(flags)
    build_env["CARGO_TARGET_DIR"] = str(target_dir)
    for name in sets:
        run(["cargo", "build", "--release", "--target", TARGET, *SETS[name]], checkout, build_env)
    return digests(target_dir / TARGET / "release")


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--rev", default="HEAD")
    ap.add_argument("--sets", default="user,kernel", help="comma list of: " + ", ".join(SETS))
    ap.add_argument("--no-remap", action="store_true")
    ap.add_argument("--keep", action="store_true", help="keep the scratch directory")
    args = ap.parse_args()
    sets = [s for s in args.sets.split(",") if s]
    unknown = [s for s in sets if s not in SETS]
    if unknown:
        ap.error(f"unknown set(s): {', '.join(unknown)}")

    env = dict(os.environ)
    env["_SYSROOT"] = run(["rustc", "--print", "sysroot"], ROOT)
    env["_CARGO_HOME"] = env.get("CARGO_HOME") or str(Path.home() / ".cargo")
    scratch = Path(tempfile.mkdtemp(prefix="repro-"))
    checkouts = [scratch / "a", scratch / "second-checkout-with-a-longer-name"]
    try:
        results = []
        for checkout in checkouts:
            run(["git", "worktree", "add", "--detach", "-q", str(checkout), args.rev], ROOT)
            print(f"building {args.sets} in {checkout} ...", flush=True)
            results.append(build(checkout, checkout.parent / (checkout.name + "-target"), sets, not args.no_remap, env))
        bad = differences(*results)
        total = len(set(results[0]) | set(results[1]))
        for name in bad:
            print(f"DIFFERS  {name}")
        print(f"{total - len(bad)}/{total} programs bit-identical ({'remapped' if not args.no_remap else 'no remap'})")
        return 1 if bad else 0
    finally:
        for checkout in checkouts:
            subprocess.run(["git", "worktree", "remove", "--force", str(checkout)], cwd=ROOT, capture_output=True)
        if not args.keep:
            shutil.rmtree(scratch, ignore_errors=True)


if __name__ == "__main__":
    sys.exit(main())
