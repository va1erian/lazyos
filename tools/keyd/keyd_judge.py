"""Judge the keyd secrets check (tools/keyd/run.py) from its serial logs and
the volume as the host reads it.

Each guest step prints `KEYD:T:<step>:PASS|FAIL:<detail>` (assets/keyd/secrets.rhai),
which the Terminal reports on serial as `TERM:OUT:...`. A boot's `EXPECT` lists
the detail each step must print; `keyd` itself reports `KEYD:SECRETS:PASS
count=<n>` at start.

App secrets (`os.lazy.keyd.appsecret.v1`) are judged beside them: the probe
apps print `KEYD:APP:<id>:<verb>:PASS|FAIL:<detail>` (assets/keyd/probe.sh,
assets/keyd/appprobe.rhai), `keyd` reports `KEYD:APPSECRETS:PASS count=<n>`
at start and `KEYD:SWEEP:PASS removed=<n>` when its sweep of removed apps'
secrets completes, and no secret value may be in any serial log.
"""

from __future__ import annotations

import re

MARKER = re.compile(r"TERM:OUT:KEYD:T:([a-z_]+):(PASS|FAIL):(\S*)")
APP_MARKER = re.compile(r"TERM:OUT:KEYD:APP:([a-z]+):([a-z]+):(PASS|FAIL):(\S*)")
APP_STARTED = re.compile(r"KEYD:APPSECRETS:(PASS|FAIL)(?: count=(\d+))?")
SWEEP = re.compile(r"KEYD:SWEEP:PASS removed=(\d+)")
#: What the probe apps keep (appprobe.rhai); it must never reach a log.
SECRET_VALUES = ("s3cret-probea", "s3cret-probeb")
#: What `probea` prints on its first run: `token` is not found (another app's
#: or nobody's), Put and Get round-trip, bad input and a 17th secret are refused.
FIRST_RUN = "ENOENT,ok,roundtrip,EINVAL,EINVAL,filled,ENOSPC,left=1"
LATER_RUN = "survived,names=1"
STARTED = re.compile(r"KEYD:SECRETS:(PASS|FAIL)(?: count=(\d+))?(?: reason=(.*?))? file=")

#: Per boot: (step, detail) in the order they run, and how many secrets keyd
#: found on disk when it started.
BOOTS = {
    "first": {
        "loaded": 0,
        "app_loaded": 0,
        "sweep": 0,
        "apps": [
            ("probea", "install", "PASS:ok"),
            ("probeb", "install", "PASS:ok"),
            ("probea", "run", "PASS:" + FIRST_RUN),
            ("probeb", "run", "PASS:" + FIRST_RUN),
            ("probeb", "remove", "PASS:ok"),
        ],
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
            ("app_denied", "EPERM"),
        ],
    },
    "second": {
        "loaded": 2,
        "app_loaded": 2,
        "sweep": 1,
        "apps": [("probea", "run", "PASS:" + LATER_RUN)],
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
        "app_loaded": 1,
        "sweep": 0,
        "apps": [("probea", "run", "PASS:" + LATER_RUN)],
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
    failures += judge_apps(name, log, expect)
    for bad in ("panic", "KEYD:SECRETS:WRITE:FAIL", "KEYD:APPSECRETS:WRITE:FAIL",
                "KEYD:SELFTEST:FAIL", *SECRET_VALUES):
        if bad in log:
            failures.append(f"{name}: serial log contains {bad!r}")
    return failures


def judge_apps(name: str, log: str, expect: dict) -> list[str]:
    """The app-secrets half of one boot: what `keyd` loaded, what the probe
    apps printed, and what its sweep removed."""
    failures = []
    started = APP_STARTED.search(log)
    if not started or started.group(1) != "PASS":
        failures.append(f"{name}: keyd did not report KEYD:APPSECRETS:PASS "
                        f"({started.group(0) if started else 'nothing'})")
    elif int(started.group(2)) != expect["app_loaded"]:
        failures.append(f"{name}: keyd loaded {started.group(2)} app secrets, "
                        f"expected {expect['app_loaded']}")
    seen = [(app, verb, f"{outcome}:{detail}")
            for app, verb, outcome, detail in APP_MARKER.findall(log)]
    if seen != expect["apps"]:
        failures.append(f"{name}: probe apps printed {seen}, expected {expect['apps']}")
    sweeps = SWEEP.findall(log)
    # The first completed sweep is the one that counts; the next is an hour on.
    if not sweeps or int(sweeps[0]) != expect["sweep"]:
        failures.append(f"{name}: sweep removed {sweeps[:1] or 'nothing (never completed)'}, "
                        f"expected {expect['sweep']}")
    return failures


def judge_app_file(tree: dict[str, tuple[str, ...]], appsecrets: bytes, key: bytes) -> list[str]:
    """`appsecrets` after the first boot: root's 0600 file of its own, a
    version header, and no secret, app or name in the clear."""
    failures = []
    node = tree.get("/conf/svc/keyd/appsecrets")
    if node is None:
        failures.append("files: /conf/svc/keyd/appsecrets does not exist")
    else:
        kind, mode, uid = node[0], node[1], node[2]
        if kind != "f" or int(mode, 8) & 0o7777 != 0o600 or uid != "0":
            failures.append(f"files: appsecrets is {kind} mode={mode} uid={uid}, "
                            "expected a 0600 file of uid 0")
    if not appsecrets.startswith(b"LZAPSECR"):
        failures.append("files: appsecrets does not start with its magic")
    elif appsecrets[8:10] != b"\x01\x00":
        failures.append(f"files: appsecrets version is {appsecrets[8:10]!r}, expected 1")
    elif int.from_bytes(appsecrets[10:12], "little") != 2:
        failures.append("files: appsecrets does not hold the two probes' secrets")
    for clear in (b"s3cret", b"org.keyd", b"probea", b"token"):
        if clear in appsecrets:
            failures.append(f"files: {clear!r} is in the appsecrets file in the clear")
    if key and key in appsecrets:
        failures.append("files: the machine key is inside the appsecrets file")
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
