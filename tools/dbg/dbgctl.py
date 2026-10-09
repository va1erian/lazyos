#!/usr/bin/env python3
"""`dbgctl`: inspect a LazyOS box through its `dbgd` (docs/dbgd-plan.md).

    python tools/dbg/dbgctl.py --host 192.168.1.50 log --lines 100
    python tools/dbg/dbgctl.py --host 192.168.1.50 log --follow
    python tools/dbg/dbgctl.py tasks | sysinfo | mem | fabric | devices | drivers
    python tools/dbg/dbgctl.py usb | hw | msg-registry | msg-services | msg-topics
    python tools/dbg/dbgctl.py topic system/health/summary
    python tools/dbg/dbgctl.py cat /system/etc/passwd
    python tools/dbg/dbgctl.py call log.tail '{"lines": 5, "source": "kernel"}'
    python tools/dbg/dbgctl.py methods

`--host` defaults to 127.0.0.1 (a QEMU boot with `run_demo.py --dbgd`
forwards the port there), the key to `target/dbgd.key`. `--json` prints the
raw result of any command.
"""

from __future__ import annotations

import argparse
import json
import sys
import time
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
from dbgclient import DEFAULT_PORT, DbgClient, DbgError, default_key  # noqa: E402


def line_text(record: dict) -> str:
    return record.get("text", "")


def show_table(rows: list[dict], columns: list[str]) -> None:
    widths = [max(len(c), *(len(str(r.get(c, ""))) for r in rows)) if rows else len(c)
              for c in columns]
    print("  ".join(c.upper().ljust(w) for c, w in zip(columns, widths)))
    for row in rows:
        print("  ".join(str(row.get(c, "")).ljust(w) for c, w in zip(columns, widths)))


def run(args, dbg: DbgClient) -> object:
    cmd = args.command
    if cmd == "log":
        if args.follow:
            seen = 0
            def show(record):
                nonlocal seen
                seen += 1
                print(line_text(record), flush=True)
            try:
                dbg.follow(show, seconds=args.seconds, backlog=args.lines, source=args.source)
            except KeyboardInterrupt:
                pass
            return None
        result = dbg.call("log.tail", lines=args.lines, **({"source": args.source} if args.source else {}))
        if not args.json:
            for record in result["lines"]:
                print(line_text(record))
            return None
        return result
    if cmd == "tasks":
        return dbg.call("tasks.list")
    simple = {"sysinfo": "sysinfo", "mem": "mem.stats", "fabric": "fabric.stats",
              "devices": "devices.list", "drivers": "drivers.list", "usb": "usb.dump",
              "hw": "hwreport", "msg-registry": "msg.registry",
              "msg-services": "msg.services", "msg-topics": "msg.topics",
              "methods": "methods", "ping": "ping", "log-sources": "log.sources"}
    if cmd in simple:
        return dbg.call(simple[cmd])
    if cmd == "topic":
        return dbg.call("msg.topic", topic=args.name)
    if cmd == "cat":
        return dbg.call("fs.read", path=args.name, len=args.len)
    if cmd == "call":
        return dbg.call(args.name, **json.loads(args.params or "{}"))
    raise SystemExit(f"unknown command {cmd}")


def pretty(cmd: str, result: dict) -> None:
    tables = {"tasks": ("tasks", ["pid", "ppid", "state", "class", "cpu_ticks", "name"]),
              "devices": ("devices", ["id", "class", "vendor", "device", "owner", "rights"]),
              "drivers": ("drivers", ["id", "vendor", "device", "class", "driver", "state", "owner"]),
              "msg-registry": ("services", ["name", "owner_slot", "interfaces"]),
              "msg-services": ("services", ["name", "state", "pid", "restarts", "health"]),
              "msg-topics": ("topics", ["topic", "subscribers", "retained", "payload"])}
    if cmd in tables:
        key, columns = tables[cmd]
        show_table(result[key], columns)
    elif cmd in ("usb", "hw"):
        for record in result.get("lines", result.get("verdicts", [])):
            print(line_text(record))
    elif cmd == "cat" and result.get("kind") == "file":
        sys.stdout.write(result["text"])
    else:
        print(json.dumps(result, indent=2))


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__,
                                     formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--host", default="127.0.0.1")
    parser.add_argument("--port", type=int, default=DEFAULT_PORT)
    parser.add_argument("--key", help="hex key (default: target/dbgd.key)")
    parser.add_argument("--json", action="store_true", help="print the raw result")
    parser.add_argument("--lines", type=int, default=50)
    parser.add_argument("--follow", action="store_true", help="with `log`: stream new lines")
    parser.add_argument("--seconds", type=float, default=3600, help="with --follow: how long")
    parser.add_argument("--source", help="with `log`: a logd journal instead of the kernel log")
    parser.add_argument("--len", type=int, default=4096, help="with `cat`: bytes")
    parser.add_argument("command")
    parser.add_argument("name", nargs="?", help="topic, path or method")
    parser.add_argument("params", nargs="?", help="with `call`: params as JSON")
    args = parser.parse_args()
    key = args.key or default_key()
    if not key:
        print("no key: pass --key or build with LAZYOS_DBGD=1 (target/dbgd.key)", file=sys.stderr)
        return 2
    try:
        with DbgClient(args.host, args.port, key) as dbg:
            result = run(args, dbg)
    except DbgError as error:
        print(f"dbgctl: {error}", file=sys.stderr)
        return 1
    except OSError as error:
        print(f"dbgctl: cannot reach {args.host}:{args.port}: {error}", file=sys.stderr)
        return 1
    if result is not None:
        if args.json:
            print(json.dumps(result, indent=2))
        else:
            pretty(args.command, result)
    return 0


if __name__ == "__main__":
    sys.exit(main())
