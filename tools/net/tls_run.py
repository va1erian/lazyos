#!/usr/bin/env python3
"""Build the HTTPS clients into an image, run them against host TLS servers,
and judge what the servers and the wire saw (docs/tls-plan.md §8, stage T3).

1. `tools/nettls/build.py --require`: `fetch` (also `curl` and `wget`).
2. A throwaway test CA and leaves (`tlscerts.py`); the image is built with
   `LAZYOS_TLS=1`, the CA appended to the system bundle
   (`LAZYOS_TLS_TEST_CA`) and `tls.test` mapped to the host in /etc/hosts
   (`LAZYOS_TLS_TEST_HOSTS`): the tools load roots exactly as in production.
3. The host's servers (`tlspeers.py`): TLS 1.3 and 1.2, an RSA chain, plain
   HTTP, and the negative servers (expired, not yet valid, wrong name,
   self-signed, unknown CA, TLS 1.0, CBC-only, a truncated record).
4. A console session (`tls_session.py`) types every check; each prints
   `TLS:<check>:PASS|FAIL`.
5. The verdict: every marker passed; the good servers saw SNI, ALPN
   `http/1.1`, the expected protocol versions and requests; the negative
   servers received no application data; and the capture shows a ClientHello
   with the right SNI on every TLS connection and no page in the clear
   (`tls_pcap.py`).

    python tools/net/tls_run.py                   # build, boot, judge (also: tools/net/run.py --tls)
    python tools/net/tls_run.py --no-build        # reuse target/lazyos.img (built by this script)
    python tools/net/tls_run.py --live            # real sites, Mozilla roots only (needs internet)
    python tools/net/tls_run.py --live --extra-ca proxy.pem --live-url https://github.com/

`--live` never runs in CI. `--extra-ca` is for a network whose egress
re-signs TLS (a corporate or sandbox proxy): it appends that CA as a test CA.
Exit status is non-zero on any failure; logs and the capture go to `shots/tls`.
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
import busybox  # noqa: E402
import pcap  # noqa: E402
import tls_pcap  # noqa: E402
import tls_session  # noqa: E402
import tlscerts  # noqa: E402
import tlspeers as tp  # noqa: E402

PY = sys.executable
IMAGE = ROOT / "target" / "lazyos.img"
GUEST_IP = pcap.parse_ip("10.0.2.15")
GATEWAY_IP = pcap.parse_ip("10.0.2.2")
LIVE_URLS = ["https://www.google.com/", "https://en.wikipedia.org/"]
#: Paths the TLS 1.3 server must have been asked for.
GOOD_PATHS = {"/", "/chunked", "/gzip", "/big", "/files/page.txt", "/redirect/3", "/redirect/2",
              "/redirect/1", "/redirect/0", "/downgrade", "/nope"}


def build(env_extra: dict[str, str]) -> str | None:
    """Build the tools and the image; an error message, or None."""
    if subprocess.call([PY, str(ROOT / "tools" / "nettls" / "build.py"), "--require"], cwd=ROOT) != 0:
        return "tools/nettls/build.py --require failed (it needs the x86_64-unknown-linux-musl target)"
    shell = busybox.ensure_busybox()
    if shell is None:
        return "no BusyBox for the console shell (see tools/abi/busybox.py)"
    env = dict(os.environ, LAZYOS_CLI="1", LAZYOS_NETD="1", LAZYOS_NETD_ARGS="demo=0", LAZYOS_TLS="1",
               LAZYOS_RESET_OS="1", LAZYOS_BUSYBOX=str(shell), **env_extra)
    for stale in ("LAZYOS_TLS_TEST_CA", "LAZYOS_TLS_TEST_HOSTS", "LAZYOS_DESKTOP"):
        if stale not in env_extra:
            env.pop(stale, None)
    print(f"tls: {' '.join(f'{k}={v}' for k, v in env_extra.items())} LAZYOS_TLS=1 cargo build", flush=True)
    result = subprocess.run(["cargo", "build"], cwd=ROOT, env=env, capture_output=True, text=True)
    if result.returncode != 0:
        return "cargo build failed:\n" + result.stderr[-4000:]
    return None if IMAGE.is_file() else f"{IMAGE} was not built"


def run_session(items, out: Path, args) -> tuple[bool, str]:
    """Type the checks into the guest; (session ok, serial log text)."""
    script = out / "session.json"
    script.write_text(json.dumps(tls_session.script(items, args.step_timeout), indent=1))
    command = [PY, str(ROOT / "tools" / "screenshot" / "qemu_session.py"), "--image", str(IMAGE),
               "--out", str(out), "--script", str(script), "--accel", args.accel,
               "--net", "--net-forward", "none", "--net-pcap", str(out / "net.pcap"),
               "--fail-on", "PANIC", "--fail-on", "EXCEPTION"]
    if args.qemu:
        command += ["--qemu", args.qemu]
    if args.memory:
        command += ["--memory", args.memory]
    code = subprocess.call(command, cwd=ROOT, stdout=subprocess.DEVNULL)
    log = out / "serial.log"
    return code == 0, log.read_text(errors="replace") if log.is_file() else ""


def judge_markers(text: str, names: list[str]) -> list[str]:
    problems = []
    for name in names:
        found = re.findall(rf"^TLS:{name}:(PASS|FAIL\S*)", text, re.M)
        print(f"  TLS:{name}:{found[-1] if found else 'MISSING'}")
        if not found or found[-1] != "PASS":
            problems.append(f"check {name}: {found[-1] if found else 'never reported'}")
    return problems


def judge_servers(record: tp.Record) -> list[str]:
    """What the host's servers saw against what correct clients do."""
    problems = []
    expect = {tp.GOOD_PORT: (tlscerts.NAME, "TLSv1.3"), tp.TLS12_PORT: (tlscerts.NAME, "TLSv1.2"),
              tp.RSA_PORT: (tlscerts.RSA_NAME, "TLSv1.3")}
    for port, (name, version) in expect.items():
        mine = [h for h in record.handshakes if h.port == port]
        if not mine:
            problems.append(f"port {port}: no completed handshake")
        for h in mine:
            if (h.sni, h.alpn, h.version) != (name, "http/1.1", version):
                problems.append(f"port {port}: handshake sni={h.sni} alpn={h.alpn} version={h.version}, "
                                f"expected sni={name} alpn=http/1.1 version={version}")
    paths = {r.path for r in record.requests if r.port == tp.GOOD_PORT}
    if not GOOD_PATHS <= paths:
        problems.append(f"the TLS 1.3 server was never asked for {sorted(GOOD_PATHS - paths)}")
    gzip_asks = [r for r in record.requests if r.path == "/gzip"]
    if not gzip_asks or any("gzip" not in r.headers.get("accept-encoding", "") for r in gzip_asks):
        problems.append("the client did not offer gzip")
    plain = [r.path for r in record.requests if r.port == tp.PLAIN_PORT and r.path != tp.SCRIPT_PATH]
    if "/downgraded" in plain:
        problems.append("the client followed an https -> http redirect")
    if plain.count("/") != 1:
        problems.append(f"the plain server saw {plain}, expected one request for /")
    for role, port in tp.NEGATIVE_PORTS.items():
        if record.connections.get(port, 0) == 0:
            problems.append(f"{role}: the client never connected")
        if record.leaked.get(port, 0):
            problems.append(f"{role}: the server received {record.leaked[port]} bytes of application data")
        if any(r.port == port for r in record.requests):
            problems.append(f"{role}: the server received an HTTP request")
    return problems


def judge_wire(pcap_path: Path) -> tuple[str, list[str]]:
    if not pcap_path.is_file():
        return "", ["QEMU wrote no capture"]
    expected = {port: tlscerts.NAME for port in (tp.GOOD_PORT, tp.TLS12_PORT, *tp.NEGATIVE_PORTS.values())}
    expected[tp.RSA_PORT] = tlscerts.RSA_NAME
    minimum = {port: 1 for port in expected}
    minimum[tp.GOOD_PORT] = len(GOOD_PATHS)
    secrets = [tp.INDEX[64:128], tp.CHUNKED[:64], tp.BIG[4096:4160], tp.PAGE]
    count, problems = tls_pcap.check_tls_flows(pcap.read_pcap(pcap_path), GUEST_IP, GATEWAY_IP,
                                               expected, minimum, secrets)
    return f"{count} TLS connections, each opened with the right SNI and ALPN, no page in the clear", problems


def report(section: str, detail: str, problems: list[str]) -> bool:
    for problem in problems[:12]:
        print(f"TLS:{section}:FAIL {problem}")
    if not problems:
        print(f"TLS:{section}:PASS {detail}".rstrip())
    return not problems


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--out", default="shots/tls")
    parser.add_argument("--no-build", action="store_true", help="reuse the image (built by this script)")
    parser.add_argument("--accel", default="auto", choices=["auto", "none", "tcg", "whpx", "kvm"])
    parser.add_argument("--qemu")
    parser.add_argument("--memory")
    parser.add_argument("--step-timeout", type=float, default=180.0, help="seconds per typed check")
    parser.add_argument("--live", action="store_true", help="real public sites instead of the host servers")
    parser.add_argument("--live-url", action="append", default=[], metavar="URL")
    parser.add_argument("--extra-ca", metavar="PEM", help="--live: also trust this CA (an intercepting proxy)")
    args = parser.parse_args(argv)
    out = Path(args.out) if Path(args.out).is_absolute() else ROOT / args.out
    out.mkdir(parents=True, exist_ok=True)
    for stale in ("serial.log", "net.pcap", "summary.json"):
        (out / stale).unlink(missing_ok=True)
    if args.live:
        return live(args, out)

    certs = tlscerts.load(out / "certs") if args.no_build else None
    if certs is None:
        if args.no_build:
            print("TLS:HARNESS:FAIL --no-build needs the certificates of the run that built the image")
            return 1
        certs = tlscerts.generate(out / "certs")
    hosts = out / "certs" / "hosts"
    hosts.write_text(f"{pcap.ip_text(GATEWAY_IP)} {tlscerts.NAME} {tlscerts.RSA_NAME}\n")
    if not args.no_build:
        error = build({"LAZYOS_TLS_TEST_CA": str(certs["ca"]), "LAZYOS_TLS_TEST_HOSTS": str(hosts)})
        if error:
            print(f"TLS:HARNESS:FAIL {error}")
            return 1
    elif not IMAGE.is_file():
        print(f"TLS:HARNESS:FAIL {IMAGE} not found")
        return 1
    items = tls_session.checks()
    try:
        peers = tp.Peers(certs, tls_session.body(items))
    except OSError as exc:
        print(f"TLS:HARNESS:FAIL cannot start the host servers on ports 47790-47801: {exc}")
        return 1
    try:
        session_ok, text = run_session(items, out, args)
    finally:
        record = peers.snapshot()
        peers.close()
    ok = report("SESSION", "", [] if session_ok else ["the session did not finish (see summary.json)"])
    ok = report("GUEST", f"{len(items)} checks", judge_markers(text, [n for n, _ in items])) and ok
    ok = report("SERVERS", f"{len(record.handshakes)} handshakes, {len(record.requests)} requests as expected; "
                "the negative servers received nothing", judge_servers(record)) and ok
    detail, problems = judge_wire(out / "net.pcap")
    ok = report("WIRE", detail, problems) and ok
    print("TLS:HARNESS:" + ("PASS" if ok else "FAIL"))
    return 0 if ok else 1


def live(args, out: Path) -> int:
    """`--live`: the image's roots (plus `--extra-ca`) against real sites."""
    extra = {"LAZYOS_TLS_TEST_CA": str(Path(args.extra_ca).resolve())} if args.extra_ca else {}
    if not args.no_build:
        error = build(extra)
        if error:
            print(f"TLS:HARNESS:FAIL {error}")
            return 1
    items = tls_session.live_checks(args.live_url or LIVE_URLS)
    server = tp.Server(tp.PLAIN_PORT, "plain", tp.Record(), None, {tp.SCRIPT_PATH: tls_session.body(items)})
    try:
        session_ok, text = run_session(items, out, args)
    finally:
        server.close()
    for line in text.splitlines():
        if line.startswith("TLS:HANDSHAKE") or line.startswith("TLS:FAIL"):
            print(f"  {line}")
    ok = report("SESSION", "", [] if session_ok else ["the session did not finish (see summary.json)"])
    ok = report("LIVE", ", ".join(args.live_url or LIVE_URLS), judge_markers(text, [n for n, _ in items])) and ok
    print("TLS:HARNESS:" + ("PASS" if ok else "FAIL"))
    return 0 if ok else 1


if __name__ == "__main__":
    sys.exit(main())
