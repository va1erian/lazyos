#!/usr/bin/env python3
"""Build the development-run test package ``target/pkg/lrdev-test.lzp``.

A test fixture for issue #529 (``docs/lazyrad-package-plan.md`` section 3):
the LazyRAD IDE (``target/lazyrad/lazyrad.elf``, from ``tools/lazyrad/build.py``)
packaged as the user app ``org.lazy.test.lrdev`` with ``develop = true`` and
``--play-dev`` (``lazyrad-os/devtest/manifest.toml``). Installed and launched
through ``init``, it runs under its own ``app:`` label and plays the project it
is given under ``dev:<system_name>``, which is what
``tools/screenshot/examples/lazyrad_devplay.json`` checks. A ``LAZYOS_LAZYRAD=1``
image embeds the package as ``/system/share/samples/lrdev-test.lzp`` when it
exists (``build_support/lazyrad_embed.rs``).

    python tools/lazyrad/build.py && python tools/lazyrad/devtest.py

Exits 1 when the IDE has not been built or the package does not validate.
"""

from __future__ import annotations

import shutil
import sys
import tempfile
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent.parent
TREE = ROOT / "lazyrad-os" / "devtest"
IDE = ROOT / "target" / "lazyrad" / "lazyrad.elf"
OUT = ROOT / "target" / "pkg" / "lrdev-test.lzp"

sys.path.insert(0, str(ROOT / "tools" / "pkg"))
import build as pkg_build  # noqa: E402  (tools/pkg/build.py)


def main() -> int:
    if not IDE.is_file():
        print(f"error: {IDE} is missing; run `python tools/lazyrad/build.py`", file=sys.stderr)
        return 1
    with tempfile.TemporaryDirectory() as scratch:
        tree = Path(scratch) / "lrdev"
        shutil.copytree(TREE, tree)
        (tree / "bin").mkdir()
        shutil.copyfile(IDE, tree / "bin" / "lazyrad.elf")
        try:
            archive = pkg_build.build(tree, Path(scratch) / "dist")
        except pkg_build.BuildError as error:
            print(f"error: the test package does not validate:\n{error}", file=sys.stderr)
            return 1
        OUT.parent.mkdir(parents=True, exist_ok=True)
        shutil.copyfile(archive, OUT)
    print(f"{OUT} ({OUT.stat().st_size // 1024} KiB)")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
