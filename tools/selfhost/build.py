#!/usr/bin/env python3
"""Cross-compile rustc for LazyOS and stage it for the image (`LAZYOS_RUSTC=1`).

LazyOS runs Linux ``x86_64-unknown-linux-musl`` programs, so a rustc for that
host triple is a rustc for LazyOS (docs/self-hosting-plan.md). This builds one
from the pinned source release in Docker (``Dockerfile``, an Alpine build
machine; ``bootstrap.toml``: LLVM X86 only, ``rust-lld``, no debug info) and
stages ``rustc`` + ``rust-std`` as a ``/system``-shaped tree in
``target/selfhost/system``, which ``build_support/rustc_embed.rs`` places.

Usage::

    python tools/selfhost/build.py              # build (cached), then stage
    python tools/selfhost/build.py --rebuild    # run x.py again even if built
    python tools/selfhost/build.py --upstream   # stage the official dist build instead
    python tools/selfhost/build.py --cpus 12    # cap the build container

The first build takes about an hour on 24 cores (LLVM, then two compiler
stages); the source tree and build directory live in the Docker volume
``lazyos-rustc``, so a rebuild is incremental. ``--upstream`` stages the
release's own musl-host compiler (built by Rust's CI) in minutes.
"""

from __future__ import annotations

import argparse
import shutil
import subprocess
import sys
from pathlib import Path

HERE = Path(__file__).resolve().parent
ROOT = HERE.parent.parent
sys.path.insert(0, str(ROOT / "tools" / "doom"))
import fetch  # noqa: E402  (tools/doom/fetch.py: pinned, verified downloads)

VERSION = "1.99.1"
DIST = "https://static.rust-lang.org/dist/2026-10-15"
TRIPLE = "x86_64-unknown-linux-musl"
#: SHA-256 of each archive, from the release's channel-rust-stable.toml and
#: (for the source) the ``.sha256`` file published beside it.
PINS = {
    f"rustc-{VERSION}-src.tar.xz":
        "6451598a99e77f07571960f6b21d577399d3a30f33e80cbd2e3a4aae6535d265",
    f"rustc-{VERSION}-{TRIPLE}.tar.xz":
        "034e2cfea9551483c8cedbf89e52d045b89dcf860ab73aae618c89b1a87b0f18",
    f"rust-std-{VERSION}-{TRIPLE}.tar.xz":
        "1dd241eaeb19ff3b5180461c77bdee93d6cafc669eda9de834e7c06b0ec3ca95",
}
WORK = ROOT / "target" / "selfhost"
DOWNLOADS = WORK / "dl"
#: The cross build's dist tarballs.
BUILT = WORK / "out"
#: The release's own musl-host tarballs (``--upstream``).
UPSTREAM = WORK / "upstream"
STAGED = WORK / "system"
#: Which tarballs `STAGED` was made from (`BUILT` or `UPSTREAM`).
STAGED_FROM = WORK / "system.from"
IMAGE = "lazyos-rustc-build"
VOLUME = "lazyos-rustc"


def log(message: str) -> None:
    print(f"selfhost: {message}", file=sys.stderr)


def fetch_pinned(name: str) -> Path | None:
    dest = DOWNLOADS / name
    return dest if fetch.download(f"{DIST}/{name}", dest, PINS[name], log=log) else None


def dist_names() -> list[str]:
    return [f"rustc-{VERSION}-{TRIPLE}.tar.xz", f"rust-std-{VERSION}-{TRIPLE}.tar.xz"]


def docker(*args: str, mounts: dict[Path, str] = {}, cpus: int | None = None) -> bool:
    """Run ``docker run`` of the build image with `mounts` (host path -> container
    path; a path ending in ``:ro`` is read-only) and the volume at ``/b``."""
    argv = ["docker", "run", "--rm", "-v", f"{VOLUME}:/b"]
    if cpus:
        argv += ["--cpus", str(cpus)]
    for host, inside in mounts.items():
        argv += ["-v", f"{host}:{inside}"]
    return subprocess.run([*argv, IMAGE, *args]).returncode == 0


def script(name: str) -> list[str]:
    """``bash`` running ``/cfg/<name>`` with Windows line ends removed."""
    return ["bash", "-c", f'tr -d "\\r" < /cfg/{name} > /tmp/s.sh && bash /tmp/s.sh "$@"', "_"]


def build_image() -> bool:
    if shutil.which("docker") is None:
        log("docker not found")
        return False
    return subprocess.run(["docker", "build", "-q", "-t", IMAGE, str(HERE)],
                          stdout=subprocess.DEVNULL).returncode == 0


def cross_build(cpus: int | None) -> bool:
    """x.py dist rustc rust-std in the build container, into `BUILT`."""
    source = fetch_pinned(f"rustc-{VERSION}-src.tar.xz")
    if source is None:
        return False
    BUILT.mkdir(parents=True, exist_ok=True)
    log(f"building rustc {VERSION} for {TRIPLE} (about an hour the first time)")
    return docker(*script("build-in-container.sh"), VERSION, cpus=cpus, mounts={
        DOWNLOADS: "/dl:ro", HERE: "/cfg:ro", BUILT: "/out"})


def stage(source: Path) -> bool:
    """Install the dist tarballs in `source` as the `STAGED` tree (kept when it is
    newer than both, so an unchanged compiler does not rewrite the image)."""
    stamp = STAGED / "bin" / "rustc"
    newest = max((source / name).stat().st_mtime for name in dist_names())
    same = STAGED_FROM.is_file() and STAGED_FROM.read_text() == source.name
    if same and stamp.is_file() and stamp.stat().st_mtime > newest:
        log(f"{STAGED} is current")
        return True
    WORK.mkdir(parents=True, exist_ok=True)
    tree = WORK / "stage-tmp"
    shutil.rmtree(tree, ignore_errors=True)
    tree.mkdir()
    if not docker(*script("stage-in-container.sh"),
                  mounts={source: "/in:ro", HERE: "/cfg:ro", tree: "/out"}):
        return False
    shutil.rmtree(STAGED, ignore_errors=True)
    (tree / "system").rename(STAGED)
    tree.rmdir()
    STAGED_FROM.write_text(source.name)
    log(f"staged {STAGED}")
    return True


def upstream_tarballs() -> bool:
    UPSTREAM.mkdir(parents=True, exist_ok=True)
    for name in dist_names():
        if fetch_pinned(name) is None:
            return False
        shutil.copyfile(DOWNLOADS / name, UPSTREAM / name)
    return True


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__,
                                     formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--upstream", action="store_true",
                        help="stage the release's own musl-host compiler, build nothing")
    parser.add_argument("--rebuild", action="store_true",
                        help="run the cross build even when its tarballs exist")
    parser.add_argument("--cpus", type=int, help="CPU cap of the build container")
    parser.add_argument("--require", action="store_true",
                        help="exit 1 when Docker or a download is unavailable")
    args = parser.parse_args()
    if not build_image():
        log("no Docker: the compiler was not built")
        return 1 if args.require else 0
    if args.upstream:
        ok = upstream_tarballs() and stage(UPSTREAM)
    else:
        built = all((BUILT / name).is_file() for name in dist_names())
        ok = (built and not args.rebuild) or cross_build(args.cpus)
        ok = ok and stage(BUILT)
    if not ok:
        log("failed")
        return 1
    print(STAGED)
    return 0


if __name__ == "__main__":
    sys.exit(main())
