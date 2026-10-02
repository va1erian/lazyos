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
| `tools/usb/` | The harness: `run.py` (QEMU sessions), `judge.py` (the verdict), `test_judge.py` |

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
away; the boot scan uses the same path. Detach releases everything the device
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

- Any class but HID, hub and mass storage ([usb-storage.md](usb-storage.md));
  keyboard LEDs (`SET_REPORT`); isochronous endpoints.
- Not exercised by QEMU, so proven only by host tests until a real machine
  runs it: 64-byte contexts, the BIOS handoff with a BIOS that owns the
  controller, USB 3 warm reset, high-speed hubs with transaction
  translators and multiple TTs, SuperSpeed hubs (QEMU's `usb-hub` is a
  full-speed USB 1.1 hub), low-speed devices. Intel's pre-Z890 EHCI port
  routing (`XUSB2PR`, PCI config space) is not done: Z890 has no EHCI.
- Interrupts: `usbd` polls. Registering its sources raises it to the
  Interactive class, so a console repaint no longer starves it (QEMU's
  keyboard holds only 16 events); every interrupt endpoint is polled at most
  once per millisecond, which bounds what a flooding device costs. Under TCG
  keys can still come late or repeat; KVM is the reference
  ([usb-hid-plan.md](../usb-hid-plan.md) risk 10).
- `usbctl` and `idl/usb.midl` (a read-only device list): optional in the plan,
  not built yet; the serial markers are the only device listing.
