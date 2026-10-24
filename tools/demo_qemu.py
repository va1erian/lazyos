"""QEMU devices `tools/run_demo.py` derives from its flags: the sound card, the
NIC model, the `devd` switch (issue #497) and a USB audio device
(docs/usb-audio-plan.md)."""

from __future__ import annotations

import argparse
import subprocess
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent

#: `--sound-card`: the QEMU devices of each card (`{dev}` is the audiodev id).
#: QEMU's ICH6 HDA controller works on either machine type.
SOUND_CARDS = {
    "virtio": ["virtio-sound-pci,audiodev={dev}"],
    "hda": ["intel-hda,id=hda0", "hda-output,bus=hda0.0,audiodev={dev}"],
}
#: `--usb-audio`: QEMU's UAC 1.0 speaker on an xHCI controller of its own, on
#: the same audiodev as the sound card, so the guest has two outputs to pick.
USB_AUDIO = ["qemu-xhci,id=xhciaudio", "usb-audio,bus=xhciaudio.0,audiodev={dev}"]
#: `--nic`: the QEMU device of each NIC model (`tools/net/qemu_net.py`).
NICS = ("virtio", "e1000")


#: `--irqchip` values, default first (issue #616).
IRQCHIPS = ["ioapic", "pic"]


def boot_disk_args(disk: str, image) -> list[str]:
    """The QEMU arguments of the boot disk (`--disk`).

    virtio-blk is DMA-based; the IDE/PIO path costs a VM exit per 16 bits read,
    which made loading the ~2.7 MB desktop ELFs take tens of seconds."""
    if disk == "virtio":
        return ["-drive", f"format=raw,file={image},if=none,id=boot",
                "-device", "virtio-blk-pci,drive=boot"]
    if disk == "ahci":
        # Its own AHCI controller (works on every machine type), the image the
        # only disk on it: the kernel must find it as ahci0.
        return ["-device", "ahci,id=ahcib",
                "-drive", f"format=raw,file={image},if=none,id=boot",
                "-device", "ide-hd,drive=boot,bus=ahcib.0"]
    return ["-drive", f"format=raw,file={image}"]


def add_device_options(parser: argparse.ArgumentParser) -> None:
    """`--sound`, `--sound-card`, `--usb-audio`, `--nic`, `--no-devd`,
    `--irqchip`, `--no-msi` and `--intelgfx`."""
    parser.add_argument("--sound", nargs="?", const="auto", metavar="BACKEND",
                        help="attach a sound card (--sound-card) and build with LAZYOS_SOUND=1, "
                             "which boots the `sndd` driver and plays its test tones. "
                             "BACKEND is a QEMU -audiodev driver (dsound, pa, alsa, sdl, "
                             "none, ...) or wav:PATH; default: this OS's usual one")
    parser.add_argument("--sound-card", choices=sorted(SOUND_CARDS), default="virtio",
                        help="with --sound: virtio-sound (default) or an Intel HDA controller "
                             "with a line-out codec (issue #497); `sndd` drives either")
    parser.add_argument("--usb-audio", action="store_true",
                        help="USB speakers, headsets and DACs (LAZYOS_USB_AUDIO=1 with "
                             "LAZYOS_USB=1 and the sound stack, docs/usb-audio-plan.md): `usbd` "
                             "drives the device and `sndd usb=1` serves it as a second output "
                             "the Volume tray menu and `mixer outputs` offer; implies --sound, "
                             "and QEMU gets a `usb-audio` device next to the sound card. "
                             "Opt-in until it has run on the Kaby Lake and Alder Lake boxes "
                             "(add --usb-image --dbgd --build-only for their stick)")
    parser.add_argument("--nic", choices=NICS, default="virtio",
                        help="with --net: virtio-net (default) or an Intel 8254x (QEMU's "
                             "e1000, issue #497); `netdrv` drives either")
    parser.add_argument("--no-devd", action="store_true",
                        help="build without the device manager (LAZYOS_DEVD=0): `init` starts "
                             "the drivers at boot and each finds its own device")
    parser.add_argument("--irqchip", choices=IRQCHIPS, default=IRQCHIPS[0],
                        help="interrupt controller for the legacy lines (LAZYOS_IRQCHIP, "
                             "issue #616): the I/O APIC (default, when the MADT names one) "
                             "or the 8259 PIC")
    parser.add_argument("--no-msi", action="store_true",
                        help="build with LAZYOS_MSI=0: drivers take INTx lines, never "
                             "MSI or MSI-X vectors")
    parser.add_argument("--intelgfx", action="store_true",
                        help="the Intel GPU service (LAZYOS_INTELGFX=1, docs/kabylake-gpu-plan.md "
                             "stage G1): `gpud`, which devd starts for the HD Graphics 620 "
                             "(8086:5916) it finds and which READS AND REPORTS the display state "
                             "the firmware left (`GPU:*` lines; `os.lazy.gpu.v1`). Read-only, "
                             "off by default. QEMU has no such GPU (gpud then does not start): "
                             "it matters on the Kaby Lake box, e.g. with --usb-image --dbgd "
                             "--desktop --build-only. Without --desktop, --sound or --net it "
                             "builds a services image with the sound stack so devd exists")


def device_env(args: argparse.Namespace, env: dict) -> None:
    """The build switches the device options set: `LAZYOS_DEVD` either way,
    so a value inherited from the environment never overrides `--no-devd`."""
    env["LAZYOS_DEVD"] = "0" if args.no_devd else "1"
    # The SMP kernel (docs/smp-plan.md S8.3): every CPU runs tasks.
    env["LAZYOS_SMP"] = "1" if args.smp_kernel else "0"
    env["LAZYOS_IRQCHIP"] = args.irqchip
    env["LAZYOS_MSI"] = "0" if args.no_msi else "1"
    # Either way, so an inherited value never overrides `--usb-audio`.
    env["LAZYOS_USB_AUDIO"] = "1" if args.usb_audio else "0"
    if args.usb_audio:
        env["LAZYOS_USB"] = "1"
        env["LAZYOS_SOUND"] = "1"
        if not args.desktop:
            env.setdefault("LAZYOS_SERVICES", "1")
    # Either way, so an inherited value never overrides `--intelgfx`.
    env["LAZYOS_INTELGFX"] = "1" if args.intelgfx else "0"
    if args.intelgfx and not (args.desktop or args.net or args.sound):
        # `devd` starts `gpud`, and `devd` is in an image that has a driver
        # stack and `init`: the sound stack, with no card attached.
        env["LAZYOS_SOUND"] = "1"
        env.setdefault("LAZYOS_SERVICES", "1")


def sound_args(backend: str, card: str = "virtio", usb_audio: bool = False) -> list[str]:
    """QEMU arguments for a `card` sound card on `backend` (see `--sound`), and
    a USB audio device on the same backend with `usb_audio`."""
    if backend == "auto":
        backend = {"win32": "dsound", "darwin": "coreaudio"}.get(sys.platform, "pa")
    if backend.startswith("wav:"):
        # A comma in a path is doubled for QEMU's option parser.
        path = Path(backend[4:]).resolve().as_posix().replace(",", ",,")
        audiodev = f"wav,id=snd0,path={path}"
    else:
        audiodev = f"{backend},id=snd0"
    args = ["-audiodev", audiodev]
    for device in SOUND_CARDS[card] + (USB_AUDIO if usb_audio else []):
        args += ["-device", device.format(dev="snd0")]
    return args


# --- USB sticks plugged into the guest (`--usb-stick`) ------------------------
# A stick is a raw image on one xHCI controller, each its own drive and device
# so QEMU's monitor can unplug and plug it (`device_del usbstick0`, `device_add
# usb-storage,bus=xhci.0,drive=ustk0,id=usbstick0`). A missing image is made as
# an empty 64 MiB FAT32 stick by `libs/fatvol`'s `fatstick`; an existing one is
# never touched.


def prepare_usb_stick(path: Path) -> bool:
    """Make sure the stick image exists."""
    if path.is_file():
        return True
    path.parent.mkdir(parents=True, exist_ok=True)
    command = ["cargo", "run", "-q", "-p", "fatvol", "--example", "fatstick", "--",
               "make", str(path.resolve()), "--size", "64M", "--label", "LAZYFAT"]
    print(f"creating the USB stick {path} (64 MiB FAT32, label LAZYFAT)", flush=True)
    if subprocess.run(command, cwd=ROOT).returncode != 0 or not path.is_file():
        print(f"could not create the USB stick {path}", file=sys.stderr)
        return False
    return True


def usb_sticks(paths: list[str]) -> list[str] | None:
    """The QEMU arguments for the sticks at `paths`, after making any that is
    missing; `None` when one could not be made."""
    sticks = [Path(p) for p in paths]
    return usb_stick_args(sticks) if all(prepare_usb_stick(stick) for stick in sticks) else None


# The removable-drives tray applet, shipped by a desktop with the USB stack
# (`build_support/xui_embed.rs` USB_XUI_APPS).
USB_APPS = [ROOT / "target" / "xui" / "xui-removable.elf"]


def usb_desktop(args) -> bool:
    """Whether this build ships the USB stack (`LAZYOS_USB=1`), which brings the
    removable-drives tray applet."""
    return (bool(args.usb_stick) or bool(args.usb_image) or bool(args.netboot)
            or bool(getattr(args, "usb_audio", False)))


def usb_env(sticks: list[str], desktop: bool) -> dict[str, str]:
    """Sticks in the guest need the driver and a session that starts it."""
    if not sticks:
        return {}
    return {"LAZYOS_USB": "1"} | ({} if desktop else {"LAZYOS_SERVICES": "1"})


def usb_stick_args(sticks: list[Path]) -> list[str]:
    """The controller and one `usb-storage` per stick; nothing for none."""
    if not sticks:
        return []
    args = ["-device", "qemu-xhci,id=xhci"]
    for index, stick in enumerate(sticks):
        args += ["-drive", f"format=raw,file={stick},if=none,id=ustk{index}",
                 "-device", f"usb-storage,bus=xhci.0,drive=ustk{index},id=usbstick{index}"]
    return args
