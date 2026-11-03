#!/usr/bin/env python3
"""Judge a firmware run of the Bluetooth harness (docs/bluetooth-plan.md, B7).

`tools/bt/run.py --fw KIND` boots against a radio that needs firmware
(`modes_fw.py`). The verdict has two parts:

* the firmware phase: `btd` found the vendor from the USB ids, asked for the
  files the part's version answer names (through `libs/fwload`), downloaded,
  and only then ran `bthci::Init` (`BT:FW:DONE` before `BT:CTRL:PASS`), with
  the re-enumerations the kind expects (the Intel recovery case: one, then
  success; the others: none, and `usbd` bound the controller once);
* what follows is the `--pair` test (`judge_hid.py`), judged on the log from
  the last `btd` attach on, since a re-enumeration starts `btd`'s bring-up
  again (and prints its bond load and `BT:LINK` again).

    python tools/bt/judge_fw.py intel shots/bt-fw/serial.log
"""

from __future__ import annotations

import argparse
import re
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
import judge_hid  # noqa: E402
import modes_fw  # noqa: E402

FW_LINE = re.compile(r"BT:FW:(VENDOR|FILE|DONE|REENUM|FAIL|READ)\b(.*)")
DONE = re.compile(r"BT:FW:DONE vendor=(\w+) downloaded=(\d) config=(\S+) ms=(\d+)")
VENDOR = re.compile(r"BT:FW:VENDOR vendor=(\w+) usb=([0-9a-f]{4}:[0-9a-f]{4}) attempt=(\d+)")
FILE_ASK = re.compile(r"BT:FW:FILE name=(\S+) required=(true|false)")
FILE_GOT = re.compile(r"BT:FW:FILE name=(\S+) bytes=(\d+)")
REENUM = re.compile(r"BT:FW:REENUM attempt=(\d+)")
FATAL = re.compile(r"BT:FW:(FAIL|READ)")


def fw_lines(log: str) -> list[str]:
    return [line.strip() for line in log.splitlines() if FW_LINE.search(line)]


def judge_phase(log: str, expect: dict) -> list[str]:
    failures = [f"firmware error: {line}" for line in fw_lines(log) if FATAL.search(line)]
    vendors = VENDOR.findall(log)
    want = (expect["vendor"], expect["usb"])
    attempts = 1 + expect["reenum"]
    if len(vendors) != attempts:
        failures.append(f"{len(vendors)} BT:FW:VENDOR lines, want {attempts}")
    for vendor, usb, _ in vendors:
        if (vendor, usb) != want:
            failures.append(f"BT:FW:VENDOR {vendor} {usb}, want {want[0]} {want[1]}")
    if [int(n) for _, _, n in vendors] != list(range(attempts)):
        failures.append(f"BT:FW:VENDOR attempt counts {[n for _, _, n in vendors]}")
    reenums = [int(n) for n in REENUM.findall(log)]
    if reenums != list(range(1, expect["reenum"] + 1)):
        failures.append(f"BT:FW:REENUM attempts {reenums}, want {expect['reenum']} of them")
    failures += judge_files(log, expect["files"], attempts)
    done = DONE.findall(log)
    if len(done) != 1:
        failures.append(f"{len(done)} BT:FW:DONE lines, want exactly one")
    elif done[0][0] != expect["vendor"] or done[0][1] != "1":
        failures.append(f"BT:FW:DONE {done[0]}: the last attempt should have downloaded")
    failures += judge_order(log)
    return failures


def judge_files(log: str, names: list[str], attempts: int) -> list[str]:
    """Every file the part asked for was found and read, each time."""
    failures = []
    asked = [name for name, _ in FILE_ASK.findall(log)]
    if asked != names * attempts:
        failures.append(f"files asked for {asked}, want {names * attempts}")
    got = {name: int(size) for name, size in FILE_GOT.findall(log)}
    for name in names:
        if got.get(name, 0) <= 0:
            failures.append(f"{name} was not read from the firmware tree")
    if "missing=1" in log:
        failures.append("a firmware file was reported missing")
    return failures


def judge_order(log: str) -> list[str]:
    """The download ends before the controller is brought up and used."""
    failures = []
    done = log.find("BT:FW:DONE")
    for marker in ("BT:CTRL:PASS", "BT:READY", "BT:HID:KBD"):
        at = log.find(marker)
        if at < 0:
            failures.append(f"no {marker}")
        elif 0 <= done and at < done:
            failures.append(f"{marker} before BT:FW:DONE")
    if log.count("BT:CTRL:PASS") != 1:
        failures.append(f"{log.count('BT:CTRL:PASS')} BT:CTRL:PASS lines, want one")
    return failures


def judge_usb(log: str, reenum: int) -> list[str]:
    """`usbd` bound the controller once, plus once per re-enumeration, and
    unbound it for each of them."""
    binds, unbinds = log.count("USBD:BT:BIND "), log.count("USBD:BT:UNBIND")
    failures = []
    if binds != 1 + reenum:
        failures.append(f"{binds} USBD:BT:BIND lines, want {1 + reenum}")
    if unbinds != reenum:
        failures.append(f"{unbinds} USBD:BT:UNBIND lines, want {reenum}")
    if log.count("USBD:BT:SERVE") != 1 + reenum:
        failures.append(f"{log.count('USBD:BT:SERVE')} USBD:BT:SERVE lines, want {1 + reenum}")
    return failures


def last_attach(log: str) -> str:
    """The log from the last `btd` bond load (the start of the final bring-up)."""
    at = log.rfind("BT:BOND:LOAD")
    return log[at:] if at >= 0 else log


def judge(kind: str, log: str, text: str) -> list[str]:
    expect = modes_fw.KINDS[kind][1]
    failures = judge_phase(log, expect)
    failures += judge_usb(log, expect["reenum"])
    failures += judge_hid.judge_pair(last_attach(log), text)
    return failures


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("kind", choices=sorted(modes_fw.KINDS))
    parser.add_argument("log", type=Path)
    parser.add_argument("--text", default="hello")
    args = parser.parse_args()
    log = args.log.read_text(errors="replace")
    failures = judge(args.kind, log, args.text)
    for failure in failures:
        print(f"BT:JUDGE:FAIL {failure}")
    done = DONE.search(log)
    if done:
        print(f"BT:TIMING fw_ms={done.group(4)} downloaded={done.group(2)} config={done.group(3)}")
    print(f"BT:JUDGE:{'FAIL' if failures else 'PASS'} fw-{args.kind}")
    return 1 if failures else 0


if __name__ == "__main__":
    sys.exit(main())
