#!/usr/bin/env python3
"""Build the stand-ins of the browser's sandbox attack harness (issue #832).

``tools/abi/fixtures/src/sandboxprobe.rs`` is a small static musl program that, from inside
the label of the real WebProcess or NetworkProcess, tries what an exploited engine process
would try. An image built with ``LAZYOS_WEBKIT=1 LAZYOS_WEBKIT_ATTACK=1`` ships it as both
``/system/bin/webprocess`` and ``/system/bin/networkprocess`` (``build_support/
linuxapps_embed.rs``), under the real browser window, so the kernel stamps it with the label
of the real programs; ``python tools/webkit/attack_run.py`` boots such an image and judges
what it printed.

It needs no WebKit build and no Docker: Rust's musl target (the ABI fixtures' recipe,
``tools/abi/build.py``). The output is ``target/webkit/attack/WebProcess`` and
``target/webkit/attack/NetworkProcess`` (the names the image build looks for), two copies of
one program that tells the two apart by its own name.

    python tools/webkit/attack_build.py
"""

from __future__ import annotations

import shutil
import subprocess
import sys
from pathlib import Path

HERE = Path(__file__).resolve().parent
ROOT = HERE.parent.parent
sys.path.insert(0, str(ROOT / "tools" / "abi"))
import build as abi_build  # noqa: E402

OUT = ROOT / "target" / "webkit" / "attack"
#: What `build_support/linuxapps_embed.rs` embeds, in `WEBKIT_PROCESSES` order.
PROGRAMS = ("WebProcess", "NetworkProcess")


def build() -> Path:
    """Build the probe; returns the program, or exits with the reason."""
    if not abi_build.ensure_target():
        sys.exit("the x86_64-unknown-linux-musl target is not installed (rustup target add ...)")
    command = ["cargo", "build", "--manifest-path", str(abi_build.FIXTURES / "Cargo.toml"),
               "--target", abi_build.TARGET, "--release", "--bin", "sandboxprobe"]
    result = subprocess.run(command, cwd=ROOT, env=abi_build.linker_env(),
                            capture_output=True, text=True)
    if result.returncode != 0:
        sys.exit(f"sandboxprobe did not build:\n{result.stderr[-3000:]}")
    built = abi_build.FIXTURES / "target" / abi_build.TARGET / "release" / "sandboxprobe"
    if not built.is_file():
        sys.exit(f"{built} is missing after the build")
    return built


def main() -> int:
    built = build()
    OUT.mkdir(parents=True, exist_ok=True)
    for name in PROGRAMS:
        shutil.copyfile(built, OUT / name)
    print(f"sandboxprobe: {built.stat().st_size} bytes -> {OUT} ({', '.join(PROGRAMS)})")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
