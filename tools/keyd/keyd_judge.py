"""Judge the keyd secrets check (tools/keyd/run.py) from its serial logs and
the volume as the host reads it.

Each guest step prints `KEYD:T:<step>:PASS|FAIL:<detail>` (assets/keyd/secrets.rhai),
which the Terminal reports on serial as `TERM:OUT:...`. A boot's `EXPECT` lists
the detail each step must print; `keyd` itself reports `KEYD:SECRETS:PASS
count=<n>` at start.
"""

from __future__ import annotations

import re

MARKER = re.compile(r"TERM:OUT:KEYD:T:([a-z_]+):(PASS|FAIL):(\S*)")
STARTED = re.compile(r"KEYD:SECRETS:(PASS|FAIL)(?: count=(\d+))?(?: reason=(.*?))? file=")

#: Per boot: (step, detail) in the order they run, and how many secrets keyd
#: found on disk when it started.
BOOTS = {
    "first": {
        "loaded": 0,
        "steps": [
            ("user_store", "stored"),
            ("user_list", "home"),
            ("bad_input", "EINVAL"),
            ("pmk_denied", "EPERM"),
            ("pmk_other", "EPERM"),
            ("system_denied", "EPERM"),
            ("system_list", ""),
            ("system_store", "stored"),
            ("system_list", "office"),
        ],
    },
    "second": {
        "loaded": 2,
        "steps": [
            ("user_list", "home"),
            ("system_list", "office"),
            ("pmk_denied", "EPERM"),
            ("user_delete", "deleted"),
            ("user_list", ""),
            ("system_delete", "deleted"),
            ("system_list", ""),
        ],
    },
    "third": {
        "loaded": 0,
        "steps": [("user_list", ""), ("system_list", "")],
    },
}


def judge_boot(name: str, log: str) -> list[str]:
    """The failures of one boot's serial log."""
    expect = BOOTS[name]
    failures = []
    started = STARTED.search(log)
    if not started or started.group(1) != "PASS":
        failures.append(f"{name}: keyd did not report KEYD:SECRETS:PASS "
                        f"({started.group(0) if started else 'nothing'})")
    elif int(started.group(2)) != expect["loaded"]:
        failures.append(f"{name}: keyd loaded {started.group(2)} secrets, "
                        f"expected {expect['loaded']}")
    seen = MARKER.findall(log)
    want = expect["steps"]
    if len(seen) != len(want):
        failures.append(f"{name}: {len(seen)} steps reported, expected {len(want)}: {seen}")
    for (step, outcome, detail), (want_step, want_detail) in zip(seen, want):
        if (step, outcome, detail) != (want_step, "PASS", want_detail):
            failures.append(f"{name}: {step} printed {outcome}:{detail}, "
                            f"expected PASS:{want_detail} ({want_step})")
    for bad in ("panic", "KEYD:SECRETS:WRITE:FAIL", "KEYD:SELFTEST:FAIL"):
        if bad in log:
            failures.append(f"{name}: serial log contains {bad!r}")
    return failures


def judge_files(tree: dict[str, tuple[str, ...]], secrets: bytes, key: bytes) -> list[str]:
    """What the sealed files look like on the volume after the first boot:
    root's, 0600, a 32-byte key, and no secret in the clear."""
    failures = []
    for path in ("/conf/svc/keyd/secrets", "/conf/svc/keyd/machine.key"):
        node = tree.get(path)
        if node is None:
            failures.append(f"files: {path} does not exist")
            continue
        kind, mode, uid = node[0], node[1], node[2]
        if kind != "f" or int(mode, 8) & 0o7777 != 0o600 or uid != "0":
            failures.append(f"files: {path} is {kind} mode={mode} uid={uid}, expected a 0600 file of uid 0")
    directory = tree.get("/conf/svc/keyd")
    if directory is not None and int(directory[1], 8) & 0o777 != 0o700:
        failures.append(f"files: /conf/svc/keyd has mode {directory[1]}, expected 0700")
    if len(key) != 32:
        failures.append(f"files: machine.key is {len(key)} bytes, expected 32")
    if not secrets.startswith(b"LZSECRT1"):
        failures.append("files: secrets does not start with its magic")
    for clear in (b"correct horse", b"home", b"office"):
        if clear in secrets:
            failures.append(f"files: {clear!r} is in the secrets file in the clear")
    if key and key in secrets:
        failures.append("files: the machine key is inside the secrets file")
    return failures
