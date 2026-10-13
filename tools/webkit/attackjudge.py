"""The verdict of the browser's sandbox attack harness (issue #832).

``tools/abi/fixtures/src/sandboxprobe.rs`` runs as the WebProcess and as the NetworkProcess and
prints one line per attempt (``ATTACK:<role>:<probe>:BLOCKED|ALLOWED:<detail>``). This module
holds the table of what each role must be refused and what it must still be allowed, and
judges a serial log against it:

* every probe of the table is present exactly once per role, with the expected verdict;
* the process runs under the label the harness expects (``app:os.lazy.webkit/webprocess`` for
  the browser, ``app:os.lazy.sandbox-web`` for the kernel-only mode);
* each refusal a kernel gate makes is also a ``LABEL:DENY`` line of that label for that
  operation (the trace of ``LAZYOS_LABEL_TRACE=1``): the attempt was *refused by the policy*,
  not merely a failure that looked like one;
* the two calls that are not implemented (``ptrace``, ``process_vm_readv``) are ``ENOSYS``.

Like ``tools/accounts/attack_judge.py`` the expectation of an attack is ``blocked``; a probe
that is allowed where it must be blocked is a ``BREACH`` and fails the run.
"""

from __future__ import annotations

import re
from dataclasses import dataclass

#: One probe line: role, probe, verdict, detail (an errno or `ok`).
PROBE = re.compile(r"^ATTACK:(?P<role>[a-z]+):(?P<probe>[a-z_0-9]+):(?P<verdict>BLOCKED|ALLOWED):(?P<detail>\S+)\s*$", re.M)
#: The label the process reports for itself.
LABEL_LINE = re.compile(r"^ATTACK:(?P<role>[a-z]+):LABEL:(?P<label>\S.*?)\s*$", re.M)
DONE_LINE = re.compile(r"^ATTACK:(?P<role>[a-z]+):DONE\s*$", re.M)
#: The kernel's refusal trace.
DENY = re.compile(r"LABEL:DENY label=(?P<label>\S+) (?P<what>.*?)\s*$", re.M)
DRIVER = re.compile(r"^ATTACK:driver:(?P<what>\S+?)(?::(?P<rest>.*?))?\s*$", re.M)

ROLES = ("webprocess", "networkprocess")

#: The label of each role per mode: the real browser, and the kernel-only harness.
LABELS = {
    "browser": {"webprocess": "app:os.lazy.webkit/webprocess",
                "networkprocess": "app:os.lazy.webkit/networkprocess"},
    "kernel": {"webprocess": "app:os.lazy.sandbox-web", "networkprocess": "app:os.lazy.sandbox-net"},
}


@dataclass(frozen=True)
class Expect:
    """What one probe must come to: ``allowed``, ``blocked`` by the kernel's gate (the ``LABEL:DENY``
    ``what`` must match ``trace``), or ``absent`` (``ENOSYS``, no gate involved)."""

    kind: str
    trace: str = ""
    #: For `allowed`: any outcome that is not a refusal counts (a connect that times out is
    #: still "the socket was made"), instead of only success.
    loose: bool = False
    #: The same, but only for the real browser's image, where the file may not be there yet
    #: (`netd` writes the resolver configuration when DHCP answers).
    browser_loose: bool = False


def allowed(loose: bool = False, browser_loose: bool = False) -> Expect:
    return Expect("allowed", loose=loose, browser_loose=browser_loose)


def blocked(trace: str) -> Expect:
    return Expect("blocked", trace)


ABSENT = Expect("absent")

#: Common to both roles: what an exploit reaches for.
_FILES = {
    "read_home_file": blocked(r"file=/\S+/\.profile access=stat"),
    "read_ssh_key": blocked(r"file=/\S+/\.ssh/id_rsa access=stat"),
    "read_system_binary": blocked(r"file=/system/bin/busybox access=stat"),
    "read_confd": blocked(r"file=/conf/confd\.db access=stat"),
    "read_passwd": blocked(r"file=/etc/passwd access=stat"),
    "read_tty": blocked(r"file=/dev/tty access="),
    "write_system": blocked(r"file=/system/bin/sandbox-breach access=(write|stat)"),
    "write_tmp": blocked(r"file=/tmp/sandbox-breach access=(write|stat)"),
    "unix_connect": blocked(r"file=/tmp/sandbox-probe\.sock access=write"),
}
_PROCESS = {
    "exec_busybox": blocked(r"spawn=(process|exec)"),
    "exec_native": blocked(r"spawn=(process|exec)"),
    "fork": blocked(r"spawn=process"),
    "signal_parent": blocked(r"signal=other"),
    "resolve_display": blocked(r"resolve=os\.lazy\.display\.v1"),
    "resolve_input": blocked(r"resolve=os\.lazy\.input\.v1"),
    "resolve_clipboard": blocked(r"resolve=os\.lazy\.clipboard\.v1"),
}
_ABSENT = {"ptrace": ABSENT, "process_vm_readv": ABSENT}

EXPECTED: dict[str, dict[str, Expect]] = {
    # Fonts, the machine's /proc and the random devices; nothing else, no network.
    "webprocess": {
        "read_fonts": allowed(),
        "read_proc_self": allowed(),
        "read_dev_urandom": allowed(),
        "read_resolv_conf": blocked(r"file=/etc/resolv\.conf access=stat"),
        "read_ca_bundle": blocked(r"file=/etc/ssl/certs/ca-certificates\.crt access=stat"),
        "write_profile": blocked(r"file=/\S+/\.apps(/os\.lazy\.webkit)? access=(write|stat)"),
        "inet_socket": blocked(r"net=inet"),
        "tcp_connect": blocked(r"net=inet"),
        "read_home_dir": blocked(r"file=/home/[^/ ]+ access=(stat|read)"),
        "write_home": blocked(r"file=/home/[^/ ]+(/sandbox-breach\.txt)? access=(write|stat)"),
        **_FILES, **_PROCESS, **_ABSENT,
    },
    # The resolver, the CA bundle, sockets and its own profile folder; not the fonts, not the home.
    "networkprocess": {
        "read_proc_self": allowed(),
        "read_dev_urandom": allowed(),
        "read_resolv_conf": allowed(browser_loose=True),
        "read_ca_bundle": allowed(browser_loose=True),
        "write_profile": allowed(),
        "inet_socket": allowed(),
        "tcp_connect": allowed(loose=True),
        "read_fonts": blocked(r"file=/system/share/fonts access=stat"),
        "read_home_dir": blocked(r"file=/home/[^/ ]+ access=read"),
        "write_home": blocked(r"file=/home/[^/ ]+(/sandbox-breach\.txt)? access=(write|stat)"),
        **_FILES, **_PROCESS, **_ABSENT,
    },
}

#: What a blocked probe's errno may be: the policy's `EPERM`, a Messenger refusal's `EACCES`,
#: `ENOSYS` for what does not exist.
POLICY_ERRNOS = {"1", "13"}
ABSENT_ERRNO = "38"


def judge(serial: str, mode: str = "kernel") -> tuple[list[str], list[str]]:
    """``(problems, rows)`` for one boot's serial log: ``problems`` is empty when every probe of
    both roles did what the table says; ``rows`` are the per-probe lines of the report."""
    labels = LABELS[mode]
    problems: list[str] = []
    rows: list[str] = []
    probes: dict[tuple[str, str], list[tuple[str, str]]] = {}
    for found in PROBE.finditer(serial):
        probes.setdefault((found["role"], found["probe"]), []).append((found["verdict"], found["detail"]))
    denies = [(m["label"], m["what"]) for m in DENY.finditer(serial)]
    for role in ROLES:
        reported = [m["label"] for m in LABEL_LINE.finditer(serial) if m["role"] == role]
        want_label = labels[role]
        if not reported:
            problems.append(f"{role}: never started (no ATTACK:{role}:LABEL line)")
            continue
        if reported[0] != want_label:
            problems.append(f"{role}: ran under {reported[0]!r}, not {want_label!r}")
        if not any(m["role"] == role for m in DONE_LINE.finditer(serial)):
            problems.append(f"{role}: did not finish its probes (no DONE line)")
        mine = [what for label, what in denies if label == want_label]
        for probe, expect in EXPECTED[role].items():
            seen = probes.get((role, probe), [])
            if len(seen) != 1:
                problems.append(f"{role}.{probe}: reported {len(seen)} times, want once")
                rows.append(f"{role:15} {probe:20} MISSING")
                continue
            verdict, detail = seen[0]
            ok, why = _check(expect, verdict, detail, mine, mode)
            rows.append(f"{role:15} {probe:20} {verdict:8} {detail:6} {'ok' if ok else 'FAIL: ' + why}")
            if not ok:
                breach = "BREACH " if expect.kind != "allowed" and verdict == "ALLOWED" else ""
                problems.append(f"{role}.{probe}: {breach}{why}")
        extra = sorted(p for (r, p) in probes if r == role and p not in EXPECTED[role])
        for probe in extra:
            problems.append(f"{role}.{probe}: not in the judge's table")
    return problems, rows


def _check(expect: Expect, verdict: str, detail: str, denies: list[str], mode: str) -> tuple[bool, str]:
    if expect.kind == "allowed":
        if verdict != "ALLOWED":
            return False, f"was refused (errno {detail}) but must still work"
        loose = expect.loose or (expect.browser_loose and mode == "browser")
        if detail != "ok" and not loose:
            return False, f"failed with errno {detail}: it is not refused, but it did not work"
        return True, ""
    if verdict != "BLOCKED":
        return False, f"was not refused (ALLOWED:{detail})"
    if expect.kind == "absent":
        return (detail == ABSENT_ERRNO, f"errno {detail}, want ENOSYS")
    if detail not in POLICY_ERRNOS:
        return False, f"refused with errno {detail}, not the policy's"
    if not any(re.search(expect.trace, what) for what in denies):
        return False, f"no LABEL:DENY of the label matching /{expect.trace}/"
    return True, ""


def driver_problems(serial: str) -> list[str]:
    """The kernel-only harness's driver loaded both labels, started both roles and finished."""
    lines = {m["what"]: m for m in DRIVER.finditer(serial)}
    problems = []
    failed = [m for m in DRIVER.finditer(serial) if m["what"] == "FAIL"]
    for line in failed:
        problems.append(f"driver: {line['rest']}")
    if "DONE" not in lines:
        problems.append("driver: did not finish (no ATTACK:driver:DONE)")
    started = {m["rest"].split(":")[0] for m in DRIVER.finditer(serial) if m["what"] == "START"}
    for role in ROLES:
        if role not in started:
            problems.append(f"driver: never started {role}")
    return problems


def trace_suppressed(serial: str) -> bool:
    """The label channel's rate limit dropped lines (the probes are paced to avoid it)."""
    return bool(re.search(r"SUPPRESSED", serial))
