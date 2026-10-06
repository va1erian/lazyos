"""The launcher's driver choices (issue #497): which sound card and NIC model
QEMU gets, and whether `init` starts the device manager `devd`. Split out of
`catalog` (file-size budget); the Advanced tab's group is `driveropts`.

The cards are run_demo flags (`--sound-card`, `--nic`); `devd` is the build
switch `LAZYOS_DEVD` (on by default, `run_demo.py --no-devd` turns it off).
"""

from __future__ import annotations

from .netplan import wants_net

#: `run_demo.py --sound-card` values, default first.
SOUND_CARDS = ["virtio", "hda"]
#: `run_demo.py --nic` values, default first.
NICS = ["virtio", "e1000"]


def driver_env(cfg: dict) -> dict[str, str]:
    """`LAZYOS_DEVD` as the launcher chose it, so an inherited value never
    overrides the checkbox."""
    return {"LAZYOS_DEVD": "1" if cfg.get("devd", True) else "0"}


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
    return flags
