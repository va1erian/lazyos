"""The verdict of webd's sandbox attack harness (issue #832; the engine processes belong to webd, which
LazyWeb+ and the other web clients draw their pages with: docs/webd-plan.md).

``tools/abi/fixtures/src/sandboxprobe.rs`` runs as the WebProcess and as the NetworkProcess and
prints one line per attempt (``ATTACK:<role>:<probe>:BLOCKED|ALLOWED:<detail>``). This module
holds the table of what each role must be refused and what it must still be allowed, and
judges a serial log against it:

* every probe of the table is present exactly once per role, with the expected verdict;
* the process runs under the label the harness expects (``app:os.lazy.webd/webprocess`` for
  webd, ``app:os.lazy.sandbox-web`` for the kernel-only mode);
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

#: The apps whose engine processes the harness attacks: webd's package id (it starts the two programs under
#: labels of its own, `app:<id>/<name>`).
APPS = ("os.lazy.webd",)
DEFAULT_APP = APPS[0]


def labels(mode: str, app: str = DEFAULT_APP) -> dict[str, str]:
    """The label of each role: the real browser's (`app:<app>/<name>`), and the kernel-only harness's."""
    if mode == "browser":
        return {"webprocess": f"app:{app}/webprocess", "networkprocess": f"app:{app}/networkprocess"}
    return {"webprocess": "app:os.lazy.sandbox-web", "networkprocess": "app:os.lazy.sandbox-net"}


@dataclass(frozen=True)
class Expect:
    """What one probe must come to: ``allowed``, ``blocked`` by the kernel's gate (the ``LABEL:DENY``
    ``what`` must match ``trace``), or ``absent`` (``ENOSYS``, no gate involved)."""

    kind: str
    trace: str = ""
    #: For `allowed`: any outcome that is not a refusal counts (a connect that times out is
    #: still "the socket was made"), instead of only success.
    loose: bool = False
    #: Only for the real browser's image, where the file or folder may not be there yet (`netd` writes
    #: the resolver configuration when DHCP answers; webd makes Downloads at the first download):
    #: success, or `ENOENT` and nothing else (`ENOSPC`, `EIO`, `EROFS` are not "not there yet").
    browser_loose: bool = False


def allowed(loose: bool = False, browser_loose: bool = False) -> Expect:
    return Expect("allowed", loose=loose, browser_loose=browser_loose)


def blocked(trace: str) -> Expect:
    return Expect("blocked", trace)


ABSENT = Expect("absent")

#: Common to both roles: what an exploit reaches for.
_FILES = {
    "read_home_file": blocked(r"file=/home/[^/ ]+/\.profile access=stat"),
    "read_ssh_key": blocked(r"file=/home/[^/ ]+/\.ssh/id_rsa access=stat"),
    "read_system_binary": blocked(r"file=/system/bin/busybox access=stat"),
    "read_confd": blocked(r"file=/conf/confd\.db access=stat"),
    "read_passwd": blocked(r"file=/etc/passwd access=stat"),
    "read_tty": blocked(r"file=/dev/tty access="),
    "write_system": blocked(r"file=/system/bin/sandbox-breach access=(write|stat)"),
    "write_tmp": blocked(r"file=/tmp/sandbox-breach access=(write|stat)"),
    "unix_connect": blocked(r"file=/tmp/sandbox-probe\.sock access=write"),
}
#: A refused start is `spawn=process|exec` for a label of the kernel-only harness and `start=<call>` (the
#: system call that tried: `fork`, `vfork`, `execve`, ...) for a child label of a package.
_START = r"(spawn=(process|exec)|start=\w+)"
_PROCESS = {
    "exec_busybox": blocked(_START),
    "exec_native": blocked(_START),
    "fork": blocked(_START),
    "signal_parent": blocked(r"signal=other"),
    "resolve_display": blocked(r"resolve=os\.lazy\.display\.v1"),
    "resolve_input": blocked(r"resolve=os\.lazy\.input\.v1"),
    "resolve_clipboard": blocked(r"resolve=os\.lazy\.clipboard\.v1"),
    # webd's own services: a child label owns no names and has only the app's interfaces.
    "resolve_webd_view": blocked(r"resolve=app\.os\.lazy\.webd\.view\b"),
    "resolve_webd_light": blocked(r"resolve=app\.os\.lazy\.webd\.light\b"),
    # The light views' network-lock rule store: webd's own process alone writes it.
    "write_rules": blocked(r"file=/home/[^/ ]+/\.apps/os\.lazy\.webd/rules(/attack-probe)? access=(write|stat)"),
}
_ABSENT = {"ptrace": ABSENT, "process_vm_readv": ABSENT}

DOWNLOADS_DENY = r"file=/home/[^/ ]+/Downloads(/attack-probe)? access=(write|stat)"

EXPECTED: dict[str, dict[str, Expect]] = {
    # Fonts, the machine's /proc and the random devices; nothing else, no network.
    "webprocess": {
        "read_fonts": allowed(),
        "read_proc_self": allowed(),
        "read_dev_urandom": allowed(),
        "read_resolv_conf": blocked(r"file=/etc/resolv\.conf access=stat"),
        "read_ca_bundle": blocked(r"file=/etc/ssl/certs/ca-certificates\.crt access=stat"),
        # The children's folder is `.apps/os.lazy.webd/profiles` (P4) and the probe may be refused at
        # any level of it, so the pattern takes the folder and anything below it, in a home.
        "write_profile": blocked(r"file=/home/[^/ ]+/\.apps(/os\.lazy\.webd(/\S*)?)? access=(write|stat)"),
        "write_downloads": blocked(DOWNLOADS_DENY),
        "inet_socket": blocked(r"net=inet"),
        "inet_serve": blocked(r"net=inet"),
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
        # A download's `.part` file is written by the process that fetches it. The folder is made by
        # webd's own (unconfined) process at the first download; in the real webd's image the probe
        # runs before any, so a folder that is not there yet (ENOENT) is not a refusal.
        "write_downloads": allowed(browser_loose=True),
        "inet_socket": allowed(),
        "inet_serve": blocked(r"net=serve"),
        "tcp_connect": allowed(loose=True),
        "read_fonts": blocked(r"file=/system/share/fonts access=stat"),
        "read_home_dir": blocked(r"file=/home/[^/ ]+ access=read"),
        "write_home": blocked(r"file=/home/[^/ ]+(/sandbox-breach\.txt)? access=(write|stat)"),
        **_FILES, **_PROCESS, **_ABSENT,
    },
}

def expected(app: str = DEFAULT_APP) -> dict[str, dict[str, Expect]]:
    """The table for `app`: the network process alone may write the user's Downloads folder."""
    return {role: dict(rows) for role, rows in EXPECTED.items()}


#: What a blocked probe's errno may be: the policy's `EPERM`, a Messenger refusal's `EACCES`,
#: `ENOSYS` for what does not exist.
POLICY_ERRNOS = {"1", "13"}
ABSENT_ERRNO = "38"
#: ENOENT: a file or folder another process writes later (`Expect.browser_loose`).
NOT_THERE_ERRNO = "2"


def judge(serial: str, mode: str = "kernel", app: str = DEFAULT_APP) -> tuple[list[str], list[str]]:
    """``(problems, rows)`` for one boot's serial log: ``problems`` is empty when every probe of
    both roles did what the table says; ``rows`` are the per-probe lines of the report."""
    table = expected(app)
    role_labels = labels(mode, app)
    problems: list[str] = []
    rows: list[str] = []
    probes: dict[tuple[str, str], list[tuple[str, str]]] = {}
    for found in PROBE.finditer(serial):
        probes.setdefault((found["role"], found["probe"]), []).append((found["verdict"], found["detail"]))
    for role in ROLES:
        reported = [m["label"] for m in LABEL_LINE.finditer(serial) if m["role"] == role]
        want_label = role_labels[role]
        if not reported:
            problems.append(f"{role}: never started (no ATTACK:{role}:LABEL line)")
            continue
        if reported[0] != want_label:
            problems.append(f"{role}: ran under {reported[0]!r}, not {want_label!r}")
        if not any(m["role"] == role for m in DONE_LINE.finditer(serial)):
            problems.append(f"{role}: did not finish its probes (no DONE line)")
        windows = deny_windows(serial, role, want_label)
        for probe, expect in table[role].items():
            seen = probes.get((role, probe), [])
            if len(seen) != 1:
                problems.append(f"{role}.{probe}: reported {len(seen)} times, want once")
                rows.append(f"{role:15} {probe:20} MISSING")
                continue
            verdict, detail = seen[0]
            ok, why = _check(expect, verdict, detail, windows.get(probe, []), mode)
            rows.append(f"{role:15} {probe:20} {verdict:8} {detail:6} {'ok' if ok else 'FAIL: ' + why}")
            if not ok:
                breach = "BREACH " if expect.kind != "allowed" and verdict == "ALLOWED" else ""
                problems.append(f"{role}.{probe}: {breach}{why}")
        extra = sorted(p for (r, p) in probes if r == role and p not in table[role])
        for probe in extra:
            problems.append(f"{role}.{probe}: not in the judge's table")
    return problems, rows


def deny_windows(serial: str, role: str, label: str) -> dict[str, list[str]]:
    """The `LABEL:DENY` lines of `label` that belong to each probe of `role`: those printed after the
    role's previous `ATTACK:<role>:` line and before the probe's own. The kernel prints a refusal at the
    system call and the probe prints its line after its pause, so each probe's refusals sit just before
    it. Pairing them (rather than pooling every denial of the label) keeps one refusal from standing in
    for several probes that share a pattern (`fork`, `exec_busybox` and `exec_native`; the `net=inet`
    ones)."""
    probe_line = re.compile(rf"ATTACK:{role}:(?P<probe>[a-z_0-9]+):(BLOCKED|ALLOWED):")
    reset = re.compile(rf"ATTACK:{role}:LABEL:")
    windows: dict[str, list[str]] = {}
    window: list[str] = []
    for line in serial.splitlines():
        deny = DENY.search(line)
        if deny and deny["label"] == label:
            window.append(deny["what"])
            continue
        if reset.search(line):
            window = []
            continue
        found = probe_line.search(line)
        if found:
            windows[found["probe"]] = window
            window = []
    return windows


def _check(expect: Expect, verdict: str, detail: str, denies: list[str], mode: str) -> tuple[bool, str]:
    if expect.kind == "allowed":
        if verdict != "ALLOWED":
            return False, f"was refused (errno {detail}) but must still work"
        if detail == "ok" or expect.loose:
            return True, ""
        if expect.browser_loose and mode == "browser" and detail == NOT_THERE_ERRNO:
            return True, ""
        return False, f"failed with errno {detail}: it is not refused, but it did not work"
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
    """The label channel's rate limit dropped lines (the probes are paced to avoid it). Only the label
    trace's own notice (`LABEL:SUPPRESSED n=<count>`, `tracefmt`): another channel's says nothing about
    the denials."""
    return bool(re.search(r"LABEL:SUPPRESSED\b", serial))
