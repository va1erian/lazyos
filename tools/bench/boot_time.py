#!/usr/bin/env python3
"""Boot-time benchmark: how long does LazyOS take to reach a desktop?

Boots a disk image headless in QEMU N times per accelerator and timestamps,
from the host clock, the moments that matter for "time to desktop":

* ``first_frame``   first framebuffer capture that is in the guest's graphics
                    mode and not black (QMP ``screendump`` polling);
* serial milestones (kernel entered, memory up, FAT mounted, tasks spawned,
  scheduler started, ``XUID:UP:PASS`` = compositor painted its first frame =
  **desktop ready**, and ``XDEMO:UP:PASS`` = the demo clients are on screen);
* any ``BOOT:PHASE:<name>:tsc=<n>`` lines the kernel prints, reported as
  guest-side cycle deltas (clock-source independent, so the numbers stay
  comparable between TCG and WHPX).

Serial is tailed by a dedicated thread so a slow screendump cannot delay a
timestamp. Frame polling has a small cost of its own (QEMU serialises the
dump against the vCPU); ``--no-frames`` measures serial milestones only.

Usage
-----
    python tools/bench/boot_time.py --image target/lazyos.img --accel whpx,none --runs 5
    python tools/bench/boot_time.py --image target/lazyos.img --runs 3 --json out.json

Build the desktop image with ``LAZYOS_XUID=1 cargo build`` first (see
``docs/perf/boot-time.md``). Standard library only.
"""

from __future__ import annotations

import argparse
import json
import re
import statistics
import subprocess
import sys
import tempfile
import threading
import time
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent.parent / "screenshot"))
from qemu_qmp import (  # noqa: E402
    Qmp, accel_args, build_qemu_command, find_qemu, free_port, resolve_accel,
)

# (milestone name, regex on a serial line, occurrence that counts). Order is
# the expected boot order; the report sorts by measured time anyway.
MILESTONES: list[tuple[str, str, int]] = [
    ("kernel_entered", r"^LazyOS: kernel entered", 1),
    ("mem_ready", r"^mem: \d+ frames usable", 1),
    ("fs_mounted", r"^LazyOS: FAT16 filesystem mounted", 1),
    ("tasks_spawned", r"^mem: live frames", 1),
    ("sched_started", r"^LazyOS: scheduler started", 1),
    ("desktop_ready", r"^XUID:UP:PASS", 1),
    ("clients_ready", r"^XDEMO:UP:PASS", 2),
]
PHASE_RE = re.compile(r"^BOOT:PHASE:([A-Za-z0-9_]+):tsc=(\d+)")


class SerialTail(threading.Thread):
    """Tail the QEMU serial log file, stamping each complete line on arrival."""

    def __init__(self, path: Path, t0: float):
        super().__init__(daemon=True)
        self.path, self.t0 = path, t0
        self.lines: list[tuple[float, str]] = []
        self._halt = threading.Event()

    def run(self) -> None:
        buf, fh = b"", None
        while not self._halt.is_set():
            if fh is None:
                try:
                    fh = open(self.path, "rb")
                except OSError:
                    time.sleep(0.002)
                    continue
            chunk = fh.read()
            if not chunk:
                time.sleep(0.002)
                continue
            now = time.perf_counter() - self.t0
            buf += chunk
            *done, buf = buf.split(b"\n")
            self.lines.extend((now, ln.decode("utf-8", "replace").rstrip("\r")) for ln in done)

    def stop(self) -> None:
        self._halt.set()


def ppm_nonblack(path: Path) -> tuple[int, int, float] | None:
    """(width, height, fraction of non-zero bytes) of a binary PPM, or None."""
    try:
        data = path.read_bytes()
    except OSError:
        return None
    m = re.match(rb"P6\s+(\d+)\s+(\d+)\s+(\d+)\s", data)
    if not m:
        return None
    pixels = data[m.end():]
    if not pixels:
        return None
    return int(m.group(1)), int(m.group(2)), 1.0 - pixels.count(0) / len(pixels)


def milestones_from(lines: list[tuple[float, str]]) -> dict[str, float]:
    found: dict[str, float] = {}
    counts = {name: 0 for name, _, _ in MILESTONES}
    for stamp, line in lines:
        for name, pattern, nth in MILESTONES:
            if name in found or not re.search(pattern, line):
                continue
            counts[name] += 1
            if counts[name] >= nth:
                found[name] = stamp
    return found


def guest_phases(lines: list[tuple[float, str]]) -> list[tuple[str, int]]:
    out = []
    for _, line in lines:
        m = PHASE_RE.match(line)
        if m:
            out.append((m.group(1), int(m.group(2))))
    return out


def run_once(qemu: str, image: Path, accel: str, args, scratch: Path) -> dict:
    serial = scratch / "serial.log"
    serial.write_text("")
    frame = scratch / "frame.ppm"
    port = free_port()
    extra = list(args.extra_arg) + accel_args(accel, qemu)
    cmd = build_qemu_command(qemu, str(image), port, serial, args.memory, extra)
    t0 = time.perf_counter()
    proc = subprocess.Popen(cmd, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    tail = SerialTail(serial, t0)
    tail.start()
    result: dict = {"ok": False, "t": {}}
    qmp = None
    try:
        qmp = Qmp("127.0.0.1", port, 30)
        result["t"]["qmp_ready"] = time.perf_counter() - t0
        need = {"desktop_ready"} | ({"clients_ready"} if args.clients else set())
        need |= set() if args.no_frames else {"first_frame"}
        result["need"] = sorted(need)
        while time.perf_counter() - t0 < args.timeout:
            result["t"].update(milestones_from(list(tail.lines)))
            if need <= result["t"].keys():
                result["ok"] = True
                break
            if not args.no_frames and "first_frame" not in result["t"]:
                try:
                    qmp.execute("screendump", filename=frame.as_posix())
                    stamp = time.perf_counter() - t0
                    info = ppm_nonblack(frame)
                    if info and info[0] == args.gfx_width and info[2] >= args.min_nonblack:
                        result["t"]["first_frame"] = stamp
                except RuntimeError:
                    pass
                time.sleep(args.frame_ms / 1000)
            else:
                time.sleep(0.005)
        # Give the tail a beat to flush lines that landed after the last poll.
        time.sleep(0.05)
        result["t"].update(milestones_from(list(tail.lines)))
    finally:
        tail.stop()
        if qmp is not None:
            try:
                qmp.execute("quit")
            except Exception:
                pass
            qmp.close()
        try:
            proc.wait(timeout=5)
        except subprocess.TimeoutExpired:
            proc.kill()
    result["phases"] = guest_phases(tail.lines)
    if not result["ok"]:
        result["serial_tail"] = [ln for _, ln in tail.lines[-8:]]
    return result


def percentile(values: list[float], pct: float) -> float:
    """Linear-interpolated percentile (pct in 0..100)."""
    ordered = sorted(values)
    if len(ordered) == 1:
        return ordered[0]
    rank = (len(ordered) - 1) * pct / 100
    lo = int(rank)
    hi = min(lo + 1, len(ordered) - 1)
    return ordered[lo] + (ordered[hi] - ordered[lo]) * (rank - lo)


def summarise(runs: list[dict]) -> dict:
    """Per-milestone median/p90/min/max plus per-phase (consecutive) deltas."""
    good = [r for r in runs if r["ok"]]
    names = ["qmp_ready", "first_frame"] + [n for n, _, _ in MILESTONES]
    summary: dict = {"runs": len(runs), "ok": len(good), "milestones": {}, "phases": {}}
    for name in names:
        vals = [r["t"][name] for r in good if name in r["t"]]
        if vals:
            summary["milestones"][name] = {
                "median": statistics.median(vals), "p90": percentile(vals, 90),
                "min": min(vals), "max": max(vals), "n": len(vals),
            }
    order = sorted(summary["milestones"], key=lambda n: summary["milestones"][n]["median"])
    for prev, cur in zip(order, order[1:]):
        deltas = [r["t"][cur] - r["t"][prev] for r in good if prev in r["t"] and cur in r["t"]]
        if deltas:
            summary["phases"][f"{prev} -> {cur}"] = {
                "median": statistics.median(deltas), "p90": percentile(deltas, 90)}
    gp: dict[str, list[float]] = {}
    for r in good:
        ph = r["phases"]
        seen: dict[str, int] = {}
        for (a, ta), (b, tb) in zip(ph, ph[1:]):
            key = f"{a} -> {b}"
            seen[key] = seen.get(key, 0) + 1
            if seen[key] > 1:  # the same phase pair twice (two xdemo clients)
                key += f" #{seen[key]}"
            gp.setdefault(key, []).append((tb - ta) / 1e6)
    summary["guest_mcycles"] = {k: {"median": statistics.median(v)} for k, v in gp.items()}
    return summary


def render(label: str, accel: str, s: dict) -> str:
    out = [f"### {label} ({accel}): {s['ok']}/{s['runs']} runs ok", "",
           "| milestone | median s | p90 s | min s | max s |", "|---|---:|---:|---:|---:|"]
    for name, m in s["milestones"].items():
        out.append(f"| {name} | {m['median']:.2f} | {m['p90']:.2f} | {m['min']:.2f} | {m['max']:.2f} |")
    out += ["", "| phase | median s | p90 s |", "|---|---:|---:|"]
    for name, p in s["phases"].items():
        out.append(f"| {name} | {p['median']:.2f} | {p['p90']:.2f} |")
    if s["guest_mcycles"]:
        out += ["", "| guest phase (TSC) | median Mcycles |", "|---|---:|"]
        out += [f"| {k} | {v['median']:.1f} |" for k, v in s["guest_mcycles"].items()]
    return "\n".join(out)


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__,
                                 formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--image", required=True, help="disk image to boot")
    ap.add_argument("--label", default=None, help="label for the report (default: image name)")
    ap.add_argument("--accel", default="auto",
                    help="comma list of accelerators: auto,none,tcg,whpx,kvm")
    ap.add_argument("--runs", type=int, default=5, help="measured iterations per accelerator")
    ap.add_argument("--warmup", type=int, default=1, help="discarded runs per accelerator")
    ap.add_argument("--qemu", help="path to qemu-system-x86_64")
    ap.add_argument("--memory", default="256M")
    ap.add_argument("--timeout", type=float, default=120.0, help="per-run timeout, seconds")
    ap.add_argument("--gfx-width", type=int, default=1280,
                    help="framebuffer width of the guest graphics mode")
    ap.add_argument("--min-nonblack", type=float, default=0.01,
                    help="non-black byte fraction that counts as a rendered frame")
    ap.add_argument("--frame-ms", type=float, default=50, help="frame poll interval")
    ap.add_argument("--no-frames", action="store_true", help="skip screendump polling")
    ap.add_argument("--clients", action=argparse.BooleanOptionalAction, default=True,
                    help="also wait for the XDEMO client markers")
    ap.add_argument("--extra-arg", action="append", default=[], metavar="ARG")
    ap.add_argument("--json", help="write raw runs + summaries here")
    args = ap.parse_args()

    image = Path(args.image).resolve()
    if not image.is_file():
        sys.exit(f"--image not found: {image}")
    qemu = find_qemu(args.qemu)
    label = args.label or image.name
    report: dict = {"label": label, "image_bytes": image.stat().st_size, "results": {}}
    print(f"# boot-time: {label} ({image.stat().st_size} bytes)\n", flush=True)
    with tempfile.TemporaryDirectory(prefix="lazyos-boot-") as tmp:
        scratch = Path(tmp)
        for requested in [a.strip() for a in args.accel.split(",") if a.strip()]:
            accel = resolve_accel(requested, qemu)
            runs = []
            for i in range(args.warmup + args.runs):
                r = run_once(qemu, image, accel, args, scratch)
                tag = "warmup" if i < args.warmup else f"run {i - args.warmup + 1}"
                d = r["t"].get("desktop_ready")
                print(f"  [{accel}] {tag}: ok={r['ok']} desktop_ready="
                      f"{'%.2fs' % d if d else 'n/a'}", file=sys.stderr, flush=True)
                if not r["ok"]:
                    print("    missing:", sorted(set(r.get("need", [])) - r["t"].keys()),
                          "serial tail:", r.get("serial_tail"), file=sys.stderr, flush=True)
                if i >= args.warmup:
                    runs.append(r)
            summary = summarise(runs)
            report["results"][accel] = {"runs": runs, "summary": summary}
            print(render(label, accel, summary), "\n", flush=True)
    if args.json:
        Path(args.json).write_text(json.dumps(report, indent=1))
    bad = any(r["summary"]["ok"] < r["summary"]["runs"] for r in report["results"].values())
    return 1 if bad else 0


if __name__ == "__main__":
    raise SystemExit(main())
