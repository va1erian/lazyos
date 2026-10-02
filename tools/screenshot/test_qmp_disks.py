"""Unit tests for the QEMU disk arguments (no QEMU needed):
python tools/screenshot/test_qmp_disks.py"""

from __future__ import annotations

import sys
import tempfile
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
import qemu_qmp  # noqa: E402


def command(**disks) -> list[str]:
    return qemu_qmp.build_qemu_command("qemu", "boot.img", 4444, Path("serial.log"), **disks)


def drive_ids(argv: list[str]) -> list[str]:
    """The `id=` of each `-drive`, in command-line (and so PCI) order."""
    return [arg.split("id=")[1].split(",")[0]
            for flag, arg in zip(argv, argv[1:]) if flag == "-drive" and "id=" in arg]


class DiskOrderTests(unittest.TestCase):
    def test_hermetic_by_default(self) -> None:
        self.assertEqual(drive_ids(command()), ["boot"])

    def test_home_disk_follows_the_boot_disk(self) -> None:
        argv = command(home_disk="home.img")
        self.assertEqual(drive_ids(argv), ["boot", "home"])
        self.assertIn("virtio-blk-pci,drive=home", argv)

    def test_home_disk_follows_the_data_disk(self) -> None:
        argv = command(data_disk="data.img", home_disk="home.img")
        self.assertEqual(drive_ids(argv), ["boot", "data", "home"])

    def test_commas_in_the_path_are_doubled(self) -> None:
        args = qemu_qmp.home_disk_args("a,b/home.img")
        self.assertIn(",,", args[1])


class OptionTests(unittest.TestCase):
    def test_missing_home_disk_is_a_hint_not_a_format(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            missing = str(Path(tmp) / "home.img")
            with self.assertRaises(SystemExit) as caught:
                qemu_qmp.existing_home_disk(missing)
            self.assertIn("--home-volume", str(caught.exception))
            self.assertFalse(Path(missing).exists())

    def test_unset_means_none_and_existing_file_resolves(self) -> None:
        self.assertIsNone(qemu_qmp.existing_home_disk(None))
        with tempfile.TemporaryDirectory() as tmp:
            path = Path(tmp) / "home.img"
            path.write_bytes(b"x")
            self.assertEqual(qemu_qmp.existing_home_disk(str(path)), path.resolve())


if __name__ == "__main__":
    unittest.main()
