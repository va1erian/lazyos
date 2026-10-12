#!/usr/bin/env python3
"""TCP throughput of a real LazyOS box on the LAN, over Wi-Fi (`wlan0`) or
wired, against this host (docs/perf/network.md, "Wi-Fi on real hardware").

`dbgd` cannot start programs, so a person types `netbulk` on the box; this
tool is everything around that:

1. reads the box over `dbgd` (read-only) and checks that the interface under
   test (`--iface`, default `wlan0`) holds its default route; with a cable in,
   the wired route (metric 100) beats Wi-Fi (600), so it says to unplug it and
   waits for the route to move;
2. serves `tools/net/bulkpeers.py`'s bulk server on this host's LAN address
   (all interfaces; no firewall or system setting is touched);
3. prints the exact command to type on the box;
4. collects both ends: the server's record (bytes, timing, the box's source
   address) and the box's `NETBULK:` lines read over `dbgd`; the cross-check
   fails when they disagree or the traffic left by another interface;
5. prints a table (`--json` for a machine-readable result on stdout).

    python tools/net/lan_bulk.py --box 192.168.1.81                  # the NUC over Wi-Fi
    python tools/net/lan_bulk.py --box 192.168.1.81 --iface eth0     # wired, same box
    python tools/net/lan_bulk.py --dry-run-qemu                      # no hardware: QEMU stands in

The key is `--key HEX`, `--key-file PATH` or `target/dbgd.key`. Exit status:
0 verified, 1 the cross-check found problems, 2 setup failed (box unreachable,
wrong route, cannot listen), 3 the box never connected.
"""

from __future__ import annotations

import argparse
import datetime
import ipaddress
import json
import secrets
import socket
import subprocess
import sys
import time
from pathlib import Path

HERE = Path(__file__).resolve().parent
ROOT = HERE.parent.parent
sys.path.insert(0, str(HERE))
sys.path.insert(0, str(ROOT / "tools" / "dbg"))
import lan_bulk_judge as judge  # noqa: E402
import lan_bulk_qemu as qemu  # noqa: E402
from bulkpeers import BulkServer  # noqa: E402
from dbgclient import DbgClient, DbgError, default_key  # noqa: E402
from lanbox import (BoxSnapshot, GuestLog, check_route, netbulk_command, parse_output,  # noqa: E402
                    read_box, read_out_file, wait_until)

PORT = 47810
FIREWALL_LINE = ("FIREWALL: if Windows asks to allow Python on the network, tick *Private networks* and "
                 "Allow (this tool changes no firewall rule or setting).")


class Out:
    """Progress goes to stdout, or to stderr under `--json` (stdout is then the result)."""

    def __init__(self, json_mode: bool):
        self.stream = sys.stderr if json_mode else sys.stdout

    def __call__(self, text: str = "") -> None:
        print(text, file=self.stream, flush=True)


# -- this host -----------------------------------------------------------------

def route_source(dest: str) -> str:
    """The local address the OS would use towards `dest` (nothing is sent)."""
    with socket.socket(socket.AF_INET, socket.SOCK_DGRAM) as s:
        s.connect((dest, 9))
        return s.getsockname()[0]


def host_adapters() -> list[dict]:
    """`{ip, name, media, profile}` of this Windows host's IPv4 addresses
    (read-only queries; empty elsewhere or when PowerShell fails)."""
    if sys.platform != "win32":
        return []
    script = ("Get-NetIPAddress -AddressFamily IPv4 | ForEach-Object { "
              "$p = Get-NetConnectionProfile -InterfaceIndex $_.InterfaceIndex -ErrorAction SilentlyContinue; "
              "$a = Get-NetAdapter -InterfaceIndex $_.InterfaceIndex -ErrorAction SilentlyContinue; "
              "'{0}|{1}|{2}|{3}' -f $_.IPAddress,$_.InterfaceAlias,$a.PhysicalMediaType,$p.NetworkCategory }")
    try:
        done = subprocess.run(["powershell", "-NoProfile", "-NonInteractive", "-Command", script],
                              capture_output=True, text=True, timeout=30)
    except (OSError, subprocess.TimeoutExpired):
        return []
    rows = []
    for line in done.stdout.splitlines():
        parts = line.strip().split("|")
        if len(parts) == 4:
            rows.append(dict(zip(("ip", "name", "media", "profile"), parts)))
    return rows


def prefer_wired(ip: str, adapters: list[dict], network: str) -> str:
    """`ip`, or a wired address of this host in `network` when `ip` is on a
    Wi-Fi adapter: the host's own radio would share the air with the box."""
    net = ipaddress.ip_network(network, strict=False)
    mine = next((a for a in adapters if a["ip"] == ip), None)
    if mine is None or "802.11" not in mine["media"]:
        return ip
    for a in adapters:
        if "802.11" not in a["media"] and a["media"] and ipaddress.ip_address(a["ip"]) in net:
            return a["ip"]
    return ip


def describe_host_address(ip: str, adapters: list[dict]) -> list[str]:
    """Notes on the host address the box will connect to."""
    mine = next((a for a in adapters if a["ip"] == ip), None)
    notes = []
    if mine:
        wifi = "802.11" in mine["media"]
        notes.append(f"this host: {ip} on '{mine['name']}' ({'Wi-Fi' if wifi else mine['media'] or 'adapter'}, "
                     f"network profile {mine['profile'] or 'unknown'})")
        if wifi:
            notes.append("WARNING: that adapter is Wi-Fi: both ends of the path share the air, and the figure "
                         "is lower than the box's link allows. Prefer a wired host address (--host-addr).")
        if mine["profile"] == "Public":
            notes.append("WARNING: the network profile is Public: Windows blocks inbound connections to "
                         "Python there unless an allow rule exists, and the box will not connect.")
    else:
        notes.append(f"this host: {ip}")
    others = [a for a in adapters if a["ip"] != ip and a["ip"].rsplit(".", 1)[0] == ip.rsplit(".", 1)[0]]
    for a in others:
        notes.append(f"also on this subnet: {a['ip']} ('{a['name']}', {a['media'] or 'adapter'}); "
                     f"any of them works, the server listens on every interface")
    return notes


# -- dbgd ------------------------------------------------------------------------

class Link:
    """A `dbgd` connection that reconnects: a guest that is busy booting, or a
    Wi-Fi link that blips, must not end the run."""

    def __init__(self, host: str, port: int, key: str, connect_timeout: float):
        self.host, self.port, self.key = host, port, key
        self.client: DbgClient | None = None
        self.first_timeout = connect_timeout

    def _connect(self, timeout: float) -> DbgClient:
        end = time.monotonic() + timeout
        while True:
            client = DbgClient(self.host, self.port, self.key, timeout=20)
            try:
                client.connect()
                return client
            except (OSError, DbgError):
                client.close()
                if time.monotonic() >= end:
                    raise
                time.sleep(3)

    def open(self) -> None:
        """Connect for the first time, retrying while the box boots."""
        self.client = self._connect(self.first_timeout)

    def _retry(self, fn):
        last: Exception | None = None
        for _ in range(4):
            try:
                if self.client is None:
                    self.client = self._connect(15)
                return fn(self.client)
            except (OSError, DbgError) as error:
                last = error
                self.close()
                time.sleep(2)
        raise last  # type: ignore[misc]

    def read(self) -> BoxSnapshot:
        return self._retry(read_box)

    def poll_guest(self, guest: GuestLog) -> None:
        self._retry(guest.poll)

    def read_file(self, path: str) -> str | None:
        return self._retry(lambda client: read_out_file(client, path))

    def close(self) -> None:
        if self.client is not None:
            self.client.close()
            self.client = None


def wait_route(link: Link, iface: str, wait_s: float, say: Out) -> tuple[BoxSnapshot, list[str]]:
    """The box's snapshot once `iface` holds its default route, or the last one
    and the problems after `wait_s` seconds."""
    end = time.monotonic() + wait_s
    told = False
    while True:
        snap = link.read()
        problems, hints = check_route(snap, iface)
        if not problems:
            return snap, []
        if not told:
            say(f"ROUTE: {iface} will not carry the traffic yet:")
            for p in problems:
                say(f"  - {p}")
            for h in hints:
                say(f"  => {h}")
            if wait_s > 0:
                say(f"  waiting up to {wait_s:.0f} s for the route to move ...")
            told = True
        if time.monotonic() >= end:
            return snap, problems
        time.sleep(3)


def link_text(iface) -> str:
    return iface.wifi.describe() if iface and iface.wifi else (iface.kind if iface else "")


# -- the run -----------------------------------------------------------------------

def parse(argv: list[str] | None) -> argparse.Namespace:
    p = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    p.add_argument("--box", default="192.168.1.81", help="the box's address (its dbgd and its LAN address)")
    p.add_argument("--dbgd-port", type=int, default=9701)
    p.add_argument("--key", help="dbgd key, hex")
    p.add_argument("--key-file", type=Path, help="a file holding the dbgd key (default target/dbgd.key)")
    p.add_argument("--iface", help="the interface under test (default wlan0; eth0 with --dry-run-qemu)")
    p.add_argument("--port", type=int, default=PORT, help="the server's TCP port")
    p.add_argument("--bind", default="0.0.0.0", help="the address to listen on")
    p.add_argument("--host-addr", help="the address the box should connect to (default: this host's "
                                       "address towards the box)")
    p.add_argument("--bytes", type=int, default=16 * 1024 * 1024, help="per transfer")
    p.add_argument("--rounds", type=int, default=2, help="PUT+GET pairs")
    p.add_argument("--seed", type=int, default=1)
    p.add_argument("--wait-route", type=float, default=180, help="seconds to wait for --iface to hold the route")
    p.add_argument("--connect-timeout", type=float, default=600, help="seconds to wait for the box to connect")
    p.add_argument("--run-timeout", type=float, default=900, help="seconds the transfers may take once begun")
    p.add_argument("--tolerance", type=float, default=0.25, help="allowed host/guest timing difference")
    p.add_argument("--out-file",
                   help="where the typed command leaves netbulk's output for dbgd to read (under /tmp; "
                        "default: a name unique to this run, so an earlier run's file is never mistaken)")
    p.add_argument("--no-out-file", action="store_true",
                   help="type the bare netbulk command (a console image's log holds every line)")
    p.add_argument("--server-only", action="store_true", help="do not read the box's NETBULK lines")
    p.add_argument("--link-note", default="", help="free text for the link column (e.g. 'legacy 54M, 20 MHz')")
    p.add_argument("--label", default="", help="the box's name in the results row (e.g. 'AMD NUC')")
    p.add_argument("--json", action="store_true", help="print the result as JSON on stdout")
    p.add_argument("--dry-run-qemu", action="store_true", help="a QEMU guest stands in for the box")
    p.add_argument("--no-build", action="store_true", help="with --dry-run-qemu: use target/lazyos.img as is")
    p.add_argument("--accel", default="auto", choices=["auto", "none", "tcg", "whpx", "kvm"])
    p.add_argument("--qemu")
    p.add_argument("--memory")
    p.add_argument("--out", default="shots/lan_bulk")
    return p.parse_args(argv)


def load_key(args) -> str | None:
    if args.key:
        return args.key.strip()
    if args.key_file:
        return args.key_file.read_text().strip()
    return default_key()


def table_row(args, iface, link: str, summary: dict) -> str:
    cell = lambda k: f"{summary[k]['median']}" if k in summary else "-"  # noqa: E731
    label = args.label or ("QEMU dry run" if args.dry_run_qemu else args.box)
    where = "" if args.dry_run_qemu else f" ({args.box})"
    return (f"| {label}{where} | {iface.name} | {link} | {cell('PUT')} | {cell('GET')} | "
            f"{datetime.date.today().isoformat()} |")


def main(argv: list[str] | None = None) -> int:
    args = parse(argv)
    args.out_file = args.out_file or f"/tmp/nb{secrets.token_hex(3)}.out"
    say = Out(args.json)
    dry = args.dry_run_qemu
    iface_name = args.iface or ("eth0" if dry else "wlan0")
    box, dbgd_port, key = args.box, args.dbgd_port, load_key(args)
    command_ip, expect_peer, bind = args.host_addr, None, args.bind
    proc = None
    out = Path(args.out) if Path(args.out).is_absolute() else ROOT / args.out
    if dry:
        box, dbgd_port, key = "127.0.0.1", qemu.DBGD_HOST_PORT, qemu.DRY_KEY
        command_ip, expect_peer, bind = qemu.GATEWAY, qemu.SERVER_SEES, "127.0.0.1"
    if not key:
        say("lan_bulk: no dbgd key: pass --key / --key-file, or have target/dbgd.key")
        return 2
    command = None
    if dry:
        command = netbulk_command(command_ip, args.port, args.bytes, args.seed, args.rounds,
                                  None if args.no_out_file else args.out_file)
        if not args.no_build and (error := qemu.build()):
            say(f"lan_bulk: {error}")
            return 2
        if not qemu.IMAGE.is_file():
            say(f"lan_bulk: {qemu.IMAGE} is missing (drop --no-build)")
            return 2
        say(f"DRY RUN: QEMU will boot {qemu.IMAGE.name} and type: {command}")

    def launch() -> None:
        nonlocal proc
        proc = qemu.start(command, out, args.accel, args.qemu, args.memory)

    try:
        return run(args, say, iface_name, box, dbgd_port, key, command_ip, expect_peer, bind, dry,
                   launch if dry else None)
    except (OSError, DbgError) as error:
        say(f"lan_bulk: lost the box over dbgd: {type(error).__name__}: {error}")
        return 2
    finally:
        if proc is not None:
            qemu.stop(proc)


def start_server(args, say: Out, bind: str) -> BulkServer | None:
    """The bulk server, listening; None when it cannot. Windows asks about the
    firewall at the first listen, and the person should answer while there is
    still nothing to type."""
    server = BulkServer(args.port, host=bind)
    for _, _, seed in judge.expected_plan(args.rounds, args.seed):
        server.expected(args.bytes, seed)
    say(FIREWALL_LINE)
    try:
        server.start()
    except OSError as error:
        say(f"lan_bulk: cannot listen on {bind}:{args.port}: {error}")
        return None
    return server


def run(args, say: Out, iface_name: str, box: str, dbgd_port: int, key: str, command_ip: str | None,
        expect_peer: str | None, bind: str, dry: bool, launch=None) -> int:
    link = Link(box, dbgd_port, key, 400 if dry else 20)
    server = None
    guest: GuestLog | None = None
    if launch is not None:
        # The guest types its command as soon as it is up, so the server must
        # already be listening (a refused connect hangs QEMU's user network).
        if (server := start_server(args, say, bind)) is None:
            return 2
        launch()
    try:
        link.open()
    except (OSError, DbgError) as error:
        say(f"lan_bulk: cannot reach dbgd at {box}:{dbgd_port}: {error}")
        say("          (is the box on, on the LAN, and running a LAZYOS_DBGD=1 image? is the key right?)")
        if server is not None:
            server.stop()
        return 2
    try:
        if dry:
            time.sleep(3)  # the stack's address settles after dbgd is up
        first = link.read()
        if server is None and (server := start_server(args, say, bind)) is None:
            return 2
        if not args.server_only:
            guest = GuestLog(0 if dry else first.log_total)

        snap, problems = wait_route(link, iface_name, 120 if dry else args.wait_route, say)
        say(f"BOX {box}: up {snap.uptime_ms / 1000:.0f} s")
        for line in snap.summary():
            say(f"  {line}")
        iface = snap.get(iface_name)
        if problems or iface is None:
            say(f"lan_bulk: {iface_name} does not carry the box's traffic; not starting")
            return 2
        peer = expect_peer or iface.addr
        if command_ip is None:
            try:
                command_ip = route_source(iface.addr)
            except OSError as error:
                say(f"lan_bulk: no route from this host to {iface.addr}: {error}")
                return 2
        if not dry:
            adapters = host_adapters()
            if not args.host_addr:
                command_ip = prefer_wired(command_ip, adapters, f"{iface.addr}/{iface.prefix or 24}")
            for note in describe_host_address(command_ip, adapters):
                say(f"  {note}")
        say(f"ROUTE OK: new connections from the box leave by {iface_name} ({iface.addr}), "
            f"which the server should see as {peer}")
        return measure(args, say, server, guest, link, iface_name, iface, peer, command_ip, first, dry)
    finally:
        if server is not None:
            server.stop()
        link.close()


def box_output(link: Link, guest: GuestLog | None, args) -> tuple[list, list, str]:
    """The box's own results: from its output file when the command wrote one,
    else the lines in its programs log. `(results, done, where)`."""
    if not args.no_out_file:
        try:
            text = link.read_file(args.out_file)
        except (OSError, DbgError):
            text = None
        if text:
            results, done = parse_output(text)
            if results or done:
                return results, done, args.out_file
    if guest is not None:
        try:
            link.poll_guest(guest)
        except (OSError, DbgError):
            pass
        results, done = guest.snapshot()
        return results, done, "dbgd programs log"
    return [], [], "none"


def measure(args, say: Out, server: BulkServer, guest: GuestLog | None, link: Link, iface_name: str,
            iface, peer: str, command_ip: str, before: BoxSnapshot, dry: bool) -> int:
    command = netbulk_command(command_ip, args.port, args.bytes, args.seed, args.rounds,
                              None if args.no_out_file else args.out_file)
    total = 2 * args.rounds
    say()
    if dry:
        say(f"WAITING for the QEMU session to type: {command}")
    else:
        say(f"ON THE BOX, in its Terminal, type (about {total * args.bytes / 2.5e6:.0f} to "
            f"{total * args.bytes / 0.5e6:.0f} s at 2.5 to 0.5 MB/s):")
        say(f"    {command}")
        say(f"WAITING for {iface_name} ({iface.addr}) to connect to {command_ip}:{args.port} "
            f"(up to {args.connect_timeout:.0f} s) ...")
    started = time.monotonic()
    last_check = started
    while not server.transfers:
        if time.monotonic() - started > args.connect_timeout:
            say("lan_bulk: nothing connected. The box could not reach this host's port, or the command was "
                "not typed.")
            say(f"  - allow Python on Private networks in the Windows Firewall prompt (or an inbound rule "
                f"for TCP {args.port}) and run again;")
            say(f"  - check the command used {command_ip} (an address of this host on the box's subnet);")
            say("  - a cable plugged in sends traffic by eth0, not Wi-Fi.")
            return 3
        if time.monotonic() - last_check > 20:
            last_check = time.monotonic()
            try:
                if guest is not None:
                    link.poll_guest(guest)
                problems, _ = check_route(link.read(), iface_name)
                if problems:
                    say(f"  (route changed while waiting: {problems[0]})")
            except (OSError, DbgError):
                pass
        time.sleep(0.5)
    say("CONNECTED: the transfers have begun ...")
    finished = lambda: sum(1 for t in server.transfers if t.done) >= total  # noqa: E731
    if not wait_until(finished, args.run_timeout):
        say("lan_bulk: the transfers did not finish in time")
    # The box prints its `done` line after the last transfer; give the file
    # (or the log) a moment, then take what there is.
    if not args.server_only:
        wait_until(lambda: bool(box_output(link, guest, args)[1]), 20, step=1.0)
    try:
        after = link.read()
    except (OSError, DbgError):
        after = before
    results, done, where = (box_output(link, guest, args) if not args.server_only else ([], [], "none"))
    rows, problems, warnings = judge.cross_check(
        results, list(server.transfers), size=args.bytes, rounds=args.rounds, seed=args.seed,
        peer=peer, tolerance=args.tolerance, server_only=args.server_only)
    if results and len(results) == total and not done:
        problems.append("the box printed no `NETBULK:native:done` line")
    elif done and not done[-1].ok:
        problems.append("the box's netbulk said done ok=false")
    summary = judge.summarise(rows)
    link_before, link_after = link_text(before.get(iface_name)), link_text(after.get(iface_name))
    link_desc = link_after + (f", {args.link_note}" if args.link_note else "")
    verdict = "FAIL" if problems else "PASS"

    say()
    say(judge.table(rows))
    say()
    say(f"LINK {iface_name}: before: {link_before}; after: {link_after}")
    say(f"BOX OUTPUT from: {where} ({len(results)} transfer lines, {len(done)} done)")
    for kind, s in summary.items():
        say(f"{kind}: median {s['median']} MB/s (mean {s['mean']}, min {s['min']}, max {s['max']}, n={s['n']})")
    say(f"({args.bytes} bytes per transfer; MB/s = 10^6 bytes/s, the host server's figure)")
    for w in warnings:
        say(f"WARNING: {w}")
    for p in problems:
        say(f"FAIL: {p}")
    say(f"LAN_BULK:{verdict}")
    if verdict == "PASS":
        say("results row: " + table_row(args, iface, link_desc, summary))
    if args.json:
        print(json.dumps({
            "verdict": verdict, "box": args.box if not dry else "qemu", "iface": iface_name,
            "address": iface.addr, "server_saw": peer, "host_address": command_ip, "port": args.port,
            "command": command, "bytes": args.bytes, "rounds": args.rounds, "seed": args.seed,
            "date": datetime.date.today().isoformat(), "link_before": link_before, "link_after": link_after,
            "link_note": args.link_note, "box_output_from": where, "rows": judge.rows_json(rows),
            "summary": summary, "problems": problems, "warnings": warnings}, indent=1))
    return 1 if problems else 0


if __name__ == "__main__":
    sys.exit(main())
