"""The verdict of webd's malicious-client attack harness (docs/webd-plan.md P5).

The client `os.lazy.webdattack` (`xui-app/src/bin/webdattack.rs`, logic in
`xui_webd_client::attack`) is a real client of webd whose manifest lists only the base full-view
interface `os.lazy.webd.view.v1`. From inside that label it fires a fixed sequence of hostile calls
and prints one line per probe:

    WEBDATTACK:PROBE:<name>:<outcome>      outcome: OK | REFUSED:<errno> | REFUSED:<errno>:<count> | ...
    WEBDATTACK:DONE:PASS
    WEBDATTACK:QUIT:PASS

This module holds the table of the outcome each probe must come to in a guest run and judges a
serial log against it, in the style of `tools/accounts/attack_judge.py` and
`tools/webkit/attackjudge.py`: every probe is present with its expected outcome; a probe that is
allowed where it must be refused is a BREACH. The two scope probes must also be refused *by the
policy*: a `LABEL:DENY` line of this client's label (the kernel refused its `CreateView` through a
scope its manifest does not grant). A policy probe counts as blocked only with that refusal.
"""

from __future__ import annotations

import re

LABEL = "app:os.lazy.webdattack"

PROBE = re.compile(r"^WEBDATTACK:PROBE:(?P<name>[a-z_]+):(?P<outcome>\S+)\s*$", re.M)
DONE = re.compile(r"^WEBDATTACK:DONE:PASS\s*$", re.M)
QUIT = re.compile(r"^WEBDATTACK:QUIT:PASS\s*$", re.M)
DENY = re.compile(r"^LABEL:DENY label=(?P<label>\S+) (?P<what>.*?)\s*$", re.M)

#: Positive errno values a policy refusal carries: EPERM (the kernel's gate) or EACCES (a Messenger
#: refusal). The service's own resource refusals carry their own errno (ENOSPC 28).
POLICY_ERRNOS = {"1", "13"}
ENOSPC = "28"

#: Views one client may hold at once (`xui_webd_client::attack::VIEWS_PER_CLIENT`,
#: `xui_webd::limits::VIEWS_PER_CLIENT`): the cap probe is refused when this many are live.
VIEWS_PER_CLIENT = 8


def _scope_refused(outcome: str) -> tuple[bool, str]:
    """A scope probe must be REFUSED with a policy errno."""
    match = re.fullmatch(r"REFUSED:(\d+)", outcome)
    if not match:
        return False, f"was {outcome!r}, not refused by the policy (want REFUSED:<EPERM|EACCES>)"
    if match.group(1) not in POLICY_ERRNOS:
        return False, f"refused with errno {match.group(1)}, not the policy's ({sorted(POLICY_ERRNOS)})"
    return True, ""


def _ok(outcome: str) -> tuple[bool, str]:
    return (outcome == "OK", "" if outcome == "OK" else f"was {outcome!r}, want OK")


def _cap(outcome: str) -> tuple[bool, str]:
    want = f"REFUSED:{ENOSPC}:{VIEWS_PER_CLIENT}"
    if outcome == want:
        return True, ""
    return False, f"was {outcome!r}, want {want} (the {VIEWS_PER_CLIENT + 1}th view refused with ENOSPC)"


#: Each probe's name and the check its outcome must pass. `scope_*` are the kernel scope gate (also
#: need a LABEL:DENY, below); the rest are the service's own refusals and liveness.
CHECKS: dict[str, object] = {
    "scope_net": _scope_refused,
    "scope_browser": _scope_refused,
    "view_ok": _ok,
    "flood_alive": _ok,
    "view_cap": _cap,
    "alive_after": _ok,
}

#: The probes whose refusal must be a policy refusal of the client's label (a LABEL:DENY).
POLICY_PROBES = ("scope_net", "scope_browser")


def judge(serial: str) -> tuple[list[str], list[str]]:
    """``(problems, rows)``: ``problems`` is empty when every probe did what the table says."""
    problems: list[str] = []
    rows: list[str] = []
    seen = {m["name"]: m["outcome"] for m in PROBE.finditer(serial)}
    denies = [m["what"] for m in DENY.finditer(serial) if m["label"] == LABEL]

    for name, check in CHECKS.items():
        if name not in seen:
            problems.append(f"{name}: no WEBDATTACK:PROBE line")
            rows.append(f"{name:14} MISSING")
            continue
        outcome = seen[name]
        ok, why = check(outcome)  # type: ignore[operator]
        rows.append(f"{name:14} {outcome:20} {'ok' if ok else 'FAIL: ' + why}")
        if not ok:
            breach = "BREACH " if name in POLICY_PROBES and outcome == "OK" else ""
            problems.append(f"{name}: {breach}{why}")

    # The scope probes must be refused by the policy: one LABEL:DENY of this label per scope (the
    # kernel refused the CreateView it sent through a scope its manifest does not grant).
    if len(denies) < len(POLICY_PROBES):
        problems.append(
            f"only {len(denies)} LABEL:DENY of {LABEL} (want one per scope: {POLICY_PROBES}); "
            "the scope refusals were not the kernel's policy"
        )
    # No refusal of this label beyond the scope gate: a client with the view scope must be denied
    # nothing else.
    if len(denies) > len(POLICY_PROBES):
        extra = denies[len(POLICY_PROBES):]
        problems.append(f"{len(denies)} LABEL:DENY of {LABEL}, more than the {len(POLICY_PROBES)} scope gates: {extra[:6]}")

    extra = sorted(name for name in seen if name not in CHECKS)
    for name in extra:
        problems.append(f"{name}: not in the judge's table")
    if not DONE.search(serial):
        problems.append("no WEBDATTACK:DONE:PASS (the probe did not finish)")
    if not QUIT.search(serial):
        problems.append("no WEBDATTACK:QUIT:PASS (the probe did not quit cleanly)")
    rows.append(f"{'label denies':14} {len(denies)} of {LABEL}")
    return problems, rows
