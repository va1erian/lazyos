#!/usr/bin/env python3
"""Tests for seeded layouts and ownership (issue #347). Run: python tools/mkdisk/test_seed.py.

There is no ``chown`` in the guest yet, so the formatter is the only place
ownership can be set; these tests decode the image independently of the encoder
and check owner, mode, the sticky bit, link counts and free counts.
"""

from __future__ import annotations

import io
import shutil
import subprocess
import sys
import tempfile
import unittest
from unittest import mock
from contextlib import redirect_stderr, redirect_stdout
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent.parent))
sys.path.insert(0, str(Path(__file__).resolve().parent))
from mkdisk import accounts, geometry, layout, volume  # noqa: E402
from mkdisk.__main__ import main as mkdisk_main, octal_mode  # noqa: E402
from test_mkdisk import MIB, Volume, format_bytes, kernel_open_errors  # noqa: E402

USER = accounts.Account("user", 1000, 1000, "/home/user", "sh")
BOB = accounts.Account("bob", 1001, 100, "/home/bob", "sh")
SHAPES = [(1 * MIB, 1024), (1 * MIB, 4096), (64 * MIB, 4096), (40 * MIB, 1024),
          (200 * MIB, 4096), (200 * MIB, 2048)]  # the last three span several groups
S_ISVTX = 0o1000


def demo_layout(**root) -> layout.Layout:
    """Two users plus a nested directory, so parent links and depth are covered."""
    seeded = layout.seeded(accounts=[USER, BOB], **root)
    nested = layout.DirSpec("/home/user/docs", 0o700, USER.uid, USER.gid)
    return layout.Layout(seeded.root_mode, seeded.root_uid, seeded.root_gid,
                         seeded.dirs + (nested,))


def walk(v: Volume) -> dict[str, int]:
    """Every directory path reachable from the root, mapped to its inode number."""
    found, pending = {"/": geometry.ROOT_INO}, [("/", geometry.ROOT_INO)]
    while pending:
        path, ino = pending.pop()
        for child, _, name, _ in v.entries(ino):
            if child in (0, ino) or name == b"..":
                continue
            child_path = path.rstrip("/") + "/" + name.decode()
            found[child_path] = child
            pending.append((child_path, child))
    return found


class SeededTreeTests(unittest.TestCase):
    def setUp(self) -> None:
        self.v = Volume(format_bytes(64 * MIB, layout=demo_layout()))
        self.tree = walk(self.v)

    def attributes(self, path: str) -> tuple[int, int, int]:
        node = self.v.inode(self.tree[path])
        return node["mode"], node["uid"], node["gid"]

    def test_owner_and_mode_round_trip(self) -> None:
        self.assertEqual(self.attributes("/home/user"), (0o040700, 1000, 1000))
        self.assertEqual(self.attributes("/home/bob"), (0o040700, 1001, 100))
        self.assertEqual(self.attributes("/home/user/docs"), (0o040700, 1000, 1000))
        self.assertEqual(self.attributes("/home"), (0o040755, 0, 0))

    def test_tmp_is_sticky_and_world_writable(self) -> None:
        mode, uid, gid = self.attributes("/tmp")
        self.assertEqual(mode & 0o7777, 0o1777)
        self.assertTrue(mode & S_ISVTX)
        self.assertEqual((uid, gid), (0, 0))

    def test_default_root_stays_root_owned_and_not_world_writable(self) -> None:
        self.assertEqual(self.attributes("/"), (0o040755, 0, 0))

    def test_custom_root_attributes(self) -> None:
        v = Volume(format_bytes(4 * MIB, layout=demo_layout(
            root_mode=0o1777, root_uid=1000, root_gid=1000)))
        root = v.inode(geometry.ROOT_INO)
        self.assertEqual((root["mode"], root["uid"], root["gid"]), (0o041777, 1000, 1000))

    def test_the_tree_is_exactly_what_was_asked_for(self) -> None:
        self.assertEqual(set(self.tree), {"/", "/lost+found", "/home", "/home/user",
                                          "/home/user/docs", "/home/bob", "/tmp"})
        self.assertEqual([e[2] for e in self.v.entries(geometry.ROOT_INO)],
                         [b".", b"..", b"lost+found", b"home", b"tmp"])

    def test_dotdot_and_link_counts(self) -> None:
        for path, ino in self.tree.items():
            entries = self.v.entries(ino)
            subdirs = [e for e in entries if e[2] not in (b".", b"..") and e[0]]
            parent = self.tree["/" if path.count("/") <= 1 else path.rsplit("/", 1)[0]]
            self.assertEqual(entries[0][0], ino, path)
            self.assertEqual(entries[1][0], parent if path != "/" else ino, path)
            self.assertEqual(self.v.inode(ino)["links"], 2 + len(subdirs), path)

    def test_seeded_inodes_follow_lost_found(self) -> None:
        self.assertEqual([self.tree[p] for p in ("/home", "/home/user", "/home/bob")],
                         [12, 13, 14])


class SeededConsistencyTests(unittest.TestCase):
    """Bitmaps, free counts and used-dir counts stay right for every shape."""

    def check(self, size: int, block_size: int) -> None:
        plan = demo_layout()
        v = Volume(format_bytes(size, block_size, layout=plan))
        tree = walk(v)
        owned = set()
        for group in range(v.groups):
            geo = geometry.plan(size, block_size).group(group)
            owned.update(range(geo.start, geo.first_free))
        for ino in tree.values():
            owned.update(b for b in v.inode(ino)["blocks"] if b)
        used_inodes = set(range(1, geometry.FIRST_INO + 1)) | set(tree.values())
        totals = [0, 0]
        for group in range(v.groups):
            gd = v.descriptor(group)
            start = v.first_data + group * v.bpg
            in_group = min(v.bpg, v.blocks_count - start)
            bits = v.bitmap_bits(gd["block_bitmap"], v.bpg)
            for bit, used in enumerate(bits):
                self.assertEqual(used, bit >= in_group or start + bit in owned, f"g{group} b{bit}")
            ibits = v.bitmap_bits(gd["inode_bitmap"], v.block_size * 8)
            for bit in range(v.ipg):
                self.assertEqual(ibits[bit], group * v.ipg + bit + 1 in used_inodes)
            self.assertEqual(gd["free_blocks"], bits[:in_group].count(False))
            self.assertEqual(gd["free_inodes"], ibits[:v.ipg].count(False))
            self.assertEqual(gd["used_dirs"], len(tree) if group == 0 else 0)
            totals[0] += gd["free_blocks"]
            totals[1] += gd["free_inodes"]
        self.assertEqual(totals, [v.free_blocks, v.free_inodes])
        self.assertEqual(kernel_open_errors(format_bytes(size, block_size, layout=plan)), [])

    def test_all_shapes(self) -> None:
        for size, block_size in SHAPES:
            with self.subTest(size=size, block_size=block_size):
                self.check(size, block_size)

    def test_seeding_costs_one_block_and_one_inode_per_directory(self) -> None:
        bare = Volume(format_bytes(64 * MIB))
        seeded = Volume(format_bytes(64 * MIB, layout=demo_layout()))
        extra = len(demo_layout().dirs)
        self.assertEqual(bare.free_blocks - seeded.free_blocks, extra)
        self.assertEqual(bare.free_inodes - seeded.free_inodes, extra)


class LayoutRuleTests(unittest.TestCase):
    def test_rejects_bad_directories(self) -> None:
        bad = [
            [layout.DirSpec("/a/b")],                     # parent never created
            [layout.DirSpec("/a"), layout.DirSpec("/a")],  # duplicate
            [layout.DirSpec("relative")],
            [layout.DirSpec("/")],
            [layout.DirSpec("/trailing/")],
            [layout.DirSpec("/lost+found")],               # the formatter's own
            [layout.DirSpec("/a"), layout.DirSpec("/a/..")],
            [layout.DirSpec("/" + "x" * 256)],
        ]
        for dirs in bad:
            with self.subTest(dirs=dirs), self.assertRaises(ValueError):
                layout.Layout(dirs=tuple(dirs))

    def test_rejects_unrepresentable_attributes(self) -> None:
        for kwargs in ({"mode": 0o10000}, {"mode": -1}, {"uid": 65536}, {"gid": -1}):
            with self.subTest(**kwargs), self.assertRaises(ValueError):
                layout.DirSpec("/x", **kwargs)
        with self.assertRaises(ValueError):
            layout.Layout(root_uid=70000)

    def test_too_many_directories_for_the_volume_is_refused(self) -> None:
        many = tuple(layout.DirSpec(f"/d{i}") for i in range(100))
        with self.assertRaises(ValueError):
            format_bytes(1 * MIB, 4096, layout=layout.Layout(dirs=many))  # 32 inodes per group

    def test_too_many_root_entries_for_one_block_are_refused(self) -> None:
        many = tuple(layout.DirSpec(f"/{'n' * 60}{i}") for i in range(20))
        with self.assertRaises(ValueError):
            format_bytes(8 * MIB, 1024, layout=layout.Layout(dirs=many))

    def test_home_dirs_only_for_accounts_homed_under_home(self) -> None:
        nobody = accounts.Account("nobody", 65534, 65534, "/", "sh")
        odd = accounts.Account("svc", 5, 5, "/var/svc", "sh")
        self.assertEqual([d.path for d in layout.home_dirs([nobody, USER, odd])],
                         ["/home/user"])


class HomeVolumeLayoutTests(unittest.TestCase):
    """`--home-volume`: <user>/ at the root, the same owners and modes as /home/<user>."""

    def test_users_sit_at_the_volume_root(self) -> None:
        plan = layout.home_volume(accounts=[USER, BOB])
        self.assertEqual([(d.path, d.mode, d.uid, d.gid) for d in plan.dirs],
                         [("/user", 0o700, 1000, 1000), ("/bob", 0o700, 1001, 100)])

    def test_no_home_or_tmp_directory(self) -> None:
        paths = {d.path for d in layout.home_volume().dirs}
        self.assertFalse({"/home", "/tmp"} & paths)

    def test_same_owner_and_mode_as_the_seeded_homes(self) -> None:
        seeded = {d.path.removeprefix("/home"): (d.mode, d.uid, d.gid)
                  for d in layout.seeded().dirs if d.path.startswith("/home/")}
        home = {d.path: (d.mode, d.uid, d.gid) for d in layout.home_volume().dirs}
        self.assertEqual(home, seeded)

    def test_services_and_foreign_homes_are_skipped(self) -> None:
        nobody = accounts.Account("nobody", 65534, 65534, "/", "sh")
        odd = accounts.Account("svc", 5, 5, "/var/svc", "sh")
        self.assertEqual(layout.home_volume(accounts=[nobody, USER, odd]).dirs[0].path, "/user")
        self.assertEqual(len(layout.home_volume(accounts=[nobody, USER, odd]).dirs), 1)

    def test_formatted_tree_is_exactly_lost_found_plus_the_users(self) -> None:
        plan = layout.home_volume(accounts=[USER, BOB])
        v = Volume(format_bytes(8 * MIB, layout=plan))
        tree = walk(v)
        self.assertEqual(set(tree), {"/", "/lost+found", "/user", "/bob"})
        node = v.inode(tree["/bob"])
        self.assertEqual((node["mode"], node["uid"], node["gid"]), (0o040700, 1001, 100))


class DemoAccountsTests(unittest.TestCase):
    """The seed reads the one account file the build installs (issue #508)."""

    def test_the_build_installs_the_same_file(self) -> None:
        # No second copy: build.rs embeds this very file as /system/etc/passwd.
        build = accounts.BUILD_SCRIPT.read_text(encoding="utf-8")
        self.assertIn('include_bytes!("build_support/passwd")', build)
        self.assertNotIn("BUILTIN", (accounts.ROOT / "user" / "src" / "bin" / "accountsd.rs")
                         .read_text(encoding="utf-8"))

    def test_demo_accounts_are_admin_and_user(self) -> None:
        self.assertEqual(
            [(a.name, a.uid, a.gid, a.home) for a in accounts.demo_accounts()],
            [("admin", 0, 0, "/home/admin"), ("user", 1000, 1000, "/home/user")])

    def test_home_volume_owners_match_passwd(self) -> None:
        # #447: the home volume's owners are the account file's, nothing else.
        dirs = {d.path: (d.mode, d.uid, d.gid) for d in layout.home_volume().dirs}
        self.assertEqual(dirs, {f"/{a.name}": (0o700, a.uid, a.gid)
                                for a in accounts.demo_accounts()})

    def test_default_seed_has_a_home_per_account(self) -> None:
        homes = {d.path: d for d in layout.seeded().dirs}
        for account in accounts.demo_accounts():
            if account.home.startswith("/home/"):
                spec = homes[account.home]
                self.assertEqual((spec.uid, spec.gid), (account.uid, account.gid))

    def test_parser(self) -> None:
        text = "# accounts\n\na:1:2:s:/home/a:sh\nb:3:4:s:/home/b:sh\n"
        self.assertEqual([a.name for a in accounts.parse_passwd(text)], ["a", "b"])
        with self.assertRaises(ValueError):
            accounts.parse_passwd("only:three:fields")
        with self.assertRaises(ValueError):
            accounts.parse_passwd("a:-1:2:s:/home/a:sh")

    def test_missing_file_is_reported(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            with self.assertRaises(ValueError):
                accounts.demo_accounts(Path(tmp) / "passwd")


class CommandLineTests(unittest.TestCase):
    def setUp(self) -> None:
        self.tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self.tmp.cleanup)
        self.path = Path(self.tmp.name) / "data.img"

    def run_main(self, *argv: str) -> tuple[int, str, str]:
        out, err = io.StringIO(), io.StringIO()
        with redirect_stdout(out), redirect_stderr(err):
            code = mkdisk_main([str(self.path), "--size", "8M", *argv])
        return code, out.getvalue(), err.getvalue()

    def test_octal_mode(self) -> None:
        self.assertEqual((octal_mode("755"), octal_mode("0o1777"), octal_mode("0700")),
                         (0o755, 0o1777, 0o700))
        with self.assertRaises(Exception):
            octal_mode("9")

    def test_default_image_is_seeded_with_root_owned_0755_root(self) -> None:
        code, out, _ = self.run_main()
        self.assertEqual(code, 0)
        self.assertIn("/home/user (mode 0700, uid 1000, gid 1000)", out)
        self.assertIn("/home/admin (mode 0700, uid 0, gid 0)", out)
        v = Volume(self.path.read_bytes())
        self.assertEqual(v.inode(geometry.ROOT_INO)["mode"], 0o040755)
        self.assertIn("/tmp", walk(v))

    def test_root_flags_and_no_seed(self) -> None:
        code, _, _ = self.run_main("--no-seed", "--root-mode", "1777", "--root-uid", "1000",
                                   "--root-gid", "1000")
        self.assertEqual(code, 0)
        v = Volume(self.path.read_bytes())
        root = v.inode(geometry.ROOT_INO)
        self.assertEqual((root["mode"], root["uid"], root["gid"]), (0o041777, 1000, 1000))
        self.assertEqual(set(walk(v)), {"/", "/lost+found"})

    def test_home_volume_flag(self) -> None:
        code, out, _ = self.run_main("--home-volume")
        self.assertEqual(code, 0)
        self.assertIn("label 'lazyhome'", out)
        self.assertIn("/admin (mode 0700, uid 0, gid 0)", out)
        self.assertIn("/user (mode 0700, uid 1000, gid 1000)", out)
        image = self.path.read_bytes()
        self.assertEqual(image[1024 + 120:1024 + 128], b"lazyhome")  # s_volume_name
        self.assertEqual(set(walk(Volume(image))), {"/", "/lost+found", "/admin", "/user"})

    def test_home_volume_label_can_be_overridden(self) -> None:
        code, out, _ = self.run_main("--home-volume", "--label", "other")
        self.assertEqual(code, 0)
        self.assertIn("label 'other'", out)

    def test_empty_label_is_an_error_not_a_silent_default(self) -> None:
        for label in ("", "  "):
            code, _, err = self.run_main("--label", label)
            self.assertEqual(code, 1)
            self.assertIn("--label must not be empty", err)
            self.assertFalse(self.path.exists())

    def test_default_label_without_home_volume(self) -> None:
        code, out, _ = self.run_main()
        self.assertEqual(code, 0)
        self.assertIn("label 'lazyos-data'", out)

    def test_bad_ids_are_a_clean_error(self) -> None:
        code, _, err = self.run_main("--root-uid", "99999")
        self.assertEqual(code, 1)
        self.assertIn("uid", err)
        self.assertFalse(self.path.exists())

    def test_format_image_defaults_to_seeded_and_can_be_bare(self) -> None:
        volume.format_image(self.path, 8 * MIB)
        self.assertIn("/home/user", walk(Volume(self.path.read_bytes())))
        volume.format_image(self.path, 8 * MIB, layout=layout.EMPTY)
        self.assertEqual(set(walk(Volume(self.path.read_bytes()))), {"/", "/lost+found"})


@unittest.skipUnless(shutil.which("e2fsck"), "e2fsck not installed on this host")
class E2fsckSeededTests(unittest.TestCase):
    def test_e2fsck_reports_clean_for_seeded_images(self) -> None:
        for size, block_size in SHAPES:
            with self.subTest(size=size, block_size=block_size), \
                    tempfile.TemporaryDirectory() as tmp:
                path = Path(tmp) / "data.img"
                path.write_bytes(format_bytes(size, block_size, layout=demo_layout()))
                done = subprocess.run(["e2fsck", "-fn", str(path)], capture_output=True, text=True)
                self.assertEqual(done.returncode, 0, done.stdout + done.stderr)

    def test_e2fsck_reports_clean_for_home_volumes(self) -> None:
        for size, block_size in SHAPES:
            with self.subTest(size=size, block_size=block_size), \
                    tempfile.TemporaryDirectory() as tmp:
                path = Path(tmp) / "home.img"
                plan = layout.home_volume(accounts=[USER, BOB])
                path.write_bytes(format_bytes(size, block_size, layout=plan, label="lazyhome"))
                done = subprocess.run(["e2fsck", "-fn", str(path)], capture_output=True, text=True)
                self.assertEqual(done.returncode, 0, done.stdout + done.stderr)


class RunDemoErrorTests(unittest.TestCase):
    """`run_demo` must report a broken plan, not die with a traceback."""

    def test_unplannable_reset_is_reported_and_leaves_the_volume(self) -> None:
        sys.path.insert(0, str(Path(__file__).resolve().parent.parent))
        import run_demo  # noqa: E402

        def unreadable(*_args, **_kwargs):
            raise OSError("accounts source missing")

        with tempfile.TemporaryDirectory() as tmp:
            path = Path(tmp) / "data.img"
            path.write_bytes(b"precious")
            err = io.StringIO()
            with mock.patch.object(run_demo.mkdisk, "seeded", unreadable), redirect_stderr(err):
                ok = run_demo.prepare_data_disk(path, reset=True, assume_yes=True)
            self.assertFalse(ok)
            self.assertIn("accounts source missing", err.getvalue())
            self.assertEqual(path.read_bytes(), b"precious")


if __name__ == "__main__":
    unittest.main()
