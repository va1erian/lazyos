#!/usr/bin/env python3
"""Build the sample packages the disk image ships (`COUNTER.LZP`).

A sample is a source tree under `tools/pkg/samples/<name>/` whose binary is not
checked in: this copies the built program into a scratch copy of the tree,
builds the archive with `tools/pkg/build.py` (the same checks the OS reader
makes) and writes it as an 8.3 name under `target/pkg/`, where the root
`build.rs` picks it up and embeds it in the FAT boot volume. The file is
installed from the Terminal with `pkgctl install /COUNTER.LZP`.

`tools/xui/build.py` runs this after building the xui apps (the Counter ELF is
one of them); run it by hand after a `cargo build` of `xui-app` only if you do
not use that script:

    python tools/pkg/build_samples.py [--xui-dir target/xui] [--out target/pkg]

A sample whose program has not been built is skipped with a note, so a machine
without the xui toolchain still builds the rest of the image.
"""

from __future__ import annotations

import argparse
import shutil
import sys
import tempfile
from pathlib import Path

HERE = Path(__file__).resolve().parent
ROOT = HERE.parent.parent
sys.path.insert(0, str(HERE))
import build  # noqa: E402

# name -> (the built program under --xui-dir, where it goes in the package,
#          the 8.3 name the archive is embedded as)
SAMPLES = {
    "counter": ("xui-counter.elf", "bin/counter.elf", "COUNTER.LZP"),
}


def build_sample(name: str, xui_dir: Path, out_dir: Path) -> Path | None:
    program, destination, disk_name = SAMPLES[name]
    source = xui_dir / program
    if not source.is_file():
        print(f"note: {name}: {source} is not built; skipping the sample package", file=sys.stderr)
        return None
    with tempfile.TemporaryDirectory() as scratch:
        tree = Path(scratch) / name
        shutil.copytree(HERE / "samples" / name, tree)
        target = tree / destination
        target.parent.mkdir(parents=True, exist_ok=True)
        shutil.copyfile(source, target)
        archive = build.build(tree, Path(scratch) / "dist")
        out_dir.mkdir(parents=True, exist_ok=True)
        final = out_dir / disk_name
        shutil.copyfile(archive, final)
    return final


def build_sample_all(xui_dir: Path, out_dir: Path) -> list[Path]:
    """Build every sample whose program exists; returns the archives written."""
    written = []
    for name in SAMPLES:
        result = build_sample(name, xui_dir, out_dir)
        if result is not None:
            print(f"sample package: {result}", file=sys.stderr)
            written.append(result)
    return written


def main(argv=None) -> int:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--xui-dir", type=Path, default=ROOT / "target" / "xui")
    parser.add_argument("--out", type=Path, default=ROOT / "target" / "pkg")
    args = parser.parse_args(argv)
    status = 0
    for name in SAMPLES:
        try:
            result = build_sample(name, args.xui_dir, args.out)
        except build.BuildError as error:
            print(f"error: sample {name} cannot be built:\n{error}", file=sys.stderr)
            status = 1
            continue
        if result is not None:
            print(result)
    return status


if __name__ == "__main__":
    raise SystemExit(main())
