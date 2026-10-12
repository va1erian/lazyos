"""The Simple tab: pick a build type, an interface and feature groups, press Start."""

from __future__ import annotations

from tkinter import ttk

from .catalog import SIMPLE_BUILDS, SIMPLE_GROUPS, SIMPLE_INTERFACES, SIMPLE_OPTIONS, group_switches


#: `catalog.simple_config`'s switches, in its argument order.
SIMPLE_EXTRAS = ("lazyrad", "shell", "devices", "doom", "quake", "modplayer", "net", "linuxapps",
                 "hidpi", "tls", "lazyweb", "mail", "traydemo", "autologin", "setup", "pictures",
                 "emusic", "usb_image", "wifi", "lazysky", "srb2kart")

_OPTION_TEXT = {
    "shell": "LazyShell desktop (taskbar, start menu, desktop icons)",
    "autologin": "Log in automatically as user (skip the login screen; "
                 "passwords: docs/security-model.md)",
    "setup": "First-boot setup: no account yet, the login screen asks "
             "for the owner (recreates the OS volume)",
    "devices": "Open the Devices app at boot (device owners and driver rules)",
    "hidpi": "HiDPI: 2560x1440 at 2x (a sharp 720p desktop for a 4K monitor)",
    "usb_image": "USB stick image too (target/lazyos-usb.img for a real PC; "
                 "write it with tools/boot/stick_gui.py)",
}


def simple_choice(build_label: str, iface_label: str) -> tuple[str, str]:
    """Map the tab's radio labels to ``(cargo profile, interface)``."""
    return dict(SIMPLE_BUILDS)[build_label], iface_label


def simple_flags(v: dict) -> dict[str, bool]:
    """The `simple_config` keyword switches for the tab's Tk variables ``v``
    (``simple_group_<key>`` per group, ``simple_<name>`` per option)."""
    chosen = {key for key, *_ in SIMPLE_GROUPS if v[f"simple_group_{key}"].get()}
    flags = group_switches(chosen)
    flags.update({name: v[f"simple_{name}"].get() for name in SIMPLE_OPTIONS})
    return flags


def build_simple_tab(parent: ttk.Frame, v: dict, on_start) -> None:
    """Populate ``parent`` with the build, the interface, the feature groups and
    the Start button; ``v`` is the launcher's Tk variable table."""
    ttk.Label(parent, text="Start LazyOS", font=("TkDefaultFont", 14, "bold")
              ).pack(anchor="w", padx=10, pady=(12, 2))
    ttk.Label(parent, text="Choose how to build and what to boot, then press Start. "
                           "Machine settings (accelerator, disk bus, memory, QEMU path) come "
                           "from the Advanced tab.",
              wraplength=440, foreground="#444").pack(anchor="w", padx=10, pady=(0, 8))

    build = ttk.LabelFrame(parent, text="Build")
    build.pack(fill="x", padx=8, pady=6)
    for label, profile in SIMPLE_BUILDS:
        hint = ("faster to build, best under QEMU" if profile == "dev"
                else "optimized for real hardware (slow full LTO build)")
        ttk.Radiobutton(build, text=f"{label} - {hint}", value=label,
                        variable=v["simple_build"]).pack(anchor="w", padx=8, pady=2)

    iface = ttk.LabelFrame(parent, text="Interface")
    iface.pack(fill="x", padx=8, pady=6)
    for label, desc in SIMPLE_INTERFACES:
        ttk.Radiobutton(iface, text=label, value=label,
                        variable=v["simple_iface"]).pack(anchor="w", padx=8, pady=(4, 0))
        ttk.Label(iface, text=desc, wraplength=420, foreground="#555"
                  ).pack(anchor="w", padx=28, pady=(0, 4))

    groups = ttk.LabelFrame(parent, text="Features (the Desktop always has the Picture Viewer)")
    groups.pack(fill="x", padx=8, pady=6)
    for key, label, desc, _names in SIMPLE_GROUPS:
        ttk.Checkbutton(groups, text=label, variable=v[f"simple_group_{key}"]
                        ).pack(anchor="w", padx=8, pady=(4, 0))
        ttk.Label(groups, text=desc, wraplength=420, foreground="#555"
                  ).pack(anchor="w", padx=28, pady=(0, 4))

    options = ttk.LabelFrame(parent, text="Options")
    options.pack(fill="x", padx=8, pady=6)
    for name in SIMPLE_OPTIONS:
        ttk.Checkbutton(options, text=_OPTION_TEXT[name], variable=v[f"simple_{name}"]
                        ).pack(anchor="w", padx=8, pady=4)

    ttk.Button(parent, text="Start LazyOS", command=on_start
               ).pack(anchor="w", padx=10, pady=12)
