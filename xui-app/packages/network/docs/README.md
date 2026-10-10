# Network

Network shows the machine's network connection and lets you change how it is
configured.

**Status** (refreshed every second): the interface and its hardware address,
whether the link is up, how the address was obtained, the address itself (with
the DHCP lease left), the default gateway and how much traffic has passed.

**Configuration**:

* *Automatic (DHCP)* asks the network for an address. Under QEMU's user
  network this is always `10.0.2.15/24`, gateway `10.0.2.2`, DNS `10.0.2.3`.
* *Manual* uses the address (with its prefix, `192.168.1.20/24`), gateway and
  DNS server you type. The gateway must be on the same network as the address.

**Apply** saves the settings (in `confd`, under `sys/net/<card>/`, for the card
the window shows); the network stack picks them up within a few seconds and
rebuilds that card with them (its open connections drop). **Next card** shows
the next network card when the machine has several. **Renew lease**
asks the DHCP server for a fresh address now. **Revert** puts the saved
settings back into the form.

Settings the stack would not accept are refused with the reason instead of
being saved. See `docs/networking-host-access.md` for reaching this machine
from the host.
