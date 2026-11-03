# USB HID: `usbd`, the xHCI keyboard, mouse and tablet driver

**What it is.** `usbd` is an ordinary ring-3 driver for USB keyboards, mice and
tablets on xHCI controllers ([usb-hid-plan.md](../usb-hid-plan.md)), written
for real Intel controllers as well as QEMU's
([real-pc-boot-plan.md](../real-pc-boot-plan.md) H3). It claims every
controller through the device syscall (23), drives them by polling,
and publishes what the devices report onto the kernel's raw input bus through
**input sources** (syscall 25). From there `inputd` treats a USB key or pointer
exactly like a PS/2 one: one keymap, one cursor, one stream. The driver holds no
input policy (no layout, no repeat, no cursor, no clamping).

**Key files**

| Path | Role |
|---|---|
| `libs/xhci/` | Register, TRB and context layouts (32- and 64-byte contexts, hub and TT fields); command, transfer and event ring state machines behind an MMIO/DMA trait; extended capabilities (`extcap`: BIOS handoff, Supported Protocol); device location (`route`: route string, TT); control requests (`setup`) (host-tested) |
| `libs/usbhid/` | Device/configuration descriptors (`desc`: every interface and endpoint, SuperSpeed companions), hub descriptors and port status (`hub`), boot keyboard and mouse reports (`boot`), HID report descriptors and the report-protocol pointer decoder (`report`); three fuzz targets |
| `libs/usbpolicy/` | `_usb` (uid 904) and its ACL rules: claim, map and DMA on `os.kernel.dev.usb` |
| `kernel/src/input/sources.rs`, `rawsys.rs` | Input sources: register/publish/close, kernel-stamped device ids, class checks, a token bucket per slot, release on close and on task death |
| `user/src/bin/usbd.rs`, `usbd/` | The driver: `hc.rs` (one controller: handoff, reset, rings, event pump), `port.rs` (root ports: power, debounce, reset, USB 3 training and warm reset), `bus.rs` (one controller's device tree: attach, retry, detach children first), `device.rs` (any device: address at its location, descriptors, control transfers with stall recovery, Configure Endpoint), `pipe.rs` (endpoint rings; interrupt-IN report queues), `class.rs` (class dispatch), `hub.rs` (hub class), `hid.rs` (decoders to bus records), `mem.rs` (BAR and DMA regions) |
| `libs/btusb/`, `libs/btpolicy/`, `user/src/bin/usbd/bt*.rs`, `tools/bt/` | Bluetooth controllers (`LAZYOS_BT=1`, [bluetooth-plan.md](../bluetooth-plan.md) B2): packet framing, the `_bt` uid, the function and its `os.lazy.usb.bt` server, the fake dongle and harness |
| `libs/bthost/`, `libs/btsdp/`, `libs/bthid/`, `libs/btgatt/`, `libs/btsmp/`, `user/src/bin/btd.rs`, `user/src/bin/btd/` | The Bluetooth host (B3, B4): `btd`'s logic and its I/O shell, the SDP client, HIDP and report decoding, the ATT/GATT client and HOGP discovery, LE Secure Connections |
| `tools/usb/` | The harness: `run.py` (QEMU sessions), `judge.py` (the verdict), `test_judge.py` |
| `libs/usbaudio/`, `user/src/bin/usbd/audio*.rs` | USB audio (`LAZYOS_USB_AUDIO=1`, [usb-audio-plan.md](../usb-audio-plan.md)): UAC 1.0/2.0 descriptors, alternate and rate choice, the isochronous OUT ring and explicit feedback, the `os.lazy.usb.audio.v1` server `sndd usb=1` drives; harness `tools/sound/usb_run.py` |

USB sticks (mass storage) are served to the kernel as block devices by the
same driver (`class.rs` binds them to `msc.rs`): [usb-storage.md](usb-storage.md).

## How a keystroke gets in

1. `init` starts `usbd` as `_usb` with only `CAP_DEV_CLAIM | CAP_INPUT_SOURCE`
   and, for sticks, `CAP_BLOCK_PROVIDER` (`USBD:CRED`). It claims **every** PCI function of class `0C/03/30` (an
   Arrow Lake desktop has a CPU-side and a chipset controller; one failing
   does not stop the others), maps BAR 0 (32- or 64-bit, any address) and,
   before any other register write, takes the controller from the BIOS:
   the OS-owned semaphore of the USB Legacy Support capability, a bounded
   wait (1 s) for the BIOS to let go (forced after it, as Linux does), every
   SMI turned off. It then halts and resets it (waiting out Controller Not
   Ready, 1 ms after HCRST for Intel), reads the Supported Protocol
   capabilities (which root ports are USB 2 and which USB 3, and their
   speed IDs), sizes contexts from `CSZ` (32 or 64 bytes), and gives it a
   DCBAA, the scratchpads `HCSPARAMS2` asks for, a command ring and an event
   ring (`USBD:XHCI hc=<n> ... usb2= usb3= csz64= handoff=`). Root ports
   with power switches (`PPC`) are powered.
2. Every connected port is enabled the way its protocol wants: a USB 2 port
   is debounced (100 ms), reset and given its recovery time (10 ms); a USB 3
   port trains on its own and gets a warm reset only when stuck (Polling
   timeout, SS.Inactive, Compliance). The device is given a slot (Enable
   Slot, Address Device with its route string, root port and, for a low- or
   full-speed device behind a high-speed hub, the hub's transaction
   translator), endpoint 0 starts at its speed's default packet size (8, 64,
   64, 512) and is corrected by Evaluate Context from the first 8 bytes of
   the device descriptor. Its descriptors are read into a copy and parsed by
   `libs/usbhid` (`USBD:DESC:*`). Enumeration is retried twice with a fresh
   reset (`USBD:PORT:RETRY`). `class.rs` then binds each interface by class:
   every boot keyboard or mouse interface of the device, wherever it sits in
   a composite device, is switched to the boot protocol (`SET_PROTOCOL`, and
   `SET_IDLE` for keyboards; a stall of either is recovered and tolerated);
   with none, a HID interface whose report descriptor holds a pointer (a
   tablet). A hub (`USBD:HUB`) is told it is a hub (slot context Hub, ports,
   TT think time, multi-TT when it has it; `SET_HUB_DEPTH` for SuperSpeed
   hubs), its ports are powered and its status-change endpoint watched; each
   port it flags is examined with `GET_STATUS` and a new device below it is
   enumerated the same way, up to five hub tiers and 15 ports per hub. Each
   interrupt-IN endpoint gets its interval from the speed's encoding
   (frames for low and full speed, `2^(bInterval-1)` microframes above), its
   Max ESIT payload and burst from the descriptor (and SuperSpeed companion),
   and eight transfers kept in flight (`USBD:HID:KBD|MOUSE|TABLET`).
3. Each completed report is decoded into edges and deltas: key presses and
   releases (a boot report is a *state*; the bus carries *edges*), relative
   motion, absolute position (`0..=0xFFFF`), wheel notches, button edges. They
   are published in a batch through the interface's input source: keyboard,
   pointer or tablet class. The kernel stamps the device id, refuses records
   the class may not carry and rate-limits the source.
4. `inputd` reads the bus and applies the keymap and the cursor; `xuid` takes
   the pointer from `inputd`.

## Hot-plug and memory

A port-status-change event attaches a new device or detaches one that went
away; the boot scan uses the same path, one root port per pass of the main
loop (`usbd/boot.rs`), so the Messenger names (`os.lazy.usb.bt`) are served
and answered between two devices, not after the slowest one. Detach releases everything the device
held (keys and buttons, so nothing stays stuck), runs Disable Slot, and drops
the slot's queued transfer events.

**DMA is never freed while `usbd` runs.** The kernel treats a driver freeing
its own DMA buffer as stopping the device ([audio.md](audio.md), the `sndd`
lesson); for a controller with other devices still running that would be a
stall for all of them. A detached device's 12 KiB region goes back to a
per-slot pool once Disable Slot completes and serves that slot's next device,
so memory is bounded by the 12 slots `usbd` enables per controller however
long the churn (the kernel records 16 DMA buffers per claim: the core
region, the scratchpads and the 12 device regions). Each device region holds
its input and output contexts (64-byte ones fit), endpoint 0's ring, a
control buffer and up to four pipe windows (ring and report buffers). A
Disable Slot that fails keeps its memory out of reuse (`USBD:SLOT:LEAK`).
Unplugging a hub detaches everything below it first (`USBD:DETACH ...
(its hub went away)`). A halted interrupt endpoint is reset and restarted
(Reset Endpoint, Set TR Dequeue Pointer, `CLEAR_FEATURE(ENDPOINT_HALT)` after
a stall); four failures in a row give the device up. A controller reporting
a fatal error (`HSE`/`HCE`) is dropped with its devices released
(`USBD:XHCI:ERROR`) and the others keep running.

## Adding a device class

`class.rs` dispatches on the interface class code. A new class adds a
`Function` variant, an arm in `bind_interface` (its class requests through
`Device::control_in`/`control_out`; its endpoints through
`Device::open_pipe`, a ring and context it submits its own transfers on, or
`Device::open_reports` for interrupt-IN reports `usbd` refills), and arms in
`Function::owns`, `Function::on_transfer` (events of its own pipes) and
`Function::close`. `bind` configures every opened pipe in one Configure
Endpoint. Descriptors for any class come from `usbhid::desc::Interface`
(class, subclass, protocol, endpoints with type, direction and burst);
bulk endpoint contexts from `xhci::context::EndpointContext::bulk`.

## Bluetooth controllers (`LAZYOS_BT=1`)

[bluetooth-plan.md](../bluetooth-plan.md) phase B2. `class.rs` binds interface
0 of a USB Bluetooth controller (class `E0/01/01`) to `bt.rs`; interface 1
(the isochronous voice channel and its alternates) is left alone. HCI commands
go out as class requests on endpoint 0 (`bmRequestType` 0x20, `bRequest` 0),
events come in on the interrupt-IN endpoint and ACL data on the bulk pair.
Each IN endpoint keeps chunk-sized transfers queued (a packet longer than
`wMaxPacketSize` arrives in several) and `libs/btusb` cuts packets out of the
chunks by the event length byte and the ACL length field: that is all the
Bluetooth `usbd` knows, because `btd` parses what a stranger in radio range
sends. One controller at a time; SuperSpeed controllers (1024-byte bulk
packets) are skipped.

`usbd` serves the controller as `os.lazy.usb.bt` (`idl/usb_bt.midl`) while it
is bound, registered on the pass after the bind (the other ports of the boot
scan follow), and tells `init` (`Ready`) at that moment, or when the scan ends
with none: that is what starts `btd`. It withdraws the name (after a `Notify` with `Gone`) when it is
unplugged. Only `_bt` (uid 916, `libs/btpolicy`) and root may call it. Packets
never travel in a message body: `btd` creates one shared buffer holding the
`Commands` ring (frames from `btd`: an H4 indicator byte and an HCI command or
ACL packet) and the `Events` ring (frames from the controller), the
`libs/framering` rings of the NIC interface, and a notify endpoint; `usbd`
validates every index and length, checks that a command's or ACL packet's own
length field matches its frame before it reaches the device, queues events a
full ring has no room for (bounded, then counted as dropped) and wakes `btd`
only when it armed the ring, and `btd`'s `Kick` wakes `usbd` the same way, so
neither polls. Evidence: `USBD:BT:BIND|UNBIND|SERVE|WITHDRAW|ATTACH`.

### `btd`, the host stack (B3)

[bluetooth-plan.md](../bluetooth-plan.md) phase B3. `btd` is a boot service
(`init`, as `_bt`, `Restart::Always`) that runs the Bluetooth BR/EDR HID host
against the controller `usbd` serves. Its decisions live in `libs/bthost`, a
sans-IO state machine (no I/O, no clock, no `unsafe`) built on `bthci` (packets,
command credits, ACL buffers, the bring-up script), `btl2cap`, `btsdp` (the
HID record), `bthid` (HIDP framing, report decoding with `usbhid`) and `btbond`.
The I/O shell, `user/src/bin/btd.rs` and `btd/`, feeds it what arrives (event
and ACL packets, the time) and carries out what it asks: commands and ACL to the
rings (`link.rs`), input sources to the kernel (`sources.rs`, syscall 25, the
same record mapping as `usbd`), the bond file (`disk.rs`, over
`bthost::store::Fs`) and evidence lines. It sleeps on the transport doorbell
until `Host::next_deadline`; nothing polls on a timer. `init` starts it once
`usbd` is ready (above) and `inputd` serves, so it finds the controller on its
first look; only a dongle plugged in later makes it look once a second (there
is no registry watch to park on). `usbd` itself starts in the first wave, with
no dependency on `inputd`: it publishes to the bus directly, and `inputd`
reads the table of sources when it attaches. A key pressed while no `inputd`
reads the bus, or before the greeter takes focus, goes nowhere: the bus keeps
a ring per reader and `inputd` queues for a focused session only.

- **Bring-up and the fast path.** `Reset`, buffer sizes, event masks, Secure
  Simple Pairing and page scan come first (`BT:READY ms=` since boot when the
  bonded devices can connect), then the cosmetic commands; `BT:CTRL:PASS`
  follows. The bond file is read before the controller is attached
  (`BT:BOND:LOAD n=`).
- **Who may type** (plan decision 6). An unbonded device that pages the host is
  rejected at the connection request (`BT:REFUSE reason=unbonded`) unless the
  pairing window names it. A bonded device is admitted; L2CAP then refuses the
  HID PSMs until the link is authenticated and encrypted with the bond's key
  (`btl2cap`'s `set_allowed`, `BT:REFUSE reason=psm`). A source is registered
  only when both HID channels are open on such a link, with the class of the
  bond's record: a keyboard half only for an authenticated key (type 5, or 8
  if Secure Connections host support is enabled, which it is not yet), so a
  Just Works device that presents a keyboard descriptor is refused and its
  bond dropped (`BT:REFUSE reason=keyboard-unauthenticated`), and a combo
  device keeps only its pointer. A peer's first L2CAP request can beat the
  `Encryption_Change` (events and data are different USB endpoints): data from
  a link that was offered its key is held for up to 500 ms for it, then
  processed as it is.
- **Pairing** (harness only until B5). SSP with host IO capability
  DisplayYesNo: a mouse (NoInputNoOutput) pairs with Just Works, a keyboard
  (KeyboardOnly) with Passkey Entry, the host showing the passkey (traced only
  with `trace=1`). Legacy PIN pairing, debug keys and unauthenticated numeric
  comparison are refused. The record (SDP `ServiceSearchAttribute`, bounded and
  hostile-safe, with continuation) gives the report descriptor, subclass and
  `HIDReconnectInitiate`/`HIDNormallyConnectable`; keyboards in report protocol
  are decoded by `usbhid::report::keyboard` (the boot layout behind a report
  id), pointers by `usbhid::report`, and a device whose descriptor is not usable
  is switched to the boot protocol (`SET_PROTOCOL`).
- **Reconnect.** A device that pages the host is answered from the bond store
  (`Link_Key_Request`); one that waits to be paged is paged by the host after
  the fast path, one at a time, five attempts (`BT:RECONNECT:GIVEUP`), and
  again, three attempts, after a link to it is lost.
- **Links end cleanly.** On a lost link the decoder releases every held key
  before the source closes; if `btd` itself dies the kernel releases them.
- **State.** The bond store is `/btd/bonds` (`fhs::state::BTD_*`), 0600, owned
  by `_bt`, written to `bonds.new` and renamed. It is a top-level directory
  because `_bt` is not root and `/conf` is root's alone (0700), the reason the
  account database has `/accounts`.

Evidence: `BT:BOND:LOAD`, `BT:READY ms=`, `BT:CTRL:PASS`, `BT:CONN`,
`BT:ENCRYPT`, `BT:PAIR:START|PASSKEY|PASS|FAIL|STALE`, `BT:HID:KBD|MOUSE`,
`BT:RECONNECT ms=` (link up to first report), `BT:REFUSE reason=`,
`BT:BOND:SAVE|FORGET`, `BT:CONN:DOWN`, `BT:CTRL:GONE`. Addresses show their last
two octets; no key is ever printed. A harness build (`LAZYOS_BT_TRACE=1`) adds
`BT:KEY` (one line per key edge), `BT:REPORT` and the passkey.

### LE HID over GATT (B4)

The same host, `libs/bthost`, runs LE links beside BR/EDR ones (`le.rs`,
`le_sec.rs`, `le_gatt.rs`); `libs/btgatt` is the ATT client (one request at a
time, the 30 s ATT timeout as a deadline, notifications, bounded discovery
loops) and the HID over GATT discovery (service `0x1812`: Report Map including
long reads, Report characteristics with their Report Reference and CCCD,
Protocol Mode, HID Information, the Boot Keyboard/Mouse Input reports as the
boot-protocol fallback; Battery and GAP are skipped).

- **Radio.** One thing at a time: the accept-list auto-connect that the fast
  path armed (bonded devices at their identity address), or a passive scan
  with a direct connect. The host scans while a pairing window names LE
  (`pair=any`, or `pair=le:AA:..` in a harness build) and while a bonded
  device with an IRK is not connected: it resolves each connectable
  advertisement itself (`btsmp::func::rpa_matches`), so a device that comes
  back under a fresh resolvable private address is recognised (`BT:CONN ...
  rpa=1`). A pairing candidate must advertise a HID appearance or the HID
  UUID and be at an allowed address; one that is refused or fails is not
  connected again in that window.
- **Who may type** (decision 6). An LE link is made only for a bonded device
  (known address or IRK match) or a pairing candidate; any other connection the
  controller reports is dropped (`BT:REFUSE reason=unbonded`). A bonded link
  starts encryption with the stored LTK (`LE_Start_Encryption`) before it
  asks the device anything; ATT data is read only on an encrypted link, ATT
  that arrives held across the start of encryption is dropped, and a
  notification counts only after the host's own CCCD write was answered
  (`BT:REFUSE reason=att-unencrypted|notification-unsolicited`). SMP data
  that races the `Encryption_Change` is held and replayed after the event is
  handled, so `btsmp` sees `Encryption { ok }` first.
- **Pairing.** LE Secure Connections only (legacy is refused, plan Risk 3);
  Just Works for a device without input, Passkey Entry for a keyboard (shown
  in a `trace=1` harness build; the simulated keyboard learns it through a
  vendor HCI opcode that a normal build never sends). The key is bonded only
  after GATT discovery shows what the device is: a keyboard report with an
  unauthenticated key is dropped, an older bond for it forgotten, and
  `BT:REFUSE reason=keyboard-unauthenticated` traced. The bond (LTK, IRK,
  report map, GATT handles) is stored under the identity address.
- **Reconnect.** No discovery: the stored Report Map picks the decoder, the
  stored CCCD handles are written, the source is registered; `BT:RECONNECT
  ms=` is link up to first report. Pairing entropy comes from the kernel's
  `random` syscall (an unseeded host refuses to pair).

The simulated LE keyboard and mouse (`libs/btsim`: `LePeer`, a GATT server, the
responder side of SMP with the real crypto) and `fakebt`'s `--le-*` options drive
`--le-pair`, `--le-reboot`, `--le-rpa` and `--le-refuse`.

QEMU has no Bluetooth emulation, so the harness is `tools/bt/run.py`: QEMU
with a `qemu-xhci` and `usb-redir` talking to `tools/bt/fakebt`, a host
program serving a fake dongle over usbredir through `libs/btsim`'s controller,
which also plays a BR/EDR keyboard and mouse that pair, bond, reconnect and
type on a script. `--unplug` pulls the dongle out after the bring-up; `--pair`,
`--reboot`, `--refuse` and `--kill` are the BR/EDR HID tests, `--le-pair`,
`--le-reboot`, `--le-rpa` and `--le-refuse` the LE ones ([tools/bt/README.md](../../tools/bt/README.md)).
`--boot-typing` and `--le-boot-typing` are `--reboot` and `--le-reboot` with
the boot's spans (`INIT:SPAN`, `USBD:SPAN`, `BT:SPAN`, `BT:READY`,
`BT:RECONNECT`: kernel start to first report) tabulated and held to the
boot-latency budgets ([perf/bluetooth-boot.md](../perf/bluetooth-boot.md));
`--boot-typing --desktop` boots a desktop image and reads the greeter.

## Security

- **No ambient authority.** `_usb` holds three capabilities (the third,
  `CAP_BLOCK_PROVIDER`, serves sticks: [usb-storage.md](usb-storage.md)). `CAP_INPUT_SOURCE`
  lets it publish but not read the bus (`CAP_INPUT_RAW`, `inputd`'s alone), so
  a compromised `usbd` cannot keylog the PS/2 keyboard. The kernel stamps its
  records with device ids of its own, so it cannot impersonate another device.
- **Class rules.** USB host controllers are their own ACL class,
  `os.kernel.dev.usb`; `libs/usbpolicy` grants `_usb` claim, map and DMA on it
  and nothing else, and no other uid that class. `libs/usbpolicy`'s host
  test pins the table (every rule's actor is `_usb`, its interface the USB
  class, no wildcard); `dev_suite::sys_usb_driver_policy_is_exactly_the_class_rules`
  loads it into the real ACL and checks the decisions, refusing other system
  uids, a driver uid and root. The kernel installs the table at boot with
  every other driver's (`dev::policy`, issue #481), so on every boot `_usb`
  can claim a USB controller and nothing else, and no other non-root uid
  can claim one (`dev_sys_boot_policy_confines_each_driver_to_its_class`).
- **Untrusted devices.** A USB device can be hostile. Every descriptor and
  report is parsed from a copy, every length checked, every loop bounded by its
  input; the parsers are fuzzed (`usbdesc`, `hidreport`, `hidreportdesc`).
- **DMA trust.** A driver with DMA is trusted like the kernel until an IOMMU
  exists (`docs/driver-plan.md` D5); this adds no new exposure.

## Restart

`usbd` runs under `init` with `Restart::OnFailure`. If it dies, the kernel
releases everything its sources held and its controller claim (quiescing the
device), `init` restarts it, and the new instance resets the controller and
enumerates every device again. `tools/usb/run.py --restart` proves it with a
crash-test build that exits while holding a key.

## Testing

| Layer | What | Run |
|---|---|---|
| Host unit | `xhci` (23): ring wrap and cycle bits, Link chain bits, event ring read order, abandoning a halted ring, contexts in both sizes (QEMU only has 32-byte ones, so the 64-byte layout is proven here), hub and TT slot fields, route strings, PORTSC writes, the BIOS handoff against a model BIOS (releasing, never releasing, absent) and hostile capability lists, Supported Protocol and PSI tables. `usbhid` (29): QEMU's golden descriptors (keyboard, mouse, tablet in both QEMU variants), boot interfaces anywhere in a composite device, SuperSpeed companions, hub descriptors and port status (USB 2 and SuperSpeed), boot decoders, report descriptors with report ids, Push/Pop, hostile streams. `usbpolicy` (2) | `cargo test -p xhci -p usbhid -p usbpolicy` |
| Fuzz | `usbdesc`, `hidreport`, `hidreportdesc` (seeded tests and cargo-fuzz targets) | `cargo test -p usbhid --features fuzz` |
| Kernel | `input_bus_suite` source cases (capability gate, class and range checks, release on close and death, rate limit, stress); `dev_suite` class map and the `_usb` policy, with a claim/release stress | `python tools/test/run.py --accel none` |
| End to end | QEMU with `qemu-xhci`, a USB keyboard and mouse and no i8042: typed text and pointer steps judged from what reached `inputd` | `python tools/usb/run.py` |
| Variants | `--ps2` (PS/2 and USB side by side), `--hotplug 200` (unplug/replug, held key and button, bounded DMA), `--tablet` (absolute cursor on exact pixels), `--restart` (crash, release, re-enumerate), `--machine q35 --virtio-disk`, `--hub` (keyboard and mouse behind `usb-hub`, full speed with a route string; the hub unplugged), `--full-speed` (USB 1.1 devices on root ports), `--controllers 2` (keyboard and mouse on different controllers) | `.github/workflows/usb.yml` |

## Not done

- Any class but HID, hub, mass storage ([usb-storage.md](usb-storage.md)) and
  audio playback ([usb-audio-plan.md](../usb-audio-plan.md); isochronous OUT
  and feedback IN endpoints, opt-in); keyboard LEDs (`SET_REPORT`); audio
  capture.
- Not exercised by QEMU, so proven only by host tests until a real machine
  runs it: 64-byte contexts, the BIOS handoff with a BIOS that owns the
  controller, USB 3 warm reset, high-speed hubs with transaction
  translators and multiple TTs, SuperSpeed hubs (QEMU's `usb-hub` is a
  full-speed USB 1.1 hub), low-speed devices. Intel's pre-Z890 EHCI port
  routing (`XUSB2PR`, PCI config space) is not done: Z890 has no EHCI.
- Interrupts (P3.7, `usbd/irq.rs`): each controller is claimed with an
  interrupt endpoint and interrupter 0 raises the INTx line (`IMOD` 40 us);
  the idle loop parks on every controller's endpoint with the Messenger wait
  set, clears `IMAN.IP`/`USBSTS.EINT` and acknowledges before draining the
  ring, and keeps a 100 ms poll as a safety net (`USBD:IRQ hc=<n> armed`, or
  `polled` when the line is refused). A live USB stick still idles in its
  one-tick serve (its request queue is not an endpoint). USB pointer to
  present measured from the xHCI interrupt (`tools/perf/run.py --usb`, WHPX):
  p50 0.28 ms, p99 0.64 ms, against 4.8 ms and 10.4 ms polled.
  Registering its sources raises it to the
  Interactive class, so a console repaint no longer starves it (QEMU's
  keyboard holds only 16 events); every interrupt endpoint is polled at most
  once per millisecond, which bounds what a flooding device costs. Under TCG
  keys can still come late or repeat; KVM is the reference
  ([usb-hid-plan.md](../usb-hid-plan.md) risk 10).
- `usbctl` and `idl/usb.midl` (a read-only device list): optional in the plan,
  not built yet; the serial markers are the only device listing.
