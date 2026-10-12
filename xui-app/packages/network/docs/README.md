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
the window shows). They are system settings, so an administrator approves the
change on the prompt that names the card and the address; the network stack
then picks them up within a few seconds and
rebuilds that card with them (its open connections drop). **Next card** shows
the next network card when the machine has several. **Renew lease**
asks the DHCP server for a fresh address now. **Revert** puts the saved
settings back into the form.

Settings the stack would not accept are refused with the reason instead of
being saved. See `docs/networking-host-access.md` for reaching this machine
from the host.

## Wi-Fi

On a machine with a wireless card a **Wi-Fi** button appears beside Next card
and opens the Wi-Fi page. It lists the networks heard (a lock marks one that
needs a password), strongest first, with "Connected" or "Saved" where it
applies, and refreshes itself.

* **Connect** (or a double-click) joins a saved network, saves and joins an
  open one, and asks for the password of a new protected one. A password is
  8 to 63 characters, or 64 hex digits; it is kept for you only and is never
  shown, logged or kept by this window.
* **Other network...** joins a network that is not listed or is hidden: give
  its name, whether it needs a password, and the password.
* **Available to all users** (on the password form) saves the network for
  everyone on this computer, even before login. That is a system setting, so
  an administrator approves it on the prompt.
* **Disconnect** leaves the network and stays off until you connect again.
  **Forget** deletes a saved network and its password after asking; one saved
  for all users needs an administrator to approve.
* **Back** returns to the wired page.
