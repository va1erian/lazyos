"""Manifest-first uploads and mandatory bound fields (docs/netboot-plan.md section 4, decision 4)."""

import unittest

import auth
import ed25519
import store as store_mod
import fixtures
import keys
from fixtures import build_id


class UploadCase(unittest.TestCase):
    def setUp(self):
        self.h = fixtures.Harness()
        self.addCleanup(self.h.close)
        self.bid = build_id(1)
        self.dir = self.h.tmp / "u"
        self.build = fixtures.make_build(self.dir, self.h.seed, self.bid)

    def put(self, name, data=None, bid=None):
        data = (self.dir / name).read_bytes() if data is None else data
        return self.h.json("PUT", f"/api/builds/dev/{bid or self.bid}/{name}", data)

    def incoming(self):
        return sorted(p.name for p in (self.h.root / "builds").rglob("*.incoming"))

    def open_upload(self):
        self.assertEqual(self.put("manifest.json")[0], 201)
        self.assertEqual(self.put("manifest.sig")[0], 201)


class ManifestFirstTests(UploadCase):
    def test_a_file_before_the_manifest_is_403_and_stores_nothing(self):
        status, doc = self.put("ramdisk")
        self.assertEqual(status, 403)
        self.assertIn("first", doc["error"])
        self.assertEqual(self.incoming(), [])          # not even the directory is made

    def test_a_file_after_the_manifest_alone_is_403(self):
        self.assertEqual(self.put("manifest.json")[0], 201)
        self.assertEqual(self.put("kernel-x86_64")[0], 403)
        self.assertEqual(self.put("manifest.json")[0], 201)

    def test_the_two_manifest_files_may_come_in_either_order(self):
        self.assertEqual(self.put("manifest.sig")[0], 201)
        self.assertEqual(self.put("manifest.json")[0], 201)
        self.assertEqual(self.put("ramdisk")[0], 201)

    def test_a_name_the_manifest_does_not_list_is_400(self):
        self.open_upload()
        status, doc = self.put("extra.bin", b"x")
        self.assertEqual(status, 400)
        self.assertIn("not a file of the manifest", doc["error"])
        self.assertEqual(self.put("ramdisk")[0], 201)

    def test_a_size_other_than_the_manifests_is_413(self):
        self.open_upload()
        data = self.build["files"]["ramdisk"]
        for bad in (data + b"x", data[:-1], b"", data * 2):
            status, doc = self.put("ramdisk", bad)
            self.assertEqual(status, 413, len(bad))
        self.assertEqual(self.put("ramdisk")[0], 201)
        files = [p.name for p in (self.h.root / "builds" / "dev" / f"{self.bid}.incoming").iterdir()]
        self.assertEqual(sorted(files), ["manifest.json", "manifest.sig", "ramdisk"])

    def test_oversized_manifest_files_are_413(self):
        self.assertEqual(self.put("manifest.json", b"x" * ((1 << 20) + 1))[0], 413)
        self.assertEqual(self.put("manifest.sig", b"x" * 5000)[0], 413)
        self.assertEqual(self.incoming(), [])

    def test_a_manifest_that_does_not_verify_leaves_nothing_behind(self):
        stranger = ed25519.generate_seed()
        raw = (self.dir / "manifest.json").read_bytes()
        self.assertEqual(self.put("manifest.json")[0], 201)
        status, _ = self.put("manifest.sig", keys.format_signature(ed25519.sign(stranger, raw)).encode())
        self.assertEqual(status, 403)
        self.assertEqual(self.incoming(), [])
        self.assertEqual(self.put("ramdisk")[0], 403)   # and the upload is not open

    def test_replacing_the_manifest_by_one_that_does_not_verify_deletes_the_upload(self):
        self.open_upload()
        self.assertEqual(self.put("ramdisk")[0], 201)
        status, _ = self.put("manifest.json", raw_changed(self.build["manifest"]))
        self.assertEqual(status, 403)
        self.assertEqual(self.incoming(), [])

    def test_overwriting_a_file_already_present_is_allowed_and_commit_rehashes(self):
        self.open_upload()
        for name in self.build["files"]:
            self.assertEqual(self.put(name)[0], 201)
        self.assertEqual(self.put("kernel-x86_64", b"y" * len(self.build["files"]["kernel-x86_64"]))[0], 201)
        status, doc = self.h.signed("POST", f"/api/builds/dev/{self.bid}/commit", "commit")
        self.assertEqual(status, 422)                   # same size, other bytes: refused at the commit
        self.assertEqual((self.incoming(), self.h.server.store.build_ids("dev")), ([], []))

    def test_a_full_upload_in_order_commits(self):
        self.open_upload()
        for name in self.build["files"]:
            self.assertEqual(self.put(name)[0], 201)
        self.assertEqual(self.h.signed("POST", f"/api/builds/dev/{self.bid}/commit", "commit")[0], 201)

    def test_the_manifest_must_name_the_upload_id(self):
        self.assertEqual(self.put("manifest.json", bid=build_id(2))[0], 201)
        self.assertEqual(self.put("manifest.sig", bid=build_id(2))[0], 400)
        self.assertEqual(self.incoming(), [])

    def test_a_public_manifest_cannot_reopen_an_existing_build(self):
        self.open_upload()
        for name in self.build["files"]:
            self.put(name)
        self.h.signed("POST", f"/api/builds/dev/{self.bid}/commit", "commit")
        listing = self.h.json("GET", "/api/builds")[1]["channels"][0]["builds"][0]
        status, _ = self.put("manifest.json", listing["manifest_text"].encode())
        self.assertEqual(status, 409)


class CommitRaceTests(UploadCase):
    """The commit claims the upload (a private rename) before it hashes it."""

    def setUp(self):
        super().setUp()
        self.store = self.h.server.store
        self.final = self.h.root / "builds" / "dev" / self.bid
        self.open_upload()
        for name in self.build["files"]:
            self.assertEqual(self.put(name)[0], 201)

    def intrude_during_verify(self, intrusion):
        original = self.store._verify_incoming

        def verify_then_intrude(directory):
            result = original(directory)
            intrusion(directory)
            return result

        self.store._verify_incoming = verify_then_intrude
        self.addCleanup(setattr, self.store, "_verify_incoming", original)

    def test_a_file_written_after_the_claim_does_not_reach_the_build(self):
        seen = {}

        def intrusion(claimed):
            incoming = self.store.incoming_dir("dev", self.bid)
            self.assertFalse(incoming.exists())               # the upload moved to a private name
            self.assertIn(".verifying-", claimed.name)
            seen["late_put"] = self.put("kernel-x86_64", b"EVIL" * 100)[0]
            incoming.mkdir()                                    # the worst case: a directory appears
            (incoming / "evil.bin").write_bytes(b"evil")

        self.intrude_during_verify(intrusion)
        status, doc = self.h.signed("POST", f"/api/builds/dev/{self.bid}/commit", "commit")
        self.assertEqual(status, 201, doc)
        self.assertEqual(seen["late_put"], 403)                # manifest-first: no incoming, no file
        self.assertEqual(sorted(p.name for p in self.final.iterdir()),
                         sorted([*self.build["files"], "manifest.json", "manifest.sig"]))
        self.assertEqual((self.final / "kernel-x86_64").read_bytes(), self.build["files"]["kernel-x86_64"])
        self.assertEqual([p.name for p in (self.h.root / "builds" / "dev").glob("*.verifying-*")], [])

    def test_a_second_commit_while_the_upload_is_claimed_is_404(self):
        seen = {}

        def intrusion(_claimed):
            with self.assertRaises(store_mod.StoreError) as ctx:
                self.store.commit("dev", self.bid)
            seen["status"], seen["message"] = ctx.exception.status, ctx.exception.message

        self.intrude_during_verify(intrusion)
        self.assertEqual(self.h.signed("POST", f"/api/builds/dev/{self.bid}/commit", "commit")[0], 201)
        self.assertEqual(seen["status"], 404)
        self.assertIn("claimed", seen["message"])

    def test_a_refused_commit_removes_the_claimed_directory(self):
        (self.h.root / "builds" / "dev" / f"{self.bid}.incoming" / "ramdisk").write_bytes(b"z" * 200_000)
        status, _ = self.h.signed("POST", f"/api/builds/dev/{self.bid}/commit", "commit")
        self.assertEqual(status, 422)
        self.assertEqual(list((self.h.root / "builds" / "dev").iterdir()), [])


def raw_changed(raw: bytes) -> bytes:
    return raw.replace(b"devbox", b"evilbx")


class BoundFieldTests(unittest.TestCase):
    """A signed body must carry the fields that name its target, and they must match the path."""

    def setUp(self):
        self.h = fixtures.Harness(machines={"box": {"mac": "aa:bb:cc:dd:ee:ff"}})
        self.addCleanup(self.h.close)
        self.bid = build_id(1)
        self.h.push("b", self.bid)

    def signed(self, method, path, action, fields):
        return self.h.signed(method, path, action, fields, bind=False)[0]

    def test_commit_without_id_or_channel_is_refused(self):
        path = f"/api/builds/dev/{self.bid}/commit"
        self.assertEqual(self.signed("POST", path, "commit", {"channel": "dev"}), 400)
        self.assertEqual(self.signed("POST", path, "commit", {"id": self.bid}), 400)
        self.assertEqual(self.signed("POST", path, "commit", {}), 400)
        self.assertEqual(self.signed("POST", path, "commit", {"channel": "dev", "id": build_id(9)}), 400)

    def test_delete_needs_both(self):
        path = f"/api/builds/dev/{self.bid}"
        self.assertEqual(self.signed("DELETE", path, "delete", {"channel": "dev"}), 400)
        self.assertEqual(self.signed("DELETE", path, "delete", {"id": self.bid}), 400)
        self.assertEqual(self.h.server.store.build_ids("dev"), [self.bid])
        self.assertEqual(self.signed("DELETE", path, "delete", {"channel": "dev", "id": self.bid}), 200)

    def test_channel_actions_need_the_channel(self):
        for action, fields in (("pin", {"id": self.bid}), ("unpin", {"id": self.bid}), ("prune", {"keep": 5})):
            self.assertEqual(self.signed("POST", f"/api/channels/dev/{action}", action, fields), 400, action)
            self.assertEqual(self.signed("POST", f"/api/channels/dev/{action}", action,
                                         {**fields, "channel": "box"}), 400, action)
            self.assertEqual(self.signed("POST", f"/api/channels/dev/{action}", action,
                                         {**fields, "channel": "dev"}), 200, action)

    def test_machine_actions_need_the_machine(self):
        for action, fields in (("next", {"id": self.bid}), ("booting", {"id": self.bid}),
                               ("booted", {"id": self.bid})):
            self.assertEqual(self.signed("POST", f"/api/machines/box/{action}", action, fields), 400, action)
            self.assertEqual(self.signed("POST", f"/api/machines/box/{action}", action,
                                         {**fields, "machine": "laptop"}), 400, action)
            self.assertEqual(self.signed("POST", f"/api/machines/box/{action}", action,
                                         {**fields, "machine": "box"}), 200, action)
        self.assertEqual(self.signed("POST", "/api/machines/nuc/add", "add",
                                     {"mac": "00:00:00:00:00:09"}), 400)

    def test_nothing_was_changed_by_the_refusals(self):
        self.signed("POST", f"/api/channels/dev/pin", "pin", {"id": self.bid})
        self.assertEqual(self.h.server.store.settings("dev")[1], [])


if __name__ == "__main__":
    unittest.main()
