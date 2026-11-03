#!/usr/bin/env python3
"""Where a boot spends its time before a bonded Bluetooth keyboard can type
(docs/bluetooth-plan.md B6, decision 5), read off one serial log.

Every step of the path prints its own milliseconds since the kernel started
(the guest's monotonic clock, so the kernel's start is 0):

    INIT:SPAN step=usbd-spawn          init started usbd
    USBD:SPAN step=start               usbd running
    USBD:SPAN step=controller-up       the first xHCI controller is up
    USBD:SPAN step=bt-bound            the Bluetooth function is bound
    USBD:SPAN step=bt-published        os.lazy.usb.bt is registered
    INIT:SPAN step=btd-spawn           init started btd
    BT:SPAN  step=start                btd running
    BT:SPAN  step=attached             btd attached the transport rings
    BT:SPAN  step=bonds-loaded         the bond file is read
    BT:SPAN  step=reset-done           HCI_Reset completed
    BT:READY ms=                       the fast path is armed
    BT:RECONNECT ... link_at= report_at=   the link is up / the first report

The budgets are the plan's (B6) and apply to a hardware-accelerated run
(WHPX/KVM); `--relaxed` (TCG) reports the same table and checks the order
only, since a software-emulated guest is several times slower.

    python tools/bt/spans.py boot2/serial.log
    python tools/bt/spans.py boot2/serial.log --relaxed
"""

from __future__ import annotations

import argparse
import re
import sys
from pathlib import Path

#: Radio ready (`BT:READY`) this long after `usbd` started, at most.
READY_BUDGET_MS = 1500
#: Link up to the first report published, at most.
REPORT_BUDGET_MS = 250

SPAN = re.compile(r"(INIT|USBD|BT):SPAN step=(\S+) ms=(\d+)")
READY = re.compile(r"BT:READY ms=(\d+)")
RECONNECT = re.compile(r"BT:RECONNECT ms=(\d+) addr=\S+ link_at=(\d+) report_at=(\d+)")

#: The path in causal order: (label shown, where it comes from).
PATH = [
    ("init spawns usbd", ("INIT", "usbd-spawn")),
    ("usbd running", ("USBD", "start")),
    ("controller up", ("USBD", "controller-up")),
    ("BT function bound", ("USBD", "bt-bound")),
    ("os.lazy.usb.bt published", ("USBD", "bt-published")),
    ("init spawns btd", ("INIT", "btd-spawn")),
    ("btd running", ("BT", "start")),
    ("transport attached", ("BT", "attached")),
    ("bonds loaded", ("BT", "bonds-loaded")),
    ("HCI Reset done", ("BT", "reset-done")),
    ("fast path armed (BT:READY)", ("READY", "")),
    ("first link up", ("LINK", "")),
    ("first report published", ("REPORT", "")),
]


def collect(log: str) -> dict[tuple[str, str], int]:
    """The first time each step was reached."""
    found: dict[tuple[str, str], int] = {}
    for channel, step, ms in SPAN.findall(log):
        found.setdefault((channel, step), int(ms))
    ready = READY.search(log)
    if ready:
        found[("READY", "")] = int(ready.group(1))
    links = [(int(link), int(report)) for _, link, report in RECONNECT.findall(log)]
    if links:
        found[("LINK", "")] = min(link for link, _ in links)
        found[("REPORT", "")] = min(report for _, report in links)
    return found


def table(found: dict[tuple[str, str], int]) -> list[str]:
    """One line per step: ms since boot and the time since the step before."""
    lines = []
    last = 0
    for label, key in PATH:
        if key not in found:
            lines.append(f"  {label:<30} missing")
            continue
        at = found[key]
        lines.append(f"  {label:<30} {at:>7} ms   +{at - last}")
        last = at
    return lines


def judge(log: str, relaxed: bool = False) -> tuple[list[str], list[str], dict[str, int]]:
    """(table lines, failures, the two budgeted numbers)."""
    found = collect(log)
    failures = []
    for label, key in PATH:
        if key not in found:
            failures.append(f"no span for '{label}'")
    present = [(label, found[key]) for label, key in PATH if key in found]
    for (before, a), (after, b) in zip(present, present[1:]):
        if b < a:
            failures.append(f"'{after}' ({b} ms) is before '{before}' ({a} ms)")
    numbers: dict[str, int] = {}
    usbd, ready = found.get(("USBD", "start")), found.get(("READY", ""))
    if usbd is not None and ready is not None:
        numbers["ready_after_usbd_ms"] = ready - usbd
    # Per device, its own link to its own first report; the slowest counts.
    gaps = [int(report) - int(link) for _, link, report in RECONNECT.findall(log)]
    if gaps:
        numbers["report_after_link_ms"] = max(gaps)
    if not relaxed:
        late = numbers.get("ready_after_usbd_ms")
        if late is not None and late > READY_BUDGET_MS:
            failures.append(f"radio ready {late} ms after usbd started, budget {READY_BUDGET_MS} ms")
        slow = numbers.get("report_after_link_ms")
        if slow is not None and slow > REPORT_BUDGET_MS:
            failures.append(f"first report {slow} ms after the link, budget {REPORT_BUDGET_MS} ms")
    return table(found), failures, numbers


def report(log: str, relaxed: bool) -> list[str]:
    """Print the table and the verdict; the failures."""
    lines, failures, numbers = judge(log, relaxed)
    print("BT:BOOT:SPANS (ms since boot)")
    print("\n".join(lines))
    print("BT:BOOT " + " ".join(f"{k}={v}" for k, v in numbers.items()) + (" relaxed=1" if relaxed else ""))
    for failure in failures:
        print(f"BT:JUDGE:FAIL {failure}")
    return failures


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("log", type=Path)
    parser.add_argument("--relaxed", action="store_true", help="software emulation: check the order, not the budgets")
    args = parser.parse_args()
    failures = report(args.log.read_text(errors="replace"), args.relaxed)
    print(f"BT:JUDGE:{'FAIL' if failures else 'PASS'} boot-spans")
    return 1 if failures else 0


if __name__ == "__main__":
    sys.exit(main())
