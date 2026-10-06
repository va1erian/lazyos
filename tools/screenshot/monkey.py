#!/usr/bin/env python3
"""Monkey: blast seeded random input at a LazyOS guest until something breaks.

The Android ``monkey`` for LazyOS. It boots the image headless under QEMU,
waits for the desktop, then fires random keyboard / mouse / drag / scroll /
chord / typing bursts over QMP for ``--duration`` seconds while watching the
serial log for crash signatures (kernel ``EXCEPTION:`` and ``LazyOS PANIC``,
``HANG:``, and ring-3 ``killed by`` reports). Long soak, intermittent bug
(memory faults that only show after a minute of use) is what it is for.

    python tools/screenshot/monkey.py --image target/lazyos.img \
        --duration 600 --seed 1 --out shots/monkey

Reproducibility: every action is drawn from ``random.Random(seed)`` and also
appended to ``actions.jsonl`` *before* it is sent, so the last line is the
input in flight when the guest died. Guest timing is not deterministic, so
the same seed is not guaranteed to fault at the same step, but
``--replay shots/monkey/run_000/actions.jsonl`` re-sends the exact recorded
sequence (``--replay-tail N`` only the last N actions), and ``--runs N`` walks
seeds ``seed .. seed+N-1`` to hunt a rare fault.

On a finding the run keeps ``shot_fault.png``, ``serial.log``, ``report.json``
(matched line, action index, elapsed time) and ``registers.txt`` (monitor
``info registers``), prints ``MONKEY: FAULT ...`` and exits 1. A clean run
prints ``MONKEY: OK`` and exits 0. The image runs with ``-snapshot`` so the
guest can never damage it.

``--accounts`` aims the profile at account surfaces and checks account invariants;
``--audit`` diffs the OS volume of a non-snapshot copy (monkey_accounts.py, monkey_audit.py).

A display that stops changing is a freeze, except when the monitor shows the
CPU in ring 0 at a port instruction (``freeze_probe``): that may be a long
device poll rather than a hang (issue #449), so the guest gets ``--io-grace``
more seconds to draw again; a recovery is noted in ``report.json`` as an
``io_stalls`` entry instead of a finding. ``--ide-disk`` boots the image from
IDE, as images did before virtio-blk, to exercise the ATA driver.
"""

from __future__ import annotations

import argparse
import collections
import hashlib
import importlib.util
import json
import os
import random
import re
import subprocess
import sys
import time
from pathlib import Path

import monkey_accounts
import monkey_audit
from freeze_probe import port_io_stall
from qemu_qmp import DEFAULT_MEMORY, Qmp, accel_args, build_qemu_command, find_qemu, free_port

DEFAULT_FATAL = [
    r"EXCEPTION:", r"LazyOS PANIC", r"HANG:", r"double fault",
    r"killed by ",  # ring-3 fault: contained, but still a bug worth catching
]

# Weighted action mix. Clicks and drags dominate because window management and
# pointer routing are where a compositor OS has the most state to corrupt.
WEIGHTS = {
    "move": 30, "click": 22, "rclick": 4, "dblclick": 6, "drag": 8,
    "scroll": 6, "key": 8, "type": 6, "chord": 5, "shell": 3, "burst": 2,
}
KEYS = ["enter", "esc", "tab", "backspace", "delete", "space", "up", "down",
        "left", "right", "home", "end", "pageup", "pagedown", "insert",
        "f1", "f2", "f3", "f4", "f5", "f10", "f11", "f12", "menu", "super"]
# Modifier chords the window manager and apps care about.
CHORDS = [("alt", "tab"), ("alt", "f4"), ("ctrl", "esc"), ("ctrl", "c"),
          ("ctrl", "a"), ("ctrl", "s"), ("ctrl", "o"), ("ctrl", "z"),
          ("ctrl", "shift"), ("alt", "esc"), ("super", "d"), ("ctrl", "alt")]
SHELL = ["ls", "ls /", "cat /system/share/samples/hello.txt", "echo $((6*7))", "ps", "pwd",
         "cd /tmp", "ls /tmp", "echo hi > /tmp/m; cat /tmp/m", "true", "false",
         "for i in 1 2 3 4 5 6 7 8; do (echo $i &); done", "cat /dev/null",
         "sleep 1", "uname -a", "env", "free", "dd if=/dev/zero of=/tmp/z bs=4k count=64",
         "rm -f /tmp/z", "mkdir /tmp/d; rmdir /tmp/d"]
PRINTABLE = "abcdefghijklmnopqrstuvwxyz0123456789 -_./;'[]=,`"


class Tail:
    """Incremental serial reader that only decodes bytes it has not seen."""

    def __init__(self, path: Path):
        self.path, self.offset, self.partial = path, 0, ""

    def new_text(self) -> str:
        try:
            with self.path.open("rb") as handle:
                handle.seek(self.offset)
                data = handle.read()
        except OSError:
            return ""
        self.offset += len(data)
        return data.decode("utf-8", errors="replace")

    def new_lines(self) -> list[str]:
        """Complete new lines only; a line still being written is held back so
        a pattern split across two reads (``EXCEPT`` + ``ION:``) still matches."""
        text = self.partial + self.new_text()
        *lines, self.partial = text.split("\n")
        return lines

    def flush(self) -> list[str]:
        """Everything still unread, including an unterminated last line (a
        guest that dies mid-line never sends the newline). Clears the fragment
        so it cannot be reported again."""
        text, self.partial = self.partial + self.new_text(), ""
        return text.splitlines()


class Monkey:
    """Draws random actions from a seeded RNG and sends them over QMP."""

    def __init__(self, qmp: Qmp, rng: random.Random):
        self.qmp, self.rng = qmp, rng

    def next_action(self) -> dict:
        kind = self.rng.choices(list(WEIGHTS), list(WEIGHTS.values()))[0]
        r = self.rng
        if kind == "move":
            # Mostly local nudges; sometimes a slam that pins a screen edge.
            span = r.choice([8, 40, 150, 600])
            return {"a": "move", "dx": r.randint(-span, span), "dy": r.randint(-span, span)}
        if kind in ("click", "rclick", "dblclick"):
            return {"a": kind}
        if kind == "drag":
            path = [[r.randint(-120, 120), r.randint(-120, 120)] for _ in range(r.randint(2, 6))]
            return {"a": "drag", "path": path}
        if kind == "scroll":
            return {"a": "scroll", "n": r.choice([-5, -2, -1, 1, 2, 5])}
        if kind == "key":
            return {"a": "key", "k": r.choice(KEYS)}
        if kind == "type":
            return {"a": "type", "s": "".join(r.choice(PRINTABLE) for _ in range(r.randint(1, 12)))}
        if kind == "chord":
            return {"a": "chord", "keys": list(r.choice(CHORDS))}
        if kind == "shell":
            return {"a": "type", "s": r.choice(SHELL) + "\n"}
        # burst: many back-to-back events with no pacing, to overrun queues
        return {"a": "burst", "n": r.randint(20, 120), "seed": r.getrandbits(32)}

    def perform(self, act: dict) -> None:
        q, kind = self.qmp, act["a"]
        if kind == "move":
            q.mouse_move(act["dx"], act["dy"])
        elif kind == "click":
            q.mouse_click("left")
        elif kind == "rclick":
            q.mouse_click("right")
        elif kind == "dblclick":
            q.mouse_click("left")
            q.mouse_click("left")
        elif kind == "drag":
            q.mouse_down("left")
            try:
                for dx, dy in act["path"]:
                    q.mouse_move(dx, dy)
                    time.sleep(0.02)
            finally:
                q.mouse_up("left")
        elif kind == "scroll":
            q.mouse_scroll(act["n"])
        elif kind == "key":
            q.press_key(act["k"])
        elif kind == "type":
            q.type_text(act["s"])
        elif kind == "chord":
            *mods, last = act["keys"]
            for mod in mods:
                q.key_down(mod)
            try:
                q.press_key(last)
            finally:
                for mod in reversed(mods):
                    q.key_up(mod)
        elif kind == "burst":
            self._burst(act["n"], act["seed"])
        else:
            raise ValueError(f"unknown action {act}")

    def _burst(self, count: int, seed: int) -> None:
        rng = random.Random(seed)
        for _ in range(count):
            if rng.random() < 0.5:
                self.qmp.mouse_move(rng.randint(-60, 60), rng.randint(-60, 60))
            elif rng.random() < 0.5:
                self.qmp.mouse_click(rng.choice(["left", "left", "right"]))
            else:
                self.qmp.press_key(rng.choice(KEYS))


def frame_hash(qmp: Qmp, out: Path) -> str:
    """Hash of the current display (PPM/PNG bytes) for liveness comparison."""
    return hashlib.md5(qmp.screenshot(out / "probe").read_bytes()).hexdigest()


def is_frozen(qmp: Qmp, out: Path, probes: int) -> bool:
    """True when the display never changes across ``probes`` mouse nudges.

    Each nudge moves the cursor to a different spot than the previous frame
    (alternating direction, so an edge clamp cannot repeat a position twice),
    and a live desktop redraws the cursor there. Identical frames after
    moving the pointer mean the guest stopped drawing.
    """
    seen = {frame_hash(qmp, out)}
    for step in range(probes):
        qmp.mouse_move(*((13, 9) if step % 2 == 0 else (-13, -9)))
        time.sleep(0.7)
        seen.add(frame_hash(qmp, out))
        if len(seen) > 1:
            return False
    return True


def monitor(qmp: Qmp):
    """A ``command -> text`` view of the human monitor."""
    return lambda command: str(qmp.execute("human-monitor-command", **{"command-line": command}))


def recovers_from_io_stall(qmp: Qmp, out: Path, grace: float) -> bool:
    """Whether a guest stalled at port I/O draws again within ``grace`` s."""
    deadline = time.time() + grace
    while time.time() < deadline:
        if not is_frozen(qmp, out, 2):
            return True
    return False


def capture_freeze(qmp: Qmp, out: Path, tail: Tail) -> str:
    """Registers, stack and an NMI-triggered HANG report for a frozen guest."""
    text = ""
    try:
        state = "\n".join(qmp.execute("human-monitor-command", **{"command-line": c})
                          for c in ("info registers", "x /256gx $rsp"))
        (out / "freeze_registers.txt").write_text(state, encoding="utf-8")
        qmp.execute("inject-nmi")
        time.sleep(2.0)
        text = tail.new_text()
        (out / "freeze_hang_report.txt").write_text(text, encoding="utf-8")
    except Exception as error:
        text = f"(freeze capture failed: {error})"
    return "\n".join(l for l in text.splitlines() if "HANG:" in l)


def dump_finding(qmp: Qmp | None, out: Path, report: dict) -> None:
    """Keep everything needed to look at the fault after QEMU is gone."""
    if qmp is not None:
        try:
            qmp.screenshot(out / "shot_fault")
        except Exception as error:
            report["screenshot_error"] = str(error)
        try:
            regs = qmp.execute("human-monitor-command", **{"command-line": "info registers"})
            (out / "registers.txt").write_text(str(regs), encoding="utf-8")
        except Exception:
            pass
    (out / "report.json").write_text(json.dumps(report, indent=2), encoding="utf-8")


def start_guest(args: argparse.Namespace, qemu: str, out: Path):
    serial = out / "serial.log"
    serial.unlink(missing_ok=True)
    port = free_port()
    extra = [*([] if args.audit else ["-snapshot"]), *args.extra_arg, *accel_args(args.accel, qemu)]
    command = build_qemu_command(qemu, str(Path(args.image).resolve()), port, serial,
                                 args.memory, extra, ide=args.ide_disk)
    proc = subprocess.Popen(command, stdout=subprocess.DEVNULL, stderr=subprocess.STDOUT)
    try:
        return proc, Qmp("127.0.0.1", port, 60), serial
    except BaseException:
        proc.kill()  # never leave an orphan QEMU behind a failed QMP connect
        proc.wait()
        raise


def wait_ready(tail: Tail, proc, marker: str, timeout: float) -> bool:
    text, deadline = "", time.time() + timeout
    while time.time() < deadline and proc.poll() is None:
        text += tail.new_text()
        if marker in text:
            return True
        time.sleep(0.25)
    return False


def build_desktop_image() -> None:
    """Build the desktop image exactly like the launcher's Desktop mode.

    ``tools/xui/build.py`` produces the app ELFs, then ``cargo build`` embeds
    them with the env ``tools/lazygui/catalog.py`` derives for Desktop, plus
    the Terminal at boot (``LAZYOS_XUI_AUTOSTART=term`` unless set). A plain
    ``cargo build`` yields a userspace-less image that never shows a desktop.
    """
    root = Path(__file__).resolve().parents[2]
    # `lazygui` is a package (catalog.py imports its siblings relatively).
    sys.path.insert(0, str(root / "tools"))
    catalog = importlib.import_module("lazygui.catalog")
    cfg = catalog.simple_config({"accel": "auto", "memory": DEFAULT_MEMORY, "qemu": "", "extra": ""},
                                "dev", "Desktop")
    # The Terminal no longer opens by default; keep it in the soak so random
    # typing reaches a focused window, as it did before.
    env = {**os.environ, **catalog.build_env(cfg)}
    env.setdefault("LAZYOS_XUI_AUTOSTART", "term")
    for argv, e in (([sys.executable, "tools/xui/build.py"], None), (["cargo", "build"], env)):
        print("MONKEY: build:", " ".join(argv), flush=True)
        if subprocess.run(argv, cwd=root, env=e).returncode != 0:
            raise SystemExit(f"build step failed: {argv}")


def load_replay(path: Path, last: int | None) -> list[dict]:
    acts = [json.loads(line)["act"] for line in path.read_text().splitlines() if line.strip()]
    return acts[-last:] if last else acts


def run_one(args: argparse.Namespace, qemu: str, seed: int, out: Path,
            replay: list[dict] | None) -> bool:
    """One boot + monkey session. Returns True when no fault was found."""
    out.mkdir(parents=True, exist_ok=True)
    try:
        proc, qmp, serial = start_guest(args, qemu, out)
    except (RuntimeError, OSError) as error:
        print(f"MONKEY: FAULT seed={seed} could not start the guest: {error}")
        return False
    tail, fatal = Tail(serial), [re.compile(p) for p in (args.fail_on or DEFAULT_FATAL)]
    ignore = [re.compile(p) for p in args.ignore]
    acct = None  # the account monkey (--accounts), set once the desktop is up
    if args.accounts:
        fatal.append(re.compile(monkey_accounts.FINDING_PREFIX))
    report: dict = {"seed": seed, "image": args.image, "found": False}
    recent: collections.deque = collections.deque(maxlen=25)
    actions = (out / "actions.jsonl").open("w", buffering=1, encoding="utf-8")

    def scan(lines: list[str], settle: bool = True) -> None:
        """Record fatal serial lines; the first one becomes the headline."""
        lines = acct.observe(lines) if acct else lines
        for line in lines:
            if any(p.search(line) for p in ignore) or not any(p.search(line) for p in fatal):
                continue
            report.setdefault("findings", []).append(line.strip())
            if not report["found"]:
                report.update(found=True, kind="serial", detail=line.strip())
                if settle:
                    time.sleep(1.5)  # let the rest of the fault report land

    try:
        if not wait_ready(tail, proc, args.marker, args.boot_timeout):
            report.update(found=True, kind="boot", detail=f"no {args.marker} in {args.boot_timeout:g}s")
            print(f"MONKEY: FAULT seed={seed} boot never reached {args.marker}")
            return False
        time.sleep(1.0)
        rng = random.Random(seed)
        monkey = Monkey(qmp, rng)
        acct = monkey_accounts.attach(args, monkey, lambda: scan(tail.new_lines()), serial)
        monkey = acct or monkey
        began, index, next_shot, counts = time.time(), 0, time.time() + args.shot_every, collections.Counter()
        next_probe = time.time() + args.probe_every
        queue = collections.deque(replay or [])
        while True:
            now = time.time()
            if replay is None and now - began >= args.duration:
                break
            if replay is not None and not queue:
                break
            if proc.poll() is not None:
                report.update(found=True, kind="qemu-exit", detail=f"QEMU exited {proc.returncode}")
                break
            act = queue.popleft() if replay is not None else monkey.next_action()
            actions.write(json.dumps({"i": index, "t": round(now - began, 2), "act": act}) + "\n")
            recent.append(act)
            counts[act["a"]] += 1
            try:
                monkey.perform(act)
            except (RuntimeError, OSError) as error:
                report.update(found=True, kind="qmp", detail=f"QMP died on {act}: {error}")
                break
            index += 1
            time.sleep(rng.random() * args.max_gap)
            scan(tail.new_lines())
            if report["found"] and not args.keep_going:
                break
            if args.probe_every > 0 and not report["found"] and now >= next_probe:
                next_probe = time.time() + args.probe_every
                if is_frozen(qmp, out, args.freeze_probes):
                    stall = port_io_stall(monitor(qmp)) if args.io_grace > 0 else None
                    if stall and recovers_from_io_stall(qmp, out, args.io_grace):
                        note = {"t": round(time.time() - began, 1), "action": index, "where": stall}
                        report.setdefault("io_stalls", []).append(note)
                        print(f"monkey seed={seed}: display stalled ({stall}), recovered", flush=True)
                        next_probe = time.time() + args.probe_every
                        continue
                    hang = capture_freeze(qmp, out, tail)
                    where = f" ({stall}, still frozen after {args.io_grace:g}s)" if stall else ""
                    report.update(found=True, kind="freeze",
                                  detail="display unchanged after mouse input" + where + "; "
                                  + (hang or "no HANG: report"))
                    break
            if now >= next_shot:
                next_shot = now + args.shot_every
                try:
                    qmp.screenshot(out / "shot_latest")
                except Exception:
                    pass
            if index % 200 == 0:
                print(f"monkey seed={seed}: {index} actions, {now - began:5.0f}s", flush=True)
        scan(tail.flush(), settle=False)  # a fault at the very end, or mid-line
        report.update(actions=index, elapsed=round(time.time() - began, 1),
                      last_actions=list(recent), counts=dict(counts))
        if acct:
            report["accounts"] = acct.summary()
        if report["found"]:
            (out / "serial_tail.txt").write_text(
                "\n".join(serial.read_text(errors="replace").splitlines()[-80:]), encoding="utf-8")
            dump_finding(qmp, out, report)
            print(f"MONKEY: FAULT seed={seed} after {index} actions / {report['elapsed']}s: "
                  f"{report.get('detail')}\nMONKEY: artifacts in {out}")
            return False
        (out / "report.json").write_text(json.dumps(report, indent=2), encoding="utf-8")
        print(f"MONKEY: OK seed={seed} actions={index} elapsed={report['elapsed']}s")
        return True
    finally:
        actions.close()
        try:
            if args.audit and proc.poll() is None:
                time.sleep(monkey_audit.FLUSH_WAIT)  # let the block cache commit before the kill
            qmp.execute("quit")
        except Exception:
            pass
        qmp.close()
        try:
            proc.wait(timeout=10)
        except subprocess.TimeoutExpired:
            proc.kill()


def main() -> int:
    p = argparse.ArgumentParser(description=__doc__,
                                formatter_class=argparse.RawDescriptionHelpFormatter)
    p.add_argument("--image", required=True, help="raw disk image to boot")
    p.add_argument("--build", action="store_true",
                   help="build the desktop image first (as the launcher's Desktop mode does)")
    p.add_argument("--duration", type=float, default=300.0, help="seconds of input per run")
    p.add_argument("--seed", type=int, default=1, help="first RNG seed")
    p.add_argument("--runs", type=int, default=1, help="sequential runs (seed, seed+1, ...)")
    p.add_argument("--max-gap", type=float, default=0.15,
                   help="max random pause between actions (lower = harsher)")
    # LazyShell drew the desktop: every desktop image prints it, while the
    # Terminal only opens when LAZYOS_XUI_AUTOSTART lists it.
    p.add_argument("--marker", default="SHELL:DESKTOP:PASS", help="serial marker meaning the desktop is up")
    p.add_argument("--boot-timeout", type=float, default=180.0)
    p.add_argument("--fail-on", action="append", default=[], metavar="REGEX",
                   help=f"serial crash pattern (repeatable; default {DEFAULT_FATAL})")
    p.add_argument("--ignore", action="append", default=[], metavar="REGEX",
                   help="serial lines matching this never count as a fault")
    p.add_argument("--keep-going", action="store_true",
                   help="do not stop at the first finding; stop only at --duration")
    p.add_argument("--probe-every", type=float, default=10.0,
                   help="seconds between freeze probes (0 disables)")
    p.add_argument("--freeze-probes", type=int, default=3,
                   help="identical frames in a row that mean the guest is frozen")
    p.add_argument("--io-grace", type=float, default=20.0,
                   help="extra seconds a frozen display gets when the CPU is in ring 0 at "
                        "port I/O, a possible long device poll (0: no grace)")
    p.add_argument("--ide-disk", action="store_true",
                   help="attach the image as IDE (the ATA driver) instead of virtio-blk")
    p.add_argument("--replay", metavar="ACTIONS.JSONL", help="re-send a recorded sequence")
    p.add_argument("--replay-tail", type=int, help="with --replay, only the last N actions")
    p.add_argument("--shot-every", type=float, default=15.0, help="seconds between shot_latest.png")
    p.add_argument("--accel", default="auto", choices=["auto", "none", "tcg", "whpx", "kvm"])
    p.add_argument("--memory", default=DEFAULT_MEMORY, help="guest RAM (default: %(default)s)")
    p.add_argument("--qemu", help="path to qemu-system-x86_64")
    p.add_argument("--extra-arg", action="append", default=[], metavar="ARG")
    p.add_argument("--out", default="shots/monkey")
    monkey_accounts.add_arguments(p)
    args = p.parse_args()

    if args.build:
        build_desktop_image()
    qemu = find_qemu(args.qemu)
    replay = load_replay(Path(args.replay), args.replay_tail) if args.replay else None
    args.accounts = args.accounts or monkey_accounts.uses_accounts(replay)  # a recorded account run
    failed = []
    for n in range(args.runs):
        seed = args.seed + n
        run_dir = Path(args.out) / f"run_{seed:03d}"
        passed = (monkey_audit.run_audited(args, qemu, seed, run_dir, replay, run_one, (start_guest, wait_ready, Tail))
                  if args.audit else run_one(args, qemu, seed, run_dir, replay))
        if not passed:
            failed.append(seed)
    print(f"MONKEY: runs={args.runs} faulted={len(failed)} seeds={failed}")
    return 1 if failed else 0


if __name__ == "__main__":
    sys.exit(main())
