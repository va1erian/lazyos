#!/usr/bin/env python3
"""The RTL8168 diagnosis tools fail when they should (and agree with the
driver on the XID encoding)."""

from __future__ import annotations

import sys
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
import regmap  # noqa: E402

MAC = bytes([0x00, 0xE0, 0x4C, 0x12, 0x34, 0x56])


def tx_config_for(xid: int) -> int:
    """The driver's `fake::tx_config_for`: the scatter, the other way round."""
    return (xid & 0xF) << 20 | ((xid >> 6) & 0x3F) << 26 | 0x700


def chip(xid: int = 0x541, status: int = 0x13) -> bytearray:
    dump = bytearray(256)
    dump[0:6] = MAC
    dump[0x37] = 0x0C
    dump[0x3C:0x3E] = (0x8000 | 0x3F).to_bytes(2, "little")
    dump[0x40:0x44] = tx_config_for(xid).to_bytes(4, "little")
    dump[0x44:0x48] = (0x0E | 7 << 8 | 7 << 13).to_bytes(4, "little")
    dump[0x6C] = status  # link, 1000, full duplex
    dump[0xDA:0xDC] = (1528).to_bytes(2, "little")
    return dump


def as_ethtool(dump: bytes) -> str:
    lines = ["Offset\t\tValues", "------\t\t------"]
    for offset in range(0, 256, 16):
        lines.append(f"0x{offset:04x}:\t\t" + " ".join(f"{b:02x}" for b in dump[offset : offset + 16]) + " ")
    return "\n".join(lines) + "\n"


def as_serial(dump: bytes, label: str) -> str:
    return "".join(
        f"NETDRV:REGS {label} {offset:02x}: " + " ".join(f"{b:02x}" for b in dump[offset : offset + 32]) + "\n"
        for offset in range(0, 256, 32)
    )


class Xid(unittest.TestCase):
    def test_scatter_matches_the_driver(self):
        for xid in (0x541, 0x540, 0x449, 0x4C0, 0x000, 0x7C0):
            self.assertEqual(regmap.xid(tx_config_for(xid)), xid)

    def test_the_box(self):
        self.assertEqual(regmap.xid(0x5410_0700), 0x541)


class Parsing(unittest.TestCase):
    def test_ethtool_round_trip(self):
        dump = bytes(chip())
        self.assertEqual(regmap.parse_ethtool(as_ethtool(dump)), dump)

    def test_a_short_ethtool_dump_is_refused(self):
        with self.assertRaises(ValueError):
            regmap.parse_ethtool(as_ethtool(bytes(chip()))[:300])

    def test_serial_round_trip_with_noise_and_two_labels(self):
        a, b = bytes(chip()), bytes(chip(status=0x00))
        log = "boot noise\n" + as_serial(a, "open") + "NETDRV:RTL8168:LINK x\n" + as_serial(b, "link-change")
        dumps = regmap.parse_serial(log)
        self.assertEqual(dumps, {"open": a, "link-change": b})

    def test_a_truncated_serial_dump_is_ignored(self):
        log = as_serial(bytes(chip()), "open").splitlines(True)
        self.assertEqual(regmap.parse_serial("".join(log[:3])), {})


class Decoding(unittest.TestCase):
    def test_report_names_the_fields(self):
        text = regmap.report(bytes(chip()), "t")
        self.assertIn("xid=0x541", text)
        self.assertIn("link=up speed=1000 full", text)
        self.assertIn("00:e0:4c:12:34:56", text)
        self.assertNotIn("FAIL", text)

    def test_another_revision_fails_the_assumptions(self):
        self.assertIn("FAIL", regmap.report(bytes(chip(xid=0x449)), "t"))

    def test_a_gone_device_fails_the_assumptions(self):
        gone = bytes([0xFF]) * 256
        failed = [text for ok, text in regmap.check(gone) if not ok]
        self.assertGreaterEqual(len(failed), 3)

    def test_group_mac_fails(self):
        dump = chip()
        dump[0] = 0x01
        self.assertTrue(any(not ok for ok, _ in regmap.check(bytes(dump))))


class Diff(unittest.TestCase):
    def test_identical_ignoring_volatile(self):
        a, b = chip(), chip()
        b[0x3E] = 0xFF  # IntrStatus
        b[0x60] = 0x80  # PHYAR
        b[0xE4] = 0x12  # RDSAR
        self.assertEqual(regmap.diff(bytes(a), bytes(b)), [])

    def test_a_real_difference_is_named(self):
        a, b = chip(), chip(status=0x00)
        lines = regmap.diff(bytes(a), bytes(b), "linux", "lazyos")
        self.assertEqual(len(lines), 1)
        self.assertIn("PHYstatus", lines[0])
        self.assertIn("link=down", lines[0])

    def test_unnamed_bytes_are_reported(self):
        a, b = chip(), chip()
        b[0xF0] = 1
        self.assertIn("0xf0", regmap.diff(bytes(a), bytes(b))[0])


if __name__ == "__main__":
    unittest.main()
