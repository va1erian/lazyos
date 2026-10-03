# USB HID harness

`usbd` (`user/src/bin/usbd.rs`, [docs/usb-hid-plan.md](../../docs/usb-hid-plan.md))
is verified like the sound and network drivers: the serial markers say when the
guest is ready, the verdict is what the input actually did. `run.py` boots QEMU
with `qemu-xhci`, a `usb-kbd`, a `usb-mouse` and **no i8042**
(`-machine pc,i8042=off`), so nothing can arrive over PS/2, then types the
physical key sequence of `tools/screenshot/examples/input_keys.json` and drives
the mouse (corner, +40,+30, left click, two wheel notches).

```bash
python tools/usb/run.py                  # build (LAZYOS_SERVICES=1 LAZYOS_USB=1), boot, judge
python tools/usb/run.py --no-build       # reuse target/lazyos.img
python tools/usb/run.py --ps2            # keep the i8042: PS/2 and USB side by side
python tools/usb/run.py --no-mouse       # keyboard only
python tools/usb/run.py --accel none     # TCG: paced for a slow guest (see below)
python tools/usb/run.py --hotplug 200    # U3: unplug/replug cycles over QMP
python tools/usb/run.py --tablet         # U4: usb-tablet (absolute) instead of the mouse
python tools/usb/run.py --restart        # U5: usbd dies holding a key; init restarts it
python tools/usb/run.py --machine q35 --virtio-disk   # U5: on q35
python tools/usb/run.py --hub            # H3: keyboard and mouse behind a usb-hub; the hub unplugged
python tools/usb/run.py --full-speed     # H3: USB 1.1 keyboard and mouse on root ports
python tools/usb/run.py --controllers 2  # H3: two controllers, keyboard on the second
python tools/usb/test_judge.py           # the judge fails when it should
cargo test -p usbhid -p xhci             # the libraries (host, seeded fuzz)
```

Verdict (all must pass):

- `tools/input/verify_trace.py --layout us`: `inputd` typed the expected text,
  modifiers, locks and key repeat (the same check the PS/2 input session uses);
- `judge.py` requires `DEV:CROSSCLAIM:usb:PASS`: `usbd` (`trace=1`), as
  `_usb`, tried every device of another class and each was refused (#481).
- `judge.py`: every key edge `inputd` decoded is one `usbd` sent (`USBD:KEY`,
  same usage and direction, same order); the cursor reached the corner, pressed
  left at (40, 30) and scrolled two notches; the device and configuration
  descriptors (`USBD:DESC:*`) are QEMU's, byte for byte (the golden bytes
  `libs/usbhid` is tested against); no `USBD:FATAL`, `USBD:PANIC` or
  `USBD:PORT:FAIL`.

**Real-hardware variants** (docs/real-pc-boot-plan.md H3). `--hub` puts
QEMU's `usb-hub` (a full-speed USB 1.1 hub, the only one QEMU has) on root
port 1 with the keyboard and mouse on its ports 1 and 2: `usbd` must
configure the hub (`USBD:HUB`), power its ports, find both devices through
its status-change endpoint and enumerate them at full speed with a route
string (`port=0-5.1`, `port=0-5.2`; QEMU's USB 2 root ports are 5..=8).
`judge.py --hub` checks they were bound one tier below the hub, their
descriptors are QEMU's full-speed ones (endpoint 0 of 8 bytes, 10 ms
intervals), the typing and pointer steps arrived, and when the session
unplugs the hub both devices detach before it. `--full-speed` attaches the
same devices with `usb_version=1` straight to root ports: endpoint 0 starts
at 64 bytes and is fixed to 8 by Evaluate Context. `--controllers N` adds N
controllers with the mouse on the first and the keyboard on the last;
`judge.py --controllers N` checks each came up (`USBD:XHCI hc=<n>`) and the
devices were bound on the right one. What QEMU cannot model (64-byte
contexts, a BIOS that owns the controller, USB 3 warm reset, high-speed
hubs and their transaction translators, SuperSpeed hubs) is covered by the
host tests of `libs/xhci` and `libs/usbhid`.

**Restart** (`--restart`, U5) builds `usbd` with `LAZYOS_USB_CRASH_TEST=1`:
on its first attempt it exits (status 3) right after publishing a key press.
The session holds `x`; `judge.py --restart` checks `usbd` crashed
(`USBD:CRASH:TEST`), `inputd` saw `x` released before the restarted `usbd`
was ready (the kernel releases a dead source's keys), `init` restarted it
(`INIT:RESTART:PASS name=usbd`) as `_usb` both times (`USBD:CRED`), both
devices were bound again, and the keys typed afterwards arrived.

**Tablet** (`--tablet`, U4) swaps the `usb-mouse` for a `usb-tablet`. It
has no boot protocol, so `usbd` reads its report descriptor
(`USBD:DESC:REPORT`), finds X/Y/wheel/buttons with `libs/usbhid::report` and
publishes absolute positions (`ABS_MOTION`, scaled to `0..=0xFFFF`) from a
tablet-class source (`USBD:HID:TABLET`). The session sends QMP absolute moves
to the corner, the far corner and an inner point, clicks and scrolls there;
`judge.py --tablet` checks the descriptor is QEMU's (five- or three-button
variant) and that `inputd`'s cursor landed on the exact pixel each position
maps to on its default 1280x720 screen.

**Hot-plug** (`--hotplug N`, U3) replaces the typing session: QMP
`device_del`/`device_add` unplug and replug the keyboard N times and the mouse
every tenth cycle, with a key (`x`) and the left button held across the first
unplug; then `a b c` is typed on the last keyboard. `judge.py --hotplug N`
checks every cycle detached and re-attached, the held key and button were
released (by `usbd` on detach, traced as `USBD:KEY ... up`), `inputd` saw
exactly `usbd`'s key edges, the typing arrived, and the DMA allocation count
`regions=` never exceeded the 12 slots `usbd` enables (no leak; regions are
reused per slot, never freed). No `USBD:SLOT:LEAK` either: a Disable Slot that
fails keeps its memory out of reuse and is reported.

Serial markers: `USBD:XHCI hc=<n>` (a controller up: its BIOS handoff, USB 2
and USB 3 port counts, context size), `USBD:PORT port=<hc>-<root>[.<hub port>...]`
(`USBD:PORT:RETRY` before another reset, `USBD:PORT:SKIP` for a device with
nothing to bind), `USBD:DESC:DEVICE`, `USBD:DESC:CONFIG`, `USBD:HUB`,
`USBD:HID:KBD` / `USBD:HID:MOUSE`, `USBD:READY devices=N controllers=M`,
`USBD:DETACH port= slot= regions= functions=` (an unplug or a failed pipe;
everything below a hub detaches before it);
with `trace=1` (only on `LAZYOS_USB_TRACE=1` test images, which `run.py`
builds: the trace carries every keystroke, so ordinary USB images never
enable it) `USBD:REPORT <hex>` and `USBD:KEY` per report and edge. A machine without xHCI prints `USBD:XHCI:NONE` and exits 0.

**TCG.** USB input is polled, and QEMU's `usb-kbd` queues only 16 keycodes.
Under TCG each keystroke makes the text console redraw for about a second, and
`usbd` (Interactive, like the console it shares the CPU with) gets only its
turn meanwhile, so fast typing
overruns QEMU's queue and a press and its release can land more than the
500 ms repeat delay apart. `--accel none` therefore paces the keys (3 s) and
settles after boot (120 s); the repeat can still show up. KVM runs (CI,
`.github/workflows/usb.yml`) are the verdict.
