#!/usr/bin/env python3
"""Tests for the launcher's driver choices (issue #497): the sound card and NIC
model reach run_demo as flags, `devd` off reaches the build as LAZYOS_DEVD=0,
and the choices are exactly run_demo's.

Run: python tools/lazygui/test_drivers.py (also run by test_catalog.py)
"""

from __future__ import annotations

import sys
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent.parent))
sys.path.insert(0, str(Path(__file__).resolve().parent.parent / "net"))
import demo_qemu  # noqa: E402
import qemu_net  # noqa: E402
from lazygui import catalog, drivers  # noqa: E402
from lazygui.testplan import demo_argv, demo_config  # noqa: E402


def advanced(**overrides) -> dict:
    """A configuration complete enough for `catalog.build_env`."""
    cfg = demo_config(services=False, xuid=False, xui_client=False, xui_app="(none)",
                      shellprobe=False, msgctl=False, msgrd=False, busybox="")
    cfg.update(overrides)
    return cfg


class DriverChoiceTests(unittest.TestCase):
    def test_defaults_add_nothing(self) -> None:
        argv = demo_argv(sound=True)
        self.assertIn("--sound", argv)
        for flag in ("--sound-card", "--nic", "--no-devd"):
            self.assertNotIn(flag, argv)
        self.assertEqual(catalog.build_env(advanced()).get("LAZYOS_DEVD"), "1")

    def test_the_hda_card_is_passed_with_the_sound_card(self) -> None:
        argv = demo_argv(sound=True, sound_card="hda")
        self.assertEqual(argv[argv.index("--sound-card") + 1], "hda")
        # No sound card, no card model.
        self.assertNotIn("--sound-card", demo_argv(sound=False, sound_card="hda"))

    def test_the_e1000_is_passed_only_with_networking(self) -> None:
        argv = demo_argv(net=True, nic="e1000")
        self.assertIn("--net", argv)
        self.assertEqual(argv[argv.index("--nic") + 1], "e1000")
        self.assertNotIn("--nic", demo_argv(net=False, nic="e1000"))
        # HTTPS brings networking, so the NIC choice too.
        self.assertIn("--nic", demo_argv(tls=True, nic="e1000"))

    def test_devd_off_reaches_the_run_and_the_build(self) -> None:
        self.assertIn("--no-devd", demo_argv(devd=False))
        self.assertEqual(catalog.build_env(advanced(devd=False)).get("LAZYOS_DEVD"), "0")

    def test_interrupt_routing_reaches_the_run_and_the_build(self) -> None:
        # Issue #616: the defaults (I/O APIC, MSI) add no flag but are set.
        env = catalog.build_env(advanced())
        self.assertEqual((env.get("LAZYOS_IRQCHIP"), env.get("LAZYOS_MSI")), ("ioapic", "1"))
        for flag in ("--irqchip", "--no-msi"):
            self.assertNotIn(flag, demo_argv())
        argv = demo_argv(irqchip="pic", msi=False)
        self.assertEqual(argv[argv.index("--irqchip") + 1], "pic")
        self.assertIn("--no-msi", argv)
        env = catalog.build_env(advanced(irqchip="pic", msi=False))
        self.assertEqual((env.get("LAZYOS_IRQCHIP"), env.get("LAZYOS_MSI")), ("pic", "0"))
        self.assertEqual(drivers.IRQCHIPS, demo_qemu.IRQCHIPS)

    def test_the_choices_are_run_demos(self) -> None:
        self.assertEqual(sorted(drivers.SOUND_CARDS), sorted(demo_qemu.SOUND_CARDS))
        self.assertEqual(sorted(drivers.NICS), sorted(demo_qemu.NICS))
        self.assertEqual(sorted(drivers.NICS), sorted(qemu_net.NIC_DEVICES))
        self.assertEqual(drivers.SOUND_CARDS[0], "virtio")
        self.assertEqual(drivers.NICS[0], "virtio")

    def test_run_demo_turns_the_choices_into_qemu_devices(self) -> None:
        hda = demo_qemu.sound_args("none", "hda")
        self.assertIn("intel-hda,id=hda0", hda)
        self.assertIn("hda-output,bus=hda0.0,audiodev=snd0", hda)
        self.assertIn("virtio-sound-pci,audiodev=snd0", demo_qemu.sound_args("none"))
        e1000 = qemu_net.netdev_args([], nic="e1000")
        self.assertIn(f"e1000,netdev={qemu_net.NETDEV_ID}", e1000)


if __name__ == "__main__":
    unittest.main()
