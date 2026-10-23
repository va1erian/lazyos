#!/usr/bin/env python3
"""Load real sites in LazyWeb and say whether the browser hangs (docs/lazyweb.md, issue #861 style).

Boots the image of the last `run.py` or `frame_run.py` (QEMU's user network reaches the real internet),
opens each address in turn, waits, and reads the serial log for the signs of the hang a first visit to a
heavy page showed: the page's process unresponsive (webd's `WEBD:NOTE:WEBPROCESS:UNRESPONSIVE`), a view webd
lost (`WEBD:LOST`), a process busy in the
kernel for whole `PERF:CPU` periods (the NetworkProcess in ext2: `kernel=` over 40), a lock held for
seconds (`LOCK:WAIT`), a stuck call (`SCHED:STUCK`) and the cache's `ENOSYS 285 fallocate` (a finding: the HTTP
cache then writes every body inline, issue #864; any other `ENOSYS` is listed as a note).

    python tools/lazyweb/live_run.py                              # google, wikipedia, ars technica
    python tools/lazyweb/live_run.py --url https://example.org/ --wait 60
    python tools/lazyweb/live_run.py --judge-only shots/lazyweb-live/serial.log

Needs the artifacts and ports of `run.py` (the host servers are not used, but the image's test CA is).
"""

from __future__ import annotations

import argparse
import importlib.util
import re
import sys
from pathlib import Path

HERE = Path(__file__).resolve().parent
ROOT = HERE.parent.parent
for sub in ("tools/webtest", "tools/abi", "tools"):
    sys.path.insert(0, str(ROOT / sub))
sys.path.insert(0, str(HERE))

_spec = importlib.util.spec_from_file_location("lazyweb_run", HERE / "run.py")
lazyweb_run = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(lazyweb_run)

import certs  # noqa: E402
import frame_run  # noqa: E402  (its wheel steps)
import judge  # noqa: E402
import session  # noqa: E402

DEFAULT_URLS = ["https://www.google.com/", "https://en.wikipedia.org/wiki/LazyOS", "https://arstechnica.com/"]


def live_session(urls: list[str], wait: float, timeout: float, scroll: float = 0.0, caption: str = "") -> list[dict]:
    """Opens each address and watches; with `scroll`, turns the wheel for that many seconds over the last page
    (`caption` is its window title: the pointer is put in the page through it)."""
    steps = [*session.boot_gates(timeout), {"wait_for": f"{judge.P}:LOAD:{judge.START}", "timeout": timeout}]
    for index, url in enumerate(urls):
        steps += [
            *session.navigate(url, f"{judge.P}:NAV:", timeout=60),
            {"wait": wait},
            {"shot": f"site{index}"},
        ]
    if scroll > 0:
        steps += [
            {"move_to": {"window": caption, "widget": "page_origin", "offset": list(session.WHEEL_AT)}, "timeout": 60},
            {"wait": 1.0},
            *frame_run.scroll_steps(scroll),
            {"shot": "scrolled"},
            {"wait": 5.0},
        ]
    return steps


#: SIGILL, SIGABRT, SIGBUS, SIGFPE, SIGSEGV: a program that ended by one of these crashed.
CRASH_SIGNALS = (4, 6, 7, 8, 11)
#: The client, webd (the daemon that owns the engine) and its two processes.
BROWSER_PROGRAMS = ("lazyweb.elf", "webd.elf", "webprocess", "networkprocess", "WebProcess", "NetworkProcess")
#: An execve of a 100 MB browser program can hold the ext2 lock (or run in the kernel) for seconds: the known
#: start-up cost, not a hang, below this many milliseconds (the slowest seen took about 9 s).
EXECVE_MS = 15000


def verdict(text: str) -> tuple[list[str], list[str]]:
    """(findings that mean a hang, other facts) from a serial log."""
    hang, facts = [], []
    if any(line.startswith("WEBPROCESS:UNRESPONSIVE") for line in judge.engine_lines(text)):
        hang.append("the page's process was reported unresponsive (WEBPROCESS:UNRESPONSIVE)")
    for lost in judge.VIEW_LOST.finditer(text):
        hang.append(f"webd lost the window's view ({lost.group(0)})")
    for tgid, pct, user, kernel, comm in re.findall(
            r"PERF:CPU tgid=(\d+) pct=(\d+) user=(\d+) kernel=(\d+) class=\S+ demotions=\d+ comm=(\S*)", text):
        if int(kernel) >= 40:
            hang.append(f"{comm} (tgid {tgid}) spent {kernel}% of the core in the kernel for a whole period")
    # A browser program that died of a signal that means a bug (abort, segv, ...): patch 0027's UI abort
    # was exactly this, and nothing else in the verdict would have seen it.
    for tgid, sig, comm in re.findall(r"PROC:EXIT tgid=(\d+) \S+ status=\S+ sig=(\d+) \S+ \S+ \S+ comm=(\S+)", text):
        if int(sig) in CRASH_SIGNALS and comm in BROWSER_PROGRAMS:
            hang.append(f"{comm} (tgid {tgid}) died of signal {sig}")
    waits = re.findall(r"LOCK:WAIT lock=(\w+) holder=(\S+) thread=(\S+) (.*?) ms=(\d+) comm=(\S+)", text)
    for lock, holder, thread, middle, ms, comm in waits:
        execve = "held_in=linux:59 " in middle + " "
        if int(ms) >= 500 and not (execve and int(ms) < EXECVE_MS):
            hang.append(f"{comm} waited {ms} ms for {lock}, held by {holder}")
    for line in re.findall(r"SCHED:STUCK [^\n]*", text):
        ms = re.search(r" ms=(\d+)", line)
        if not ("abi=linux nr=59 " in line and ms and int(ms.group(1)) < EXECVE_MS):
            hang.append(line[:160])
    # A refused SCM_RIGHTS send: WebKit drops that message (issue #865, a page waiting for good).
    for line in re.findall(r"PROC:SCM:REFUSE [^\n]*", text):
        hang.append(line.strip())
    if "Error sending IPC message" in text:
        hang.append("WebKit lost an IPC message to a failed sendmsg (Error sending IPC message)")
    # Every unimplemented call the programs met: the first line of a (process, call), then repeat counts.
    # `fallocate` is a finding: the HTTP cache then stores every body inline (issue #864).
    for (number, name), times in sorted(judge.enosys_calls(text).items()):
        if number == 285:
            hang.append(f"fallocate answered ENOSYS {times} times: the HTTP cache wrote its bodies inline (issue #864)")
        else:
            facts.append(f"{name} ({number}) answered ENOSYS {times} times")
    return sorted(set(hang)), facts


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--out", default="shots/lazyweb-live")
    parser.add_argument("--url", action="append", help="an address to open (repeat); default: three heavy pages")
    parser.add_argument("--wait", type=float, default=45.0, help="seconds to watch after each page")
    parser.add_argument("--accel", default="auto", choices=["auto", "none", "tcg", "whpx", "kvm"])
    parser.add_argument("--qemu")
    parser.add_argument("--memory", default=lazyweb_run.limits.BIG_APP_MEMORY)
    parser.add_argument("--step-timeout", type=float, default=300.0)
    parser.add_argument("--scroll", type=float, default=0.0, help="turn the wheel this many seconds over the last page")
    parser.add_argument("--caption", default="", help="the last page's window title (`<title> - LazyWeb`), for --scroll")
    parser.add_argument("--sample-port", type=int, help="record CR3 and RIP at 30 Hz over this QMP port to <out>/samples.tsv")
    parser.add_argument("--home-disk", metavar="IMG", help="attach this /home volume (label lazyhome), as run_demo.py does; a copy is used")
    parser.add_argument("--build", action="store_true",
                        help="build the image first with no test host mappings: the real CA bundle and the real filter list")
    parser.add_argument("--mode", default="2560x1440", help="the display mode of a built image")
    parser.add_argument("--judge-only", metavar="SERIAL_LOG")
    args = parser.parse_args(argv)
    if args.judge_only:
        text = Path(args.judge_only).read_text(errors="replace")
    else:
        out = Path(args.out) if Path(args.out).is_absolute() else ROOT / args.out
        out.mkdir(parents=True, exist_ok=True)
        if args.build:
            # The harness image maps the sites' names (and easylist.to) to host stand-ins that this run does
            # not serve: a page that is meant to come from the internet times out. A plain image does not.
            error = lazyweb_run.build_image({"LAZYOS_DISPLAY_MODE": args.mode}, False)
            if error:
                print(f"{judge.P}:LIVE:FAIL {error}")
                return 1
        if not lazyweb_run.IMAGE.is_file():
            print(f"{judge.P}:LIVE:FAIL no image: build one first (python tools/lazyweb/run.py or frame_run.py)")
            return 1
        home = None
        if args.home_disk:
            import shutil
            home = str(out / "home.img")
            shutil.copyfile(args.home_disk, home)
        qemu_args: tuple[str, ...] = ()
        sampler = None
        if args.sample_port:
            import subprocess
            qemu_args = ("-qmp", f"tcp:127.0.0.1:{args.sample_port},server=on,wait=off")
            sampler = subprocess.Popen([sys.executable, str(HERE / "sample.py"), "record", "--port", str(args.sample_port),
                                        "--out", str(out / "samples.tsv"), "--seconds", "1800"], cwd=ROOT)
        try:
            ok, text = lazyweb_run.run_session(
                live_session(args.url or DEFAULT_URLS, args.wait, args.step_timeout, args.scroll, args.caption), out, args,
                qemu_args=qemu_args, home_disk=home)
        finally:
            if sampler:
                sampler.terminate()
        print(f"session {'ok' if ok else 'FAILED (it may have stopped on a marker; read the log)'}")
    hang, facts = verdict(text)
    for fact in facts:
        print(f"  note: {fact}")
    for finding in hang:
        print(f"  HANG: {finding}")
    print(f"{judge.P}:LIVE:" + ("FAIL" if hang else "PASS"))
    return 1 if hang else 0


if __name__ == "__main__":
    sys.exit(main())
