"""The Tk variables behind every launcher control, and their defaults.

Split out of `ui.py` (file-size budget): one table, so a new control is a new
row here plus its widget there.
"""

from __future__ import annotations

import os
import shutil
import tkinter as tk

from .catalog import (DATA_IMAGE, DEFAULT_MEMORY, HOME_IMAGE, MODES, SCRIPTS, SIMPLE_BUILDS,
                      SIMPLE_INTERFACES)


def make_vars() -> dict:
    """Create every Tk variable backing the controls, with sensible defaults."""
    s = tk.StringVar
    b = tk.BooleanVar
    return {
        "mode": s(value=MODES[0][0]),
        "profile": s(value="dev"),
        "accel": s(value="auto"),
        "disk": s(value="virtio"),
        "home_path": s(value=HOME_IMAGE),
        "data_path": s(value=DATA_IMAGE),
        "memory": s(value=DEFAULT_MEMORY),
        "limits": s(value=""),
        "display_mode": s(value=""),
        "assets": s(value=""),
        "times": s(value="10,14,18"),
        "timeout": s(value="180"),
        "abi_time": s(value="8"),
        "abi_only": s(value=""),
        "extra": s(value=""),
        "out": s(value="shots"),
        "qemu": s(value=find_qemu()),
        "busybox": s(value=""),
        "skip_build": b(value=False),
        "headless": b(value=False),
        "tablet": b(value=False),
        "sound": b(value=True),
        # The driver choices (issue #497, `drivers`): QEMU's sound card and
        # NIC model, and the device manager.
        "sound_card": s(value="virtio"),
        "nic": s(value="virtio"),
        "devd": b(value=True),
        "abi_build": b(value=False),
        "home_disk": b(value=True),
        "data_disk": b(value=False),
        "reset_os": b(value=False),
        "desktop": b(value=False),
        "services": b(value=False),
        "xuid": b(value=False),
        "shellprobe": b(value=False),
        "msgctl": b(value=False),
        "msgrd": b(value=False),
        "xui_client": b(value=False),
        "xui_app": s(value="(none)"),
        "xui_autostart": s(value=""),
        "lazyrad": b(value=False),
        "usb_image": b(value=False),
        # LazyShell on the desktop profile (issue #157); unchecked = LAZYOS_SHELL=0.
        "shell": b(value=True),
        "lazyrad_samples": s(value=""),
        "simple_lazyrad": b(value=False),
        "simple_shell": b(value=True),
        "devices": b(value=False),
        "simple_devices": b(value=False),
        "doom": b(value=False),
        "simple_doom": b(value=False),
        "modplayer": b(value=False),
        "simple_modplayer": b(value=False),
        # Networking (LAZYOS_NETD + a QEMU user-mode card, run_demo --net).
        "net": b(value=False),
        "net_forwards": s(value=""),
        "net_restrict": b(value=False),
        "simple_net": b(value=False),
        "linuxapps": b(value=False),
        "simple_linuxapps": b(value=False),
        # The HTTPS clients (LAZYOS_TLS, run_demo --tls; implies networking).
        "tls": b(value=False),
        "simple_tls": b(value=False),
        # An ext2 journal on the OS volume (LAZYOS_JOURNAL, run_demo --journal).
        "journal": b(value=False),
        # The LazyWeb browser (LAZYOS_LAZYWEB, run_demo --lazyweb; implies the
        # desktop, networking and HTTPS).
        "lazyweb": b(value=False),
        "simple_lazyweb": b(value=False),
        # The Mail app (LAZYOS_MAIL, run_demo --mail; implies HTTPS).
        "mail": b(value=False),
        "simple_mail": b(value=False),
        "simple_hidpi": b(value=False),
        "script": s(value=SCRIPTS[0][1]),
        "simple_build": s(value=SIMPLE_BUILDS[0][0]),
        "simple_iface": s(value=SIMPLE_INTERFACES[0][0]),
    }


def find_qemu() -> str:
    """Best-effort QEMU path: PATH first, then the usual Windows install."""
    found = shutil.which("qemu-system-x86_64")
    if found:
        return found
    if os.name == "nt":
        common = r"C:\Program Files\qemu\qemu-system-x86_64.exe"
        if os.path.isfile(common):
            return common
    return ""
