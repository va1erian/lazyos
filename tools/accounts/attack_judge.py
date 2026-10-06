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


U0 = "#623"
U3 = "U3 (brick-proofing, no issue yet)"

#: In the order they run (run.py). Flip an entry to "blocked" when its phase lands.
EXPECTATIONS: dict[str, Expect] = {
    "uid": Expect("xfail", U0),
    "rm_system": Expect("xfail", U0, ("/system/share/accounts",)),
    "overwrite_init": Expect("xfail", U0, ("/system/bin/init",)),
    "write_conf": Expect("xfail", U0, ("/conf",)),
    "confd_sys": Expect("xfail", U0, ("/conf",)),
    "keyd_provision": Expect("xfail", U0, ("/conf",)),
    "read_home_admin": Expect("xfail", U0),
    "signal_service": Expect("xfail", U0),
    "fork_bomb": Expect("xfail", U3),
    "disk_fill": Expect("xfail", U3, ("/home",)),
}


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
