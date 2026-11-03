# Plan: a basic Bluetooth stack for keyboards and mice, usable from boot

> **Status: plan (2026-11-03); B0 to B4 built**
> (`libs/bthci`, `libs/btl2cap`, `libs/btbond`, `libs/btsim`, `libs/btsmp`,
> P-256 in `libs/crypto`; the USB transport and the `fakebt` harness; `btd`
> with BR/EDR HID: `libs/bthost`, `libs/btsdp`, `libs/bthid`; LE HID over GATT:
> `libs/btgatt` and the LE half of `libs/bthost`, with LE peers in
> `libs/btsim`: [architecture/usb.md](architecture/usb.md), "Bluetooth
> controllers"). What B4 settled: the host resolves resolvable private
> addresses itself (no controller resolving list: the host scans while a
> bonded device has an IRK and connects the advertisement `rpa_matches`
> resolves); SMP cannot know a device is a keyboard before GATT discovery, so
> a Just Works LE pairing is taken as far as discovery and a device with a
> keyboard report is then dropped with no bond made
> (`BT:REFUSE reason=keyboard-unauthenticated`); a notification is heard only
> after the host's own CCCD write was answered, which also closes the window in
> which a plaintext PDU could ride in across the start of encryption; the
> harness-only `trace=1` build tells the simulated controller the passkey it
> shows (`HARNESS_SHOW_PASSKEY`, a vendor opcode a normal build never sends) so
> the simulated keyboard can "type" it. Two things changed in B3: the bond store is `/btd/bonds`, not `/conf/svc/btd`
> (`_bt` cannot cross root's private `/conf`: first boot showed `EACCES`; the
> account database moved out for the same reason), and a peer's first L2CAP
> request can beat the `Encryption_Change` (separate USB endpoints), so the
> host holds data from a link it has offered its key to for up to 500 ms.
> Builds on
> [usb-hid-plan.md](usb-hid-plan.md) (input sources on the raw bus, `inputd`
> owning keyboard and pointer policy), [architecture/usb.md](architecture/usb.md)
> (`usbd` and its class dispatch; "Adding a device class"),
> [usb-audio-plan.md](usb-audio-plan.md) (the model for `usbd` serving one
> USB function to another service over MIDL) and
> [wifi-prerequisites-plan.md](wifi-prerequisites-plan.md) 3.8 (`libs/fwload`,
> firmware files on the OS volume).

## Goal and scope

A Bluetooth keyboard or mouse that was paired once works **at the login
screen of every later boot**, with no user action beyond pressing a key or
moving the mouse, and it behaves exactly like a USB or PS/2 one: same raw bus,
same `inputd`, one keymap and one cursor. Pairing a new device is done from
Settings (or `btctl`) in a session.

**In:**

- The USB transport (HCI over USB, "H2": class `E0/01/01`), which is how every
  PC radio is attached (Intel CNVi, Realtek combo chips, USB dongles).
- Controller bring-up: generic controllers, then the Intel and Realtek
  firmware download the target boxes need.
- **BR/EDR (Classic) HID**: L2CAP channels on PSM `0x11`/`0x13`, HIDP, Secure
  Simple Pairing (the controller does the crypto).
- **LE HID over GATT (HOGP)**: scanning, connection, ATT/GATT discovery of the
  HID service, LE Secure Connections pairing (Just Works and Passkey Entry),
  encryption with the stored key on reconnect.
- A bond store read before any session exists, and a reconnect path that is
  armed as the first thing after controller reset.
- Pairing, listing and forgetting devices over `os.lazy.bt.v1`.

**Out (v1):** audio (A2DP, HFP, SCO, LE Audio), file transfer, networking,
UART-attached controllers (H4/H5), BR/EDR legacy PIN pairing (pre-2.1
devices), LE legacy pairing (added only if a real device in hand needs it,
see Risks), multiple controllers at once, a firmware/BIOS-time Bluetooth
keyboard (the bootloader and the kernel's pre-userspace console are before
any driver runs), gamepads (the seam is the same input sources; see
[usb-gamepad-plan.md](usb-gamepad-plan.md)).

## What exists, what is missing

| Piece | State today | Needed |
|---|---|---|
| Input sources | Syscall 25 `register_source`/`publish`/`close_source`, `CAP_INPUT_SOURCE`, kernel-stamped ids, release on close and on death, Interactive class for a source owner | none: `btd` is a second holder of the capability |
| HID decoding | `libs/usbhid::boot` (boot keyboard and mouse), `libs/usbhid::report` (report descriptors, pointer) | keyboard report-protocol decoding if `report` does not already cover page `0x07`; both are reused as-is for Bluetooth reports, which are USB HID reports in a different envelope |
| USB transport | `usbd` binds HID, hub, mass storage and audio interfaces; `class.rs` documents how to add a class; `usb_audio.midl` is a served-function precedent | a `bt.rs` function: control endpoint for HCI commands, interrupt-IN for events, bulk IN/OUT for ACL; `os.lazy.usb.bt.v1` served to `_bt` only |
| Crypto | `libs/crypto`: SHA-256, HMAC, `aes_cmac_128` (RustCrypto `aes` + `cmac`), RNG | P-256 ECDH for LE Secure Connections (RustCrypto `p256`, already in the `xui-app` lockfile) and the SMP functions `f4`, `f5`, `f6`, `g2`, `ah` |
| Firmware | `libs/fwload` + `tools/firmware/fetch.py` (pinned downloads into the OS volume) | Intel `ibt-*.sfi`/`.ddc` and Realtek `rtl_bt/*` entries in the manifest |
| Bluetooth itself | the kernel classes radio `0D/11` as `other`; the USB hotplug plan expects the Intel radio (`8087:0026`) to show `unsupported` | everything below |

## Key decisions

1. **Transport in `usbd`, host stack in a new service `btd`.** `usbd` owns
   every xHCI controller, so only it can move the radio's packets; it does so
   the way it feeds `sndd` for USB audio: a class function that forwards
   bytes and holds no Bluetooth knowledge. The host stack (HCI, L2CAP, ATT,
   SMP, HID) runs in `btd`, because it parses packets **a stranger in radio
   range controls**, and `usbd` holds DMA rights (kernel-equivalent until an
   IOMMU exists). A bug in the stack then costs `btd`'s authority, not the
   machine. Alternative rejected: the stack inside `usbd` saves one hop and one
   process start, but puts a remote attack surface in the most privileged
   driver.
2. **`btd` publishes input directly to the raw bus.** It holds
   `CAP_INPUT_SOURCE` and registers **one source per connected HID device**
   (keyboard or pointer class; a combo device is two sources, as in `usbd`).
   It needs nothing from `inputd`, so input works the moment a source is
   registered, and a disconnect or a `btd` crash releases every held key and
   button through the kernel's existing close path.
3. **`btd` is a boot service, not a session service.** `init` starts it with
   `usbd`, before `logind` shows the greeter, as `_bt` (uid **916**, the next
   free system uid per `libs/devmatch`) with `CAP_INPUT_SOURCE` only: no device
   claim, no DMA, no read of the input bus. It waits for `usbd` to serve
   `os.lazy.usb.bt` exactly as `sndd-usb` waits for its card, so it has no
   start dependency to serialise behind.
4. **The bond store is machine state, read before the controller is up.**
   Bonds (address, address type, link key or LTK/IRK/EDIV/Rand, key size,
   authenticated flag, device class, name, the HID report descriptor and GATT
   handles so a reconnect needs no rediscovery) live in one versioned,
   checksummed file at `fhs::state::BTD_BONDS` (`/btd/bonds`: see the status note), owned by
   `_bt`, mode 0600, written atomically (`.new` + rename, as `keyd` does).
   Keys never reach a trace. Bonds are per machine, not per user: the greeter
   has no user yet, and a keyboard is a machine's device.
5. **Fast path at boot.** The first commands after `HCI_Reset` and the event
   mask are the ones that let a bonded device in: BR/EDR page scan on (a
   bonded HID device pages the host when a key is pressed), and an LE
   auto-connect on the filter accept list of bonded LE devices
   (`LE_Create_Connection` with the accept-list policy, falling back to a
   passive scan). Everything else (local name, inquiry scan, the class of
   device beyond "computer", reading the controller's features in full) comes
   after. `usbd` binds a Bluetooth function in its first port scan without
   waiting for slower devices on other ports. Stored report descriptors and
   GATT handles mean a reconnect goes straight to "encrypt, enable
   notifications, publish". The boot is measured (decision 9), not assumed.
6. **Only bonded, encrypted links may type.** A HID channel is opened, and a
   source registered, only for a bonded device on a link encrypted with its
   stored key (BR/EDR: authentication and encryption complete before the
   interrupt channel is accepted; LE: encryption with the LTK before any
   report notification is acted on). An unbonded device connecting while
   pairing is off is refused at the L2CAP/ATT layer. This is the class of bug
   behind CVE-2023-45866 (unauthenticated HID connections accepted by
   several stacks), so it is a test, not a comment.
7. **Report protocol by default, boot protocol as the fallback.** Bluetooth
   HID reports are USB HID reports, so `libs/usbhid::report` decodes them
   from the device's report descriptor (BR/EDR: the SDP record's
   `HIDDescriptorList`; LE: the Report Map characteristic), giving wheels and
   extra buttons. A device whose descriptor `usbhid` cannot use is switched to
   boot protocol (BR/EDR `SET_PROTOCOL`, LE Protocol Mode characteristic) and
   decoded by `usbhid::boot`. `btd` holds no input policy.
8. **Crypto on the host, not the controller.** LE Secure Connections needs
   P-256 ECDH and AES-CMAC. Controllers since 4.2 can do ECDH for the host,
   but cheap 4.0 dongles cannot, and an HCI round trip per AES block is slow;
   so `libs/btsmp` uses `libs/crypto` (CMAC exists, P-256 is added there from
   RustCrypto `p256`) and the controller only encrypts the link. BR/EDR SSP
   crypto is the controller's job by specification.
9. **Evidence through `user::trace` on a new `Bt` channel.** `BT:CTRL` (bring
   up, with the controller's version and vendor), `BT:READY ms=<since boot>`,
   `BT:BOND:LOAD n=`, `BT:CONN`, `BT:ENCRYPT`, `BT:HID:KBD|MOUSE`,
   `BT:RECONNECT ms=<link up to first report>`, `BT:PAIR:*`, `BT:REFUSE
   reason=`. No address appears in full in a default-build trace (the last
   two octets only), no key ever does, and report contents are echoed only
   with a harness-only `trace=1`, as `usbd` does (typed passwords).
10. **Every interface is MIDL.** `idl/usb_bt.midl` (`usbd` to `btd`: open,
    send command, send ACL, receive events and ACL) and `idl/bt.midl`
    (`os.lazy.bt.v1`: list, discover, pair, confirm or enter a passkey,
    forget, a `PairingEvent` stream). `os.lazy.usb.bt.v1` is callable by
    `_bt` only; `os.lazy.bt.v1`'s pair and forget change which devices may
    type into every session, so both go in `devtool::DENY`, and only a local
    session (the session user) or `elevd`-elevated callers may pair or
    forget.
11. **No timer polling.** The transport carries packets through a shared
    `Buffer` ring per direction with a doorbell (the `framering` pattern),
    so `btd` sleeps until `usbd` has an event or ACL data; HCI and L2CAP
    timeouts are deadlines in `btd`'s single event loop, not a periodic tick.

## Architecture

```
 BT keyboard / mouse  ~~radio~~>  controller (Intel AX2xx, Realtek, dongle)
                                       |  USB: EP0 class req (HCI cmd), intr-IN (events), bulk (ACL)
 usbd   (_usb, uid 904; unchanged capabilities)
        |  usbd/bt.rs: bind E0/01/01, three pipes, no BT parsing
        |  os.lazy.usb.bt.v1 (idl/usb_bt.midl): Buffer rings + doorbell, _bt only
        v
 btd    (_bt, uid 916, CAP_INPUT_SOURCE only; started by init at boot)
        |  libs/bthci   : packets, command credits, ACL credits, init sequence
        |  libs/btfw    : Intel / Realtek firmware download (state machines)
        |  libs/btl2cap : BR/EDR channels (signalling, config), LE fixed channels
        |  libs/btsdp   : the HID SDP record (descriptor list, reconnect flags)
        |  libs/btgatt  : ATT client, HOGP discovery (HID 0x1812, Report Map)
        |  libs/btsmp   : LE SC pairing, key distribution (libs/crypto, p256)
        |  libs/bthid   : HIDP and HOGP reports -> libs/usbhid decoders
        |  libs/btbond  : bond store encode/decode (hostile bytes, versioned)
        |  syscall 25: one input source per HID device
        v
 kernel raw bus -> inputd -> greeter / xuid        [unchanged]

 Settings -> Bluetooth, btctl  --os.lazy.bt.v1 (idl/bt.midl)-->  btd
```

Every `libs/bt*` crate is `no_std` + `alloc`, host-tested, with no I/O and no
clock (time is passed in), so the whole stack runs on the host against the
simulator below. Hostile-input parsers (`bthci` events, `btl2cap`, `btsdp`,
`btgatt`, `btsmp`, `btbond`) get seeded fuzz targets under `fuzz/`.

### The simulator, `libs/btsim`

A deterministic controller model plus peer devices: a BR/EDR HID keyboard, a
BR/EDR mouse, an LE HOGP keyboard (passkey entry) and an LE mouse (Just
Works), each able to pair, bond, disconnect and reconnect on its own. It
answers HCI commands, emits events, carries ACL to the peers' own L2CAP,
SDP, ATT and SMP, and runs the real crypto (so a wrong `f5` fails the test).
It implements one trait:

```rust
pub trait Controller {
    fn command(&mut self, now_ms: u64, packet: &[u8]);
    fn acl_out(&mut self, now_ms: u64, packet: &[u8]);
    /// The next packet the controller sends the host, if any is due by now.
    fn poll_in(&mut self, now_ms: u64) -> Option<HciIn>;
}
```

Host tests drive `btd`'s stack (the libraries, and `btd`'s logic split from
its I/O) against it. The QEMU harness uses the same model: `tools/bt/fakebt`
is a small host program that serves a fake USB Bluetooth dongle to QEMU over
**usbredir** (`-chardev socket,... -device usb-redir,chardev=...`) and
answers through `btsim`. QEMU has had no Bluetooth emulation of its own since
the `-bt` option was removed, so this is the only CI path; scripted peers can
"press a key" at a chosen time, including during boot.

## Phases

Each phase is shippable and has its tests. Kernel changes are not expected
(input sources already exist); if one turns out to be needed it brings its
correctness and stress tests per AGENTS.md.

| Phase | Deliverable | Tests |
|---|---|---|
| **B0** HCI and L2CAP libraries | `libs/bthci` (command, event, ACL packet codecs; command-credit and ACL-buffer flow control; the controller init state machine as a pure step function), `libs/btl2cap` (basic-mode connection-oriented channels with signalling and configuration, LE fixed channels 4/5/6, fragmentation and reassembly with bounded buffers), `libs/btbond` (store format) | `cargo test -p bthci -p btl2cap -p btbond`: golden packets from the Core specification's examples, credit accounting, reassembly of every split, malformed and oversized lengths refused. Fuzz targets with checked-in seeds (`fuzz/gen_corpus.py --check`) |
| **B1** Simulator | `libs/btsim`: the controller model and the four peers above, BR/EDR side first | Its own tests: a scripted host sequence gets the expected events; peers bond and reconnect |
| **B2** USB transport | `usbd/bt.rs` (bind `E0/01/01`, interface 0 only; HCI commands as class requests on EP0, events on interrupt-IN, ACL on bulk; ISO interface 1 ignored), `idl/usb_bt.midl` + generated code, served as `os.lazy.usb.bt` to `_bt` only, ring + doorbell. Build switch `LAZYOS_BT=1` (`build.rs`/`build_support`, `cfg(lazyos_bt)`), `run_demo.py --bt`, GUI Advanced/Simple controls via `tools/lazygui/catalog.py` with tests, `docs/build-switches.md`. `tools/bt/fakebt` (usbredir server over a `btsim` controller) and `tools/bt/run.py` + `judge.py` + `test_judge.py` | Harness: QEMU with `qemu-xhci` and the fake dongle; a test client (or the B3 `btd`) round-trips `HCI_Reset` and `Read_BD_ADDR`; `USBD:BT` bound; hot-unplug of the dongle withdraws the interface. `midlc --check`, `devtool/check_interfaces.py` |
| **B3** `btd` with BR/EDR HID | `user/src/bin/btd.rs` + `btd/` (event loop, controller bring-up, connection table, the boot fast path of decision 5), `libs/btsdp` (the HID record), `libs/bthid` (HIDP over `0x11`/`0x13`, reports to `usbhid`), SSP pairing (DisplayYesNo host; Just Works for mice, Passkey Entry for keyboards), the bond store, input sources. `init` row (`_bt`, `CAP_INPUT_SOURCE`, `Restart::Always`), `libs/fhs` paths, `Bt` trace channel (`python tools/trace/catalog.py`). Pairing in this phase is a boot argument for the harness (`pair=<addr>`), the interface comes in B5 | Host: full pair, bond, disconnect, reconnect against `btsim`; refusal of an unbonded or unencrypted HID connection (decision 6); stuck-key release on link loss. Harness: pair once, reboot the same image, the peer types during the greeter, `verify_trace`-style check that the text arrived; kill `btd` mid-keypress, nothing stuck |
| **B4** LE HOGP (built) | `libs/btgatt` (ATT client: MTU exchange, discovery by type, read, read blob, write, CCCD, notifications), `libs/btsmp` (LE SC Just Works and Passkey Entry, key distribution of LTK and IRK, RPA resolution with the IRK), P-256 in `libs/crypto`, LE connection management (accept-list auto-connect, connection parameters suitable for HID, encryption with the LTK on reconnect), `btsim` LE peers; harness modes `--le-pair`, `--le-reboot`, `--le-rpa`, `--le-refuse` | Host: SMP against the Core specification's sample data for `f4`/`f5`/`f6`/`g2`/`ah`; pair and reconnect the LE keyboard and mouse against `btsim`, including a peer using a resolvable private address; a notification before encryption is ignored. Harness: as B3, LE peers |
| **B5** Pairing interface and UI | `idl/bt.midl` (`os.lazy.bt.v1`), caller checks (session user or elevated), `devtool::DENY` entries; `btctl` (a Rhai script); a Settings -> Bluetooth pane (discover, pair with the passkey shown, connected state, forget) | `midlc --check`, Rhai schema regenerated; host tests of the caller policy; a `qemu_session.py` script pairs the `btsim` LE keyboard from Settings by clicking (`click_at` names), the screenshot is read |
| **B6** Boot latency | Measure and fix: `BT:READY` and `BT:RECONNECT` timings from the B3/B4 harness at boot; `usbd` binding the radio in the first scan; anything in `btd`'s bring-up that is not on the fast path moved after it. A `--boot-typing` harness mode where the peer presses keys from the moment the controller is up | The harness reports both numbers and fails above a budget (start: radio ready within 1.5 s of `usbd`'s start under KVM/WHPX, first report published within 300 ms of the link; tightened once measured). A row in `docs/perf/` |
| **B7** Real controllers | `libs/btfw`: Intel (read version in bootloader mode, download `ibt-*.sfi` in secure-send fragments, the reset and the USB re-enumeration that follows, `.ddc` config) and Realtek (`rtl_bt/*_fw.bin` + `_config.bin` patch download by the vendor command); firmware entries in `tools/firmware/fetch.py`'s manifest; `btd` loading them through `libs/fwload`; `usbd` tolerating the radio's detach and reattach during download | Host: both download state machines against recorded command/event fixtures (`--pcap`-style captures from the real boxes via `dbgd`). On hardware (Alder Lake / Kaby Lake boxes with Intel radios, the AMD NUC's RTL8822CE, one USB dongle): a real keyboard and mouse pair, and work at the greeter after a reboot, evidenced through `dbgctl log` |
| **B8** Docs and CI | `docs/architecture/bluetooth.md`, links from `docs/index.md` and `architecture/usb.md`, `docs/agents/hardware.md` section, `.github/workflows/bt.yml` running the host tests and the `tools/bt/run.py` harness | the workflow green |

B0, B2 and the crypto part of B4 (`libs/btsmp` and P-256 in `libs/crypto`)
are independent and can be built in parallel; B1 follows B0's codecs; B3
needs B0, B1 and B2; B4 needs B3's skeleton; B5, B6 and B7 are independent of
each other after B4.

## Security

- **Remote attack surface.** Everything from the radio is parsed in `btd`,
  which holds one capability. Every length is checked before use, every loop
  and every queue is bounded (connections, channels, reassembly, pending
  commands), and a peer that misbehaves is disconnected and counted.
- **No silent pairing.** Pairing happens only while a caller of
  `os.lazy.bt.v1` has turned it on, for a bounded time, and a keyboard's
  passkey is shown to the user. Discoverable (inquiry scan, LE advertising)
  is off outside that window; page scan and LE auto-connect admit bonded
  devices only (decision 6).
- **Spoofing.** A bonded address alone grants nothing: the link must be
  encrypted with the bond's key. BR/EDR keys from Just Works are marked
  unauthenticated; keyboards must use an authenticated key (Passkey Entry or
  Numeric Comparison), so a mouse-class Just Works bond cannot later present
  itself as a keyboard (the source class follows the bond's record).
- **Input sources.** The kernel's class checks and rate limits apply to
  `btd` as to `usbd`; `btd` cannot read the bus.
- **Secrets.** Keys stay in `btd` and its 0600 file; no interface returns a
  key; traces never carry one.

## Risks and open questions

1. **Vendor firmware is the real-hardware gate.** Intel AX2xx radios do
   nothing useful until their firmware is loaded, and the download makes the
   radio re-enumerate on USB. If `usbd`'s detach/reattach does not survive
   it, B7 grows a `usbd` change. Nothing in B0-B6 depends on it.
   `libs/btfw` (the loader half of B7) narrows this from Linux's documented
   behaviour: after a successful Intel download the new image announces itself
   with a "boot up" vendor event on the same USB device; the device leaves the
   bus only when the loader sends its recovery reset after a failure (and,
   from the protocol, may on a part that resets on boot). `btfw` reports that
   as `Done::Reenumerate` / `Failure.reenumerate`, so `btd` restarts from HCI
   Reset when the interface returns. Realtek parts keep their device across
   the download. Unverified on hardware until a radio is on the bench.
   **Built (B7 wiring):** `libs/bthost/src/firmware.rs` runs the loader between
   the transport attaching and `bthci::Init` (the vendor from `Describe()`'s USB
   ids; files through `libs/fwload`; the loader's deadlines are part of
   `Host::next_deadline`), counts re-enumerations in `btd`'s memory (cap 3, then
   `BT:FW:FAIL reason=reenumeration-cap` and a bring-up that touches nothing
   until the radio is replugged), and `usbd` withdraws and re-registers
   `os.lazy.usb.bt` for a controller that left and came back even within one
   loop pass (`btd` tells registrations apart by their kernel object). The
   `bt` firmware set ships in every `LAZYOS_BT=1` image. Exercised against
   `btsim`'s Intel and Realtek radios (`tools/bt/run.py --fw ...`); the real
   bench procedure is in [agents/hardware.md](agents/hardware.md), "Bluetooth
   controllers". Vendor identification uses the USB ids only; a radio that
   a vendor id does not name but whose `Read_Local_Version` manufacturer does
   is not loaded yet.
2. **Reconnect behaviour differs per device.** BR/EDR keyboards usually page
   the host; some wait to be paged (`HIDReconnectInitiate` false,
   `HIDNormallyConnectable` true), so `btd` also pages bonded BR/EDR devices
   at boot with a bounded retry. LE devices that advertise with a fresh
   random address need the IRK in the accept list (controller address
   resolution) or host resolution of every advertisement.
3. **LE legacy pairing.** Some older LE mice support only legacy pairing.
   Left out until a device in hand needs it; it is `c1`/`s1` on the same AES.
4. **Coexistence with Wi-Fi.** On combo chips (RTL8822CE) Bluetooth and Wi-Fi
   share the antenna; `libs/rtw88::coex` already handles the Wi-Fi side. A
   Bluetooth link that drops under Wi-Fi load is a coex bug in that crate,
   not in `btd`.
5. **The first keyboard.** A machine whose only keyboard is Bluetooth cannot
   type the greeter's password before the first pairing. v1 needs another
   input device once; a greeter-side pairing dialog driven by the pointer is
   a follow-up.
6. **usbredir availability.** The CI harness needs a QEMU built with
   usbredir. If the Windows QEMU lacks it, the harness runs under WSL/Linux
   only and says so.

## Suggested order of work

Wave 1, in parallel: B0 + B1 (libraries and simulator), B2 (transport,
switch and harness), and the crypto half of B4 (`libs/btsmp`, P-256). Wave 2:
B3 then the rest of B4, which give a working keyboard at the greeter in QEMU.
Wave 3, in parallel: B5, B6, B7. B8 lands with the last of them.
