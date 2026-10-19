#!/usr/bin/env python3
"""The two probe apps the keyd app-secrets check installs (tools/keyd/run.py).

Each is `rhai` (`target/rhai/rhai.elf`) as an installed package, so it runs
under the label `app:<system_name>` the way an app does, with a manifest that
lists `os.lazy.keyd.appsecret.v1`, runs `assets/keyd/appprobe.rhai` (which
`keyd/manifest.txt` ships) and writes its one result line to
`$HOME/.apps/<system_name>/out.txt`, where `probe.sh` reads it:

* `probea.lzp`: `org.keyd.probea`;
* `probeb.lzp`: `org.keyd.probeb`, another label asking for the same name.

The packages go into an asset tree of their own (`manifest.txt` +
`keyd/*.lzp`) that `run.py` adds to `LAZYOS_ASSETS`, so they land in
`/system/share/keyd/`.

    python tools/keyd/probe_packages.py OUT_DIR
"""

from __future__ import annotations

import shutil
import sys
import tempfile
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent.parent
sys.path.insert(0, str(ROOT / "tools" / "pkg"))
import build as pkgbuild  # noqa: E402

SAMPLE = ROOT / "tools" / "pkg" / "samples" / "counter"
PROGRAM = ROOT / "target" / "rhai" / "rhai.elf"
SCRIPT = "/system/share/keyd/appprobe.rhai"
LICENCE = "GPL-3.0-or-later"
#: file name -> short id (the app is `org.keyd.<id>`)
PACKAGES = {"probea.lzp": "probea", "probeb.lzp": "probeb"}

MANIFEST = """[app]
name = "Keyd probe {short}"
system_name = "org.keyd.{short}"
author = "LazyOS"
version = "1.0.0"
description = "Test app of the keyd app-secrets check: keeps and reads one secret"

[entry]
binary = "bin/rhai.elf"
args = ["{script}", "{short}"]
abi = "linux"

[permissions]
interfaces = ["os.lazy.keyd.appsecret.v1"]
files = ["read:/system/share/keyd", "write:$HOME/.apps/org.keyd.{short}"]
"""


def manifest(short: str) -> str:
    return MANIFEST.format(short=short, script=SCRIPT)


def build_all(out: Path) -> list[str]:
    """Build every probe package into `out/keyd/` and write `out/manifest.txt`.
    Returns the problems (empty when every package was built)."""
    if not PROGRAM.is_file():
        return [f"missing {PROGRAM}: run tools/rhai/build.py"]
    (out / "keyd").mkdir(parents=True, exist_ok=True)
    lines = ["# The keyd app-secrets check's probe packages (tools/keyd/probe_packages.py)."]
    for file, short in PACKAGES.items():
        with tempfile.TemporaryDirectory() as scratch:
            tree = Path(scratch) / "tree"
            tree.mkdir()
            shutil.copytree(SAMPLE / "icons", tree / "icons")
            shutil.copytree(SAMPLE / "docs", tree / "docs")
            (tree / "manifest.toml").write_text(manifest(short), encoding="utf-8")
            (tree / "bin").mkdir()
            shutil.copyfile(PROGRAM, tree / "bin" / "rhai.elf")
            try:
                archive = pkgbuild.build(tree, Path(scratch) / "dist")
            except pkgbuild.BuildError as error:
                return [f"{file}: {error}"]
            shutil.copyfile(archive, out / "keyd" / file)
        lines.append(f"keyd/{file} | {LICENCE} | desktop | LazyOS original: org.keyd.{short}, "
                     "the rhai interpreter as a labelled test app")
    (out / "manifest.txt").write_text("\n".join(lines) + "\n", encoding="utf-8")
    return []


if __name__ == "__main__":
    problems = build_all(Path(sys.argv[1]))
    for problem in problems:
        print(problem, file=sys.stderr)
    sys.exit(1 if problems else 0)
