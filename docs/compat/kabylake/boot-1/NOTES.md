# Boot 1: the Kaby Lake box (i3-7100U), 2026-10-09

First bare-metal boot of `lazyos-usb.img`, UEFI, stick in a rear port, a USB
keyboard and mouse on the box's USB 2 ports (all of them full speed). Photos
and video are with the maintainer; this file is what they showed.

## Result

Boots to the desktop and, after the fixes below, takes keyboard and mouse
input. LazyWriter opened, text typed and saved to the user's home on the stick.
The system stays responsive, the mouse never freezes; windows are laggy and
some waits are long (to investigate, see "Open").

## What worked on the first boot, unmodified

- UEFI loader, kernel, ramdisk (about 130 MiB) from the stick; desktop reached.
- Framebuffer, timer and compositor on the real chipset (the clock advanced).
- Services start; `devd` sees the 15 PCI functions of the survey.
- **RTL8168 driver (`netdrv`)**: `NETDRV:RTL8168 xid=0x541 phy_id=0x001cc880`,
  MSI armed, 256-entry rings, `NETDRV:READY`. `link=false` because no cable was
  plugged in. First run of that driver on real hardware.
- `sndd` on the HDMI codec: `SNDD:HDA codec=2 afg=1 pin=3 ... SNDD:READY`.

## What failed, and how it was found

| Symptom | Cause | Fix |
|---|---|---|
| Boot text huge and clipped; desktop blurry on a 2560x1440 monitor | The loader keeps the firmware's default GOP mode unless asked; this firmware's is 800x600 | Stick image asks for at least 1280x720 (`build_support/usb_image.rs`); the loader then takes the largest mode (2560x1440) and the kernel shows a centred 1080p logical screen |
| Long boot-log lines cut at the pane edge | The kernel's boot panes did not wrap | Panes wrap (`kernel/src/mux.rs`) |
| Log gone when the desktop opened | The panes stop when the compositor binds the screen | `LAZYOS_DIAG_HOLD=<s>` keeps them up (`diag.hold` in `lazyos.cfg`, read by `xuid`) |
| **No keyboard or mouse** | Control transfers were queued with the TRB Chain bit on their Setup and Data stages. QEMU ignores it; Intel's xHCI timed out the first GET_DESCRIPTOR (or stalled it) on every device | `usbd` no longer chains control stages (`user/src/bin/usbd/device.rs`); host test in `libs/xhci` |
| Write failed on Windows | `Set-Disk -IsOffline` is refused for removable media | `write_stick.py` clears the partition table instead |

The diagnosis used three lines the drivers print: `USBD:PORT:RETRY ... timed
out waiting for GET_DESCRIPTOR transfer`, then `USBD:DIAG` (slot state and
address, endpoint 0 state, the controller's dequeue pointer against the ring
base, `USBSTS`): address assigned, endpoint running, dequeue pointer never
advanced, `USBSTS` clean.

## Facts for the plans

- xHCI `8086:9d2f`: 18 ports (12 USB 2, 6 USB 3), 12 slots, **34 scratchpad
  buffers**, 32-byte contexts, 64-bit addressing, `handoff=Released`.
- All devices on the box (keyboard, mouse, Bluetooth, a USB audio device) are
  full speed. QEMU had never exercised full speed.
- `usbd` spends up to 5 s per timed-out step and retries three times, so a
  dead device costs about 15 s of boot (visible as the long wait).
- `devd` matched `10ec:8168` to `netdrv` and the HDA controller to `sndd`.

## Open

- Responsiveness: laggy windows and long waits. Candidates: uncached
  framebuffer writes at 2560x1440 (look for `HW:FB:WC:` in the log), the tick
  source (`HW:TIMER:`), and `usbd` retrying devices it does not bind.
- Link and DHCP on the RTL8168 with a cable.
- The AHCI driver and installing to the SSD (plan steps B1, B4).
