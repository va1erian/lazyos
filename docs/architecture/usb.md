# USB HID: `usbd`, the xHCI keyboard, mouse and tablet driver

**What it is.** `usbd` is an ordinary ring-3 driver for USB keyboards, mice and
tablets on an xHCI controller ([usb-hid-plan.md](../usb-hid-plan.md)). It
claims the controller through the device syscall (23), drives it by polling,
and publishes what the devices report onto the kernel's raw input bus through
**input sources** (syscall 25). From there `inputd` treats a USB key or pointer
exactly like a PS/2 one: one keymap, one cursor, one stream. The driver holds no
input policy (no layout, no repeat, no cursor, no clamping).

**Key files**

| Path | Role |
|---|---|
| `libs/xhci/` | Register, TRB and context layouts; command, transfer and event ring state machines behind an MMIO/DMA trait (host-tested) |
| `libs/usbhid/` | Device/configuration descriptors (`desc`), boot keyboard and mouse reports (`boot`), HID report descriptors and the report-protocol pointer decoder (`report`); three fuzz targets |
| `libs/usbpolicy/` | `_usb` (uid 904) and its ACL rules: claim, map and DMA on `os.kernel.dev.usb` |
| `kernel/src/input/sources.rs`, `rawsys.rs` | Input sources: register/publish/close, kernel-stamped device ids, class checks, a token bucket per slot, release on close and on task death |
| `user/src/bin/usbd.rs`, `usbd/` | The driver: `hc.rs` (controller, rings, event pump), `device.rs` (enumeration, control and interrupt transfers, Disable Slot), `hid.rs` (decoders to bus records), `mem.rs` (BAR and DMA regions) |
| `tools/usb/` | The harness: `run.py` (QEMU sessions), `judge.py` (the verdict), `test_judge.py` |

## How a keystroke gets in

1. `init` starts `usbd` as `_usb` with only `CAP_DEV_CLAIM | CAP_INPUT_SOURCE`
   (`USBD:CRED`). It claims the first PCI function of class `0C/03/30`, maps
   BAR 0, resets the controller and gives it a DCBAA, scratchpads, a command
   ring and an event ring (`USBD:XHCI`).
2. Every root port with a device is reset; the device is given a slot
   (Enable Slot, Address Device) and its descriptors are read into a copy and
   parsed by `libs/usbhid` (`USBD:DESC:*`). A boot keyboard or mouse is
   switched to the boot protocol; any other HID interface has its report
   descriptor read and searched for a pointer (a tablet). Its interrupt-IN
   endpoint is configured and one transfer is kept in flight
   (`USBD:HID:KBD|MOUSE|TABLET`).
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
so memory is bounded by the 8 slots `usbd` enables however long the churn. A
Disable Slot that fails keeps its memory out of reuse (`USBD:SLOT:LEAK`).

## Security

- **No ambient authority.** `_usb` holds two capabilities. `CAP_INPUT_SOURCE`
  lets it publish but not read the bus (`CAP_INPUT_RAW`, `inputd`'s alone), so
  a compromised `usbd` cannot keylog the PS/2 keyboard. The kernel stamps its
  records with device ids of its own, so it cannot impersonate another device.
- **Class rules.** USB host controllers are their own ACL class,
  `os.kernel.dev.usb`; `libs/usbpolicy` grants `_usb` claim, map and DMA on it
  and nothing else, and no other uid that class (pinned by
  `dev_suite::sys_usb_driver_policy_is_exactly_the_class_rules`). Like every
  class rule today it takes effect once a policy loads.
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
| Host unit | `xhci` (12): ring wrap and cycle bits, Link chain bits, event ring read order, contexts, PORTSC writes. `usbhid` (23): QEMU's golden descriptors (keyboard, mouse, tablet in both QEMU variants), boot decoders, report descriptors with report ids, Push/Pop, hostile streams. `usbpolicy` (2) | `cargo test -p xhci -p usbhid -p usbpolicy` |
| Fuzz | `usbdesc`, `hidreport`, `hidreportdesc` (seeded tests and cargo-fuzz targets) | `cargo test -p usbhid --features fuzz` |
| Kernel | `input_bus_suite` source cases (capability gate, class and range checks, release on close and death, rate limit, stress); `dev_suite` class map and the `_usb` policy, with a claim/release stress | `python tools/test/run.py --accel none` |
| End to end | QEMU with `qemu-xhci`, a USB keyboard and mouse and no i8042: typed text and pointer steps judged from what reached `inputd` | `python tools/usb/run.py` |
| Variants | `--ps2` (PS/2 and USB side by side), `--hotplug 200` (unplug/replug, held key and button, bounded DMA), `--tablet` (absolute cursor on exact pixels), `--restart` (crash, release, re-enumerate), `--machine q35 --virtio-disk` | `.github/workflows/usb.yml` |

## Not done

- USB hubs, mass storage and any non-HID class; composite devices beyond their
  first HID interface; keyboard LEDs (`SET_REPORT`).
- Interrupts: `usbd` polls. Registering its sources raises it to the
  Interactive class, so a console repaint no longer starves it (QEMU's
  keyboard holds only 16 events); every interrupt endpoint is polled at most
  once per millisecond, which bounds what a flooding device costs. Under TCG
  keys can still come late or repeat; KVM is the reference
  ([usb-hid-plan.md](../usb-hid-plan.md) risk 10).
- `usbctl` and `idl/usb.midl` (a read-only device list): optional in the plan,
  not built yet; the serial markers are the only device listing.
- The class rules wait for a policy loader, like every driver's.
