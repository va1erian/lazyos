#!/usr/bin/env python3
"""keyd's named secrets, end to end (docs/wifi-prerequisites-plan.md WP2).

Boots a desktop image three times on a COPY of `target/lazyos.img` and drives
the Terminal as the session user (`user`):

1. **first**: store a user secret through `rhai` (`sys::keyd::store_secret`),
   list it, see bad input refused, see `WifiPmk` refused (`EPERM`: it is
   `wlanmd`'s alone, whichever uid it names), see a `system` store refused
   without `elevd`, then store one through `elevd` (`net.wifi.system`,
   approved at the trusted prompt as `admin`/`nimda`) and list it. Power off
   through `init`. The host then reads the volume: `/conf/svc/keyd/secrets`
   and `machine.key` are root's 0600 files, and no secret is in the clear.
2. **second**: `keyd` loads both secrets (`KEYD:SECRETS:PASS count=2`), the
   lists still show them, `WifiPmk` is still refused; delete the user secret
   directly and the system one through `elevd`.
3. **third**: both lists are empty after another reboot.

The positive `WifiPmk` path (the Annex J.4 vector, answered to uid 912
only) is covered by the host tests in `libs/secretstore`: nothing in the
image runs as `_wlan` until `wlanmd` exists.

    python tools/keyd/run.py              # build, boot three times, judge
    python tools/keyd/run.py --no-build   # reuse target/lazyos.img (built by this script)
    python tools/keyd/run.py --accel none # force TCG
    python tools/keyd/test_judge.py       # the judge fails when it should

Needs BusyBox (`tools/abi/busybox.py`), the xui apps and `rhai` (built here
unless `--no-build`). Output: `shots/keyd/`.
"""

from __future__ import annotations

import argparse
import importlib.util
import os
import shutil
import subprocess
import sys
from pathlib import Path

HERE = Path(__file__).resolve().parent
ROOT = HERE.parent.parent
sys.path.insert(0, str(HERE))
sys.path.insert(0, str(ROOT / "tools"))
sys.path.insert(0, str(ROOT / "tools/accounts"))

import audit  # noqa: E402  (tools/accounts: the volume listing)
import keyd_judge as judge  # noqa: E402

# The accounts harness already knows how to focus the Terminal, type a
# command, power off and run one session; reuse it rather than copy it.
_spec = importlib.util.spec_from_file_location("accounts_run", ROOT / "tools/accounts/run.py")
accounts_run = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(accounts_run)

IMAGE = ROOT / "target/lazyos.img"
ASSETS = HERE / "assets"
GUEST = "/system/share/keyd/secrets.rhai"
#: What the trusted prompt takes: the development administrator's name,
#: Tab, password, Enter (docs/accounts-plan.md U2).
ADMIN, ADMIN_PASSWORD = "admin", "nimda"


def build() -> bool:
    if not any(path.is_file() for path in accounts_run.BUSYBOX):
        print(f"missing {accounts_run.BUSYBOX[1]}: run tools/abi/busybox.py")
        return False
    import demo_builds
    demo_builds.build_rhai()
    env = dict(os.environ, LAZYOS_DESKTOP="1", LAZYOS_XUI_AUTOSTART="term",
               LAZYOS_AUTOLOGIN="user", LAZYOS_UI_PROBE="1", LAZYOS_RESET_OS="1",
               LAZYOS_ASSETS=str(ASSETS))
    return (subprocess.run([sys.executable, "tools/xui/build.py"], cwd=ROOT, env=env).returncode == 0
            and subprocess.run(["cargo", "build"], cwd=ROOT, env=env).returncode == 0)


def step(name: str, prompt: bool = False) -> list[dict]:
    """Run one guest step and wait for its line; an `elevd` step answers the
    trusted prompt as the administrator first."""
    until = f"TERM:OUT:KEYD:T:{name}:"
    if not prompt:
        return accounts_run.command(f"rhai {GUEST} {name}", until, timeout=180)
    return [
        {"at": 1.0, "type": f"rhai {GUEST} {name}"},
        {"at": 0.5, "key": "enter", "until": "XUID:PROMPT:UP", "timeout": 120, "retries": 1},
        {"at": 2.0, "type": ADMIN},
        {"at": 0.3, "key": "tab"},
        {"at": 0.3, "type": ADMIN_PASSWORD},
        {"at": 0.3, "key": "ret", "until": until, "timeout": 240},
    ]


def session(steps: list[tuple[str, bool]]) -> list[dict]:
    script = accounts_run.focus_terminal()
    for name, prompt in steps:
        script += step(name, prompt)
    # The flusher runs every 5 s; `shutdown` goes through init and syncs.
    return script + accounts_run.power_off()


SESSIONS = {
    "first": session([("user_store", False), ("user_list", False), ("bad_input", False),
                      ("pmk_denied", False), ("pmk_other", False), ("system_denied", False),
                      ("system_list", False), ("system_store", True), ("system_list", False)]),
    "second": session([("user_list", False), ("system_list", False), ("pmk_denied", False),
                       ("user_delete", False), ("user_list", False), ("system_delete", True),
                       ("system_list", False)]),
    "third": session([("user_list", False), ("system_list", False)]),
}


def osread(image: Path, command: str, path: str) -> bytes:
    result = subprocess.run(["cargo", "run", "-q", "-p", "ext2fs", "--example", "osread", "--",
                             str(image), command, path], cwd=ROOT, capture_output=True)
    return result.stdout if result.returncode == 0 else b""


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__,
                                     formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--no-build", action="store_true")
    parser.add_argument("--accel", default="auto")
    parser.add_argument("--memory")
    parser.add_argument("--out", type=Path, default=ROOT / "shots/keyd")
    args = parser.parse_args()
    if not args.no_build and not build():
        return 1
    if not IMAGE.is_file():
        print(f"missing {IMAGE}")
        return 1
    args.out.mkdir(parents=True, exist_ok=True)
    work = args.out / "work" / "lazyos.img"
    work.parent.mkdir(parents=True, exist_ok=True)
    shutil.copyfile(IMAGE, work)  # the built image is never booted

    failures: list[str] = []
    for name, steps in SESSIONS.items():
        ok, log = accounts_run.boot(name, steps, args.out, work, args.accel, args.memory)
        if not ok:
            failures.append(f"{name}: the session did not complete")
        failures += judge.judge_boot(name, log)
        if "power: filesystems synced" not in log:
            failures.append(f"{name}: no clean power-off")
        if name == "first":
            tree = audit.parse(audit.read_tree(work))
            failures += judge.judge_files(
                tree, osread(work, "cat", "/conf/svc/keyd/secrets"),
                osread(work, "cat", "/conf/svc/keyd/machine.key"))
    for failure in failures:
        print(f"FAIL: {failure}")
    print("KEYD:SECRETS: PASS" if not failures else f"KEYD:SECRETS: {len(failures)} failure(s)")
    return 1 if failures else 0


if __name__ == "__main__":
    sys.exit(main())
