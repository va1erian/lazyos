#!/usr/bin/env python3
"""A monkey for LazyWeb: browse the way a person does for an hour, and say what went wrong.

Boots the image of the last `run.py` / `live_run.py --build`, waits for the browser's start page, then for
`--duration` seconds draws seeded random actions: type an address (the seeds, then every page it has
reached: a random walk over the web), follow the nth link by keyboard (Tab..Enter) or by a click at a random
point, scroll with the wheel in both directions (steady, and flings of hundreds of notches), page keys,
Back/Forward/Reload/Stop, drag a selection, open History and Downloads, and sit idle (a settled page must
cost nothing). While it does, the serial log is read as it grows (`webmonkey_watch.py`):

* **freeze**: the display stops changing under a moving pointer (as `tools/screenshot/monkey.py`);
* **jank**: while the wheel turns, no frame drawn for `--jank-ms` (the client's `INFO:SMOOTH gapmax`), or the page
  took `--answer-ms` to answer a wheel notch (the engine's `w2d`), or the window thread spent `--jank-ms` on one
  frame (`INFO:WINDOW max=`); checkerboard seconds are counted;
* **stuck load**: a navigation neither finished nor failed in `--load-timeout` seconds;
* **breakage**: webd lost the view, the page's process stopped answering (the hung-page bar), a process was
  killed or crashed, a lock was held for half a second, a task was stuck, WebKit lost an IPC message,
  a kernel fault; and, at the end, `live_run.verdict` over the whole log.

    python tools/lazyweb/webmonkey.py --build --duration 1800 --seed 1     # build for the real internet, then 30 minutes
    python tools/lazyweb/webmonkey.py --start arstechnica.com --duration 600   # begin on one site
    python tools/lazyweb/webmonkey.py --duration 600 --seed 7 --keep-going  # reuse the image; list every finding
    python tools/lazyweb/webmonkey.py --replay shots/webmonkey/run_001/actions.jsonl --replay-tail 30

Every action is written to `actions.jsonl` before it is sent, so the last line is the input in flight at a
finding; `--replay` sends a recorded sequence again (guest timing is not deterministic, so a seed does not
promise the same fault). A finding keeps `shot_fault.png`, `serial.log`, `serial_tail.txt`, `report.json` and, for each distinct finding
(kind and site), a reproduction bundle: `repro_NN.json` (the page, the last 40 actions with their pauses, the
commit and how the guest was run) and `issue_NN.md` (the same as text for an issue; `webmonkey_repro.py` says
what is in it). `--repro repro_NN.json` boots fresh guests and prints REPRODUCED or NOT REPRODUCED: the test of a fix;
the run prints `WEBMONKEY: FAULT ...` and exits 1; a clean one prints `WEBMONKEY: OK`. The guest runs with
`-snapshot`: the image is never touched. Needs the internet through QEMU's user network.
"""

from __future__ import annotations

import argparse
import collections
import json
import random
import re
import subprocess
import sys
import time
from pathlib import Path

HERE = Path(__file__).resolve().parent
ROOT = HERE.parent.parent
for sub in ("tools/net", "tools/screenshot", "tools/webtest", "tools/abi", "tools"):
    sys.path.insert(0, str(ROOT / sub))
sys.path.insert(0, str(HERE))

import monkey as guestmonkey  # noqa: E402  (tools/screenshot/monkey.py: the guest, the serial tail, the freeze probe)
import qemu_net  # noqa: E402
import session_pointer  # noqa: E402
import webmonkey_actions as acts  # noqa: E402
import webmonkey_repro as repros  # noqa: E402
import webmonkey_watch as watching  # noqa: E402
from qemu_qmp import find_qemu  # noqa: E402

LIVE = None  # live_run, imported when needed: it loads run.py, which wants the build tree


def live():
    global LIVE
    if LIVE is None:
        import live_run
        LIVE = live_run
    return LIVE


class Screen:
    """The page area on the screen, from the UI probe lines as they arrive."""

    def __init__(self, size: tuple[int, int]):
        self.size, self.windows, self.page = size, {}, None

    def feed(self, lines: list[str]) -> None:
        for line in lines:
            if m := session_pointer._WIDGET.search(line):  # noqa: SLF001 - one regex for both readers
                x, y, w, h, name, window = m.groups()
                if name == "page" and window.strip().endswith("LazyWeb"):
                    self.page = (window.strip(), int(x), int(y), int(w), int(h))
            elif m := session_pointer._RECT.search(line):  # noqa: SLF001
                x, y, w, h, name = m.groups()
                if name.strip().startswith("window:"):
                    self.windows[name.strip()[7:]] = (int(x), int(y))

    def region(self) -> tuple[int, int, int, int]:
        """The page area, or the middle of the screen until the probe has said."""
        if self.page and self.page[0] in self.windows:
            title, x, y, w, h = self.page
            left, top = self.windows[title]
            w = min(w, self.size[0] - (left + x) - 1)
            h = min(h, self.size[1] - (top + y) - 1)
            if w > 20 and h > 20:
                return left + x, top + y, w, h
        width, height = self.size
        return width // 8, height // 6, width * 3 // 4, height * 2 // 3


def start(args, qemu: str, out: Path):
    """Boot the guest with a tablet and the user network; (process, qmp, serial path)."""
    net = qemu_net.netdev_args([], False, None, "virtio", 1)
    tablet = [] if args.no_tablet else ["-device", "qemu-xhci,id=tabletbus", "-device", "usb-tablet,bus=tabletbus.0"]
    args.extra_arg = [*args.extra_arg, *tablet, *net]
    args.audit = False
    args.ide_disk = False
    session_pointer.POINTER.update(screen=tuple(int(n) for n in args.screen.split("x")), tablet=not args.no_tablet)
    return guestmonkey.start_guest(args, qemu, out)


def build(args) -> None:
    error = live().lazyweb_run.build_image({"LAZYOS_DISPLAY_MODE": args.screen}, False)
    if error:
        raise SystemExit(f"WEBMONKEY: FAULT build failed: {error}")


def gates(tail, proc, boot_timeout: float, log: list[str]) -> str | None:
    """None when the browser is up and showing its start page, else what never came. The markers may
    arrive in any order, so every gate is looked for in everything read so far."""
    for marker in ("PKGD:PROVISION:DONE", "LAZYWEB:UP:PASS", "NETD:RESOLV wrote", "LAZYWEB:LOAD:about:start"):
        deadline = time.time() + boot_timeout
        while not any(marker in line for line in log):
            if time.time() >= deadline or proc.poll() is not None:
                return marker
            log += tail.new_lines()
            time.sleep(0.25)
    return None


def run_one(args, qemu: str, seed: int, out: Path, replay: list[dict] | None, repro: dict | None = None) -> bool:
    """One boot and one session; True when nothing was found (with `repro`: when the bug did not come back)."""
    out.mkdir(parents=True, exist_ok=True)
    try:
        proc, qmp, serial = start(args, qemu, out)
    except (RuntimeError, OSError) as error:
        print(f"WEBMONKEY: FAULT seed={seed} could not start the guest: {error}")
        return False
    tail, rng = guestmonkey.Tail(serial), random.Random(seed)
    watch = watching.Watch(jank_ms=args.jank_ms, answer_ms=args.answer_ms)
    screen = Screen(tuple(int(n) for n in args.screen.split("x")))
    pool = acts.Pool(args.url or acts.START_URLS)
    driver = acts.Driver(qmp, screen.region)
    report: dict = {"seed": seed, "found": False, "findings": []}
    recent: collections.deque = collections.deque(maxlen=40)
    counts: collections.Counter = collections.Counter()
    actions = (out / "actions.jsonl").open("w", buffering=1, encoding="utf-8")
    index, began = 0, time.time()
    trail: collections.deque = collections.deque(maxlen=repros.TAIL)  # {act, page, wait}: what a bundle replays
    bundled: set = set()
    facts = repros.run_facts(args, ROOT, seed)

    def pump() -> None:
        lines = tail.new_lines()
        screen.feed(lines)
        for finding in watch.feed(lines, time.time()):
            note(finding)
        for m in (watching.LOAD.search(line) for line in lines):
            if m:
                pool.note(m.group(1))

    def note(finding: str) -> None:
        report["findings"].append({"t": round(time.time() - began, 1), "action": index, "finding": finding})
        print(f"webmonkey seed={seed} t={time.time() - began:5.0f}s #{index}: {finding}", flush=True)
        if not report["found"]:
            report.update(found=True, kind=repros.kind_of(finding), detail=finding)
        key = repros.key_of(finding, watch.page)
        if repro is None and key not in bundled and len(bundled) < repros.MAX_BUNDLES:
            bundled.add(key)
            excerpt = serial.read_text(errors="replace").splitlines()[-80:]
            repros.write(repros.bundle(finding, watch.page, time.time() - began, index, list(trail), facts, excerpt),
                         len(bundled), out)

    try:
        boot_log: list[str] = []
        missing = gates(tail, proc, args.boot_timeout, boot_log)
        screen.feed(boot_log)
        if missing:
            report.update(found=True, kind="boot", detail=f"never saw {missing} in {args.boot_timeout:g}s")
            print(f"WEBMONKEY: FAULT seed={seed} boot never reached {missing}")
            return False
        time.sleep(2.0)
        first = repro["replay"]["page"] if repro else args.start
        if first:
            loads = watch.loads
            driver.perform({"a": "navigate", "url": first})
            pool.note(first)
            until = time.time() + 90
            while repro and watch.loads == loads and time.time() < until:
                time.sleep(0.5)
                pump()
            time.sleep(args.start_wait)
            pump()
        began = time.time()
        if repro:
            watch.findings.clear()
            report["findings"].clear()
            replay = [step["act"] for step in repro["replay"]["steps"]]
            waits = collections.deque(step["wait"] for step in repro["replay"]["steps"])
        queue = collections.deque(replay or [])
        gap = 0.0  # the pause after the previous action: what a bundle records before the next
        next_probe = began + args.probe_every
        while True:
            now = time.time()
            if (replay is None and now - began >= args.duration) or (replay is not None and not queue):
                break
            if proc.poll() is not None:
                report.update(found=True, kind="qemu-exit", detail=f"QEMU exited {proc.returncode}")
                break
            act = queue.popleft() if replay is not None else acts.draw(rng, screen.region(), pool)
            pause = waits.popleft() if repro else 0.0
            time.sleep(pause)
            trail.append({"act": act, "page": watch.page, "wait": pause if repro else gap})
            actions.write(json.dumps({"i": index, "t": round(now - began, 2), "act": act}) + "\n")
            recent.append(act)
            counts[act["a"]] += 1
            try:
                driver.perform(act)
            except (RuntimeError, OSError) as error:
                report.update(found=True, kind="qmp", detail=f"QMP died on {act}: {error}")
                break
            index += 1
            gap = 0.0 if repro else rng.random() * args.max_gap
            time.sleep(gap)
            pump()
            if watch.quit:
                report.update(found=True, kind="quit", detail="the window closed (an action reached its close button)")
            if stuck := watch.stuck_load(time.time(), args.load_timeout):
                note(stuck)
                watch.reset_load()
                if not report["found"]:
                    report.update(found=True, kind="stuck-load", detail=stuck)
            if report["found"] and not args.keep_going:
                break
            if args.probe_every > 0 and time.time() >= next_probe:
                next_probe = time.time() + args.probe_every
                if guestmonkey.is_frozen(qmp, out, args.freeze_probes):
                    hang = guestmonkey.capture_freeze(qmp, out, tail)
                    note("freeze: the display did not change under a moving pointer; " + (hang or "no HANG: report"))
                    if not args.keep_going:
                        break
            if index % 50 == 0:
                print(f"webmonkey seed={seed}: {index} actions, {time.time() - began:5.0f}s, "
                      f"{watch.loads} loads, worst scroll gap {watch.worst_gap_ms:.0f} ms, wheel answer {watch.worst_answer_ms:.0f} ms, page {watch.page}", flush=True)
        time.sleep(args.repro_tail if repro else 1.0)  # a hang is only reported seconds after its cause
        pump()
        text = serial.read_text(errors="replace")
        for finding in live().verdict(text)[0]:
            if not any(finding[:60] in f["finding"] for f in report["findings"]):
                note("log: " + finding)
        if repro:
            hits = repros.reproduced(repro, [f["finding"] for f in report["findings"]])
            print(f"WEBMONKEY: {'REPRODUCED' if hits else 'NOT REPRODUCED'} {repro['kind']} on {repro['site']}"
                  + (f": {hits[0]}" if hits else ""))
            report.update(reproduced=bool(hits), hits=hits)
            (out / "report.json").write_text(json.dumps(report, indent=2), encoding="utf-8")
            return not hits
        report.update(actions=index, elapsed=round(time.time() - began, 1), counts=dict(counts),
                      last_actions=list(recent), watch=watching.summary(watch))
        if report["found"]:
            (out / "serial_tail.txt").write_text("\n".join(text.splitlines()[-120:]), encoding="utf-8")
            guestmonkey.dump_finding(qmp, out, report)
            print(f"WEBMONKEY: FAULT seed={seed} after {index} actions / {report['elapsed']}s: "
                  f"{report.get('detail')}\nWEBMONKEY: artifacts in {out}")
            return False
        (out / "report.json").write_text(json.dumps(report, indent=2), encoding="utf-8")
        print(f"WEBMONKEY: OK seed={seed} actions={index} elapsed={report['elapsed']}s {watching.summary(watch)}")
        return True
    finally:
        actions.close()
        try:
            qmp.execute("quit")
        except Exception:
            pass
        qmp.close()
        try:
            proc.wait(timeout=10)
        except subprocess.TimeoutExpired:
            proc.kill()


def load_replay(path: Path, last: int | None) -> list[dict]:
    rows = [json.loads(line)["act"] for line in path.read_text().splitlines() if line.strip()]
    return rows[-last:] if last else rows


def main(argv: list[str] | None = None) -> int:
    p = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    p.add_argument("--out", default="shots/webmonkey")
    p.add_argument("--build", action="store_true", help="build the image first, for the real internet (no test CA)")
    p.add_argument("--duration", type=float, default=600.0, help="seconds of input per run")
    p.add_argument("--seed", type=int, default=1)
    p.add_argument("--runs", type=int, default=1, help="sequential runs (seed, seed+1, ...)")
    p.add_argument("--url", action="append", help="an address the monkey may type (repeat); default: a dozen heavy sites")
    p.add_argument("--start", metavar="URL", help="the first page, before the random browsing (arstechnica.com is fine: https:// is assumed)")
    p.add_argument("--start-wait", type=float, default=20.0, help="seconds to let the first page load before the monkey starts")
    p.add_argument("--max-gap", type=float, default=0.8, help="longest random pause between actions (s)")
    p.add_argument("--jank-ms", type=float, default=100.0, help="one window-thread frame over this is jank")
    p.add_argument("--answer-ms", type=float, default=400.0, help="a wheel answer slower than this is jank")
    p.add_argument("--repro", metavar="REPRO.JSON", help="replay a finding's bundle (repro_NN.json): REPRODUCED (exit 1) or NOT REPRODUCED (exit 0)")
    p.add_argument("--attempts", type=int, default=3, help="with --repro, fresh boots to try (a live page and the guest's timing vary)")
    p.add_argument("--repro-tail", type=float, default=15.0, help="with --repro, seconds to watch after the last action")
    p.add_argument("--load-timeout", type=float, default=90.0, help="a load in flight this long is stuck (s)")
    p.add_argument("--keep-going", action="store_true", help="list every finding; stop only at --duration")
    p.add_argument("--probe-every", type=float, default=20.0, help="seconds between freeze probes (0: none)")
    p.add_argument("--freeze-probes", type=int, default=3)
    p.add_argument("--boot-timeout", type=float, default=300.0)
    p.add_argument("--replay", metavar="ACTIONS.JSONL")
    p.add_argument("--replay-tail", type=int)
    p.add_argument("--screen", default="1280x720", help="the display mode (also the built image's)")
    p.add_argument("--no-tablet", action="store_true", help="relative pointer from a corner (slow); for images without USB")
    p.add_argument("--accel", default="auto", choices=["auto", "none", "tcg", "whpx", "kvm"])
    p.add_argument("--memory", default=None, help="guest RAM (default: the big-app profile's)")
    p.add_argument("--qemu")
    p.add_argument("--extra-arg", action="append", default=[], metavar="ARG")
    args = p.parse_args(argv)
    if args.start and "://" not in args.start and not args.start.startswith("about:"):
        args.start = "https://" + args.start
    if args.build:
        build(args)
    live_run = live()
    args.memory = args.memory or live_run.lazyweb_run.limits.BIG_APP_MEMORY
    image = live_run.lazyweb_run.IMAGE
    if not image.is_file():
        print(f"WEBMONKEY: FAULT no image {image}: run with --build")
        return 1
    args.image = str(image)
    qemu = find_qemu(args.qemu)
    replay = load_replay(Path(args.replay), args.replay_tail) if args.replay else None
    if args.repro:
        want = repros.load(Path(args.repro))
        args.screen = want["run"]["screen"]
        args.start_wait = max(args.start_wait, 10.0)
        for attempt in range(1, args.attempts + 1):
            print(f"WEBMONKEY: repro attempt {attempt}/{args.attempts}", flush=True)
            if not run_one(args, qemu, want["run"]["seed"], Path(args.out) / f"repro_attempt_{attempt}", None, want):
                return 1
        print(f"WEBMONKEY: not reproduced in {args.attempts} attempt(s)")
        return 0
    failed = [seed for seed in range(args.seed, args.seed + args.runs)
              if not run_one(args, qemu, seed, Path(args.out) / f"run_{seed:03d}", replay)]
    print(f"WEBMONKEY: runs={args.runs} faulted={len(failed)} seeds={failed}")
    return 1 if failed else 0


if __name__ == "__main__":
    sys.exit(main())
