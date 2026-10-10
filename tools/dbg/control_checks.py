"""The control tier's checks for `tools/dbg/run.py` (docs/dbgd-plan.md, v2):
restart, hot reload with commit, both rollbacks (an exit during the trial,
a binary that will not spawn), revert, and the refusals.

The reloaded service is `sysmond` (it serves only the kernel's statistics,
so nothing else in the boot depends on it); the committed binary is the
image's own `sysmond`, the crashing one the image's `flaky` (its first
attempt exits 3 after 0.2 s), both read from the image with `osread`.
"""

from __future__ import annotations

import time
from pathlib import Path

import hotreload
from dbgclient import DbgClient

SERVICE = "sysmond"
TRIAL_MS = 3000
DENIED, BAD_PARAMS = -32002, -32602


def service_row(dbg: DbgClient, name: str) -> dict:
    rows = dbg.call("msg.services")["services"]
    return next((r for r in rows if r["name"] == name), {})


def wait_running(dbg: DbgClient, name: str, not_pid: int, timeout: float = 15) -> dict:
    """`name`'s row once it runs with a pid other than `not_pid`."""
    end = time.monotonic() + timeout
    row: dict = {}
    while time.monotonic() < end:
        row = service_row(dbg, name)
        if row.get("state") == "running" and row.get("pid") not in (0, not_pid):
            return row
        time.sleep(0.5)
    return row


def starts(dbg: DbgClient, name: str) -> int:
    """How many times init logged starting `name`."""
    lines = dbg.call("log.tail", lines=2000, source="programs")["lines"]
    return sum(f"init: started {name} " in r.get("text", "") for r in lines)


def exercise_off(key: str, c, host: str, port: int) -> None:
    """An image without `LAZYOS_DBGD_CONTROL=1`: everything is refused."""
    with DbgClient(host, port, key) as dbg:
        c.raises("control.begin is refused with control off", DENIED,
                 lambda: dbg.call("control.begin", confirm="control"))
        c.raises("service.restart is refused with control off", DENIED,
                 lambda: dbg.call("service.restart", name=SERVICE))


def exercise(key: str, c, host: str, port: int, image: Path) -> None:
    good = hotreload.image_binary(image, SERVICE)
    try:
        crashing = hotreload.image_binary(image, SERVICE, program="flaky")
    except RuntimeError:
        crashing = None
    with DbgClient(host, port, key) as dbg:
        call = dbg.call
        # -- the gate ----------------------------------------------------
        c.raises("a control method before control.begin is refused", DENIED,
                 lambda: call("service.restart", name=SERVICE))
        c.raises("control.begin needs the confirmation word", BAD_PARAMS,
                 lambda: call("control.begin", confirm="yes"))
        c.check("control.begin opens control", call("control.begin", confirm="control")["control"])
        c.raises("messengerd cannot be restarted", DENIED,
                 lambda: call("service.restart", name="messengerd"))
        c.raises("dbgd cannot be replaced", DENIED,
                 lambda: call("service.upload", name="dbgd", offset=0, total=4, data="AAAA"))
        c.raises("an unknown service is refused by init", DENIED,
                 lambda: call("service.restart", name="nosuchd"))

        # -- restart -------------------------------------------------------
        # A pid is a task slot: the new run may get the same one back, so
        # count the supervisor's start lines instead.
        before = starts(dbg, SERVICE)
        stopped = call("service.restart", name=SERVICE)["stopped_pid"]
        after = wait_running(dbg, SERVICE, 0)
        end = time.monotonic() + 15
        while starts(dbg, SERVICE) <= before and time.monotonic() < end:
            time.sleep(0.5)
        c.check("service.restart starts the service again",
                stopped > 0 and starts(dbg, SERVICE) == before + 1
                and after.get("state") == "running", f"{before} starts, then {after}")

        # -- upload refusals -------------------------------------------------
        c.raises("chunks out of order are refused", BAD_PARAMS,
                 lambda: call("service.upload", name=SERVICE, offset=6, total=12, data="AAAA"))
        c.raises("a reload without a finished upload is refused", BAD_PARAMS,
                 lambda: call("service.reload", name=SERVICE, sha256="00" * 32))
        sha = hotreload.upload(dbg, SERVICE, good)
        c.raises("a digest that does not match is refused by init", DENIED,
                 lambda: call("service.reload", name=SERVICE, sha256="00" * 32))
        c.check("a refused reload changed nothing",
                hotreload.state_of(dbg, SERVICE) is None)

        # -- commit ------------------------------------------------------------
        verdict = hotreload.reload(dbg, SERVICE, good, TRIAL_MS)
        c.check("a good binary is committed after its trial",
                verdict["state"] == "committed" and verdict["sha256"] == sha, str(verdict))
        c.check("the reloaded service runs",
                service_row(dbg, SERVICE).get("state") == "running")

        # -- rollback: the new binary exits during the trial -------------------
        if crashing is not None:
            verdict = hotreload.reload(dbg, SERVICE, crashing, TRIAL_MS)
            c.check("a binary that exits in its trial is rolled back",
                    verdict["state"] == "rolled-back" and "exited" in verdict["detail"],
                    str(verdict))
            row = wait_running(dbg, SERVICE, 0)
            c.check("the image's binary runs again after the rollback",
                    row.get("state") == "running", str(row))
        else:
            c.check("the image has flaky for the crash rollback", False)

        # -- rollback: the new binary does not spawn ---------------------------
        junk = b"\x7fELF" + bytes(range(256)) * 8
        verdict = hotreload.reload(dbg, SERVICE, junk, TRIAL_MS)
        c.check("a binary that cannot spawn is rolled back",
                verdict["state"] == "rolled-back" and "spawn" in verdict["detail"], str(verdict))
        c.check("the service is back after a failed spawn",
                wait_running(dbg, SERVICE, 0).get("state") == "running")

        # -- revert -----------------------------------------------------------
        hotreload.reload(dbg, SERVICE, good, TRIAL_MS)
        call("service.revert", name=SERVICE)
        c.check("service.revert goes back to the image's binary",
                (hotreload.state_of(dbg, SERVICE) or {}).get("state") == "reverted")

        # -- the audit trail --------------------------------------------------
        log = call("log.tail", lines=2000, source="programs")["lines"]
        tags = {r.get("tag") for r in log}
        c.check("init logs each reload verdict",
                {"INIT:RELOAD:TRIAL", "INIT:RELOAD:COMMIT", "INIT:RELOAD:ROLLBACK",
                 "INIT:RELOAD:REVERT"} <= tags, str(sorted(t for t in tags if t and "RELOAD" in t)))
        c.check("dbgd records opening control as a security event",
                any(r.get("tag") == "DBGD:SECURITY" and "control opened" in r.get("text", "")
                    for r in log))

    # A new connection starts without control.
    with DbgClient(host, port, key) as dbg:
        c.raises("control does not carry over to the next connection", DENIED,
                 lambda: dbg.call("service.restart", name=SERVICE))
