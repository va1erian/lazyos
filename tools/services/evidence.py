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
    python tools/services/evidence.py shots/serial.log --desktop
    python tools/services/evidence.py shots/serial.log --require "MSGCTL:LAUNCH:PASS"

A plain (`LAZYOS_SERVICES=1`) boot is checked for the full demo evidence; a
`LAZYOS_DESKTOP=1` boot (``--desktop``) is checked only for the markers the
desktop profile still emits, and the excluded programs' startup markers are
asserted absent, since it starts no evidence programs.
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
    ("launch argument validation", r"^INIT:LAUNCH:ARGS:PASS"),
    ("desktop shell supervised always-restart (issue #157)", r"^INIT:SHELL:PASS"),
    ("session owners learned from logind (issue #157)", r"^INIT:SESSIONS:PASS$"),
    ("crash restart with backoff", r"^INIT:RESTART:PASS name=\S+ status=\d+ attempt=\d+ delay=\d+$"),
    ("launched app exited", r"^INIT:LAUNCH:EXIT app=\S+ status=\d+$"),
    ("launched app's own marker", r"^SYS:TOP:PASS$"),
    ("timed followed a confd zone change", r"^TIMED:DEMO:PASS"),
    ("timed answered every method (timectl)", r"^TIMECTL:SELFTEST:PASS paris_offset=-?\d+$"),
    ("open-with publish fallback", r"^MIME:OPEN:PASS"),
    ("timed serving (issue #369)", r"TIMED:READY unix=\d+ "),
    ("timed published time/tick", r"TIMED:TICK:PASS unix=\d+ offset=-?\d+ zone=\S+$"),
    ("logd journals in /logs (issue #508)", r"^LOGD:STORE:READY dir=/logs boot=[0-9a-f]{16} "),
    ("confd store in /conf (issue #508)", r"^CONFD:READY dir=/conf persistent=true$"),
    ("accountsd loaded 2 rows from /conf/accounts/db (issues #508, #624)",
     r"^ACCOUNTS:LOAD:PASS rows=2 admins=1 file=/conf/accounts/db$"),
    ("mime overrides from /system/share/mime.types (issue #508)",
     r"^MIME:GUESS:PASS SAMPLE\.LZT \S+ \(/system/share/mime\.types\)$"),
]

#: Markers a desktop-profile boot (`LAZYOS_DESKTOP=1`, issue #217) still
#: emits. The profile deliberately leaves the demo/evidence programs out (the
#: `flaky` crash service, the `top` launch self-test, the clipboard demo pair),
#: so their markers are only required of a plain `LAZYOS_SERVICES=1` boot.
DESKTOP: list[tuple[str, str]] = [
    ("app registry served", r"^INIT:APPS:PASS count=\d+$"),
    ("shipped apps counted (issue #216)", r"^INIT:APPS:SHIPPED count=\d+$"),
    ("foreign-session launch denied", r"^INIT:LAUNCH:DENIED:PASS"),
    ("launch argument validation", r"^INIT:LAUNCH:ARGS:PASS"),
    ("desktop shell supervised always-restart (issue #157)", r"^INIT:SHELL:PASS"),
    ("session owners learned from logind (issue #157)", r"^INIT:SESSIONS:PASS$"),
    ("open-with publish fallback", r"^MIME:OPEN:PASS"),
    ("timed serving (issue #369)", r"TIMED:READY unix=\d+ "),
    ("timed published time/tick", r"TIMED:TICK:PASS unix=\d+ offset=-?\d+ zone=\S+$"),
    ("logd journals in /logs (issue #508)", r"^LOGD:STORE:READY dir=/logs boot=[0-9a-f]{16} "),
    ("confd store in /conf (issue #508)", r"^CONFD:READY dir=/conf persistent=true$"),
    ("accountsd loaded 2 rows from /conf/accounts/db (issues #508, #624)",
     r"^ACCOUNTS:LOAD:PASS rows=2 admins=1 file=/conf/accounts/db$"),
    ("mime overrides from /system/share/mime.types (issue #508)",
     r"^MIME:GUESS:PASS SAMPLE\.LZT \S+ \(/system/share/mime\.types\)$"),
]

#: Lines that must NOT appear (issue #216): `init` refuses a registered app whose
#: ELF the image does not ship quietly, so a boot never logs a launch failure
#: for one (it used to print `init: launch editor failed: /system/bin/editor`).
FORBIDDEN: list[tuple[str, str]] = [
    ("no launch failure for an unshipped app", r"^init: launch \S+ failed"),
    # Issue #508: the services keep their state on the OS volume.
    ("mimed found its override file", r"^MIME:GUESS:INFO no override file"),
    ("pkgd's store (/apps, /docs/apps, /logs) is writable", r"^PKGD:STORE:ABSENT"),
    ("confd did not fall back to the ramfs", r"^CONFD:READY dir=\S+ persistent=false"),
    ("accountsd loaded its account file", r"^ACCOUNTS:LOAD:FAIL"),
]

#: Markers of the demo/evidence programs the desktop profile excludes (issue
#: #217). They are asserted absent only under ``--desktop``, so a regression
#: that re-enables one fails the desktop job instead of passing silently.
DESKTOP_FORBIDDEN: list[tuple[str, str]] = [
    ("no flaky crash service", r"^flaky: starting"),
    ("no clipboard demo pair", r"^clipboardd: started demo /system/bin/clip"),
    ("no top text client", r"^(?:sysmond: started demo top|SYS:TOP:PASS|top: LazyOS)"),
    ("no xdemo client", r"^(?:xdemo: |XDEMO:UP:PASS)"),
    ("no dragdemo launcher", r"^dragdemo: "),
]

#: Interactive-CLI markers booted with `LAZYOS_MESSENGERCTL=1`. Each note is
#: reported but only fails when `--require-cli` is set.
CLI: list[tuple[str, str]] = [
    ("registry listed over the wire", r"^MSGCTL:APPS:PASS count=\d+$"),
    ("launch over the wire", r"^MSGCTL:LAUNCH:PASS app=\S+ pid=\d+$"),
    ("foreign-session denial over the wire", r"^MSGCTL:LAUNCH:DENIED:PASS$"),
]


#: `logind`'s console prompt. It ends without a newline (it waits for a name
#: on the same line), so whichever task writes to serial next lands right
#: after it, and an anchored marker would not match.
LOGIN_PROMPT = re.compile(r"^LazyOS login: ", flags=re.MULTILINE)


def unglue(text: str) -> str:
    """Put a marker printed straight after the login prompt on its own line."""
    return LOGIN_PROMPT.sub("LazyOS login: \n", text)


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
        "--desktop",
        action="store_true",
        help="check the LAZYOS_DESKTOP=1 profile, which omits the evidence programs",
    )
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
    text = unglue(path.read_text(encoding="utf-8", errors="replace"))

    required = list(DESKTOP if args.desktop else REQUIRED)
    for pattern in args.require:
        required.append((pattern, pattern))

    ok = report("services", required, text)
    forbidden = FORBIDDEN + (DESKTOP_FORBIDDEN if args.desktop else [])
    ok &= report_absent("services", forbidden, text)
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
