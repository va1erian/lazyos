"""The console session the TLS harness types into the guest (docs/tls-plan.md §8).

Each line runs `curl`, `wget` or `fetch` against the host's servers
(`tlspeers.py`) and prints `TLS:<check>:PASS|FAIL`. The marker is built from a
shell variable, so the echo of the typed command never contains it and a
gate only opens on the command's own answer. Bodies are compared by a
SHA-256 prefix computed here from the pages the servers hold.
"""

from __future__ import annotations

import hashlib

import tlscerts
import tlspeers as tp

PROMPT = "/ #"
HELPERS = (
    "m=TLS; ok() { if grep -qxF -- \"$1\"; then echo $m:$2:PASS; else echo $m:$2:FAIL; fi; }; "
    "chk() { if [ \"$2\" = \"$3\" ]; then echo $m:$1:PASS; else echo $m:$1:FAIL:$2; fi; }; "
    "no() { n=$1; shift; if \"$@\" >/dev/null 2>/tmp/no.err; then echo $m:$n:FAIL; "
    "else echo $m:$n:PASS; head -c 200 /tmp/no.err; echo; fi; }; "
    "h() { sha256sum | cut -c1-16; }"
)


def digest(body: bytes) -> str:
    return hashlib.sha256(body).hexdigest()[:16]


def url(port: int, path: str = "/", name: str = tlscerts.NAME, scheme: str = "https") -> str:
    return f"{scheme}://{name}:{port}{path}"


def checks() -> list[tuple[str, str]]:
    """(marker name, command) for every positive and negative check, in order."""
    good = url(tp.GOOD_PORT, "")
    index = digest(tp.INDEX)
    positive = [
        ("resolv", "grep -c '^nameserver ' /etc/resolv.conf | ok 1 resolv"),
        ("bundle", "test $(grep -c 'BEGIN CERTIFICATE' /etc/ssl/certs/ca-certificates.crt) -gt 100 "
                   "&& echo $m:bundle:PASS"),
        ("fetch", f"fetch {good}/ | h | ok {index} fetch"),
        ("curl", f"curl -sS {good}/ | h | ok {index} curl"),
        ("tls12", f"curl -sS {url(tp.TLS12_PORT)} | h | ok {index} tls12"),
        ("rsa", f"curl -sS {url(tp.RSA_PORT, name=tlscerts.RSA_NAME)} | h | ok {index} rsa"),
        ("chunked", f"curl -sS {good}/chunked | h | ok {digest(tp.CHUNKED)} chunked"),
        ("gzip", f"curl -sS {good}/gzip | h | ok {digest(tp.GZIPPED)} gzip"),
        ("big", f"wget -q -O /tmp/big {good}/big; h < /tmp/big | ok {digest(tp.BIG)} big"),
        ("wgetname", f"cd /tmp && wget -q {good}/files/page.txt; h < /tmp/page.txt | "
                     f"ok {digest(tp.PAGE)} wgetname; cd /"),
        ("redirect", f"curl -sSL {good}/redirect/3 | h | ok {index} redirect"),
        ("noredirect", f"curl -s -o /dev/null -w '%{{http_code}}' {good}/redirect/3 | ok 301 noredirect"),
        ("head", f"curl -sI {good}/ | tr -d '\\r' | grep -ci '^content-length: {len(tp.INDEX)}$' "
                 "| ok 1 head"),
        ("plain", f"curl -sS {url(tp.PLAIN_PORT, scheme='http')} | h | ok {index} plain"),
        ("fail22", f"curl -sf {good}/nope >/dev/null; chk fail22 $? 22"),
        ("verbose", f"fetch -v {good}/ 2>&1 >/dev/null | grep -c '^TLS:HANDSHAKE' | ok 1 verbose"),
    ]
    negative = [(role, f"no {role} curl -sS {url(port)}") for role, port in tp.NEGATIVE_PORTS.items()]
    negative += [
        ("downgrade", f"no downgrade curl -sSL {good}/downgrade"),
        ("insecure", f"no insecure curl -k {good}/"),
        ("nocheck", f"no nocheck wget --no-check-certificate -O - {good}/"),
        ("nobundle", f"no nobundle env SSL_CERT_FILE=/nope curl -sS {good}/"),
    ]
    return positive + negative


def live_checks(urls: list[str]) -> list[tuple[str, str]]:
    """`--live`: real public sites, verified against the image's roots only."""
    out = [("bundle", "test $(grep -c 'BEGIN CERTIFICATE' /etc/ssl/certs/ca-certificates.crt) -gt 100 "
                      "&& echo $m:bundle:PASS")]
    for n, target in enumerate(urls):
        out.append((f"live{n}", f"curl -sSL -o /dev/null -w '%{{http_code}}' {target} | ok 200 live{n}"))
    out.append(("livev", f"fetch -v -o /dev/null {urls[0]} 2>&1 | grep '^TLS:' ; echo $m:livev:PASS"))
    return out


def script(items: list[tuple[str, str]], step_timeout: float) -> list[dict]:
    """The `qemu_session.py` steps: wait for the shell and an address, the
    helpers, then one typed line per check gated on its marker."""
    steps: list[dict] = [
        {"wait_for": PROMPT, "timeout": 300},
        {"wait_for": "NETD:ADDR", "timeout": 300},
        {"type": HELPERS},
        {"key": "enter", "until": PROMPT, "timeout": 60, "retries": 1},
    ]
    for name, command in items:
        steps.append({"type": command})
        steps.append({"key": "enter", "until": f"TLS:{name}:", "timeout": step_timeout})
    steps += [{"wait": 1.0}, {"shot": "tls"}, {"quit": True}]
    return steps
