#!/usr/bin/env python3
"""The account attack harness (UT of docs/accounts-plan.md, issue #626).

Boots a desktop image, runs a list of attacks from the Terminal as the session
user, powers off, boots again and audits the OS volume from the host. Boots,
all on a COPY of the image (`shots/accounts/work/lazyos.img`; the built image
is never booted):

1. **warm**: first boot (the core packages install), clean power-off. Its
   volume is the baseline of the audit.
2. **attack**: each scenario of `attack_judge.EXPECTATIONS` once from the
   Terminal (`assets/accounts/attack.sh`, two rhai scripts), then a clean
   power-off, judged like a shutdown (`boot_judge.judge_stop`). The volume is
   audited against the baseline.
3. **verify**: boots again and must answer a command; the session then ends
   with the machine running, a hard kill.
4. **verify-kill**: boots after the hard kill and must answer again.

The verdict: every scenario meets its expectation (`attack_judge`: `blocked`,
or `xfail` for what a later phase closes, which passes while it SUCCEEDS), the
machine comes back, and no file outside `audit.ALLOWED` changed (except what an
open attack is declared to touch, or a scenario changes by allowed means). Since
U0 (#623) the desktop session is `user` (the image logs it straight in,
`LAZYOS_AUTOLOGIN`): its scenarios are `blocked`; the quota ones stay `xfail`
for U3. `autostart_root` is judged from the verify boot: the package the attack
session installed must open at that login as `user`.

    python tools/accounts/run.py              # build, boot four times, judge
    python tools/accounts/run.py --no-build   # reuse target/lazyos.img (built with this script's assets)
    python tools/accounts/run.py --quick      # skip the hard-kill boot
    python tools/accounts/run.py --prebuilt-apps  # CI: the xui apps are already in target/
    python tools/accounts/run.py --accel none # force TCG

The image needs BusyBox (`tools/abi/busybox.py`), the xui apps and `rhai`
(built here unless `--no-build`). Exit status is non-zero on any failure.
Screenshots, serial logs, `audit_*.txt` land in `shots/accounts/`.
"""

from __future__ import annotations

import argparse
import json
import os
import shutil
import subprocess
import sys
import time
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent.parent
HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))
sys.path.insert(0, str(ROOT / "tools"))

import attack_judge  # noqa: E402
import audit  # noqa: E402
import boot_judge  # noqa: E402
import probe_packages  # noqa: E402
import prompt_judge  # noqa: E402

IMAGE = ROOT / "target/lazyos.img"
BUSYBOX = ROOT / "target/abi/busybox/busybox"
ASSETS = HERE / "assets"
#: The probe packages' asset tree, generated at build time (probe_packages.py).
GENERATED = ROOT / "target" / "accounts-assets"
#: Where the guest scripts land (`assets/manifest.txt`: /system/share/<path>).
GUEST = "/system/share/accounts"

#: The attack session's commands, in order (attack.sh or a rhai script each).
SHELL_ATTACKS = ["uid", "rm_system", "overwrite_init", "write_conf", "read_conf_store",
                 "read_home_admin",
                 "signal_service", "autostart_pkg", "core_replace", "fork_bomb", "disk_fill"]
RHAI_ATTACKS = {name: f"{name}.rhai" for name in (
    "confd_sys", "keyd_provision",
    # U1 (#624): accounts change only through elevd; Authenticate is slowed.
    "acct_create", "acct_delete", "acct_promote", "acct_password", "keyd_forget", "keyd_verify",
    "auth_flood",
    # U2 (#625): the privileged paths answer elevd alone; the prompt is elevd's.
    "direct_time", "direct_zone", "direct_restart", "prompt_spoof", "input_focus",
    "display_read")}
#: A step that only prepares a scenario prints this marker instead.
SETUP_MARKERS = {"autostart_pkg": "TERM:OUT:ACCT:INSTALL:autostart_pkg:"}


def command_for(name: str) -> str:
    if name in RHAI_ATTACKS:
        return f"rhai {GUEST}/{RHAI_ATTACKS[name]}"
    return f"sh {GUEST}/attack.sh {name}"


def build(apps: bool = True) -> bool:
    """`rhai`, the xui apps (unless `apps` is false: CI hands them over
    built, `target/xui` and `target/pkg`), the probe packages and the image."""
    if not BUSYBOX.is_file():
        print(f"missing {BUSYBOX}: run tools/abi/busybox.py (the Terminal needs sh)")
        return False
    import demo_builds
    demo_builds.build_rhai()
    # The session logs `user` straight in (LAZYOS_AUTOLOGIN, issue #623);
    # the verify boot clicks the Terminal by name (LAZYOS_UI_PROBE).
    env = dict(os.environ, LAZYOS_DESKTOP="1", LAZYOS_XUI_AUTOSTART="term",
               LAZYOS_AUTOLOGIN="user", LAZYOS_UI_PROBE="1", LAZYOS_RESET_OS="1",
               LAZYOS_ASSETS=os.pathsep.join([str(ASSETS), str(GENERATED)]))
    if apps and subprocess.run([sys.executable, "tools/xui/build.py"], cwd=ROOT,
                               env=env).returncode:
        return False
    problems = probe_packages.build_all(GENERATED)
    for problem in problems:
        print(f"probe packages: {problem}")
    return not problems and subprocess.run(["cargo", "build"], cwd=ROOT, env=env).returncode == 0


# ---- session scripts -------------------------------------------------------

def home(start: float) -> list[dict]:
    """Park the pointer in the top-left corner (moves are relative and clamped)."""
    return [{"at": start + 0.2 * n, "mouse_move": [-300, -300]} for n in range(6)]


def focus_terminal() -> list[dict]:
    """Wait for the desktop and the core package install, click into the
    Terminal, shorten the prompt (a wrapped command line would be reported
    instead of its output) and run one command to prove it answers."""
    return [
        {"wait_for": "PKGD:PROVISION:DONE", "timeout": 420},
        {"wait_for": "TERM:UP:PASS", "timeout": 240},
        {"wait_for": "INIT:AUTOSTART:PASS app=terminal", "timeout": 60},
        *home(2.0),
        {"at": 3.4, "mouse_move": [400, 58]},
        {"at": 3.8, "mouse_click": "left"},
        {"at": 5.0, "type": "PS1='# '"},
        {"at": 5.5, "key": "enter", "until": "TERM:CMD:PS1='# '", "timeout": 30, "retries": 2},
        *command("echo ACCT:BOOT:OK", boot_judge.BOOT_OK),
    ]


def command(text: str, until: str, timeout: int = 60) -> list[dict]:
    return [{"at": 1.0, "type": text},
            {"at": 0.5, "key": "enter", "until": until, "timeout": timeout, "retries": 2}]


def power_off() -> list[dict]:
    return [*command("shutdown", "INIT:SHUTDOWN:BEGIN", 30),
            {"wait_for": "power: filesystems synced", "timeout": 120}]


def warm_session() -> list[dict]:
    return [*focus_terminal(), *power_off()]


def prompt_steps() -> list[dict]:
    """The trusted prompt (U2), judged by `prompt_judge`:

    * `prompt_over`: elevd asks, a window opens over the prompt; screenshots
      before and after the window, then Escape. The Counter then has the
      focus: click the Terminal again by name (`LAZYOS_UI_PROBE`).
    * `prompt_keys`: the Terminal has the focus; type into the prompt, Enter,
      Escape. A key that reached the Terminal shows as `TERM:CMD:inject`.
    * `input_flood`: the same while inputd's shared endpoint is flooded; the
      prompt may also refuse to open (then the typing reaches the Terminal,
      harmlessly: no prompt asked for a password).
    * `prompt_flood`: the first prompt is cancelled, the next requests must be
      refused without one (the script judges).

    Each request first waits out the hold the previous cancel left (elevd,
    review of #659 H4), so the gates allow for it."""
    def typed_at_prompt(name: str, tag: str, text: str, opened: str) -> list[dict]:
        return [
            {"at": 1.0, "type": f"sh {GUEST}/attack.sh {name}"},
            {"at": 0.5, "key": "enter", "until": opened, "regex": True, "timeout": 180},
            {"at": 1.5, "type": text},
            {"at": 0.5, "key": "enter"},
            {"at": 0.5, "key": "esc"},
            {"wait_for": f"TERM:OUT:ACCT:PROMPT:{tag}:", "timeout": 180},
        ]
    return [
        {"at": 1.0, "type": f"sh {GUEST}/attack.sh prompt_over"},
        {"at": 0.5, "key": "enter", "until": "XUID:PROMPT:UP", "timeout": 120, "retries": 1},
        {"at": 1.0, "shot": "prompt_up"},
        {"wait_for": "XUIAPP:COUNTER:PASS", "timeout": 120},
        {"at": 3.0, "shot": "prompt_window"},
        {"at": 0.5, "key": "esc", "until": "XUID:PROMPT:DONE", "timeout": 60, "retries": 1},
        {"wait_for": "TERM:OUT:ACCT:PROMPT:over:", "timeout": 120},
        {"at": 2.0, "click_at": {"window": "Terminal", "offset": [250, 150]}, "timeout": 60},
        # The first key after clicking back into the Terminal from another
        # window is lost (with or without the prompt): spend it on End.
        {"at": 1.0, "key": "end"},
        *typed_at_prompt("prompt_keys", "keys", prompt_judge.TYPED, "XUID:PROMPT:UP"),
        *typed_at_prompt("input_flood", "flood", prompt_judge.FLOOD_TYPED,
                         "XUID:PROMPT:(UP|REFUSED)"),
        {"at": 1.0, "type": f"sh {GUEST}/attack.sh prompt_flood"},
        {"at": 0.5, "key": "enter", "until": "XUID:PROMPT:UP", "timeout": 180},
        {"at": 1.5, "key": "esc", "until": "TERM:OUT:ACCT:ATTACK:prompt_flood:", "timeout": 240},
    ]


def lockout_steps() -> list[dict]:
    """`admin_lockout` (review of #659, H5): the session floods
    `Authenticate("admin", ...)` in the background while elevd asks for an
    administrator; admin's right password is typed into the prompt and the
    request must be granted (the flood counts against the session alone).
    It runs after `prompt_steps`, whose judge reads the log's first prompt."""
    return [
        {"at": 1.0, "type": f"sh {GUEST}/attack.sh admin_lockout"},
        {"at": 0.5, "key": "enter", "until": "XUID:PROMPT:UP", "timeout": 120, "retries": 1},
        {"at": 2.0, "type": "admin"},
        {"at": 0.3, "key": "tab"},
        {"at": 0.3, "type": "nimda"},
        {"at": 0.3, "key": "ret", "until": "TERM:OUT:ACCT:ATTACK:admin_lockout:",
         "timeout": 240},
    ]


def attack_session(names: list[str]) -> list[dict]:
    steps = focus_terminal()
    for name in names:
        until = SETUP_MARKERS.get(name, f"TERM:OUT:ACCT:ATTACK:{name}:")
        steps += command(command_for(name), until, timeout=180)
    return [*steps, *prompt_steps(), *lockout_steps(), *power_off()]


def verify_session() -> list[dict]:
    """Up and answering, then the session ends with the machine running. The
    package `autostart_root` installed opens at this login (as `user`) and,
    whichever opened first, may cover part of the Terminal: raise the Terminal
    by name (`LAZYOS_UI_PROBE`) at the bottom right of its content, which the
    smaller window never reaches, then type without clicking again."""
    raise_terminal = [{"wait_for": f"INIT:AUTOSTART:PASS app={attack_judge.AUTOPROBE}",
                       "timeout": 420},
                      {"wait_for": "XUIAPP:COUNTER:PASS", "timeout": 60},
                      {"wait_for": "TERM:UP:PASS", "timeout": 240},
                      {"at": 1.0, "click_at": {"window": "Terminal", "offset": [250, 150]},
                       "timeout": 60}]
    typing = [step for step in focus_terminal()
              if "mouse_move" not in step and "mouse_click" not in step]
    return [*raise_terminal, *typing, {"at": 2.0, "shot": "up"}]


def boot(name: str, steps: list[dict], out: Path, image: Path, accel: str,
         memory: str | None) -> tuple[bool, str]:
    """Run one session on `image`; returns whether it completed and its serial log."""
    out.mkdir(parents=True, exist_ok=True)
    script = out / f"{name}.json"
    script.write_text(json.dumps(steps, indent=1), encoding="utf-8")
    session = out / name
    started = time.time()
    result = subprocess.run([sys.executable, "tools/screenshot/qemu_session.py",
                             "--image", str(image), "--accel", accel, "--out", str(session),
                             "--script", str(script), "--extra-arg=-no-shutdown"]
                            + (["--memory", memory] if memory else []),
                            cwd=ROOT, capture_output=True, text=True)
    print(f"{name}: session {'ok' if result.returncode == 0 else 'FAILED'} "
          f"in {time.time() - started:.0f} s", flush=True)
    if result.returncode != 0:
        print(result.stderr[-3000:])
    log_path = session / "serial.log"
    log = log_path.read_text(encoding="utf-8", errors="replace") if log_path.is_file() else ""
    return result.returncode == 0, log


def snapshot(image: Path, out: Path, name: str) -> dict[str, audit.Node]:
    text = audit.read_tree(image)
    (out / f"audit_{name}.txt").write_text(text, encoding="utf-8")
    return audit.parse(text)


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__,
                                     formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--no-build", action="store_true")
    parser.add_argument("--prebuilt-apps", action="store_true",
                        help="build rhai, the probe packages and the image, but take the xui "
                             "apps and core packages as they are in target/ (CI)")
    parser.add_argument("--quick", action="store_true", help="skip the hard-kill boot")
    parser.add_argument("--accel", default="auto")
    parser.add_argument("--memory", help="guest RAM (default: the session tool's, 1G)")
    parser.add_argument("--out", type=Path, default=ROOT / "shots/accounts")
    args = parser.parse_args()
    if not args.no_build and not build(apps=not args.prebuilt_apps):
        return 1
    if not IMAGE.is_file():
        print(f"missing {IMAGE}")
        return 1
    args.out.mkdir(parents=True, exist_ok=True)
    work = args.out / "work" / "lazyos.img"
    work.parent.mkdir(parents=True, exist_ok=True)
    shutil.copyfile(IMAGE, work)  # the built image is never booted
    failures: list[str] = []
    names = SHELL_ATTACKS + list(RHAI_ATTACKS)

    ok, log = boot("warm", warm_session(), args.out, work, args.accel, args.memory)
    failures += [] if ok else ["warm session did not complete"]
    failures += [f"warm: {f}" for f in boot_judge.judge_stop(log)]
    baseline = snapshot(work, args.out, "baseline")
    if not baseline:
        failures.append("audit: could not read the baseline volume with osread")

    ok, attack_log = boot("attack", attack_session(names), args.out, work, args.accel,
                          args.memory)
    failures += [] if ok else ["attack session did not complete"]
    failures += boot_judge.judge_stop(attack_log)
    after = snapshot(work, args.out, "after_attacks")

    ok, log = boot("verify", verify_session(), args.out, work, args.accel, args.memory)
    failures += [] if ok else ["verify session did not complete"]
    failures += boot_judge.judge_boot(log, "verify")
    # The package the attack session installed opened at this login: as whom?
    verdict = attack_judge.judge(attack_log + "\n" + attack_judge.autostart_marker(attack_log, log)
                                 + "\n" + prompt_judge.markers(attack_log, args.out / "attack"))
    failures += verdict.failures
    excused = [path for name in verdict.open_attacks
               for path in attack_judge.EXPECTATIONS[name].touches]
    excused += attack_judge.side_effects()
    if not args.quick:
        ok, log = boot("verify-kill", verify_session(), args.out, work, args.accel, args.memory)
        failures += [] if ok else ["verify-kill session did not complete"]
        failures += boot_judge.judge_boot(log, "verify-kill", after_hard_kill=True)
    final = snapshot(work, args.out, "after_reboots")

    audit_fail, audit_notes = [], []
    for label, tree in (("attacks", after), ("reboots", final)):
        fails, notes = audit.judge(audit.diff(baseline, tree), excused)
        audit_fail += [f"{f} [after {label}]" for f in fails]
        audit_notes += notes
    failures += audit_fail if baseline else []

    for note in verdict.notes + sorted(set(audit_notes)):
        print(f"note: {note}")
    for failure in failures:
        print(f"FAIL: {failure}")
    print("ACCOUNTS: PASS" if not failures else f"ACCOUNTS: {len(failures)} failure(s)")
    return 1 if failures else 0


if __name__ == "__main__":
    sys.exit(main())
