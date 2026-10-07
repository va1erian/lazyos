"""The interrupt path a harness builds and judges (issue #616).

The net, sound and USB harnesses run each driver on either path:

* ``msi`` (the default): the legacy lines on the I/O APIC
  (``LAZYOS_IRQCHIP=ioapic``) and message interrupts on
  (``LAZYOS_MSI=1``). A device that has MSI or MSI-X must get a vector:
  the kernel prints ``DEV:MSI:PASS`` the first time one delivers, and the
  driver reports its mode (``NETDRV:IRQ:MsiX``, ``SNDD:IRQ:Msi``,
  ``USBD:IRQ hc=0 armed MsiX``).
* ``pic``: the 8259 (``LAZYOS_IRQCHIP=pic``) and INTx only
  (``LAZYOS_MSI=0``), the path every driver took before.

Use: ``add_option(parser)``, merge ``build_env(args.irq_path)`` into the
build's environment, and fail the run on ``judge(...)``'s complaint.
"""

from __future__ import annotations

import argparse
import re

#: `--irq-path` values, default first.
PATHS = ["msi", "pic"]


def add_option(parser: argparse.ArgumentParser) -> None:
    parser.add_argument(
        "--irq-path", choices=PATHS, default=PATHS[0],
        help="interrupt path to build and judge (issue #616): msi = I/O APIC + "
             "MSI/MSI-X (default), pic = the 8259 and INTx only")


def build_env(path: str) -> dict[str, str]:
    """The build switches for `path`."""
    if path == "pic":
        return {"LAZYOS_IRQCHIP": "pic", "LAZYOS_MSI": "0"}
    return {"LAZYOS_IRQCHIP": "ioapic", "LAZYOS_MSI": "1"}


def label(path: str) -> str:
    return " ".join(f"{name}={value}" for name, value in build_env(path).items())


def driver_mode(text: str, marker: str) -> str | None:
    """The mode a driver reported after `marker` (`Intx`, `Msi`, `MsiX`)."""
    found = re.search(re.escape(marker) + r"(Intx|MsiX|Msi)\b", text)
    return found.group(1) if found else None


def judge(text: str, path: str, marker: str, message_capable: bool) -> str | None:
    """Why the serial log `text` does not show `path`, or None when it does.

    `marker` is the driver's mode report up to the mode (`"SNDD:IRQ:"`);
    `message_capable` says whether the device QEMU gave it has MSI or MSI-X
    (an e1000 has neither and stays on INTx on either path). A driver that
    never armed its interrupt (a polled run) is not judged here."""
    mode = driver_mode(text, marker)
    if path == "pic":
        if "HW:IRQCHIP:pic" not in text:
            return "the legacy lines were not left on the 8259"
        if "DEV:MSI:PASS" in text:
            return "a message interrupt was delivered with LAZYOS_MSI=0"
        if mode not in (None, "Intx"):
            return f"the driver took {mode} with LAZYOS_MSI=0"
        return None
    if "HW:IRQCHIP:ioapic" not in text:
        return "the legacy lines did not move to the I/O APIC"
    if mode is None:
        return None
    if message_capable:
        if mode == "Intx":
            return "a message-capable device was left on INTx"
        if "DEV:MSI:PASS" not in text:
            return f"the driver took {mode} but no message interrupt was delivered"
    elif mode != "Intx":
        return f"a device without MSI reported {mode}"
    return None
