#!/usr/bin/env python3
"""webd's light views end to end (docs/webd-plan.md, P2 slice 2): build the desktop image with `webd` and
the demo client, boot it once, and judge what the guest, the host's listener and the screen saw.

Needs `webd` built first (its recipe is `python tools/run_demo.py --webd`, which builds what it
needs); this harness calls it. The image is a desktop with the Terminal and the demo
(`LAZYOS_XUI_AUTOSTART=webddemo,term`), the UI probe (the session clicks the page's link by name), the
label trace (every refused call is printed as `LABEL:DENY`) and networking (so that a light view that
did load its picture could reach the host: the picture is `http://10.0.2.2:18080/blocked.png`).

1. the host listens on 127.0.0.1:18080 (`listener.py`) and counts every connection and request;
2. the guest boots; `session.py` waits for webd and the demo, for the page's first frame, clicks the
   link, types in the form, then (unless `--skip-loss`) kills the page's web process from the Terminal
   and waits for the demo to lose the view, make a new one and show the page again, and quits the demo;
3. `judge.py` judges the serial log (webd's and the demo's markers, the label trace), the listener (it
   must have seen **nothing**) and the screenshots (the demo window shows the page's blue).

    python tools/webd/run.py                       # build, boot, judge
    python tools/webd/run.py --no-build            # the image of the last run
    python tools/webd/run.py --skip-loss           # leave out the kill (no way to end a process from the guest)
    python tools/webd/run.py --no-label-trace      # build without LAZYOS_LABEL_TRACE=1
    python tools/webd/run.py --judge-only DIR      # judge a saved run (DIR/serial.log, requests.json, shot_*.png)

The listener needs port 18080 on 127.0.0.1; it takes none of the web harnesses' ports (80, 81, 443), so
it can run beside them. Exit status is non-zero on any failure. Logs, screenshots and the listener's
record go to `shots/webd`; read `shot_w01_page.png` (the page), `w02_link`, `w03_form` and
`w04_recreated`. Every run recreates `target/lazyos.img`'s OS volume.
"""

from __future__ import annotations

import argparse
import json
import os
import subprocess
import sys
from pathlib import Path

HERE = Path(__file__).resolve().parent
ROOT = HERE.parent.parent
for sub in ("tools/abi", "tools"):
    sys.path.append(str(ROOT / sub))
sys.path.insert(0, str(HERE))
import judge as j  # noqa: E402  (this directory's)
import listener  # noqa: E402
import session  # noqa: E402
from lazygui import limits  # noqa: E402

PY = sys.executable
IMAGE = ROOT / "target" / "lazyos.img"
RUN_DEMO = ROOT / "tools" / "run_demo.py"
PREFIX = "WEBD"


def report(section: str, detail: str, problems: list[str]) -> bool:
    for problem in problems[:16]:
        print(f"{PREFIX}:{section}:FAIL {problem}")
    if not problems:
        print(f"{PREFIX}:{section}:PASS {detail}".rstrip())
    return not problems


def build_image(args) -> str | None:
    """Build the image through `run_demo.py --webd`; an error message, or None."""
    import busybox  # noqa: PLC0415  (tools/abi: only a build needs it)

    shell = busybox.ensure_busybox()
    if shell is None:
        return "no BusyBox for the Terminal's shell (see tools/abi/busybox.py)"
    # A hermetic build: none of the caller's switches, then ours.
    env = {k: v for k, v in os.environ.items() if not k.startswith("LAZYOS_")}
    env.update(LAZYOS_XUI_AUTOSTART="webddemo,term", LAZYOS_UI_PROBE="1", LAZYOS_BUSYBOX=str(shell))
    if args.label_trace:
        env["LAZYOS_LABEL_TRACE"] = "1"
    command = [PY, str(RUN_DEMO), "--webd", "--desktop", "--net", "--big-app", "--reset-os", "--yes", "--autologin", "user", "--build-only"]
    print(f"webd: {' '.join(command[1:])} (LAZYOS_XUI_AUTOSTART=webddemo,term, UI probe"
          f"{', label trace' if args.label_trace else ''})", flush=True)
    done = subprocess.run(command, cwd=ROOT, env=env, capture_output=True, text=True)
    if done.returncode != 0:
        return "run_demo.py --webd failed:\n" + (done.stdout + done.stderr)[-4000:]
    return None if IMAGE.is_file() else f"{IMAGE} was not built"


def run_session(steps: list[dict], out: Path, args) -> tuple[bool, str]:
    """Drive the guest; (session ok, serial log text)."""
    out.mkdir(parents=True, exist_ok=True)
    for stale in ["serial.log", "net.pcap", "summary.json", *(p.name for p in out.glob("shot_*.png"))]:
        (out / stale).unlink(missing_ok=True)
    script = out / "session.json"
    script.write_text(session.render(steps), encoding="utf-8")
    command = [PY, str(ROOT / "tools" / "screenshot" / "qemu_session.py"), "--image", str(IMAGE),
               "--out", str(out), "--script", str(script), "--accel", args.accel, "--memory", args.memory,
               "--net", "--net-forward", "none", "--net-pcap", str(out / "net.pcap"),
               "--fail-on", "PANIC", "--fail-on", "EXCEPTION",
               "--fail-on", rf"{j.D}:(BIND|RUN):FAIL", "--fail-on", rf"{j.D}:[A-Z]+:FAIL",
               "--fail-on", rf"{j.W}:(?!NOTE:)[A-Z_]+(:[A-Z_]+)*:FAIL"]
    if args.qemu:
        command += ["--qemu", args.qemu]
    code = subprocess.call(command, cwd=ROOT, stdout=subprocess.DEVNULL)
    log = out / "serial.log"
    return code == 0, log.read_text(errors="replace") if log.is_file() else ""


def verdict(text: str, record: dict, out: Path, ran: bool, loss: bool) -> int:
    """Judge a run. `record`: the listener's; `out`: where the screenshots are."""
    shots = {name: out / f"shot_{name}.png" for name in ("w01_page", "w04_recreated")}
    region = j.window_region(text)
    ok = report("SESSION", "the boot ran to its end",
                [] if ran else ["the session did not finish (see summary.json and serial.log)"])
    ok = report("SERVICE", "webd is up and serves app.os.lazy.webd.light", j.judge_webd(text)) and ok
    ok = report("VIEWS", "every view the demo has was created by webd for the demo", j.judge_views(text)) and ok
    ok = report("PAGE", "the first non-blank frame is the blue page, with its latency and title",
                j.judge_page(text)) and ok
    ok = report("LINK", "the click on the link came back as LinkClicked and loaded nothing",
                j.judge_link(text)) and ok
    ok = report("NETWORK", f"nothing reached the listener on port {j.BLOCKED_PORT}: "
                f"{record.get('connections', 0)} connection(s), {len(record.get('requests', []))} request(s)",
                j.judge_requests(record, control=loss)) and ok
    ok = report("SHOT", f"the demo window {region} shows the page's blue",
                j.judge_shot(shots["w01_page"], region, "page")) and ok
    if loss:
        ok = report("LOSS", "the killed web process lost the view; the demo made a new one and showed the page",
                    j.judge_loss(text, True)) and ok
        ok = report("RECREATED", "the demo window shows the page's blue again",
                    j.judge_shot(shots["w04_recreated"], region, "recreated page")) and ok
    else:
        ok = report("LOSS", "no loss (skipped)", j.judge_loss(text, False)) and ok
    ok = report("QUIT", "the demo quit and webd forgot every view", j.judge_quit(text)) and ok
    ok = report("FAILURES", "no FAIL marker from webd or the demo", j.judge_failures(text)) and ok
    ok = report("LABEL", "no refused call of the demo or webd beyond the documented optional ones",
                j.judge_denies(text)) and ok
    for line in j.denies(text)[:40]:
        print(f"{PREFIX}:INFO:LABEL:DENY {line}")
    frames, latencies = j.FRAME.findall(text), j.LATENCY.findall(text)
    print(f"{PREFIX}:INFO:LATENCY first page after set_html (ms): {latencies[:1]}; first frame {frames[:1]}")
    print(f"{PREFIX}:HARNESS:" + ("PASS" if ok else "FAIL"))
    return 0 if ok else 1


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--out", default="shots/webd")
    parser.add_argument("--no-build", action="store_true", help="reuse the image of the last run")
    parser.add_argument("--accel", default="auto", choices=["auto", "none", "tcg", "whpx", "kvm"])
    parser.add_argument("--qemu")
    parser.add_argument("--memory", default=limits.BIG_APP_MEMORY, help="guest RAM (default: the big-app profile's 4G)")
    parser.add_argument("--step-timeout", type=float, default=300.0, help="seconds per gate")
    parser.add_argument("--skip-loss", action="store_true", help="leave out the kill of the web process")
    parser.add_argument("--no-label-trace", dest="label_trace", action="store_false",
                        help="build without LAZYOS_LABEL_TRACE=1 (the label check then sees nothing)")
    parser.add_argument("--judge-only", metavar="DIR", help="judge a saved run (DIR/serial.log, requests.json, shot_*.png)")
    args = parser.parse_args(argv)
    out = Path(args.out) if Path(args.out).is_absolute() else ROOT / args.out
    if args.judge_only:
        saved = Path(args.judge_only)
        text = (saved / "serial.log").read_text(errors="replace")
        record = json.loads((saved / "requests.json").read_text(encoding="utf-8"))
        return verdict(text, record, saved, True, not args.skip_loss)
    out.mkdir(parents=True, exist_ok=True)
    if listener.busy():
        print(f"{PREFIX}:HARNESS:FAIL port {listener.PORT} on {listener.HOST} is already in use")
        return 1
    if args.no_build:
        if not IMAGE.is_file():
            print(f"{PREFIX}:HARNESS:FAIL --no-build needs the image of an earlier run ({IMAGE})")
            return 1
    else:
        error = build_image(args)
        if error:
            print(f"{PREFIX}:HARNESS:FAIL {error}")
            return 1
    try:
        served = listener.Listener()
    except OSError as error:
        print(f"{PREFIX}:HARNESS:FAIL cannot listen on {listener.HOST}:{listener.PORT}: {error}")
        return 1
    try:
        ran, text = run_session(session.light(args.step_timeout, args.skip_loss), out, args)
    finally:
        record = served.snapshot()
        served.close()
    (out / "requests.json").write_text(json.dumps(record, indent=1), encoding="utf-8")
    return verdict(text, record, out, ran, not args.skip_loss)


if __name__ == "__main__":
    sys.exit(main())
