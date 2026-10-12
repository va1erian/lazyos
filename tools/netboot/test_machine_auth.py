"""Machines added by the registry, their callback keys, and the HMAC-authenticated booting/booted."""

import json
import time
import unittest

import auth
import fixtures
import registry
import store as store_mod
from fixtures import build_id

MAC = "aa:bb:cc:dd:ee:ff"


class MachineAuthCase(unittest.TestCase):
    def setUp(self):
        self.h = fixtures.Harness()
        self.addCleanup(self.h.close)
        self.bid = build_id(1)
        self.h.push("b", self.bid)
        status, doc = self.h.signed("POST", "/api/machines/box/add", "add", {"mac": MAC, "channel": "dev"})
        self.assertEqual(status, 201, doc)
        self.assertNotIn("callback_key", doc)           # no key crosses the wire
        self.key = self.mint("box")

    def mint(self, name):
        """What `registry.py --machine-key NAME` does, on the registry host."""
        return registry.mint_machine_key(self.h.root, name)

    def machine_post(self, action, key=None, machine="box", path_machine="box", fields=None,
                     now=None, nonce=None, bid=None):
        """A request signed with a callback key, as the boot OS sends it."""
        fields = {"id": bid or self.bid, **(fields or {})}
        body, header = auth.sign_machine(key or self.key, machine, action, fields, now, nonce)
        return self.h.json("POST", f"/api/machines/{path_machine}/{action}", body,
                           {auth.MACHINE_HEADER: header, "Content-Type": "application/json"})

    def entry(self, name="box"):
        return json.loads((self.h.root / "machines.json").read_text())[name]


class AddTests(MachineAuthCase):
    def test_add_creates_the_entry_without_a_key(self):
        status, doc = self.h.signed("POST", "/api/machines/nuc/add", "add",
                                    {"mac": "AA-BB-CC-DD-EE-01", "channel": "box", "timeout_ms": 2000})
        self.assertEqual(status, 201)
        self.assertNotIn("callback_key", json.dumps(doc))
        entry = self.entry("nuc")
        self.assertEqual((entry["mac"], entry["channel"], entry["timeout_ms"]), ("aa:bb:cc:dd:ee:01", "box", 2000))
        self.assertNotIn("callback_key", entry)

    def test_minting_makes_a_random_64_hex_key_and_stores_it(self):
        self.assertRegex(self.key, "^[0-9a-f]{64}$")
        self.assertEqual(self.entry()["callback_key"], self.key)
        self.h.signed("POST", "/api/machines/nuc/add", "add", {"mac": "00:00:00:00:00:05"})
        self.assertNotEqual(self.mint("nuc"), self.key)

    def test_the_key_is_never_listed_or_logged(self):
        listing = self.h.request("GET", f"/api/builds?mac={MAC}")[2].decode()
        self.assertIn('"box"', listing)
        self.assertNotIn(self.key, listing)
        self.assertNotIn("callback_key", listing)
        self.assertNotIn(self.key, self.h.request("GET", f"/boot.ipxe?mac={MAC}")[2].decode())
        self.assertFalse(any(self.key in line for line in self.h.log))

    def test_duplicates_and_bad_values_are_refused(self):
        add = lambda name, fields: self.h.signed("POST", f"/api/machines/{name}/add", "add", fields)[0]
        self.assertEqual(add("box", {"mac": "00:00:00:00:00:02"}), 409)                  # name exists
        self.assertEqual(add("other", {"mac": MAC}), 409)                                 # mac taken
        self.assertEqual(add("other", {"mac": "nope"}), 400)
        self.assertEqual(add("other", {}), 400)
        self.assertEqual(add("other", {"mac": "00:00:00:00:00:02", "channel": "../x"}), 400)
        self.assertEqual(add("other", {"mac": "00:00:00:00:00:02", "timeout_ms": -1}), 400)
        self.assertEqual(add("other", {"mac": "00:00:00:00:00:02", "timeout_ms": True}), 400)
        self.assertNotIn("other", json.loads((self.h.root / "machines.json").read_text()))

    def test_add_is_a_signed_write_and_there_is_no_rekey_route(self):
        self.assertEqual(self.h.json("POST", "/api/machines/nuc/add",
                                     json.dumps({"mac": "00:00:00:00:00:02"}).encode())[0], 401)
        for method in ("POST", "GET"):
            status, _ = self.h.signed(method, "/api/machines/box/rekey", "rekey")
            self.assertIn(status, (404, 405))
        self.assertEqual(self.entry()["callback_key"], self.key)

    def test_minting_for_an_unknown_machine_is_refused(self):
        with self.assertRaises(store_mod.StoreError) as ctx:
            self.mint("ghost")
        self.assertEqual(ctx.exception.status, 404)
        self.assertNotIn("ghost", json.loads((self.h.root / "machines.json").read_text()))

    def test_re_minting_kills_the_old_key(self):
        new = self.mint("box")
        self.assertNotEqual(new, self.key)
        self.assertEqual(self.machine_post("booting", key=self.key)[0], 401)
        self.assertEqual(self.machine_post("booting", key=new)[0], 200)

    def test_a_machine_without_a_key_refuses_hmac_and_accepts_the_dev_key(self):
        self.h.signed("POST", "/api/machines/nuc/add", "add", {"mac": "00:00:00:00:00:06"})
        for key in (self.key, "00" * 32, "ab" * 32):
            status, doc = self.machine_post("booting", key=key, machine="nuc", path_machine="nuc")
            self.assertEqual(status, 401, key)
        self.assertNotIn("last_attempt", self.entry("nuc"))
        self.assertEqual(self.h.signed("POST", "/api/machines/nuc/booting", "booting", {"id": self.bid})[0], 200)


class HmacTests(MachineAuthCase):
    def test_a_correct_hmac_is_accepted_for_booting_and_booted(self):
        self.h.signed("POST", "/api/machines/box/next", "next", {"id": self.bid})
        status, doc = self.machine_post("booting")
        self.assertEqual((status, doc["next_attempts"]), (200, 1))
        status, doc = self.machine_post("booted")
        self.assertEqual((status, doc["consumed_next"]), (200, True))
        entry = self.entry()
        self.assertEqual((entry["next"], entry["last_boot"]["id"]), (None, self.bid))

    def test_a_wrong_key_is_refused(self):
        status, doc = self.machine_post("booting", key="00" * 32)
        self.assertEqual(status, 401)
        self.assertNotIn("last_attempt", self.entry())

    def test_unknown_machine_and_machine_without_a_key_are_refused(self):
        self.assertEqual(self.machine_post("booting", machine="ghost", path_machine="ghost")[0], 401)
        data = json.loads((self.h.root / "machines.json").read_text())
        data["old"] = {"mac": "00:00:00:00:00:07", "channel": "dev"}   # hand-written: no callback_key
        (self.h.root / "machines.json").write_text(json.dumps(data))
        self.assertEqual(self.machine_post("booting", machine="old", path_machine="old", key="00" * 32)[0], 401)
        self.assertEqual(self.h.signed("POST", "/api/machines/old/booting", "booting", {"id": self.bid})[0], 200)

    def test_a_replayed_nonce_is_refused(self):
        body, header = auth.sign_machine(self.key, "box", "booting", {"id": self.bid})
        headers = {auth.MACHINE_HEADER: header}
        self.assertEqual(self.h.json("POST", "/api/machines/box/booting", body, headers)[0], 200)
        status, doc = self.h.json("POST", "/api/machines/box/booting", body, headers)
        self.assertEqual(status, 401)
        self.assertIn("nonce", doc["error"])
        self.assertEqual(self.machine_post("booting", nonce="a" * 32)[0], 200)
        self.assertEqual(self.machine_post("booting", nonce="a" * 32)[0], 401)

    def test_a_replay_across_actions_is_refused_too(self):
        # the nonce list is shared with the dev-key writes
        self.assertEqual(self.machine_post("booting", nonce="b" * 32)[0], 200)
        status, _ = self.h.signed("POST", "/api/machines/box/booted", "booted", {"id": self.bid}, nonce="b" * 32)
        self.assertEqual(status, 401)

    def test_a_body_whose_machine_differs_from_the_path_is_refused(self):
        status, doc = self.machine_post("booting", fields={"machine": "laptop"})
        self.assertEqual(status, 400)
        # a valid machine, but the path names another one
        status, _ = self.h.signed("POST", "/api/machines/laptop/add", "add", {"mac": "00:00:00:00:00:03"})
        self.assertEqual(status, 201)
        self.assertEqual(self.machine_post("booting", path_machine="laptop")[0], 400)
        self.assertNotIn("last_attempt", self.entry("laptop"))

    def test_a_body_without_machine_is_refused(self):
        body = json.dumps({"action": "booting", "nonce": "c" * 32, "ts": int(time.time()), "id": self.bid},
                          sort_keys=True, separators=(",", ":")).encode()
        import hashlib
        import hmac
        mac = hmac.new(bytes.fromhex(self.key), body, hashlib.sha256).hexdigest()
        status, _ = self.h.json("POST", "/api/machines/box/booting", body, {auth.MACHINE_HEADER: f"box:{mac}"})
        self.assertEqual(status, 400)

    def test_tampered_body_and_wrong_action_are_refused(self):
        body, header = auth.sign_machine(self.key, "box", "booting", {"id": self.bid})
        other = body.replace(self.bid.encode(), build_id(2).encode())
        self.assertEqual(self.h.json("POST", "/api/machines/box/booting", other, {auth.MACHINE_HEADER: header})[0], 401)
        # a booting body presented to booted
        self.assertEqual(self.h.json("POST", "/api/machines/box/booted", body, {auth.MACHINE_HEADER: header})[0], 400)

    def test_stale_clock_and_malformed_headers_are_refused(self):
        for offset in (-400, 400):
            self.assertEqual(self.machine_post("booting", now=time.time() + offset)[0], 401, offset)
        body, header = auth.sign_machine(self.key, "box", "booting", {"id": self.bid})
        for bad in ("", "box", "box:zz", f"box:{'0' * 63}", f":{'0' * 64}", f"../x:{'0' * 64}"):
            self.assertEqual(self.h.json("POST", "/api/machines/box/booting", body, {auth.MACHINE_HEADER: bad})[0],
                             401, bad)

    def test_the_header_naming_another_machine_than_the_path_is_refused(self):
        status, doc = self.h.signed("POST", "/api/machines/laptop/add", "add", {"mac": "00:00:00:00:00:03"})
        body, header = auth.sign_machine(self.mint("laptop"), "laptop", "booting", {"id": self.bid})
        status, _ = self.h.json("POST", "/api/machines/box/booting", body, {auth.MACHINE_HEADER: header})
        self.assertEqual(status, 400)

    def test_the_id_must_be_an_existing_build(self):
        self.assertEqual(self.machine_post("booting", bid=build_id(40))[0], 404)
        self.assertEqual(self.machine_post("booted", bid=build_id(40))[0], 404)
        self.assertEqual(self.machine_post("booting", bid="nonsense")[0], 400)

    def test_the_dev_key_is_accepted_and_unauthenticated_calls_are_not(self):
        self.assertEqual(self.h.signed("POST", "/api/machines/box/booting", "booting", {"id": self.bid})[0], 200)
        self.assertEqual(self.h.signed("POST", "/api/machines/box/booted", "booted", {"id": self.bid})[0], 200)
        for action in ("booting", "booted"):
            status, doc = self.h.json("POST", f"/api/machines/box/{action}", json.dumps({"id": self.bid}).encode())
            self.assertEqual(status, 401)
        # a dev-key signature by an unregistered key is no better than none
        from ed25519 import generate_seed
        self.assertEqual(self.h.signed("POST", "/api/machines/box/booting", "booting", {"id": self.bid},
                                       seed=generate_seed())[0], 401)

    def test_both_kinds_of_requests_share_the_failed_next_rule(self):
        self.h.signed("POST", "/api/machines/box/next", "next", {"id": self.bid})
        self.machine_post("booting")
        self.h.signed("POST", "/api/machines/box/booting", "booting", {"id": self.bid})
        doc = self.h.json("GET", f"/api/builds?mac={MAC}")[1]
        self.assertIsNone(doc["next"])
        self.assertTrue(doc["machine"]["last_boot"]["failed"])


if __name__ == "__main__":
    unittest.main()
