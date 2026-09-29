#!/usr/bin/env python3
"""Assert the app launch/supervision evidence markers in a services serial log.

Boot the services image with ``LAZYOS_SERVICES=1`` (and, for the interactive
CLI markers, ``LAZYOS_MESSENGERCTL=1``) and capture ``serial.log`` with
``qemu_shot.py`` or ``qemu_session.py``; then run this checker. It is the
reproducible form of the issue #158 evidence: every assertion is a regex over
the serial log, so the same run can be compared across commits.

Usage
-----
    python tools/services/evidence.py shots/serial.log
    python tools/services/evidence.py shots/serial.log --require "MSGCTL:LAUNCH:PASS"
"""

from __future__ import annotations

import argparse
import re
import sys
from pathlib import Path

#: Marker regexes that must appear at least once.
REQUIRED: list[tuple[str, str]] = [
    ("app registry served", r"^INIT:APPS:PASS count=\d+$"),
    ("shipped apps counted (issue #216)", r"^INIT:APPS:SHIPPED count=\d+$"),
    ("launch succeeded", r"^INIT:LAUNCH:PASS app=\S+ pid=\d+ session=\d+$"),
    ("foreign-session launch denied", r"^INIT:LAUNCH:DENIED:PASS"),
    ("crash restart with backoff", r"^INIT:RESTART:PASS name=\S+ status=\d+ attempt=\d+ delay=\d+$"),
    ("launched app exited", r"^INIT:LAUNCH:EXIT app=\S+ status=\d+$"),
    ("launched app's own marker", r"^SYS:TOP:PASS$"),
    ("open-with publish fallback", r"^MIME:OPEN:PASS"),
]

#: Lines that must NOT appear (issue #216): `init` refuses a registered app whose
#: ELF the image does not ship quietly, so a boot never logs a launch failure
#: for one (it used to print `init: launch editor failed: EDITOR.ELF`).
FORBIDDEN: list[tuple[str, str]] = [
    ("no launch failure for an unshipped app", r"^init: launch \S+ failed"),
]

#: Interactive-CLI markers booted with `LAZYOS_MESSENGERCTL=1`. Each note is
#: reported but only fails when `--require-cli` is set.
CLI: list[tuple[str, str]] = [
    ("registry listed over the wire", r"^MSGCTL:APPS:PASS count=\d+$"),
    ("launch over the wire", r"^MSGCTL:LAUNCH:PASS app=\S+ pid=\d+$"),
    ("foreign-session denial over the wire", r"^MSGCTL:LAUNCH:DENIED:PASS$"),
]


def find(text: str, pattern: str) -> int:
    """Number of lines matching ``pattern`` (anchored, multiline)."""
    return len(re.findall(pattern, text, flags=re.MULTILINE))


def report(group: str, checks: list[tuple[str, str]], text: str) -> bool:
    ok = True
    for note, pattern in checks:
        count = find(text, pattern)
        status = "PASS" if count else "FAIL"
        ok &= count > 0
        print(f"{status} {group}: {note} ({count} match(es))")
    return ok


def report_absent(group: str, checks: list[tuple[str, str]], text: str) -> bool:
    ok = True
    for note, pattern in checks:
        count = find(text, pattern)
        status = "PASS" if not count else "FAIL"
        ok &= not count
        print(f"{status} {group}: {note} ({count} match(es))")
    return ok


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("log", help="serial log captured from a services boot")
    parser.add_argument(
        "--require-cli",
        action="store_true",
        help="also fail when the messengerctl boot markers are missing",
    )
    parser.add_argument(
        "--require",
        action="append",
        default=[],
        metavar="REGEX",
        help="additional required marker regex; repeat for multiple",
    )
    args = parser.parse_args()

    path = Path(args.log)
    if not path.is_file():
        sys.exit(f"serial log not found: {path}")
    text = path.read_text(encoding="utf-8", errors="replace")

    required = list(REQUIRED)
    for pattern in args.require:
        required.append((pattern, pattern))

    ok = report("services", required, text)
    ok &= report_absent("services", FORBIDDEN, text)
    cli_ok = report("cli", CLI, text)
    if args.require_cli:
        ok &= cli_ok

    if ok:
        print("evidence: PASS")
        return 0
    print("evidence: FAIL")
    return 1


if __name__ == "__main__":
    raise SystemExit(main())
