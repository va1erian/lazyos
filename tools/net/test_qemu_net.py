#!/usr/bin/env python3
"""Tests for qemu_net.py: forward parsing, QEMU arguments, the port check.

Run: python tools/net/test_qemu_net.py
"""

from __future__ import annotations

import argparse
import socket
import sys
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
import qemu_net  # noqa: E402
from qemu_net import Forward, parse_forward  # noqa: E402


class ForwardTests(unittest.TestCase):
    def test_short_and_full_forms(self) -> None:
        self.assertEqual(parse_forward("8080:80"), Forward("tcp", "127.0.0.1", 8080, 80))
        self.assertEqual(parse_forward("udp:5353:53"), Forward("udp", "127.0.0.1", 5353, 53))
        self.assertEqual(parse_forward("TCP:0.0.0.0:2222:22"), Forward("tcp", "0.0.0.0", 2222, 22))
        self.assertEqual(parse_forward("8080:80").hostfwd(), "tcp:127.0.0.1:8080-:80")

    def test_bad_forwards_are_refused(self) -> None:
        for bad in ["", "80", "a:b", "0:80", "80:65536", "sctp:1:2", "host:1:2",
                    "1.2.3.4:5:6:7", "tcp:1.2.3:4:5", "-1:2"]:
            with self.assertRaises(ValueError, msg=bad):
                parse_forward(bad)

    def test_defaults_none_and_duplicates(self) -> None:
        self.assertEqual(qemu_net.forwards_from(None),
                         [Forward("tcp", "127.0.0.1", 8080, 8080)])
        self.assertEqual(qemu_net.forwards_from(["none"]), [])
        with self.assertRaises(ValueError):
            qemu_net.forwards_from(["none", "1:2"])
        with self.assertRaises(ValueError):
            qemu_net.forwards_from(["8080:80", "0.0.0.0:8080:81"])
        # The same port number over TCP and UDP is two different forwards.
        self.assertEqual(len(qemu_net.forwards_from(["53:53", "udp:53:53"])), 2)


class ArgsTests(unittest.TestCase):
    def parse(self, *argv: str) -> argparse.Namespace:
        parser = argparse.ArgumentParser()
        qemu_net.add_net_options(parser, "net")
        return parser.parse_args(list(argv))

    def test_the_card_and_netdev(self) -> None:
        args, forwards = qemu_net.args_from_options(self.parse("--net"))
        self.assertEqual(args[:2], ["-netdev", "user,id=n0,hostfwd=tcp:127.0.0.1:8080-:8080"])
        self.assertEqual(args[2:], ["-device", "virtio-net-pci,netdev=n0"])
        self.assertEqual(len(forwards), 1)

    def test_restrict_and_capture(self) -> None:
        args, _ = qemu_net.args_from_options(
            self.parse("--net", "--net-forward", "none", "--net-restrict", "--net-pcap", "x.pcap"))
        self.assertEqual(args[1], "user,id=n0,restrict=on")
        self.assertEqual(args[-2:], ["-object", "filter-dump,id=netdump,netdev=n0,file=x.pcap"])

    def test_more_cards_each_get_a_network_of_their_own(self) -> None:
        args, _ = qemu_net.args_from_options(self.parse("--net", "--nics", "2", "--net-forward", "none"))
        self.assertEqual(args[:4], ["-netdev", "user,id=n0", "-device", "virtio-net-pci,netdev=n0"])
        self.assertEqual(args[4:], [
            "-netdev", "user,id=n1,net=10.0.3.0/24",
            "-device", "virtio-net-pci,netdev=n1,mac=52:54:00:12:34:57,id=nic1",
        ])
        args, _ = qemu_net.args_from_options(self.parse("--net", "--nics", "3", "--net-restrict"))
        self.assertEqual(sum(1 for a in args if a.startswith("user,id=n")), 3)
        self.assertTrue(all("restrict=on" in a for a in args if a.startswith("user,id=n")))
        # One card is what it always was, whatever else is asked.
        one, _ = qemu_net.args_from_options(self.parse("--net", "--nics", "1"))
        self.assertEqual(len(one), 4)
        for bad in ("0", "5"):
            with self.assertRaises(ValueError, msg=bad):
                qemu_net.args_from_options(self.parse("--net", "--nics", bad))
        with self.assertRaises(ValueError):
            qemu_net.args_from_options(self.parse("--nics", "2"))

    def test_nothing_without_net_and_options_need_it(self) -> None:
        self.assertEqual(qemu_net.args_from_options(self.parse()), ([], []))
        for argv in (["--net-forward", "1:2"], ["--net-restrict"], ["--net-pcap", "p"]):
            with self.assertRaises(ValueError, msg=argv):
                qemu_net.args_from_options(self.parse(*argv))

    def test_the_description_says_how_to_get_in(self) -> None:
        text = qemu_net.describe(qemu_net.forwards_from(None))
        self.assertIn("10.0.2.15", text)
        self.assertIn("http://localhost:8080", text)
        self.assertIn("cannot connect in", qemu_net.describe([]))


class BusyPortTests(unittest.TestCase):
    def test_a_held_port_is_reported_and_a_free_one_is_not(self) -> None:
        with socket.socket() as holder:
            holder.bind(("127.0.0.1", 0))
            holder.listen(1)
            port = holder.getsockname()[1]
            held = Forward("tcp", "127.0.0.1", port, 80)
            self.assertEqual(qemu_net.busy_ports([held]), [f"tcp/127.0.0.1:{port}"])
        self.assertEqual(qemu_net.busy_ports([held]), [])


if __name__ == "__main__":
    unittest.main()
