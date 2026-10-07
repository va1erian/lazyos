#!/usr/bin/env python3
"""Judge the account attack scenarios from a LazyOS serial log.

Each scenario prints one `ACCT:ATTACK:<name>:BLOCKED|SUCCEEDED:<detail>` line
(`assets/accounts/attack.sh` and the rhai scripts next to it; the Terminal
reports it on serial as `TERM:OUT:...`). What each scenario is *expected* to
do lives in `EXPECTATIONS`:

* `blocked`: the phase that closes it has landed; a `SUCCEEDED` is a failure.
* `xfail`: known open, tracked by an issue. `SUCCEEDED` is the expected
  outcome today and only noted; `BLOCKED` is an "XPASS" (the fix landed): flip
  the entry to `blocked`, which turns the scenario into a real gate. An XPASS
  is a note, not a failure, so a fix never breaks the harness.

Whatever the expectation, a scenario that printed nothing, printed `ERROR`, or
was `BLOCKED` only because its target did not exist (`ENOENT`: the attack never
ran) is a failure, and so is a marker for a scenario the table does not know.

A scenario with a `refusal` must be `BLOCKED` for exactly that reason: the
accountsd, timed and init probes print `EPERM-policy` only when the refusal is
`EPERM` *and* carries the service's policy text (`POLICY_TEXT`). An `EACCES`
(a wrong old password, an ACL) or another `EPERM` is a different refusal and
fails, so a regression cannot pass as BLOCKED.

`autostart_root` has no guest command: `run.py` installs a package that opens
at login during the attack session and writes its marker from the next boot's
log ([`autostart_marker`]): the package must have opened in a login session as
that session's user, never as root.

    python tools/accounts/attack_judge.py shots/accounts/attack/serial.log
"""

from __future__ import annotations

import argparse
import re
import sys
from dataclasses import dataclass
from pathlib import Path

MARKER = re.compile(r"ACCT:ATTACK:([a-z0-9_]+):(BLOCKED|SUCCEEDED|ERROR):(\S*)")


@dataclass(frozen=True)
class Expect:
    """What one scenario must do. `touches`: the image paths the attack changes
    when it succeeds, so the host-side audit tolerates exactly those while the
    scenario is `xfail` (audit.py)."""

    state: str  # "blocked" or "xfail"
    issue: str = ""  # the issue or phase tracking an xfail
    touches: tuple[str, ...] = ()
    #: Image paths the scenario changes by allowed means whatever its state
    #: (a user installing a package); the audit excuses them always.
    side_effects: tuple[str, ...] = ()
    #: The only BLOCKED detail that counts (empty: any refusal).
    refusal: str = ""


U0 = "#623"
U1 = "#624"
U2 = "#625"
U3 = "U3 (brick-proofing, no issue yet)"
#: What an installed package writes: its tree, its docs, confd's record.
INSTALL_PATHS = ("/apps", "/docs/apps", "/conf")
#: The detail a probe prints for EPERM with the service's policy text.
POLICY = "EPERM-policy"

#: Each policy probe's script, the source file holding the service's refusal
#: text and that text; `test_judge.py` checks the script and the source agree.
POLICY_TEXT: dict[str, tuple[str, str]] = {
    name: (source, text) for names, source, text in (
        (("acct_create", "acct_delete", "acct_promote"), "libs/accountdb/src/policy.rs",
         "only an administrator, through elevd, may create, delete or promote accounts"),
        (("acct_password",), "libs/accountdb/src/policy.rs",
         "only an administrator, through elevd, may change another user's password"),
        (("direct_time", "direct_zone"), "user/src/bin/timed/handler.rs",
         "only an administrator, through elevd, may change the clock or the time zone"),
        (("direct_restart",), "user/src/bin/init/homes.rs",
         "only elevd may restart a service, once an administrator approved"),
    ) for name in names
}

#: Flip an entry to "blocked" when its phase lands. U0 (#623) landed: the
#: desktop session runs as `user` with no capability. U1 (#624): accounts
#: change only through elevd, Authenticate is slowed. U2 (#625): the
#: privileged service paths answer elevd alone, the trusted prompt holds,
#: and a core app is replaced only through elevd.
EXPECTATIONS: dict[str, Expect] = {
    "uid": Expect("blocked", U0),
    "rm_system": Expect("blocked", U0, ("/system/share/accounts",)),
    "overwrite_init": Expect("blocked", U0, ("/system/bin/init",)),
    "write_conf": Expect("blocked", U0, ("/conf",)),
    "confd_sys": Expect("blocked", U0, ("/conf",)),
    "keyd_provision": Expect("blocked", U0, ("/conf",)),
    "read_home_admin": Expect("blocked", U0),
    "signal_service": Expect("blocked", U0),
    "autostart_root": Expect("blocked", U0, side_effects=INSTALL_PATHS),
    "acct_create": Expect("blocked", U1, ("/conf", "/home"), refusal=POLICY),
    "acct_delete": Expect("blocked", U1, ("/conf", "/home"), refusal=POLICY),
    "acct_promote": Expect("blocked", U1, ("/conf",), refusal=POLICY),
    "acct_password": Expect("blocked", U1, ("/conf",), refusal=POLICY),
    "keyd_forget": Expect("blocked", U1),
    # Review of #659 (H1): confd's raw store is unreadable to a session.
    "read_conf_store": Expect("blocked", U1),
    # H2: keyd's Verify is accountsd's alone (no way around the brake).
    "keyd_verify": Expect("blocked", U1),
    # H5: a session's Authenticate flood cannot lock admin out of elevd.
    "admin_lockout": Expect("blocked", U1),
    "auth_flood": Expect("blocked", U1),
    "direct_time": Expect("blocked", U2, refusal=POLICY),
    # The zone is a machine setting like the clock (review of #659).
    "direct_zone": Expect("blocked", U2, ("/conf",), refusal=POLICY),
    "direct_restart": Expect("blocked", U2, refusal=POLICY),
    "prompt_spoof": Expect("blocked", U2),
    "input_focus": Expect("blocked", U2),
    "display_read": Expect("blocked", U2),
    "prompt_over": Expect("blocked", U2),
    "prompt_keys": Expect("blocked", U2),
    # Review of #659: a flooded inputd cannot hand the prompt's keys to an
    # app (H3), and a cancelled prompt cannot be raised again at once (H4).
    "input_flood": Expect("blocked", U2),
    "prompt_flood": Expect("blocked", U2),
    "core_replace": Expect("blocked", U2, INSTALL_PATHS),
    # Review of #659: a value cannot forge an audit line, pkg.install cannot
    # replace a core app, and the services identity and the prompt rest on
    # cannot be restarted from a session.
    "audit_forge": Expect("blocked", U2, ("/conf",)),
    "core_claim": Expect("blocked", U2, INSTALL_PATHS),
    "restart_elevd": Expect("blocked", U2),
    "restart_xuid": Expect("blocked", U2),
    "fork_bomb": Expect("xfail", U3),
    "disk_fill": Expect("xfail", U3, ("/home",)),
}

#: An audit line `audit_forge` would have forged: one that *starts* with its
#: fields (a real line carries the text quoted inside its summary).
FORGED = re.compile(r"^ELEVD:REQUEST op=account\.admin uid=0 label=0 session=0 admin=forged",
                    re.MULTILINE)

#: The package `autostart_root` installs (tools/accounts/probe_packages.py).
AUTOPROBE = "org.acct.autoprobe"


def autostart_marker(install_log: str, login_log: str) -> str:
    """`autostart_root`'s marker from the attack boot (the install) and the
    next boot (the login that opens it): BLOCKED when it opened in a login
    session as that session's non-root user, SUCCEEDED when it ran as root or
    outside any session, ERROR when it was not installed or never opened."""
    if "ACCT:INSTALL:autostart_pkg:OK" not in install_log:
        return "ACCT:ATTACK:autostart_root:ERROR:notinstalled"
    opened = re.search(rf"INIT:AUTOSTART:PASS app={re.escape(AUTOPROBE)}(?: session=(\d+))?",
                       login_log)
    if not opened:
        return "ACCT:ATTACK:autostart_root:ERROR:notopened"
    session = opened.group(1) or "0"
    owner = re.search(rf"INIT:AUTOSTART:SESSION session={session} uid=(\d+)", login_log)
    uid = owner.group(1) if owner else "0"
    if session == "0" or uid == "0":
        return f"ACCT:ATTACK:autostart_root:SUCCEEDED:session={session}_uid={uid}"
    return f"ACCT:ATTACK:autostart_root:BLOCKED:uid={uid}"


def side_effects(expectations: dict[str, Expect] = EXPECTATIONS) -> list[str]:
    """The paths the audit excuses whatever happened (allowed side effects)."""
    return [path for expect in expectations.values() for path in expect.side_effects]


@dataclass
class Verdict:
    failures: list[str]
    notes: list[str]
    #: Scenarios that SUCCEEDED while expected to be open: the audit's allowance.
    open_attacks: list[str]


def parse(log: str) -> dict[str, tuple[str, str]]:
    """name -> (outcome, detail); the first marker of each scenario counts."""
    found: dict[str, tuple[str, str]] = {}
    for name, outcome, detail in MARKER.findall(log):
        found.setdefault(name, (outcome, detail))
    return found


def judge(log: str, expectations: dict[str, Expect] = EXPECTATIONS) -> Verdict:
    """Failures, notes and the open attacks of one attack session's serial log."""
    seen = parse(log)
    verdict = Verdict([], [], [])
    if FORGED.search(log):
        verdict.failures.append("audit_forge: a request value forged an ELEVD:REQUEST line")
    for name in seen.keys() - expectations.keys():
        verdict.failures.append(f"{name}: a scenario with no expectation (add it to EXPECTATIONS)")
    for name, expect in expectations.items():
        if name not in seen:
            verdict.failures.append(f"{name}: no ACCT:ATTACK marker (the scenario did not finish)")
            continue
        outcome, detail = seen[name]
        if outcome == "ERROR":
            verdict.failures.append(f"{name}: the scenario could not run ({detail})")
        elif outcome == "BLOCKED" and detail == "ENOENT":
            verdict.failures.append(f"{name}: BLOCKED only by ENOENT, the attack never ran")
        elif outcome == "BLOCKED" and expect.refusal and detail != expect.refusal:
            verdict.failures.append(
                f"{name}: BLOCKED for another reason ({detail}), not the policy's "
                f"{expect.refusal}")
        elif outcome == "SUCCEEDED" and expect.state == "blocked":
            verdict.failures.append(f"{name}: SUCCEEDED ({detail}) but must be BLOCKED")
        elif outcome == "SUCCEEDED":
            verdict.notes.append(f"{name}: SUCCEEDED ({detail}), xfail {expect.issue}")
            verdict.open_attacks.append(name)
        elif expect.state == "xfail":
            verdict.notes.append(
                f"{name}: XPASS, BLOCKED ({detail}); flip the expectation to blocked "
                f"({expect.issue})")
    return verdict


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__,
                                     formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("serial_log", type=Path)
    args = parser.parse_args()
    verdict = judge(args.serial_log.read_text(encoding="utf-8", errors="replace"))
    for note in verdict.notes:
        print(f"note: {note}")
    for failure in verdict.failures:
        print(f"FAIL: {failure}")
    print("PASS" if not verdict.failures else f"{len(verdict.failures)} failure(s)")
    return 1 if verdict.failures else 0


if __name__ == "__main__":
    sys.exit(main())
