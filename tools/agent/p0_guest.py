#!/usr/bin/env python3
"""P0 spike of docs/ai-agent-plan.md: LazyAgent's HTTPS streaming from a guest.

Builds `agent_cli` (the core's host CLI) for x86_64-unknown-linux-musl, puts it
in a console image as `fetch` (`LAZYOS_FETCH`, `LAZYOS_TLS=1`; the program does
not look at argv[0]), boots it, and in the guest:

1. `fetch --probe tls.test:PORT` reads a streamed body from a host server that
   writes every TLS record in two parts with a pause between them, so the
   client's 250 ms socket read timeout fires in the middle of a record
   (item 5). The body's FNV-1a checksum must match the server's.
2. With `FIREWORKS_API_KEY` in the host's environment, the key is typed into
   `read -s` (no echo) and `fetch --once ...` streams a real Fireworks reply
   (item 1): the first-chunk time and tokens/s are printed. Without the
   variable this part is skipped. The key is never written to a file, an
   argument or a log; the script scans the serial log for it.

    python tools/agent/p0_guest.py [--accel none|auto] [--no-build] [--skip-fireworks]

Exit status is non-zero on any failed check. Output goes to `shots/agent_p0`.
This is a spike harness, not the P2 `tools/agent/run.py`.
"""

from __future__ import annotations

import argparse
import json
import os
import re
import socket
import ssl
import subprocess
import sys
import threading
import time
from pathlib import Path

HERE = Path(__file__).resolve().parent
ROOT = HERE.parent.parent
sys.path.insert(0, str(ROOT / "tools" / "net"))
sys.path.insert(0, str(ROOT / "tools" / "abi"))
import busybox  # noqa: E402
import tlscerts  # noqa: E402

PY = sys.executable
IMAGE = ROOT / "target" / "lazyos.img"
TARGET = "x86_64-unknown-linux-musl"
ELF = ROOT / "target" / "agent_p0" / "agent_cli.elf"
PORT = 47811
CHUNKS = 10
CHUNK_BYTES = 6000
PAUSE = 0.6
PROMPT = "/ #"


def musl_env() -> dict[str, str]:
    """The cargo environment, with the bundled lld on a Windows host."""
    env = dict(os.environ)
    if os.name != "nt":
        return env
    sysroot = subprocess.run(["rustc", "--print", "sysroot"], capture_output=True, text=True).stdout.strip()
    version = subprocess.run(["rustc", "-vV"], capture_output=True, text=True).stdout
    host = next((l.split(":", 1)[1].strip() for l in version.splitlines() if l.startswith("host:")), "")
    linker = Path(sysroot) / "lib" / "rustlib" / host / "bin" / "rust-lld.exe"
    if linker.is_file():
        prefix = f"CARGO_TARGET_{TARGET.upper().replace('-', '_')}"
        env.setdefault(f"{prefix}_LINKER", str(linker))
        env.setdefault(f"{prefix}_RUSTFLAGS", "-C linker-flavor=ld.lld")
    return env


def build_cli() -> str | None:
    manifest = ROOT / "xui-app" / "Cargo.toml"
    result = subprocess.run(
        ["cargo", "build", "--release", "--manifest-path", str(manifest), "-p", "lazyagent-core",
         "--bin", "agent_cli", "--target", TARGET],
        cwd=ROOT, env=musl_env(), capture_output=True, text=True)
    if result.returncode != 0:
        return "cargo build failed:\n" + result.stderr[-3000:]
    built = ROOT / "xui-app" / "target" / TARGET / "release" / "agent_cli"
    if not built.is_file():
        return f"{built} was not built"
    ELF.parent.mkdir(parents=True, exist_ok=True)
    ELF.write_bytes(built.read_bytes())
    return None


def build_image(ca: Path, hosts: Path) -> str | None:
    shell = busybox.ensure_busybox()
    if shell is None:
        return "no BusyBox for the console shell"
    env = dict(os.environ, LAZYOS_CLI="1", LAZYOS_NETD="1", LAZYOS_NETD_ARGS="demo=0", LAZYOS_TLS="1",
               LAZYOS_FETCH=str(ELF), LAZYOS_RESET_OS="1", LAZYOS_BUSYBOX=str(shell),
               LAZYOS_TLS_TEST_CA=str(ca), LAZYOS_TLS_TEST_HOSTS=str(hosts))
    env.pop("LAZYOS_DESKTOP", None)
    result = subprocess.run(["cargo", "build"], cwd=ROOT, env=env, capture_output=True, text=True)
    if result.returncode != 0:
        return "cargo build failed:\n" + result.stderr[-3000:]
    return None if IMAGE.is_file() else f"{IMAGE} was not built"


def fnv(data: bytes) -> int:
    h = 0xCBF29CE484222325
    for b in data:
        h = ((h ^ b) * 0x100000001B3) & 0xFFFFFFFFFFFFFFFF
    return h


def probe_body() -> list[bytes]:
    return [bytes((i * 31 + n) & 0xFF for i in range(CHUNK_BYTES)) for n in range(CHUNKS)]


def serve_probe(leaf: tlscerts.Leaf, results: list[str]) -> socket.socket:
    """A TLS server that sends each chunk as one record in two halves."""
    context = ssl.SSLContext(ssl.PROTOCOL_TLS_SERVER)
    context.load_cert_chain(str(leaf.cert), str(leaf.key))
    context.set_alpn_protocols(["http/1.1"])
    listener = socket.socket()
    listener.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
    listener.bind(("0.0.0.0", PORT))
    listener.listen(4)

    def pump(sslobj, incoming, outgoing, conn) -> None:
        data = outgoing.read()
        if data:
            conn.sendall(data)

    def handle(conn: socket.socket) -> None:
        conn.setsockopt(socket.IPPROTO_TCP, socket.TCP_NODELAY, 1)
        incoming, outgoing = ssl.MemoryBIO(), ssl.MemoryBIO()
        sslobj = context.wrap_bio(incoming, outgoing, server_side=True)
        try:
            while True:
                try:
                    sslobj.do_handshake()
                    break
                except ssl.SSLWantReadError:
                    pump(sslobj, incoming, outgoing, conn)
                    data = conn.recv(4096)
                    if not data:
                        return
                    incoming.write(data)
            pump(sslobj, incoming, outgoing, conn)
            request = b""
            while b"\r\n\r\n" not in request:
                try:
                    request += sslobj.read(4096)
                except ssl.SSLWantReadError:
                    data = conn.recv(4096)
                    if not data:
                        return
                    incoming.write(data)
            head = (b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\n"
                    b"Transfer-Encoding: chunked\r\n\r\n")
            sslobj.write(head)
            pump(sslobj, incoming, outgoing, conn)
            body = b"".join(probe_body())
            for chunk in probe_body():
                sslobj.write(b"%x\r\n" % len(chunk) + chunk + b"\r\n")
                wire = outgoing.read()
                cut = len(wire) // 2
                conn.sendall(wire[:cut])
                time.sleep(PAUSE)
                conn.sendall(wire[cut:])
            sslobj.write(b"0\r\n\r\n")
            pump(sslobj, incoming, outgoing, conn)
            results.append(f"bytes={len(body)} fnv={fnv(body):016x}")
            time.sleep(0.5)
        except (OSError, ssl.SSLError) as error:
            results.append(f"server error: {error}")
        finally:
            conn.close()

    def accept() -> None:
        while True:
            try:
                conn, _ = listener.accept()
            except OSError:
                return
            threading.Thread(target=handle, args=(conn,), daemon=True).start()

    threading.Thread(target=accept, daemon=True).start()
    return listener


def steps(with_key: bool) -> list[dict]:
    out: list[dict] = [
        {"wait_for": PROMPT, "timeout": 300},
        {"wait_for": "NETD:RESOLV wrote", "timeout": 300},
        {"type": "export SSL_CERT_FILE=/etc/ssl/certs/ca-certificates.crt", "delay": 0.05},
        {"key": "enter"},
        {"type": f"fetch --probe {tlscerts.NAME}:{PORT}", "delay": 0.05},
        {"key": "enter", "until": "probe done", "timeout": 120},
    ]
    if with_key:
        out += [
            {"type": "read -s FIREWORKS_API_KEY", "delay": 0.05},
            {"key": "enter"},
            {"type_secret": "FIREWORKS_API_KEY", "delay": 0.03},
            {"key": "enter"},
            {"type": "export FIREWORKS_API_KEY", "delay": 0.05},
            {"key": "enter"},
            {"type": "fetch --once 'Say hello in five words.' --dir /tmp/agent", "delay": 0.05},
            {"key": "enter", "until": "first chunk", "timeout": 180},
            # A longer reply, for the throughput; the marker is built by the
            # shell so the typed command never contains it.
            {"type": "fetch --once 'Explain in about 150 words how DHCP works.' --effort low "
                     "--dir /tmp/agent; echo P0-DONE-$((1+1))", "delay": 0.05},
            {"key": "enter", "until": "P0-DONE-2", "timeout": 180},
        ]
    return out + [{"wait": 2}, {"shot": "p0"}, {"quit": True}]


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--out", default="shots/agent_p0")
    parser.add_argument("--accel", default="auto", choices=["auto", "none", "tcg", "whpx", "kvm"])
    parser.add_argument("--no-build", action="store_true")
    parser.add_argument("--skip-fireworks", action="store_true")
    parser.add_argument("--qemu")
    args = parser.parse_args()
    out = ROOT / args.out
    out.mkdir(parents=True, exist_ok=True)
    for stale in ("serial.log", "summary.json"):
        (out / stale).unlink(missing_ok=True)
    key = os.environ.get("FIREWORKS_API_KEY", "")
    with_key = bool(key) and not args.skip_fireworks
    certs = tlscerts.load(out / "certs") if args.no_build else None
    if certs is None:
        certs = tlscerts.generate(out / "certs")
    hosts = out / "certs" / "hosts"
    hosts.write_text(f"10.0.2.2 {tlscerts.NAME}\n")
    if not args.no_build:
        for what, build in (("cli", build_cli), ("image", lambda: build_image(certs["ca"], hosts))):
            error = build()
            if error:
                print(f"P0:HARNESS:FAIL {what}: {error}")
                return 1
    results: list[str] = []
    listener = serve_probe(certs["good"], results)
    script = out / "session.json"
    script.write_text(json.dumps(steps(with_key), indent=1))
    command = [PY, str(ROOT / "tools" / "screenshot" / "qemu_session.py"), "--image", str(IMAGE),
               "--out", str(out), "--script", str(script), "--accel", args.accel, "--net",
               "--net-forward", "none", "--fail-on", "PANIC", "--fail-on", "EXCEPTION"]
    if args.qemu:
        command += ["--qemu", args.qemu]
    try:
        code = subprocess.call(command, cwd=ROOT, stdout=subprocess.DEVNULL)
    finally:
        listener.close()
    log = (out / "serial.log").read_text(errors="replace") if (out / "serial.log").is_file() else ""
    problems = [] if code == 0 else ["the session did not finish (see summary.json)"]
    done = re.findall(r"probe done status=200 bytes=(\d+) fnv=([0-9a-f]{16}) wakes=(\d+) ms=(\d+)", log)
    expect = f"bytes={CHUNKS * CHUNK_BYTES} fnv={fnv(b''.join(probe_body())):016x}"
    if not done:
        problems.append("the guest never printed 'probe done'")
    else:
        size, digest, wakes, ms = done[-1]
        print(f"P0:PROBE bytes={size} fnv={digest} wakes={wakes} ms={ms} (server sent {expect})")
        if f"bytes={size} fnv={digest}" != expect:
            problems.append("the guest's checksum differs from the server's: the stream was corrupted")
        if int(wakes) < CHUNKS:
            problems.append(f"only {wakes} read timeouts fired: the mid-record case was not exercised")
    if with_key:
        first = re.findall(r"first chunk (\d+) ms \(server (\d+|-) ms\), total (\d+) ms, (\d+) in / (\d+) out", log)
        if first:
            print("P0:FIREWORKS first chunk %s ms (server %s ms), total %s ms, %s in / %s out" % first[-1])
        else:
            problems.append("no Fireworks reply in the guest (see serial.log)")
        if key in log:
            problems.append("THE KEY IS IN THE SERIAL LOG")
    for p in problems:
        print(f"P0:FAIL {p}")
    print("P0:HARNESS:" + ("PASS" if not problems else "FAIL"))
    return 0 if not problems else 1


if __name__ == "__main__":
    sys.exit(main())
