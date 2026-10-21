#!/usr/bin/env python3
"""Build the desktop with the developer tools, boot it, drive the Messenger Playground, judge it.

One command, the guest half of docs/messenger-playground-plan.md (P1 to P3, D7):

1. `tools/rhai/build.py` (the `rhai` command the runs start), BusyBox, `tools/xui/build.py` (the
   app and its core package);
2. `cargo build` of `LAZYOS_DESKTOP=1 LAZYOS_DEVTOOLS=1 LAZYOS_UI_PROBE=1
   LAZYOS_XUI_AUTOSTART=playground LAZYOS_AUTOLOGIN=user LAZYOS_LABEL_TRACE=1 LAZYOS_RESET_OS=1`
   (what `run_demo.py --devtools --autologin user` sets, plus the probe, the autostart and the
   label trace, which prints every refused call as `LABEL:DENY`);
3. one headless QEMU session per script in `tools/screenshot/examples/playground_*.json`:
   `browse` (search, a method's doc, the Live, Topics and Gallery tabs, Help), `run` (a script,
   the Call form, a refused call, Stop), `serve` (example 12 served by one run and called by
   another), `listen` (time/tick, system/stats/memory, a publish to demo/pg/x), `elevd` (example 11b: the trusted prompt names the tool, an administrator approves), `review` (the
   strip, a pasted script that asks before its first Run, nothing runs on open);
4. `judge.py` over each serial log (the `tool:` label installed and the app started under it,
   `PLAYGROUND:*` markers, no refused call of the app's own, no `rhai` left running) and over the
   screenshots (`pngstats`). Read the screenshots in `shots/playground/<session>/`.

    python tools/playground/run.py                 # build, then every session
    python tools/playground/run.py --no-build      # reuse target/lazyos.img
    python tools/playground/run.py --only run,listen
    python tools/playground/run.py --only notice    # leaves the notice dismissed: follow with notice_stored
    python tools/playground/run.py --accel none    # force TCG

Exit status is non-zero on any failure. `python tools/playground/test_judge.py` tests the judge.
"""

from __future__ import annotations

import argparse
import os
import subprocess
import sys
from pathlib import Path

HERE = Path(__file__).resolve().parent
ROOT = HERE.parent.parent
sys.path.insert(0, str(HERE))
sys.path.insert(0, str(ROOT / "tools" / "abi"))
import judge  # noqa: E402

PY = sys.executable
SESSION = ROOT / "tools" / "screenshot" / "qemu_session.py"
EXAMPLES = ROOT / "tools" / "screenshot" / "examples"
IMAGE = ROOT / "target" / "lazyos.img"

#: The image every session boots (a developer-tool desktop with the probe and the label trace).
BUILD_ENV = {
    "LAZYOS_UI_PROBE": "1",
    "LAZYOS_XUI_AUTOSTART": "playground",
    "LAZYOS_LABEL_TRACE": "1",
    "LAZYOS_RESET_OS": "1",
}

#: session -> (script, the screenshots it takes, in order)
SESSIONS = {
    "browse": ("playground_browse.json", ["browse_initial", "browse_search", "browse_method_doc", "browse_inserted",
                                          "browse_live", "browse_topics", "browse_gallery", "browse_help"]),
    "run": ("playground_run.json", ["run_hello", "run_form", "run_call_reply", "run_call_args", "run_refused",
                                    "run_serving", "run_stopped"]),
    "serve": ("playground_serve.json", ["serve_loaded", "serve_serving", "serve_answered",
                                        "serve_live", "serve_stopped", "serve_serving_again"]),
    "listen": ("playground_listen.json", ["listen_peek", "listen_tick", "listen_memory", "listen_published", "listen_controls"]),
    "elevd": ("playground_elevd.json", ["elevd_prompt", "elevd_done"]),
    "review": ("playground_review.json", ["review_idle", "review_card", "review_cancelled",
                                          "review_ran", "review_second_run", "review_save_dialog",
                                          "review_saved", "review_open_dialog", "review_file_card",
                                          "review_typing_deleted", "review_bypass_card"]),
    # These two last and in this order: the first leaves the notice dismissed (the choice is
    # stored on the volume), the second boots with it hidden and restores it.
    "notice": ("playground_notice.json", ["notice_shown", "notice_hidden"]),
    "notice_stored": ("playground_notice_stored.json", ["notice_stored_hidden", "notice_stored_shown"]),
    # The layout is stored on the volume too: `layout` drags the dividers, `layout_stored` boots with them.
    "layout": ("playground_layout.json", ["layout_initial", "layout_hidden", "layout_shown", "layout_explorer_narrow",
                                          "layout_doc_short", "layout_run_reveals"]),
    "layout_stored": ("playground_layout_stored.json", ["layout_stored", "layout_pulled"]),
}


def fail(message: str) -> int:
    print(f"PLAYGROUND:HARNESS:FAIL: {message}", file=sys.stderr)
    return 1


def build(args: argparse.Namespace) -> bool:
    """rhai, the xui apps, then the image (`run_demo.py --devtools` is the same build).

    The OS volume is recreated every time: `LAZYOS_RESET_OS=1` alone does nothing when no input of
    the build changed since the last one (cargo only reruns the image script on a change, and
    `build_support/os_track.rs` keeps an unchanged image), which would leave the last run's stored
    layout and dismissed notice on the volume. Bumping `build.rs`'s mtime makes cargo run the script
    again, and with the reset set it formats a new volume.
    """
    os.utime(ROOT / "build.rs")
    steps = [
        [PY, str(ROOT / "tools" / "rhai" / "build.py")],
        [PY, str(ROOT / "tools" / "xui" / "build.py")],
    ]
    for step in steps:
        print(f"playground: {' '.join(step[1:])}", flush=True)
        if subprocess.run(step, cwd=ROOT, capture_output=True, text=True).returncode != 0:
            return False
    env = dict(os.environ, **BUILD_ENV)
    command = [PY, str(ROOT / "tools" / "run_demo.py"), "--devtools", "--autologin", "user",
               "--build-only"]
    print("playground: " + " ".join(f"{k}={v}" for k, v in BUILD_ENV.items()) + " run_demo --devtools",
          flush=True)
    result = subprocess.run(command, cwd=ROOT, env=env, capture_output=True, text=True)
    if result.returncode != 0:
        sys.stderr.write(result.stderr[-4000:])
        return False
    return IMAGE.is_file()


def run_session(name: str, args: argparse.Namespace) -> Path:
    """Boot the image and drive one session; the directory holding its serial log and shots."""
    script, _ = SESSIONS[name]
    out = ROOT / "shots" / "playground" / name
    command = [
        PY, str(SESSION), "--image", str(IMAGE), "--out", str(out), "--timeout", str(args.timeout),
        "--script", str(EXAMPLES / script), "--accel", args.accel,
        "--fail-on", r"PLAYGROUND:(BIND|RUN):FAIL", "--fail-on", "LazyOS PANIC",
    ]
    if args.qemu:
        command += ["--qemu", args.qemu]
    if args.memory:
        command += ["--memory", args.memory]
    print(f"playground: session {script}", flush=True)
    code = subprocess.call(command, cwd=ROOT)
    if code != 0:
        print(f"playground: {script} exited with {code}", file=sys.stderr)
    return out


def judge_session(name: str, out: Path, names: dict[int, str], first_boot: bool) -> list[str]:
    log = out / "serial.log"
    if not log.is_file():
        return [f"{name}: no serial log"]
    text = log.read_text(errors="replace")
    problems = judge.judge_serial(name, text, names, first_boot)
    problems += judge.judge_shots(out, SESSIONS[name][1])
    for marker in judge.markers(text):
        print(f"  {judge.P}:{marker}")
    for label, what in judge.denials(text):
        print(f"  LABEL:DENY label={label} {what}")
    return [f"{name}: {p}" for p in problems]


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--no-build", action="store_true", help="reuse target/lazyos.img")
    parser.add_argument("--only", help="comma-separated session names")
    parser.add_argument("--list", action="store_true", help="list the sessions and exit")
    parser.add_argument("--accel", default="auto", choices=["auto", "none", "tcg", "whpx", "kvm"])
    parser.add_argument("--timeout", type=int, default=900, help="seconds a session may take")
    parser.add_argument("--qemu", help="path to qemu-system-x86_64")
    parser.add_argument("--memory", help="guest RAM (default: the tools' 1G)")
    args = parser.parse_args(argv)
    if args.list:
        for name, (script, shots) in SESSIONS.items():
            print(f"{name}: {script} ({len(shots)} screenshots)")
        return 0
    chosen = args.only.split(",") if args.only else list(SESSIONS)
    unknown = [n for n in chosen if n not in SESSIONS]
    if unknown:
        return fail(f"unknown session {unknown} (try --list)")
    if "layout_stored" in chosen and "layout" not in chosen:
        chosen.insert(chosen.index("layout_stored"), "layout")
    if "notice_stored" in chosen and "notice" not in chosen:
        # It restores what `notice` dismissed (the choice lives on the volume).
        chosen.insert(chosen.index("notice_stored"), "notice")
    if not args.no_build and not build(args):
        return fail("the build failed")
    if not IMAGE.is_file():
        return fail(f"{IMAGE} does not exist (build first)")
    names = judge.interface_names()
    problems: list[str] = []
    for number, name in enumerate(chosen):
        out = run_session(name, args)
        found = judge_session(name, out, names, first_boot=number == 0 and not args.no_build)
        print(f"playground: {name}: {'PASS' if not found else 'FAIL'}", flush=True)
        problems += found
    for problem in problems:
        print(f"PLAYGROUND:HARNESS:FAIL: {problem}", file=sys.stderr)
    if not problems:
        print("PLAYGROUND:HARNESS:PASS")
    return 1 if problems else 0


if __name__ == "__main__":
    raise SystemExit(main())
