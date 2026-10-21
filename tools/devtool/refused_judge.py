"""What the guest session `playground_refused_services.json` must show on serial.

A script run by a developer tool that is *not* the user's shell (the Messenger
Playground, label `tool:os.lazy.playground`) is refused the services that
guard the user: credentials, accounts, the package manager, the network stack
and file-server mounts (docs/devtool-packages-plan.md sections 3 and 9). The
session runs one call to each from a Playground script; this judge reads the
serial log of an image built with `LAZYOS_LABEL_TRACE=1` and fails unless

  * each kernel-refused interface (`devtool::DENY` entries that need no
    declaration: keyd, keyd.appsecret, accounts, net.stack, mount) produced a
    `LABEL:DENY label=tool:os.lazy.playground iface=<id>` line, the id taken
    from idl/manifest.json;
  * the script's two `net_stack` calls (`Resolve`, `Ping`) were each refused: the
    log needs `LABEL:DENY` lines for two different methods of that interface;
  * `pkgd` refused the install itself ("applications may not install or remove
    other applications": the package manager's own check, which the Terminal
    passes as the session owner);
  * nothing was created: no account, no mount daemon, no installed package;
  * the toast the second script posts was accepted as the Playground's own
    (`SHELL:NOTIFY:POST owner=os.lazy.playground `: another app's toast does not
    count) and not refused as coming from "not an app" (`SHELL:NOTIFY:DENY`);
  * both scripts ran to a normal end (`PLAYGROUND:RUN:<n>:EXIT:0:`), so a script
    that was stopped or failed before its calls proves nothing.

What the serial log cannot show: which *call* a `LABEL:DENY` line belongs to.
The kernel prints the interface and method of a refused call, never the
script's text (the Playground's markers carry only a hash), so one line per
interface is the proof for a script that calls the interface once, and the two
methods of `net.stack` are told apart by their method ids. That the script's own
`try/catch` printed "refused" (and not "ALLOWED") is in the output pane of the
`refused_services` screenshot, which a person reads; a script that printed
"ALLOWED" for a call the kernel had refused is not possible, because a refused
call raises.

Pure functions over the log text; `refused_run.py` calls them and
`test_refused_judge.py` proves they fail when they should. The log must not
have dropped lines (`serial_integrity`).
"""

from __future__ import annotations

import json
import re
import sys
from pathlib import Path

HERE = Path(__file__).resolve().parent
ROOT = HERE.parent.parent
sys.path.insert(0, str(ROOT / "tools"))
import serial_integrity  # noqa: E402

MANIFEST = ROOT / "idl" / "manifest.json"
LABEL = "tool:os.lazy.playground"

#: Interfaces whose calls the kernel refuses to this label (the rules `pkgd`
#: loaded): the name, and the script call that exercises it.
KERNEL_REFUSED = (
    "os.lazy.keyd.v1",
    "os.lazy.keyd.appsecret.v1",
    "os.lazy.accounts.v1",
    "os.lazy.net.stack.v1",
    "os.lazy.mount.v1",
)
#: The line a toast posted from a Playground script makes: LazyShell names the sender by
#: its label, so the owner is the Playground and no other app's toast satisfies this.
TOAST_POST = "SHELL:NOTIFY:POST owner=os.lazy.playground "
#: The package manager's own refusal of a labelled caller that is not a shell.
PKGD_REFUSAL = "INSTALL:FAIL applications may not install or remove other applications"
#: Lines that would mean something was created or changed.
CREATED = (
    (re.compile(r"^ACCOUNTS:CREATE:PASS"), "an account was created"),
    (re.compile(r"^MOUNTD:START "), "a mount daemon was started"),
    (re.compile(r"^PKGD:INSTALL:PASS"), "a package was installed"),
    (re.compile(r"^PKGD:REMOVE:PASS"), "a package was removed"),
)


def interface_ids(manifest_text: str) -> dict[str, int]:
    """`interface name -> id` from idl/manifest.json."""
    return {entry["interface"]: int(entry["interface_id"], 16) for entry in json.loads(manifest_text)}


def denied_ids(text: str, label: str = LABEL) -> set[int]:
    """The interface ids of every `LABEL:DENY` line of `label`."""
    found = set()
    for line in text.splitlines():
        match = re.match(rf"LABEL:DENY label={re.escape(label)} iface=(0x[0-9a-f]+) ", line)
        if match:
            found.add(int(match.group(1), 16))
    return found


def denied_methods(text: str, label: str = LABEL) -> dict[int, set[str]]:
    """`interface id -> the method ids` of every `LABEL:DENY` line of `label`."""
    found: dict[int, set[str]] = {}
    for line in text.splitlines():
        match = re.match(
            rf"LABEL:DENY label={re.escape(label)} iface=(0x[0-9a-f]+) method=(\S+)", line
        )
        if match:
            found.setdefault(int(match.group(1), 16), set()).add(match.group(2))
    return found


def judge_serial(text: str, ids: dict[str, int]) -> list[str]:
    """Every problem with the session's serial log; empty when it shows what it must."""
    problems = [f"serial log: {p}" for p in serial_integrity.problems(text)]
    denied = denied_ids(text)
    for name in KERNEL_REFUSED:
        if name not in ids:
            problems.append(f"{name} is not in idl/manifest.json")
        elif ids[name] not in denied:
            problems.append(
                f"no LABEL:DENY for {name} ({ids[name]:#018x}) from {LABEL}: the call was not "
                "refused by the kernel's rules, or never ran"
            )
    stack = ids.get("os.lazy.net.stack.v1")
    if stack is not None and len(denied_methods(text).get(stack, set())) < 2:
        problems.append(
            "the net_stack calls were not both refused: expected LABEL:DENY lines for two "
            "methods (Resolve, Ping) of os.lazy.net.stack.v1"
        )
    if not any(PKGD_REFUSAL in line for line in text.splitlines()):
        problems.append(f"pkgd did not refuse the install: no {PKGD_REFUSAL!r}")
    for line in text.splitlines():
        for pattern, what in CREATED:
            if pattern.match(line):
                problems.append(f"{what}: {line.strip()}")
    if not any(line.startswith(TOAST_POST) for line in text.splitlines()):
        problems.append(
            f"the tool script's toast was not posted as the Playground (no {TOAST_POST.strip()!r} line)"
        )
    for line in text.splitlines():
        if line.startswith("SHELL:NOTIFY:DENY"):
            problems.append(f"the toast was refused: {line.strip()}")
    runs = re.findall(r"^PLAYGROUND:RUN:(\d+):EXIT:(\S+?):", text, re.M)
    finished = {run for run, status in runs if status == "0"}
    if len(finished) < 2:
        problems.append(
            f"expected two script runs to end normally (EXIT:0), saw {sorted(runs)}"
        )
    return problems
