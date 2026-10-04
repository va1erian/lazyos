#!/usr/bin/env python3
"""Tests for run_demo.py's home volume, reset flags and QEMU disk order (no QEMU, no build).

Run: python tools/test_run_demo.py
"""

from __future__ import annotations

import io
import os
import re
import sys
import tempfile
import unittest
from contextlib import redirect_stderr, redirect_stdout
from pathlib import Path
from unittest import mock

sys.path.insert(0, str(Path(__file__).resolve().parent))
import run_demo  # noqa: E402

LABEL_OFFSET = 1024 + 120  # ext2 s_volume_name
# No drive letter: on Linux `os.pathsep` is `:`, which would split `C:\...`.
SAMPLES = [os.path.join(os.sep, "lr", "hello"), os.path.join(os.sep, "lr", "calc")]
#: The LazyOS-only samples every LazyRAD image embeds (`catalog.LAZYOS_LAZYRAD_SAMPLES`).
LAZYOS_SAMPLES = ["lazyrad-os/samples/messenger", "lazyrad-os/samples/modplayer"]


class PrepareHomeDiskTests(unittest.TestCase):
    def setUp(self) -> None:
        tmp = tempfile.TemporaryDirectory()
        self.addCleanup(tmp.cleanup)
        self.path = Path(tmp.name) / "home.img"

    def test_created_when_missing_with_the_home_layout(self) -> None:
        with redirect_stdout(io.StringIO()):
            self.assertTrue(run_demo.prepare_home_disk(self.path, False, False))
        image = self.path.read_bytes()
        self.assertEqual(image[LABEL_OFFSET:LABEL_OFFSET + 8], b"lazyhome")
        self.assertIn(b"admin", image)
        self.assertNotIn(b"tmp\0", image[:1 << 20])

    def test_existing_volume_is_never_regenerated_implicitly(self) -> None:
        self.path.write_bytes(b"precious")
        self.assertTrue(run_demo.prepare_home_disk(self.path, False, False))
        self.assertEqual(self.path.read_bytes(), b"precious")

    def test_existing_volume_does_not_build_the_plan(self) -> None:
        self.path.write_bytes(b"precious")
        plan = mock.Mock(side_effect=AssertionError("planned for nothing"))
        self.assertTrue(run_demo.prepare_volume("home disk", self.path, False, False, plan,
                                                "lazyhome"))
        plan.assert_not_called()

    def test_reset_asks_and_a_decline_leaves_the_volume(self) -> None:
        self.path.write_bytes(b"precious")
        with mock.patch.object(run_demo, "confirm", return_value=False) as ask, \
                redirect_stderr(io.StringIO()):
            self.assertFalse(run_demo.prepare_home_disk(self.path, True, False))
        self.assertIn("/user (mode 0700", ask.call_args.args[0])
        self.assertEqual(self.path.read_bytes(), b"precious")

    def test_reset_with_yes_skips_the_question(self) -> None:
        self.path.write_bytes(b"precious")
        with mock.patch.object(run_demo, "confirm") as ask, redirect_stdout(io.StringIO()):
            self.assertTrue(run_demo.prepare_home_disk(self.path, True, True))
        ask.assert_not_called()
        self.assertEqual(self.path.read_bytes()[LABEL_OFFSET:LABEL_OFFSET + 8], b"lazyhome")


class MainTests(unittest.TestCase):
    """`main` with the build, QEMU and the filesystem faked out."""

    def setUp(self) -> None:
        tmp = tempfile.TemporaryDirectory()
        self.addCleanup(tmp.cleanup)
        self.dir = Path(tmp.name)
        self.image = self.dir / "lazyos.img"
        self.image.write_bytes(b"\0" * 512)
        self.home = self.dir / "home.img"
        self.builds: list[dict] = []
        self.commands: list[list[str]] = []

    def run_main(self, *argv: str) -> tuple[int, list[str]]:
        launched: list[str] = []

        def fake_build(command, cwd=None, env=None, **_):
            self.commands.append([str(part) for part in command])
            if env is not None:
                self.builds.append(env)
            return mock.Mock(returncode=0)

        with mock.patch.object(run_demo.busybox, "ensure_busybox"), \
                mock.patch.object(run_demo, "build_rhai"), \
                mock.patch.object(run_demo, "find_qemu", return_value="qemu"), \
                mock.patch.object(run_demo, "accel_args", return_value=[]), \
                mock.patch.object(run_demo.subprocess, "run", fake_build), \
                mock.patch.object(run_demo.subprocess, "call",
                                  lambda command: launched.extend(command) or 0), \
                redirect_stdout(io.StringIO()), redirect_stderr(io.StringIO()):
            code = run_demo.main(["--image", str(self.image), "--home-disk", str(self.home),
                                  *argv])
        return code, launched

    def test_home_disk_is_created_and_attached_second(self) -> None:
        code, command = self.run_main("--no-build")
        self.assertEqual(code, 0)
        self.assertTrue(self.home.is_file())
        self.assertIn("virtio-blk-pci,drive=home", command)
        self.assertLess(command.index("virtio-blk-pci,drive=boot"),
                        command.index("virtio-blk-pci,drive=home"))

    def test_data_disk_is_not_attached_by_default(self) -> None:
        _, command = self.run_main("--no-build")
        self.assertFalse([arg for arg in command if "drive=data" in arg])

    def test_data_disk_still_works_and_precedes_home(self) -> None:
        data = self.dir / "data.img"
        code, command = self.run_main("--no-build", "--data-disk", str(data))
        self.assertEqual(code, 0)
        self.assertTrue(data.is_file())
        self.assertLess(command.index("virtio-blk-pci,drive=data"),
                        command.index("virtio-blk-pci,drive=home"))

    def test_no_home_disk_boots_with_the_boot_disk_only(self) -> None:
        code, command = self.run_main("--no-build", "--no-home-disk")
        self.assertEqual(code, 0)
        self.assertFalse(self.home.exists())
        self.assertEqual(sum("virtio-blk-pci" in arg for arg in command), 1)

    def test_reset_home_without_a_tty_declines(self) -> None:
        self.home.write_bytes(b"precious")
        code, _ = self.run_main("--no-build", "--reset-home")  # stdin is not a TTY under test
        self.assertEqual(code, 1)
        self.assertEqual(self.home.read_bytes(), b"precious")

    def test_reset_home_with_yes_reformats(self) -> None:
        self.home.write_bytes(b"precious")
        code, _ = self.run_main("--no-build", "--reset-home", "--yes")
        self.assertEqual(code, 0)
        self.assertEqual(self.home.read_bytes()[LABEL_OFFSET:LABEL_OFFSET + 8], b"lazyhome")

    def test_reset_os_sets_the_build_variable(self) -> None:
        code, _ = self.run_main("--reset-os", "--yes")
        self.assertEqual(code, 0)
        self.assertEqual(self.builds[-1].get("LAZYOS_RESET_OS"), "1")

    def test_reset_os_without_a_tty_declines_before_building(self) -> None:
        code, _ = self.run_main("--reset-os")  # stdin is not a TTY under test
        self.assertEqual(code, 1)
        self.assertEqual(self.builds, [])

    def test_reset_os_asks_and_a_no_stops_the_build(self) -> None:
        with mock.patch.object(run_demo, "confirm", return_value=False) as ask:
            code, _ = self.run_main("--reset-os")
        self.assertEqual(code, 1)
        self.assertEqual(self.builds, [])
        self.assertIn("OS volume", ask.call_args.args[0])

    def test_reset_os_with_a_yes_answer_builds(self) -> None:
        with mock.patch.object(run_demo, "confirm", return_value=True):
            code, _ = self.run_main("--reset-os")
        self.assertEqual(code, 0)
        self.assertEqual(self.builds[-1].get("LAZYOS_RESET_OS"), "1")

    def test_reset_os_with_no_existing_image_has_nothing_to_confirm(self) -> None:
        self.image.unlink()
        with mock.patch.object(run_demo, "confirm") as ask:
            self.run_main("--reset-os")
        ask.assert_not_called()
        self.assertEqual(self.builds[-1].get("LAZYOS_RESET_OS"), "1")

    def test_build_leaves_reset_os_unset_by_default(self) -> None:
        with mock.patch.dict(os.environ, {}, clear=False):
            os.environ.pop("LAZYOS_RESET_OS", None)
            self.run_main()
        self.assertNotIn("LAZYOS_RESET_OS", self.builds[-1])

    def test_a_desktop_build_packages_the_core_apps_first(self) -> None:
        # Issue #509: the desktop apps ship as core packages, so the image
        # build needs `target/pkg/core` from the apps just built.
        with mock.patch.object(run_demo, "build_xui_shell", return_value=True):
            code, _ = self.run_main("--desktop")
        self.assertEqual(code, 0)
        scripts = [Path(command[1]).name for command in self.commands if len(command) > 1]
        self.assertIn("core_packages.py", scripts)
        packaged = scripts.index("core_packages.py")
        cargo = next(i for i, command in enumerate(self.commands) if "cargo" in command[0])
        self.assertLess(packaged, cargo)
        self.assertEqual(self.builds[-1].get("LAZYOS_DESKTOP"), "1")

    def test_a_desktop_build_builds_missing_desktop_apps_first(self) -> None:
        # A `target/xui` from before LazyWriter (issue #533) lacks
        # xui-writer.elf, which the desktop image build requires.
        with mock.patch.object(run_demo, "build_xui_shell", return_value=True), \
                mock.patch.object(run_demo, "DESKTOP_ELFS", [self.dir / "xui-writer.elf"]), \
                mock.patch.object(run_demo, "build_xui_apps", return_value=True) as built:
            code, _ = self.run_main("--desktop")
        self.assertEqual(code, 0)
        built.assert_called_once()
        (self.dir / "xui-writer.elf").write_bytes(b"\x7fELF")
        with mock.patch.object(run_demo, "build_xui_shell", return_value=True), \
                mock.patch.object(run_demo, "DESKTOP_ELFS", [self.dir / "xui-writer.elf"]), \
                mock.patch.object(run_demo, "build_xui_apps", return_value=True) as built:
            code, _ = self.run_main("--desktop")
        self.assertEqual(code, 0)
        built.assert_not_called()

    def test_the_desktop_apps_match_the_image_build(self) -> None:
        # `build_support/xui_embed.rs` fails a desktop build without any of
        # DESKTOP_XUI_APPS and DOCUMENT_XUI_APPS; run_demo checks the same set.
        source = (run_demo.ROOT / "build_support" / "xui_embed.rs").read_text(encoding="utf-8")
        wanted = []
        for name in ("DESKTOP_XUI_APPS", "DOCUMENT_XUI_APPS"):
            block = re.search(rf"const {name}: &\[&str\] = &\[(.*?)\];", source, re.S)
            self.assertIsNotNone(block, name)
            wanted += re.findall(r'"([^"]+\.elf)"', block.group(1))
        self.assertIn("xui-writer.elf", wanted)
        self.assertEqual(sorted(path.name for path in run_demo.DESKTOP_ELFS), sorted(wanted))

    def test_a_console_build_does_not_package_apps(self) -> None:
        code, _ = self.run_main()
        self.assertEqual(code, 0)
        self.assertFalse(any("core_packages.py" in " ".join(c) for c in self.commands))

    def test_lazyrad_samples_are_passed_to_the_build_and_imply_lazyrad(self) -> None:
        with mock.patch.object(run_demo, "build_lazyrad", return_value=True) as built, \
                mock.patch.object(run_demo, "build_xui_shell", return_value=True), \
                mock.patch.object(run_demo, "build_core_packages", return_value=True):
            code, _ = self.run_main("--lazyrad-samples", os.pathsep.join(SAMPLES))
        self.assertEqual(code, 0)
        built.assert_called_once()
        self.assertEqual(self.builds[-1].get("LAZYOS_LAZYRAD"), "1")
        # The caller's samples first, then the LazyOS-only ones.
        self.assertEqual(self.builds[-1].get("LAZYRAD_SAMPLES", "").split(os.pathsep),
                         SAMPLES + LAZYOS_SAMPLES)

    def test_lazyrad_alone_embeds_only_the_lazyos_samples(self) -> None:
        with mock.patch.dict(os.environ, {}, clear=False), \
                mock.patch.object(run_demo, "build_xui_shell", return_value=True), \
                mock.patch.object(run_demo, "build_core_packages", return_value=True), \
                mock.patch.object(run_demo, "build_lazyrad", return_value=True):
            os.environ.pop("LAZYRAD_SAMPLES", None)
            self.run_main("--lazyrad")
        self.assertEqual(self.builds[-1].get("LAZYOS_LAZYRAD"), "1")
        self.assertEqual(self.builds[-1].get("LAZYRAD_SAMPLES", "").split(os.pathsep),
                         LAZYOS_SAMPLES)

    def test_modplayer_brings_lazyrad_the_desktop_and_a_sound_card(self) -> None:
        with mock.patch.object(run_demo, "build_lazyrad", return_value=True) as lazyrad,                 mock.patch.object(run_demo, "build_modplayer", return_value=True) as package,                 mock.patch.object(run_demo, "build_xui_shell", return_value=True):
            code, command = self.run_main("--modplayer")
        self.assertEqual(code, 0)
        lazyrad.assert_called_once()
        package.assert_called_once()
        env = self.builds[-1]
        for switch in ("LAZYOS_MODPLAYER", "LAZYOS_LAZYRAD", "LAZYOS_DESKTOP", "LAZYOS_SOUND"):
            self.assertEqual(env.get(switch), "1", switch)
        self.assertIn("virtio-sound-pci,audiodev=snd0", command)

    def test_lazyrad_is_a_desktop_core_package_built_before_packaging(self) -> None:
        # os.lazy.lazyrad is a core package: `--lazyrad` implies the desktop,
        # and the packages are built (after the IDE) before the image.
        order: list[str] = []
        with mock.patch.object(run_demo, "build_xui_shell", return_value=True), \
                mock.patch.object(run_demo, "build_lazyrad",
                                  side_effect=lambda: order.append("lazyrad") or True), \
                mock.patch.object(run_demo, "build_core_packages",
                                  side_effect=lambda: order.append("packages") or True):
            code, _ = self.run_main("--lazyrad")
        self.assertEqual(code, 0)
        self.assertEqual(order, ["lazyrad", "packages"])
        self.assertEqual(self.builds[-1].get("LAZYOS_DESKTOP"), "1")
        self.assertEqual(self.builds[-1].get("LAZYOS_LAZYRAD"), "1")

    def test_journal_sets_the_build_switch(self) -> None:
        code, _ = self.run_main("--journal")
        self.assertEqual(code, 0)
        self.assertEqual(self.builds[-1].get("LAZYOS_JOURNAL"), "1")
        code, _ = self.run_main("--journal", "8192")
        self.assertEqual(self.builds[-1].get("LAZYOS_JOURNAL"), "8192")
        self.run_main()
        self.assertNotIn("LAZYOS_JOURNAL", self.builds[-1])

    def test_reset_os_cannot_combine_with_no_build(self) -> None:
        with self.assertRaises(SystemExit), redirect_stderr(io.StringIO()):
            self.run_main("--no-build", "--reset-os")


class NetTests(unittest.TestCase):
    """`--net`: the whole stack in the image, a card and forwards in QEMU."""

    # The same faked build and QEMU, without inheriting MainTests' tests.
    setUp = MainTests.setUp
    run_main = MainTests.run_main

    def run_net(self, *argv: str, busy: list[str] | None = None) -> tuple[int, list[str]]:
        with mock.patch.object(run_demo.qemu_net, "busy_ports", return_value=busy or []), \
                mock.patch.dict(os.environ, {}, clear=False):
            os.environ.pop("LAZYOS_NETD_ARGS", None)
            return self.run_main(*argv)

    def netdev(self, command: list[str]) -> str:
        return command[command.index("-netdev") + 1]

    def test_net_builds_the_stack_without_the_harness_clients(self) -> None:
        code, command = self.run_net("--net")
        self.assertEqual(code, 0)
        self.assertEqual(self.builds[-1].get("LAZYOS_NETD"), "1")
        self.assertEqual(self.builds[-1].get("LAZYOS_NETD_ARGS"), "demo=0")
        self.assertIn("virtio-net-pci,netdev=n0", command)
        # The Net Tools web server, reachable from this machine only.
        self.assertIn("hostfwd=tcp:127.0.0.1:8080-:8080", self.netdev(command))

    def test_forwards_restrict_and_capture_reach_qemu(self) -> None:
        code, command = self.run_net("--net", "--no-build", "--net-forward", "2323:23",
                                     "--net-forward", "udp:0.0.0.0:5353:53",
                                     "--net-restrict", "--net-pcap", "net.pcap")
        self.assertEqual(code, 0)
        netdev = self.netdev(command)
        self.assertIn("restrict=on", netdev)
        self.assertIn("hostfwd=tcp:127.0.0.1:2323-:23", netdev)
        self.assertIn("hostfwd=udp:0.0.0.0:5353-:53", netdev)
        self.assertNotIn(":8080", netdev, "explicit forwards replace the default")
        self.assertIn("filter-dump,id=netdump,netdev=n0,file=net.pcap", command)

    def test_tls_builds_the_https_tools_and_brings_the_network(self) -> None:
        with mock.patch.object(run_demo, "build_tls", return_value=True) as tools:
            code, command = self.run_net("--tls")
        self.assertEqual(code, 0)
        tools.assert_called_once()
        self.assertEqual(self.builds[-1].get("LAZYOS_TLS"), "1")
        self.assertEqual(self.builds[-1].get("LAZYOS_NETD"), "1")
        self.assertIn("virtio-net-pci,netdev=n0", command)

    def test_tls_stops_when_the_tools_do_not_build(self) -> None:
        with mock.patch.object(run_demo, "build_tls", return_value=False):
            code, command = self.run_net("--tls")
        self.assertEqual(code, 1)
        self.assertEqual(command, [])

    def run_lazyweb(self, built: bool = True) -> tuple[int, list[str]]:
        with mock.patch.object(run_demo, "build_lazyweb", return_value=built) as browser, \
                mock.patch.object(run_demo, "build_tls", return_value=True), \
                mock.patch.object(run_demo, "build_xui_shell", return_value=True), \
                mock.patch.object(run_demo, "build_xui_apps", return_value=True):
            result = self.run_net("--lazyweb")
        browser.assert_called_once()
        return result

    def test_lazyweb_is_a_desktop_with_the_network_and_https(self) -> None:
        code, command = self.run_lazyweb()
        self.assertEqual(code, 0)
        env = self.builds[-1]
        for name in ("LAZYOS_LAZYWEB", "LAZYOS_DESKTOP", "LAZYOS_NETD", "LAZYOS_TLS"):
            self.assertEqual(env.get(name), "1", name)
        self.assertEqual(env.get("LAZYOS_NETD_ARGS"), "demo=0")
        self.assertIn("virtio-net-pci,netdev=n0", command)

    def test_lazyweb_stops_when_the_browser_is_not_built(self) -> None:
        code, command = self.run_lazyweb(built=False)
        self.assertEqual(code, 1)
        self.assertEqual(command, [])

    def test_lazyweb_build_needs_the_browser_binary(self) -> None:
        missing = self.dir / "xui-lazyweb.elf"
        with mock.patch.object(run_demo.demo_builds, "LAZYWEB_ELF", missing), \
                mock.patch.object(run_demo.demo_builds, "build_xui_apps", return_value=True), \
                redirect_stderr(io.StringIO()) as err:
            self.assertFalse(run_demo.demo_builds.build_lazyweb())
        self.assertIn("zig", err.getvalue())
        missing.write_bytes(b"\x7fELF")
        with mock.patch.object(run_demo.demo_builds, "LAZYWEB_ELF", missing), \
                mock.patch.object(run_demo.demo_builds, "build_xui_apps") as rebuilt:
            self.assertTrue(run_demo.demo_builds.build_lazyweb())
        rebuilt.assert_not_called()

    def test_no_net_attaches_no_card(self) -> None:
        _, command = self.run_net("--no-build")
        self.assertNotIn("-netdev", command)
        self.assertNotIn("LAZYOS_NETD", self.builds[-1] if self.builds else {})

    def test_a_busy_host_port_stops_before_qemu(self) -> None:
        code, command = self.run_net("--net", "--no-build", busy=["tcp/127.0.0.1:8080"])
        self.assertEqual(code, 1)
        self.assertEqual(command, [])

    def test_bad_or_orphan_net_options_are_refused(self) -> None:
        for argv in (["--net", "--net-forward", "80"], ["--net-forward", "8080:8080"],
                     ["--net", "--net-forward", "none", "--net-forward", "1:2"]):
            with self.assertRaises(SystemExit), redirect_stderr(io.StringIO()):
                self.run_net("--no-build", *argv)

    def test_a_net_desktop_builds_missing_network_apps(self) -> None:
        with mock.patch.object(run_demo, "build_xui_shell", return_value=True), \
                mock.patch.object(run_demo, "NET_APPS", [self.dir / "missing.elf"]), \
                mock.patch.object(run_demo, "build_xui_apps", return_value=True) as built:
            code, _ = self.run_net("--desktop", "--net")
        self.assertEqual(code, 0)
        built.assert_called_once()


if __name__ == "__main__":
    unittest.main()
