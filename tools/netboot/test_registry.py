"""The registry over a loopback server: pushes, refusals, signed writes, files and Range."""

import hashlib
import io
import json
import shutil
import socket
import time
import unittest
from pathlib import Path

import auth
import ed25519
import fixtures
import keys
from fixtures import build_id


class RegistryCase(unittest.TestCase):
    def setUp(self):
        self.h = fixtures.Harness()
        self.addCleanup(self.h.close)

    def builds_on_disk(self, channel="dev"):
        return self.h.server.store.build_ids(channel)

    def incoming(self):
        return sorted(p.name for p in (self.h.root / "builds").rglob("*.incoming"))


class FirstRunTests(unittest.TestCase):
    def test_empty_root_gets_its_files_and_a_log_line_about_keys(self):
        h = fixtures.Harness(register_key=False)
        self.addCleanup(h.close)
        for name in ("channels.json", "machines.json", "keys.json"):
            self.assertTrue((h.root / name).is_file(), name)
        self.assertEqual(json.loads((h.root / "keys.json").read_text()), {"keys": []})
        self.assertTrue(any("no key may push yet" in line for line in h.log), h.log)
        status, doc = h.json("GET", "/healthz")
        self.assertEqual((status, doc["ok"], doc["builds"]), (200, True, 0))

    def test_existing_files_are_not_overwritten(self):
        h = fixtures.Harness(machines={"box": {"mac": "aa:bb:cc:dd:ee:ff"}})
        self.addCleanup(h.close)
        self.assertIn("box", json.loads((h.root / "machines.json").read_text()))
        self.assertFalse(any("no key may push yet" in line for line in h.log))


class PushTests(RegistryCase):
    def test_push_and_read_back(self):
        bid = build_id(1)
        build, _, (status, doc) = self.h.push("b1", bid)
        self.assertEqual((status, doc["ok"], doc["already"]), (201, True, False))
        self.assertEqual(self.builds_on_disk(), [bid])
        self.assertEqual(self.incoming(), [])
        for name, data in build["files"].items():
            status, _, body = self.h.request("GET", f"/files/dev/{bid}/{name}")
            self.assertEqual((status, body), (200, data), name)
        status, listing = self.h.json("GET", "/api/builds")
        entry = listing["channels"][0]["builds"][0]
        self.assertEqual(entry["id"], bid)
        # the manifest is stored and shown as uploaded, byte for byte
        self.assertEqual(entry["manifest_text"].encode(), build["manifest"])
        self.assertEqual(entry["manifest"]["files"]["ramdisk"]["size"], len(build["files"]["ramdisk"]))
        self.assertEqual(len(entry["sig"]), 128)

    def test_files_not_in_the_manifest_json_are_not_reserialised(self):
        # unknown manifest fields are allowed and the bytes are kept as they came
        raw_extra = {"future": {"x": [1, 2, 3]}, "unicode": "café"}
        build = fixtures.make_build(self.h.tmp / "u", self.h.seed, build_id(2), extra=raw_extra)
        self.assertEqual(self.h.upload(self.h.tmp / "u", build)[0], 201)
        stored = (self.h.root / "builds" / "dev" / build["id"] / "manifest.json").read_bytes()
        self.assertEqual(stored, build["manifest"])

    def test_a_torn_upload_leaves_no_build(self):
        bid = build_id(3)
        build = fixtures.make_build(self.h.tmp / "t", self.h.seed, bid)
        # the client dies after the kernel: no ramdisk, no manifest
        self.h.upload(self.h.tmp / "t", build, commit=False, skip=("ramdisk", "manifest.json", "manifest.sig"))
        self.assertEqual(self.builds_on_disk(), [])
        status, listing = self.h.json("GET", "/api/builds")
        self.assertEqual(listing["channels"], [])
        # committing what is there refuses and cleans up
        status, doc = self.h.signed("POST", f"/api/builds/dev/{bid}/commit", "commit")
        self.assertEqual(status, 400)
        self.assertEqual((self.builds_on_disk(), self.incoming()), ([], []))

    def test_a_body_that_ends_early_stores_no_file(self):
        bid = build_id(4)
        sock = socket.create_connection(("127.0.0.1", self.h.port))
        sock.sendall((f"PUT /api/builds/dev/{bid}/ramdisk HTTP/1.1\r\nHost: x\r\n"
                      "Content-Length: 100000\r\n\r\n").encode() + b"x" * 1000)
        sock.close()
        for _ in range(50):  # the handler notices the closed socket
            files = list((self.h.root / "builds" / "dev" / f"{bid}.incoming").glob("*")) \
                if (self.h.root / "builds" / "dev" / f"{bid}.incoming").exists() else []
            if not any(not p.name.startswith(".") for p in files):
                break
            time.sleep(0.05)
        self.assertEqual([p.name for p in files if not p.name.startswith(".")], [])

    def test_put_without_content_length_is_refused(self):
        sock = socket.create_connection(("127.0.0.1", self.h.port))
        sock.sendall((f"PUT /api/builds/dev/{build_id(5)}/ramdisk HTTP/1.1\r\nHost: x\r\n"
                      "Transfer-Encoding: chunked\r\n\r\n0\r\n\r\n").encode())
        self.assertIn(b" 411 ", sock.recv(200))
        sock.close()

    def test_same_id_again_is_refused_at_the_put(self):
        bid = build_id(6)
        self.assertEqual(self.h.push("a", bid)[2][0], 201)
        status, doc = self.h.json("PUT", f"/api/builds/dev/{bid}/ramdisk", b"other")
        self.assertEqual(status, 409)
        status, doc = self.h.json("PUT", f"/api/builds/box/{bid}/ramdisk", b"other")
        self.assertEqual(status, 409)  # an id lives in one channel only

    def test_same_id_with_other_bytes_is_refused_at_the_commit(self):
        # two pushers raced: the second PUT happened before the first commit
        bid = build_id(7)
        first = fixtures.make_build(self.h.tmp / "f", self.h.seed, bid, payload_seed=1)
        second = fixtures.make_build(self.h.tmp / "s", self.h.seed, bid, payload_seed=2)
        store = self.h.server.store
        fixtures.put_dir(store, "dev", bid, self.h.tmp / "f", list(first["files"]) + ["manifest.json", "manifest.sig"])
        store.commit("dev", bid)
        # the second pusher's files were already in place when the first commit won
        shutil.copytree(self.h.tmp / "s", self.h.root / "builds" / "dev" / f"{bid}.incoming")
        with self.assertRaises(Exception) as ctx:
            store.commit("dev", bid)
        self.assertEqual(ctx.exception.status, 409)
        kept = self.h.root / "builds" / "dev" / bid / "ramdisk"
        self.assertEqual(kept.read_bytes(), first["files"]["ramdisk"])
        self.assertEqual(self.incoming(), [])

    def test_the_same_push_twice_is_a_no_op(self):
        bid = build_id(8)
        build, directory, _ = self.h.push("same", bid)
        store = self.h.server.store
        # a retried push whose incoming copy was already in place when the first commit won
        shutil.copytree(directory, self.h.root / "builds" / "dev" / f"{bid}.incoming")
        self.assertTrue(store.commit("dev", bid)["already"])
        self.assertEqual(self.incoming(), [])


class RefusalTests(RegistryCase):
    def refused(self, build, directory, expect, **kw):
        status, doc = self.h.upload(directory, build, **kw)
        self.assertEqual(status, expect, doc)
        self.assertEqual((self.builds_on_disk(), self.incoming()), ([], []))
        return doc

    def test_missing_signature(self):
        build = fixtures.make_build(self.h.tmp / "x", self.h.seed, build_id(1), sign=False)
        self.refused(build, self.h.tmp / "x", 400)

    def test_bad_signature(self):
        build = fixtures.make_build(self.h.tmp / "x", self.h.seed, build_id(1))
        sig = bytearray(bytes.fromhex((self.h.tmp / "x" / "manifest.sig").read_text().strip()))
        sig[5] ^= 1
        (self.h.tmp / "x" / "manifest.sig").write_text(keys.format_signature(bytes(sig)))
        self.assertIn("does not verify", self.refused(build, self.h.tmp / "x", 403)["error"])

    def test_malformed_signature_file(self):
        build = fixtures.make_build(self.h.tmp / "x", self.h.seed, build_id(1))
        (self.h.tmp / "x" / "manifest.sig").write_text("not hex\n")
        self.refused(build, self.h.tmp / "x", 400)

    def test_signature_by_a_key_not_in_keys_json(self):
        stranger = ed25519.generate_seed()
        build = fixtures.make_build(self.h.tmp / "x", stranger, build_id(1))
        self.refused(build, self.h.tmp / "x", 403)

    def test_empty_keys_json_commits_nothing(self):
        h = fixtures.Harness(register_key=False)
        self.addCleanup(h.close)
        build = fixtures.make_build(h.tmp / "x", h.seed, build_id(1))
        status, doc = h.upload(h.tmp / "x", build)
        # the commit itself is signed too: with no key it is refused before it looks at the build
        self.assertEqual(status, 401)
        self.assertEqual(h.server.store.build_ids("dev"), [])

    def test_tampered_file(self):
        directory = self.h.tmp / "x"
        build = fixtures.make_build(directory, self.h.seed, build_id(1))
        tampered = bytearray(build["files"]["ramdisk"])
        tampered[1000] ^= 0xFF
        (directory / "ramdisk").write_bytes(bytes(tampered))
        self.assertIn("SHA-256", self.refused(build, directory, 422)["error"])

    def test_wrong_size(self):
        directory = self.h.tmp / "x"
        build = fixtures.make_build(directory, self.h.seed, build_id(1))
        (directory / "ramdisk").write_bytes(build["files"]["ramdisk"] + b"x")
        self.assertIn("size", self.refused(build, directory, 422)["error"])

    def test_a_missing_file_and_an_unlisted_file(self):
        directory = self.h.tmp / "x"
        build = fixtures.make_build(directory, self.h.seed, build_id(1))
        self.assertIn("missing", self.refused(build, directory, 400, skip=("boot.json",))["error"])
        (directory / "extra.bin").write_bytes(b"x")
        build["files"]["extra.bin"] = b"x"
        self.assertIn("does not list", self.refused(build, directory, 400)["error"])

    def test_manifest_naming_another_id(self):
        directory = self.h.tmp / "x"
        build = fixtures.make_build(directory, self.h.seed, build_id(1))
        # upload it under a different (valid) id
        for name in list(build["files"]) + ["manifest.json", "manifest.sig"]:
            self.h.json("PUT", f"/api/builds/dev/{build_id(2)}/{name}", (directory / name).read_bytes())
        status, doc = self.h.signed("POST", f"/api/builds/dev/{build_id(2)}/commit", "commit")
        self.assertEqual(status, 400)
        self.assertEqual(self.builds_on_disk(), [])

    def test_manifest_without_files_or_with_bad_entries(self):
        for bad in ({"files": {}}, {"files": {"a": {"size": -1, "sha256": "0" * 64}}},
                    {"files": {"a": {"size": 1, "sha256": "XY"}}}, {"files": {"../a": {"size": 0, "sha256": "0" * 64}}},
                    {"files": {"manifest.sig": {"size": 0, "sha256": "0" * 64}}}):
            raw = json.dumps({"id": build_id(1), **bad}).encode()
            store = self.h.server.store
            store.put_file("dev", build_id(1), "manifest.json", io.BytesIO(raw), len(raw))
            sig = keys.format_signature(ed25519.sign(self.h.seed, raw)).encode()
            store.put_file("dev", build_id(1), "manifest.sig", io.BytesIO(sig), len(sig))
            with self.assertRaises(Exception) as ctx:
                store.commit("dev", build_id(1))
            self.assertEqual(ctx.exception.status, 400, bad)
            self.assertEqual(self.incoming(), [])

    def test_bad_channel_and_id(self):
        status, _ = self.h.json("PUT", "/api/builds/dev/not-an-id/ramdisk", b"x")
        self.assertEqual(status, 400)
        status, _ = self.h.json("PUT", f"/api/builds/.hidden/{build_id()}/ramdisk", b"x")
        self.assertEqual(status, 400)
        status, _ = self.h.signed("POST", "/api/builds/dev/nope/commit", "commit")
        self.assertEqual(status, 400)


class PathTests(RegistryCase):
    def test_traversal_and_odd_names_are_refused(self):
        bid = build_id(1)
        self.h.push("p", bid)
        (self.h.root / "secret.txt").write_text("secret")
        bad_names = ["..", "%2e%2e", "..%2Fsecret.txt", "%2e%2e%2f%2e%2e%2fsecret.txt", "%2Fetc%2Fpasswd",
                     "a%5Cb", "C%3A%5Cx", "with%20space", "caf%C3%A9", ".hidden", "a%00b", "x%2Fy"]
        for name in bad_names:
            with self.subTest(name=name):
                status, _ = self.h.json("PUT", f"/api/builds/dev/{build_id(2)}/{name}", b"x")
                self.assertIn(status, (400, 404, 405), name)
                status, _ = self.h.json("GET", f"/files/dev/{bid}/{name}")
                self.assertEqual(status, 404, name)
        for path in (f"/files/dev/{bid}/../../../secret.txt", f"/files/%2e%2e/{bid}/ramdisk",
                     f"/files/dev/{bid}", "/files/dev", f"/files/../secret.txt"):
            self.assertEqual(self.h.request("GET", path)[0], 404, path)
        self.assertEqual(sorted(p.name for p in (self.h.root / "builds" / "dev").iterdir()), [bid])
        self.assertFalse((self.h.root / "builds" / "dev" / f"{build_id(2)}.incoming").exists())

    def test_a_manifest_cannot_name_a_path(self):
        directory = self.h.tmp / "x"
        build = fixtures.make_build(directory, self.h.seed, build_id(1), extra={})
        manifest = json.loads(build["manifest"])
        manifest["files"]["../../escape"] = {"size": 0, "sha256": hashlib.sha256(b"").hexdigest()}
        raw = json.dumps(manifest).encode()
        (directory / "manifest.json").write_bytes(raw)
        (directory / "manifest.sig").write_text(keys.format_signature(ed25519.sign(self.h.seed, raw)))
        status, doc = self.h.upload(directory, build)
        self.assertEqual(status, 400)
        self.assertFalse((self.h.root.parent / "escape").exists())


class FileTests(RegistryCase):
    def setUp(self):
        super().setUp()
        self.bid = build_id(1)
        self.build, _, _ = self.h.push("f", self.bid)
        self.data = self.build["files"]["ramdisk"]
        self.path = f"/files/dev/{self.bid}/ramdisk"

    def test_full_file_has_length_and_accepts_ranges(self):
        status, response, body = self.h.request("GET", self.path)
        self.assertEqual(status, 200)
        self.assertEqual(int(response.getheader("Content-Length")), len(self.data))
        self.assertEqual(response.getheader("Accept-Ranges"), "bytes")

    def test_range_requests(self):
        data = self.data
        status, response, body = self.h.request("GET", self.path, headers={"Range": "bytes=1000-1999"})
        self.assertEqual((status, body), (206, data[1000:2000]))
        self.assertEqual(response.getheader("Content-Range"), f"bytes 1000-1999/{len(data)}")
        self.assertEqual(self.h.request("GET", self.path, headers={"Range": "bytes=-10"})[::2], (206, data[-10:]))
        status, _, body = self.h.request("GET", self.path, headers={"Range": f"bytes={len(data) - 5}-"})
        self.assertEqual((status, body), (206, data[-5:]))
        status, response, _ = self.h.request("GET", self.path, headers={"Range": f"bytes={len(data)}-"})
        self.assertEqual(status, 416)
        self.assertEqual(response.getheader("Content-Range"), f"bytes */{len(data)}")

    def test_head_has_length_and_no_body(self):
        status, response, body = self.h.request("HEAD", self.path)
        self.assertEqual((status, body), (200, b""))
        self.assertEqual(int(response.getheader("Content-Length")), len(self.data))

    def test_missing_file_and_log_line(self):
        self.assertEqual(self.h.request("GET", f"/files/dev/{self.bid}/nope")[0], 404)
        self.h.request("GET", self.path)
        for _ in range(100):  # the line is written just after the body
            lines = [l for l in self.h.log if f"GET {self.path} 200" in l]
            if lines:
                break
            time.sleep(0.02)
        line = lines[-1]
        self.assertIn("127.0.0.1", line)
        self.assertIn(f"{len(self.data)} B", line)
        self.assertIn(" s", line)


class SignedWriteTests(RegistryCase):
    def setUp(self):
        super().setUp()
        self.ids = [build_id(m) for m in (1, 2, 3, 4)]
        for i, bid in enumerate(self.ids):
            self.assertEqual(self.h.push(f"b{i}", bid, payload_seed=i + 1)[2][0], 201)

    def pin_path(self, action="pin"):
        return f"/api/channels/dev/{action}"

    def test_unsigned_writes_are_refused(self):
        for method, path in (("POST", self.pin_path()), ("POST", "/api/channels/dev/prune"),
                             ("POST", "/api/machines/box/next"),
                             ("DELETE", f"/api/builds/dev/{self.ids[0]}"),
                             ("POST", f"/api/builds/dev/{self.ids[0]}/commit")):
            status, doc = self.h.json(method, path, b'{"id": "%s"}' % self.ids[0].encode())
            self.assertEqual(status, 401, path)
        self.assertEqual(len(self.builds_on_disk()), 4)

    def test_unknown_key_bad_signature_tampered_body(self):
        other = ed25519.generate_seed()
        self.assertEqual(self.h.signed("POST", self.pin_path(), "pin", {"id": self.ids[0]}, seed=other)[0], 401)
        body, header = auth.sign_body(self.h.seed, "pin", {"id": self.ids[0]})
        flipped = body.replace(self.ids[0].encode(), self.ids[1].encode())
        status, _ = self.h.json("POST", self.pin_path(), flipped, {auth.SIGNATURE_HEADER: header})
        self.assertEqual(status, 401)
        for bad in (None, "", "nocolon", f"{keys.key_id(self.h.pub)}:zz", "x:" + "0" * 128):
            headers = {auth.SIGNATURE_HEADER: bad} if bad is not None else {}
            self.assertEqual(self.h.json("POST", self.pin_path(), body, headers)[0], 401, bad)
        self.assertNotIn(self.ids[0], self.h.server.store.settings("dev")[1])

    def test_wrong_action_is_refused(self):
        status, doc = self.h.signed("POST", self.pin_path("pin"), "unpin", {"id": self.ids[0]})
        self.assertEqual(status, 400)
        self.assertIn("action", doc["error"])

    def test_replayed_nonce_is_refused(self):
        body, header = auth.sign_body(self.h.seed, "pin", {"id": self.ids[0]})
        headers = {auth.SIGNATURE_HEADER: header}
        self.assertEqual(self.h.json("POST", self.pin_path(), body, headers)[0], 200)
        status, doc = self.h.json("POST", self.pin_path(), body, headers)
        self.assertEqual(status, 401)
        self.assertIn("nonce", doc["error"])

    def test_replay_survives_a_restart(self):
        body, header = auth.sign_body(self.h.seed, "pin", {"id": self.ids[0]})
        headers = {auth.SIGNATURE_HEADER: header}
        self.assertEqual(self.h.json("POST", self.pin_path(), body, headers)[0], 200)
        self.h.stop()
        self.h.start()
        self.assertEqual(self.h.json("POST", self.pin_path(), body, headers)[0], 401)

    def test_clock_skew_is_refused_in_both_directions(self):
        for offset in (-400, 400):
            status, doc = self.h.signed("POST", self.pin_path(), "pin", {"id": self.ids[0]},
                                        now=time.time() + offset)
            self.assertEqual(status, 401, offset)
            self.assertIn("ts", doc["error"])
        self.assertEqual(self.h.signed("POST", self.pin_path(), "pin", {"id": self.ids[0]},
                                       now=time.time() - 200)[0], 200)

    def test_bad_nonce_and_ts_types(self):
        for fields in ({"nonce": "short"}, {"nonce": 5}, {"ts": "now"}, {"ts": True}):
            body, header = auth.sign_body(self.h.seed, "pin", {"id": self.ids[0], **fields})
            status, _ = self.h.json("POST", self.pin_path(), body, {auth.SIGNATURE_HEADER: header})
            self.assertEqual(status, 400, fields)

    def test_body_naming_another_target_than_the_path_is_refused(self):
        status, doc = self.h.signed("POST", self.pin_path(), "pin", {"id": self.ids[0], "channel": "box"})
        self.assertEqual(status, 400)
        status, doc = self.h.signed("POST", f"/api/builds/dev/{self.ids[0]}/commit", "commit",
                                    {"id": self.ids[1]})
        self.assertEqual(status, 400)

    def test_pin_prune_delete(self):
        ids = self.ids  # ids[3] is the newest
        self.assertEqual(self.h.signed("POST", self.pin_path(), "pin", {"id": ids[0]})[1]["pinned"], True)
        status, doc = self.h.signed("POST", self.pin_path("prune"), "prune", {"keep": 2})
        self.assertEqual(status, 200)
        self.assertEqual(doc["deleted"], [ids[1]])           # the oldest unpinned beyond the newest two
        self.assertEqual(self.builds_on_disk(), [ids[3], ids[2], ids[0]])
        self.assertFalse((self.h.root / "builds" / "dev" / f"{ids[1]}.deleting").exists())
        # a pinned build is refused by delete, and not touched by prune
        status, doc = self.h.signed("DELETE", f"/api/builds/dev/{ids[0]}", "delete")
        self.assertEqual(status, 409)
        self.h.signed("POST", self.pin_path("prune"), "prune")
        self.assertIn(ids[0], self.builds_on_disk())
        # keep was persisted by the first prune
        self.assertEqual(self.h.server.store.settings("dev")[0], 2)
        # unpin, then prune takes it; delete takes one more
        self.assertEqual(self.h.signed("POST", self.pin_path("unpin"), "unpin", {"id": ids[0]})[1]["pinned"], False)
        self.assertEqual(self.h.signed("POST", self.pin_path("prune"), "prune")[1]["deleted"], [ids[0]])
        self.assertEqual(self.h.signed("DELETE", f"/api/builds/dev/{ids[2]}", "delete")[0], 200)
        self.assertEqual(self.builds_on_disk(), [ids[3]])
        self.assertEqual(self.h.signed("DELETE", f"/api/builds/dev/{ids[2]}", "delete")[0], 404)

    def test_pin_of_an_unknown_build_and_a_bad_keep(self):
        self.assertEqual(self.h.signed("POST", self.pin_path(), "pin", {"id": build_id(55)})[0], 404)
        self.assertEqual(self.h.signed("POST", self.pin_path("prune"), "prune", {"keep": 0})[0], 400)
        self.assertEqual(self.h.signed("POST", self.pin_path("prune"), "prune", {"keep": "2"})[0], 400)
        self.assertEqual(self.h.signed("POST", "/api/channels/none/prune", "prune")[0], 404)

    def test_prune_keeps_a_queued_next(self):
        h = fixtures.Harness(machines={"box": {"mac": "aa:bb:cc:dd:ee:ff", "channel": "dev"}})
        self.addCleanup(h.close)
        ids = [build_id(m) for m in (1, 2, 3)]
        for i, bid in enumerate(ids):
            h.push(f"b{i}", bid, payload_seed=i + 1)
        self.assertEqual(h.signed("POST", "/api/machines/box/next", "next", {"id": ids[0]})[0], 200)
        status, doc = h.signed("POST", "/api/channels/dev/prune", "prune", {"keep": 1})
        self.assertEqual(doc["deleted"], [ids[1]])
        self.assertIn(ids[0], h.server.store.build_ids("dev"))


class MiscTests(RegistryCase):
    def test_unknown_routes_and_methods(self):
        self.assertEqual(self.h.request("GET", "/nope")[0], 404)
        self.assertEqual(self.h.request("GET", "/api/channels/dev/pin")[0], 404)
        self.assertEqual(self.h.request("PATCH", "/api/builds")[0], 405)
        self.assertEqual(self.h.request("GET", "/api/builds?mac=zz")[0], 400)

    def test_access_log_has_one_line_per_request(self):
        before = len(self.h.log)
        self.h.request("GET", "/healthz")
        for _ in range(100):
            if len(self.h.log) > before:
                break
            time.sleep(0.02)
        self.assertEqual(len(self.h.log), before + 1)
        self.assertIn("GET /healthz 200", self.h.log[-1])

    def test_oversized_signed_body_is_refused(self):
        status, _ = self.h.json("POST", "/api/channels/dev/prune", b"x" * (auth.MAX_BODY + 1))
        self.assertEqual(status, 413)


if __name__ == "__main__":
    unittest.main()
