"""The launcher's driver choices (issue #497): which sound card and NIC model
QEMU gets, and whether `init` starts the device manager `devd`. Split out of
`catalog` (file-size budget); the Advanced tab's group is `driveropts`.

The cards are run_demo flags (`--sound-card`, `--nic`); `devd` is the build
switch `LAZYOS_DEVD` (on by default, `run_demo.py --no-devd` turns it off).
Interrupt routing (issue #616) is `LAZYOS_IRQCHIP` (`--irqchip`) and
`LAZYOS_MSI` (`--no-msi`). The Intel GPU service `gpud` is `LAZYOS_INTELGFX`
(`--intelgfx`, docs/kabylake-gpu-plan.md): devd starts it for the HD Graphics
620 it finds, so the image needs devd and a driver stack to start it from.
USB audio is `LAZYOS_USB_AUDIO` (`--usb-audio`, docs/usb-audio-plan.md):
`usbd` drives USB speakers, headsets and DACs and `sndd usb=1` serves them as
an output beside the sound card, so it brings the USB and sound stacks.
"""

from __future__ import annotations

import os

from demo_qemu import sound_args

from .netplan import wants_net

#: `run_demo.py --sound-card` values, default first.
SOUND_CARDS = ["virtio", "hda"]
#: `run_demo.py --nic` values, default first.
NICS = ["virtio", "e1000"]
#: `run_demo.py --irqchip` values, default first.
IRQCHIPS = ["ioapic", "pic"]


def driver_env(cfg: dict) -> dict[str, str]:
    """`LAZYOS_DEVD`, `LAZYOS_IRQCHIP`, `LAZYOS_MSI` and `LAZYOS_INTELGFX` as
    the launcher chose them, so an inherited value never overrides the
    controls. `gpud` is started by devd, which needs a driver stack: the
    launcher always builds the network stack (`netplan.wants_dbgd`), so the
    switch needs nothing more here (`run_demo.py --intelgfx` itself adds the
    sound stack when it is run with none)."""
    return {
        "LAZYOS_DEVD": "1" if cfg.get("devd", True) else "0",
        "LAZYOS_IRQCHIP": cfg.get("irqchip", IRQCHIPS[0]),
        "LAZYOS_MSI": "1" if cfg.get("msi", True) else "0",
        "LAZYOS_INTELGFX": "1" if cfg.get("intelgfx") else "0",
        "LAZYOS_USB_AUDIO": "1" if cfg.get("usb_audio") else "0",
    } | ({"LAZYOS_USB": "1", "LAZYOS_SOUND": "1"} if cfg.get("usb_audio") else {})


def device_flags(cfg: dict) -> list[str]:
    """run_demo's sound card and NIC flags, and `--no-devd`. The sound card
    goes on the host's audio backend; run_demo also builds with
    LAZYOS_SOUND=1 (the desktop profile ships the sound stack anyway, and on
    other images the driver plays its boot tones)."""
    flags: list[str] = []
    if cfg.get("sound"):
        flags.append("--sound")
        card = cfg.get("sound_card", SOUND_CARDS[0])
        if card != SOUND_CARDS[0]:
            flags += ["--sound-card", card]
    nic = cfg.get("nic", NICS[0])
    if wants_net(cfg) and nic != NICS[0]:
        flags += ["--nic", nic]
    if not cfg.get("devd", True):
        flags.append("--no-devd")
    if cfg.get("irqchip", IRQCHIPS[0]) != IRQCHIPS[0]:
        flags += ["--irqchip", cfg["irqchip"]]
    if not cfg.get("msi", True):
        flags.append("--no-msi")
    if cfg.get("intelgfx"):
        flags.append("--intelgfx")
    if cfg.get("usb_audio"):
        flags.append("--usb-audio")
    return flags


def session_sound(cfg: dict) -> list[str]:
    """The sound card for the headless modes (screenshots, scripted
    sessions), as `--extra-arg` flags: when the run asks for sound or carries
    an app that plays it (emusic). The card records to `<out>/sound.wav`
    instead of the host's speakers, so a headless run stays quiet and leaves
    a recording to judge."""
    if not (cfg.get("sound") or cfg.get("emusic") or cfg.get("usb_audio")):
        return []
    wav = os.path.join(cfg.get("out") or ".", "sound.wav")
    card = cfg.get("sound_card", SOUND_CARDS[0])
    usb = bool(cfg.get("usb_audio"))
    return [f"--extra-arg={arg}" for arg in sound_args(f"wav:{wav}", card, usb)]
