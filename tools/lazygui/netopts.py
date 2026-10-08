"""The Advanced tab's Networking group (split out of `ui.py`, file-size budget).

The switch builds the network stack into the image and gives QEMU a virtio-net
card on its user-mode network (`tools/net/qemu_net.py`); the forwards let the
host reach guest ports. See docs/networking-host-access.md.
"""

from __future__ import annotations

from tkinter import ttk

from .catalog import qemu_net

HINT = (f"Guest {qemu_net.GUEST_ADDR} by DHCP; the host is {qemu_net.GATEWAY}. Forwards: "
        "[tcp:|udp:][HOSTADDR:]HOSTPORT:GUESTPORT, space-separated; empty = "
        f"{qemu_net.DEFAULT_FORWARDS[0]} (Net Tools' web server, open "
        f"http://localhost:{qemu_net.NETTOOLS_PORT}); `none` = no forwards.")


def build_group(parent: ttk.Frame, net_var, forwards_var, restrict_var, tls_var=None,
                lazyweb_var=None, smb_var=None) -> None:
    """Populate the Networking group: the switch, the HTTPS tools, the LazyWeb
    browser, the SMB client, the forwards, isolation."""
    ttk.Checkbutton(parent, text="Network card + stack (LAZYOS_NETD; run_demo --net; "
                                 "Network and Net Tools apps on the desktop)",
                    variable=net_var).pack(anchor="w", padx=6)
    if tls_var is not None:
        ttk.Checkbutton(parent, text="HTTPS tools curl/wget/fetch (LAZYOS_TLS; run_demo --tls; "
                                     "implies the stack)",
                        variable=tls_var).pack(anchor="w", padx=6)
    if lazyweb_var is not None:
        ttk.Checkbutton(parent, text="LazyWeb browser (LAZYOS_LAZYWEB; run_demo --lazyweb; "
                                     "implies the desktop, the stack and HTTPS)",
                        variable=lazyweb_var).pack(anchor="w", padx=6)
    if smb_var is not None:
        ttk.Checkbutton(parent, text="SMB client smb (LAZYOS_SMB; run_demo --smb; implies the stack; "
                                     "docs/smb-plan.md)",
                        variable=smb_var).pack(anchor="w", padx=6)
    row = ttk.Frame(parent)
    row.pack(fill="x", padx=6, pady=2)
    ttk.Label(row, text="Port forwards:").pack(side="left")
    ttk.Entry(row, textvariable=forwards_var).pack(side="left", fill="x", expand=True, padx=6)
    ttk.Checkbutton(parent, text="Isolate the guest (no outbound traffic; forwards still work)",
                    variable=restrict_var).pack(anchor="w", padx=6)
    ttk.Label(parent, text=HINT, wraplength=520, foreground="#666"
              ).pack(fill="x", padx=6, pady=(0, 6))
