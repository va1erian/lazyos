# Bluetooth: `btd`, a keyboard and mouse host that works at the greeter

**What it is.** A Bluetooth keyboard or mouse that was paired once types at
the login screen of every later boot and behaves exactly like a USB or PS/2
one: same raw input bus, same `inputd`, one keymap and one cursor
([bluetooth-plan.md](../bluetooth-plan.md)). It is two ordinary ring-3
services. `usbd` binds the radio (a USB Bluetooth controller, class
`E0/01/01`, "HCI over USB") and moves its packets without understanding them
([usb.md](usb.md), "Bluetooth controllers"). `btd` runs as `_bt` (uid 916,
`CAP_INPUT_SOURCE` only: no device claim, no DMA, no read of the input bus),
because it parses what a stranger in radio range sends. It brings the
controller up (loading vendor firmware first when the part needs it), runs the
BR/EDR HID host and the LE HID-over-GATT host, keeps the bonds, and registers
one input source per connected keyboard or pointer (syscall 25, the same
record mapping as `usbd`). It holds no input policy. Pairing is done in a
session from Settings or `btctl` over `os.lazy.bt.v1`. Out of scope: audio,
file transfer, UART-attached controllers, BR/EDR legacy PIN pairing, LE legacy
pairing, more than one controller, a keyboard before the kernel's drivers run
(firmware/bootloader time). Build switch `LAZYOS_BT=1` (`run_demo.py --bt`).

**Key files**

| Path | Role |
|---|---|
| `libs/bthci/` | HCI codecs (commands, events, ACL headers), command-credit and ACL-buffer flow control, the controller bring-up script as a pure step function |
| `libs/btl2cap/` | L2CAP: signalling, basic-mode connection-oriented channels with configuration, LE fixed channels, bounded fragmentation and reassembly, a per-PSM allow list (`set_allowed`) |
| `libs/btsdp/` | The SDP client: `ServiceSearchAttribute` for the HID record (report descriptor, subclass, reconnect flags), bounded, with continuation |
| `libs/bthid/` | HIDP framing on PSMs `0x11`/`0x13`, HOGP reports, decoder choice (report protocol through `libs/usbhid`, boot protocol as the fallback) |
| `libs/btgatt/` | The ATT client (MTU, discovery, long reads, CCCD, notifications) and HID-over-GATT discovery (service `0x1812`, Report Map, Report Reference, Protocol Mode) |
| `libs/btsmp/` | LE Secure Connections: SMP PDUs, `f4`/`f5`/`f6`/`g2`/`ah`, Just Works and Passkey Entry, key distribution, `rpa_matches`; P-256 and AES-CMAC come from `libs/crypto` (`lazyos-crypto`) |
| `libs/btbond/` | The bond store format: versioned, checksummed, decoded from untrusted bytes, no file I/O |
| `libs/btfw/` | Firmware loaders as step functions: `intel/` (legacy and TLV `Read_Version`, SFI fragments, DDC, recovery reset), `realtek/` (epatch v1/v2, config, fragments), `vendor.rs` (vendor from USB ids) |
| `libs/btusb/` | USB transport framing shared by `usbd` and host tests: cutting HCI packets out of endpoint chunks |
| `libs/bthost/` | The decisions of `btd`, sans-IO (no I/O, no clock, no `unsafe`): bring-up, links, who is admitted, SSP and SMP pairing, discovery, the `os.lazy.bt.v1` logic (`ctl*.rs`, `discover.rs`), the firmware phase (`firmware.rs`), the bond load/save protocol (`store.rs`) |
| `libs/btsim/` | A deterministic controller model with BR/EDR and LE keyboard and mouse peers and simulated Intel and Realtek radios (`radio.rs`); runs the real crypto |
| `libs/btpolicy/` | `_bt` (uid 916), who may call the transport and the API (`may_use_transport`, `may_use_api`), and `names.rs`, the owners of the registry names `os.lazy.usb.bt` and `os.lazy.bt` |
| `user/src/bin/btd.rs`, `user/src/bin/btd/` | The I/O shell: `link.rs` (rings to `usbd`), `sources.rs` (input sources), `disk.rs` (bond file), `firmware.rs` (files through `libs/fwload`), `api.rs` (the `os.lazy.bt` server) |
| `user/src/bin/usbd/bt*.rs` | `bt.rs` (bind, pipes, endpoint chunks), `bt_link.rs`, `bt_out.rs` (commands and ACL to the device), `bt_service.rs` (the `os.lazy.usb.bt` server) |
| `idl/usb_bt.midl`, `idl/bt.midl` | `os.lazy.usb.bt.v1` (`usbd` to `btd`: rings and doorbells, `_bt` only) and `os.lazy.bt.v1` (list, discover, pair, confirm, cancel, forget, events) |
| `xui-app/crates/settings/` (`src/bluetooth*.rs`), `xui-app/src/platform/bluetooth.rs`, `tools/bt/btctl.rhai` | The Settings page (every call off the UI thread) and `btctl`, a Rhai script `init` finds at `/system/bin/btctl` |
| `tools/bt/` | The harness: `fakebt/` (a fake dongle served over usbredir), `run.py`, the judges, `spans.py` (boot budgets), [README](../../tools/bt/README.md) |
| `fuzz/fuzz_targets/bt*.rs` | Fuzz targets: `bthci`, `btl2cap`, `btbond`, `btsmp`, `btfw`, `btsdp`, `bthid`, `btgatt` (run by [net.yml](../../.github/workflows/net.yml)) |

```text
 radio  ~~>  controller (Intel, Realtek, dongle)
               | USB: EP0 class request (HCI command), interrupt-IN (events), bulk (ACL)
 usbd (_usb)   | bt.rs binds interface 0; no Bluetooth knowledge but packet delimiting
               | os.lazy.usb.bt.v1: shared Buffer, Commands ring + Events ring, doorbells
 btd (_bt, CAP_INPUT_SOURCE)
               | libs/bthost over bthci, btl2cap, btsdp, bthid, btgatt, btsmp, btfw, btbond
               | syscall 25: one input source per connected keyboard or pointer
 kernel raw bus -> inputd -> greeter / xuid
 Settings, btctl --os.lazy.bt.v1--> btd
```

## A keystroke at boot

1. **`usbd` binds the radio.** It starts right after `inputd` (so a key
   typed at boot on a wired keyboard has a reader) and scans one root port per loop pass. `class.rs`
   binds interface 0 to `bt.rs`; interface 1 (isochronous voice) is left
   alone. It registers `os.lazy.usb.bt` on the pass after the bind and sends
   `Ready` to `init` then, or when its scan finds nothing: that is what
   releases `btd`. One controller at a time; a SuperSpeed controller (1024-byte
   bulk packets) is skipped (`USBD:BT:SKIP`).
2. **`init` starts `btd`** after `usbd` is ready and `inputd` serves
   (`deps: ["usbd", "inputd"]`, `Restart::Always`), so the registry name is
   found on the first look. A normal build passes it no arguments.
3. **The transport.** `btd` creates one shared buffer holding the `Commands`
   and `Events` rings (`libs/framering`) and attaches it with `AttachRing`.
   Each side arms its ring before sleeping and the producer rings the
   doorbell only if it was armed, so neither polls. `btd` sleeps on the
   doorbell and its API endpoint until the host's next deadline.
4. **The bond file is read before the controller is up** (`BT:BOND:LOAD n=`),
   from `/btd/bonds`.
5. **Firmware phase.** `libs/bthost/src/firmware.rs` runs between the
   transport attaching and `bthci::Init`: the vendor comes from the USB ids in
   `Describe()`, the file the radio's own version answer names is read through
   `libs/fwload` from the firmware tree, and `libs/btfw` downloads it
   (`BT:FW:VENDOR|FILE|DONE`). A radio that already runs firmware, or a
   generic dongle, skips it. An Intel part may leave the bus and return
   (`Done::Reenumerate`): `usbd` withdraws and re-registers `os.lazy.usb.bt`
   and `btd` tells the registrations apart by their kernel object and restarts
   from `HCI_Reset`. Re-enumerations are counted in `btd`'s memory; after
   three, `BT:FW:FAIL reason=reenumeration-cap` and it touches nothing until
   the radio is replugged.
6. **The fast path.** `Reset`, buffer sizes, event masks, Secure Simple
   Pairing, page scan and an LE accept-list auto-connect for the bonded
   devices come first; then `BT:READY ms=`. The local name, the full feature
   read and the other cosmetic commands follow, then `BT:CTRL:PASS`.
7. **Reconnect.** A bonded device that pages the host is answered from the
   bond store (`Link_Key_Request`). A device that waits to be paged is paged by
   the host, one at a time, five attempts and then `BT:RECONNECT:GIVEUP`
   (three after a link is lost). LE devices reconnect through the accept list;
   the host also scans while a bonded device with an IRK is absent and
   resolves resolvable private addresses itself (`rpa_matches`; no controller
   resolving list). A reconnect needs no discovery: the stored Report Map picks
   the decoder and the stored CCCD handles are written.
8. **Encryption before anything.** BR/EDR: authentication and encryption
   complete before L2CAP admits the HID PSMs. LE: `LE_Start_Encryption` with
   the stored LTK before the device is asked anything.
9. **The input source.** Only when both HID channels are open (BR/EDR) or the
   CCCD write was answered (LE) is a source registered, with the class of the
   bond's record. Reports go through `libs/usbhid` decoders; on a lost link
   the decoder releases every held key before the source closes, and if `btd`
   dies the kernel releases them on close.

## Pairing

Pairing is never on by default. A normal build starts `btd` with no
arguments and a caller of `os.lazy.bt.v1` opens it; the harness-only `pair=`
argument (`LAZYOS_BT_TRACE=1`) opens a bounded window without one.

- **BR/EDR.** Secure Simple Pairing with host IO capability DisplayYesNo,
  done by the controller: a mouse (NoInputNoOutput) pairs with Just Works, a
  keyboard (KeyboardOnly) with Passkey Entry, a device that can show a number
  with Numeric Comparison, which the caller answers. Legacy PIN pairing and
  debug keys are refused.
- **LE.** LE Secure Connections only, in `libs/btsmp`, with the host's own
  P-256 and AES-CMAC (the controller only encrypts the link). Just Works and
  Passkey Entry; entropy comes from the kernel's `random` syscall and an
  unseeded host refuses to pair. Legacy pairing is refused
  (`Error::LegacyPairingRefused`).
- **The keyboard-must-be-authenticated rule.** A keyboard half is admitted
  only with an authenticated key (Passkey Entry or Numeric Comparison). SMP
  cannot know a device is a keyboard before GATT discovery, so an LE Just
  Works pairing is taken as far as discovery and, if the device has a keyboard
  report, dropped with no bond made (`BT:REFUSE
  reason=keyboard-unauthenticated`). On BR/EDR a Just Works device presenting
  a keyboard descriptor is refused and its bond dropped; a combo device keeps
  only its pointer half. A mouse-class Just Works bond therefore cannot later
  present itself as a keyboard.
- **The interface (`os.lazy.bt.v1`).** `List` is open to a session user.
  `Status`, `Subscribe`, `StartDiscovery`, `StopDiscovery`, `Pair`,
  `ConfirmNumber`, `CancelPairing` and `Forget` are for the session user or
  `elevd`. A session user is an account uid (1000 to 59999) with a login
  session; `logind` opens one at a time on the local console. Root, `_bt`
  itself, other services and callers outside a session get `EACCES`, and the
  interface is in `devtool::DENY`. No method returns a key. Discovery only
  lists (Peripheral-class BR/EDR results, LE advertisers of HID). `Pair`
  opens a 60 s window for one address, with inquiry scan on only while it is
  open, closed again when the pairing ends, fails or is cancelled. The
  passkey travels only in the `Status` reply and the `PairingEvent` messages
  to the session user.

## The bond store

`/btd/bonds` (`fhs::state::BTD_BONDS`; `bonds.new` is the temporary,
`bonds.bad` where a refused file is moved). `btbond` defines it: magic `LZBT`,
layout version 1 (a new layout drops the old one), CRC-32, at most 32 bonds,
bounded names and descriptors, keyed by address and address kind. A record
holds the link key or LTK/IRK/EDIV/Rand, key size, the authenticated and
Secure Connections flags, the input class, name, and (so a reconnect needs no
rediscovery) the report descriptor and GATT report handles. It is written to
`bonds.new` and renamed, with one retry on failure. Bonds belong to the
machine, not a user, since the greeter has no user yet.

It is not under `/conf` because `/conf` is root's private tree (0700) and
`_bt` is not root: first boot showed `EACCES`. The image build creates `/btd`
mode 0700 owned by `_bt`, the same reason the account database has
`/accounts`.

## Security

| Rule | Enforced in |
|---|---|
| The stack runs as `_bt` with `CAP_INPUT_SOURCE` and nothing else | `init` (`service_creds.rs`, `BT_CRED`); `libs/btpolicy` |
| Only `_bt` and root call `os.lazy.usb.bt` | `usbd/bt_service.rs` (`may_use_transport`); the interface is in `devtool::DENY` |
| Only `usbd` (`_usb`) may register `os.lazy.usb.bt` and only `btd` (`_bt`) `os.lazy.bt`, unlabelled, outside a session, root excluded | `libs/btpolicy/src/names.rs`, asked by the kernel's `OP_REGISTER` gate (`kernel/src/ipc/policy.rs`) |
| Only a session user or `elevd` may use `os.lazy.bt.v1` | `btd/api.rs` with `btpolicy::may_use_api`, on the kernel-stamped uid, label and session |
| `usbd` and `btd` treat each other as hostile: indices and lengths read once and validated, frames copied before use, a breaking peer poisons the ring and the attachment is dropped; a command or ACL packet whose length field disagrees with its frame never reaches the device | `usbd/bt_service.rs`, `bt_out.rs`, `libs/framering` |
| An unbonded device is refused at the connection request (BR/EDR) or its link dropped (LE), unless a pairing window names it | `libs/bthost` (`conn.rs`, `le.rs`; `BT:REFUSE reason=unbonded`) |
| A bonded address grants nothing without the bond's key on an encrypted link: the HID PSMs are refused until then | `libs/btl2cap` `set_allowed` driven by `bthost/chan.rs` (`reason=psm`) |
| LE ATT is read only on an encrypted link, ATT held across the start of encryption is dropped, a notification counts only after the host's own CCCD write was answered | `bthost/le_gatt.rs` (`att-unencrypted`, `notification-unsolicited`) |
| SMP data that races `Encryption_Change` is replayed after it; a peer's early L2CAP request is held up to 500 ms for the event, then handled | `bthost/le_sec.rs`, `bthost/link.rs` (`ENCRYPT_GRACE_MS`, `DEFER_MS`) |
| A BR/EDR link opens no HID PSM until `Read_Encryption_Key_Size` answers 16 (KNOB, CVE-2019-9506); a shorter key is refused and dropped, its fresh bond with it | `bthost/knob.rs` (`BT:REFUSE reason=key-size`) |
| A keyboard needs an authenticated key | `bthost/chan.rs`, `bthost/le_gatt.rs` |
| Hostile input is bounded: 8 links, bounded deferred data, reassembly, discovery results (32), pending commands, ACL queue; hostile parsers are fuzzed | `bthost/link.rs`, `libs/bt*` parsers, `fuzz/fuzz_targets/bt*.rs` |
| Keys never leave `btd`: no interface returns one, no trace carries one, `Debug` hides them | `btbond`, `btd/api.rs`, `user::trace` |
| A default-build trace shows only the last two address octets and never report contents; `trace=1` (harness build) adds `BT:KEY`, `BT:REPORT` and the passkey | `btd.rs`; the Settings window redacts addresses in its own message trace |
| The bond file is decoded from untrusted bytes and a bad one is moved aside | `libs/btbond`, `bthost/store.rs` |

## Boot latency

Spans and budgets are in [perf/bluetooth-boot.md](../perf/bluetooth-boot.md).
Every step prints `SPAN step= ms=` (`INIT:SPAN`, `USBD:SPAN`, `BT:SPAN`, then
`BT:READY ms=` and `BT:RECONNECT link_at= report_at=`), and `tools/bt/spans.py`
puts them in order. `tools/bt/run.py --boot-typing` and `--le-boot-typing`
fail above a budget on a hardware-accelerated guest (WHPX or KVM); under
`--accel none` only the order is checked.

| | Budget | Measured on WHPX |
|---|---:|---:|
| Radio ready after `usbd` started | 1500 ms | 416 to 623 ms (one 1706 ms with `init` busy) |
| First report published after the link | 250 ms | 73 to 108 ms |

Before B6 these were 800 to 1170 ms and 64 to 67 ms respectively, with the
radio attached 1 to 2 s late because `btd` looked for the registry name once a
second. Roughly 160 ms of what is left is the USB reset and debounce.

## Limits and known gaps

- **Keys typed before a consumer exists are dropped**, as on PS/2: the raw bus
  keeps a ring per reader and `inputd` queues for a focused session only. A
  key typed before `inputd` attaches or before the greeter takes focus is
  lost.
- **A dongle plugged in after `btd` started is found by a look once a second**
  (`WAIT_TICKS` in `btd.rs`), the only timed wake besides deadlines and a
  10 ms retry while the command ring is full. Messenger has no registry watch
  to park on instead.
- **SuperSpeed controllers are skipped** (their 1024-byte bulk packets do not
  fit a pipe window). One controller at a time.
- **Intel parts: TLV hardware variants `0x17` to `0x1b` and the legacy `0x0b`,
  `0x0c`, `0x11` to `0x14` are handled; `0x1c` and later (the "intermediate
  loader", unlocker flows), the `.bseq` flow of 7260/3160 and DSBR/SAR/PPAG
  are not** and fail with `BT:FW:FAIL reason=unsupported`. Vendor
  identification uses USB ids only.
- **LE legacy pairing and BR/EDR legacy PIN pairing are refused.**
- **Nothing is verified on a real radio yet.** Everything above is exercised
  against `libs/btsim` and `fakebt`; the simulated radios verify no firmware
  signature. The 4 s Intel firmware download was measured on the simulator
  and says nothing about a real part. The bench procedure is
  [agents/hardware.md](../agents/hardware.md), "Bluetooth controllers".
- **A machine whose only keyboard is Bluetooth needs another input device for
  the first pairing**; a greeter-side pairing dialog is a follow-up.
- **Full-speed radios (both boxes' internal ones)** have 64-byte bulk and
  16-byte interrupt packets, which the high-speed `fakebt` does not exercise.

## Verification

```bash
cargo test -p bthci -p btl2cap -p btbond -p btsim -p btsmp -p btsdp -p bthid -p bthost -p btgatt -p btfw -p btusb -p btpolicy -p fakebt
python tools/bt/test_judge.py        # the judges fail when they should
python tools/bt/run.py --pair        # QEMU + fakebt; also --reboot, --refuse, --kill, --le-*, --fw ...
python tools/bt/run.py --boot-typing # boot spans against the budgets
```

CI is [bt.yml](../../.github/workflows/bt.yml). The `--ui-pair` and `--btctl`
modes boot a desktop image (xui apps, zig, core packages) and are run by hand.
