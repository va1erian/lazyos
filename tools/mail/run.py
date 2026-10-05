#!/usr/bin/env python3
"""Build a desktop with Mail, point it at a mock mail server over TLS, and
judge what the app and the server saw (docs/mail.md).

1. The apps (`tools/xui/build.py --mail`, `tools/nettls/build.py`, `rhai`) and
   an image with `LAZYOS_DESKTOP=1 LAZYOS_TLS=1 LAZYOS_MAIL=1`, the harness's
   test CA in the system bundle and `tls.test` mapped to the host
   (`LAZYOS_TLS_TEST_CA`, `LAZYOS_TLS_TEST_HOSTS`, as `tools/net/tls_run.py`).
2. esMail's `mail-mock-server` (built from the revision Mail pins) on the
   host, with TLS fronts for `tls.test` (`tlsproxy.py`): IMAPS on 9993 and
   SMTPS on 9465.
3. The session `tools/screenshot/examples/mail_tls.json` launches Mail, fills
   in the account (the password is typed, never written anywhere), opens a
   message and sends one.
4. The verdict: every `MAIL:` marker of the session; both fronts completed a
   TLS handshake for `tls.test`; and the password never reached the serial log.

    python tools/mail/run.py                 # build, boot, judge
    python tools/mail/run.py --no-build      # reuse target/lazyos.img and shots/mail/certs
    python tools/mail/run.py --label-trace   # also LAZYOS_LABEL_TRACE=1: list LABEL:DENY lines

Exit status is non-zero on any failure; logs and screenshots go to `shots/mail`.
"""

from __future__ import annotations

import argparse
import os
import re
import subprocess
import sys
import time
from pathlib import Path

HERE = Path(__file__).resolve().parent
ROOT = HERE.parent.parent
sys.path.insert(0, str(HERE))
sys.path.insert(0, str(ROOT / "tools" / "net"))
sys.path.insert(0, str(ROOT / "tools" / "abi"))
import busybox  # noqa: E402
import tlscerts  # noqa: E402
import tlsproxy  # noqa: E402

PY = sys.executable
IMAGE = ROOT / "target" / "lazyos.img"
SCRIPT = ROOT / "tools" / "screenshot" / "examples" / "mail_tls.json"
ESMAIL = "https://github.com/va1erian/esmail"
#: The revision `xui-app/mail/Cargo.toml` pins.
ESMAIL_REV = re.search(r'esmail = \{[^}]*rev = "([0-9a-f]+)"',
                       (ROOT / "xui-app" / "mail" / "Cargo.toml").read_text()).group(1)
MOCK_ROOT = ROOT / "target" / "mail-mock"
IMAPS_PORT, SMTPS_PORT = 9993, 9465
MOCK_IMAP_PORT, MOCK_SMTP_PORT = 21993, 21025
#: The mock's account (esmail `mail-mock-server/src/fixtures.rs`).
PASSWORD = "hunter2"
MARKERS = ["MAIL:UP:PASS", "MAIL:ACCOUNT:SAVED", "MAIL:CONNECTED:0", "MAIL:FOLDERS:0:",
           "MAIL:HEADERS:INBOX:", "MAIL:BODY:PASS:", "MAIL:RENDER:PASS", "MAIL:SEND:PASS"]


def run(argv: list[str], env: dict[str, str] | None = None) -> None:
    print("mail: " + " ".join(str(a) for a in argv), flush=True)
    subprocess.run(argv, cwd=ROOT, env=env, check=True)


def build(certs: dict, hosts: Path, label_trace: bool) -> None:
    run([PY, "tools/xui/build.py", "--mail"])
    run([PY, "tools/nettls/build.py", "--require"])
    run([PY, "tools/rhai/build.py"])
    # The Terminal's shell, which the session types into (`build.rs` finds the cache).
    if busybox.ensure_busybox() is None:
        raise SystemExit("MAIL:HARNESS:FAIL no BusyBox for the Terminal (see tools/abi/busybox.py)")
    env = dict(os.environ, LAZYOS_DESKTOP="1", LAZYOS_NETD="1", LAZYOS_NETD_ARGS="demo=0",
               LAZYOS_TLS="1", LAZYOS_MAIL="1", LAZYOS_RESET_OS="1",
               # The session types into a Terminal; nothing autostarts by default.
               LAZYOS_XUI_AUTOSTART="term",
               LAZYOS_TLS_TEST_CA=str(certs["ca"]), LAZYOS_TLS_TEST_HOSTS=str(hosts))
    if label_trace:
        env["LAZYOS_LABEL_TRACE"] = "1"
    else:
        env.pop("LAZYOS_LABEL_TRACE", None)
    run(["cargo", "build"], env=env)


def mock_server() -> Path:
    """`mail-mock-server` at the pinned revision (installed once)."""
    binary = MOCK_ROOT / "bin" / "mail-mock-server"
    stamp = MOCK_ROOT / "rev"
    if not binary.is_file() or not stamp.is_file() or stamp.read_text() != ESMAIL_REV:
        run(["cargo", "install", "--locked", "--git", ESMAIL, "--rev", ESMAIL_REV,
             "--root", str(MOCK_ROOT), "--force", "mail-mock-server"])
        stamp.write_text(ESMAIL_REV)
    return binary


def start_mock(binary: Path, out: Path) -> tuple[subprocess.Popen, Path]:
    ca = out / "certs" / "mock-ca.pem"
    ca.unlink(missing_ok=True)
    env = dict(os.environ, IMAP_BIND=f"127.0.0.1:{MOCK_IMAP_PORT}", SMTP_BIND=f"127.0.0.1:{MOCK_SMTP_PORT}",
               INBOX_COUNT="5", CA_OUT=str(ca))
    with open(out / "mock.log", "w") as log:
        process = subprocess.Popen([str(binary)], env=env, stdout=log, stderr=subprocess.STDOUT)
    # The mock starts IMAP first; the session needs both, so wait for SMTP's line.
    smtp_up = re.compile(r"SMTP[^\n]*listening on")
    deadline = time.monotonic() + 20
    while not smtp_up.search((out / "mock.log").read_text()):
        if process.poll() is not None:
            raise RuntimeError(f"mail-mock-server exited (see {out / 'mock.log'})")
        if time.monotonic() >= deadline:
            process.terminate()
            raise RuntimeError(f"mail-mock-server did not start SMTP (see {out / 'mock.log'})")
        time.sleep(0.2)
    return process, ca


def judge(text: str, record: tlsproxy.Record) -> list[str]:
    problems = []
    for marker in MARKERS:
        found = marker in text
        print(f"  {marker}{'' if found else ' MISSING'}")
        if not found:
            problems.append(f"{marker} never reported")
    for line in re.findall(r"^MAIL:(?:ERROR|[A-Z]+:FAIL).*$", text, re.M):
        print(f"  {line}")
    for port, what in ((IMAPS_PORT, "IMAPS"), (SMTPS_PORT, "SMTPS")):
        mine = [h for h in record.handshakes if h.port == port]
        if not mine:
            problems.append(f"{what}: no TLS handshake reached the host")
        for h in mine:
            if h.sni != tlscerts.NAME:
                problems.append(f"{what}: handshake with SNI {h.sni!r}, expected {tlscerts.NAME}")
        print(f"  {what}: {len(mine)} handshakes ({', '.join(sorted({str(h.version) for h in mine}))})")
    if PASSWORD in text:
        problems.append("the password appeared in the serial log")
    denies = sorted(set(re.findall(r"^LABEL:DENY .*$", text, re.M)))
    for line in denies:
        print(f"  {line}")
    return problems


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    parser.add_argument("--no-build", action="store_true")
    parser.add_argument("--label-trace", action="store_true")
    parser.add_argument("--out", default="shots/mail")
    parser.add_argument("--accel", default="auto")
    parser.add_argument("--memory", default="2G")
    args = parser.parse_args()
    out = ROOT / args.out
    out.mkdir(parents=True, exist_ok=True)
    certs = tlscerts.load(out / "certs") if args.no_build else tlscerts.generate(out / "certs")
    if certs is None:
        print("MAIL:HARNESS:FAIL --no-build needs the certificates of the run that built the image")
        return 1
    hosts = out / "certs" / "hosts"
    hosts.write_text(f"10.0.2.2 {tlscerts.NAME}\n")
    if not args.no_build:
        build(certs, hosts, args.label_trace)
    mock, mock_ca = start_mock(mock_server(), out)
    fronts = None
    try:
        fronts = tlsproxy.Fronts(certs["good"].cert, certs["good"].key, mock_ca,
                                 (IMAPS_PORT, MOCK_IMAP_PORT), (SMTPS_PORT, MOCK_SMTP_PORT))
        command = [PY, "tools/screenshot/qemu_session.py", "--image", str(IMAGE), "--out", str(out),
                   "--script", str(SCRIPT), "--accel", args.accel, "--memory", args.memory,
                   "--net", "--net-forward", "none", "--fail-on", "PANIC", "--fail-on", "EXCEPTION"]
        session_ok = subprocess.call(command, cwd=ROOT) == 0
    finally:
        if fronts:
            fronts.close()
        mock.terminate()
    log = out / "serial.log"
    problems = [] if session_ok else ["the session did not finish (see summary.json)"]
    record = fronts.record if fronts else tlsproxy.Record()
    problems += judge(log.read_text(errors="replace") if log.is_file() else "", record)
    for problem in problems:
        print(f"  FAIL {problem}")
    print("MAIL:HARNESS:" + ("FAIL" if problems else "PASS"))
    return 1 if problems else 0


if __name__ == "__main__":
    sys.exit(main())
