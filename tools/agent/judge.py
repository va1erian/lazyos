"""The verdicts of LazyAgent's harness: what the guest printed, what the mock
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

#: Markers a run must print.
REQUIRED = [
    "AGENT:UP:PASS", "AGENT:READY:PASS", "AGENT:CONSENT:PASS", "AGENT:KEY:PASS:SET",
    "AGENT:CHAT:SEND:", "AGENT:CHAT:FIRST:", "AGENT:CHAT:DONE:", "AGENT:CHAT:FAIL:401",
    "AGENT:CHAT:FAIL:stopped", "AGENT:FOLLOW:0", "AGENT:QUIT:PASS",
]
#: A failure marker that is not one the session provokes (401 and Stop).
FAILURES = re.compile(r"^AGENT:(?:BIND|RUN|BUILD|PANIC)[A-Z:]*FAIL[^\n]*$", re.M)
#: The model's default id (docs/ai-agent-plan.md §4).
MODEL = "accounts/fireworks/models/deepseek-v4p1-flash"


#: What a live run (the real service, no stall, no long reply) cannot show.
LIVE_SKIPS = ("AGENT:CHAT:FAIL:stopped", "AGENT:FOLLOW:0")


def judge_serial(text: str, secrets: list[str], live: bool = False) -> list[str]:
    """The markers, no failure marker, and no secret in the log."""
    problems = []
    for marker in REQUIRED:
        if live and marker in LIVE_SKIPS:
            continue
        if marker not in text:
            problems.append(f"{marker} never printed")
    done = re.findall(r"^AGENT:CHAT:DONE:(\d+):(\d+):(\d+)$", text, re.M)
    if len(done) < 2:
        problems.append("fewer than two answers completed (AGENT:CHAT:DONE:<in>:<out>:<ms>)")
    for line in FAILURES.findall(text):
        problems.append(f"failure marker: {line}")
    for secret in secrets:
        if secret and secret in text:
            problems.append("a key appeared in the serial log")
    # A marker never carries text the model wrote or an address.
    for line in re.findall(r"^AGENT:LINK:[^\n]*$", text, re.M):
        if "http" in line:
            problems.append(f"a marker carries a link: {line}")
    return problems


def judge_requests(requests: list[dict], good_key: str, wrong_key: str) -> list[str]:
    """What the mock server recorded (`mockserver.Request` as dicts)."""
    chats = [r for r in requests if r["path"] == "/inference/v1/chat/completions"]
    if not chats:
        return ["the guest never asked for a chat completion"]
    problems = []
    if chats[0]["headers"].get("authorization") != f"Bearer {wrong_key}":
        problems.append("the first request did not carry the wrong key the session typed")
    good = [r for r in chats if r["headers"].get("authorization") == f"Bearer {good_key}"]
    if len(good) < 3:
        problems.append(f"only {len(good)} requests carried the right key (answer, title, long, stall)")
    for r in chats:
        if r["host"] != "api.fireworks.ai" or r["sni"] != "api.fireworks.ai":
            problems.append(f"host {r['host']!r} / SNI {r['sni']!r}, expected api.fireworks.ai")
            break
        if good_key in r["path"] or wrong_key in r["path"]:
            problems.append("a key is in a URL")
        if good_key in r["body"] or wrong_key in r["body"]:
            problems.append("a key is in a request body")
        if not r["headers"].get("user-agent", "").startswith("LazyAgent/"):
            problems.append("a request without the LazyAgent/ user agent")
            break
    first = next((r for r in good), None)
    if first is not None:
        try:
            body = json.loads(first["body"])
        except json.JSONDecodeError:
            return problems + ["a request body is not JSON"]
        if body.get("model") != MODEL:
            problems.append(f"model {body.get('model')!r}, expected the default {MODEL!r}")
        if body.get("stream") is not True:
            problems.append("the request did not ask for a stream")
        if body.get("tools"):
            problems.append("a plain chat sent tools")
    if any(r["path"] != "/inference/v1/chat/completions" for r in requests):
        problems.append("a request to another path")
    return problems


def judge_shots(paths: list[Path]) -> list[str]:
    """The screenshots exist, are not black and are not all alike."""
    import hashlib
    if len(paths) < 6:
        return [f"only {len(paths)} screenshots"]
    sys.path.insert(0, str(ROOT / "tools" / "screenshot"))
    import pngstats  # noqa: E402
    problems = []
    for path in paths:
        stats = pngstats.analyze(str(path)) if hasattr(pngstats, "analyze") else None
        if stats is not None and stats.get("nonblack_frac", 1.0) < 0.05:
            problems.append(f"{path.name} is almost black")
    digests = {hashlib.sha256(p.read_bytes()).hexdigest() for p in paths}
    if len(digests) < 6:
        problems.append("the screenshots barely differ: the app did not change")
    return problems
