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

On a machine with a Wi-Fi radio the window has a second tab, **Wi-Fi**, and
the first one stays as described above.

The table lists every network the radio hears, strongest first, with its
signal (`[###.] -60 dBm`), its security (**Open**, **WPA2**, or **Not
supported** for WPA3 and enterprise networks) and whether it is **Saved** or
**Connected**. The line above it says what the radio is doing now.

* **Join** connects to the selected network. A network you have joined before
  needs nothing more. A new WPA2 network needs its passphrase typed in the
  box first (8 to 63 characters, or 64 hexadecimal digits); an open network
  needs none. The passphrase is kept for your account only, is never shown
  again and is cleared from the box as soon as you press Join. A wrong
  passphrase ends as "probably a wrong passphrase": **Forget** the network
  and join it again.
* **Disconnect** leaves the network and stops reconnecting until you join one.
* **Forget** deletes a saved network and its passphrase.
* **Scan** looks again; the list also refreshes by itself while you are not
  connected (a scan pauses traffic, so a connected radio scans only when you
  ask).
* **Hidden network** joins a network that does not announce its name: type the
  name (and the passphrase above, or leave it empty for an open network).

A network joined here belongs to your account and is dropped from the radio
when you log out. Networks for every user (and the country, which decides the
channels the radio may use) need an administrator and are not in this page yet.

The Network Status icon in the tray shows signal bars while Wi-Fi is the
connection in use, and its tooltip names the network.
