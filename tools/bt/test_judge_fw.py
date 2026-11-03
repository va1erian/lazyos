"""The firmware judge (`judge_fw.py`) must fail when it should. Run through
`python tools/bt/test_judge.py`, which imports this class."""

from __future__ import annotations

import sys
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
import judge_fw  # noqa: E402

SFI, DDC = "intel/ibt-19-0-4.sfi", "intel/ibt-19-0-4.ddc"


def vendor(attempt: int) -> str:
    return f"BT:FW:VENDOR vendor=intel usb=8087:0026 attempt={attempt}\n"


def files(*names: str) -> str:
    return "".join(f"BT:FW:FILE name={n} required=true\nBT:FW:FILE name={n} bytes=1000\n" for n in names)


DONE = "BT:FW:DONE vendor=intel downloaded=1 config=applied:2 ms=4200\n"
BIND = "USBD:BT:BIND port=0-5 slot=1 vendor=0x8087 product=0x0026 interface=0\nUSBD:BT:SERVE name=os.lazy.usb.bt\n"


class FwJudge(unittest.TestCase):
    @classmethod
    def setUpClass(cls) -> None:
        # The pairing fixtures of the HID judge tests (imported late: that
        # module imports this one when it runs as a script).
        import test_judge

        cls.pair = test_judge.PAIR
        cls.boot = test_judge.BOOT.format(n=0, file=" file=missing")

    def log(self, phase: str, usb: str = BIND) -> str:
        """The firmware phase, then the usual boot and pairing of `--pair`."""
        body = self.pair.replace(self.boot, self.boot.replace("BT:READY", "BT:READY"))
        before, _, after = body.partition("BT:LINK ring=1\n")
        return usb + before + "BT:LINK ring=1\n" + phase + after

    def failures(self, kind: str, log: str) -> list[str]:
        return judge_fw.judge(kind, log, "hello")

    def assertFails(self, kind: str, log: str, why: str) -> None:
        failures = self.failures(kind, log)
        self.assertTrue(any(why in f for f in failures), f"{why!r} not in {failures}")

    def test_a_good_intel_run_passes(self) -> None:
        self.assertEqual(self.failures("intel", self.log(vendor(0) + files(SFI, DDC) + DONE)), [])

    def test_a_good_recovery_run_passes(self) -> None:
        first = vendor(0) + files(SFI) + "BT:FW:REENUM attempt=1 ms=4476 reason=download-rejected result=0x12\n"
        log = self.log(first + vendor(1) + files(SFI, DDC) + DONE, BIND + "USBD:BT:UNBIND port=0-5\n" + BIND)
        self.assertEqual(self.failures("intel-recovery", log), [])

    def test_a_download_that_failed_fails(self) -> None:
        log = self.log(vendor(0) + files(SFI) + "BT:FW:FAIL reason=file-missing ms=3\n" + DONE)
        self.assertFails("intel", log, "firmware error")

    def test_the_vendor_and_ids_must_be_the_ones_the_kind_plays(self) -> None:
        log = self.log(vendor(0).replace("8087:0026", "0bda:c822") + files(SFI, DDC) + DONE)
        self.assertFails("intel", log, "want intel 8087:0026")

    def test_the_files_must_be_asked_for_in_order_and_read(self) -> None:
        self.assertFails("intel", self.log(vendor(0) + files(DDC, SFI) + DONE), "files asked for")
        unread = self.log(vendor(0) + files(SFI, DDC).replace("bytes=1000", "bytes=0") + DONE)
        self.assertFails("intel", unread, "was not read")
        missing = self.log(vendor(0) + files(SFI, DDC) + f"BT:FW:FILE name={DDC} missing=1\n" + DONE)
        self.assertFails("intel", missing, "reported missing")

    def test_the_download_must_finish_before_the_controller_is_brought_up(self) -> None:
        log = self.log(vendor(0) + files(SFI, DDC))
        self.assertFails("intel", log, "0 BT:FW:DONE lines")
        late = self.log(vendor(0) + files(SFI, DDC)).replace("BT:LINK ring=1\n", "BT:LINK ring=1\n", 1) + DONE
        self.assertFails("intel", late, "before BT:FW:DONE")

    def test_a_reenumeration_the_kind_does_not_expect_fails(self) -> None:
        log = self.log(vendor(0) + files(SFI) + "BT:FW:REENUM attempt=1 ms=9 reason=reset\n" + vendor(1)
                       + files(SFI, DDC) + DONE)
        self.assertFails("intel", log, "BT:FW:VENDOR lines")

    def test_usbd_must_bind_once_per_enumeration(self) -> None:
        self.assertFails("intel", self.log(vendor(0) + files(SFI, DDC) + DONE, BIND + BIND), "USBD:BT:BIND")
        self.assertFails("intel", self.log(vendor(0) + files(SFI, DDC) + DONE, BIND + "USBD:BT:UNBIND x\n"), "UNBIND")

    def test_the_pairing_after_the_download_is_judged_too(self) -> None:
        log = self.log(vendor(0) + files(SFI, DDC) + DONE).replace("BT:HID:KBD", "BT:HID:XXX")
        self.assertTrue(self.failures("intel", log))


if __name__ == "__main__":
    unittest.main()
