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
python tools/usb/test_judge.py           # the judge fails when it should
cargo test -p usbhid -p xhci             # the libraries (host, seeded fuzz)
```

Verdict (all must pass):

- `tools/input/verify_trace.py --layout us`: `inputd` typed the expected text,
  modifiers, locks and key repeat (the same check the PS/2 input session uses);
- `judge.py`: every key edge `inputd` decoded is one `usbd` sent (`USBD:KEY`,
  same usage and direction, same order); the cursor reached the corner, pressed
  left at (40, 30) and scrolled two notches; the device and configuration
  descriptors (`USBD:DESC:*`) are QEMU's, byte for byte (the golden bytes
  `libs/usbhid` is tested against); no `USBD:FATAL`, `USBD:PANIC` or
  `USBD:PORT:FAIL`.

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
`regions=` never exceeded the 8 slots `usbd` enables (no leak; regions are
reused per slot, never freed). No `USBD:SLOT:LEAK` either: a Disable Slot that
fails keeps its memory out of reuse and is reported.

Serial markers: `USBD:XHCI` (controller up), `USBD:PORT`, `USBD:DESC:DEVICE`,
`USBD:DESC:CONFIG`, `USBD:HID:KBD` / `USBD:HID:MOUSE`, `USBD:READY devices=N`,
`USBD:DETACH port= slot= regions=` (an unplug or a failed pipe);
with `trace=1` (debug services images) `USBD:REPORT <hex>` and `USBD:KEY` per
report and edge. A machine without xHCI prints `USBD:XHCI:NONE` and exits 0.

**TCG.** USB input is polled, and QEMU's `usb-kbd` queues only 16 keycodes.
Under TCG each keystroke makes the text console redraw for about a second, and
`usbd` (a Normal-class task) waits for the CPU meanwhile, so fast typing
overruns QEMU's queue and a press and its release can land more than the
500 ms repeat delay apart. `--accel none` therefore paces the keys (3 s) and
settles after boot (120 s); the repeat can still show up. KVM runs (CI,
`.github/workflows/usb.yml`) are the verdict.
