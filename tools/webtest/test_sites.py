#!/usr/bin/env python3
"""Tests of the browser harnesses' shared stand-ins: the pages the fixtures
describe, the checked-in pictures, and the sites the host serves (which must
answer as the real ones do and record what the judges read).

Run: python tools/webtest/test_sites.py
"""

from __future__ import annotations

import http.client
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
import pages  # noqa: E402
import sites  # noqa: E402


class FixtureTests(unittest.TestCase):
    def test_the_pictures_are_current(self) -> None:
        self.assertEqual(gen_fixtures.main(["--check"]), 0)

    def test_the_page_uses_every_format(self) -> None:
        assets = pages.page_assets()
        for suffix in (".png", ".jpg", ".gif", ".css"):
            self.assertTrue([a for a in assets if a.endswith(suffix)], suffix)
        for asset in assets:
            self.assertTrue((pages.FIXTURES / "theoldnet.com" / asset.lstrip("/")).is_file(), asset)

    def test_titles(self) -> None:
        self.assertEqual(pages.EXAMPLE_TITLE, "Example Domain")
        self.assertEqual(pages.fixture_title("example.com"), "Example Domain")
        self.assertIn("TheOldNet", pages.OLDNET_TITLE)


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


if __name__ == "__main__":
    unittest.main()
