#!/usr/bin/env python3
"""Bulk TCP throughput between the guest and a host server
(docs/performance-plan.md P0 and P4).

1. Builds the Linux fixtures (`tools/abi/build.py`, for `netbulk-linux`) and a
   console image with the stack (`LAZYOS_CLI=1 LAZYOS_NETD=1`,
   `LAZYOS_NETD_ARGS=demo=0`, BusyBox as the shell).
2. Starts `bulkpeers.BulkServer` on 127.0.0.1:47810 (the guest's 10.0.2.2).
3. Types `netbulk` (the native socket service, `os.lazy.net.socket.v1`) and
   `netbulk-linux` (the kernel's `AF_INET` shim) into the shell: each sends
   (`PUT`) and receives (`GET`) `--bytes` per round.
4. The verdict: every transfer byte-exact by the server's own check (`PUT`)
   and the guest's (`GET`, which the server's record confirms by length),
   each guest marker `PASS`; with `--pcap`, the capture's TCP streams to the
   server are reassembled too. The throughput is printed and written to
   `<out>/bulk.json`: the host's figure for each transfer (first payload byte
   to end of stream) next to the guest's.

    python tools/net/bulk.py                      # 16 MiB each way, both paths
    python tools/net/bulk.py --bytes 33554432 --rounds 2
    python tools/net/bulk.py --no-build --accel none
    python tools/net/bulk.py --pcap               # also judge the capture (slower: QEMU writes every frame)
    python tools/net/bulk.py --no-build --image target/base.img   # A/B: an image copied from an earlier build

Exit status is non-zero on any failure; logs go to `shots/bulk`.
"""

from __future__ import annotations

import argparse
import json
import os
import re
import subprocess
import sys
from pathlib import Path

HERE = Path(__file__).resolve().parent
ROOT = HERE.parent.parent
sys.path.insert(0, str(HERE))
sys.path.insert(0, str(ROOT / "tools" / "abi"))
import bulkpeers  # noqa: E402
import busybox  # noqa: E402

PY = sys.executable
IMAGE = ROOT / "target" / "lazyos.img"
FIXTURE = ROOT / "target" / "abi" / "fixtures" / "netbulk.elf"
PORT = 47810
GATEWAY = "10.0.2.2"
PROMPT = "/ #"
PATHS = ("native", "linux")
#: The Linux fixture by its full path: the shell's `$PATH` (`/bin`) only maps
#: BusyBox's applets and the native programs.
COMMANDS = {"native": "netbulk", "linux": "/system/bin/netbulk-linux"}
MARKER = re.compile(r"^NETBULK:(native|linux):(PUT|GET):(PASS|FAIL)(.*)$", re.M)


def build() -> str | None:
    """Build the fixture and the image; an error message, or None."""
    FIXTURE.unlink(missing_ok=True)
    if subprocess.call([PY, str(ROOT / "tools" / "abi" / "build.py")], cwd=ROOT,
                       stdout=subprocess.DEVNULL) != 0 or not FIXTURE.is_file():
        return "tools/abi/build.py did not produce netbulk.elf (needs the x86_64-unknown-linux-musl target)"
    shell = busybox.ensure_busybox()
    if shell is None:
        return "no BusyBox for the console shell (see tools/abi/busybox.py)"
    env = dict(os.environ, LAZYOS_CLI="1", LAZYOS_NETD="1", LAZYOS_NETD_ARGS="demo=0",
               LAZYOS_RESET_OS="1", LAZYOS_BUSYBOX=str(shell))
    env.pop("LAZYOS_DESKTOP", None)
    print("bulk: LAZYOS_CLI=1 LAZYOS_NETD=1 cargo build", flush=True)
    result = subprocess.run(["cargo", "build"], cwd=ROOT, env=env, capture_output=True, text=True)
    if result.returncode != 0:
        return "cargo build failed:\n" + result.stderr[-4000:]
    return None if IMAGE.is_file() else f"{IMAGE} was not built"


def script(args) -> list[dict]:
    steps: list[dict] = [
        {"wait_for": PROMPT, "timeout": 300},
        {"wait_for": "NETD:ADDR", "timeout": 300},
    ]
    for path in args.paths:
        command = f"{COMMANDS[path]} {GATEWAY} {PORT} {args.bytes} {PATHS.index(path) * 100 + 1} {args.rounds}"
        steps += [
            {"type": command, "delay": 0.05},
            {"key": "enter", "until": f"NETBULK:{path}:", "timeout": 60},
            {"wait_for": f"NETBULK:{path}:done", "timeout": args.step_timeout},
        ]
    return steps + [{"quit": True}]


def run_session(args, out: Path) -> tuple[bool, str]:
    session = out / "session.json"
    session.write_text(json.dumps(script(args), indent=1))
    command = [PY, str(ROOT / "tools" / "screenshot" / "qemu_session.py"), "--image", str(args.image),
               "--out", str(out), "--script", str(session), "--accel", args.accel,
               "--net", "--net-forward", "none", "--fail-on", "PANIC", "--fail-on", "EXCEPTION"]
    if args.pcap:
        command += ["--net-pcap", str(out / "net.pcap")]
    if args.qemu:
        command += ["--qemu", args.qemu]
    if args.memory:
        command += ["--memory", args.memory]
    code = subprocess.call(command, cwd=ROOT, stdout=subprocess.DEVNULL)
    log = out / "serial.log"
    return code == 0, log.read_text(errors="replace") if log.is_file() else ""


def judge(args, text: str, server: bulkpeers.BulkServer) -> tuple[list[str], list[dict]]:
    problems: list[str] = []
    guest = [m.groups() for m in MARKER.finditer(text)]
    rows = []
    for path in args.paths:
        if f"NETBULK:{path}:done ok=true" not in text:
            problems.append(f"{path}: the client did not finish cleanly")
        mine = [g for g in guest if g[0] == path]
        if len(mine) != 2 * args.rounds:
            problems.append(f"{path}: {len(mine)} transfer markers, expected {2 * args.rounds}")
        for _, kind, verdict, detail in mine:
            fields = dict(re.findall(r"(\w+)=(\S+)", detail))
            if verdict != "PASS":
                problems.append(f"{path} {kind}: {detail.strip()}")
            rows.append({"path": path, "kind": kind, "verdict": verdict, "guest_mbps": float(fields.get("mbps", 0)),
                         "connect_us": int(fields["connect_us"]) if "connect_us" in fields else None})
    seeds = {path: PATHS.index(path) * 100 + 1 for path in args.paths}
    by_seed = {(t.kind, t.seed): t for t in server.transfers}
    for row_index, row in enumerate(rows):
        round_no = (row_index % (2 * args.rounds)) // 2
        seed = seeds[row["path"]] + round_no * 2 + (1 if row["kind"] == "GET" else 0)
        record = by_seed.get((row["kind"], seed))
        if record is None:
            problems.append(f"{row['path']} {row['kind']} seed {seed}: the server never saw it")
            continue
        if not record.ok:
            problems.append(f"{row['path']} {row['kind']}: server: {record.problem}")
        row.update(host_mbps=round(record.mbps, 2), bytes=record.size, header_ms=round(record.header_s * 1e3, 2))
    return problems, rows


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--out", default="shots/bulk")
    parser.add_argument("--no-build", action="store_true")
    parser.add_argument("--image", type=Path, default=IMAGE,
                        help="boot this image (with --no-build): a copy of an earlier build, for A/B runs")
    parser.add_argument("--accel", default="auto", choices=["auto", "none", "tcg", "whpx", "kvm"])
    parser.add_argument("--qemu")
    parser.add_argument("--memory")
    parser.add_argument("--bytes", type=int, default=16 * 1024 * 1024)
    parser.add_argument("--rounds", type=int, default=1)
    parser.add_argument("--paths", nargs="+", choices=PATHS, default=list(PATHS))
    parser.add_argument("--pcap", action="store_true", help="capture and judge the wire too")
    parser.add_argument("--step-timeout", type=float, default=900.0)
    args = parser.parse_args(argv)
    out = Path(args.out) if Path(args.out).is_absolute() else ROOT / args.out
    out.mkdir(parents=True, exist_ok=True)
    for stale in ("serial.log", "net.pcap", "bulk.json"):
        (out / stale).unlink(missing_ok=True)
    if not args.no_build:
        error = build()
        if error:
            print(f"BULK:HARNESS:FAIL {error}")
            return 1
    server = bulkpeers.BulkServer(PORT)
    for path in args.paths:
        for r in range(args.rounds):
            seed = PATHS.index(path) * 100 + 1 + r * 2
            server.expected(args.bytes, seed)
            server.expected(args.bytes, seed + 1)
    try:
        server.start()
    except OSError as exc:
        print(f"BULK:HARNESS:FAIL cannot listen on 127.0.0.1:{PORT}: {exc}")
        return 1
    try:
        session_ok, text = run_session(args, out)
    finally:
        server.stop()
    problems, rows = judge(args, text, server)
    if not session_ok:
        problems.insert(0, "the session did not finish (see summary.json)")
    if args.pcap:
        problems += judge_wire(out / "net.pcap", server)
    for row in rows:
        print(f"BULK:{row['path']}:{row['kind']} host={row.get('host_mbps', '?')} MB/s "
              f"guest={row['guest_mbps']} MB/s header_ms={row.get('header_ms', '?')}"
              + (f" connect_us={row['connect_us']}" if row["connect_us"] is not None else ""))
    (out / "bulk.json").write_text(json.dumps({"bytes": args.bytes, "rounds": args.rounds, "accel": args.accel,
                                               "rows": rows, "problems": problems}, indent=1))
    for problem in problems[:20]:
        print(f"BULK:FAIL {problem}")
    print("BULK:HARNESS:" + ("FAIL" if problems else "PASS"))
    return 1 if problems else 0


def judge_wire(pcap_path: Path, server: bulkpeers.BulkServer) -> list[str]:
    """Reassemble every TCP stream to the server from the capture and compare
    it with what the server recorded."""
    import pcap
    import sockets_pcap
    if not pcap_path.is_file():
        return ["QEMU wrote no capture"]
    frames = pcap.read_pcap(pcap_path)
    guest, gateway = pcap.parse_ip("10.0.2.15"), pcap.parse_ip(GATEWAY)
    flows = [f for f in sockets_pcap.flows(frames) if f.initiator[0] == guest and f.responder == (gateway, PORT)]
    problems: list[str] = []
    if len(flows) != len(server.transfers):
        problems.append(f"the capture holds {len(flows)} flows to port {PORT}, the server saw {len(server.transfers)}")
    for flow in flows:
        sent, sent_problems = flow.stream(True)
        received, received_problems = flow.stream(False)
        problems += sent_problems + received_problems
        header, _, body = sent.partition(b"\n")
        parts = header.split()
        if len(parts) != 3:
            problems.append(f"a flow opened with {header[:40]!r}")
            continue
        kind, size, seed = parts[0].decode(), int(parts[1]), int(parts[2])
        payload = body if kind == "PUT" else received
        if payload != server.expected(size, seed):
            problems.append(f"{kind} seed {seed}: the wire carried {len(payload)} bytes, not the stream of {size}")
    return problems


if __name__ == "__main__":
    sys.exit(main())
