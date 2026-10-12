"""serve.py: Range parsing, the route table, and a loopback round trip."""

import http.client
import json
import tempfile
import threading
import unittest
from pathlib import Path

import common
import scripts
import serve
import testlib


class RangeTests(unittest.TestCase):
    def test_no_header_or_unsupported_forms_mean_the_whole_file(self):
        for header in (None, "", "items=0-1", "bytes=0-1,5-6", "bytes=-", "garbage", "bytes=5-2"):
            self.assertIsNone(serve.parse_range(header, 100), header)

    def test_forms(self):
        self.assertEqual(serve.parse_range("bytes=0-9", 100), (0, 9))
        self.assertEqual(serve.parse_range("bytes=90-", 100), (90, 99))
        self.assertEqual(serve.parse_range("bytes=90-500", 100), (90, 99))
        self.assertEqual(serve.parse_range("bytes=-10", 100), (90, 99))
        self.assertEqual(serve.parse_range("bytes=-500", 100), (0, 99))
        self.assertEqual(serve.parse_range("bytes=99-99", 100), (99, 99))

    def test_unsatisfiable(self):
        for header in ("bytes=100-", "bytes=100-120", "bytes=-0"):
            with self.assertRaises(serve.RangeError, msg=header):
                serve.parse_range(header, 100)
        with self.assertRaises(serve.RangeError):
            serve.parse_range("bytes=0-1", 0)


class NameTests(unittest.TestCase):
    def test_served_file_refuses_anything_but_a_plain_name(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            (root / "ramdisk").write_bytes(b"x")
            (root / "sub").mkdir()
            self.assertEqual(serve.served_file(root, "ramdisk"), root / "ramdisk")
            for bad in ("", "..", "../x", "sub", "sub/x", "a\\b", ".hidden", "ramdisk\0", "missing"):
                self.assertIsNone(serve.served_file(root, bad), repr(bad))

    def test_label_uses_the_manifest(self):
        manifest = [{"name": "kernel-x86_64", "size": 5, "sha256": "ab" * 32},
                    {"name": "ramdisk", "size": 3 << 20, "sha256": "cd" * 32}]
        label = serve.build_label(manifest)
        self.assertIn("abababab", label)
        self.assertIn("cdcdcdcd", label)
        self.assertIn("3.0 MiB", label)
        self.assertEqual(serve.build_label([]), "spike build")


class ServerTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.tmp = tempfile.TemporaryDirectory()
        cls.root = Path(cls.tmp.name)
        cls.contents = testlib.make_payload(cls.root, ramdisk_bytes=300_000)
        cls.log = []
        cls.server = serve.serve(cls.root, "127.0.0.1:0", "10.0.2.2", 5000, cls.log.append)
        cls.port = cls.server.server_address[1]
        cls.thread = threading.Thread(target=cls.server.serve_forever, daemon=True)
        cls.thread.start()

    @classmethod
    def tearDownClass(cls):
        cls.server.shutdown()
        cls.server.server_close()
        cls.tmp.cleanup()

    def get(self, path, method="GET", headers=None):
        conn = http.client.HTTPConnection("127.0.0.1", self.port, timeout=10)
        conn.request(method, path, headers=headers or {})
        response = conn.getresponse()
        body = response.read()
        conn.close()
        return response, body

    def test_menu_uses_the_guest_host_not_the_bind_address(self):
        response, body = self.get("/boot.ipxe?mac=52:54:00:12:34:56")
        text = body.decode()
        self.assertEqual(response.status, 200)
        self.assertEqual(int(response.getheader("Content-Length")), len(body))
        self.assertTrue(text.startswith("#!ipxe"))
        self.assertIn(f"set reg http://10.0.2.2:{self.port}", text)
        self.assertNotIn("127.0.0.1", text)
        self.assertIn("--timeout 5000", text)

    def test_build_script(self):
        _, body = self.get("/build.ipxe")
        self.assertEqual(body.decode(), scripts.build_script(f"http://10.0.2.2:{self.port}"))

    def test_sanboot_script(self):
        _, body = self.get("/sanboot.ipxe")
        self.assertIn(f"sanboot --no-describe http://10.0.2.2:{self.port}/files/esp.img", body.decode())

    def test_full_file(self):
        response, body = self.get("/files/ramdisk")
        self.assertEqual(response.status, 200)
        self.assertEqual(body, self.contents["ramdisk"])
        self.assertEqual(response.getheader("Accept-Ranges"), "bytes")
        self.assertEqual(int(response.getheader("Content-Length")), len(body))

    def test_range_requests(self):
        data = self.contents["ramdisk"]
        response, body = self.get("/files/ramdisk", headers={"Range": "bytes=1000-1999"})
        self.assertEqual(response.status, 206)
        self.assertEqual(body, data[1000:2000])
        self.assertEqual(response.getheader("Content-Range"), f"bytes 1000-1999/{len(data)}")
        response, body = self.get("/files/ramdisk", headers={"Range": "bytes=-10"})
        self.assertEqual((response.status, body), (206, data[-10:]))
        response, body = self.get("/files/ramdisk", headers={"Range": f"bytes={len(data) - 5}-"})
        self.assertEqual((response.status, body), (206, data[-5:]))
        response, _ = self.get("/files/ramdisk", headers={"Range": f"bytes={len(data)}-"})
        self.assertEqual(response.status, 416)
        self.assertEqual(response.getheader("Content-Range"), f"bytes */{len(data)}")

    def test_head_has_length_and_no_body(self):
        response, body = self.get("/files/kernel-x86_64", method="HEAD")
        self.assertEqual(response.status, 200)
        self.assertEqual(int(response.getheader("Content-Length")), len(self.contents["kernel-x86_64"]))
        self.assertEqual(body, b"")

    def test_unknown_and_hostile_paths_are_404(self):
        for path in ("/nope", "/files/missing", "/files/../x", "/files/%2e%2e/x", "/files/", "/files/sub/x"):
            self.assertEqual(self.get(path)[0].status, 404, path)

    def test_every_request_is_logged_with_bytes_and_time(self):
        self.get("/files/boot.json")
        line = self.log[-1]
        self.assertIn("GET /files/boot.json 200", line)
        self.assertIn(f"{len(self.contents['boot.json'])} B", line)
        self.assertIn(" s", line)
        self.assertIn("127.0.0.1", line)

    def test_index_lists_the_files(self):
        _, body = self.get("/")
        for name in common.PAYLOAD_FILES:
            self.assertIn(f"/files/{name}", body.decode())

    def test_manifest_json_is_read_when_present(self):
        self.addCleanup((self.root / "manifest.json").unlink, missing_ok=True)
        (self.root / "manifest.json").write_text(json.dumps(
            [{"name": "kernel-x86_64", "size": 1, "sha256": "12" * 32},
             {"name": "ramdisk", "size": 1 << 20, "sha256": "34" * 32}]))
        _, body = self.get("/boot.ipxe")
        self.assertIn("12121212", body.decode())


if __name__ == "__main__":
    unittest.main()
