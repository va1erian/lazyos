#!/usr/bin/env python3
"""webd's full views end to end (docs/webd-plan.md, P2 slice 3): build a desktop with webd, networking
and the full-view test client `webdbrowse` (`os.lazy.webdbrowse`, shipped by `--webd-demo`), serve real
HTTP from the host, boot once, and judge what the client, the server, the OS volume and the screen saw.

The client runs a scenario (`xui-app/crates/webd-client/src/browse/`) through `os.lazy.webd.view.v1` and
its two scopes against `full_server.py` on 127.0.0.1:18081 (the guest's 10.0.2.2:18081): navigation, back
and forward, cookies in the `browser` profile and in the app's own profile (isolated), a download into
the person's Downloads folder, a JavaScript alert and confirm, a `window.open`, a held `mailto:`
navigation, content blocking with a compiled rule list and a reload that bypasses it, a view without the
network scope that reaches nothing, frames with a scroll shift after a wheel turn, and the latencies.

    python tools/webd/full_run.py                   # build, boot, judge
    python tools/webd/full_run.py --no-build        # the image of the last run
    python tools/webd/full_run.py --no-label-trace  # build without LAZYOS_LABEL_TRACE=1
    python tools/webd/full_run.py --judge-only DIR  # judge a saved run (serial.log, requests.json, shots)

Needs webd and the engine's programs (`python tools/webkit/webd.py`, `tools/webkit/port.py`) and the
client package (`python tools/xui/build.py`); `run_demo.py --webd-demo --net` builds the image. The port
18081 must be free (no web harness uses it). Results go to `shots/webd_full`.
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
import full_judge as j  # noqa: E402
import full_server  # noqa: E402
from lazygui import limits  # noqa: E402

PY = sys.executable
IMAGE = ROOT / "target" / "lazyos.img"
RUN_DEMO = ROOT / "tools" / "run_demo.py"
PREFIX = "WEBDFULL"
DOWNLOADED = "/home/user/Downloads/file.bin"


def report(section: str, detail: str, problems: list[str]) -> bool:
    for problem in problems[:16]:
        print(f"{PREFIX}:{section}:FAIL {problem}")
    if not problems:
        print(f"{PREFIX}:{section}:PASS {detail}".rstrip())
    return not problems


def session(timeout: float) -> list[dict]:
    wait = lambda marker, secs=timeout: {"wait_for": marker, "timeout": secs}  # noqa: E731
    return [
        wait("PKGD:PROVISION:DONE", 600), wait("TERM:UP:PASS"), wait(f"{j.W}:UP:PASS"),
        wait(f"{j.W}:SERVE:app.os.lazy.webd.view"), wait("NETD:RESOLV wrote"),
        wait(f"{j.B}:UP:PASS"),
        # The Terminal opened after the client and covers it: raise the client.
        {"click_at": {"target": f"title:{j.WINDOW}"}, "timeout": 60},
        wait(f"{j.B}:NAV:", 300), {"wait": 1.0}, {"shot": "f01_index"},
        wait(f"{j.B}:DELTA:", 900), {"wait": 1.0}, {"shot": "f02_long"},
        wait(f"{j.B}:DONE:", 900), {"wait": 2.0}, {"shot": "f03_done"},
        # The flusher writes every 5 s: the download must be on disk before QEMU is killed.
        {"wait": 7.0},
        {"key_down": "ctrl"},
        {"key": "q", "until": f"{j.B}:QUIT:PASS", "timeout": 60, "retries": 2},
        {"key_up": "ctrl"},
        {"wait": 6.0},
        {"quit": True},
    ]


def build_image(args) -> str | None:
    import busybox  # noqa: PLC0415

    shell = busybox.ensure_busybox()
    if shell is None:
        return "no BusyBox for the Terminal's shell (see tools/abi/busybox.py)"
    env = {k: v for k, v in os.environ.items() if not k.startswith("LAZYOS_")}
    env.update(LAZYOS_XUI_AUTOSTART="webdbrowse,term", LAZYOS_UI_PROBE="1", LAZYOS_BUSYBOX=str(shell))
    if args.label_trace:
        env["LAZYOS_LABEL_TRACE"] = "1"
    command = [PY, str(RUN_DEMO), "--webd-demo", "--desktop", "--net", "--big-app", "--reset-os", "--yes",
               "--autologin", "user", "--build-only"]
    print(f"webd: {' '.join(command[1:])} (LAZYOS_XUI_AUTOSTART=webdbrowse,term)", flush=True)
    done = subprocess.run(command, cwd=ROOT, env=env, capture_output=True, text=True)
    if done.returncode != 0:
        return "run_demo.py --webd-demo --net failed:\n" + (done.stdout + done.stderr)[-4000:]
    return None if IMAGE.is_file() else f"{IMAGE} was not built"


def run_session(out: Path, args) -> tuple[bool, str]:
    out.mkdir(parents=True, exist_ok=True)
    for stale in ["serial.log", "net.pcap", "summary.json", *(p.name for p in out.glob("shot_*.png"))]:
        (out / stale).unlink(missing_ok=True)
    script = out / "session.json"
    script.write_text(json.dumps(session(args.step_timeout), indent=1) + "\n", encoding="utf-8")
    command = [PY, str(ROOT / "tools" / "screenshot" / "qemu_session.py"), "--image", str(IMAGE),
               "--out", str(out), "--script", str(script), "--accel", args.accel, "--memory", args.memory,
               "--net", "--net-forward", "none", "--fail-on", "PANIC", "--fail-on", "EXCEPTION"]
    if args.qemu:
        command += ["--qemu", args.qemu]
    code = subprocess.call(command, cwd=ROOT, stdout=subprocess.DEVNULL)
    log = out / "serial.log"
    return code == 0, log.read_text(errors="replace") if log.is_file() else ""


def read_download() -> bytes | None:
    """The downloaded file on the OS volume, read from the host with the ext2 library."""
    done = subprocess.run(["cargo", "run", "-q", "-p", "ext2fs", "--example", "osread", "--", str(IMAGE), "cat",
                           DOWNLOADED], cwd=ROOT, capture_output=True)
    return done.stdout if done.returncode == 0 else None


def verdict(text: str, record: dict, out: Path, ran: bool, saved: bytes | None) -> int:
    region = j.window_region(text)
    ok = report("SESSION", "the boot ran to its end", [] if ran else ["the session did not finish"])
    ok = report("SERVICE", "webd serves the full interface; the client is up", j.judge_service(text)) and ok
    ok = report("STEPS", f"every step of the scenario passed ({len(j.STEPS)})", j.judge_steps(text)) and ok
    ok = report("BLOCKING", "the blocked picture stayed off the network until the bypassing reload",
                j.judge_blocking(record)) and ok
    ok = report("COOKIES", "each profile sent only its own cookie", j.judge_cookies(record)) and ok
    ok = report("NONET", "the view without the network scope reached nothing", j.judge_network(record)) and ok
    ok = report("DOWNLOAD", f"{DOWNLOADED} holds the server's bytes",
                j.judge_download(text, record, saved, full_server.file_body())) and ok
    ok = report("SHOT", f"the client window {region} shows the long page",
                j.judge_shot(out / "shot_f02_long.png", region, "long page")) and ok
    ok = report("LABEL", "no refused call beyond the documented optional ones", j.judge_denies(text)) and ok
    for line in sorted(set(j.light.DENY.findall(text)))[:0]:
        print(line)
    numbers = j.latency(text)
    print(f"{PREFIX}:INFO:LATENCY {numbers}")
    print(f"{PREFIX}:HARNESS:" + ("PASS" if ok else "FAIL"))
    return 0 if ok else 1


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--out", default="shots/webd_full")
    parser.add_argument("--no-build", action="store_true")
    parser.add_argument("--accel", default="auto", choices=["auto", "none", "tcg", "whpx", "kvm"])
    parser.add_argument("--qemu")
    parser.add_argument("--memory", default=limits.BIG_APP_MEMORY)
    parser.add_argument("--step-timeout", type=float, default=300.0)
    parser.add_argument("--no-label-trace", dest="label_trace", action="store_false")
    parser.add_argument("--judge-only", metavar="DIR")
    args = parser.parse_args(argv)
    out = Path(args.out) if Path(args.out).is_absolute() else ROOT / args.out
    if args.judge_only:
        saved = Path(args.judge_only)
        record = json.loads((saved / "requests.json").read_text(encoding="utf-8"))
        download = saved / "file.bin"
        return verdict((saved / "serial.log").read_text(errors="replace"), record, saved, True,
                       download.read_bytes() if download.is_file() else None)
    if full_server.busy():
        print(f"{PREFIX}:HARNESS:FAIL port {full_server.PORT} is already in use")
        return 1
    if not args.no_build:
        error = build_image(args)
        if error:
            print(f"{PREFIX}:HARNESS:FAIL {error}")
            return 1
    served = full_server.Server()
    try:
        ran, text = run_session(out, args)
    finally:
        record = served.snapshot()
        served.close()
    (out / "requests.json").write_text(json.dumps(record, indent=1), encoding="utf-8")
    saved = read_download()
    if saved is not None:
        (out / "file.bin").write_bytes(saved)
    return verdict(text, record, out, ran, saved)


if __name__ == "__main__":
    sys.exit(main())
