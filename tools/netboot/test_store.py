"""Units under the server: names, key files, the nonce list, Range parsing, the store's sweep."""

import json
import os
import tempfile
import time
import unittest
from pathlib import Path

import auth
import ed25519
import httpfiles
import keys
import names
import store as store_mod
from fixtures import build_id


class NameTests(unittest.TestCase):
    def test_names(self):
        for good in ("dev", "kernel-x86_64", "boot.json", "a_b-c.d", "x" * 128):
            self.assertTrue(names.is_name(good), good)
        for bad in ("", ".", "..", ".hidden", "a/b", "a\\b", "/etc", "C:", "a b", "café", "a\0b",
                    "x" * 129, None, 5, "a\n"):
            self.assertFalse(names.is_name(bad), repr(bad))

    def test_build_ids(self):
        for good in ("20261012-093000-2eeee936", "20261012-093000-2eeee936-dirty"):
            self.assertTrue(names.is_build_id(good))
        for bad in ("20261012-093000-2EEEE936", "20261012-093000-2eeee93", "2026-10-12", "20261012-093000-2eeee936-clean",
                    "20261012-093000-2eeee936\n", "../20261012-093000-2eeee936", ""):
            self.assertFalse(names.is_build_id(bad), repr(bad))

    def test_macs(self):
        self.assertEqual(names.normalize_mac("AA-BB-CC-DD-EE-FF"), "aa:bb:cc:dd:ee:ff")
        self.assertEqual(names.normalize_mac(" aa:bb:cc:dd:ee:ff "), "aa:bb:cc:dd:ee:ff")
        for bad in ("aa:bb:cc:dd:ee", "aa:bb:cc:dd:ee:gg", "aabbccddeeff", "", None, "aa:bb-cc:dd:ee:ff:00:11"):
            self.assertIsNone(names.normalize_mac(bad), repr(bad))


class KeyFileTests(unittest.TestCase):
    def test_key_files_and_ids(self):
        seed = ed25519.generate_seed()
        with tempfile.TemporaryDirectory() as tmp:
            sec, pub = keys.write_keypair(Path(tmp), seed)
            self.assertEqual(sec.read_text(), seed.hex() + "\n")
            self.assertEqual(pub.read_text(), ed25519.public_key(seed).hex() + "\n")
            self.assertEqual(keys.read_secret(sec), seed)
            self.assertEqual(keys.read_public(pub), ed25519.public_key(seed))
            if os.name == "posix":
                self.assertEqual(sec.stat().st_mode & 0o777, 0o600)
            sec.write_text("zz\n")
            with self.assertRaises(ValueError):
                keys.read_secret(sec)
            with self.assertRaises(ValueError):
                keys.read_secret(Path(tmp) / "missing")

    def test_key_id_is_the_first_16_hex_of_sha256(self):
        import hashlib
        pub = bytes(range(32))
        self.assertEqual(keys.key_id(pub), hashlib.sha256(pub).hexdigest()[:16])

    def test_keys_json_loading_skips_bad_entries(self):
        good = ed25519.public_key(ed25519.generate_seed())
        other = ed25519.public_key(ed25519.generate_seed())
        with tempfile.TemporaryDirectory() as tmp:
            path = Path(tmp) / "keys.json"
            self.assertEqual(keys.load_keys(path), {})                       # missing
            path.write_text("not json")
            self.assertEqual(keys.load_keys(path), {})
            path.write_text(json.dumps({"keys": [
                keys.keys_entry(good, "ok"),
                {"id": "0123456789abcdef", "pub": other.hex(), "name": "id does not match"},
                {"pub": "zz"}, {"id": "x"}, "string", {"pub": other.hex(), "name": "no id: accepted"}]}))
            self.assertEqual(set(keys.load_keys(path)), {keys.key_id(good), keys.key_id(other)})

    def test_signature_text(self):
        sig = bytes(range(64))
        self.assertEqual(keys.parse_signature(keys.format_signature(sig)), sig)
        self.assertEqual(keys.parse_signature(sig.hex()), sig)
        self.assertEqual(keys.parse_signature(sig.hex() + "\r\n"), sig)
        for bad in ("", sig.hex()[:-2], sig.hex().upper(), sig.hex() + "\n\n", "g" * 128, b"\xff" * 128):
            self.assertIsNone(keys.parse_signature(bad), repr(bad)[:30])


class NonceTests(unittest.TestCase):
    def test_duplicate_refused_expiry_and_cap(self):
        now = [1000.0]
        nonces = auth.NonceList(None, clock=lambda: now[0], memory_s=100, cap=3)
        self.assertTrue(nonces.add("a"))
        self.assertFalse(nonces.add("a"))
        now[0] += 101
        self.assertTrue(nonces.add("a"))          # forgotten after the memory time
        for n in ("b", "c", "d"):
            self.assertTrue(nonces.add(n))
        self.assertEqual(len(nonces), 3)           # capped: the oldest ("a") went
        self.assertFalse(nonces.add("d"))
        self.assertTrue(nonces.add("a"))

    def test_persisted_across_instances_and_garbage_ignored(self):
        with tempfile.TemporaryDirectory() as tmp:
            path = Path(tmp) / "nonces.json"
            path.write_text(json.dumps([["x", 5], "junk", ["y"], [1, 2], ["z", True]]))
            nonces = auth.NonceList(path, clock=lambda: 10.0)
            self.assertEqual(len(nonces), 1)
            self.assertTrue(nonces.add("n1"))
            self.assertFalse(auth.NonceList(path, clock=lambda: 11.0).add("n1"))
            self.assertFalse(auth.NonceList(path, clock=lambda: 11.0).add("x"))

    def test_verify_signed_records_the_nonce_only_after_every_check(self):
        seed = ed25519.generate_seed()
        known = {keys.key_id(ed25519.public_key(seed)): ed25519.public_key(seed)}
        nonces = auth.NonceList(None)
        import time
        body, header = auth.sign_body(seed, "pin", {"id": "x"})
        with self.assertRaises(auth.AuthError):
            auth.verify_signed(body, header, "unpin", known, nonces, time.time())   # wrong action
        with self.assertRaises(auth.AuthError):
            auth.verify_signed(body, header, "pin", known, nonces, time.time() + 1000)  # stale
        self.assertEqual(len(nonces), 0)
        fields, _ = auth.verify_signed(body, header, "pin", known, nonces, time.time())
        self.assertEqual(fields["id"], "x")
        self.assertEqual(len(nonces), 1)


class RangeTests(unittest.TestCase):
    def test_no_header_or_unsupported_forms_mean_the_whole_file(self):
        for header in (None, "", "items=0-1", "bytes=0-1,5-6", "bytes=-", "garbage", "bytes=5-2"):
            self.assertIsNone(httpfiles.parse_range(header, 100), header)

    def test_forms(self):
        self.assertEqual(httpfiles.parse_range("bytes=0-9", 100), (0, 9))
        self.assertEqual(httpfiles.parse_range("bytes=90-", 100), (90, 99))
        self.assertEqual(httpfiles.parse_range("bytes=90-500", 100), (90, 99))
        self.assertEqual(httpfiles.parse_range("bytes=-10", 100), (90, 99))
        self.assertEqual(httpfiles.parse_range("bytes=-500", 100), (0, 99))
        self.assertEqual(httpfiles.parse_range("bytes=99-99", 100), (99, 99))

    def test_unsatisfiable(self):
        for header in ("bytes=100-", "bytes=100-120", "bytes=-0"):
            with self.assertRaises(httpfiles.RangeError, msg=header):
                httpfiles.parse_range(header, 100)
        with self.assertRaises(httpfiles.RangeError):
            httpfiles.parse_range("bytes=0-1", 0)


class StoreTests(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory(ignore_cleanup_errors=True)
        self.addCleanup(self.tmp.cleanup)
        self.now = [time.time()]
        self.store = store_mod.Store(Path(self.tmp.name) / "r", keep=3, clock=lambda: self.now[0])

    def test_init_is_idempotent_and_keeps_edits(self):
        notes = self.store.init_layout()
        self.assertTrue(any("no key may push yet" in n for n in notes))
        (self.store.root / "machines.json").write_text('{"box": {}}')
        again = self.store.init_layout()
        self.assertEqual(json.loads((self.store.root / "machines.json").read_text()), {"box": {}})
        self.assertFalse(any(n.startswith("created") for n in again))

    def test_stale_incoming_is_swept_and_fresh_kept(self):
        self.store.init_layout()
        stale = self.store.incoming_dir("dev", build_id(1))
        fresh = self.store.incoming_dir("dev", build_id(2))
        for d in (stale, fresh):
            d.mkdir(parents=True)
            (d / "ramdisk").write_bytes(b"x")
        old = self.now[0] - store_mod.STALE_INCOMING_S - 60
        os.utime(stale / "ramdisk", (old, old))
        os.utime(stale, (old, old))
        self.assertEqual(self.store.sweep_incoming(), 1)
        self.assertFalse(stale.exists())
        self.assertTrue(fresh.exists())

    def test_empty_channel_directories_are_removed_by_the_sweep(self):
        self.store.init_layout()
        (self.store.builds_dir / "junk").mkdir()
        self.store.sweep_incoming()
        self.assertFalse((self.store.builds_dir / "junk").exists())

    def test_hostile_names_never_reach_the_file_system(self):
        self.store.init_layout()
        import io
        for channel, bid, name in (("..", build_id(), "x"), ("dev", "../x", "x"), ("dev", build_id(), "../x"),
                                   ("dev", build_id(), "/abs"), ("a/b", build_id(), "x")):
            with self.assertRaises(store_mod.StoreError, msg=(channel, bid, name)):
                self.store.put_file(channel, bid, name, io.BytesIO(b"x"), 1)
        self.assertIsNone(self.store.file_path("dev", build_id(), "../keys.json"))
        self.assertEqual(list(self.store.root.rglob("x")), [])

    def test_upload_file_limit(self):
        self.store.init_layout()
        import io
        for i in range(store_mod.MAX_UPLOAD_FILES):
            self.store.put_file("dev", build_id(), f"f{i}", io.BytesIO(b"x"), 1)
        with self.assertRaises(store_mod.StoreError):
            self.store.put_file("dev", build_id(), "one-too-many", io.BytesIO(b"x"), 1)
        self.store.put_file("dev", build_id(), "f0", io.BytesIO(b"y"), 1)   # replacing one is fine


if __name__ == "__main__":
    unittest.main()
