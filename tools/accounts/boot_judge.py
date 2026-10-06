"""Judge the boots around an attack session: the machine must stop cleanly
after the attacks and come back up, after the clean stop and after a hard kill
(invariant 1 of docs/accounts-plan.md: no bricking).

Until the login screen ships (U0, #623) "comes back" is the desktop Terminal
answering a command (`ACCT:BOOT:OK`); the marker moves to the login screen's
when that lands.
"""

from __future__ import annotations

#: Printed by the Terminal when `echo ACCT:BOOT:OK` runs: the desktop is up
#: and a session can run a command.
BOOT_OK = "TERM:OUT:ACCT:BOOT:OK"
#: What a panic or a lost task leaves in the serial log.
BAD = ("panic", "TERM:PANIC", "BIND:FAIL", "SPAWN:FAIL")


def judge_stop(log: str) -> list[str]:
    """The attack boot's power-off must have run the orderly sequence."""
    failures = []
    for marker in ("INIT:SHUTDOWN:BEGIN", "INIT:SHUTDOWN:QUIESCED", "power: filesystems synced"):
        if marker not in log:
            failures.append(f"stop: missing {marker!r} (the machine did not shut down cleanly)")
    for line in ("power: watchdog expired", "power: sync failed"):
        if line in log:
            failures.append(f"stop: {line!r}")
    return failures


def judge_boot(log: str, label: str, after_hard_kill: bool = False) -> list[str]:
    """A boot after the attacks: up, answering, no panic; the volume is clean
    unless the boot before it was killed (a hard kill leaves it unclean on purpose)."""
    failures = []
    if "TERM:UP:PASS" not in log:
        failures.append(f"{label}: the desktop never came up (no TERM:UP:PASS)")
    if BOOT_OK not in log:
        failures.append(f"{label}: no {BOOT_OK} (the session could not run a command)")
    for bad in BAD:
        if bad in log:
            failures.append(f"{label}: serial log contains {bad!r}")
    unclean = "was not cleanly unmounted" in log
    if unclean and not after_hard_kill:
        failures.append(f"{label}: the OS volume was not clean after a clean power-off")
    if after_hard_kill and not unclean:
        failures.append(f"{label}: expected the unclean-unmount notice after a hard kill "
                        "(the harness did not really kill the machine)")
    return failures
