#!/usr/bin/env python3
"""Tests of the LazyWeb harness: the judge must fail when it should, the
stand-in sites must answer as the real ones do, and the checked-in fixtures
and session script must be current.

Run: python tools/web/test_judge.py
"""

from __future__ import annotations

import http.client
import json
import socket
import ssl
import sys
import tempfile
import unittest
from pathlib import Path

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))
import certs  # noqa: E402
import gen_fixtures  # noqa: E402
import imgenc  # noqa: E402
import judge  # noqa: E402
import session  # noqa: E402
import sites  # noqa: E402
from sites import Record, Request  # noqa: E402

GOOD_SERIAL = """\
WEBH:started
WEBH:http:PASS
WEB:UP:PASS
WEB:LOAD:http://example.com/
WEB:TITLE:Example Domain
WEB:LOAD:https://theoldnet.com/
WEB:TITLE:TheOldNet.com - Surf the Old Web
"""


def good_record() -> Record:
    record = Record(handshakes=["theoldnet.com"])
    record.requests.append(Request("http", "example.com", "GET", "/", 200, "LazyWeb"))
    record.requests.append(Request("http", "theoldnet.com", "GET", "/", 301, "LazyWeb"))
    for path in ["/", *judge.page_assets()]:
        record.requests.append(Request("https", "theoldnet.com", "GET", path, 200, "LazyWeb",
                                       "theoldnet.com"))
    return record


FEATURE_SERIAL = f"""\
OPEN:x-scheme-handler/http:os.lazy.lazyweb
WEB:DOWNLOAD:START:{sites.DOWNLOAD_NAME}
WEB:DOWNLOAD:DONE:{sites.DOWNLOAD_NAME}:{len(sites.download_payload())}
WEB:LAUNCH:{judge.MAILTO_URL}:FAIL
WEB:LOAD:about:history
WEB:LOAD:about:downloads
"""


def feature_record() -> Record:
    record = good_record()
    record.requests.append(Request("https", "theoldnet.com", "GET", sites.DOWNLOAD_PATH, 200,
                                   "LazyWeb", "theoldnet.com"))
    return record


class FeatureTests(unittest.TestCase):
    def test_a_good_run_passes(self) -> None:
        self.assertEqual(judge.judge_features(FEATURE_SERIAL, feature_record()), [])

    def test_each_missing_marker_fails(self) -> None:
        for line in FEATURE_SERIAL.splitlines():
            text = FEATURE_SERIAL.replace(line + "\n", "")
            self.assertTrue(judge.judge_features(text, feature_record()), f"passed without {line}")

    def test_a_short_download_fails(self) -> None:
        text = FEATURE_SERIAL.replace(f":{len(sites.download_payload())}", ":1024")
        self.assertTrue(judge.judge_features(text, feature_record()))

    def test_a_renamed_download_passes(self) -> None:
        text = FEATURE_SERIAL.replace(sites.DOWNLOAD_NAME, "oldnet-kit (2).zip")
        self.assertEqual(judge.judge_features(text, feature_record()), [])
        text = FEATURE_SERIAL.replace(sites.DOWNLOAD_NAME, "oldnet-kit.zip.exe")
        self.assertTrue(judge.judge_features(text, feature_record()))

    def test_another_app_for_http_fails(self) -> None:
        text = FEATURE_SERIAL.replace(":os.lazy.lazyweb", ":os.lazy.editor")
        self.assertTrue(judge.judge_features(text, feature_record()))

    def test_a_download_the_server_never_sent_fails(self) -> None:
        self.assertTrue(judge.judge_features(FEATURE_SERIAL, good_record()))
        headers_only = good_record()
        headers_only.requests.append(Request("https", "theoldnet.com", "HEAD", sites.DOWNLOAD_PATH,
                                             200, "LazyWeb", "theoldnet.com"))
        self.assertTrue(judge.judge_features(FEATURE_SERIAL, headers_only))

    def test_the_session_drives_every_feature(self) -> None:
        steps = json.dumps(session.script())
        for marker in ("WEB:DOWNLOAD:DONE:", "WEB:LAUNCH:", "about:history", "about:downloads",
                       "open.rhai"):
            self.assertIn(marker, steps)
        self.assertNotIn("WEB:DOWNLOAD", json.dumps(session.script(live=True)))


class SerialTests(unittest.TestCase):
    def test_a_good_run_passes(self) -> None:
        self.assertEqual(judge.judge_serial(GOOD_SERIAL), [])

    def test_each_missing_marker_fails(self) -> None:
        for line in GOOD_SERIAL.splitlines()[2:]:
            text = GOOD_SERIAL.replace(line + "\n", "")
            self.assertTrue(judge.judge_serial(text), f"passed without {line}")

    def test_a_failure_marker_fails(self) -> None:
        problems = judge.judge_serial(GOOD_SERIAL + "WEB:FAIL:tls: unknown issuer\n")
        self.assertIn("the browser failed: tls: unknown issuer", problems)

    def test_the_wrong_title_fails(self) -> None:
        text = GOOD_SERIAL.replace("TheOldNet.com - Surf the Old Web", "Site Maintenance")
        self.assertTrue(judge.judge_serial(text))

    def test_live_takes_any_other_title_but_not_none(self) -> None:
        text = GOOD_SERIAL.replace("TheOldNet.com - Surf the Old Web", "Site Maintenance")
        self.assertEqual(judge.judge_serial(text, None), [])
        without = GOOD_SERIAL.replace("WEB:TITLE:TheOldNet.com - Surf the Old Web\n", "")
        self.assertTrue(judge.judge_serial(without, None))

    def test_a_url_without_its_trailing_slash_counts(self) -> None:
        text = GOOD_SERIAL.replace("WEB:LOAD:http://example.com/", "WEB:LOAD:http://example.com")
        self.assertEqual(judge.judge_serial(text), [])

    def test_prechecks_pass_fail_and_go_missing(self) -> None:
        self.assertEqual(judge.judge_prechecks("TERM:OUT:WEBH:http:PASS\n", ["http"]), [])
        self.assertTrue(judge.judge_prechecks("WEBH:http:FAIL\n", ["http"]))
        self.assertTrue(judge.judge_prechecks("WEBH:http:PASS\n", ["http", "https"]))
        # The last report wins: a pass followed by a failure is a failure.
        self.assertTrue(judge.judge_prechecks("WEBH:png:PASS\nWEBH:png:FAIL\n", ["png"]))


class ServerTests(unittest.TestCase):
    def test_a_good_record_passes(self) -> None:
        self.assertEqual(judge.judge_servers(good_record(), judge.page_assets()), [])

    def test_every_missing_picture_fails(self) -> None:
        for path in judge.page_assets():
            record = good_record()
            record.requests = [r for r in record.requests if r.path != path]
            problems = judge.judge_servers(record, judge.page_assets())
            self.assertIn(f"the browser never fetched {path} from theoldnet.com", problems)

    def test_the_checks_requests_do_not_count_for_the_browser(self) -> None:
        record = good_record()
        for r in record.requests:
            r.path += "?precheck"
        self.assertGreaterEqual(len(judge.judge_servers(record, judge.page_assets())), 3)

    def test_a_wrong_or_missing_sni_fails(self) -> None:
        record = good_record()
        record.requests[-1].sni = "example.com"
        self.assertTrue(judge.judge_servers(record, judge.page_assets()))
        record = good_record()
        record.requests[-1].sni = None
        self.assertTrue(judge.judge_servers(record, judge.page_assets()))
        record = good_record()
        record.handshakes = [None]
        self.assertTrue(judge.judge_servers(record, judge.page_assets()))

    def test_a_missing_host_header_or_plain_http_page_fails(self) -> None:
        record = good_record()
        record.requests[0].host = ""
        self.assertTrue(judge.judge_servers(record, judge.page_assets()))
        record = good_record()
        for r in record.requests:
            r.scheme = "http"
        self.assertTrue(judge.judge_servers(record, judge.page_assets()))

    def test_precheck_requests(self) -> None:
        record = Record()
        for scheme, host, path, status in judge.PRECHECK_REQUESTS:
            record.requests.append(Request(scheme, host, "GET", path + "?precheck", status, "fetch",
                                           host if scheme == "https" else None))
        self.assertEqual(judge.judge_precheck_servers(record), [])
        record.requests.pop()
        self.assertTrue(judge.judge_precheck_servers(record))
        self.assertTrue(judge.judge_precheck_servers(Record()))


class ShotTests(unittest.TestCase):
    def setUp(self) -> None:
        tmp = tempfile.TemporaryDirectory()
        self.addCleanup(tmp.cleanup)
        self.dir = Path(tmp.name)

    def shot(self, name: str, colourful: bool, black: bool = False) -> Path:
        size = 64
        pixels = []
        for y in range(size):
            for x in range(size):
                if black:
                    pixels.append((0, 0, 0))
                elif colourful:
                    pixels.append(((x * 4) % 256, (y * 4) % 256, ((x + y) * 2) % 256))
                else:
                    pixels.append((238, 238, 238) if y > 8 else (50, 70, 140))
        path = self.dir / name
        path.write_bytes(imgenc.png(size, size, pixels))
        return path

    def test_two_different_pages_pass(self) -> None:
        shots = [self.shot("a.png", False), self.shot("b.png", True)]
        self.assertEqual(judge.judge_shots(shots, min_colors=2), [])

    def test_a_black_screen_fails(self) -> None:
        shots = [self.shot("a.png", False, black=True), self.shot("b.png", True)]
        self.assertTrue(judge.judge_shots(shots, min_colors=2))

    def test_an_unchanged_screen_fails(self) -> None:
        shots = [self.shot("a.png", True), self.shot("b.png", True)]
        self.assertIn("the first and last screenshots are identical: the page never changed",
                      judge.judge_shots(shots, min_colors=2))

    def test_a_dull_retro_page_or_a_missing_shot_fails(self) -> None:
        self.assertTrue(judge.judge_shots([self.shot("a.png", True), self.shot("b.png", False)],
                                          min_colors=2))
        self.assertTrue(judge.judge_shots([self.shot("a.png", True)]))
        self.assertTrue(judge.judge_shots([self.shot("a.png", True), self.dir / "missing.png"]))


class FixtureTests(unittest.TestCase):
    def test_the_pictures_are_current(self) -> None:
        self.assertEqual(gen_fixtures.main(["--check"]), 0)

    def test_the_page_uses_every_format(self) -> None:
        assets = judge.page_assets()
        for suffix in (".png", ".jpg", ".gif", ".css"):
            self.assertTrue([a for a in assets if a.endswith(suffix)], suffix)
        for asset in assets:
            self.assertTrue((judge.FIXTURES / "theoldnet.com" / asset.lstrip("/")).is_file(), asset)

    def test_titles(self) -> None:
        self.assertEqual(judge.EXAMPLE_TITLE, "Example Domain")
        self.assertEqual(judge.fixture_title("example.com"), "Example Domain")
        self.assertIn("TheOldNet", judge.OLDNET_TITLE)


class SessionTests(unittest.TestCase):
    def test_the_example_script_is_current(self) -> None:
        self.assertEqual(session.EXAMPLE.read_text(encoding="utf-8"), session.render(session.script()))
        json.loads(session.EXAMPLE.read_text(encoding="utf-8"))

    def test_every_check_is_tagged_and_judged(self) -> None:
        urls = [f"{scheme}://{host}{path}" for scheme, host, path, _ in judge.PRECHECK_REQUESTS]
        commands = [command for _, command in session.checks()]
        for url in urls:
            self.assertTrue([c for c in commands if url + "?precheck" in c or url + "?precheck" in
                             c.replace("/?precheck", "/?precheck")], url)
        for command in commands:
            if "curl" in command:
                self.assertIn("?precheck", command)

    def test_the_typed_lines_stay_short(self) -> None:
        # The Terminal reports a wrapped (over 80 columns) command by its tail.
        for console in (True, False):
            self.assertLess(len(session.bootstrap(console)), 120)
        typed = [s["type"] for s in session.script() if "type" in s]
        self.assertTrue(all(len(line) < 80 for line in typed[1:]), typed)

    def test_the_console_run_has_no_browser(self) -> None:
        steps = session.script(console=True)
        self.assertFalse([s for s in steps if "WEB:UP" in json.dumps(s)])
        self.assertIn("WEB:UP:PASS", json.dumps(session.script()))


def _free_port() -> int:
    with socket.socket() as probe:
        probe.bind(("127.0.0.1", 0))
        return probe.getsockname()[1]


class SitesTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls) -> None:
        cls.tmp = tempfile.TemporaryDirectory()
        cls.files = certs.generate(Path(cls.tmp.name))
        cls.http, cls.https = _free_port(), _free_port()
        cls.sites = sites.Sites(cls.files.leaf_cert, cls.files.leaf_key,
                                script=("/check.sh", b"echo hi\n"),
                                http_port=cls.http, https_port=cls.https)

    @classmethod
    def tearDownClass(cls) -> None:
        cls.sites.close()
        cls.tmp.cleanup()

    def get(self, host: str, path: str, tls: bool = False) -> http.client.HTTPResponse:
        if tls:
            context = ssl.create_default_context(cafile=str(self.files.ca))
            conn = http.client.HTTPSConnection("127.0.0.1", self.https, context=context)
            # Verify and send SNI for the site's name, as the guest does.
            conn.sock = context.wrap_socket(socket.create_connection(("127.0.0.1", self.https)),
                                            server_hostname=host)
        else:
            conn = http.client.HTTPConnection("127.0.0.1", self.http)
        conn.request("GET", path, headers={"Host": host})
        response = conn.getresponse()
        response.body = response.read()
        conn.close()
        return response

    def test_example_com_is_the_copy(self) -> None:
        response = self.get("example.com", "/")
        self.assertEqual(response.status, 200)
        self.assertIn(b"<title>Example Domain</title>", response.body)
        self.assertIn(b"https://iana.org/domains/example", response.body)

    def test_theoldnet_redirects_plain_http(self) -> None:
        response = self.get("theoldnet.com", "/about.html")
        self.assertEqual(response.status, 301)
        self.assertEqual(response.getheader("Location"), "https://theoldnet.com/about.html")

    def test_theoldnet_over_tls_records_sni(self) -> None:
        for host in ("theoldnet.com", "www.theoldnet.com"):
            response = self.get(host, "/images/photo.jpg", tls=True)
            self.assertEqual(response.status, 200)
            self.assertEqual(response.getheader("Content-Type"), "image/jpeg")
        seen = [r for r in self.sites.snapshot().requests if r.path == "/images/photo.jpg"]
        self.assertEqual({(r.host, r.sni) for r in seen},
                         {("theoldnet.com", "theoldnet.com"), ("www.theoldnet.com", "www.theoldnet.com")})

    def test_nothing_outside_the_site_and_no_cross_scheme_site(self) -> None:
        self.assertEqual(self.get("theoldnet.com", "/../example.com/index.html", tls=True).status, 404)
        self.assertEqual(self.get("example.com", "/../theoldnet.com/style.css").status, 404)
        # No certificate names example.com: a verifying client refuses it.
        with self.assertRaises(ssl.SSLCertVerificationError):
            self.get("example.com", "/", tls=True)

    def test_the_download_is_an_attachment(self) -> None:
        response = self.get("theoldnet.com", sites.DOWNLOAD_PATH, tls=True)
        self.assertEqual(response.status, 200)
        self.assertEqual(response.body, sites.download_payload())
        self.assertIn("attachment", response.getheader("Content-Disposition", ""))

    def test_the_check_script_is_served_to_any_host(self) -> None:
        response = self.get("10.0.2.2", "/check.sh?precheck")
        self.assertEqual((response.status, response.body), (200, b"echo hi\n"))


if __name__ == "__main__":
    unittest.main()
