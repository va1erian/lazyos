# Wi-Fi prerequisites (everything but the driver)

> **Status: draft, revision 1 (2026-10-09). Nothing here is built.**
> [wifi-plan.md](wifi-plan.md) answers whether LazyOS can get Wi-Fi from
> the MT7925 driver (yes) and stages the whole job (W0–W6). This plan takes
> everything in that job that is **not the chip driver** (`wifid`'s
> firmware download, MCU protocol, bus glue and descriptors) and turns it
> into work that can start now, needs no hardware, and is mostly useful on
> its own. When it is done, a driver is the only missing piece: one
> program that serves two interfaces this plan defines, on a test rig
> that already joins a WPA2 network in CI with a simulated radio.

Related: [wifi-plan.md](wifi-plan.md) (the hardware, licence and driver
plan; §3 and §5 are the starting point here),
[networking-plan.md](networking-plan.md) (N0–N6; multi-NIC was a §12
non-goal there and is WP1 here, built),
[architecture/networking.md](architecture/networking.md),
[security-model.md](security-model.md) §8 (`keyd`),
[accounts-plan.md](accounts-plan.md) (`elevd`, who may change what),
[driver-config-plan.md](driver-config-plan.md) §1 (secrets belong to `keyd`),
[architecture/usb.md](architecture/usb.md),
[tray-plan.md](tray-plan.md) (the Network Status applet),
[packages.md](packages.md).

## 1. Scope

**In:** the network stack, secrets, cryptography, 802.11 protocol
libraries, the station manager (`wlanmd`), the Messenger interfaces between
them, network profiles and policy, regulatory configuration, firmware file
plumbing, USB device access for a second process, a simulated radio for
CI, the user interface, the launchers and the licence paperwork.

**Out (the driver):** anything specific to a chip: MediaTek firmware
download, connac MCU commands and events, TXD/RXD descriptors, the USB or
PCIe bus glue of one adapter. That is wifi-plan W1–W3's driver half and W6.

**Rule kept from wifi-plan §5.1:** the process that holds DMA parses nothing
from the air, and the process that parses the air holds no capabilities.

## 2. Where things are (checked 2026-10-09)

Several facts in wifi-plan §3 (2026-10-02) have moved. This table replaces it
for the non-driver rows.

| Piece | State today | Gap for Wi-Fi |
|---|---|---|
| `netd` | One interface: `IFNAME = "eth0"` (`user/src/bin/netd/service.rs`), one smoltcp `Interface`, one NIC client (`netd/nic.rs`), config from `sys/net/eth0/*`, topics `system/net/eth0/*`, a `Routes` method | N interfaces, interfaces appearing and vanishing at runtime, a route and DNS choice across them, re-DHCP when a link comes back on a different network |
| NIC naming | Registry name is the constant `os.lazy.net.nic` (`user/src/messenger/net.rs`); `libs/devmatch` relies on it being unique ("first matching device wins, later ones `busy`") | One name per interface, and a kind (wired/wireless) in `NicInfo` |
| `nic.v1` | 802.3 frames over `framering`, link state in `Notify`, one client | Unchanged for data (the chip does 802.3 ↔ 802.11 encap). No change needed beyond naming |
| USB | `usbd` claims **every** xHCI controller, has hubs, hot-plug, multiple controllers, and **bulk pipes** (`libs/xhci/src/tests/bulk.rs`; USB storage in `usbd/msc.rs`). Every class driver lives inside `usbd`; no interface lets another process reach a device | A device interface, or Wi-Fi code inside `usbd`. wifi-plan's W1 shortcut ("`wifid` claims a second `qemu-xhci`") **no longer works**: `usbd` takes that controller too |
| PCI | MSI and MSI-X exist (`irq_enable` returns 0/1/2, [architecture/interrupts.md](architecture/interrupts.md)) | For the M.2 card only: extended config space, ASPM. Out of scope here |
| `devd` | Matches **PCI** functions to `init` driver rows (`libs/devmatch`) | USB vendor:product matching, so a dongle starts its driver |
| Entropy | Kernel ChaCha20 pool behind `getrandom` (`kernel/src/entropy.rs`); `keyd` has `Random` | None (the handshake's SNonce comes from here) |
| Crypto | `libs/crypto`: SHA-256, HMAC-SHA256, HKDF, Argon2id, `wrap`. `nettls/crypto` (separate workspace, `std`): AES-GCM, ChaCha20 for TLS | **SHA-1, HMAC-SHA1, PBKDF2-SHA1, AES-128, AES key wrap (RFC 3394), AES-CMAC**; AES-CCM only for the simulator |
| `keyd` | Key table in memory (32 keys, 8 per uid); `Wrap`/`Unwrap`/`Sign`/`Random`/`Generate`; account verifiers persisted by `accountsd`, not by `keyd` | Named secrets that survive a reboot; a PSK→PMK operation whose result goes to `wlanmd` only |
| Firmware files | None. `assets/manifest.txt` (`path \| licence \| install \| provenance`, landing at `/system/share/<path>`) and `tools/doom/fetch.py` (hash-pinned fetch) are the precedents | An `fhs` firmware path, a fetch-and-pin tool, a licence kept beside each file |
| UI | Network app (`xui-app/src/bin/network.rs`) and Network Status tray applet (`netstatus.rs`), both `eth0`-shaped | A wireless page, a scan list, a password prompt, a signal icon |
| Test radio | QEMU has no wireless device | A simulated radio (WP4), or nothing above the driver is testable in CI |
| uids | wifi-plan proposed `_wifi` = 906, `_wlan` = 907; **906 is `_devd`** and **907 is `_greeter`** (`user/src/messenger/logind.rs`); 901-910 are all taken | **Reserved (WP0): `_wifi` 911 (`wifid`), `_wlan` 912 (`wlanmd`), `_wifisim` 913 (`wifisim`).** Defined with their programs; the reservation is the comment on `DEVD_UID` in `libs/devmatch/src/lib.rs` |

## 3. The pieces

### 3.1 Multi-interface `netd` (wifi-plan's "N7")

Useful without Wi-Fi: two virtio-net cards, or e1000 plus virtio-net, on a
real PC with two ports.

- **Naming.** A NIC driver registers `os.lazy.net.nic/<ifname>`;
  `netdrv` takes its interface name as an argument from its `init` row
  (`devd` assigns `eth0`, `eth1`, ... in enumeration order, `wlan0`, ... for
  wireless). `NicInfo` gains `kind: wired | wireless`. Per the no-backward-
  compatibility rule, the bare `os.lazy.net.nic` name goes away.
- **Discovery.** `netd` watches the registry (or a `system/devices/*` topic
  from `devd`) and attaches to each NIC that appears, detaches from one
  that vanishes. A dongle pulled out is an interface removed, not an error.
- **Stack.** One smoltcp `Interface` per NIC, **each with its own socket set**
  (DHCP, ICMP and DNS sockets included), and a `netstack::Net` above them that
  owns the socket table and the choice of interface. The first draft shared
  one socket set; the spike (below, risk 1) showed smoltcp cannot do that.
  `Net` picks the interface by route when a socket first needs one (TCP
  `connect`, each UDP `send_to`, a ping, a lookup); a listener or a UDP socket
  bound to "any" address gets one smoltcp socket per interface (a replica,
  created again for an interface that appears later) so it serves all of
  them. The Linux `AF_INET` shim goes through the same `Net` calls.
- **Routes.** One default route per interface with a metric: wired 100,
  wireless 600 (the NetworkManager convention), so a cable wins when both
  are up and the Wi-Fi takes over when it is pulled. `Routes` already
  exists; it lists all of them.
- **DNS.** Servers from the interface holding the best default route;
  `/etc/resolv.conf` (`netd/resolvfile.rs`) rewritten when that changes.
- **Link changes.** Link down: keep the lease, mark the interface dead
  (no routes, no DNS, no new sockets on it). Link up: restart DHCP discovery,
  because a Wi-Fi link that comes back may be a different network. smoltcp's
  DHCP client has no INIT-REBOOT (RFC 2131 section 3.2: it only has
  `reset()`, which drops the lease and broadcasts a DISCOVER), so the address
  is lost for the moment a DISCOVER/OFFER/REQUEST/ACK takes, and a server that
  remembers the client re-offers the same address. Static interfaces keep
  their address. Wired links do the same, which is also correct for a cable
  moved between switches.
- **Config and topics.** Already keyed by interface (`sys/net/<if>/*`,
  `system/net/<if>/*`); the work is removing the constant, plus a
  retained `system/net/interfaces` list.
- **Clients.** `netctl`, the Network app, Net Tools and the tray applet
  list interfaces instead of assuming one.

**Built (WP1, 2026-10-10)**, as described in
[architecture/networking.md](architecture/networking.md) ("Several
interfaces"). Where it differs from the draft above:

- *One socket set per interface under `netstack::Net`*, not a shared set
  (risk 1, spiked). Wildcard listeners and datagram sockets are replicated per
  interface; connections, pings and lookups pick an interface by route.
- *Discovery lists the registry* (`registry::list`, a second apart) rather than
  watching `devd`'s topics: it is what exists, it is cheap, and it sees a card
  the moment its driver registers. A name missing for 3 s is a removed card.
  `devd` also publishes the name (`DeviceState.ifname`). Listing the registry
  as an unprivileged task had never worked (`registry::list` left the target
  slot at 0, which needs `CAP_IPC_CONTROL`); it targets the caller now.
- *`StartDriver` carries the interface name* (`ifname`, a third argument);
  `init` runs one `netdrv` row per card, the driver registers
  `os.lazy.net.nic/<ifname>`, and the bare name is gone.
- *DHCP restarts, it does not confirm*: smoltcp's client has no INIT-REBOOT.
- *Link down keeps the lease and the interface drops out of route and resolver
  choice* (no per-route "dead" flag is needed). `Reattach`/driver restarts are
  not link changes.
- *A configuration change rebuilds that interface*, not `netd`.
- *Hot-unplug cannot be shown with QEMU's `device_del` today*: the guest has no
  PCI hot-plug handler, so the eject request is never completed and the card
  stays. The detach path (a name gone from the registry for 3 s removes the
  interface, sockets on it are reset, wildcard sockets keep serving the rest)
  has host tests only (`cargo test -p netstack net::`). A kernel that
  completes the eject (poll the PIIX4 hot-plug registers, write `PCI_EJ`) would
  let `--nics 2` check it.

### 3.2 Cryptography

Added to `libs/crypto` (the vetted-crate rule of its header applies), all
`no_std`, from RustCrypto: `sha1`, `hmac` (already a dependency), `pbkdf2`,
`aes`, `aes-kw`, `cmac`. Every one is `MIT OR Apache-2.0`, taken under MIT,
which also satisfies the GPLv2-compatibility rule if any of it ever links into
the `nettls` stack. New entry points:

| Function | Used for | Reference vectors |
|---|---|---|
| `pbkdf2_sha1(pass, ssid, 4096) -> PMK[32]` | WPA2-PSK passphrase → PMK | IEEE 802.11-2020 Annex J.4 |
| `prf_sha1(key, label, data, bits)` | PTK derivation (AKM 2) | Annex J.3 |
| `hmac_sha1_128` (MIC, AKM 2), `aes_cmac_128` (MIC, AKM 6) | EAPOL-Key MIC | RFC 2202; RFC 4493 (Annex J has no MIC vector independent of a full handshake) |
| `aes_wrap` / `aes_unwrap(kek, data)` | GTK/IGTK delivery in message 3 and group key handshake | RFC 3394 §4.1, §4.6 |
| `kdf_sha256` (802.11 KDF) | PTK for AKM 6 (PSK-SHA256) | none published; cross-checked against an independent Python implementation, and again by `fake_ap.py` in WP4 |

WP0 status: these are in `libs/crypto/src/wifi.rs` as `Result`-returning,
length-checked functions (`pbkdf2_sha1` also enforces the standard's 8..=63
printable-ASCII passphrase and 1..=32-byte SSID; `aes_wrap`/`aes_unwrap`
take a 16- or 32-byte KEK; `mic_eq` is the constant-time compare). The
RustCrypto `aes` crate has no `force-soft` feature, unlike `sha1`/`sha2`;
the pinned toolchain's LLVM cannot legalise its SIMD backend on
`x86_64-unknown-none`, so `.cargo/config.toml` sets `--cfg aes_force_soft`
for that target.

Measure PBKDF2's 4096 iterations on the guest early (8192 SHA-1 compressions
per block, two blocks): it runs once per new network, but under TCG it may
take seconds. `keyd` caches the PMK, not the passphrase's derivation.

WPA3-SAE (P-256 group arithmetic) stays out, as in wifi-plan §7.

### 3.3 Secrets in `keyd`

What exists does not fit: keys are random, in memory, and per uid. Wi-Fi
needs a user-chosen secret, kept across reboots, usable before anyone logs
in for a network the machine joins at boot.

- **Named secrets.** `StoreSecret(scope, name, bytes)`,
  `DeleteSecret(scope, name)`, `ListSecrets(scope)` (names only).
  `scope` is `user` (the caller's uid) or `system` (stored only through
  `elevd`, the `sys/**` rule). `keyd` never returns a stored secret.
- **Persistence.** `keyd` writes its secrets to a file it owns
  (`fhs` path under `/conf`, 0600 `_keyd`), each wrapped under a machine
  key with the existing `wrap` construction. Without a TPM the machine key
  is a file of its own beside it, so at rest this protects against reading
  a copied file but not against root on the volume. Say so in
  `security-model.md` rather than imply more; sealing to a TPM is later.
- **The PSK operation.** `WifiPmk(scope, name, ssid) -> pmk` for
  passphrases (computed once, cached sealed beside the secret). Returned
  only to the `wlanmd` label; any other caller gets `EPERM`. v1 lets
  `wlanmd` hold the PMK and the derived KCK/KEK/TK for the session
  (wifi-plan §5.1); moving the MIC and unwrap into `keyd` is a later
  tightening that does not change the wire for anyone else.
- **Per-user vs per-machine networks.** A session user may add a network
  for themselves (scope `user`, joined while they are logged in); "available
  to all users and before login" is scope `system` and asks `elevd`
  (`net.wifi.system`), as a Settings change does. This makes the greeter's
  machine able to fetch time and updates over Wi-Fi without a login.

### 3.4 802.11 protocol libraries

Two `no_std` libraries, host-tested, with seeded `fuzz::run` entries shared
with cargo-fuzz targets, like `usbhid` and `netstack`. Written from the
standard and the permissive sources only (OpenBSD `net80211`, hostap for
cross-checks; never `mac80211`, per wifi-plan §4).

- **`libs/ieee80211`**: management frame parsing and building (beacon,
  probe request/response, authentication, association request/response,
  deauthentication, disassociation, action frames we ignore safely),
  information elements (SSID, supported rates, DS parameter, country, HT/VHT/
  HE capabilities read-only, **RSN IE** parse and build), a BSS record type,
  and a scan-result table with ageing. Parsing hostile beacons is this
  library's whole job, so it is the first fuzz target.
- **`libs/eapol`**: EAPOL-Key frames and the supplicant's 4-way and group
  key handshakes as a pure state machine (`input(frame) -> actions`), with
  the crypto of §3.2 injected so tests can pin nonces. Replay counter
  checks, the RSN IE match between beacon and message 3, GTK KDE parsing.
  Tests: Annex J vectors, a recorded `hostapd` exchange, and every message
  mutated (wrong MIC, replayed counter, mismatched RSN IE, truncated KDE)
  must be refused without a state change.

### 3.5 The interfaces and `wlanmd`

Two MIDL interfaces in a new `idl/wifi.midl`, generated by `midlc` like
every other (AGENTS.md rule).

- **`os.lazy.net.wifi.hw.v1`** (driver → station manager; one client,
  `wlanmd`): `Info` (MAC, bands, cipher and AKM support, scan offload yes/
  no), `Scan(channels, ssids)`, `ScanResults` as raw beacon/probe-response
  bodies with RSSI and channel (the driver never parses them), `Join(bssid,
  channel, ...)`, `TxMgmt(frame)` / an `RxMgmt` event for auth/assoc/EAPOL
  frames, `SetKey(kind, index, key, rsc)`, `Leave`, and events for beacon
  loss and deauthentication. The data path stays `nic.v1`. This is the
  contract a driver implements; the simulator of §3.6 is its first
  implementation.
- **`os.lazy.net.wifi.v1`** (system-facing, served by `wlanmd`): `Scan`,
  `Networks` (scan results, merged with known profiles), `AddNetwork`
  (the one call that takes a passphrase, handed straight to `keyd`),
  `Connect(id)`, `Disconnect`, `Forget`, `Status`, `Interfaces`; retained topics
  `system/net/<if>/wifi/state` and `system/net/<if>/wifi/scan`, added to
  [topics-catalog.md](topics-catalog.md).
- **`wlanmd`** (no capabilities, fresh system uid): one instance per
  wireless interface or one serving all (decide in WP4; one serving all is
  simpler with a single `wifi.v1` name). Runs the STA state machine (scan →
  authenticate → associate → 4-way → run; beacon loss and deauth → back to
  scan), BSS selection (known SSID, best RSSI, band preference), reconnect
  back-off, the auto-connect policy of §3.7, and calls `keyd` for the PMK.
  Roaming between BSSes of one ESS is a later refinement.

WP0 status: both interfaces are drafted in `idl/wifi.midl` (generated stubs,
manifest, Rhai API and `docs/idl/os.lazy.net.wifi*.md` are checked in; no
server exists). Shape decisions worth knowing: `hw.v1` has one owner
(`Attach` transfers the event channel, like `nic.v1`'s notify endpoint) and
events are `oneway` methods on it (`ScanDone`, `RxMgmt`, `BeaconLoss`,
`Deauthenticated`); scan results are paged raw frames (`ScanResults`);
EAPOL travels as `TxMgmt`/`RxMgmt` with a `FrameKind` of `Eapol`; the
association state is pushed by `wlanmd` with `SetState` (the NIC's link
follows `Authorized`); `SetCountry` is a `hw.v1` call so the driver can
refuse to transmit without a domain. Temporal keys cross `hw.v1` in `SetKey`
because the chip needs them; `wifi.v1` carries no secret except the
passphrase argument of `AddNetwork`. `wifi.v1` takes an interface name on
every call (one `wlanmd` serving all radios, the §5 item 6 assumption). Uids
are reserved in §2.

### 3.6 A simulated radio for CI

QEMU emulates no wireless device (wifi-plan §2.5), so without this the
whole stack above the driver is tested only on a dev host with a dongle.
The simulator turns every stage of this plan into a CI run.

```
 guest:  netd ◄─nic.v1─► wifisim ◄─wifi.hw.v1─► wlanmd ◄─► keyd
                            │ nic.v1 client of a second virtio-net (WP1)
 host:   QEMU -netdev socket/stream ───► tools/wifi/fake_ap.py
           (802.11 frames in an Ethernet wrapper; beacons, auth, assoc,
            the authenticator side of EAPOL, CCMP, DHCP and an echo server)
```

- **`wifisim`** (guest program, `LAZYOS_WIFI_SIM=1` images only): serves
  `wifi.hw.v1` and `nic.v1` as `wlan0` exactly as a chip driver would, and
  carries the air as 802.11 frames inside Ethernet frames of a private
  ethertype on a second virtio-net card. Like real hardware it does the
  802.3 ↔ 802.11 conversion and **CCMP in software** (`aes` + `ccm`), so
  a wrong TK or packet number is caught by the other side.
- **`tools/wifi/fake_ap.py`** (host): one or more APs (open, WPA2-PSK, a
  hidden SSID, two BSSes of one SSID on different channels), the
  authenticator implemented independently with Python's `hashlib`,
  `hmac` and `cryptography` (AES key wrap, AES-CCM), so a symmetric
  mistake in `libs/eapol` cannot pass. After association it answers DHCP and
  forwards data to the net harness's echo servers. It records every frame
  to a pcap with a radiotap-free 802.11 link type for the judge.
- **Faults on demand:** wrong passphrase, deauth mid-session, beacon loss,
  AP restart with a new GTK (group rekey), an AP that sends a malformed
  RSN IE. Each is a scenario with an expected `wlanmd` outcome.

The simulator is test tooling, not a model of any chip: it says nothing
about whether a driver is right, only whether everything above it is.

### 3.7 Profiles, policy and regulatory

- **Profiles** in `confd`, non-secret only:
  `sys/net/wifi/networks/<id>/{ssid,security,hidden,autoconnect,priority}`
  for system networks, `user/<uid>/net/wifi/networks/<id>/...` for a
  user's own; the secret lives in `keyd` under the same `<id>`.
- **Auto-connect.** At boot, `wlanmd` joins the best known system network;
  at login it adds the user's; at logout it drops a user network it joined
  for that session.
- **Regulatory.** `sys/net/wifi/country` (ISO 3166 alpha-2). Unset, the
  default is the "world" domain (passive scan only on 5 GHz channels,
  12–13 passive, no 6 GHz), and Settings offers the country derived from
  the time zone as the suggestion. `wlanmd` passes the country to the
  driver through `wifi.hw.v1`; a driver may refuse to transmit without one.
- **Who may do what.** Scanning and seeing results: any session user.
  Connecting with a user profile: any session user. Adding or removing a
  system profile, changing the country: `elevd`.

### 3.8 Firmware file plumbing

Chip-agnostic, so it belongs here even though only a driver reads it.

- `fhs::FIRMWARE` = `/system/share/firmware`, files at
  `<vendor>/<file>` with the licence text at `<vendor>/LICENCE.<vendor>`.
- `tools/firmware/fetch.py`: a pinned manifest (URL from linux-firmware at a
  fixed commit, SHA-256, size, licence) generalising `tools/doom/fetch.py`;
  fetched files go through `assets/manifest.txt`-style lines so
  `assets_embed.rs` installs them and `/system/.image-manifest` replaces
  them on update. Never `include_bytes!` (the MediaTek licence forbids
  becoming part of a GPL work; wifi-plan §2.2).
- A small `libs/fwload`: open a firmware file by vendor-relative name,
  refuse anything outside the firmware tree, cap the size, return the
  bytes. Tested on the host.
- `THIRD_PARTY.md` lists each blob, its origin commit and licence.

### 3.9 USB device access for a second process

The driver's landing pad on the bus; no chip code in it.

- **`os.lazy.usb.device.v1`** (MIDL), served by `usbd` per device a class
  driver claims: device and configuration descriptors, control transfers
  as calls (with a policy table of allowed requests per claim), bulk and
  interrupt endpoints as `framering`-style shared rings with a `Kick`/
  `Notify` pair like `nic.v1`, and detach as an event. `usbd` keeps the
  DMA; the class driver never sees a physical address.
- **`devd` matches USB too:** `libs/devmatch` gains `UsbIds { vendor,
  product }` rows; `usbd` publishes `system/devices/usb/<path>` for a device
  no built-in class took, `devd` asks `init` to start the matching driver
  with the device path as an argument.
- **Test it before Wi-Fi** with a trivial class driver over the device
  interface, for example moving USB storage out of `usbd` behind a flag
  and running `tools/storage/run.py` against it, or a loopback test driver
  against QEMU's `usb-serial`.

### 3.10 Front ends and packaging (the AGENTS.md checklist)

- `wifictl` CLI (`scan`, `connect <ssid>` with a no-echo password prompt,
  `status`, `forget`); `rhai` gets `sys::wifi` from the generated API.
- Network app: a Wi-Fi page (networks with signal and lock icons, connect
  with a password dialog, forget, "available to all users" asks `elevd`);
  Network Status applet: a signal-strength icon per state and a quick menu
  of networks; Settings: the country.
- Build switches `LAZYOS_WIFI=1` (stack, `wlanmd`, `wifictl`, needs
  `LAZYOS_NETD=1`) and `LAZYOS_WIFI_SIM=1` (adds `wifisim` and its card);
  `run_demo.py --wifi-sim` (boots against `fake_ap.py`) and later
  `--wifi --usb-host VID:PID`; GUI controls on both tabs through
  `tools/lazygui/catalog.py` with tests; core package permissions for the
  Network app derived from a `LAZYOS_LABEL_TRACE=1` run.
- Licence groundwork from wifi-plan §4 (option A recorded in `README.md`,
  `THIRD_PARTY.md` created: done in WP0), and a `tools/wifi/licenses.py` like
  `tools/nettls/licenses.py` over the new crates (still to do).

## 4. Stages

Letters WP. Each ends with evidence from a harness; nothing waits on the
dongle. The driver stages of wifi-plan (W1–W3 driver halves, W6) start
after WP6 and plug into what WP4 proved.

| Stage | Deliverable | Depends on | Evidence |
|---|---|---|---|
| **WP0 Groundwork** (done, see below) | Licence decision recorded, `THIRD_PARTY.md`; §3.2 crypto with KATs; fresh uids allocated; `idl/wifi.midl` drafted and reviewed | nothing | `cargo test -p lazyos-crypto` (Annex J, RFC 3394, RFC 4493); `midlc` output |
| **WP1 Multi-NIC `netd`** (**built**) | §3.1: per-interface registry names and `kind`, `devd` naming, N interfaces, metrics, DNS choice, DHCP restart on link up (no INIT-REBOOT in smoltcp), runtime attach/detach; `netctl`, Network app and applet list interfaces | nothing | `tools/net/run.py --nics 2`: DHCP on both cards (pcap per card); the wired default route wins; link of `eth0` set down over QMP (`set_link`) moves traffic and DNS to `eth1` and back, with the re-DHCP visible; `netd` is never restarted. `device_del` is not honoured by the guest (see §3.1), so detach is covered by host tests; existing `--netd` run unchanged |
| **WP2 Secrets** | §3.3: named secrets, persistence under `/conf`, `WifiPmk`, scope rules, `elevd` action `net.wifi.system` | WP0 | `keyd` host tests; a session stores a secret, reboots, and a `WifiPmk` from `wlanmd`'s label matches the Annex J PMK while the same call from the Terminal gets `EPERM`; the accounts attack harness gains "read another user's Wi-Fi secret" as `blocked` |
| **WP3 Protocol libraries** | §3.4: `libs/ieee80211`, `libs/eapol`, fuzz targets and seeds | WP0 | `cargo test -p ieee80211 -p eapol`; seeded fuzz soak; `python fuzz/gen_corpus.py --check` |
| **WP4 Station stack on the simulator** | §3.5 + §3.6: `wlanmd`, `wifisim`, `fake_ap.py`, `wifictl`; `netd` gets `wlan0` | WP1–WP3 | `tools/wifi/sim_run.py`: scan lists the fake SSIDs with channel and RSSI; open and WPA2-PSK joins; DHCP and echo over `wlan0`; the AP-side pcap shows EAPOL 1–4 and CCMP data the independent Python side decrypted; wrong passphrase, deauth, beacon loss and group rekey scenarios each end in the expected state; `wlanmd` killed mid-handshake is restarted by `init` and reconnects. `test_judge.py` fails when it should. CI |
| **WP5 Profiles and front ends** | §3.7 + §3.10: profiles, auto-connect at boot and login, country, Network app page, tray icon and menu, Settings, `rhai`, launchers and GUI, core package permissions | WP4 | a session script against the simulator: connect from the Network app with the password dialog, reboot, auto-connect before login for a system network, a user network dropped at logout; screenshots judged by `pngstats.py`; no `LABEL:DENY` in the traced run; `test_catalog.py` |
| **WP6 Driver landing pad** | §3.8 + §3.9: firmware path, fetch tool, `libs/fwload`; `os.lazy.usb.device.v1`; USB rows in `devd` | WP0 | a test class driver over the device interface passes its harness (storage or `usb-serial`); `tools/usb/run.py` and `tools/storage/run.py` still green; the fetched firmware lands at its `fhs` path with its licence, and an in-place update replaces it |

**WP0 status (done).** Licence option A is recorded in the README and
`THIRD_PARTY.md` (register of the five RustCrypto crates plus `dbl`, and
"not yet taken" rows for mt76, OpenBSD `net80211` and the MediaTek blobs).
`libs/crypto` gained `sha1 0.10.6`, `pbkdf2 0.12.2`, `aes 0.8.4`,
`aes-kw 0.2.1`, `cmac 0.7.2` (all `MIT OR Apache-2.0`, pinned like the rest)
and the `wifi` module of §3.2 with known-answer tests (Annex J.3/J.4, RFC
2202, 3394, 4493, 6070); it builds for `x86_64-unknown-none` with
`aes_force_soft` set in `.cargo/config.toml`. Uids 911-913 are reserved (§2).
`idl/wifi.midl` is drafted and `midlc --check` passes. Deviations: the old
plan's `906/907` were both taken (fixed above); Annex J has no EAPOL-Key MIC
or KDF-SHA256 vector, so those use RFC 2202/4493 and a cross-check; the
uid reservation is a comment, not a table, because
this code base defines a uid next to the program that uses it.

WP1, WP2, WP3 and WP6 are independent and can run in parallel; WP4 is the
milestone that matters ("Wi-Fi works in CI except for the radio").

## 5. Risks and open questions

1. **smoltcp and several interfaces. Spiked 2026-10-10; the answer is no.**
   `libs/netstack/tests/shared_socket_set.rs` runs two `Interface`s over one
   `SocketSet` on smoltcp 0.14. There is no binding of a socket to an
   interface: whichever interface polls first takes a ready socket's packet
   (`socket_egress` calls `socket.dispatch`, which advances the socket's
   state, before the interface consults its routes), routes it with its own
   table and never checks that the source address is its own. A UDP socket
   bound to A's address left through B with A's source address, and one DHCP
   socket in a shared set served only one of two interfaces. So the set is per
   interface and `netstack::Net` selects the interface (section 3.1); the
   cost is the replicas of wildcard sockets. The tests fail if a smoltcp
   upgrade ever changes this. smoltcp also has no INIT-REBOOT (section 3.1).
2. **The simulator's fidelity.** It proves the station stack against an
   independent AP, not against a real chip's firmware quirks (scan offload
   timing, MCU events arriving out of order). The `wifi.hw.v1` contract
   must be written from the mt76 behaviour (wifi-plan §2.1's offload table),
   not from what is convenient for the simulator.
3. **Sealing without a TPM.** A machine key beside the secrets file only
   stops a copied file being read. Stated plainly in the security model;
   `/conf` being root-only is the real protection.
4. **PBKDF2 cost under TCG.** If 4096 iterations take many seconds, cache
   the PMK at store time (the plan already does) and run the CI harness
   under KVM where available.
5. **USB device interface latency.** Bulk traffic through `usbd`'s rings
   adds a copy and a wake per burst; at polled USB rates that is noise, but
   measure it in WP6 with the storage driver before Wi-Fi depends on it.
6. **One `wlanmd` or one per interface?** Decided in WP4; one is assumed.
7. **Who may register a NIC name. Fixed (review of WP1, 2026-10-10).**
   `netd` attaches to whatever holds `os.lazy.net.nic/<ifname>` and trusts its
   `NicInfo`, and the registry let anyone register a name: a session user or
   app could have received `netd`'s frames (DNS, traffic) by claiming
   `kind = wired` with a lower slot. Now the registry reserves the namespace
   to the NIC driver identities (`netpolicy`: `_net` 902, `_wifi` 911,
   `_wifisim` 913, unlabelled, no session; root only as the console image's
   boot identity), `List` reports each owner's kernel-stamped identity, and
   `netd` checks it again, refuses a `kind` the uid may not claim and drops
   `Notify` from non-drivers (`NETD:NIC:REFUSED`). Wireless drivers register
   `wlan*` under their own uids with no further change. Attack row
   `nic_register`; see architecture/networking.md "Who may be a card".
8. **Captive portals, enterprise (802.1X/EAP), WPA3, hotspot mode** stay
   out, as in wifi-plan §7 and §10.

## 6. Decisions requested

1. Adopt this as the non-driver half of wifi-plan, with WP1 as the
   networking plan's next stage (multi-NIC is no longer a non-goal).
2. `keyd` gains persistent named secrets with `user`/`system` scopes, and
   system networks need `elevd`.
3. Build the simulated radio (`wifisim` + `fake_ap.py`) and make WP4, not a
   dongle, the gate for the station stack.
4. `usbd` exports a device interface (WP6) instead of wifi-plan's
   second-controller shortcut, which `usbd` claiming every controller has
   made unworkable.
