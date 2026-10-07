#!/usr/bin/env python3
"""The packages the attack harness installs as the session user (issue #623).

Both are the Counter sample (`tools/pkg/samples/counter`, the built
`target/xui/xui-counter.elf`) under another manifest, built with the same
`tools/pkg/build.py` checks the OS reader makes, into an asset tree of their
own (`manifest.txt` + `accounts/*.lzp`) that `run.py` adds to
`LAZYOS_ASSETS`, so they land in `/system/share/accounts/`:

* `autoprobe.lzp`: `org.acct.autoprobe`, `autostart = true`. A user may
  install it; the next login must open it **as that user**, never as root at
  boot (the user-to-root hole of docs/accounts-plan.md section 2).
* `corereplace.lzp`: `os.lazy.counter` 99.0.0, a higher-versioned
  replacement of a core app, which only an admin should be able to install
  (U3, brick-proofing).

    python tools/accounts/probe_packages.py OUT_DIR
"""

from __future__ import annotations

import re
import shutil
import sys
import tempfile
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent.parent
sys.path.insert(0, str(ROOT / "tools" / "pkg"))
import build as pkgbuild  # noqa: E402

SAMPLE = ROOT / "tools" / "pkg" / "samples" / "counter"
PROGRAM = ROOT / "target" / "xui" / "xui-counter.elf"
#: file name -> (system_name, version, autostart)
PACKAGES = {
    "autoprobe.lzp": ("org.acct.autoprobe", "1.0.0", True),
    "corereplace.lzp": ("os.lazy.counter", "99.0.0", False),
}
LICENCE = "GPL-3.0-or-later"


def manifest(text: str, system_name: str, version: str, autostart: bool) -> str:
    """The Counter sample's manifest with another name, version and autostart."""
    text = re.sub(r'(?m)^system_name = ".*"$', f'system_name = "{system_name}"', text)
    text = re.sub(r'(?m)^version = ".*"$', f'version = "{version}"', text)
    text = re.sub(r'(?m)^name = ".*"$', f'name = "Account probe {system_name}"', text)
    text = re.sub(r'(?m)^autostart = .*\n', "", text)
    return text.replace('abi = "linux"\n', f'abi = "linux"\nautostart = {str(autostart).lower()}\n')


def build_all(out: Path) -> list[str]:
    """Build every probe package into `out/accounts/` and write `out/manifest.txt`.
    Returns the problems (empty when every package was built)."""
    if not PROGRAM.is_file():
        return [f"missing {PROGRAM}: run tools/xui/build.py"]
    (out / "accounts").mkdir(parents=True, exist_ok=True)
    lines = ["# The attack harness's probe packages (tools/accounts/probe_packages.py)."]
    for file, (system_name, version, autostart) in PACKAGES.items():
        with tempfile.TemporaryDirectory() as scratch:
            tree = Path(scratch) / "tree"
            shutil.copytree(SAMPLE, tree)
            path = tree / "manifest.toml"
            path.write_text(manifest(path.read_text(encoding="utf-8"), system_name, version,
                                     autostart), encoding="utf-8")
            (tree / "bin").mkdir(exist_ok=True)
            shutil.copyfile(PROGRAM, tree / "bin" / "counter.elf")
            try:
                archive = pkgbuild.build(tree, Path(scratch) / "dist")
            except pkgbuild.BuildError as error:
                return [f"{file}: {error}"]
            shutil.copyfile(archive, out / "accounts" / file)
        lines.append(f"accounts/{file} | {LICENCE} | desktop | LazyOS original: {system_name} "
                     f"{version}, the Counter under another manifest")
    (out / "manifest.txt").write_text("\n".join(lines) + "\n", encoding="utf-8")
    return []


if __name__ == "__main__":
    problems = build_all(Path(sys.argv[1]))
    for problem in problems:
        print(problem)
    sys.exit(1 if problems else 0)
