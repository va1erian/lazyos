#!/usr/bin/env python3
"""Attack the browser's sandbox from inside it, and judge what the kernel let through
(issue #832, docs/architecture/process-sandbox.md; in the style of ``tools/accounts/run.py``).

``tools/abi/fixtures/src/sandboxprobe.rs`` plays an exploited WebProcess and an exploited
NetworkProcess: from inside their labels it tries to read the user's files and the system's,
write them, open a socket, connect to a named socket, resolve the display, execute BusyBox and
a native program, fork, signal its parent and call ``ptrace``. Every attempt must be
**blocked** by the policy (``EPERM``/``EACCES`` and a ``LABEL:DENY`` line of the kernel's
label trace), and the few things the process needs (fonts for the page process; the resolver,
the CA bundle, sockets and its profile folder for the network process) must still work.
The judge is ``attackjudge.py`` (self-test: ``test_attack_judge.py``).

Two modes:

``--kernel`` (default)
    A console image whose first program is the probe's *driver*: it loads the rules of the
    browser's two process tables as the labels ``app:os.lazy.sandbox-web`` and
    ``app:os.lazy.sandbox-net`` (what ``pkgd`` does), starts the probe as each under the
    user's uid and waits. It tests the kernel's gates with real programs; it needs neither the
    package manager's child-label support nor a WebKit build.

``--browser``
    The real browser's UI process (``os.lazy.lazywebplus``) starts the two stand-ins as it starts the
    engine: the labels are the ones the package manifest's ``[processes.*]`` tables name
    (``app:os.lazy.lazywebplus/webprocess``, ``.../networkprocess``) and the kernel stamps at
    ``execve``. It needs ``target/xui/xui-lazywebplus.elf`` (``python tools/webkit/app.py``,
    ``python tools/xui/build.py``) and the package manager and kernel of the child-label work.

    python tools/webkit/attack_run.py                  # kernel mode: build, boot, judge
    python tools/webkit/attack_run.py --browser        # the real UI process starts the stand-ins
    python tools/webkit/attack_run.py --no-build       # the image of the last run
    python tools/webkit/attack_run.py --judge-only DIR # judge a saved serial.log

Exit status is non-zero on any breach. Logs go to ``shots/webkit-attack``. A build uses
``LAZYOS_RESET_OS=1``: **every run recreates ``target/lazyos.img``'s OS volume**.
"""

from __future__ import annotations

import argparse
import os
import subprocess
import sys
from pathlib import Path

HERE = Path(__file__).resolve().parent
ROOT = HERE.parent.parent
for sub in ("tools/abi", "tools"):
    sys.path.insert(0, str(ROOT / sub))
sys.path.insert(0, str(HERE))
import attack_build  # noqa: E402
import attackjudge  # noqa: E402
import busybox  # noqa: E402
from lazygui import limits  # noqa: E402

PY = sys.executable
IMAGE = ROOT / "target" / "lazyos.img"
OUT = ROOT / "shots" / "webkit-attack"
#: The browsers the harness can attack (``--app``): the package id (the label's prefix), the switch that
#: ships it, the window program and the app that autostarts. LazyWeb+ starts the two engine programs
#: under labels of its own (``app:os.lazy.lazywebplus/<name>``), and its network process alone may
#: write the Downloads folder (``attackjudge.expected``).
APPS = {
    "lazywebplus": ("os.lazy.lazywebplus", "LAZYOS_LAZYWEBPLUS", "xui-lazywebplus.elf", "lazywebplus"),
}
#: Switches a previous build may have left in the caller's environment.
STALE = ("LAZYOS_INIT", "LAZYOS_BUSYBOX_TEST", "LAZYOS_TLS_TEST_CA", "LAZYOS_TLS_TEST_HOSTS", "LAZYOS_CLI",
         "LAZYOS_XUI_APPS", "LAZYOS_WEBKIT_PORT", "LAZYOS_WEBKIT_DEPS", "LAZYOS_JSC",
         "LAZYOS_LINUXAPPS", "LAZYOS_TLS", "LAZYOS_LAZYWEBPLUS", "LAZYOS_WEBKIT_ATTACK",
         "LAZYOS_DESKTOP", "LAZYOS_WEBD")


def build_image(mode: str, app: str = "lazywebplus") -> str | None:
    """Build the test image for `mode` (and, in the browser's mode, browser `app`); an error message, or None."""
    _, switch, elf_name, autostart = APPS[app]
    elf = ROOT / "target" / "xui" / elf_name
    env = {k: v for k, v in os.environ.items() if k not in STALE}
    env.update(LAZYOS_LABEL_TRACE="1", LAZYOS_RESET_OS="1")
    if mode == "kernel":
        # The driver is the first program (`/system/bin/abi-init`), as for the ABI fixtures.
        env.update(LAZYOS_INIT=str(attack_build.OUT / "WebProcess"), LAZYOS_OS_SIZE="128M")
    else:
        if not elf.is_file():
            return f"{elf.relative_to(ROOT)} is not built (python tools/webkit/app.py, python tools/xui/build.py)"
        shell = busybox.ensure_busybox()
        if shell is None:
            return "no BusyBox for the shell (see tools/abi/busybox.py)"
        env.update({switch: "1"}, LAZYOS_DESKTOP="1", LAZYOS_WEBKIT_ATTACK="1", LAZYOS_NETD="1",
                   LAZYOS_NETD_ARGS="demo=0", LAZYOS_XUI_AUTOSTART=autostart, LAZYOS_UI_PROBE="1",
                   LAZYOS_BUSYBOX=str(shell),
                   **limits.limit_env(limits.with_defaults(list(limits.WEBKIT_LIMITS), limits.BIG_APP_LIMITS)))
    switches = " ".join(f"{k}={v}" for k, v in sorted(env.items()) if k.startswith("LAZYOS_") and k != "LAZYOS_BUSYBOX")
    print(f"attack: {switches} cargo build", flush=True)
    result = subprocess.run(["cargo", "build"], cwd=ROOT, env=env, capture_output=True, text=True)
    if result.returncode != 0:
        return "cargo build failed:\n" + result.stderr[-4000:]
    return None if IMAGE.is_file() else f"{IMAGE} was not built"


def boot(mode: str, seconds: int, args) -> str:
    """Boot the image headless, wait `seconds`, return the serial log."""
    OUT.mkdir(parents=True, exist_ok=True)
    log = OUT / "serial.log"
    log.unlink(missing_ok=True)
    command = [PY, str(ROOT / "tools" / "screenshot" / "qemu_shot.py"), "--out", str(OUT), "--at", str(seconds),
               "--accel", args.accel, "--image", str(IMAGE), "--memory", args.memory or ("4G" if mode == "browser" else "1G")]
    if args.qemu:
        command += ["--qemu", args.qemu]
    subprocess.run(command, cwd=ROOT, capture_output=True, text=True)
    return log.read_text(errors="replace") if log.is_file() else ""


def verdict(serial: str, mode: str, app: str = "lazywebplus") -> bool:
    """Print the report; True when nothing got through."""
    problems, rows = attackjudge.judge(serial, mode, APPS[app][0])
    if mode == "kernel":
        problems = attackjudge.driver_problems(serial) + problems
    for row in rows:
        print(f"  {row}")
    if attackjudge.trace_suppressed(serial):
        problems.append("the label trace dropped lines (rate limit): the denials cannot be matched")
    for problem in problems:
        print(f"ATTACK:HARNESS:FAIL {problem}")
    if not problems:
        print(f"ATTACK:HARNESS:PASS {sum(len(v) for v in attackjudge.EXPECTED.values())} probes, every one as expected")
    return not problems


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    parser.add_argument("--browser", action="store_true", help="the real UI process starts the stand-ins")
    parser.add_argument("--kernel", action="store_true", help="the console driver (the default)")
    parser.add_argument("--app", choices=sorted(APPS), default="lazywebplus",
                        help="with --browser: which browser's window and package labels to attack")
    parser.add_argument("--no-build", action="store_true", help="use the image of the last run")
    parser.add_argument("--judge-only", metavar="DIR", help="judge DIR/serial.log and stop")
    parser.add_argument("--seconds", type=int, help="how long to run the guest (default 60, 120 for the browser)")
    parser.add_argument("--accel", default="auto")
    parser.add_argument("--memory", default=None, help="guest RAM (default 1G, 4G for the browser)")
    parser.add_argument("--qemu", default=None)
    args = parser.parse_args()
    mode = "browser" if args.browser and not args.kernel else "kernel"

    if args.judge_only:
        log = Path(args.judge_only) / "serial.log"
        return 0 if verdict(log.read_text(errors="replace"), mode, args.app) else 1
    if not args.no_build:
        attack_build.main()
        error = build_image(mode, args.app)
        if error:
            print(f"ATTACK:HARNESS:FAIL {error}")
            return 1
    seconds = args.seconds or (120 if mode == "browser" else 60)
    serial = boot(mode, seconds, args)
    if not serial:
        print("ATTACK:HARNESS:FAIL no serial log (QEMU did not run)")
        return 1
    return 0 if verdict(serial, mode, args.app) else 1


if __name__ == "__main__":
    raise SystemExit(main())
