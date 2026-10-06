"""The Advanced tab's Drivers group (split out of `ui.py`, file-size budget):
the sound card and NIC QEMU gets, and the device manager switch (issue #497).
The logic is `drivers`."""

from __future__ import annotations

from tkinter import ttk

from .drivers import NICS, SOUND_CARDS

HINT = ("intel-hda: QEMU's HDA controller with a line-out codec; e1000: QEMU's Intel "
        "8254x. The same drivers (sndd, netdrv) run both. Without devd, init starts the "
        "drivers at boot and each finds its own device.")


def build_group(parent: ttk.Frame, sound_card_var, nic_var, devd_var) -> None:
    """Populate the Drivers group."""
    row = ttk.Frame(parent)
    row.pack(fill="x", padx=6, pady=2)
    ttk.Label(row, text="Sound card:").pack(side="left")
    ttk.Combobox(row, textvariable=sound_card_var, state="readonly",
                 values=SOUND_CARDS, width=8).pack(side="left", padx=(4, 12))
    ttk.Label(row, text="NIC:").pack(side="left")
    ttk.Combobox(row, textvariable=nic_var, state="readonly",
                 values=NICS, width=8).pack(side="left", padx=4)
    ttk.Checkbutton(parent, text="Device manager devd starts the drivers (LAZYOS_DEVD; "
                                 "run_demo --no-devd turns it off)",
                    variable=devd_var).pack(anchor="w", padx=6)
    ttk.Label(parent, text=HINT, wraplength=520, foreground="#666"
              ).pack(fill="x", padx=6, pady=(0, 6))
