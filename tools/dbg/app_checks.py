"""App swapping checks for `tools/dbg/run.py` (docs/dbgd-plan.md, v2):
install an uploaded package through `pkgd`, upgrade it, the refusals, and
`app.relaunch`.

The package is `tools/pkg/samples/counter` built by `tools/pkg/build.py`
with any ELF as its binary (the image's `sysmond`): a console image has no
compositor to run it, so `app.relaunch` finds no instance here (0/0).
"""

from __future__ import annotations

import shutil
import subprocess
import sys
from pathlib import Path

import hotreload
from dbgclient import DbgClient

ROOT = Path(__file__).resolve().parents[2]
SAMPLE = ROOT / "tools" / "pkg" / "samples" / "counter"
SYSTEM_NAME = "org.lazy.counter"
DENIED, BAD_PARAMS = -32002, -32602


def build_package(scratch: Path, elf: bytes, note: str) -> bytes:
    """The sample package with `elf` as its binary and `note` in its docs
    (so two builds differ)."""
    tree = scratch / "counter"
    shutil.rmtree(tree, ignore_errors=True)
    shutil.copytree(SAMPLE, tree)
    (tree / "bin").mkdir(exist_ok=True)
    (tree / "bin" / "counter.elf").write_bytes(elf)
    readme = tree / "docs" / "README.md"
    readme.write_text(readme.read_text(encoding="utf-8") + f"\n{note}\n", encoding="utf-8")
    out = scratch / "dist"
    shutil.rmtree(out, ignore_errors=True)
    subprocess.run([sys.executable, str(ROOT / "tools" / "pkg" / "build.py"), str(tree),
                    "--out", str(out)], cwd=ROOT, check=True, capture_output=True)
    return next(out.glob("*.lzp")).read_bytes()


def exercise(key: str, c, host: str, port: int, image: Path, scratch: Path) -> None:
    elf = hotreload.image_binary(image, "sysmond")
    first = build_package(scratch, elf, "build one")
    second = build_package(scratch, elf, "build two")
    with DbgClient(host, port, key) as dbg:
        call = dbg.call
        c.raises("app.upload needs control.begin", DENIED,
                 lambda: call("app.upload", offset=0, total=4, data="AAAA"))
        hotreload.begin(dbg)
        c.raises("app.install without an upload is refused", BAD_PARAMS,
                 lambda: call("app.install", sha256="00" * 32))
        hotreload.upload(dbg, None, first)
        c.raises("pkgd refuses a package whose digest differs", DENIED,
                 lambda: call("app.install", sha256="00" * 32))

        installed = hotreload.install_app(dbg, first)
        c.check("app.install installs the uploaded package",
                installed.get("system_name") == SYSTEM_NAME, str(installed))
        c.check("an app that is not running is not relaunched",
                installed.get("stopped") == 0 and installed.get("started") == 0, str(installed))
        c.raises("the same package twice is refused by pkgd", DENIED,
                 lambda: hotreload.install_app(dbg, first))
        upgraded = hotreload.install_app(dbg, second, relaunch=False)
        c.check("a new build of the app replaces it",
                upgraded.get("system_name") == SYSTEM_NAME and "stopped" not in upgraded,
                str(upgraded))

        relaunched = call("app.relaunch", app=SYSTEM_NAME)
        c.check("app.relaunch of an idle app answers 0/0",
                relaunched == {"app": SYSTEM_NAME, "stopped": 0, "started": 0}, str(relaunched))
        c.raises("app.relaunch refuses a malformed id", BAD_PARAMS,
                 lambda: call("app.relaunch", app="../init"))

        log = call("log.tail", lines=2000, source="programs")["lines"]
        passes = [r for r in log if r.get("tag") == "PKGD:INSTALL:PASS"
                  and SYSTEM_NAME in r.get("text", "")]
        c.check("pkgd logs both installs", len(passes) >= 2, str(passes[-2:]))
