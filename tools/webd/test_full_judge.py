#!/usr/bin/env python3
"""The full-view harness's judge fails when it should (`full_judge.py`), and its server serves what the
client's scenario expects (`full_server.py`): canned logs and records, one failure at a time."""

from __future__ import annotations

import sys
import unittest
import urllib.request
from pathlib import Path

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))
import full_judge as j  # noqa: E402
import full_server  # noqa: E402

GOOD_STEPS = "\n".join(f"{j.B}:{step}:PASS:x" for step in j.STEPS) + f"\n{j.B}:DONE:PASS:steps=17\n"
GOOD_LOG = f"{j.W}:SERVE:app.os.lazy.webd.view\n{j.B}:UP:PASS\n" + GOOD_STEPS.replace(
    f"{j.B}:DOWNLOAD:PASS:x", f"{j.B}:DOWNLOAD:PASS:name=file.bin bytes={full_server.FILE_BYTES}").replace(
    f"{j.B}:LATENCY:PASS:x", f"{j.B}:LATENCY:PASS:load_ms=900 wheel_ms min=30 median=40 max=90 n=20 lost=0 "
    "refresh_ms min=31 median=33 max=40 n=10 lost=0")


def req(path: str, cookie: str = "") -> dict:
    return {"path": path, "cookie": cookie}


GOOD_RECORD = {"requests": [
    req("/"), req("/pic.png"), req("/"), req("/pic.png"), req("/blocked.png"), req("/page2"),
    req("/cookie", "webd=browser"), req("/cookie"), req("/setcookie-b"), req("/cookie", "webd=own"),
    req("/cookie", "webd=browser"), req("/file.bin"),
]}


class Judge(unittest.TestCase):
    def test_a_good_run_passes(self):
        self.assertEqual(j.judge_service(GOOD_LOG), [])
        self.assertEqual(j.judge_steps(GOOD_LOG), [])
        self.assertEqual(j.judge_blocking(GOOD_RECORD), [])
        self.assertEqual(j.judge_cookies(GOOD_RECORD), [])
        self.assertEqual(j.judge_network(GOOD_RECORD), [])
        body = full_server.file_body()
        self.assertEqual(j.judge_download(GOOD_LOG, GOOD_RECORD, body, body), [])
        self.assertEqual(j.latency(GOOD_LOG)["wheel_median"], 40)

    def test_a_failed_or_missing_step_fails(self):
        self.assertTrue(j.judge_steps(GOOD_LOG.replace(f"{j.B}:POPUP:PASS:x", f"{j.B}:POPUP:FAIL:no event")))
        self.assertTrue(j.judge_steps(GOOD_LOG.replace(f"{j.B}:HELD:PASS:x\n", "")))
        self.assertTrue(j.judge_steps(GOOD_LOG.replace(f"{j.B}:DONE:PASS:steps=17", "")))
        self.assertTrue(j.judge_service(GOOD_LOG.replace("app.os.lazy.webd.view", "app.os.lazy.webd.light")))

    def test_a_blocked_picture_that_went_out_fails(self):
        leaked = {"requests": [req("/"), req("/blocked.png"), req("/"), req("/blocked.png")]}
        self.assertTrue(j.judge_blocking(leaked))
        never = {"requests": [req("/"), req("/pic.png"), req("/"), req("/page2")]}
        self.assertTrue(j.judge_blocking(never))

    def test_cookies_that_cross_profiles_fail(self):
        for index, value in [(7, "webd=browser"), (9, "webd=browser; webd=own"), (10, "webd=own; webd=browser"),
                             (6, "")]:
            record = {"requests": [dict(r) for r in GOOD_RECORD["requests"]]}
            record["requests"][index]["cookie"] = value
            self.assertTrue(j.judge_cookies(record), (index, value))
        self.assertTrue(j.judge_cookies({"requests": GOOD_RECORD["requests"][:8]}))

    def test_a_request_from_the_view_without_network_fails(self):
        self.assertTrue(j.judge_network({"requests": [req("/nonet.png")]}))

    def test_a_wrong_or_missing_download_fails(self):
        body = full_server.file_body()
        self.assertTrue(j.judge_download(GOOD_LOG, GOOD_RECORD, None, body))
        self.assertTrue(j.judge_download(GOOD_LOG, GOOD_RECORD, body[:-1], body))
        self.assertTrue(j.judge_download(GOOD_LOG, {"requests": []}, body, body))

    def test_denies_beyond_the_optional_ones_fail(self):
        own = "LABEL:DENY label=app:os.lazy.webdbrowse iface=1 method=2\n"
        optional = "LABEL:DENY label=app:os.lazy.webd/networkprocess file=/etc/ssl/openssl.cnf access=stat\n"
        other = "LABEL:DENY label=app:os.lazy.mail iface=1 method=2\n"
        self.assertTrue(j.judge_denies(own))
        self.assertEqual(j.judge_denies(optional + other), [])


class Server(unittest.TestCase):
    def test_it_serves_the_scenarios_pages_and_records_cookies(self):
        if full_server.busy():
            self.skipTest("port 18081 is in use")
        server = full_server.Server()
        try:
            base = f"http://{full_server.HOST}:{full_server.PORT}"
            with urllib.request.urlopen(base + "/") as reply:
                self.assertIn(b"<title>Index</title>", reply.read())
                self.assertIn("webd=browser", reply.headers.get("Set-Cookie", ""))
            request = urllib.request.Request(base + "/cookie", headers={"Cookie": "webd=own"})
            with urllib.request.urlopen(request) as reply:
                self.assertIn(b"<title>cookie=webd=own</title>", reply.read())
            with urllib.request.urlopen(base + "/file.bin") as reply:
                self.assertEqual(reply.read(), full_server.file_body())
                self.assertIn("attachment", reply.headers.get("Content-Disposition", ""))
            with urllib.request.urlopen(base + "/blocked.png") as reply:
                self.assertTrue(reply.read().startswith(b"\x89PNG"))
            record = server.snapshot()
            self.assertEqual([r["path"] for r in record["requests"]], ["/", "/cookie", "/file.bin", "/blocked.png"])
            self.assertEqual(record["requests"][1]["cookie"], "webd=own")
        finally:
            server.close()


if __name__ == "__main__":
    unittest.main()
