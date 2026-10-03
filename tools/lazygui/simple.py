"""The Simple tab: pick a build type and an interface, press Start."""

from __future__ import annotations

from tkinter import ttk

from .catalog import SIMPLE_BUILDS, SIMPLE_INTERFACES


#: The Simple tab's extra switches, in `catalog.simple_config`'s argument order;
#: each is the Tk variable `simple_<name>`.
SIMPLE_EXTRAS = ("lazyrad", "shell", "devices", "doom", "modplayer", "net", "linuxapps", "hidpi", "tls")


def simple_choice(build_label: str, iface_label: str) -> tuple[str, str]:
    """Map the tab's radio labels to ``(cargo profile, interface)``."""
    return dict(SIMPLE_BUILDS)[build_label], iface_label


def build_simple_tab(parent: ttk.Frame, build_var, iface_var, lazyrad_var, shell_var,
                     devices_var, doom_var, modplayer_var, net_var, on_start,
                     linuxapps_var=None, hidpi_var=None, tls_var=None) -> None:
    """Populate ``parent`` with the two choices and the Start button.

    ``build_var``/``iface_var`` are Tk string variables holding a
    ``SIMPLE_BUILDS`` / ``SIMPLE_INTERFACES`` label; ``lazyrad_var``,
    ``shell_var``, ``devices_var``, ``doom_var`` and ``modplayer_var`` are Tk
    booleans for the LazyRAD IDE, the LazyShell desktop, opening the Devices app
    at boot, the Doom package and the LazyRAD MOD player package; ``net_var``
    adds networking, ``linuxapps_var`` the Linux programs, ``hidpi_var``
    the 2560x1440 HiDPI screen and ``tls_var`` the HTTPS clients (with
    networking), on either interface; ``on_start`` runs the plan.
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
    ttk.Checkbutton(apps, text="Doom (builds the package; install it from "
                               "/system/share/samples/doom.lzp)",
                    variable=doom_var).pack(anchor="w", padx=8, pady=4)
    ttk.Checkbutton(apps, text="MOD player made with LazyRAD (builds the package; install "
                               "it from /system/share/samples like Doom)",
                    variable=modplayer_var).pack(anchor="w", padx=8, pady=4)

    net = ttk.LabelFrame(parent, text="Network")
    net.pack(fill="x", padx=8, pady=6)
    ttk.Checkbutton(net, text="Networking: internet through QEMU, ping/nc/ftp in the shell, "
                              "the Network and Net Tools apps on the Desktop",
                    variable=net_var).pack(anchor="w", padx=8, pady=(4, 0))
    ttk.Label(net, text="The host reaches the guest's web server (Net Tools) at "
                        "http://localhost:8080; more forwards in the Advanced tab.",
              wraplength=420, foreground="#555").pack(anchor="w", padx=28, pady=(0, 4))
    if tls_var is not None:
        ttk.Checkbutton(net, text="HTTPS: curl, wget and fetch with verified certificates "
                                  "(builds them; turns networking on)",
                        variable=tls_var).pack(anchor="w", padx=8, pady=4)

    if linuxapps_var is not None:
        extra = ttk.LabelFrame(parent, text="Extras (CLI or Desktop)")
        extra.pack(fill="x", padx=8, pady=6)
        ttk.Checkbutton(extra, text="Linux programs: dash, lua, sqlite3, jq, rg "
                                    "(builds them into /system/bin)",
                        variable=linuxapps_var).pack(anchor="w", padx=8, pady=4)

    if hidpi_var is not None:
        screen = ttk.LabelFrame(parent, text="Screen")
        screen.pack(fill="x", padx=8, pady=6)
        ttk.Checkbutton(screen, text="HiDPI: 2560x1440 at 2x (a sharp 720p desktop for a "
                                     "4K monitor)",
                        variable=hidpi_var).pack(anchor="w", padx=8, pady=4)

    ttk.Button(parent, text="Start LazyOS", command=on_start
               ).pack(anchor="w", padx=10, pady=12)
