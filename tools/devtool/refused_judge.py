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
  * `pkgd` refused the install itself ("applications may not install or remove
    other applications": the package manager's own check, which the Terminal
    passes as the session owner);
  * nothing was created: no account, no mount daemon, no installed package;
  * the toast the second script posts was accepted (`SHELL:NOTIFY:POST`) and
    not refused as coming from "not an app" (`SHELL:NOTIFY:DENY`).

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
    if not any(PKGD_REFUSAL in line for line in text.splitlines()):
        problems.append(f"pkgd did not refuse the install: no {PKGD_REFUSAL!r}")
    for line in text.splitlines():
        for pattern, what in CREATED:
            if pattern.match(line):
                problems.append(f"{what}: {line.strip()}")
    if not any(line.startswith("SHELL:NOTIFY:POST ") for line in text.splitlines()):
        problems.append("the tool script's toast was not posted (no SHELL:NOTIFY:POST)")
    for line in text.splitlines():
        if line.startswith("SHELL:NOTIFY:DENY"):
            problems.append(f"the toast was refused: {line.strip()}")
    runs = re.findall(r"^PLAYGROUND:RUN:(\d+):EXIT:", text, re.M)
    if len(set(runs)) < 2:
        problems.append(f"expected two script runs to finish, saw {sorted(set(runs))}")
    return problems
