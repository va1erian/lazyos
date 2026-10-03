#!/usr/bin/env python3
"""Host integration test for `fetch`/`curl`/`wget` (nettls) against local TLS servers.

Builds the host (glibc) binary with cargo, generates a throwaway test CA and
leaf certificates with the ``cryptography`` package, and runs Python ``ssl``
HTTPS servers on 127.0.0.1. Each case runs the real binary and checks its exit
code, output and, for refusals, that the server saw no HTTP request (the
client must stop before sending application data).

Cases: the right CA through ``SSL_CERT_FILE`` (and ``--cacert``), a wrong host
name, an expired leaf, a not-yet-valid leaf (the message must show the clock),
a self-signed leaf, an unknown CA, gzip decoding, a redirect chain and its
limit, an https -> http downgrade, the server-side ALPN/SNI the client sent,
wget's default file naming, and a missing CA bundle.

Usage::

    python tools/nettls/test_host.py            # build (debug) and run
    python tools/nettls/test_host.py --binary target/nettls/host-fetch

Exit status 0 when every case passed. Needs ``pip install cryptography``.
"""

from __future__ import annotations

import argparse
import datetime
import gzip
import http.server
import ipaddress
import os
import shutil
import ssl
import subprocess
import sys
import tempfile
import threading
from pathlib import Path

from cryptography import x509
from cryptography.hazmat.primitives import hashes, serialization
from cryptography.hazmat.primitives.asymmetric import ec, rsa
from cryptography.x509.oid import NameOID

ROOT = Path(__file__).resolve().parent.parent.parent
CRATE = ROOT / "nettls"
NOW = datetime.datetime.now(datetime.timezone.utc)
DAY = datetime.timedelta(days=1)
PAGE = b"<html><body>nettls host test page</body></html>\n" * 50


def name(common: str) -> x509.Name:
    return x509.Name([x509.NameAttribute(NameOID.COMMON_NAME, common)])


def new_key(kind: str):
    if kind == "rsa":
        return rsa.generate_private_key(public_exponent=65537, key_size=2048)
    return ec.generate_private_key(ec.SECP384R1() if kind == "p384" else ec.SECP256R1())


def make_ca(common: str, kind: str = "p256"):
    key = new_key(kind)
    cert = (
        x509.CertificateBuilder()
        .subject_name(name(common))
        .issuer_name(name(common))
        .public_key(key.public_key())
        .serial_number(x509.random_serial_number())
        .not_valid_before(NOW - 10 * DAY)
        .not_valid_after(NOW + 3650 * DAY)
        .add_extension(x509.BasicConstraints(ca=True, path_length=None), critical=True)
        .add_extension(
            x509.KeyUsage(False, False, False, False, False, True, True, False, False),
            critical=True,
        )
        .sign(key, hashes.SHA256())
    )
    return key, cert


def make_leaf(ca, dns: str, not_before, not_after, self_signed: bool = False, kind: str = "p256"):
    key = new_key(kind)
    issuer_key, issuer_cert = ca if not self_signed else (key, None)
    builder = (
        x509.CertificateBuilder()
        .subject_name(name(dns))
        .issuer_name(issuer_cert.subject if issuer_cert else name(dns))
        .public_key(key.public_key())
        .serial_number(x509.random_serial_number())
        .not_valid_before(not_before)
        .not_valid_after(not_after)
        .add_extension(
            x509.SubjectAlternativeName(
                [x509.DNSName(dns), x509.IPAddress(ipaddress.ip_address("127.0.0.1"))]
                if dns == "localhost"
                else [x509.DNSName(dns)]
            ),
            critical=False,
        )
        .add_extension(x509.ExtendedKeyUsage([x509.oid.ExtendedKeyUsageOID.SERVER_AUTH]), critical=False)
    )
    return key, builder.sign(issuer_key, hashes.SHA256())


def pem(cert) -> bytes:
    return cert.public_bytes(serialization.Encoding.PEM)


def key_pem(key) -> bytes:
    return key.private_bytes(
        serialization.Encoding.PEM, serialization.PrivateFormat.PKCS8, serialization.NoEncryption()
    )


class Handler(http.server.BaseHTTPRequestHandler):
    """Serves the test routes and records every request it parsed."""

    def log_message(self, *args) -> None:  # quiet
        pass

    def do_HEAD(self) -> None:  # noqa: N802
        self.head_only = True
        self.do_GET()

    def do_GET(self) -> None:  # noqa: N802 (http.server naming)
        headers = {k.lower(): v for k, v in self.headers.items()}
        self.server.requests.append((self.path, headers))
        self.server.ciphers.append(self.request.cipher())
        port = self.server.server_address[1]
        if self.path == "/":
            self.reply(200, PAGE, "text/html")
        elif self.path == "/gzip":
            if "gzip" not in headers.get("accept-encoding", ""):
                self.reply(406, b"client did not offer gzip\n", "text/plain")
            else:
                self.reply(200, gzip.compress(PAGE), "text/html", {"Content-Encoding": "gzip"})
        elif self.path.startswith("/hop/"):
            left = int(self.path.rsplit("/", 1)[1])
            target = "/" if left == 0 else f"/hop/{left - 1}"
            self.reply(302, b"", "text/plain", {"Location": target})
        elif self.path == "/downgrade":
            self.reply(301, b"", "text/plain", {"Location": f"http://localhost:{port}/"})
        elif self.path == "/files/report.txt":
            self.reply(200, b"report\n", "text/plain")
        elif self.path == "/missing":
            self.reply(404, b"no\n", "text/plain")
        else:
            self.reply(404, b"unknown route\n", "text/plain")

    def reply(self, code: int, body: bytes, ctype: str, extra: dict | None = None) -> None:
        self.send_response(code)
        self.send_header("Content-Type", ctype)
        self.send_header("Content-Length", str(len(body)))
        for k, v in (extra or {}).items():
            self.send_header(k, v)
        self.end_headers()
        if not getattr(self, "head_only", False):
            self.wfile.write(body)


class Server:
    """An HTTPS server on 127.0.0.1 with one leaf certificate."""

    def __init__(self, workdir: Path, label: str, key, cert, chain: list = (),
                 tls12_ciphers: str | None = None) -> None:
        cert_file = workdir / f"{label}.crt"
        key_file = workdir / f"{label}.key"
        cert_file.write_bytes(pem(cert) + b"".join(pem(c) for c in chain))
        key_file.write_bytes(key_pem(key))
        context = ssl.SSLContext(ssl.PROTOCOL_TLS_SERVER)
        context.minimum_version = ssl.TLSVersion.TLSv1_2
        if tls12_ciphers:
            # TLS 1.2 only, with exactly these suites.
            context.maximum_version = ssl.TLSVersion.TLSv1_2
            context.set_ciphers(tls12_ciphers)
        context.load_cert_chain(cert_file, key_file)
        context.set_alpn_protocols(["http/1.1"])
        self.handshakes: list[tuple] = []

        def record(sock, server_name, _ctx):
            self.handshakes.append(("sni", server_name))

        context.sni_callback = record
        self.httpd = http.server.ThreadingHTTPServer(("127.0.0.1", 0), Handler)
        self.httpd.requests = []
        self.httpd.ciphers = []
        self.httpd.socket = context.wrap_socket(self.httpd.socket, server_side=True)
        self.port = self.httpd.server_address[1]
        threading.Thread(target=self.httpd.serve_forever, daemon=True).start()

    @property
    def requests(self) -> list:
        return self.httpd.requests

    def url(self, path: str = "/") -> str:
        return f"https://localhost:{self.port}{path}"

    def stop(self) -> None:
        self.httpd.shutdown()


class Runner:
    def __init__(self, binary: Path, workdir: Path) -> None:
        self.bindir = workdir / "bin"
        self.bindir.mkdir()
        for alias in ("fetch", "curl", "wget"):
            (self.bindir / alias).symlink_to(binary.resolve())
        self.workdir = workdir
        self.failures: list[str] = []

    def run(self, tool: str, args: list[str], ca: Path | None, cwd: Path | None = None):
        env = {k: v for k, v in os.environ.items() if k not in ("SSL_CERT_FILE", "HTTPS_PROXY", "https_proxy")}
        if ca is not None:
            env["SSL_CERT_FILE"] = str(ca)
        done = subprocess.run(
            [str(self.bindir / tool), *args], capture_output=True, env=env, timeout=60,
            cwd=cwd or self.workdir,
        )
        return done.returncode, done.stdout, done.stderr.decode(errors="replace")

    def check(self, label: str, ok: bool, detail: str = "") -> None:
        print(f"{'PASS' if ok else 'FAIL'} {label}" + (f": {detail}" if not ok and detail else ""))
        if not ok:
            self.failures.append(label)


def refused(r: Runner, label: str, server: Server, ca: Path, code: int, needle: str) -> None:
    """The client must fail with `code`, mention `needle`, and send no request."""
    before = len(server.requests)
    rc, _, err = r.run("curl", ["-sS", server.url()], ca)
    r.check(f"{label}: exit {code}", rc == code, f"exit {rc}, stderr {err!r}")
    r.check(f"{label}: explains '{needle}'", needle in err, err)
    r.check(f"{label}: no request reached the server", len(server.requests) == before)


def run_cases(r: Runner, work: Path) -> None:
    ca = make_ca("nettls host-test CA")
    other_ca = make_ca("some other CA")
    ca_file = work / "ca.pem"
    ca_file.write_bytes(pem(ca[1]))
    good = Server(work, "good", *make_leaf(ca, "localhost", NOW - DAY, NOW + 30 * DAY))
    servers = [good]
    try:
        rc, out, err = r.run("fetch", ["-v", good.url()], ca_file)
        r.check("right CA via SSL_CERT_FILE: page", rc == 0 and out == PAGE, f"rc={rc} {err}")
        r.check("-v prints TLS:HANDSHAKE", "TLS:HANDSHAKE version=TLSv1." in err and "alpn=http/1.1" in err, err)
        r.check("-v prints the chain", 'TLS:CHAIN depth=0 subject="CN=localhost"' in err, err)
        r.check("server saw SNI localhost", ("sni", "localhost") in good.handshakes, str(good.handshakes))
        rc, out, _ = r.run("curl", ["-s", "--cacert", str(ca_file), good.url()], None)
        r.check("right CA via --cacert", rc == 0 and out == PAGE, f"rc={rc}")
        rc, out, err = r.run("curl", ["-s", good.url("/gzip")], ca_file)
        sent = good.requests[-1][1].get("accept-encoding", "")
        r.check("gzip offered and decoded", rc == 0 and out == PAGE and "gzip" in sent, f"rc={rc} {sent}")
        rc, out, _ = r.run("curl", ["-sL", "-w", "%{http_code} %{num_redirects}", good.url("/hop/3")], ca_file)
        r.check("curl -L follows a chain", rc == 0 and out == PAGE + b"200 4", repr(out[-20:]))
        rc, _, _ = r.run("curl", ["-s", "-o", "/dev/null", "-w", "%{http_code}", good.url("/hop/1")], ca_file)
        r.check("curl without -L does not follow", rc == 0)
        rc, _, err = r.run("curl", ["-sSL", "--max-redirs", "2", good.url("/hop/5")], ca_file)
        r.check("too many redirects: exit 47", rc == 47, f"rc={rc} {err}")
        before = len(good.requests)
        rc, _, err = r.run("fetch", ["-L", good.url("/downgrade")], ca_file)
        r.check("https -> http redirect refused", rc == 1 and "plain http" in err, f"rc={rc} {err}")
        r.check("downgrade: no plain-http request", len(good.requests) == before + 1)
        rc, _, _ = r.run("curl", ["-sf", good.url("/missing")], ca_file)
        r.check("curl -f on 404: exit 22", rc == 22, f"rc={rc}")
        rc, _, _ = r.run("wget", ["-q", good.url("/missing")], ca_file)
        r.check("wget on 404: exit 8", rc == 8, f"rc={rc}")
        rc, _, err = r.run("wget", ["-q", good.url("/files/report.txt")], ca_file)
        saved = work / "report.txt"
        r.check("wget saves under the URL's name", rc == 0 and saved.read_bytes() == b"report\n", err)
        rc, _, _ = r.run("wget", ["-q", good.url("/files/report.txt")], ca_file)
        r.check("wget does not clobber (report.txt.1)", rc == 0 and (work / "report.txt.1").is_file())
        rc, _, _ = r.run("wget", ["-q", good.url("/")], ca_file)
        r.check("wget names a directory index.html", rc == 0 and (work / "index.html").read_bytes() == PAGE)
        rc, out, _ = r.run("wget", ["-qO", "-", good.url("/")], ca_file)
        r.check("wget -O - writes stdout", rc == 0 and out == PAGE)
        rc, out, _ = r.run("curl", ["-sI", good.url("/")], ca_file)
        r.check("curl -I prints headers", rc == 0 and out.split(b"\n")[0] in (b"HTTP/1.0 200 OK", b"HTTP/1.1 200 OK"), repr(out[:40]))
        rc, _, err = r.run("curl", ["-sS", good.url()], work / "no-such-bundle.pem")
        r.check("missing bundle: exit 77, names the file", rc == 77 and "no-such-bundle.pem" in err, err)
        empty = work / "empty.pem"
        empty.write_bytes(b"")
        rc, _, err = r.run("curl", ["-sS", good.url()], empty)
        r.check("empty bundle: hard error", rc == 77 and "no certificates" in err, err)
        rc, _, err = r.run("curl", ["-sS", good.url()], None)
        r.check("system bundle does not trust the test CA", rc == 60, f"rc={rc} {err}")

        variants(r, work)

        wrong_name = Server(work, "wrongname", *make_leaf(ca, "other.example", NOW - DAY, NOW + 30 * DAY))
        expired = Server(work, "expired", *make_leaf(ca, "localhost", NOW - 60 * DAY, NOW - 30 * DAY))
        future = Server(work, "future", *make_leaf(ca, "localhost", NOW + 30 * DAY, NOW + 60 * DAY))
        selfsigned = Server(
            work, "selfsigned", *make_leaf(None, "localhost", NOW - DAY, NOW + 30 * DAY, self_signed=True)
        )
        unknown = Server(work, "unknown", *make_leaf(other_ca, "localhost", NOW - DAY, NOW + 30 * DAY))
        servers += [wrong_name, expired, future, selfsigned, unknown]
        refused(r, "wrong host name", wrong_name, ca_file, 60, "not valid for localhost")
        refused(r, "expired leaf", expired, ca_file, 60, "expired at")
        refused(r, "not-yet-valid leaf", future, ca_file, 60, "the system clock reads")
        refused(r, "self-signed leaf", selfsigned, ca_file, 60, "unknown issuer")
        refused(r, "unknown CA", unknown, ca_file, 60, "unknown issuer")
        rc, _, err = r.run("wget", ["-q", "-d", expired.url()], ca_file)
        r.check("wget TLS failure: exit 5 + TLS:FAIL", rc == 5 and "TLS:FAIL reason=" in err, f"rc={rc} {err}")
    finally:
        for server in servers:
            server.stop()


def variants(r: Runner, work: Path) -> None:
    """Every key type and TLS 1.2 suite the provider must handle, each
    against a server that allows only that combination."""
    cases = [
        # label, CA/leaf key type, TLS 1.2 OpenSSL cipher (None: TLS 1.3)
        ("tls13 rsa-2048 chain (PSS)", "rsa", None),
        ("tls13 p384 chain", "p384", None),
        ("tls12 ECDHE-ECDSA-AES128-GCM", "p256", "ECDHE-ECDSA-AES128-GCM-SHA256"),
        ("tls12 ECDHE-ECDSA-CHACHA20", "p256", "ECDHE-ECDSA-CHACHA20-POLY1305"),
        ("tls12 ECDHE-RSA-AES256-GCM", "rsa", "ECDHE-RSA-AES256-GCM-SHA384"),
        ("tls12 ECDHE-RSA-CHACHA20", "rsa", "ECDHE-RSA-CHACHA20-POLY1305"),
    ]
    for label, kind, cipher in cases:
        ca = make_ca(f"{kind} test CA", kind)
        ca_file = work / f"ca-{kind}.pem"
        ca_file.write_bytes(pem(ca[1]))
        leaf = make_leaf(ca, "localhost", NOW - DAY, NOW + 30 * DAY, kind=kind)
        server = Server(work, f"v-{kind}-{cipher}", *leaf, tls12_ciphers=cipher)
        try:
            rc, out, err = r.run("fetch", ["-v", server.url()], ca_file)
            seen = server.httpd.ciphers[-1] if server.httpd.ciphers else None
            want_version = "TLSv1.2" if cipher else "TLSv1.3"
            ok = rc == 0 and out == PAGE and seen is not None and seen[1] == want_version
            if cipher:
                ok = ok and seen[0] == cipher
            r.check(label, ok, f"rc={rc} server saw {seen} {err[-300:]}")
        finally:
            server.stop()


def build_host_binary() -> Path:
    subprocess.run(["cargo", "build", "--manifest-path", str(CRATE / "Cargo.toml")], check=True)
    return CRATE / "target" / "debug" / "fetch"


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--binary", type=Path, help="a host build of fetch (default: cargo build)")
    args = parser.parse_args()
    binary = args.binary or build_host_binary()
    work = Path(tempfile.mkdtemp(prefix="nettls-host-"))
    try:
        runner = Runner(binary, work)
        run_cases(runner, work)
    finally:
        shutil.rmtree(work, ignore_errors=True)
    if runner.failures:
        print(f"{len(runner.failures)} case(s) failed: {', '.join(runner.failures)}")
        return 1
    print("all host TLS cases passed")
    return 0


if __name__ == "__main__":
    sys.exit(main())
