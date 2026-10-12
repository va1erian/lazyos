"""Machines: the listing, the one-shot `next`, booting and booted, and the optional iPXE text."""

import json
import unittest

import fixtures
import menu
from fixtures import build_id

MAC = "aa:bb:cc:dd:ee:ff"
MACHINES = {"box": {"mac": MAC, "channel": "box", "timeout_ms": 3000},
            "laptop": {"mac": "11:22:33:44:55:66"}}


class MachineCase(unittest.TestCase):
    def setUp(self):
        self.h = fixtures.Harness(machines=json.loads(json.dumps(MACHINES)))
        self.addCleanup(self.h.close)
        # dev: two builds; box: one
        self.dev = [build_id(1), build_id(2)]
        self.box = [build_id(3, rev8="abcdef01")]
        for i, bid in enumerate(self.dev):
            self.assertEqual(self.h.push(f"d{i}", bid, "dev", payload_seed=i + 1)[2][0], 201)
        self.assertEqual(self.h.push("b0", self.box[0], "box", payload_seed=9)[2][0], 201)

    def machines_json(self):
        return (self.h.root / "machines.json").read_text()

    def listing(self, mac=MAC):
        status, doc = self.h.json("GET", f"/api/builds?mac={mac}")
        self.assertEqual(status, 200)
        return doc

    def machine(self, name="box"):
        return json.loads(self.machines_json())[name]

    def set_next(self, bid, name="box"):
        status, doc = self.h.signed("POST", f"/api/machines/{name}/next", "next", {"id": bid, "machine": name})
        self.assertEqual(status, 200, doc)
        return doc

    def booting(self, bid, name="box"):
        """The picker's call, here signed with the dev key (the HMAC path is in test_machine_auth.py)."""
        return self.h.signed("POST", f"/api/machines/{name}/booting", "booting", {"id": bid})

    def booted(self, bid, name="box"):
        return self.h.signed("POST", f"/api/machines/{name}/booted", "booted", {"id": bid})


class ListingTests(MachineCase):
    def test_known_machine(self):
        doc = self.listing()
        self.assertEqual(doc["machine"]["name"], "box")
        self.assertEqual((doc["channel"], doc["timeout_ms"]), ("box", 3000))
        self.assertEqual([c["name"] for c in doc["channels"]], ["box", "dev"])
        self.assertEqual(doc["default"], {"id": self.box[0], "channel": "box"})
        self.assertIsNone(doc["next"])
        self.assertEqual(doc["machine"]["boot_state"], "none")

    def test_channel_order_is_newest_first(self):
        doc = self.listing("11:22:33:44:55:66")  # laptop: no channel set, so dev
        self.assertEqual(doc["machine"]["name"], "laptop")
        dev = next(c for c in doc["channels"] if c["name"] == "dev")
        self.assertEqual([b["id"] for b in dev["builds"]], list(reversed(self.dev)))
        self.assertEqual(doc["default"]["id"], self.dev[1])
        self.assertEqual(doc["timeout_ms"], 5000)

    def test_unknown_mac_gets_dev_and_ten_seconds(self):
        doc = self.listing("de:ad:be:ef:00:01")
        self.assertFalse(doc["machine"]["known"])
        self.assertIsNone(doc["machine"]["name"])
        self.assertEqual((doc["channel"], doc["timeout_ms"]), ("dev", 10_000))
        self.assertEqual(doc["default"]["id"], self.dev[1])
        status, doc = self.h.json("GET", "/api/builds")  # no mac at all
        self.assertEqual((status, doc["channel"]), (200, "dev"))

    def test_mac_spellings(self):
        for spelling in ("AA:BB:CC:DD:EE:FF", "aa-bb-cc-dd-ee-ff", "AA-BB-CC-DD-EE-FF"):
            self.assertEqual(self.listing(spelling)["machine"]["name"], "box", spelling)

    def test_entries_carry_the_signed_manifest(self):
        build = self.listing()["channels"][0]["builds"][0]
        self.assertEqual(build["id"], self.box[0])
        self.assertEqual(json.loads(build["manifest_text"]), build["manifest"])
        self.assertEqual(len(build["sig"]), 128)
        self.assertFalse(build["pinned"])

    def test_pinned_is_shown(self):
        self.h.signed("POST", "/api/channels/dev/pin", "pin", {"id": self.dev[0]})
        dev = next(c for c in self.listing()["channels"] if c["name"] == "dev")
        self.assertEqual(dev["pinned"], [self.dev[0]])
        self.assertEqual([b["pinned"] for b in dev["builds"]], [False, True])

    def test_get_is_read_only(self):
        self.set_next(self.dev[0])
        before = (self.machines_json(), (self.h.root / "channels.json").read_text())
        for _ in range(3):
            self.listing()
            self.h.request("GET", f"/boot.ipxe?mac={MAC}")
        self.assertEqual((self.machines_json(), (self.h.root / "channels.json").read_text()), before)


class NextTests(MachineCase):
    def test_next_is_the_default_and_survives_a_booting(self):
        self.set_next(self.dev[0])
        doc = self.listing()
        self.assertEqual(doc["next"]["id"], self.dev[0])
        self.assertEqual((doc["default"]["id"], doc["default"]["channel"]), (self.dev[0], "dev"))
        status, reply = self.booting(self.dev[0])
        self.assertEqual((status, reply["next_attempts"]), (200, 1))
        doc = self.listing()
        self.assertEqual((doc["next"]["id"], doc["next"]["attempts"]), (self.dev[0], 1))
        self.assertEqual(doc["machine"]["last_attempt"]["id"], self.dev[0])
        self.assertEqual(doc["machine"]["boot_state"], "attempted")

    def test_booted_consumes_next_once_and_records_last_boot(self):
        self.set_next(self.dev[0])
        self.booting(self.dev[0])
        status, reply = self.booted(self.dev[0])
        self.assertEqual((status, reply["consumed_next"]), (200, True))
        doc = self.listing()
        self.assertIsNone(doc["next"])
        self.assertEqual(doc["machine"]["last_boot"]["id"], self.dev[0])
        self.assertEqual(doc["machine"]["boot_state"], "confirmed")
        self.assertEqual(doc["default"]["id"], self.box[0])  # back to the channel's newest
        self.assertFalse(self.booted(self.dev[0])[1]["consumed_next"])  # nothing left to consume

    def test_booted_of_another_id_leaves_next_pending(self):
        self.set_next(self.dev[0])
        self.booting(self.dev[1])
        self.assertEqual(self.listing()["next"]["attempts"], 0)  # not the queued build: not counted
        self.assertFalse(self.booted(self.dev[1])[1]["consumed_next"])
        doc = self.listing()
        self.assertEqual(doc["next"]["id"], self.dev[0])
        self.assertEqual(doc["machine"]["last_boot"]["id"], self.dev[1])

    def test_attempted_not_confirmed(self):
        self.booting(self.dev[0])
        self.assertEqual(self.listing()["machine"]["boot_state"], "attempted")
        self.booted(self.box[0])  # a different build came up
        self.assertEqual(self.machine()["last_boot"]["id"], self.box[0])

    def test_second_unconfirmed_booting_drops_next_as_failed(self):
        self.set_next(self.dev[0])
        self.booting(self.dev[0])
        self.assertEqual(self.listing()["next"]["attempts"], 1)       # one try: still pending
        self.assertEqual(self.booting(self.dev[0])[1]["next_attempts"], 2)
        doc = self.listing()                                          # the GET drops it
        self.assertIsNone(doc["next"])
        last = doc["machine"]["last_boot"]
        self.assertEqual((last["id"], last["failed"]), (self.dev[0], True))
        self.assertEqual(doc["machine"]["boot_state"], "failed")
        self.assertEqual(doc["default"]["id"], self.box[0])           # no loop on the bad build

    def test_booting_after_two_attempts_drops_it_too(self):
        self.set_next(self.dev[0])
        self.booting(self.dev[0])
        self.booting(self.dev[0])
        self.assertEqual(self.machine()["next"]["attempts"], 2)       # no GET in between
        status, reply = self.booting(self.dev[1])                    # the next booting expires it
        self.assertIsNone(self.machine()["next"])
        self.assertTrue(self.machine()["last_boot"]["failed"])

    def test_booted_between_the_tries_saves_it(self):
        self.set_next(self.dev[0])
        self.booting(self.dev[0])
        self.booted(self.dev[0])
        self.booting(self.dev[0])
        self.assertIsNone(self.listing()["next"])
        self.assertNotIn("failed", self.machine()["last_boot"])

    def test_a_new_next_restarts_the_count(self):
        self.set_next(self.dev[0])
        self.booting(self.dev[0])
        self.set_next(self.dev[0])
        self.assertEqual(self.listing()["next"]["attempts"], 0)

    def test_next_whose_build_was_deleted_is_not_offered(self):
        self.set_next(self.dev[0])
        self.h.signed("DELETE", f"/api/builds/dev/{self.dev[0]}", "delete")
        self.assertIsNone(self.listing()["next"])

    def test_refusals(self):
        status, _ = self.h.signed("POST", "/api/machines/ghost/next", "next", {"id": self.dev[0]})
        self.assertEqual(status, 404)
        self.assertEqual(self.h.signed("POST", "/api/machines/box/next", "next", {"id": build_id(50)})[0], 404)
        self.assertEqual(self.h.signed("POST", "/api/machines/box/next", "next", {"id": "x"})[0], 400)
        self.assertEqual(self.h.signed("POST", "/api/machines/box/next", "next",
                                       {"id": self.dev[0], "machine": "laptop"})[0], 400)
        self.assertEqual(self.h.json("POST", "/api/machines/box/next", b'{"id": "%s"}' % self.dev[0].encode())[0], 401)
        self.assertEqual(self.booting(build_id(50))[0], 404)
        self.assertEqual(self.booting("x")[0], 400)
        self.assertEqual(self.booting(self.dev[0], "ghost")[0], 404)
        self.assertEqual(self.booted(build_id(50))[0], 404)         # a build the registry does not hold
        # unauthenticated booting/booted are gone
        for action in ("booting", "booted"):
            self.assertEqual(self.h.json("POST", f"/api/machines/box/{action}",
                                         b'{"id": "%s"}' % self.dev[0].encode())[0], 401)
        self.assertEqual(self.h.json("POST", "/api/machines/%2e%2e/booted", b"{}")[0], 400)

    def test_a_hand_edited_machines_file_is_picked_up(self):
        (self.h.root / "machines.json").write_text(json.dumps({"nuc": {"mac": "00:00:00:00:00:01", "channel": "dev"}}))
        self.assertEqual(self.listing("00:00:00:00:00:01")["machine"]["name"], "nuc")


class IpxeTests(MachineCase):
    def get(self, path, host="registry.lan:8420"):
        status, _, body = self.h.request("GET", path, headers={"Host": host})
        return status, body.decode()

    def test_menu_for_a_known_machine(self):
        status, text = self.get(f"/boot.ipxe?mac={MAC}")
        self.assertEqual(status, 200)
        lines = text.splitlines()
        self.assertEqual(lines[0], "#!ipxe")
        self.assertIn("set reg http://registry.lan:8420", lines)
        self.assertTrue(any(l.startswith("item b0 ") and self.box[0] in l for l in lines))
        self.assertIn("choose --default b0 --timeout 3000 target || goto local", lines)
        self.assertIn(f"chain ${{reg}}/builds/box/{self.box[0]}.ipxe || goto menu", lines)
        self.assertNotIn(self.dev[0], text)  # the machine's channel only

    def test_menu_default_follows_next_and_orders_newest_first(self):
        self.set_next(self.dev[0], "laptop")
        status, text = self.get("/boot.ipxe?mac=11:22:33:44:55:66")
        lines = text.splitlines()
        items = [l for l in lines if l.startswith("item b")]
        self.assertEqual([l.split()[2] for l in items], [self.dev[1], self.dev[0]])
        self.assertIn("[next]", items[1])
        self.assertIn("choose --default b1 --timeout 5000 target || goto local", lines)

    def test_unknown_machine_gets_dev_with_ten_seconds(self):
        status, text = self.get("/boot.ipxe?mac=de:ad:be:ef:00:01")
        self.assertIn("--timeout 10000", text)
        self.assertIn("choose --default b0", text)
        self.assertIn(self.dev[1], text)

    def test_every_goto_has_a_label_and_every_item_is_handled(self):
        for mac in (MAC, "de:ad:be:ef:00:01"):
            text = self.get(f"/boot.ipxe?mac={mac}")[1]
            labels = {l[1:] for l in text.splitlines() if l.startswith(":")}
            for line in text.splitlines():
                for word in line.split("goto ")[1:]:
                    target = word.split()[0]
                    if target != "${target}":
                        self.assertIn(target, labels, line)
            items = [l.split()[1] for l in text.splitlines() if l.startswith("item ") and l.split()[1] != "--gap"]
            self.assertEqual(sorted(items), sorted(labels - {"menu"}))

    def test_public_url_overrides_the_host_header(self):
        h = fixtures.Harness(public_url="http://10.0.2.2:8420")
        self.addCleanup(h.close)
        status, _, body = h.request("GET", "/boot.ipxe", headers={"Host": "127.0.0.1:1"})
        self.assertIn("set reg http://10.0.2.2:8420\n", body.decode())

    def test_build_script(self):
        status, text = self.get(f"/builds/box/{self.box[0]}.ipxe")
        base = f"http://registry.lan:8420/files/box/{self.box[0]}"
        self.assertEqual(text.splitlines(), [
            "#!ipxe",
            f"imgfetch --name kernel-x86_64 {base}/kernel-x86_64",
            f"imgfetch --name boot.json {base}/boot.json",
            f"imgfetch --name ramdisk {base}/ramdisk",
            f"chain --name bootx64.efi {base}/bootx64.efi"])
        self.assertNotIn("${", text)
        self.assertEqual(self.get(f"/builds/dev/{self.box[0]}.ipxe")[0], 404)
        self.assertEqual(self.get("/builds/box/nope.ipxe")[0], 404)
        self.assertEqual(self.get("/builds/box/..%2f..%2fkeys.ipxe")[0], 404)


class MenuUnitTests(unittest.TestCase):
    def test_no_builds_means_local_by_default(self):
        text = menu.boot_menu("http://r", "x", [], None, 5000)
        self.assertIn("choose --default local --timeout 5000 target || goto local", text)

    def test_labels_cannot_inject_script_lines(self):
        build = {"id": "evil\nboot", "channel": "dev\r\nshell", "size": 1}
        label = menu.label_for(build)
        self.assertNotIn("\n", label)
        self.assertNotIn("\r", label)

    def test_loader_is_required(self):
        build = {"id": "x", "channel": "dev", "manifest": {"files": {"ramdisk": {}}}}
        with self.assertRaises(ValueError):
            menu.build_script("http://r", build)

    def test_autoexec_variants(self):
        text = menu.autoexec("http://192.168.1.5:8420", "exit")
        self.assertIn("dhcp || goto fail", text)
        self.assertIn("chain http://192.168.1.5:8420/boot.ipxe?mac=${net0/mac} || goto fail", text)
        self.assertTrue(text.rstrip().endswith("exit"))
        self.assertTrue(menu.autoexec("http://r", "shell").rstrip().endswith("shell"))
        with self.assertRaises(ValueError):
            menu.autoexec("http://r", "reboot")

    def test_registry_url_drops_port_80(self):
        self.assertEqual(menu.registry_url("h", 80), "http://h")
        self.assertEqual(menu.registry_url("h", 8420), "http://h:8420")


if __name__ == "__main__":
    unittest.main()
