#!/usr/bin/env python3
"""Build the WebKit browser into a desktop image, browse in it, and judge what the guest, the
host's servers and the screen saw (docs/webkit-port-plan.md W5).

Needs the app built first (Docker: ``python tools/webkit/deps.py``, ``port.py``, ``app.py``;
then ``python tools/xui/build.py`` packages it as ``org.lazy.webkit``). Then:

1. a throwaway test CA and a leaf for theoldnet.com (``tools/web/certs.py``); the image is built
   with ``LAZYOS_WEBKIT=1`` (desktop, ``netd``, the big-app limits), the CA appended to the
   system bundle (``LAZYOS_TLS_TEST_CA``) and the sites' names mapped to the host in /etc/hosts
   (``LAZYOS_TLS_TEST_HOSTS``): WebKit's curl verifies certificates exactly as in production;
2. the host serves stand-ins for the sites on their real ports (``tools/web/sites.py``,
   LazyWeb's harness): http://example.com/, https://theoldnet.com/ with its PNG, JPEG and
   GIF pictures, and the WebKit test pages (script, link, form and its echo);
3. the session (``app_session.py``, also ``tools/screenshot/examples/webkit.json``) starts at
   login with the browser's window, types addresses, clicks a link, types into a form, turns
   the wheel and goes Back, with a screenshot of each;
4. the verdict (``appjudge.py``): the markers, the requests the host's servers saw and the
   screenshots' page area.

    python tools/webkit/app_run.py                    # build, boot, browse, judge
    python tools/webkit/app_run.py --no-build         # the image and certificates of the last run
    python tools/webkit/app_run.py --label-trace      # also list LABEL:DENY lines (package permissions)
    python tools/webkit/app_run.py --judge-only DIR   # judge a saved run (serial.log, shots, requests.json)

The servers need ports 80 and 443 on 127.0.0.1 (any user on Windows; root, or on Linux
``sudo sysctl net.ipv4.ip_unprivileged_port_start=80``). Exit status is non-zero on any failure;
logs, screenshots and the capture go to ``shots/webkit``.
"""

from __future__ import annotations

import argparse
import json
import os
import re
import subprocess
import sys
from dataclasses import dataclass
from pathlib import Path

HERE = Path(__file__).resolve().parent
ROOT = HERE.parent.parent
for sub in ("tools/web", "tools/abi", "tools"):
    sys.path.insert(0, str(ROOT / sub))
sys.path.insert(0, str(HERE))
import appjudge  # noqa: E402
import app_session  # noqa: E402
import busybox  # noqa: E402
import certs  # noqa: E402
import judge as web_judge  # noqa: E402
import sites  # noqa: E402
from lazygui import limits  # noqa: E402

PY = sys.executable
IMAGE = ROOT / "target" / "lazyos.img"
BIN = ROOT / "target" / "webkit" / "bin"
ELF = ROOT / "target" / "xui" / "xui-webkit.elf"
#: Switches a previous build may have left in the caller's environment.
STALE = ("LAZYOS_TLS_TEST_CA", "LAZYOS_TLS_TEST_HOSTS", "LAZYOS_CLI", "LAZYOS_LAZYWEB", "LAZYOS_XUI_APPS",
         "LAZYOS_WEBKIT_PORT", "LAZYOS_WEBKIT_DEPS", "LAZYOS_JSC", "LAZYOS_LINUXAPPS", "LAZYOS_TLS")


def built() -> str | None:
    """An error message when what the image needs is not built, else None."""
    missing = [str(p.relative_to(ROOT)) for p in (BIN / "WebProcess", BIN / "NetworkProcess", ELF) if not p.is_file()]
    if missing:
        return (f"not built: {', '.join(missing)} (python tools/webkit/deps.py, port.py, app.py; "
                "then python tools/xui/build.py)")
    return None


def build_image(extra: dict[str, str], label_trace: bool) -> str | None:
    """Build the desktop image with the browser; an error message, or None."""
    shell = busybox.ensure_busybox()
    if shell is None:
        return "no BusyBox for the shell (see tools/abi/busybox.py)"
    env = {k: v for k, v in os.environ.items() if k not in STALE}
    env.update(LAZYOS_DESKTOP="1", LAZYOS_WEBKIT="1", LAZYOS_NETD="1", LAZYOS_NETD_ARGS="demo=0",
               LAZYOS_XUI_AUTOSTART="webkit", LAZYOS_UI_PROBE="1", LAZYOS_RESET_OS="1",
               LAZYOS_BUSYBOX=str(shell), **extra,
               **limits.limit_env(limits.with_defaults(list(limits.WEBKIT_LIMITS), limits.BIG_APP_LIMITS)))
    if label_trace:
        env["LAZYOS_LABEL_TRACE"] = "1"
    switches = " ".join(f"{k}={env[k]}" for k in sorted(env) if k.startswith("LAZYOS_") and k != "LAZYOS_BUSYBOX")
    print(f"webkit: {switches} cargo build", flush=True)
    result = subprocess.run(["cargo", "build"], cwd=ROOT, env=env, capture_output=True, text=True)
    if result.returncode != 0:
        return "cargo build failed:\n" + result.stderr[-4000:]
    return None if IMAGE.is_file() else f"{IMAGE} was not built"


def run_session(steps: list[dict], out: Path, args) -> tuple[bool, str]:
    """Drive the guest; (session ok, serial log text)."""
    script = out / "session.json"
    script.write_text(app_session.render(steps), encoding="utf-8")
    command = [PY, str(ROOT / "tools" / "screenshot" / "qemu_session.py"), "--image", str(IMAGE),
               "--out", str(out), "--script", str(script), "--accel", args.accel, "--memory", args.memory,
               "--net", "--net-forward", "none", "--net-pcap", str(out / "net.pcap"),
               "--fail-on", "PANIC", "--fail-on", "EXCEPTION", "--fail-on", r"WEBKIT:W5:FAIL",
               "--fail-on", r"WEBKIT:(W5:UI:)?(BIND|RUN):FAIL"]
    if args.qemu:
        command += ["--qemu", args.qemu]
    code = subprocess.call(command, cwd=ROOT, stdout=subprocess.DEVNULL)
    log = out / "serial.log"
    return code == 0, log.read_text(errors="replace") if log.is_file() else ""


@dataclass
class Record:
    """What the host's servers saw, as `sites.Record` has it (rebuilt from `requests.json`)."""
    requests: list
    handshakes: list


def load_record(path: Path) -> Record:
    rows = json.loads(path.read_text(encoding="utf-8"))
    return Record([sites.Request(**{k: v for k, v in row.items() if k in sites.Request.__dataclass_fields__})
                   for row in rows["requests"]], rows["handshakes"])


def save_record(path: Path, record) -> None:
    path.write_text(json.dumps({"requests": [vars(r) for r in record.requests],
                                "handshakes": list(record.handshakes)}, indent=1), encoding="utf-8")


def report(section: str, detail: str, problems: list[str]) -> bool:
    for problem in problems[:16]:
        print(f"WEBKIT:W5:{section}:FAIL {problem}")
    if not problems:
        print(f"WEBKIT:W5:{section}:PASS {detail}".rstrip())
    return not problems


def shots(out: Path) -> list[Path]:
    """The page screenshots, in session order (`00_window` is the empty window)."""
    return sorted(p for p in out.glob("shot_*.png") if not re.search(r"shot_(00|failed)", p.name))


def sizes() -> str:
    parts = [f"{p.name} {p.stat().st_size / 1048576:.1f} MiB" for p in (BIN / "webkit-app", BIN / "WebProcess",
                                                                         BIN / "NetworkProcess") if p.is_file()]
    return ", ".join(parts)


def verdict(out: Path, session_ok: bool, text: str, record) -> int:
    ok = report("SESSION", "", [] if session_ok else ["the session did not finish (see summary.json)"])
    ok = report("BROWSER", "up, every page loaded with its title, script ran, link clicked, form typed",
                appjudge.judge_serial(text)) and ok
    assets = web_judge.page_assets()
    ok = report("SERVERS", f"the pages, {len(assets)} resources over HTTPS, the form's query",
                appjudge.judge_servers(record, assets)) and ok
    region = appjudge.window_region(text)
    pictures = shots(out)
    by_name = {p.name: p for p in pictures}
    page_shots = [p for p in pictures if not re.search(r"shot_(04_scrolled|09_back|10_maximized)", p.name)]
    ok = report("SHOTS", f"{len(pictures)} shots, page area {region}", appjudge.judge_shots(page_shots, region)) and ok
    scroll = {"before": by_name.get("shot_03_theoldnet_later.png"), "after": by_name.get("shot_04_scrolled.png")}
    ok = report("SCROLL", "the wheel moved the page", appjudge.judge_scroll(scroll, region)) and ok
    ok = report("RESIZE", f"the page was laid out again when the window grew: {appjudge.resizes(text)}",
                appjudge.judge_resize(text)) and ok
    grown = appjudge.window_region(text, last=True)
    ok = report("MAXIMIZED", f"the page is painted to the right edge of the maximized window {grown}",
                appjudge.judge_maximized(by_name.get("shot_10_maximized.png"), grown)) and ok
    denied = sorted(set(re.findall(r"LABEL:DENY[^\r\n]*", text)))
    if denied:
        print(f"WEBKIT:W5:INFO:LABEL:DENY {len(denied)} distinct line(s) (package permissions to derive):")
        for line in denied[:40]:
            print("  " + line)
    paint = appjudge.first_paint_ms(text)
    loads = appjudge.load_ms(text)
    print(f"WEBKIT:W5:INFO:TIMES first paint after navigation (ms) {paint}; load (ms) {loads}")
    print(f"WEBKIT:W5:INFO:SIZES {sizes()}")
    print("WEBKIT:W5:HARNESS:" + ("PASS" if ok else "FAIL"))
    return 0 if ok else 1


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--out", default="shots/webkit")
    parser.add_argument("--no-build", action="store_true", help="reuse the image and certificates of the last run")
    parser.add_argument("--accel", default="auto", choices=["auto", "none", "tcg", "whpx", "kvm"])
    parser.add_argument("--qemu")
    parser.add_argument("--memory", default=limits.BIG_APP_MEMORY, help="guest RAM (default: the big-app profile's 4G)")
    parser.add_argument("--step-timeout", type=float, default=300.0, help="seconds per gate")
    parser.add_argument("--label-trace", action="store_true", help="build with LAZYOS_LABEL_TRACE=1 and list the denials")
    parser.add_argument("--judge-only", metavar="DIR", help="judge a saved run (serial.log, shot_*.png, requests.json)")
    args = parser.parse_args(argv)
    out = Path(args.out) if Path(args.out).is_absolute() else ROOT / args.out
    if args.judge_only:
        saved = Path(args.judge_only)
        return verdict(saved, True, (saved / "serial.log").read_text(errors="replace"),
                       load_record(saved / "requests.json"))
    out.mkdir(parents=True, exist_ok=True)
    for stale in ["serial.log", "net.pcap", "summary.json", *(p.name for p in out.glob("shot_*.png"))]:
        (out / stale).unlink(missing_ok=True)
    problem = built()
    if problem:
        print(f"WEBKIT:W5:HARNESS:FAIL {problem}")
        return 1
    busy = sites.ports_free()
    if busy:
        print(f"WEBKIT:W5:HARNESS:FAIL host port(s) {busy} in use: {sites.port_hint(OSError('busy'))}")
        return 1
    files = certs.load(out / "certs") if args.no_build else certs.generate(out / "certs")
    if files is None or (args.no_build and not IMAGE.is_file()):
        print("WEBKIT:W5:HARNESS:FAIL --no-build needs the image and certificates of an earlier run")
        return 1
    if not args.no_build:
        error = build_image({"LAZYOS_TLS_TEST_CA": str(files.ca), "LAZYOS_TLS_TEST_HOSTS": str(files.hosts)},
                            args.label_trace)
        if error:
            print(f"WEBKIT:W5:HARNESS:FAIL {error}")
            return 1
    try:
        served = sites.Sites(files.leaf_cert, files.leaf_key)
    except OSError as error:
        print(f"WEBKIT:W5:HARNESS:FAIL cannot start the host servers: {sites.port_hint(error)}")
        return 1
    try:
        session_ok, text = run_session(app_session.steps(args.step_timeout), out, args)
    finally:
        record = served.snapshot()
        served.close()
    save_record(out / "requests.json", record)
    return verdict(out, session_ok, text, record)


if __name__ == "__main__":
    sys.exit(main())
