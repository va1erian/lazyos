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
`tools/webkit/attackjudge.py`: every probe is present exactly once with its expected outcome; a probe
that is allowed where it must be refused is a BREACH. The two scope probes must be refused *by the
kernel's policy*: one `LABEL:DENY` of this client's label naming the `net` scope's interface id and
one naming the `browser` scope's (`idl/manifest.json`), and no other denial of the label. webd must
not restart during the run (one `WEBD:UP:PASS`), and the label trace must not have dropped lines.
"""

from __future__ import annotations

import json
import re
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent.parent
LABEL = "app:os.lazy.webdattack"

PROBE = re.compile(r"^WEBDATTACK:PROBE:(?P<name>[a-z_]+):(?P<outcome>\S+)\s*$", re.M)
DONE = re.compile(r"^WEBDATTACK:DONE:PASS\s*$", re.M)
QUIT = re.compile(r"^WEBDATTACK:QUIT:PASS\s*$", re.M)
WEBD_UP = re.compile(r"WEBD:UP:PASS")
#: Not anchored: the kernel's lines may carry a time stamp before the tag.
DENY = re.compile(r"LABEL:DENY label=(?P<label>\S+) (?P<what>.*?)\s*$", re.M)
SUPPRESSED = re.compile(r"SUPPRESSED")

#: Positive errno values a policy refusal carries: EPERM (the kernel's gate) or EACCES (a Messenger
#: refusal). The service's own resource refusals carry their own errno (ENOSPC 28).
POLICY_ERRNOS = {"1", "13"}
ENOSPC = "28"

#: Views one client may hold at once (`xui_webd_client::attack::VIEWS_PER_CLIENT`,
#: `xui_webd::limits::VIEWS_PER_CLIENT`): the cap probe is refused when this many are live.
#: `test_attack_judge.py` holds the three together.
VIEWS_PER_CLIENT = 8

#: The interfaces whose `CreateView` the client must be refused, by the kernel (its manifest lists none).
SCOPE_INTERFACES = {"scope_net": "os.lazy.webd.net.v1", "scope_browser": "os.lazy.webd.browser.v1"}


def interface_id(name: str, manifest: Path = ROOT / "idl" / "manifest.json") -> int:
    """The interface id `midlc` gave `name` (as the kernel's trace prints it, `iface=0x...`)."""

    def walk(node):
        if isinstance(node, dict):
            if node.get("interface") == name and ("id" in node or "interface_id" in node):
                yield node.get("id", node.get("interface_id"))
            for value in node.values():
                yield from walk(value)
        elif isinstance(node, list):
            for value in node:
                yield from walk(value)

    found = {int(str(v), 0) for v in walk(json.loads(manifest.read_text(encoding="utf-8")))}
    if len(found) != 1:
        raise ValueError(f"{name}: {len(found)} ids in {manifest}")
    return found.pop()


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
#: need their LABEL:DENY, below); the rest are the service's own refusals and liveness.
CHECKS: dict[str, object] = {
    "scope_net": _scope_refused,
    "scope_browser": _scope_refused,
    "view_ok": _ok,
    "flood_alive": _ok,
    "view_cap": _cap,
    "alive_after": _ok,
}


def judge(serial: str, ids: dict[str, int] | None = None) -> tuple[list[str], list[str]]:
    """``(problems, rows)``: ``problems`` is empty when every probe did what the table says. `ids` maps
    each scope probe to the interface id its denial must name (default: `idl/manifest.json`)."""
    ids = ids or {probe: interface_id(name) for probe, name in SCOPE_INTERFACES.items()}
    problems: list[str] = []
    rows: list[str] = []
    seen: dict[str, list[str]] = {}
    for m in PROBE.finditer(serial):
        seen.setdefault(m["name"], []).append(m["outcome"])
    denies = [m["what"] for m in DENY.finditer(serial) if m["label"] == LABEL]

    for name, check in CHECKS.items():
        outcomes = seen.get(name, [])
        if len(outcomes) != 1:
            problems.append(f"{name}: reported {len(outcomes)} times, want once")
            rows.append(f"{name:14} {'MISSING' if not outcomes else 'REPEATED'}")
            continue
        outcome = outcomes[0]
        ok, why = check(outcome)  # type: ignore[operator]
        rows.append(f"{name:14} {outcome:20} {'ok' if ok else 'FAIL: ' + why}")
        if not ok:
            breach = "BREACH " if name in SCOPE_INTERFACES and outcome == "OK" else ""
            problems.append(f"{name}: {breach}{why}")

    # Each scope refusal must be the kernel's: a denial of this label naming that scope's interface.
    for probe, iface in ids.items():
        pattern = re.compile(rf"\biface=0x{iface:016x}\b", re.I)
        if not any(pattern.search(what) for what in denies):
            problems.append(f"{probe}: no LABEL:DENY of {LABEL} for iface=0x{iface:016x}: the refusal was not the kernel's policy")
    # A client with the view scope must be denied nothing else.
    named = [w for w in denies if any(re.search(rf"\biface=0x{i:016x}\b", w, re.I) for i in ids.values())]
    others = [w for w in denies if w not in named]
    if others:
        problems.append(f"{len(others)} other LABEL:DENY of {LABEL}: {others[:6]}")
    if len(named) > len(ids):
        problems.append(f"{len(named)} scope denials of {LABEL}, want {len(ids)} (one per scope probe)")

    extra = sorted(name for name in seen if name not in CHECKS)
    for name in extra:
        problems.append(f"{name}: not in the judge's table")
    if not DONE.search(serial):
        problems.append("no WEBDATTACK:DONE:PASS (the probe did not finish)")
    if not QUIT.search(serial):
        problems.append("no WEBDATTACK:QUIT:PASS (the probe did not quit cleanly)")
    ups = len(WEBD_UP.findall(serial))
    if ups != 1:
        problems.append(f"WEBD:UP:PASS {ups} times, want once (webd restarted, or never came up)")
    if SUPPRESSED.search(serial):
        problems.append("the label trace dropped lines (rate limit): the denials cannot be matched")
    rows.append(f"{'label denies':14} {len(denies)} of {LABEL}")
    return problems, rows
