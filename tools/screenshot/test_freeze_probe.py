"""Unit tests for freeze_probe.py (no QEMU needed):
python tools/screenshot/test_freeze_probe.py"""

from __future__ import annotations

import sys
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
import freeze_probe  # noqa: E402

# Shaped like the issue #449 capture (`freeze_registers.txt`).
REGS = """RAX=0000000000000080 RBX=0000000000000000 RCX=0000000000000001 RDX=00000000000001f7
RSI=0000000000000000 RDI=0000000000000000 RBP=0000000000000000 RSP=0000008000345747
RIP=00000080001708f8 RFL=00000006 [-----P-] CPL=0 II=0 A20=1 SMM=0 HLT=0
"""


class ParseTests(unittest.TestCase):
    def test_registers(self):
        regs = freeze_probe.parse_registers(REGS)
        self.assertEqual(regs, {"rip": 0x80001708F8, "cpl": 0, "rdx": 0x1F7})

    def test_missing_fields_are_none(self):
        self.assertEqual(freeze_probe.parse_registers("nothing"),
                         {"rip": None, "cpl": None, "rdx": None})

    def test_byte_dump_skips_addresses(self):
        text = ("00000080001708f7: 0xec 0xc3 0x90 0x90 0x0f 0x1f 0x40 0x00\n"
                "00000080001708ff: 0x00\n")
        self.assertEqual(freeze_probe.parse_bytes(text),
                         bytes([0xEC, 0xC3, 0x90, 0x90, 0x0F, 0x1F, 0x40, 0x00, 0x00]))


class OpcodeTests(unittest.TestCase):
    def test_port_instructions_at_rip(self):
        for code in ([0xEC], [0xED], [0xE4, 0x60], [0xEE], [0x66, 0xED],
                     [0xF3, 0x66, 0x6D], [0xF3, 0x6C]):
            self.assertTrue(freeze_probe.at_port_io(bytes(code)), code)

    def test_rip_just_past_a_one_byte_in(self):
        self.assertTrue(freeze_probe.at_port_io(bytes([0xC3]), before=0xEC))

    def test_other_code_is_not_port_io(self):
        for code in ([0x90], [0xF4], [0xEB, 0xFE], [0x66, 0x90], []):
            self.assertFalse(freeze_probe.at_port_io(bytes(code), before=0x90), code)

    def test_too_many_prefixes_are_not_trusted(self):
        self.assertFalse(freeze_probe.at_port_io(bytes([0x66] * 5 + [0xEC])))


class MonitorTests(unittest.TestCase):
    def monitor(self, regs: str, dump: str):
        def run(command: str) -> str:
            return regs if command == "info registers" else dump
        return run

    def test_kernel_at_ata_status_poll(self):
        dump = "00000080001708f7: 0x90 0xec 0xa8 0x80 0x75 0xfa 0xc3 0x90 0x90"
        found = freeze_probe.port_io_stall(self.monitor(REGS, dump))
        self.assertIsNotNone(found)
        self.assertIn("RDX=0x1f7", found)

    def test_user_mode_is_never_a_port_stall(self):
        regs = REGS.replace("CPL=0", "CPL=3")
        dump = "0: 0x90 0xec 0x90 0x90 0x90 0x90 0x90 0x90 0x90"
        self.assertIsNone(freeze_probe.port_io_stall(self.monitor(regs, dump)))

    def test_a_kernel_spin_is_not_a_port_stall(self):
        dump = "0: 0x90 0xeb 0xfe 0x90 0x90 0x90 0x90 0x90 0x90"
        self.assertIsNone(freeze_probe.port_io_stall(self.monitor(REGS, dump)))

    def test_a_dead_monitor_is_not_a_port_stall(self):
        def broken(command: str) -> str:
            raise RuntimeError("QMP closed")
        self.assertIsNone(freeze_probe.port_io_stall(broken))


if __name__ == "__main__":
    unittest.main()
