"""The Simple tab: pick a build type and an interface, press Start."""

from __future__ import annotations

from tkinter import ttk

from .catalog import SIMPLE_BUILDS, SIMPLE_INTERFACES


def simple_choice(build_label: str, iface_label: str) -> tuple[str, str]:
    """Map the tab's radio labels to ``(cargo profile, interface)``."""
    return dict(SIMPLE_BUILDS)[build_label], iface_label


def build_simple_tab(parent: ttk.Frame, build_var, iface_var, lazyrad_var, shell_var,
                     devices_var, doom_var, modplayer_var, on_start) -> None:
    """Populate ``parent`` with the two choices and the Start button.

    ``build_var``/``iface_var`` are Tk string variables holding a
    ``SIMPLE_BUILDS`` / ``SIMPLE_INTERFACES`` label; ``lazyrad_var``,
    ``shell_var``, ``devices_var``, ``doom_var`` and ``modplayer_var`` are Tk
    booleans for the LazyRAD IDE, the LazyShell desktop, opening the Devices app
    at boot, the Doom package and the LazyRAD MOD player package; ``on_start``
    runs the plan.
    """
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
                        variable=build_var).pack(anchor="w", padx=8, pady=2)

    iface = ttk.LabelFrame(parent, text="Interface")
    iface.pack(fill="x", padx=8, pady=6)
    for label, desc in SIMPLE_INTERFACES:
        ttk.Radiobutton(iface, text=label, value=label,
                        variable=iface_var).pack(anchor="w", padx=8, pady=(4, 0))
        ttk.Label(iface, text=desc, wraplength=420, foreground="#555"
                  ).pack(anchor="w", padx=28, pady=(0, 4))

    apps = ttk.LabelFrame(parent, text="Extras (Desktop)")
    apps.pack(fill="x", padx=8, pady=6)
    ttk.Checkbutton(apps, text="LazyShell desktop (taskbar, start menu, desktop icons)",
                    variable=shell_var).pack(anchor="w", padx=8, pady=4)
    ttk.Checkbutton(apps, text="LazyRAD IDE (builds it; add it in Settings -> Menu)",
                    variable=lazyrad_var).pack(anchor="w", padx=8, pady=4)
    ttk.Checkbutton(apps, text="Open the Devices app at boot (device owners and driver rules)",
                    variable=devices_var).pack(anchor="w", padx=8, pady=4)
    ttk.Checkbutton(apps, text="Doom (builds the package; install it with "
                               "`pkgctl install /DOOM.LZP`)",
                    variable=doom_var).pack(anchor="w", padx=8, pady=4)
    ttk.Checkbutton(apps, text="MOD player made with LazyRAD (builds the package; install "
                               "it with `pkgctl install /MODPLAY.LZP`)",
                    variable=modplayer_var).pack(anchor="w", padx=8, pady=4)

    ttk.Button(parent, text="Start LazyOS", command=on_start
               ).pack(anchor="w", padx=10, pady=12)
