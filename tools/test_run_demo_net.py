#!/usr/bin/env python3
"""Tests for run_demo.py's `--net` family: the stack, forwards, TLS, SMB and LazyWeb (no QEMU, no build).

Run: python tools/test_run_demo_net.py
"""

from __future__ import annotations

import io
import os
import sys
import unittest
from contextlib import redirect_stderr
from pathlib import Path
from unittest import mock

sys.path.insert(0, str(Path(__file__).resolve().parent))
import run_demo  # noqa: E402
# A module import, so unittest does not collect MainTests a second time here.
import test_run_demo  # noqa: E402


class NetTests(unittest.TestCase):
    """`--net`: the whole stack in the image, a card and forwards in QEMU."""

    # The same faked build and QEMU, without inheriting MainTests' tests.
    setUp = test_run_demo.MainTests.setUp
    run_main = test_run_demo.MainTests.run_main

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

    def test_smb_builds_the_client_and_brings_the_network(self) -> None:
        code, command = self.run_net("--smb")
        self.assertEqual(code, 0)
        self.assertEqual(self.builds[-1].get("LAZYOS_SMB"), "1")
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
        self.assertIn("xui/build.py", err.getvalue())
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
