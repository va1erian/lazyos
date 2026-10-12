#!/usr/bin/env python3
"""Self-test of `tools/dbg/hw_checks.py`: the judge must fail when it should.

A fake `dbgd` answers `devices.list`, `pci.config` and `mmio.read` like the
real one does for QEMU's standard VGA; then deliberately wrong fakes (one
that serves a register the allowlist must refuse, one that never rate limits,
one that accepts a write parameter) must each make some check fail.

Run: python tools/dbg/test_judge.py
"""

from __future__ import annotations

import sys
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
import hw_checks  # noqa: E402
from dbgclient import DbgError  # noqa: E402

BDF = "00:02.0"
VGA_CONFIG = bytes.fromhex("34121111") + bytes(252)


class Checks:
    def __init__(self) -> None:
        self.failed: list[str] = []

    def check(self, name: str, ok: bool, detail: str = "") -> bool:
        if not ok:
            self.failed.append(name)
        return ok


class FakeDbgd:
    """What the real service answers for the standard VGA, plus a few
    knobs that make it wrong."""

    def __init__(self, mmio: bool = True, **faults: bool) -> None:
        self.mmio = mmio
        self.faults = faults
        self.budget = 512 * 1024
        self.sleeps = 0

    def call(self, method: str, **p) -> dict:
        if method == "methods":
            return {"methods": [{"name": "pci.config"}, {"name": "mmio.read"}]}
        if method == "devices.list":
            return {"devices": [
                {"id": 1, "bdf": BDF, "vendor": "1234", "device": "1111"},
                {"id": 2, "bdf": "00:03.0", "vendor": "1af4", "device": "1000"},
            ]}
        if method == "pci.config":
            return self.pci_config(**p)
        if method == "mmio.read":
            return self.mmio_read(**p)
        raise DbgError(-32601, "no such method")

    def pci_config(self, bdf: str = BDF, offset: int = 0, len: int = 64, **extra) -> dict:
        if extra and not self.faults.get("accepts_write"):
            raise DbgError(-32602, "unknown parameter")
        if not all(c in "0123456789abcdef:." for c in bdf):
            raise DbgError(-32602, "bdf")
        if bdf != BDF and bdf != "00:03.0":
            raise DbgError(-32003, "no PCI function")
        if offset > 255 or len < 1 or offset + len > 256:
            raise DbgError(-32602, "range")
        data = VGA_CONFIG[offset:offset + len]
        return {"bdf": bdf, "vendor": "1234", "device": "1111", "offset": offset,
                "len": len, "hex": data.hex()}

    def mmio_read(self, bdf: str, bar: int = 0, offset: int = 0, len: int = 4, **extra) -> dict:
        if not self.mmio:
            raise DbgError(-32002, "MMIO reads are off on this box (diag.dbg.mmio=1 in lazyos.cfg)")
        if extra and not self.faults.get("accepts_write"):
            raise DbgError(-32602, "unknown parameter")
        if bdf not in (BDF, "00:03.0"):
            raise DbgError(-32003, "no PCI function")
        if bar > 5 or len > 4096 or len < 4:
            raise DbgError(-32602, "range")
        if offset % 4 or len % 4:
            raise DbgError(-32602, "alignment")
        if bdf != BDF:
            raise DbgError(-32002, "none listed for 1af4:1000 BAR0")
        allowed = bar == 0 or (bar == 2 and (offset + len <= 0x400
                                             or 0x500 <= offset and offset + len <= 0x520))
        if self.faults.get("serves_everything"):
            allowed = True
        if not allowed:
            raise DbgError(-32002, "the span is not inside a range; allowed for BAR2: 0x0-0x400")
        if not self.faults.get("never_limits"):
            self.budget -= max(len, 64)
            if self.budget < 0:
                self.budget = 0
                raise DbgError(-32005, "rate limited")
        value = 0xB0C5 if (bar, offset) == (2, 0x500) else 0
        data = (value.to_bytes(4, "little") + bytes(len))[:len]
        return {"bdf": bdf, "vendor": "1234", "device": "1111", "bar": bar,
                "offset": offset, "len": len, "hex": data.hex()}


class JudgeTests(unittest.TestCase):
    def setUp(self) -> None:
        # The rate-limit refill wait is real time in the judge; skip it here.
        self._sleep = hw_checks.time.sleep
        hw_checks.time.sleep = lambda _s: None

    def tearDown(self) -> None:
        hw_checks.time.sleep = self._sleep

    def run_judge(self, fake: FakeDbgd, off: bool = False) -> list[str]:
        checks = Checks()
        (hw_checks.exercise_off if off else hw_checks.exercise)(checks, fake.call)
        return checks.failed

    def test_a_correct_service_passes(self) -> None:
        # The fake refills its bucket when the judge "waits".
        fake = FakeDbgd()
        original = hw_checks.time.sleep
        hw_checks.time.sleep = lambda _s: setattr(fake, "budget", 512 * 1024)
        try:
            self.assertEqual(self.run_judge(fake), [])
        finally:
            hw_checks.time.sleep = original

    def test_an_image_without_mmio_passes_the_off_judge(self) -> None:
        self.assertEqual(self.run_judge(FakeDbgd(mmio=False), off=True), [])

    def test_serving_a_register_the_allowlist_refuses_fails(self) -> None:
        failed = self.run_judge(FakeDbgd(serves_everything=True))
        self.assertIn("mmio.read refuses a register outside the allowlist", failed)

    def test_no_rate_limit_fails(self) -> None:
        failed = self.run_judge(FakeDbgd(never_limits=True))
        self.assertIn("a burst of hardware reads is rate limited", failed)

    def test_accepting_a_write_parameter_fails(self) -> None:
        failed = self.run_judge(FakeDbgd(accepts_write=True))
        self.assertIn("pci.config has no write parameter", failed)
        self.assertIn("mmio.read has no write parameter", failed)

    def test_mmio_on_when_it_should_be_off_fails(self) -> None:
        failed = self.run_judge(FakeDbgd(mmio=True), off=True)
        self.assertIn("mmio.read is refused unless the box turned it on", failed)

    def test_a_missing_vga_stops_early(self) -> None:
        class NoVga(FakeDbgd):
            def call(self, method: str, **p) -> dict:
                if method == "devices.list":
                    return {"devices": []}
                return super().call(method, **p)

        self.assertIn("devices.list shows the standard VGA with its address",
                      self.run_judge(NoVga()))


if __name__ == "__main__":
    unittest.main()
