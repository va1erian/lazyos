#!/usr/bin/env python3
"""Build LazyWeb+ into a desktop image, browse in it twice, and judge what the guest, the host's
servers, the OS volume and the screen saw (docs/lazywebplus.md).

Needs the app built first (Docker: ``python tools/webkit/deps.py``, ``port.py`` and ``app.py
--plus``; then ``python tools/xui/build.py`` packages it as ``os.lazy.lazywebplus``). Then:

1. a throwaway test CA and a leaf for theoldnet.com (``tools/web/certs.py``); the image is built
   with ``LAZYOS_LAZYWEBPLUS=1`` (desktop, ``netd``, the big-app limits), the CA appended to the
   system bundle (``LAZYOS_TLS_TEST_CA``) and the sites' names mapped to the host in /etc/hosts
   (``LAZYOS_TLS_TEST_HOSTS``): WebKit's curl verifies certificates exactly as in production;
2. the host serves stand-ins for the sites on their real ports (``tools/web/sites.py``, LazyWeb's
   harness): http://example.com/, https://theoldnet.com/ with its pictures, the cookie page, an
   attachment and a slow one, WebKit's test pages and LazyWeb+'s (``fixtures/theoldnet.com/webplus``);
3. the first boot's session (``session.py``, also ``tools/screenshot/examples/lazywebplus.json``)
   browses, answers the dialogs, clicks the links, downloads, opens the history and downloads pages
   by key, and closes the window; the OS volume is read from the host (``osread``);
4. the second boot of the same image (``lazywebplus_second.json``) finds the history and the cookie
   again, clears the history from the history page and closes the window;
5. the verdict (``judge.py``): the markers of both boots, the requests the host's servers saw,
   the files in the profile and the screenshots' page area.

    python tools/webplus/run.py                    # build, boot twice, judge
    python tools/webplus/run.py --no-build         # the image and certificates of the last run
    python tools/webplus/run.py --label-trace      # build with LAZYOS_LABEL_TRACE=1; any LABEL:DENY fails
    python tools/webplus/run.py --judge-only DIR   # judge a saved run (DIR/first, DIR/second, ...)

The servers need ports 80 and 443 on 127.0.0.1 (any user on Windows; root, or on Linux ``sudo
sysctl net.ipv4.ip_unprivileged_port_start=80``), and 81 for the address that hangs up. Exit
status is non-zero on any failure; logs, screenshots and captures go to ``shots/webplus``. Every
run recreates ``target/lazyos.img``'s OS volume.
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
for sub in ("tools/web", "tools/abi", "tools"):
    sys.path.insert(0, str(ROOT / sub))
sys.path.insert(0, str(HERE))
# This directory's `judge` and `session` first: tools/web has modules of the same names, and
# `certs` and `busybox` put their own directories at the front of the path.
import judge  # noqa: E402  (LazyWeb's is tools/web/judge.py, loaded by path in ours)
import session  # noqa: E402
import busybox  # noqa: E402
import certs  # noqa: E402
import sites  # noqa: E402
from lazygui import limits  # noqa: E402

PY = sys.executable
IMAGE = ROOT / "target" / "lazyos.img"
BIN = ROOT / "target" / "webkit" / "bin"
ELF = ROOT / "target" / "xui" / "xui-lazywebplus.elf"
HOME = "/home/user"
APP_DIR = f"{HOME}/.apps/os.lazy.lazywebplus"
#: Switches a previous build may have left in the caller's environment.
STALE = ("LAZYOS_TLS_TEST_CA", "LAZYOS_TLS_TEST_HOSTS", "LAZYOS_CLI", "LAZYOS_LAZYWEB", "LAZYOS_XUI_APPS",
         "LAZYOS_WEBKIT", "LAZYOS_WEBKIT_PORT", "LAZYOS_WEBKIT_DEPS", "LAZYOS_JSC", "LAZYOS_LINUXAPPS",
         "LAZYOS_TLS", "LAZYOS_LAZYWEBPLUS")


def built() -> str | None:
    """An error message when what the image needs is not built, else None."""
    missing = [str(p.relative_to(ROOT)) for p in (BIN / "WebProcess", BIN / "NetworkProcess", ELF) if not p.is_file()]
    if missing:
        return (f"not built: {', '.join(missing)} (python tools/webkit/deps.py, port.py, app.py --plus; "
                "then python tools/xui/build.py)")
    return None


def build_image(extra: dict[str, str], label_trace: bool) -> str | None:
    """Build the desktop image with the browser; an error message, or None."""
    shell = busybox.ensure_busybox()
    if shell is None:
        return "no BusyBox for the shell (see tools/abi/busybox.py)"
    env = {k: v for k, v in os.environ.items() if k not in STALE}
    env.update(LAZYOS_DESKTOP="1", LAZYOS_LAZYWEBPLUS="1", LAZYOS_NETD="1", LAZYOS_NETD_ARGS="demo=0",
               LAZYOS_XUI_AUTOSTART="lazywebplus", LAZYOS_UI_PROBE="1", LAZYOS_RESET_OS="1",
               LAZYOS_BUSYBOX=str(shell), **extra,
               **limits.limit_env(limits.with_defaults(list(limits.WEBKIT_LIMITS), limits.BIG_APP_LIMITS)))
    if label_trace:
        env["LAZYOS_LABEL_TRACE"] = "1"
    switches = " ".join(f"{k}={env[k]}" for k in sorted(env) if k.startswith("LAZYOS_") and k != "LAZYOS_BUSYBOX")
    print(f"webplus: {switches} cargo build", flush=True)
    result = subprocess.run(["cargo", "build"], cwd=ROOT, env=env, capture_output=True, text=True)
    if result.returncode != 0:
        return "cargo build failed:\n" + result.stderr[-4000:]
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
               # Every failure but the one load the session breaks on purpose.
               "--fail-on", rf"{judge.P}:FAIL:(?!provisional:{re.escape(judge.FAIL_URL)}:|Could not load {re.escape(judge.FAIL_URL)})",
               "--fail-on", rf"{judge.P}:(BIND|RUN):FAIL", "--fail-on", rf"{judge.P}:QUIT:FAIL",
               "--fail-on", rf"{judge.P}:DOWNLOAD:FAIL"]
    if args.qemu:
        command += ["--qemu", args.qemu]
    code = subprocess.call(command, cwd=ROOT, stdout=subprocess.DEVNULL)
    log = out / "serial.log"
    return code == 0, log.read_text(errors="replace") if log.is_file() else ""


def osread(*argv: str) -> bytes | None:
    """`osread IMAGE <command> PATH`'s bytes (the OS volume read from the host), or None when it fails."""
    done = subprocess.run(["cargo", "run", "-q", "-p", "ext2fs", "--example", "osread", "--", str(IMAGE), *argv],
                          cwd=ROOT, capture_output=True)
    return done.stdout if done.returncode == 0 else None


def read_profile() -> dict[str, bytes | None]:
    """What the profile and the Downloads folder hold on the OS volume, by name under the home."""
    listing: dict[str, bytes | None] = {}
    for name in (judge.DOWNLOAD_NAME, judge.SLOW_NAME, f"{judge.DOWNLOAD_NAME}.part", f"{judge.SLOW_NAME}.part"):
        listing[f"Downloads/{name}"] = osread("cat", f"{HOME}/Downloads/{name}")
    ls = osread("ls", APP_DIR)
    names = ls.decode(errors="replace").split() if ls is not None else []
    listing[f".apps/os.lazy.lazywebplus/{judge.COOKIE_DB}"] = (
        osread("cat", f"{APP_DIR}/{judge.COOKIE_DB}") if judge.COOKIE_DB in names else None)
    return listing


def read_history() -> str:
    data = osread("cat", f"{APP_DIR}/{judge.HISTORY_FILE}")
    return data.decode(errors="replace") if data is not None else ""


def load_record(path: Path) -> sites.Record:
    """The host's request log of a boot, as `save_record` wrote it."""
    rows = json.loads(path.read_text(encoding="utf-8"))
    fields = sites.Request.__dataclass_fields__
    return sites.Record([sites.Request(**{k: v for k, v in row.items() if k in fields}) for row in rows["requests"]],
                        rows["handshakes"])


def save_record(path: Path, record: sites.Record) -> None:
    path.write_text(json.dumps({"requests": [vars(r) for r in record.requests],
                                "handshakes": list(record.handshakes)}, indent=1), encoding="utf-8")


def report(section: str, detail: str, problems: list[str]) -> bool:
    for problem in problems[:16]:
        print(f"{judge.P}:{section}:FAIL {problem}")
    if not problems:
        print(f"{judge.P}:{section}:PASS {detail}".rstrip())
    return not problems


def shots(out: Path) -> list[Path]:
    """The page screenshots, in session order."""
    return sorted(out.glob("shot_*.png"))


def sizes() -> str:
    parts = [f"{p.name} {p.stat().st_size / 1048576:.1f} MiB" for p in (BIN / "lazywebplus-app", BIN / "WebProcess",
                                                                         BIN / "NetworkProcess") if p.is_file()]
    elf = ROOT / "target" / "xui" / "xui-lazywebplus.elf"
    parts.append(f"{elf.name} {elf.stat().st_size / 1048576:.1f} MiB") if elf.is_file() else None
    return ", ".join(parts)


def verdict(out: Path, firsts: dict, seconds: dict, records: tuple, profile: dict) -> int:
    """Judge both boots. `firsts` and `seconds`: {"ok", "text"}; `records`: (first, second) request
    records; `profile`: {"files", "history_first", "history_second"}."""
    first, second = firsts["text"], seconds["text"]
    ok = report("SESSION", "both boots ran to their end",
                [f"the {name} boot's session did not finish (see its summary.json)"
                 for name, boot in (("first", firsts), ("second", seconds)) if not boot["ok"]])
    ok = report("BROWSER", "up, every page loaded with its title, script and forms worked",
                judge.judge_start(first) + judge.judge_pages(first)) and ok
    ok = report("DIALOGS", "alert, confirm and prompt drawn, answered from the keyboard, answers reached the page",
                judge.judge_dialogs(first)) and ok
    ok = report("LAUNCH", "a script starts nothing (BLOCKED), a click reaches mimed", judge.judge_launches(first)) and ok
    ok = report("POPUP", "target=_blank and window.open from a click open in the one view",
                judge.judge_popups(first)) and ok
    ok = report("DOWNLOADS", "both attachments finished whole", judge.judge_downloads(first)) and ok
    assets = judge.page_assets()
    ok = report("SERVERS", f"the pages, {len(assets)} resources over HTTPS, the form's query, the cookie",
                judge.judge_servers(records[0], records[1], assets)) and ok
    ok = report("PROFILE", "history, downloads and cookies on the OS volume; Clear history emptied it",
                judge.judge_profile(profile["files"], profile["history_first"], profile["history_second"])) and ok
    ok = report("SECOND", "the second boot found the cookie and the history", judge.judge_second(second)) and ok
    region = judge.window_region(first)
    pictures = shots(out / "first")
    by_name = {p.name: p for p in pictures}
    page_shots = [p for p in pictures if not re.search(r"shot_(17_scrolled|22_maximized|21_stopped)", p.name)]
    ok = report("SHOTS", f"{len(pictures)} shots, page area {region}", judge.judge_shots(page_shots, region)) and ok
    scroll = {"before": by_name.get("shot_16_tall.png"), "after": by_name.get("shot_17_scrolled.png")}
    problems, moved = judge.judge_scroll(scroll, region, judge.SCROLL_PX)
    ok = report("SCROLL", f"the wheel moved the static bands {moved} px", problems) and ok
    ok = report("RESIZE", f"the page was laid out again when the window grew: {judge.resizes(first)}",
                judge.judge_resize(first)) and ok
    grown = judge.window_region(first, last=True)
    ok = report("MAXIMIZED", f"the page is painted to the right edge of the maximized window {grown}",
                judge.judge_maximized(by_name.get("shot_22_maximized.png"), grown)) and ok
    ok = report("QUIT", "both boots closed the window, both children were ended and reaped",
                judge.judge_quit(first) + judge.judge_quit(second)) and ok
    denied = sorted(set(re.findall(r"LABEL:DENY[^\r\n]*", first + second)))
    if denied:
        print(f"{judge.P}:INFO:LABEL:DENY {len(denied)} distinct line(s) (package permissions to derive):")
        for line in denied[:40]:
            print("  " + line)
    ok = (not denied or not os.environ.get("LAZYOS_WEBPLUS_LABEL_STRICT")) and ok
    print(f"{judge.P}:INFO:TIMES first paint after navigation (ms) {judge.first_paint_ms(first)}; "
          f"load (ms) {judge.load_ms(first)}")
    print(f"{judge.P}:INFO:SIZES {sizes()}")
    print(f"{judge.P}:HARNESS:" + ("PASS" if ok else "FAIL"))
    return 0 if ok else 1


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--out", default="shots/webplus")
    parser.add_argument("--no-build", action="store_true", help="reuse the image and certificates of the last run")
    parser.add_argument("--accel", default="auto", choices=["auto", "none", "tcg", "whpx", "kvm"])
    parser.add_argument("--qemu")
    parser.add_argument("--memory", default=limits.BIG_APP_MEMORY, help="guest RAM (default: the big-app profile's 4G)")
    parser.add_argument("--step-timeout", type=float, default=300.0, help="seconds per gate")
    parser.add_argument("--label-trace", action="store_true",
                        help="build with LAZYOS_LABEL_TRACE=1 and list the denials (a denial fails with --label-strict)")
    parser.add_argument("--judge-only", metavar="DIR", help="judge a saved run (DIR/first, DIR/second, profile.json)")
    args = parser.parse_args(argv)
    out = Path(args.out) if Path(args.out).is_absolute() else ROOT / args.out
    if args.label_trace:
        os.environ["LAZYOS_WEBPLUS_LABEL_STRICT"] = "1"
    if args.judge_only:
        saved = Path(args.judge_only)
        profile = json.loads((saved / "profile.json").read_text(encoding="utf-8"))
        files = {name: (bytes.fromhex(hexed) if hexed is not None else None) for name, hexed in profile["files"].items()}
        return verdict(saved, {"ok": True, "text": (saved / "first" / "serial.log").read_text(errors="replace")},
                       {"ok": True, "text": (saved / "second" / "serial.log").read_text(errors="replace")},
                       (load_record(saved / "first" / "requests.json"),
                        load_record(saved / "second" / "requests.json")),
                       {"files": files, "history_first": profile["history_first"],
                        "history_second": profile["history_second"]})
    out.mkdir(parents=True, exist_ok=True)
    problem = built()
    if problem:
        print(f"{judge.P}:HARNESS:FAIL {problem}")
        return 1
    busy = sites.ports_free((sites.HTTP_PORT, sites.HTTPS_PORT, judge.CLOSER_PORT))
    if busy:
        print(f"{judge.P}:HARNESS:FAIL host port(s) {busy} in use: {sites.port_hint(OSError('busy'))}")
        return 1
    files = certs.load(out / "certs") if args.no_build else certs.generate(out / "certs")
    if files is None or (args.no_build and not IMAGE.is_file()):
        print(f"{judge.P}:HARNESS:FAIL --no-build needs the image and certificates of an earlier run")
        return 1
    if not args.no_build:
        error = build_image({"LAZYOS_TLS_TEST_CA": str(files.ca), "LAZYOS_TLS_TEST_HOSTS": str(files.hosts)},
                            args.label_trace)
        if error:
            print(f"{judge.P}:HARNESS:FAIL {error}")
            return 1
    try:
        served = sites.Sites(files.leaf_cert, files.leaf_key, closer_port=judge.CLOSER_PORT)
    except OSError as error:
        print(f"{judge.P}:HARNESS:FAIL cannot start the host servers: {sites.port_hint(error)}")
        return 1
    try:
        ok_first, text_first = run_session(session.first(args.step_timeout), out / "first", args)
        first_record = served.snapshot()
        history_first = read_history()
        files_after_first = read_profile()
        ok_second, text_second = run_session(session.second(args.step_timeout), out / "second", args)
        all_record = served.snapshot()
        history_second = read_history()
    finally:
        served.close()
    second_record = sites.Record(all_record.requests[len(first_record.requests):],
                                 all_record.handshakes[len(first_record.handshakes):])
    save_record(out / "first" / "requests.json", first_record)
    save_record(out / "second" / "requests.json", second_record)
    (out / "profile.json").write_text(json.dumps({
        "files": {name: (data.hex() if data is not None else None) for name, data in files_after_first.items()},
        "history_first": history_first, "history_second": history_second}, indent=1), encoding="utf-8")
    return verdict(out, {"ok": ok_first, "text": text_first}, {"ok": ok_second, "text": text_second},
                   (first_record, second_record),
                   {"files": files_after_first, "history_first": history_first, "history_second": history_second})


if __name__ == "__main__":
    sys.exit(main())
