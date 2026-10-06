"""QEMU devices `tools/run_demo.py` derives from its flags: the sound card, the
NIC model, and the `devd` switch (issue #497)."""

from __future__ import annotations

import argparse
import sys
from pathlib import Path

#: `--sound-card`: the QEMU devices of each card (`{dev}` is the audiodev id).
#: QEMU's ICH6 HDA controller works on either machine type.
SOUND_CARDS = {
    "virtio": ["virtio-sound-pci,audiodev={dev}"],
    "hda": ["intel-hda,id=hda0", "hda-output,bus=hda0.0,audiodev={dev}"],
}
#: `--nic`: the QEMU device of each NIC model (`tools/net/qemu_net.py`).
NICS = ("virtio", "e1000")


def add_device_options(parser: argparse.ArgumentParser) -> None:
    """`--sound`, `--sound-card`, `--nic` and `--no-devd`."""
    parser.add_argument("--sound", nargs="?", const="auto", metavar="BACKEND",
                        help="attach a sound card (--sound-card) and build with LAZYOS_SOUND=1, "
                             "which boots the `sndd` driver and plays its test tones. "
                             "BACKEND is a QEMU -audiodev driver (dsound, pa, alsa, sdl, "
                             "none, ...) or wav:PATH; default: this OS's usual one")
    parser.add_argument("--sound-card", choices=sorted(SOUND_CARDS), default="virtio",
                        help="with --sound: virtio-sound (default) or an Intel HDA controller "
                             "with a line-out codec (issue #497); `sndd` drives either")
    parser.add_argument("--nic", choices=NICS, default="virtio",
                        help="with --net: virtio-net (default) or an Intel 8254x (QEMU's "
                             "e1000, issue #497); `netdrv` drives either")
    parser.add_argument("--no-devd", action="store_true",
                        help="build without the device manager (LAZYOS_DEVD=0): `init` starts "
                             "the drivers at boot and each finds its own device")


def device_env(args: argparse.Namespace, env: dict) -> None:
    """The build switches the device options set."""
    if args.no_devd:
        env["LAZYOS_DEVD"] = "0"


def sound_args(backend: str, card: str = "virtio") -> list[str]:
    """QEMU arguments for a `card` sound card on `backend` (see `--sound`)."""
    if backend == "auto":
        backend = {"win32": "dsound", "darwin": "coreaudio"}.get(sys.platform, "pa")
    if backend.startswith("wav:"):
        # A comma in a path is doubled for QEMU's option parser.
        path = Path(backend[4:]).resolve().as_posix().replace(",", ",,")
        audiodev = f"wav,id=snd0,path={path}"
    else:
        audiodev = f"{backend},id=snd0"
    args = ["-audiodev", audiodev]
    for device in SOUND_CARDS[card]:
        args += ["-device", device.format(dev="snd0")]
    return args
