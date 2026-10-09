#!/usr/bin/env python3
"""Boot a `LAZYOS_DBGD=1` image in QEMU and check every `dbgd` method over
user-mode networking (docs/dbgd-plan.md, issue #701).

1. `cargo build` with `LAZYOS_SERVICES=1 LAZYOS_NETD=1 LAZYOS_DBGD=1`
   (`LAZYOS_DBGD_PEER=10.0.2.2`: QEMU's user network shows a forwarded
   connection as coming from the gateway address).
2. Boot headless with the guest's 9701 forwarded to the host.
3. Connect as `DbgClient`: the handshake (right key, wrong key, a method
   before `auth`, the peer lock), every method's shape, a live
   `log.follow`, and the refusals (paths outside the allowlist, bad params,
   unknown methods).

    python tools/dbg/run.py                 # build, boot, judge
    python tools/dbg/run.py --no-build      # the image already built
    python tools/dbg/run.py --accel none

Exit status is non-zero on any failed check; logs go to `shots/dbg`.
"""

from __future__ import annotations

import argparse
import json
import os
import socket
import subprocess
import sys
import time
from pathlib import Path

HERE = Path(__file__).resolve().parent
ROOT = HERE.parent.parent
sys.path.insert(0, str(HERE))
from dbgclient import DbgClient, DbgError, default_key  # noqa: E402

PY = sys.executable
IMAGE = ROOT / "target" / "lazyos.img"
PORT = 9701
HOST_PORT = 19701


def build(peer: str | None, usb: bool) -> str | None:
    env = dict(os.environ, LAZYOS_SERVICES="1", LAZYOS_NETD="1", LAZYOS_NETD_ARGS="demo=0",
               LAZYOS_DBGD="1", LAZYOS_RESET_OS="1")
    env.pop("LAZYOS_DESKTOP", None)
    if peer:
        env["LAZYOS_DBGD_PEER"] = peer
    if usb:
        env["LAZYOS_USB"] = "1"
    print("dbg: LAZYOS_SERVICES=1 LAZYOS_NETD=1 LAZYOS_DBGD=1 cargo build", flush=True)
    result = subprocess.run(["cargo", "build"], cwd=ROOT, env=env, capture_output=True, text=True)
    if result.returncode != 0:
        return "cargo build failed:\n" + result.stderr[-4000:]
    return None if IMAGE.is_file() else f"{IMAGE} was not built"


class Checks:
    def __init__(self) -> None:
        self.failed: list[str] = []
        self.passed = 0

    def check(self, name: str, ok: bool, detail: str = "") -> bool:
        if ok:
            self.passed += 1
            print(f"  ok    {name}")
        else:
            self.failed.append(name)
            print(f"  FAIL  {name} {detail}")
        return ok

    def raises(self, name: str, code: int, fn) -> None:
        try:
            fn()
        except DbgError as error:
            self.check(name, error.code == code, f"(code {error.code}: {error.message})")
        except Exception as error:  # noqa: BLE001
            self.check(name, False, f"({type(error).__name__}: {error})")
        else:
            self.check(name, False, "(no error)")


def wait_ready(log: Path, timeout: float) -> bool:
    end = time.time() + timeout
    while time.time() < end:
        if log.is_file() and "DBGD:READY" in log.read_text(errors="replace"):
            return True
        time.sleep(1)
    return False


def exercise(key: str, c: Checks, usb: bool) -> None:
    host = "127.0.0.1"

    # -- the handshake and its refusals --------------------------------
    with DbgClient(host, HOST_PORT, key) as dbg:
        c.check("hello names the protocol", dbg.hello.get("proto") == "lazyos-dbg/1")
        c.check("server proved the key", True)
    bare = DbgClient(host, HOST_PORT, key)
    bare.open()
    c.raises("a method before auth is refused", -32001, lambda: bare.call("tasks.list"))
    bare.close()
    wrong = DbgClient(host, HOST_PORT, "00" * 16)
    c.raises("a wrong key is refused", -32001, wrong.connect)
    wrong.close()
    time.sleep(1.2)  # the lockout after one failure is one second

    with DbgClient(host, HOST_PORT, key) as dbg:
        call = dbg.call
        c.check("ping", call("ping")["uptime_ms"] > 0)
        names = {m["name"] for m in call("methods")["methods"]}
        c.check("methods lists the table", {"log.tail", "usb.dump", "msg.registry"} <= names)

        # -- the boot log ----------------------------------------------
        tail = call("log.tail", lines=2000)
        text = "\n".join(r["text"] for r in tail["lines"])
        c.check("log.tail returns the boot log", tail["total"] > 0 and "HW:" in text)
        programs = call("log.tail", lines=2000, source="programs")
        c.check("the programs' own output is a log of its own",
                programs["source"] == "programs" and "DBGD:READY" not in text)
        ready = next((r for r in programs["lines"] if r.get("tag") == "DBGD:READY"), {})
        c.check("log lines split into tag and fields",
                ready.get("fields", {}).get("port") == str(PORT), str(ready))
        c.check("log.tail honours lines", len(call("log.tail", lines=3)["lines"]) <= 3)
        hw = call("hwreport")
        c.check("hwreport has HW: verdicts",
                any(v["text"].startswith("HW:") for v in hw["verdicts"]))

        # -- the scheduler and memory ------------------------------------
        tasks = call("tasks.list")["tasks"]
        task_names = {t["name"] for t in tasks}
        c.check("tasks.list shows init and dbgd",
                any("init" in n for n in task_names) and any("dbgd" in n for n in task_names),
                str(sorted(task_names)))
        info = call("sysinfo")
        c.check("sysinfo has memory and uptime",
                info["ticks"] > 0 and info["memory"]["frames_total"] > 0)
        mem = call("mem.stats")
        c.check("mem.stats has frame and heap counters",
                mem["frames_live"] > 0 and mem["heap_total"] >= mem["heap_used"])
        fabric = call("fabric.stats")
        c.check("fabric.stats counts services", fabric["services"] > 0 and fabric["calls"] > 0)

        # -- devices and drivers -------------------------------------------
        devices = call("devices.list")["devices"]
        c.check("devices.list lists PCI functions", len(devices) > 0)
        try:
            drivers = call("drivers.list")["drivers"]
            c.check("drivers.list names the NIC driver",
                    any(d["class"] == "net" or d["driver"] for d in drivers), str(drivers))
        except DbgError as error:
            c.check("drivers.list answers or says unavailable", error.code == -32003, str(error))
        try:
            dump = call("usb.dump")
            tags = [r.get("tag") for r in dump["lines"]]
            c.check("usb.dump has the controller and its ports",
                    "USBD:DUMP:HC" in tags and "USBD:DUMP:PORT" in tags, str(tags))
            hc = next(r for r in dump["lines"] if r.get("tag") == "USBD:DUMP:HC")
            c.check("usb.dump splits registers into fields",
                    "usbsts" in hc["fields"] and "crcr" in hc["fields"], str(hc))
            c.check("usb.dump shows the attached keyboard slot",
                    any(r.get("tag") == "USBD:DUMP:DEV" for r in dump["lines"]), str(tags))
        except DbgError as error:
            c.check("usb.dump says unavailable without an xHCI",
                    not usb and error.code == -32003, str(error))

        # -- Messenger -----------------------------------------------------
        registry = {s["name"] for s in call("msg.registry")["services"]}
        c.check("msg.registry has the system services",
                "os.lazy.logd" in registry and any("netd" in n or "net" in n for n in registry),
                str(sorted(registry)))
        services = {s["name"]: s for s in call("msg.services")["services"]}
        c.check("msg.services shows netd and dbgd running",
                {"netd", "dbgd"} <= set(services) and services["dbgd"]["state"] == "running",
                str(services.get("dbgd")))
        topics = call("msg.topics")["topics"]
        c.check("msg.topics lists broker topics", len(topics) > 0)
        retained = next((t["topic"] for t in topics if t["retained"]), None)
        if retained:
            held = call("msg.topic", topic=retained)
            c.check("msg.topic returns a retained value", held["held"], str(held))
        c.raises("msg.topic refuses wildcards", -32602, lambda: call("msg.topic", topic="#"))

        # -- files -----------------------------------------------------------
        passwd = call("fs.read", path="/system/etc/passwd", len=256)
        c.check("fs.read reads an allowlisted file", "user" in passwd["text"], str(passwd)[:200])
        c.raises("fs.read refuses the boot config (it holds the key)", -32002,
                 lambda: call("fs.read", path="/boot/lazyos.cfg"))
        c.raises("fs.read refuses ..", -32002,
                 lambda: call("fs.read", path="/tmp/../boot/lazyos.cfg"))
        c.raises("fs.read refuses a home directory", -32002,
                 lambda: call("fs.read", path="/home/user/x"))

        # -- request errors --------------------------------------------------
        c.raises("an unknown method", -32601, lambda: call("fs.write"))
        c.raises("a parameter out of range", -32602, lambda: call("log.tail", lines=0))
        c.raises("an unknown parameter", -32602, lambda: call("ping", nope=1))
        dbg.send_line("this is not json")
        c.check("garbage is answered with a parse error",
                dbg.read_message().get("error", {}).get("code") == -32700)

        # -- the live stream -------------------------------------------------
        got: list[dict] = []
        answer = dbg.call("log.follow", lines=0, source="programs")
        c.check("log.follow starts", answer["following"] is True)
        call_id = dbg.next_id
        dbg.send_line(json.dumps({"jsonrpc": "2.0", "id": call_id, "method": "ping"}))
        dbg.next_id += 1
        for note in dbg.notifications(3.0):
            if note.get("method") == "log":
                got += note["params"]["lines"]
        c.check("log.follow streams the audit line of the next request",
                any(r.get("tag") == "DBGD:AUDIT" and r.get("fields", {}).get("method") == "ping"
                    for r in got), str(got[-3:]))
        call("log.unfollow")

    # -- the MCP bridge's TCP transport answers its two tools from dbgd ------
    sys.path.insert(0, str(ROOT / "tools" / "mcp"))
    from dbgd_session import DbgdSession  # noqa: E402

    session = DbgdSession(f"{host}:{HOST_PORT}", key)
    try:
        c.check("the MCP bridge's fabric_stats over TCP", session.fabric_stats()["services"] > 0)
        c.check("the MCP bridge's list_tasks over TCP", len(session.list_tasks()["tasks"]) > 0)
    finally:
        session.close()


def stop(proc: subprocess.Popen) -> None:
    """End the session runner and the QEMU it started (its whole tree)."""
    if os.name == "nt":
        subprocess.run(["taskkill", "/F", "/T", "/PID", str(proc.pid)],
                       capture_output=True, check=False)
    else:
        proc.terminate()
    try:
        proc.wait(timeout=20)
    except subprocess.TimeoutExpired:
        proc.kill()


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__,
                                     formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--no-build", action="store_true")
    parser.add_argument("--usb", action="store_true",
                        help="LAZYOS_USB=1 and a QEMU xHCI with a keyboard: judge usb.dump")
    parser.add_argument("--accel", default="auto")
    parser.add_argument("--memory")
    parser.add_argument("--qemu")
    parser.add_argument("--out", default=str(ROOT / "shots" / "dbg"))
    parser.add_argument("--timeout", type=float, default=240)
    args = parser.parse_args()
    out = Path(args.out)
    out.mkdir(parents=True, exist_ok=True)
    (out / "serial.log").unlink(missing_ok=True)
    if not args.no_build:
        error = build("10.0.2.2", args.usb)
        if error:
            print(error)
            return 1
    key = default_key()
    if not key:
        print("no target/dbgd.key: the build did not make a key")
        return 1
    session = out / "session.json"
    session.write_text(json.dumps([{"wait_for": "DBGD:READY", "timeout": args.timeout},
                                   {"wait": 120}, {"quit": True}]))
    command = [PY, str(ROOT / "tools" / "screenshot" / "qemu_session.py"), "--image", str(IMAGE),
               "--out", str(out), "--script", str(session), "--accel", args.accel,
               "--net", "--net-forward", f"{HOST_PORT}:{PORT}",
               "--fail-on", "PANIC", "--fail-on", "EXCEPTION"]
    if args.usb:
        for part in ("-device", "qemu-xhci", "-device", "usb-kbd"):
            command.append(f"--extra-arg={part}")
    if args.qemu:
        command += ["--qemu", args.qemu]
    if args.memory:
        command += ["--memory", args.memory]
    proc = subprocess.Popen(command, cwd=ROOT, stdout=subprocess.DEVNULL)
    checks = Checks()
    try:
        if not wait_ready(out / "serial.log", args.timeout):
            print("dbgd never printed DBGD:READY; see", out / "serial.log")
            return 1
        time.sleep(2)  # the stack's address and the listener settle
        exercise(key, checks, args.usb)
    except (OSError, DbgError, socket.timeout) as error:
        checks.check("the session ran to the end", False, f"({type(error).__name__}: {error})")
    finally:
        stop(proc)
    print(f"dbg: {checks.passed} passed, {len(checks.failed)} failed")
    return 1 if checks.failed else 0


if __name__ == "__main__":
    sys.exit(main())
