#!/usr/bin/env python3
"""Print from LazyWriter in a LazyOS guest to a fake printer on the host, and
judge what the printer received (docs/printing-plan.md, section 7).

1. Build a desktop image with networking and LazyWriter started at boot
   (`LAZYOS_DESKTOP=1 LAZYOS_NETD=1 LAZYOS_XUI_AUTOSTART=writer`).
2. Run the fake IPP printer (`fake_printer.py`) on 127.0.0.1:<port>; the
   guest reaches it at 10.0.2.2:<port> through QEMU's user network.
3. The session (`tools/screenshot/examples/writer_print.json`) types a line,
   opens the print bar with Ctrl+P, types the printer's address and clicks
   Print.
4. The verdict: LazyWriter reported `WRITER:PRINT:PASS:1`; the printer got one
   Print-Job for `image/pwg-raster` whose document is one A4 page at 300 dpi
   with the text inside the page's margins. The page is saved as
   `<out>/page-1.png` for a human to look at.

    python tools/print/run.py               # build, boot, print, judge
    python tools/print/run.py --no-build    # reuse target/lazyos.img

Exit status is non-zero on any failure; the serial log, screenshots and the
printer's jobs go to `shots/print`.
"""

from __future__ import annotations

import argparse
import json
import os
import re
import subprocess
import sys
from pathlib import Path

HERE = Path(__file__).resolve().parent
ROOT = HERE.parent.parent
sys.path.insert(0, str(HERE))
sys.path.insert(0, str(ROOT / "tools" / "abi"))
import busybox  # noqa: E402
import fake_printer  # noqa: E402
import pwg  # noqa: E402

PY = sys.executable
IMAGE = ROOT / "target" / "lazyos.img"
SESSION = ROOT / "tools" / "screenshot" / "examples" / "writer_print.json"
#: The address the session types: QEMU's host alias and this port.
PORT = 8631
#: A4 at 300 dpi.
A4 = (2480, 3508)
#: LazyWriter's Normal margins (25 mm) at 300 dpi, and some slack for the
#: font's side bearing and line spacing.
MARGIN = 295
SLACK = 60


def build() -> bool:
    env = dict(os.environ)
    env.update({
        "LAZYOS_DESKTOP": "1",
        "LAZYOS_NETD": "1",
        "LAZYOS_NETD_ARGS": "demo=0",
        "LAZYOS_XUI_AUTOSTART": "writer",
        "LAZYOS_RESET_OS": "1",
    })
    busybox.ensure_busybox()
    steps = ([PY, str(ROOT / "tools" / "xui" / "build.py")], ["cargo", "build"])
    return all(subprocess.run(step, cwd=ROOT, env=env).returncode == 0 for step in steps)


def judge(out: Path) -> list[str]:
    """Everything wrong with a run whose files are under `out`."""
    errors = []
    serial = (out / "serial.log").read_text(errors="replace") if (out / "serial.log").exists() else ""
    if not re.search(r"WRITER:PRINT:PASS:1\b", serial):
        failed = re.search(r"WRITER:PRINT:FAIL:[^\r\n]*", serial)
        errors.append(failed.group(0) if failed else "LazyWriter never reported WRITER:PRINT:PASS:1")
    jobs = out / "jobs"
    streams = sorted(jobs.glob("job-*.pwg")) if jobs.exists() else []
    if len(streams) != 1:
        return errors + [f"the printer got {len(streams)} jobs, not 1"]
    ticket = json.loads(streams[0].with_suffix(".json").read_text())
    if ticket.get("1.document-format") != ["image/pwg-raster"]:
        errors.append(f"document-format {ticket.get('1.document-format')}")
    if ticket.get("2.media") != ["iso_a4_210x297mm"]:
        errors.append(f"media {ticket.get('2.media')}")
    try:
        pages = pwg.decode(streams[0].read_bytes())
    except (ValueError, IndexError) as error:
        return errors + [f"the document is not PWG Raster: {error}"]
    if len(pages) != 1:
        return errors + [f"{len(pages)} pages, not 1"]
    page = pages[0]
    pwg.png(page, out / "page-1.png")
    if (page.width, page.height, page.dpi) != (*A4, 300):
        errors.append(f"page is {page.width}x{page.height} at {page.dpi} dpi, not A4 at 300")
    box = pwg.ink_box(page)
    if box is None:
        return errors + ["the page is blank"]
    left, top, right, bottom = box
    if not (MARGIN - 5 <= left <= MARGIN + SLACK and MARGIN - 5 <= top <= MARGIN + SLACK):
        errors.append(f"the text starts at ({left}, {top}), not at the 25 mm margins")
    if right > A4[0] - MARGIN or bottom > MARGIN + 200:
        errors.append(f"the text runs to ({right}, {bottom}): more than one line inside the margins")
    return errors


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--no-build", action="store_true", help="boot target/lazyos.img as it is")
    parser.add_argument("--out", type=Path, default=ROOT / "shots" / "print")
    parser.add_argument("--accel", default="auto", help="passed to qemu_session.py")
    args = parser.parse_args()

    if not args.no_build and not build():
        print("FAIL: the image did not build")
        return 1
    out = args.out.resolve()
    jobs = out / "jobs"
    for old in jobs.glob("job-*") if jobs.exists() else []:
        old.unlink()
    server, printer = fake_printer.serve(PORT, jobs)
    try:
        session = subprocess.run([
            PY, str(ROOT / "tools" / "screenshot" / "qemu_session.py"),
            "--image", str(IMAGE), "--net", "--out", str(out), "--accel", args.accel,
            "--timeout", "1500", "--script", str(SESSION),
            "--fail-on", "WRITER:[A-Z]+:FAIL", "--fail-on", "INIT:AUTOSTART:FAIL",
        ], cwd=ROOT)
    finally:
        server.shutdown()
    (out / "printer.log").write_text("\n".join(printer.log) + "\n")
    errors = judge(out)
    if session.returncode != 0:
        errors.insert(0, f"the session failed (exit {session.returncode})")
    for error in errors:
        print(f"FAIL: {error}")
    if not errors:
        print(f"PASS: one A4 page printed; see {out / 'page-1.png'}")
    return 1 if errors else 0


if __name__ == "__main__":
    sys.exit(main())
