"""The Tk variables behind every launcher control, and their defaults.

Split out of `ui.py` (file-size budget): one table, so a new control is a new
row here plus its widget there.
"""

from __future__ import annotations

import os
import shutil
import tkinter as tk

from .catalog import DATA_IMAGE, MODES, SCRIPTS, SIMPLE_BUILDS, SIMPLE_INTERFACES


def make_vars() -> dict:
    """Create every Tk variable backing the controls, with sensible defaults."""
    s = tk.StringVar
    b = tk.BooleanVar
    return {
        "mode": s(value=MODES[0][0]),
        "profile": s(value="dev"),
        "accel": s(value="auto"),
        "disk": s(value="virtio"),
        "data_path": s(value=DATA_IMAGE),
        "memory": s(value="256M"),
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
        "abi_build": b(value=False),
        "data_disk": b(value=True),
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
        # LazyShell on the desktop profile (issue #157); unchecked = LAZYOS_SHELL=0.
        "shell": b(value=True),
        "lazyrad_samples": s(value=""),
        "simple_lazyrad": b(value=False),
        "simple_shell": b(value=True),
        "devices": b(value=False),
        "simple_devices": b(value=False),
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
