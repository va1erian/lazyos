"""push.py against a fake registry that records requests, and once against the real one."""

import contextlib
import hashlib
import io
import json
import os
import tempfile
import threading
import unittest
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path
from unittest import mock

import auth
import ed25519
import fixtures
import keys
import push
from fixtures import build_id


class FakeRegistry:
    """Answers the calls push.py makes and writes down what it was sent."""

    def __init__(self, listing=None, fail_on=None):
        self.requests = []
        self.listing = listing if listing is not None else {"channels": []}
        self.fail_on = fail_on or {}          # (method, path suffix) -> (status, body dict)
        outer = self

        class Handler(BaseHTTPRequestHandler):
            protocol_version = "HTTP/1.1"

            def log_message(self, *_):
                pass

            def _do(self):
                length = int(self.headers.get("Content-Length") or 0)
                body = self.rfile.read(length)
                outer.requests.append((self.command, self.path, dict(self.headers.items()),
                                       body if self.command != "PUT" else len(body)))
                for (method, suffix), (status, doc) in outer.fail_on.items():
                    if method == self.command and self.path.endswith(suffix):
                        return self._reply(status, doc)
                if self.command == "GET":
                    return self._reply(200, outer.listing)
                self._reply(201 if self.command == "PUT" else 200, {"ok": True, "deleted": [], "kept": [], "callback_key": "cd" * 32,
                                              "mac": "aa:bb:cc:dd:ee:ff", "channel": "dev"})

            def _reply(self, status, doc):
                data = json.dumps(doc).encode()
                self.send_response(status)
                self.send_header("Content-Length", str(len(data)))
                self.end_headers()
                self.wfile.write(data)

            do_GET = do_PUT = do_POST = do_DELETE = _do

        self.server = ThreadingHTTPServer(("127.0.0.1", 0), Handler)
        self.server.daemon_threads = True
        self.url = f"http://127.0.0.1:{self.server.server_address[1]}"
        threading.Thread(target=lambda: self.server.serve_forever(poll_interval=0.02), daemon=True).start()

    def close(self):
        self.server.shutdown()
        self.server.server_close()

    def kinds(self):
        return [(m, p.split("?")[0]) for m, p, *_ in self.requests]


class PushCase(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory(ignore_cleanup_errors=True)
        self.addCleanup(self.tmp.cleanup)
        self.dir = Path(self.tmp.name) / "netboot"
        self.seed = ed25519.generate_seed()
        self.pub = ed25519.public_key(self.seed)
        keys.write_keypair(self.dir, self.seed)
        self.bid = build_id(1)
        self.build = fixtures.make_build(self.dir, self.seed, self.bid)

    def fake(self, **kw):
        reg = FakeRegistry(**kw)
        self.addCleanup(reg.close)
        return reg

    def run_main(self, reg, *extra):
        out, err = io.StringIO(), io.StringIO()
        with contextlib.redirect_stdout(out), contextlib.redirect_stderr(err):
            code = push.main(["--registry", reg.url, "--dir", str(self.dir), *extra])
        return code, out.getvalue(), err.getvalue()

    def verify_signed(self, request, action):
        _, _, headers, body = request
        key, sig = headers[auth.SIGNATURE_HEADER].split(":")
        self.assertEqual(key, keys.key_id(self.pub))
        self.assertTrue(ed25519.verify(self.pub, body, bytes.fromhex(sig)))
        doc = json.loads(body)
        self.assertEqual(doc["action"], action)
        self.assertRegex(doc["nonce"], "^[0-9a-f]{32}$")
        self.assertIsInstance(doc["ts"], int)
        return doc


class PushFlowTests(PushCase):
    def test_upload_order_and_signed_commit(self):
        reg = self.fake()
        code, out, _ = self.run_main(reg)
        self.assertEqual(code, 0, out)
        kinds = reg.kinds()
        base = f"/api/builds/dev/{self.bid}"
        self.assertEqual(kinds[0], ("GET", "/api/builds"))
        puts = [k[1].rsplit("/", 1)[1] for k in kinds if k[0] == "PUT"]
        self.assertEqual(puts[:2], ["manifest.json", "manifest.sig"])    # the signed manifest opens the upload
        self.assertEqual(sorted(puts[2:]), sorted(self.build["files"]))
        self.assertEqual(kinds[-1], ("POST", f"{base}/commit"))
        doc = self.verify_signed(reg.requests[-1], "commit")
        self.assertEqual((doc["channel"], doc["id"]), ("dev", self.bid))
        self.assertIn(f"pushed {self.bid}", out)
        put_sizes = {p.rsplit("/", 1)[1]: r[3] for r in reg.requests for m, p, *_ in [r] if m == "PUT"}
        self.assertEqual(put_sizes["ramdisk"], len(self.build["files"]["ramdisk"]))

    def test_machine_sets_next_after_the_commit(self):
        reg = self.fake()
        self.assertEqual(self.run_main(reg, "--machine", "box", "--channel", "box")[0], 0)
        self.assertEqual(reg.kinds()[-2:], [("POST", f"/api/builds/box/{self.bid}/commit"),
                                            ("POST", "/api/machines/box/next")])
        doc = self.verify_signed(reg.requests[-1], "next")
        self.assertEqual((doc["id"], doc["machine"]), (self.bid, "box"))

    def test_id_with_machine_pushes_nothing(self):
        reg = self.fake()
        old = build_id(0)
        code, out, _ = self.run_main(reg, "--machine", "box", "--id", old)
        self.assertEqual(code, 0, out)
        self.assertEqual(reg.kinds(), [("POST", "/api/machines/box/next")])
        self.assertEqual(self.verify_signed(reg.requests[0], "next")["id"], old)

    def test_id_without_machine_is_an_error(self):
        code, _, err = self.run_main(self.fake(), "--id", build_id(0))
        self.assertEqual(code, 1)
        self.assertIn("--machine", err)

    def test_already_pushed_uploads_nothing(self):
        listing = {"channels": [{"name": "dev", "builds": [
            {"id": self.bid, "channel": "dev", "manifest_text": self.build["manifest"].decode()}]}]}
        reg = self.fake(listing=listing)
        code, out, _ = self.run_main(reg)
        self.assertEqual(code, 0)
        self.assertEqual(reg.kinds(), [("GET", "/api/builds")])
        self.assertIn("already", out)

    def test_same_id_with_other_contents_is_refused_before_uploading(self):
        listing = {"channels": [{"name": "dev", "builds": [
            {"id": self.bid, "channel": "dev", "manifest_text": "{}"}]}]}
        reg = self.fake(listing=listing)
        code, _, err = self.run_main(reg)
        self.assertEqual(code, 1)
        self.assertIn("never overwritten", err)
        self.assertEqual(reg.kinds(), [("GET", "/api/builds")])

    def test_registry_refusal_is_reported(self):
        reg = self.fake(fail_on={("POST", "/commit"): (403, {"error": "does not verify against any key"})})
        code, _, err = self.run_main(reg)
        self.assertEqual(code, 1)
        self.assertIn("does not verify against any key", err)

    def test_unreachable_registry(self):
        reg = self.fake()
        reg.close()
        code, _, err = self.run_main(reg)
        self.assertEqual(code, 1)
        self.assertIn("cannot reach", err)


class LocalCheckTests(PushCase):
    def refused(self, fragment):
        reg = self.fake()
        code, _, err = self.run_main(reg)
        self.assertEqual(code, 1)
        self.assertIn(fragment, err)
        self.assertEqual(reg.requests, [], "nothing may be sent for a build the registry would refuse")

    def test_tampered_file(self):
        data = bytearray(self.build["files"]["ramdisk"])
        data[7] ^= 1
        (self.dir / "ramdisk").write_bytes(bytes(data))
        self.refused("ramdisk")

    def test_wrong_size(self):
        (self.dir / "kernel-x86_64").write_bytes(b"short")
        self.refused("kernel-x86_64")

    def test_missing_file(self):
        (self.dir / "boot.json").unlink()
        self.refused("boot.json")

    def test_unsigned_build(self):
        (self.dir / "manifest.sig").unlink()
        self.refused("not signed")

    def test_signature_of_another_key(self):
        other = ed25519.generate_seed()
        raw = (self.dir / "manifest.json").read_bytes()
        (self.dir / "manifest.sig").write_text(keys.format_signature(ed25519.sign(other, raw)))
        self.refused("does not verify")

    def test_manifest_edited_after_signing(self):
        raw = (self.dir / "manifest.json").read_bytes()
        (self.dir / "manifest.json").write_bytes(raw.replace(b"devbox", b"evilbx"))
        self.refused("does not verify")

    def test_malformed_signature_file(self):
        (self.dir / "manifest.sig").write_text("abc\n")
        self.refused("128 hex")

    def test_no_manifest_and_bad_id(self):
        (self.dir / "manifest.json").unlink()
        self.refused("manifest.json")

    def test_missing_secret_key(self):
        (self.dir / "key.sec").unlink()
        self.refused("key.sec")


class ManageTests(PushCase):
    def test_pin_unpin_prune_delete_are_signed(self):
        reg = self.fake()
        for args, expect in ((["--pin", self.bid], ("POST", "/api/channels/dev/pin", "pin")),
                             (["--unpin", self.bid], ("POST", "/api/channels/dev/unpin", "unpin")),
                             (["--prune", "--keep", "3"], ("POST", "/api/channels/dev/prune", "prune")),
                             (["--delete", self.bid], ("DELETE", f"/api/builds/dev/{self.bid}", "delete"))):
            self.assertEqual(self.run_main(reg, *args)[0], 0, args)
            request = reg.requests[-1]
            self.assertEqual(request[:2], expect[:2])
            doc = self.verify_signed(request, expect[2])
            self.assertEqual(doc["channel"], "dev")
        self.assertEqual(doc["id"], self.bid)
        prune = json.loads(reg.requests[2][3])
        self.assertEqual(prune["keep"], 3)

    def test_list_does_not_need_the_key(self):
        (self.dir / "key.sec").unlink()
        reg = self.fake(listing={"channels": [{"name": "dev", "keep": 10, "pinned": [], "builds": [
            {"id": self.bid, "size": 3 << 20, "pinned": True}]}]})
        code, out, _ = self.run_main(reg, "--list")
        self.assertEqual(code, 0)
        self.assertIn(self.bid, out)
        self.assertIn("pinned", out)

    def test_register_key_prints_the_keys_json_entry(self):
        code, out, _ = self.run_main(self.fake(), "--register-key", "--name", "my laptop")
        self.assertEqual(code, 0)
        entry = json.loads(out.splitlines()[0])
        self.assertEqual(entry["pub"], self.pub.hex())
        self.assertEqual(entry["id"], hashlib.sha256(self.pub).hexdigest()[:16])
        self.assertEqual(entry["name"], "my laptop")
        keys_json = Path(self.tmp.name) / "keys.json"
        keys_json.write_text(json.dumps({"keys": [entry]}))
        self.assertEqual(keys.load_keys(keys_json), {entry["id"]: self.pub})

    def test_register_key_falls_back_to_the_secret(self):
        (self.dir / "key.pub").unlink()
        entry = json.loads(self.run_main(self.fake(), "--register-key")[1].splitlines()[0])
        self.assertEqual(entry["pub"], self.pub.hex())


class MachineCommandTests(PushCase):
    def test_add_machine_prints_the_key_once_with_the_cfg_hint(self):
        reg = self.fake()
        code, out, _ = self.run_main(reg, "--add-machine", "box", "--mac", "AA-BB-CC-DD-EE-FF", "--channel", "box")
        self.assertEqual(code, 0, out)
        self.assertEqual(reg.kinds(), [("POST", "/api/machines/box/add")])
        doc = self.verify_signed(reg.requests[0], "add")
        self.assertEqual((doc["machine"], doc["mac"], doc["channel"]), ("box", "AA-BB-CC-DD-EE-FF", "box"))
        self.assertIn("bootos.machine_key=", out)
        self.assertIn("shown once", out)

    def test_add_machine_needs_a_mac(self):
        code, _, err = self.run_main(self.fake(), "--add-machine", "box")
        self.assertEqual(code, 1)
        self.assertIn("--mac", err)

    def test_rekey(self):
        reg = self.fake()
        self.assertEqual(self.run_main(reg, "--rekey-machine", "box")[0], 0)
        self.assertEqual(self.verify_signed(reg.requests[0], "rekey")["machine"], "box")

    def test_booting_with_the_dev_key_and_with_a_machine_key(self):
        reg = self.fake()
        self.assertEqual(self.run_main(reg, "--machine", "box", "--booting", self.bid)[0], 0)
        doc = self.verify_signed(reg.requests[0], "booting")
        self.assertEqual((doc["machine"], doc["id"]), ("box", self.bid))
        (self.dir / "key.sec").unlink()   # a machine key needs no dev key
        key = "ab" * 32
        self.assertEqual(self.run_main(reg, "--machine", "box", "--booted", self.bid, "--machine-key", key)[0], 0)
        _, path, headers, body = reg.requests[1]
        self.assertEqual(path, "/api/machines/box/booted")
        name, mac = headers[auth.MACHINE_HEADER].split(":")
        self.assertEqual(name, "box")
        import hmac
        self.assertEqual(mac, hmac.new(bytes.fromhex(key), body, hashlib.sha256).hexdigest())
        self.assertNotIn(auth.SIGNATURE_HEADER, headers)
        doc = json.loads(body)
        self.assertEqual((doc["action"], doc["machine"], doc["id"]), ("booted", "box", self.bid))

    def test_booting_needs_a_machine(self):
        code, _, err = self.run_main(self.fake(), "--booting", self.bid)
        self.assertEqual(code, 1)
        self.assertIn("--machine", err)


class RegistryUrlTests(unittest.TestCase):
    def test_precedence_and_error(self):
        with mock.patch.dict(os.environ, {"LAZYOS_REGISTRY": "http://env:1"}):
            self.assertEqual(push.registry_url("http://arg:2"), "http://arg:2")
            self.assertEqual(push.registry_url(None), "http://env:1")
        with tempfile.TemporaryDirectory() as tmp, mock.patch.dict(os.environ, {}, clear=False):
            os.environ.pop("LAZYOS_REGISTRY", None)
            config = Path(tmp) / "registry.local.json"
            with mock.patch.object(push, "LOCAL_CONFIG", config):
                with self.assertRaises(push.PushError):
                    push.registry_url(None)
                config.write_text('{"registry": "http://file:3"}')
                self.assertEqual(push.registry_url(None), "http://file:3")


class RealRegistryTests(PushCase):
    def setUp(self):
        super().setUp()
        self.h = fixtures.Harness()
        self.addCleanup(self.h.close)
        # register this test's key, as an administrator would
        entry = keys.keys_entry(self.pub, "push test")
        (self.h.root / "keys.json").write_text(json.dumps({"keys": [entry]}))

    def test_push_then_read_back_then_next(self):
        (self.h.root / "machines.json").write_text(json.dumps({"box": {"mac": "aa:bb:cc:dd:ee:ff"}}))
        code, out, err = self.run_main(type("R", (), {"url": self.h.url}), "--machine", "box")
        self.assertEqual(code, 0, err)
        status, listing = self.h.json("GET", "/api/builds?mac=aa:bb:cc:dd:ee:ff")
        self.assertEqual(listing["default"]["id"], self.bid)
        self.assertEqual(listing["next"]["id"], self.bid)
        status, _, data = self.h.request("GET", f"/files/dev/{self.bid}/ramdisk")
        self.assertEqual(data, self.build["files"]["ramdisk"])
        # pushing it again is a no-op; list shows it
        code, out, _ = self.run_main(type("R", (), {"url": self.h.url}))
        self.assertIn("already", out)
        code, out, _ = self.run_main(type("R", (), {"url": self.h.url}), "--list")
        self.assertIn(self.bid, out)

    def test_add_machine_then_hmac_callbacks_against_the_real_registry(self):
        reg = type("R", (), {"url": self.h.url})
        code, out, err = self.run_main(reg, "--add-machine", "box", "--mac", "aa:bb:cc:dd:ee:ff")
        self.assertEqual(code, 0, err)
        key = next(l.split(": ")[1] for l in out.splitlines() if l.startswith("callback key"))
        self.assertEqual(self.run_main(reg)[0], 0)                       # push the build
        os.environ.pop("LAZYOS_MACHINE_KEY", None)
        args = ["--machine", "box", "--machine-key", key]
        self.assertEqual(self.run_main(reg, *args, "--booting", self.bid)[0], 0)
        self.assertEqual(self.run_main(reg, *args, "--booted", self.bid)[0], 0)
        machine = json.loads((self.h.root / "machines.json").read_text())["box"]
        self.assertEqual(machine["last_boot"]["id"], self.bid)
        code, _, err = self.run_main(reg, "--machine", "box", "--machine-key", "00" * 32, "--booting", self.bid)
        self.assertEqual(code, 1)
        self.assertIn("does not verify", err)

    def test_a_key_the_registry_does_not_know_is_refused_and_leaves_no_build(self):
        (self.h.root / "keys.json").write_text(json.dumps({"keys": []}))
        code, _, err = self.run_main(type("R", (), {"url": self.h.url}))
        self.assertEqual(code, 1)
        self.assertEqual(self.h.server.store.build_ids("dev"), [])


if __name__ == "__main__":
    unittest.main()
