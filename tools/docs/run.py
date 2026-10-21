#!/usr/bin/env python3
"""Docs on a light view of webd, end to end (docs/xui-docs.md): build a desktop image with Docs and webd, boot it
twice and judge what the app and the screenshots show.

1. The apps (`tools/xui/build.py`, which packages Docs and webd) and an image with `LAZYOS_DESKTOP=1
   LAZYOS_WEBD=1 LAZYOS_XUI_AUTOSTART=docs`, the UI probe and the label trace. webd must be built first
   (`python tools/run_demo.py --webd`, or `tools/webkit/webd.py` in Docker): an image without it paints
   no page, and this harness fails.
2. `xui_docs.json`: the welcome page paints, the wheel scrolls it down and back up.
3. `xui_docs_open.json`: the Open dialog from the button and from Ctrl+O, a cancelled dialog, a typed path,
   the test document painted and scrolled.
4. The verdict (`judge.py`): the markers of both runs, no `FAIL` marker, no refused call of Docs or webd, and the
   pictures of the page.

    python tools/docs/run.py                 # build, boot twice, judge
    python tools/docs/run.py --no-build      # the image of the last run

Exit status is non-zero on any failure; logs and screenshots go to `shots/docs` and `shots/docs_open`; read the
`shot_*.png` of them.
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
import judge as j  # noqa: E402  (this directory's)

PY = sys.executable
IMAGE = ROOT / "target" / "lazyos.img"
SESSIONS = ROOT / "tools" / "screenshot" / "examples"
PREFIX = "DOCS"


def report(section: str, detail: str, problems: list[str]) -> bool:
    for problem in problems[:16]:
        print(f"{PREFIX}:HARNESS:{section}:FAIL {problem}")
    if not problems:
        print(f"{PREFIX}:HARNESS:{section}:PASS {detail}".rstrip())
    return not problems


def build(args) -> str | None:
    """Build the apps and the image; an error message, or None."""
    for need in (ROOT / "target" / "xui" / "xui-webd.elf", ROOT / "target" / "webkit" / "bin" / "WebProcess",
                 ROOT / "target" / "webkit" / "bin" / "NetworkProcess"):
        if not need.is_file():
            return (f"{need.relative_to(ROOT)} is not built: run `python tools/run_demo.py --webd --build-only` "
                    "(or tools/webkit/deps.py, port.py and webd.py)")
    steps = [[PY, "tools/xui/build.py"]]
    env = {k: v for k, v in os.environ.items() if not k.startswith("LAZYOS_")}
    env.update(LAZYOS_DESKTOP="1", LAZYOS_WEBD="1", LAZYOS_XUI_AUTOSTART="docs", LAZYOS_UI_PROBE="1",
               LAZYOS_RESET_OS="1", LAZYOS_AUTOLOGIN="user")
    if args.label_trace:
        env["LAZYOS_LABEL_TRACE"] = "1"
    steps.append(["cargo", "build"])
    for step in steps:
        print(f"docs: {' '.join(step)}", flush=True)
        done = subprocess.run(step, cwd=ROOT, env=env, capture_output=True, text=True)
        if done.returncode != 0:
            return f"{' '.join(step)} failed:\n" + (done.stdout + done.stderr)[-4000:]
    return None if IMAGE.is_file() else f"{IMAGE} was not built"


def session(script: str, out: Path, args) -> tuple[bool, str]:
    """Drive the guest through `script`; (the session finished, the serial log)."""
    out.mkdir(parents=True, exist_ok=True)
    for stale in ["serial.log", "summary.json", *(p.name for p in out.glob("shot_*.png"))]:
        (out / stale).unlink(missing_ok=True)
    command = [PY, str(ROOT / "tools" / "screenshot" / "qemu_session.py"), "--image", str(IMAGE),
               "--out", str(out), "--script", str(SESSIONS / script), "--accel", args.accel,
               "--memory", args.memory, "--timeout", str(args.timeout),
               "--fail-on", "PANIC", "--fail-on", "EXCEPTION", "--fail-on", "DOCS:[A-Z]+:FAIL",
               "--fail-on", "INIT:AUTOSTART:FAIL"]
    if args.qemu:
        command += ["--qemu", args.qemu]
    ok = subprocess.call(command, cwd=ROOT, stdout=subprocess.DEVNULL) == 0
    log = out / "serial.log"
    return ok, log.read_text(errors="replace") if log.is_file() else ""


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--no-build", action="store_true", help="reuse the image of the last run")
    parser.add_argument("--accel", default="auto", choices=["auto", "none", "tcg", "whpx", "kvm"])
    parser.add_argument("--qemu")
    parser.add_argument("--memory", default="2G", help="guest RAM")
    parser.add_argument("--timeout", type=int, default=600, help="seconds per session")
    parser.add_argument("--no-label-trace", dest="label_trace", action="store_false",
                        help="build without LAZYOS_LABEL_TRACE=1 (the label check then sees nothing)")
    parser.add_argument("--judge-only", action="store_true", help="judge shots/docs and shots/docs_open as they are")
    args = parser.parse_args(argv)
    wheel_out, open_out = ROOT / "shots" / "docs", ROOT / "shots" / "docs_open"
    ran_wheel = ran_open = True
    if args.judge_only:
        wheel_text = (wheel_out / "serial.log").read_text(errors="replace")
        open_text = (open_out / "serial.log").read_text(errors="replace")
    else:
        if not args.no_build:
            error = build(args)
            if error:
                print(f"{PREFIX}:HARNESS:FAIL {error}")
                return 1
        elif not IMAGE.is_file():
            print(f"{PREFIX}:HARNESS:FAIL --no-build needs the image of an earlier run ({IMAGE})")
            return 1
        ran_wheel, wheel_text = session("xui_docs.json", wheel_out, args)
        ran_open, open_text = session("xui_docs_open.json", open_out, args)
    ok = report("SESSION", "both sessions ran to their end",
                ([] if ran_wheel else ["xui_docs.json did not finish (see shots/docs/summary.json)"])
                + ([] if ran_open else ["xui_docs_open.json did not finish (see shots/docs_open/summary.json)"]))
    ok = report("MARKERS", "Docs and webd printed what each session needs",
                [f"wheel run: {p}" for p in j.judge_markers(wheel_text, j.WHEEL_MARKERS)]
                + [f"open run: {p}" for p in j.judge_markers(open_text, j.OPEN_MARKERS)]) and ok
    ok = report("LABEL", "no refused call of Docs or webd",
                j.judge_denies(wheel_text) + j.judge_denies(open_text)) and ok
    shot = lambda out, name: out / f"shot_{name}.png"  # noqa: E731
    try:
        ok = report("WHEEL", "the page scrolls down and back to the same picture",
                    j.judge_wheel(shot(wheel_out, "01_docs_top"), shot(wheel_out, "02_docs_wheel_down"),
                                  shot(wheel_out, "03_docs_wheel_back_up"))) and ok
        ok = report("OPENED", "the test document replaced the welcome page",
                    j.judge_opened(shot(open_out, "01_welcome_toolbar"), shot(open_out, "06_document_opened"))) and ok
        ok = report("SCROLLED", "the wheel scrolled the opened document",
                    j.judge_scrolled(shot(open_out, "06_document_opened"), shot(open_out, "07_scrolled"))) and ok
    except (OSError, ValueError) as error:
        ok = report("SHOTS", "", [f"cannot read a screenshot: {error}"]) and ok
    print(f"{PREFIX}:HARNESS:" + ("PASS" if ok else "FAIL"))
    return 0 if ok else 1


if __name__ == "__main__":
    sys.exit(main())
