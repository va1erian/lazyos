#!/usr/bin/env python3
"""Print from LazyWriter in a LazyOS guest to a fake printer on the host, and
judge what the printer received (docs/printing-plan.md, section 7).

1. Build a desktop image with networking and LazyWriter started at boot
   (`LAZYOS_DESKTOP=1 LAZYOS_NETD=1 LAZYOS_XUI_AUTOSTART=writer`).
2. Run the fake IPP printer (`fake_printer.py`) on 127.0.0.1:<port>; the
   guest reaches it at 10.0.2.2:<port> through QEMU's user network.
3. The session (`tools/screenshot/examples/writer_print.json`) types a line,
   opens the print bar with Ctrl+P, types the printer's address and clicks
   Print. LazyWriter hands the page to the print spooler, `printd`, which
   sends it.
4. The verdict: LazyWriter reported `WRITER:PRINT:PASS:1`; the printer got one
   Create-Job and one Send-Document of `image/pwg-raster` whose document is
   one A4 page at 300 dpi with the text inside the page's margins, no request
   cut off and no job left open. The page is saved as `<out>/page-1.png` for
   a human to look at.

`--quit` is the spooler's promise (docs/printing-plan.md P6): the session
(`writer_print_quit.json`) saves the document, prints it and quits LazyWriter
as soon as it says the job is queued, while the fake printer still reads the
page slowly. The printer must get the whole page all the same, after
LazyWriter has gone (`WRITER:QUIT:PASS` before `PRINTD:JOB:done`).

    python tools/print/run.py               # build, boot, print, judge
    python tools/print/run.py --no-build    # reuse target/lazyos.img
    python tools/print/run.py --no-build --quit

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
QUIT_SESSION = ROOT / "tools" / "screenshot" / "examples" / "writer_print_quit.json"
#: How fast the fake printer reads a document with `--quit` (bytes a second):
#: slow enough that LazyWriter quits while `printd` is still sending.
QUIT_PACE = 8000
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


def judge_quit(serial: str) -> list[str]:
    """What is wrong with the order of a `--quit` run's markers."""
    if not re.search(r"WRITER:PRINT:QUEUED:1\b", serial):
        failed = re.search(r"WRITER:PRINT:FAIL:[^\r\n]*", serial)
        return [failed.group(0) if failed else "LazyWriter never queued the job"]
    quit_at = serial.find("WRITER:QUIT:PASS")
    done = re.search(r"PRINTD:JOB:done:\d+", serial)
    errors = []
    if quit_at < 0:
        errors.append("LazyWriter never quit")
    if not done:
        ended = re.search(r"PRINTD:JOB:[a-z]+:[^\r\n]*", serial)
        errors.append(ended.group(0) if ended else "printd never finished the job")
    elif quit_at >= 0 and done.start() < quit_at:
        errors.append("the job was done before LazyWriter quit: the run proves nothing")
    if "WRITER:PRINT:PASS" in serial:
        errors.append("LazyWriter saw the job end: it did not quit while printd sent it")
    return errors


def judge(out: Path, printer: fake_printer.Printer, quit_early: bool = False) -> list[str]:
    """Everything wrong with a run whose files are under `out`."""
    errors = []
    serial = (out / "serial.log").read_text(errors="replace") if (out / "serial.log").exists() else ""
    if quit_early:
        errors += judge_quit(serial)
    elif not re.search(r"WRITER:PRINT:PASS:1\b", serial):
        failed = re.search(r"WRITER:PRINT:FAIL:[^\r\n]*", serial)
        errors.append(failed.group(0) if failed else "LazyWriter never reported WRITER:PRINT:PASS:1")
    if printer.truncated:
        errors.append(f"the printer got {printer.truncated} request(s) cut off")
    if printer.open_jobs():
        errors.append(f"the printer was left waiting on job(s) {printer.open_jobs()}")
    if not any(line.startswith("op=0x0005") for line in printer.log):
        errors.append("the job did not come as Create-Job + Send-Document")
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
    parser.add_argument("--quit", action="store_true",
                        help="quit LazyWriter as soon as the job is queued; the printer must still get it")
    args = parser.parse_args()

    if not args.no_build and not build():
        print("FAIL: the image did not build")
        return 1
    out = args.out.resolve()
    jobs = out / "jobs"
    for old in jobs.glob("job-*") if jobs.exists() else []:
        old.unlink()
    server, printer = fake_printer.serve(PORT, jobs, slow=QUIT_PACE if args.quit else 0)
    try:
        session = subprocess.run([
            PY, str(ROOT / "tools" / "screenshot" / "qemu_session.py"),
            "--image", str(IMAGE), "--net", "--out", str(out), "--accel", args.accel,
            "--timeout", "1500", "--script", str(QUIT_SESSION if args.quit else SESSION),
            "--fail-on", "WRITER:[A-Z]+:FAIL", "--fail-on", "INIT:AUTOSTART:FAIL",
            "--fail-on", "PRINTD:FAIL",
        ], cwd=ROOT)
    finally:
        server.shutdown()
    (out / "printer.log").write_text("\n".join(printer.log) + "\n")
    errors = judge(out, printer, args.quit)
    if session.returncode != 0:
        errors.insert(0, f"the session failed (exit {session.returncode})")
    for error in errors:
        print(f"FAIL: {error}")
    if not errors:
        what = "after LazyWriter quit" if args.quit else "through printd"
        print(f"PASS: one A4 page printed {what}; see {out / 'page-1.png'}")
    return 1 if errors else 0


if __name__ == "__main__":
    sys.exit(main())
