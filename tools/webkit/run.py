#!/usr/bin/env python3
"""Boot LazyOS with ``depsproof`` and judge what it prints (W3).

Needs ``python tools/webkit/deps.py`` first (``target/webkit/bin/depsproof``
and the host reference ``target/webkit/depsproof.host.txt``). Then:

1. builds a console image with ``LAZYOS_WEBKIT_DEPS=1`` (``depsproof`` in
   ``/system/bin``), networking, and the harness's throwaway CA appended to the
   system bundle with ``tls.test`` mapped to the host
   (``tools/net/tls_run.py`` does the same for the HTTPS clients);
2. starts the host's TLS servers (``tools/net/tlspeers.py``);
3. boots it headless with 2 GiB (the program is tens of MB and LazyOS loads an
   ELF whole into RAM), types ``depsproof https://tls.test:47790/`` at the
   shell and waits for the final ``WEBKIT:W3:PASS`` line;
4. judges the serial log (``wkjudge.py``) against the host's own run, and what
   the server saw (SNI, one request for ``/`` from ``depsproof``). The guest and
   the host reference each print the SHA-256 of their own executable
   (``WEBKIT:W3:BIN``); both must equal that of ``target/webkit/bin/depsproof``,
   so an image or a reference left over from another build fails with a message.

Usage::

    python tools/webkit/run.py [--qemu PATH] [--accel auto]
    python tools/webkit/run.py --no-build        # boot the last image again
    python tools/webkit/run.py --judge-only LOG  # judge a saved serial log
"""

from __future__ import annotations

import argparse
import json
import os
import shutil
import subprocess
import sys
from pathlib import Path

HERE = Path(__file__).resolve().parent
ROOT = HERE.parent.parent
sys.path.insert(0, str(HERE))
sys.path.insert(0, str(ROOT / "tools" / "net"))
sys.path.insert(0, str(ROOT / "tools" / "abi"))
import wkjudge  # noqa: E402

BIN = ROOT / "target" / "webkit" / "bin" / "depsproof"
HOST_RUN = ROOT / "target" / "webkit" / "depsproof.host.txt"
IMAGE = ROOT / "target" / "webkit" / "lazyos-webkit.img"
PY = sys.executable
PROMPT = "/ #"
#: The last line of a finished run (a per-library PASS has a colon after it).
FINISHED = r"WEBKIT:W3:(PASS|FAIL:see above)\r?\n"


def build_image(env_extra: dict[str, str]) -> str | None:
    """Build the console image; an error message, or None."""
    import busybox
    shell = busybox.ensure_busybox()
    if shell is None:
        return "no BusyBox for the console shell (see tools/abi/busybox.py)"
    env = dict(os.environ, LAZYOS_CLI="1", LAZYOS_NETD="1", LAZYOS_NETD_ARGS="demo=0",
               LAZYOS_WEBKIT_DEPS="1", LAZYOS_RESET_OS="1", LAZYOS_BUSYBOX=str(shell), **env_extra)
    for stale in ("LAZYOS_DESKTOP", "LAZYOS_TLS", "LAZYOS_JSC", "LAZYOS_LINUXAPPS"):
        env.pop(stale, None)
    env.setdefault("LAZYOS_OS_SIZE", "512M")
    print("webkit: cargo build (LAZYOS_WEBKIT_DEPS=1, console, networking)", flush=True)
    result = subprocess.run(["cargo", "build"], cwd=ROOT, env=env, capture_output=True, text=True)
    if result.returncode != 0:
        return "cargo build failed:\n" + result.stderr[-4000:]
    built = ROOT / "target" / "lazyos.img"
    if not built.is_file():
        return f"{built} was not built"
    IMAGE.parent.mkdir(parents=True, exist_ok=True)
    shutil.copyfile(built, IMAGE)
    return None


def session(command: str, timeout: float) -> list[dict]:
    """The ``qemu_session.py`` steps: wait for the shell and an address, type
    the command, wait for the program's last line."""
    return [
        {"wait_for": PROMPT, "timeout": 300},
        {"wait_for": "NETD:RESOLV wrote", "timeout": 300},
        {"type": command, "delay": 0.05},
        {"key": "enter", "until": "WEBKIT:W3:UP", "timeout": timeout},
        {"wait_for": FINISHED, "regex": True, "timeout": timeout},
        {"wait": 1.0},
        {"shot": "webkit"},
        {"quit": True},
    ]


def boot(args: argparse.Namespace, out: Path, command: str) -> tuple[bool, str]:
    script = out / "session.json"
    script.write_text(json.dumps(session(command, args.timeout), indent=1))
    cmd = [PY, str(ROOT / "tools" / "screenshot" / "qemu_session.py"), "--image", str(IMAGE),
           "--out", str(out), "--script", str(script), "--accel", args.accel, "--memory", args.memory,
           "--net", "--net-forward", "none", "--net-pcap", str(out / "net.pcap"),
           "--fail-on", "PANIC", "--fail-on", "EXCEPTION"]
    if args.qemu:
        cmd += ["--qemu", args.qemu]
    code = subprocess.call(cmd, cwd=ROOT, stdout=subprocess.DEVNULL)
    log = out / "serial.log"
    return code == 0, log.read_text(errors="replace") if log.is_file() else ""


def judge_servers(record, tp, tlscerts) -> list[str]:
    """Problems with what the harness's TLS 1.3 server saw (empty = fine)."""
    problems = []
    shakes = [h for h in record.handshakes if h.port == tp.GOOD_PORT]
    if not shakes:
        problems.append("no completed handshake on the TLS 1.3 server")
    for shake in shakes:
        if shake.sni != tlscerts.NAME:
            problems.append(f"SNI {shake.sni}, expected {tlscerts.NAME}")
    asked = [r for r in record.requests if r.port == tp.GOOD_PORT and r.path == "/"]
    if not asked:
        problems.append("the server was never asked for /")
    elif not any("depsproof" in r.headers.get("user-agent", "") for r in asked):
        problems.append("the request did not come from depsproof")
    return problems


def report(serial: str, host: str, body: str | None, servers: list[str] | None) -> int:
    binary = wkjudge.sha256_of(BIN) if BIN.is_file() else None
    verdicts = wkjudge.judge(serial, host, body, servers, binary)
    for verdict in verdicts:
        print(f"{'PASS' if verdict.ok else 'FAIL'}  {verdict.name}  {verdict.detail}".rstrip())
    if BIN.is_file():
        print(f"depsproof: {BIN.stat().st_size / 1048576:.1f} MiB")
    ok = wkjudge.passed(verdicts)
    print("WEBKIT:W3:HARNESS:" + ("PASS" if ok else "FAIL"))
    return 0 if ok else 1


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__,
                                     formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--qemu", help="path to qemu-system-x86_64")
    parser.add_argument("--accel", default="auto", choices=["auto", "none", "tcg", "whpx", "kvm"])
    parser.add_argument("--memory", default="2G", help="guest RAM (default 2G)")
    parser.add_argument("--timeout", type=float, default=600.0, help="seconds to wait for the program")
    parser.add_argument("--out", default="shots/webkit")
    parser.add_argument("--no-build", action="store_true", help="boot the last image (its certificates)")
    parser.add_argument("--judge-only", metavar="LOG", help="judge a saved serial log and exit")
    args = parser.parse_args()
    out = Path(args.out) if Path(args.out).is_absolute() else ROOT / args.out
    out.mkdir(parents=True, exist_ok=True)
    host = HOST_RUN.read_text(errors="replace") if HOST_RUN.is_file() else ""

    if args.judge_only:
        return report(Path(args.judge_only).read_text(errors="replace"), host, None, None)
    if not BIN.is_file() or not host:
        print("webkit: target/webkit/bin/depsproof is missing (python tools/webkit/deps.py)", file=sys.stderr)
        return 1

    import tls_session
    import tlscerts
    import tlspeers as tp
    certs = tlscerts.load(out / "certs") if args.no_build else None
    if certs is None:
        if args.no_build:
            print("webkit: --no-build needs the certificates of the run that built the image", file=sys.stderr)
            return 1
        tlscerts.require()
        certs = tlscerts.generate(out / "certs")
    hosts = out / "certs" / "hosts"
    hosts.write_text(f"10.0.2.2 {tlscerts.NAME} {tlscerts.RSA_NAME}\n")
    if not args.no_build:
        error = build_image({"LAZYOS_TLS_TEST_CA": str(certs["ca"]), "LAZYOS_TLS_TEST_HOSTS": str(hosts)})
        if error:
            print(f"webkit: {error}", file=sys.stderr)
            return 1
    elif not IMAGE.is_file():
        print(f"webkit: {IMAGE} not found", file=sys.stderr)
        return 1
    (out / "serial.log").unlink(missing_ok=True)
    try:
        peers = tp.Peers(certs, b"")
    except OSError as error:
        print(f"webkit: cannot start the host servers on ports 47790-47801: {error}", file=sys.stderr)
        return 1
    try:
        session_ok, serial = boot(args, out, f"depsproof {tls_session.url(tp.GOOD_PORT)}")
    finally:
        record = peers.snapshot()
        peers.close()
    if not session_ok:
        print("FAIL  the session did not finish (see summary.json)")
    code = report(serial, host, tls_session.digest(tp.INDEX), judge_servers(record, tp, tlscerts))
    return code if session_ok else 1


if __name__ == "__main__":
    raise SystemExit(main())
