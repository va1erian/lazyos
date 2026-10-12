"""The verdicts of LazySky's harness: what the guest printed, what the mock
saw and what the screen showed. Every function returns a list of problems
(empty = pass) so `test_judge.py` can prove each one fails when it should.
"""

from __future__ import annotations

import json
import re
import sys
from pathlib import Path

HERE = Path(__file__).resolve().parent
ROOT = HERE.parent.parent

HANDLE = "me.test"
ACCESS = "access.harness.jwt"
REFRESH = "refresh.harness.jwt"
#: R7 (docs/lazysky-plan.md §5): paint and tick budgets, in milliseconds.
BUDGET_PAINT_MAX_TCG = 25.0
BUDGET_PAINT_P99_HW = 8.0
BUDGET_TICK_MAX = 4.0
#: Markers a run must print, in order of appearance.
REQUIRED = ["SKY:UP:PASS", f"SKY:LOGIN:PASS:{HANDLE}", "SKY:FEED:PAGE:30:0", "SKY:FEED:ROWS:30"]
FAILURES = re.compile(r"^SKY:(?:LOGIN:FAIL|BIND:FAIL|RUN:FAIL|BUILD:FAIL|[A-Z]+:FAIL)[^\n]*$", re.M)


def judge_serial(text: str, password: str) -> list[str]:
    """The markers, no failure marker, and no secret in the log."""
    problems = []
    for marker in REQUIRED:
        if marker not in text:
            problems.append(f"{marker} never printed")
    pictures = [int(n) for n in re.findall(r"^SKY:IMAGES:(\d+)", text, re.M)]
    if not pictures or max(pictures) < 3:
        problems.append("fewer than 3 pictures were decoded (SKY:IMAGES:<n>)")
    for line in FAILURES.findall(text):
        problems.append(f"failure marker: {line}")
    for secret, what in ((password, "the app password"), (ACCESS, "the access token"),
                         (REFRESH, "the refresh token")):
        if secret and secret in text:
            problems.append(f"{what} appeared in the serial log")
    return problems


def perf_lines(text: str) -> list[dict]:
    """Every `SKY:PERF:paint_p50=...` line as numbers."""
    out = []
    for line in re.findall(r"^SKY:PERF:(paint_p50=[^\n]+)$", text, re.M):
        out.append({k: float(v) for k, v in (field.split("=") for field in line.split(":"))})
    return out


def judge_perf(text: str, accelerated: bool) -> list[str]:
    """R7: no frame or tick over its budget in any report."""
    lines = perf_lines(text)
    if not lines:
        return ["no SKY:PERF line: the feed never scrolled for 2 s"]
    problems = []
    for p in lines:
        if accelerated and p["paint_p99"] > BUDGET_PAINT_P99_HW:
            problems.append(f"paint p99 {p['paint_p99']} ms > {BUDGET_PAINT_P99_HW} ms")
        if not accelerated and p["paint_max"] > BUDGET_PAINT_MAX_TCG:
            problems.append(f"paint max {p['paint_max']} ms > {BUDGET_PAINT_MAX_TCG} ms")
        if p["tick_max"] > BUDGET_TICK_MAX:
            problems.append(f"tick max {p['tick_max']} ms > {BUDGET_TICK_MAX} ms")
    return problems


def judge_requests(requests: list[dict], password: str) -> list[str]:
    """What the mock server recorded (`mockserver.Request` as dicts)."""
    problems = []
    signins = [r for r in requests if r["path"].endswith("com.atproto.server.createSession")]
    if not signins:
        return ["the guest never signed in (no createSession)"]
    try:
        body = json.loads(signins[0]["body"])
    except json.JSONDecodeError:
        return ["createSession's body is not JSON"]
    if body.get("identifier") != HANDLE:
        problems.append(f"createSession identifier {body.get('identifier')!r}, expected {HANDLE!r}")
    if password and body.get("password") != password:
        problems.append("createSession carried another password than the one typed")
    if signins[0]["sni"] != "bsky.test":
        problems.append(f"sign-in SNI {signins[0]['sni']!r}, expected bsky.test")
    xrpc = [r for r in requests if "/xrpc/" in r["path"] and r not in signins]
    if not any("getTimeline" in r["path"] for r in xrpc):
        problems.append("the timeline was never fetched")
    for r in xrpc:
        if r["headers"].get("authorization") != f"Bearer {ACCESS}":
            problems.append(f"{r['path']} without the bearer token")
    pictures = [r for r in requests if re.match(r"/(thumb|full|avatar)/", r["path"])]
    if len(pictures) < 3:
        problems.append(f"only {len(pictures)} picture fetches reached the CDN")
    for r in pictures:
        if r["sni"] != "cdn.bsky.test":
            problems.append(f"picture {r['path']} with SNI {r['sni']!r}, expected cdn.bsky.test")
            break
    if any(r["headers"].get("user-agent", "").startswith("LazySky/") is False for r in requests):
        problems.append("a request without the LazySky/ user agent")
    return problems


def judge_shots(paths: list[Path]) -> list[str]:
    """The screenshots exist, are not black and are not all alike."""
    import hashlib
    if len(paths) < 3:
        return [f"only {len(paths)} screenshots"]
    sys.path.insert(0, str(ROOT / "tools" / "screenshot"))
    import pngstats  # noqa: E402
    problems = []
    for path in paths:
        stats = pngstats.analyze(str(path)) if hasattr(pngstats, "analyze") else None
        if stats is not None and stats.get("nonblack_frac", 1.0) < 0.05:
            problems.append(f"{path.name} is almost black")
    digests = {hashlib.sha256(p.read_bytes()).hexdigest() for p in paths}
    if len(digests) < 3:
        problems.append("the screenshots barely differ: the app did not change")
    return problems
