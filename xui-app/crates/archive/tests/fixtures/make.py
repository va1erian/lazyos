#!/usr/bin/env python3
"""Regenerate the interoperability fixtures with the reference tools.

The archives here were made by 7-Zip (7z.exe), bsdtar (pax), gzip and xz, not by
lazyarc, so the tests prove the library reads what other tools write:

    python make.py [--7z "C:/Program Files/7-Zip/7z.exe"]

Every archive holds the same small tree (`tree/hello.txt`, `tree/empty.txt`,
`tree/sub/numbers.txt`).
"""

import argparse
import os
import shutil
import subprocess
import tempfile
from pathlib import Path

HERE = Path(__file__).resolve().parent


def make_tree(root: Path) -> None:
    (root / "tree" / "sub").mkdir(parents=True)
    (root / "tree" / "hello.txt").write_bytes(b"Hello from 7-Zip!\n")
    (root / "tree" / "empty.txt").write_bytes(b"")
    numbers = "".join(f"number {i}\n" for i in range(4000)).encode()
    (root / "tree" / "sub" / "numbers.txt").write_bytes(numbers)


def run(*args: str, cwd: Path) -> None:
    subprocess.run(args, cwd=cwd, check=True, stdout=subprocess.DEVNULL)


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--7z", dest="sevenz", default=shutil.which("7z") or "C:/Program Files/7-Zip/7z.exe")
    args = parser.parse_args()
    with tempfile.TemporaryDirectory() as scratch:
        root = Path(scratch)
        make_tree(root)
        out = lambda name: str(HERE / name)  # noqa: E731
        for name in ["lzma2.7z", "lzma.7z", "copy.7z", "deflate.7z", "aes.7z", "7zip.zip", "deflate64.zip"]:
            (HERE / name).unlink(missing_ok=True)
        run(args.sevenz, "a", "-t7z", out("lzma2.7z"), "tree", cwd=root)
        run(args.sevenz, "a", "-t7z", "-m0=LZMA", "-mhc=off", out("lzma.7z"), "tree", cwd=root)
        run(args.sevenz, "a", "-t7z", "-m0=Copy", out("copy.7z"), "tree", cwd=root)
        run(args.sevenz, "a", "-t7z", "-m0=Deflate", "-ms=off", out("deflate.7z"), "tree", cwd=root)
        run(args.sevenz, "a", "-t7z", "-psecret", "-mhe=off", out("aes.7z"), "tree", cwd=root)
        run(args.sevenz, "a", "-tzip", out("7zip.zip"), "tree", cwd=root)
        run(args.sevenz, "a", "-tzip", "-mm=Deflate64", out("deflate64.zip"), "tree", cwd=root)
        env = dict(os.environ, GZIP="-n")
        run("tar", "--format=pax", "-cf", "pax.tar", "tree", cwd=root)
        shutil.copy(root / "pax.tar", HERE / "pax.tar")
        subprocess.run(["gzip", "-n", "-9", "-c", "pax.tar"], cwd=root, check=True,
                       stdout=open(HERE / "pax.tar.gz", "wb"), env=env)
        subprocess.run(["xz", "-c", "pax.tar"], cwd=root, check=True, stdout=open(HERE / "pax.tar.xz", "wb"))
        subprocess.run(["xz", "-c", "tree/hello.txt"], cwd=root, check=True, stdout=open(HERE / "hello.txt.xz", "wb"))
        subprocess.run(["gzip", "-9", "-c", "tree/sub/numbers.txt"], cwd=root, check=True,
                       stdout=open(HERE / "numbers.txt.gz", "wb"))


if __name__ == "__main__":
    main()
