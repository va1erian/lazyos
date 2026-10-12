"""qemu_spike.py's command line (no QEMU is started)."""

import tempfile
import unittest
from pathlib import Path

import qemu_spike

BASE = dict(memory="2G", serial=Path("/t/serial.log"), qmp_port=4555, code=Path("/f/code.fd"),
            vars_copy=Path("/t/vars.fd"), tftp_dir=Path("/t/ipxe"))


def command(**over):
    return qemu_spike.build_command("qemu", **{**BASE, "mode": "pxe", **over})


def pairs(cmd, flag):
    return [cmd[i + 1] for i, c in enumerate(cmd) if c == flag]


class CommandTests(unittest.TestCase):
    def test_no_disk_only_the_two_pflash_units(self):
        drives = pairs(command(), "-drive")
        self.assertEqual(len(drives), 2)
        self.assertIn("unit=0,readonly=on", drives[0])
        self.assertIn("unit=1", drives[1])
        self.assertTrue(all(d.startswith("if=pflash,format=raw") for d in drives))
        text = " ".join(command())
        for disk in ("virtio-blk", "usb-storage", "ide-hd", "nvme", "-cdrom", "-hda"):
            self.assertNotIn(disk, text)

    def test_pxe_mode_boots_ipxe_over_slirp_tftp_with_the_rom_off(self):
        cmd = command()
        netdev = pairs(cmd, "-netdev")[0]
        self.assertIn("tftp=", netdev)
        self.assertIn("bootfile=ipxe.efi", netdev)
        self.assertTrue(netdev.startswith("user,id=n0"))
        nic = [d for d in pairs(cmd, "-device") if d.startswith("virtio-net-pci")][0]
        self.assertIn("netdev=n0", nic)
        self.assertTrue(nic.endswith("romfile="))

    def test_rom_mode_keeps_the_nics_own_rom_and_sets_no_bootfile(self):
        cmd = command(mode="rom")
        self.assertNotIn("bootfile", pairs(cmd, "-netdev")[0])
        self.assertIn("tftp=", pairs(cmd, "-netdev")[0])
        nic = [d for d in pairs(cmd, "-device") if d.startswith("virtio-net-pci")][0]
        self.assertNotIn("romfile", nic)

    def test_usb_input_serial_and_qmp(self):
        cmd = command()
        devices = pairs(cmd, "-device")
        self.assertIn("qemu-xhci,id=xhci", devices)
        self.assertIn("usb-kbd,bus=xhci.0", devices)
        self.assertIn("usb-mouse,bus=xhci.0", devices)
        self.assertTrue(pairs(cmd, "-serial")[0].startswith("file:"))
        self.assertEqual(pairs(cmd, "-qmp"), ["tcp:127.0.0.1:4555,server=on,wait=off"])

    def test_options(self):
        cmd = command(headless=True, no_reboot=True, machine="pc", accel_args=["-accel", "whpx"],
                      extra=["-S"])
        self.assertEqual(pairs(cmd, "-machine"), ["pc"])
        self.assertEqual(pairs(cmd, "-display"), ["none"])
        self.assertIn("-no-reboot", cmd)
        self.assertIn("whpx", cmd)
        self.assertEqual(cmd[-1], "-S")
        self.assertEqual(pairs(command(), "-m"), ["2G"])
        self.assertNotIn("-display", command())

    def test_commas_in_paths_are_doubled(self):
        self.assertEqual(qemu_spike.qemu_path(Path("/a,b/c")).split("/")[-2:], ["a,,b", "c"])

    def test_bad_mode(self):
        with self.assertRaises(ValueError):
            command(mode="http")

    def test_parse_times(self):
        self.assertEqual(qemu_spike.parse_times("60, 20,"), [20.0, 60.0])
        self.assertEqual(qemu_spike.parse_times(""), [])


class FirmwareTests(unittest.TestCase):
    def test_qemu_share_dir_then_i386_vars_fallback(self):
        with tempfile.TemporaryDirectory() as tmp:
            share = Path(tmp) / "share"
            share.mkdir()
            (share / "edk2-x86_64-code.fd").write_bytes(b"c")
            (share / "edk2-i386-vars.fd").write_bytes(b"v")
            qemu = str(Path(tmp) / "qemu-system-x86_64.exe")
            code = qemu_spike.find_firmware(qemu, None, qemu_spike._SHARE_CODE, [], "code")
            vars_ = qemu_spike.find_firmware(qemu, None, qemu_spike._SHARE_VARS, [], "vars")
            self.assertEqual(code.name, "edk2-x86_64-code.fd")
            self.assertEqual(vars_.name, "edk2-i386-vars.fd")
            (share / "edk2-x86_64-vars.fd").write_bytes(b"v")
            self.assertEqual(qemu_spike.find_firmware(qemu, None, qemu_spike._SHARE_VARS, [], "vars").name,
                             "edk2-x86_64-vars.fd")

    def test_missing_firmware_exits(self):
        with tempfile.TemporaryDirectory() as tmp:
            with self.assertRaises(SystemExit):
                qemu_spike.find_firmware(str(Path(tmp) / "q"), None, ["nope.fd"], [], "code")
            with self.assertRaises(SystemExit):
                qemu_spike.find_firmware("q", str(Path(tmp) / "missing.fd"), [], [], "code")


if __name__ == "__main__":
    unittest.main()
