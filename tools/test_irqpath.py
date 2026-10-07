#!/usr/bin/env python3
"""Tests for `irqpath` (issue #616): the build switches of each path, and a
judge that fails when it should.

Run: python tools/test_irqpath.py
"""

from __future__ import annotations

import sys
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
import irqpath  # noqa: E402

MSI_LOG = "HW:IRQCHIP:ioapic pins=24\nDEV:MSI:PASS:00:04.0 dev 7 MsiX vector 0x40\nSNDD:IRQ:MsiX\n"
PIC_LOG = "HW:IRQCHIP:pic forced (LAZYOS_IRQCHIP=pic)\nSNDD:IRQ:Intx\n"


class IrqPathTests(unittest.TestCase):
    def test_build_switches(self) -> None:
        self.assertEqual(irqpath.build_env("msi"), {"LAZYOS_IRQCHIP": "ioapic", "LAZYOS_MSI": "1"})
        self.assertEqual(irqpath.build_env("pic"), {"LAZYOS_IRQCHIP": "pic", "LAZYOS_MSI": "0"})
        self.assertEqual(irqpath.PATHS[0], "msi")

    def test_the_paths_pass_their_own_logs(self) -> None:
        self.assertIsNone(irqpath.judge(MSI_LOG, "msi", "SNDD:IRQ:", True))
        self.assertIsNone(irqpath.judge(PIC_LOG, "pic", "SNDD:IRQ:", True))
        # A device without MSI stays on INTx on the msi path.
        e1000 = "HW:IRQCHIP:ioapic pins=24\nNETDRV:IRQ:Intx\n"
        self.assertIsNone(irqpath.judge(e1000, "msi", "NETDRV:IRQ:", False))
        # A polled run is not judged on its mode.
        self.assertIsNone(irqpath.judge("HW:IRQCHIP:ioapic\n", "msi", "SNDD:IRQ:", True))

    def test_the_judge_fails_when_it_should(self) -> None:
        cases = [
            (PIC_LOG, "msi", True),                       # the 8259 kept the lines
            (MSI_LOG, "pic", True),                       # the I/O APIC on the pic path
            ("HW:IRQCHIP:ioapic\nSNDD:IRQ:Intx\n", "msi", True),   # capable but on INTx
            ("HW:IRQCHIP:ioapic\nSNDD:IRQ:Msi\n", "msi", True),    # no delivery proof
            ("HW:IRQCHIP:ioapic\nNETDRV:IRQ:MsiX\nDEV:MSI:PASS:x\n", "msi", False),
            ("HW:IRQCHIP:pic\nDEV:MSI:PASS:x\nSNDD:IRQ:Intx\n", "pic", True),
            ("HW:IRQCHIP:pic\nSNDD:IRQ:Msi\n", "pic", True),
        ]
        for text, path, capable in cases:
            marker = "NETDRV:IRQ:" if "NETDRV" in text else "SNDD:IRQ:"
            with self.subTest(text=text, path=path):
                self.assertIsNotNone(irqpath.judge(text, path, marker, capable))

    def test_driver_mode_reads_each_report(self) -> None:
        self.assertEqual(irqpath.driver_mode("USBD:IRQ hc=0 armed MsiX\n", " armed "), "MsiX")
        self.assertEqual(irqpath.driver_mode("SNDD:IRQ:Msi\n", "SNDD:IRQ:"), "Msi")
        self.assertEqual(irqpath.driver_mode("SNDD:IRQ:POLLING\n", "SNDD:IRQ:"), None)


if __name__ == "__main__":
    unittest.main()
