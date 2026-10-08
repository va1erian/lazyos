#!/usr/bin/env python3
"""The verdict of `tools/smb/run.py`: the guest's markers per check, each
server's record and directory, the capture, and the leak scan. Markers say
when; the servers' files and the wire are the verdict.
"""

from __future__ import annotations

import re
import zlib
from pathlib import Path

import checks as c

FILE_COMMANDS = {"CREATE", "READ", "WRITE", "QUERY_DIRECTORY", "SET_INFO"}


def segments(text: str) -> dict[str, str]:
    """The serial log split at each `SMBCHECK:<name>:<status>` line: the text
    each check printed, its exit status appended as the last line."""
    out, start = {}, 0
    for match in re.finditer(r"^SMBCHECK:([a-z0-9]+):(\d+)", text, re.M):
        out[match.group(1)] = text[start:match.start()] + f"\nSTATUS {match.group(2)}"
        start = match.end()
    return out


def judge_markers(text: str) -> list[str]:
    problems = []
    parts = segments(text)
    for check in c.CHECKS:
        part = parts.get(check.name)
        if part is None:
            problems.append(f"{check.name}: never finished")
            print(f"  {check.name:10} MISSING")
            continue
        passed = re.search(r"^SMB:PASS commands=\d+", part, re.M) is not None
        failed = re.search(r"^SMB:FAIL reason=(.*)$", part, re.M)
        status = re.search(r"STATUS (\d+)$", part).group(1)
        state = "PASS" if passed else f"FAIL ({failed.group(1).strip() if failed else '?'})"
        print(f"  {check.name:10} {state}")
        if check.passes and (not passed or status != "0"):
            problems.append(f"{check.name}: expected success, got {state} status {status}")
        if not check.passes:
            if passed or status == "0":
                problems.append(f"{check.name}: succeeded, but must be refused")
            elif not failed or check.reason.lower() not in failed.group(1).lower():
                problems.append(f"{check.name}: refused for {state}, expected a reason with {check.reason!r}")
    return problems


def _crc_line(text: str, verb: str, name: str) -> tuple[int, str] | None:
    match = re.search(rf"^SMB:{verb} {re.escape(name)} bytes=(\d+) crc=([0-9a-f]{{8}})", text, re.M)
    return (int(match.group(1)), match.group(2)) if match else None


def judge_transfer(text: str) -> list[str]:
    """The checksums `smb` printed against the bytes the harness knows."""
    part = segments(text).get("transfer", "")
    problems = []
    expect = [("GET", "hello.txt", c.HELLO), ("GET", "big.bin", c.BIG), ("PUT", "up.bin", c.UPLOAD),
              ("PUT", "seq.txt", c.SEQ), ("GET", "readme.md", b"# read me\n")]
    for verb, name, body in expect:
        got = _crc_line(part, verb, name)
        want = (len(body), f"{zlib.crc32(body):08x}")
        if got != want:
            problems.append(f"transfer: SMB:{verb} {name} reported {got}, expected {want}")
    if c.HELLO.decode().strip() not in part:
        problems.append("transfer: `get hello.txt -` did not print the file")
    if not re.search(r"^SMB:LOGON user=\S+ domain=LAZYNAS signing=off spnego=yes", part, re.M):
        problems.append("transfer: the logon line is not domain=LAZYNAS signing=off spnego=yes")
    if "SMB:DIALECT 0x0210" not in part:
        problems.append("transfer: dialect 0x0210 was not reported")
    for name, line in (("signed", "signing=on"), ("sign", "signing=on"), ("raw", "spnego=no")):
        if line not in segments(text).get(name, ""):
            problems.append(f"{name}: the logon line lacks {line}")
    return problems


def judge_main_tree(root: Path) -> list[str]:
    """The main server's directory after the transfer check."""
    problems = []
    expect = {"hello.txt": c.HELLO, "big.bin": c.BIG, "newdir/up.bin": c.UPLOAD, "seq.txt": c.SEQ,
              "docs/readme.md": b"# read me\n"}
    for name, want in expect.items():
        path = root / name
        got = path.read_bytes() if path.is_file() else None
        ok = got == want
        print(f"  server {name:16} {'ok' if ok else 'DIFFERS'}")
        if not ok:
            problems.append(f"main server file {name}: {len(got) if got is not None else 'missing'} bytes, "
                            f"wanted {len(want)}")
    tree = sorted(p.relative_to(root).as_posix() for p in root.rglob("*"))
    want_tree = sorted(["big.bin", "docs", "docs/readme.md", "hello.txt", "newdir", "newdir/up.bin", "seq.txt"])
    if tree != want_tree:
        problems.append(f"main server tree {tree}, expected {want_tree}")
    return problems


def judge_records(records: dict[str, object], roots: dict[str, Path]) -> list[str]:
    """Each server's own record: logons, signatures, and that the refused
    sessions never reached a file."""
    problems = []
    signed = records["signed"]
    if signed.signed == 0 or signed.unsigned or signed.bad_signatures:
        problems.append(f"signed server: signed={signed.signed} unsigned={signed.unsigned} "
                        f"bad={signed.bad_signatures}")
    upload = roots["signed"] / "signed.bin"
    if not upload.is_file() or upload.read_bytes() != c.SIGNED_UPLOAD:
        problems.append("signed server: signed.bin is not the 100000-byte stream")
    sign = records["sign"]
    if sign.signed == 0 or sign.unsigned or sign.bad_signatures:
        problems.append(f"--sign server: signed={sign.signed} unsigned={sign.unsigned} bad={sign.bad_signatures}")
    main = records["main"]
    if main.signed:
        problems.append(f"main server: {main.signed} signed requests though nobody asked for signing")
    if not any(not ok for _, _, ok in main.logons):
        problems.append("main server: the wrong password never reached it")
    if not any(ok for _, _, ok in main.logons):
        problems.append("main server: no successful logon")
    for name in ("guest", "encrypt", "truncated", "smb3", "tamper"):
        events = records[name].events
        touched = [e for e in events if e[0] in FILE_COMMANDS and not (name == "tamper" and e[0] in
                                                                      ("CREATE", "READ"))]
        if not events:
            problems.append(f"{name} server: the client never connected")
        if touched:
            problems.append(f"{name} server: a refused session reached files: {touched[:3]}")
    if records["truncated"].logons:
        problems.append("truncated server: the client authenticated against a broken challenge")
    return problems


def leaks(paths: list[Path], secrets: list[bytes]) -> list[str]:
    """Every artifact scanned for every secret, raw and UTF-16LE."""
    problems = []
    for path in paths:
        if not path.is_file():
            continue
        data = path.read_bytes()
        for secret in secrets:
            for form in (secret, secret.decode().encode("utf-16-le")):
                if form and form in data:
                    problems.append(f"{path.name} contains a password")
    return problems
