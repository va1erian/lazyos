#!/usr/bin/env python3
"""The harness's own tests: each verdict of `judge.py` fails when it should,
the mock answers like Fireworks, and the checked-in session is current.

    python tools/agent/test_judge.py
"""

from __future__ import annotations

import json
import sys
import tempfile
import unittest
from pathlib import Path

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))
import judge  # noqa: E402
import mockserver  # noqa: E402
import session  # noqa: E402

KEY = "good-key-0123456789"
WRONG = session.WRONG_KEY

GOOD_LOG = "\n".join([
    "AGENT:UP:PASS", "AGENT:READY:PASS", "AGENT:CONSENT:PASS", "AGENT:KEY:PASS:SET",
    "AGENT:CHAT:SEND:1", "AGENT:CHAT:FAIL:401", "AGENT:KEY:PASS:SET", "AGENT:CHAT:SEND:1",
    "AGENT:CHAT:FIRST:812", "AGENT:CHAT:DONE:40:90:2100", "AGENT:CHAT:DONE:40:300:4000",
    "AGENT:FOLLOW:0", "AGENT:CHAT:FAIL:stopped", "AGENT:QUIT:PASS", "",
])


def request(auth: str, **over) -> dict:
    base = {"method": "POST", "path": "/inference/v1/chat/completions", "host": "api.fireworks.ai",
            "sni": "api.fireworks.ai",
            "headers": {"authorization": auth, "user-agent": "LazyAgent/0.1.0"},
            "body": json.dumps({"model": judge.MODEL, "stream": True, "messages": []})}
    base.update(over)
    return base


GOOD_REQUESTS = [request(f"Bearer {WRONG}")] + [request(f"Bearer {KEY}") for _ in range(4)]


class SerialTests(unittest.TestCase):
    def test_a_good_log_passes(self) -> None:
        self.assertEqual(judge.judge_serial(GOOD_LOG, [KEY, WRONG]), [])

    def test_a_missing_marker_fails(self) -> None:
        for marker in judge.REQUIRED:
            log = GOOD_LOG.replace(marker, "x")
            self.assertTrue(judge.judge_serial(log, [KEY]), marker)

    def test_one_answer_is_not_enough(self) -> None:
        log = GOOD_LOG.replace("AGENT:CHAT:DONE:40:300:4000", "")
        self.assertTrue(any("two answers" in p for p in judge.judge_serial(log, [KEY])))

    def test_failure_markers_and_leaked_keys_and_links_fail(self) -> None:
        for bad in ("AGENT:BIND:FAIL:5", "AGENT:RUN:FAIL:oops", "AGENT:BUILD:FAIL:x", "AGENT:PANIC:FAIL: at x.rs:1"):
            self.assertTrue(judge.judge_serial(GOOD_LOG + bad + "\n", [KEY]), bad)
        self.assertTrue(judge.judge_serial(GOOD_LOG + f"some line {KEY}\n", [KEY]))
        self.assertTrue(judge.judge_serial(GOOD_LOG + f"{WRONG}\n", [KEY, WRONG]))
        self.assertTrue(judge.judge_serial(GOOD_LOG + "AGENT:LINK:ASKED:https://evil.test/x\n", [KEY]))
        self.assertEqual(judge.judge_serial(GOOD_LOG + "AGENT:LINK:ASKED\n", [KEY]), [])

    def test_a_live_run_does_not_need_the_stall_or_the_scroll(self) -> None:
        log = GOOD_LOG.replace("AGENT:CHAT:FAIL:stopped", "").replace("AGENT:FOLLOW:0", "")
        self.assertTrue(judge.judge_serial(log, [KEY]))
        log = log.replace("AGENT:CHAT:FAIL:401", "")
        self.assertEqual(judge.judge_serial(log, [KEY], live=True), [])


class RequestTests(unittest.TestCase):
    def test_good_requests_pass(self) -> None:
        self.assertEqual(judge.judge_requests(GOOD_REQUESTS, KEY, WRONG), [])

    def test_each_defect_fails(self) -> None:
        cases = {
            "nothing": [],
            "no wrong key first": GOOD_REQUESTS[1:],
            "too few good": GOOD_REQUESTS[:3],
            "wrong host": [request(f"Bearer {WRONG}", host="evil.test")] + GOOD_REQUESTS[1:],
            "wrong sni": [request(f"Bearer {WRONG}", sni="other")] + GOOD_REQUESTS[1:],
            "key in url": [request(f"Bearer {WRONG}", path=f"/inference/v1/chat/completions?k={WRONG}")] + GOOD_REQUESTS[1:],
            "key in body": GOOD_REQUESTS[:1] + [request(f"Bearer {KEY}", body=json.dumps({"k": KEY}))] + GOOD_REQUESTS[2:],
            "other model": GOOD_REQUESTS[:1] + [request(f"Bearer {KEY}", body=json.dumps({"model": "x", "stream": True}))] + GOOD_REQUESTS[2:],
            "not streaming": GOOD_REQUESTS[:1] + [request(f"Bearer {KEY}", body=json.dumps({"model": judge.MODEL}))] + GOOD_REQUESTS[2:],
            "tools in a plain chat": GOOD_REQUESTS[:1] + [request(f"Bearer {KEY}", body=json.dumps({"model": judge.MODEL, "stream": True, "tools": [{}]}))] + GOOD_REQUESTS[2:],
            "another path": GOOD_REQUESTS + [request(f"Bearer {KEY}", path="/v1/other")],
            "user agent": [request(f"Bearer {WRONG}", headers={"authorization": f"Bearer {WRONG}", "user-agent": "curl"})] + GOOD_REQUESTS[1:],
        }
        for name, requests in cases.items():
            self.assertTrue(judge.judge_requests(requests, KEY, WRONG), name)


def png(width: int, height: int, pixel) -> bytes:
    """A truecolour PNG whose pixel (x, y) is `pixel(x, y)`."""
    import struct
    import zlib

    def chunk(kind: bytes, data: bytes) -> bytes:
        body = kind + data
        return struct.pack(">I", len(data)) + body + struct.pack(">I", zlib.crc32(body))

    rows = b"".join(b"\0" + b"".join(bytes(pixel(x, y)) for x in range(width)) for y in range(height))
    return (b"\x89PNG\r\n\x1a\n" + chunk(b"IHDR", struct.pack(">IIBBBBB", width, height, 8, 2, 0, 0, 0))
            + chunk(b"IDAT", zlib.compress(rows)) + chunk(b"IEND", b""))


class ShotTests(unittest.TestCase):
    def shots(self, directory: str, make) -> list[Path]:
        paths = []
        for n in range(6):
            p = Path(directory) / f"shot_{n}.png"
            p.write_bytes(make(n))
            paths.append(p)
        return paths

    def test_varied_shots_pass(self) -> None:
        with tempfile.TemporaryDirectory() as d:
            paths = self.shots(d, lambda n: png(16, 16, lambda x, y: (40 + n * 9, x * 15, y * 15)))
            self.assertEqual(judge.judge_shots(paths), [])

    def test_too_few_or_identical_shots_fail(self) -> None:
        with tempfile.TemporaryDirectory() as d:
            paths = self.shots(d, lambda n: png(16, 16, lambda x, y: (200, x * 15, y * 15)))
            self.assertTrue(judge.judge_shots(paths[:3]))
            self.assertTrue(any("barely differ" in p for p in judge.judge_shots(paths)))

    def test_a_black_or_unreadable_shot_fails(self) -> None:
        with tempfile.TemporaryDirectory() as d:
            paths = self.shots(d, lambda n: png(16, 16, lambda x, y: (40 + n * 9, x * 15, y * 15)))
            paths[2].write_bytes(png(16, 16, lambda x, y: (0, 0, 0)))
            self.assertTrue(any("almost black" in p for p in judge.judge_shots(paths)))
            paths[2].write_bytes(b"not a png")
            self.assertTrue(any("cannot be read" in p for p in judge.judge_shots(paths)))


class MockTests(unittest.TestCase):
    def test_answers_depend_on_the_prompt(self) -> None:
        reasoning, text, stall = mockserver.answer_for("What is DHCP?")
        self.assertTrue(reasoning and "```" in text and "](https://" in text and not stall)
        self.assertTrue(mockserver.answer_for("please STALL")[2])
        long = mockserver.answer_for("LONG one")[1]
        self.assertGreater(long.count("\n\n"), 30)

    def test_sse_chunks_are_well_formed(self) -> None:
        data = mockserver.chunk(mockserver.sse(mockserver.delta(text="hi")))
        size, _, rest = data.partition(b"\r\n")
        self.assertEqual(int(size, 16), len(rest) - 2)
        self.assertIn(b'"content": "hi"', rest)


class SessionTests(unittest.TestCase):
    def test_the_key_is_never_in_the_session(self) -> None:
        text = session.render(session.steps())
        self.assertIn('"type_secret": "AGENT_TEST_KEY"', text)
        self.assertIn(WRONG, text)
        self.assertNotIn("sk-", text)

    def test_the_checked_in_example_is_current(self) -> None:
        current = session.EXAMPLE.read_text(encoding="utf-8") if session.EXAMPLE.is_file() else ""
        self.assertEqual(current, session.render(session.steps()), "run: python tools/agent/session.py --write")

    def test_a_live_session_provokes_nothing_the_service_may_not_do(self) -> None:
        text = session.render(session.steps(live=True))
        self.assertIn('"type_secret": "AGENT_TEST_KEY"', text)
        for left_out in (WRONG, "LONG", "STALL", "AGENT:FOLLOW:0", "FAIL:stopped", "FAIL:401"):
            self.assertNotIn(left_out, text)
        # What it waits for, the live judge asks for.
        waited = {str(s["wait_for"]) for s in session.steps(live=True) if "wait_for" in s}
        for marker in judge.REQUIRED:
            if marker not in judge.LIVE_SKIPS and marker not in ("AGENT:CHAT:SEND:", "AGENT:CHAT:FIRST:"):
                self.assertTrue(any(marker in w or w in marker for w in waited), marker)

    def test_an_unexpected_failure_status_fails(self) -> None:
        self.assertTrue(judge.judge_serial(GOOD_LOG + "AGENT:CHAT:FAIL:503\n", [KEY]))
        self.assertTrue(judge.judge_serial(GOOD_LOG.replace("FAIL:stopped", "FAIL:401"), [KEY], live=True))

    def test_the_markers_the_judge_wants_are_waited_for(self) -> None:
        waited = " ".join(str(s.get("wait_for", "")) for s in session.steps())
        for marker in ("AGENT:UP:PASS", "AGENT:CONSENT:PASS", "AGENT:CHAT:FAIL:401", "AGENT:CHAT:DONE:",
                       "AGENT:FOLLOW:0", "AGENT:CHAT:FAIL:stopped"):
            self.assertIn(marker, waited)


if __name__ == "__main__":
    unittest.main()
