# Plan: basic USB HID support (keyboard, mouse, tablet) with pointer handling in `inputd`

> **Status: in progress, revision 2 (2026-10-01).** P0 (pointer records and
> tail merging on the raw bus, the PS/2 mouse tap) and P1 (`inputmap::Pointer`,
> `inputd` pointer glue, `SetBounds`/`GetPointer`/`PointerEvent`), P2 (`xuid`
> takes the pointer from `inputd`), U0 (`libs/usbhid`, `libs/xhci`) and U1
> (input sources on syscall 25, `CAP_INPUT_SOURCE`) are implemented; U2
> onward are not. Builds on
> [input-plan.md](input-plan.md) (the raw event bus, `inputd`) and
> [driver-plan.md](driver-plan.md) (the device core, userspace drivers). It
> lists USB as a non-goal of the driver plan; this plan lifts that for HID only.
>
> Revision 2 folds **mouse handling into `inputd`**. Revision 1 injected USB
> pointer events into the kernel's `mouse.rs`; that would have deepened the
> kernel pointer path that `input-plan.md` already wants to retire. Pointer
> policy now lives in `inputd` beside keyboard policy, and the kernel only
> carries raw pointer records. This reverses the "pointer path is not migrated"
> row in `input-plan.md` (Decisions), which the phases below update.

## Goal and scope

Plug a USB keyboard, mouse or tablet into LazyOS (first on QEMU's `qemu-xhci`)
and have it work exactly like the PS/2 devices, with **one pointer pipeline for
every device**: PS/2 mouse, USB mouse and USB tablet all produce raw bus
records, `inputd` turns them into one cursor and button state, and the
compositor receives that from `inputd`. Hot-unplug never leaves a key or a
button stuck.

**In:** pointer events on the raw bus; pointer policy in `inputd`; the PS/2
mouse re-fed through the bus; xHCI host controller, root-port enumeration, HID
boot-protocol keyboard and mouse, then report-protocol for absolute pointers
(`usb-tablet`); hot-plug and hot-unplug.
**Out (v1):** hubs, mass storage and every non-HID class, isochronous
transfers, USB 3 streams, suspend/resume, MSI/MSI-X, pointer acceleration
curves and per-device settings, touch and multi-touch, real-hardware BIOS
handoff (see Risks). The seams for all of these are left open.

## What exists, what is missing

| Piece | State today | Needed |
|---|---|---|
| Key vocabulary | The raw bus already speaks USB HID usages (page 0x07), so a USB keyboard needs **no translation table** | none |
| Raw bus producers | `bus::publish` is kernel-internal and hard-wired to the PS/2 tap (`device::PS2_KEYBOARD`); syscall 25 is consumer-only (`open`/`poll`/`close`) | a way for an **unprivileged driver** to publish with a kernel-stamped device id |
| Pointer state | PS/2 IRQ12 -> `kernel/src/input/mouse.rs` decodes packets, keeps the cursor position and buttons, clamps to the screen bounds (`set_bounds`), and pushes `PointerMove`/button/wheel records into the display grant's queue; `xuid` drains them with `display_input_poll` and seeds its cursor from the bind event | state, clamping and button tracking move to `inputd`; the kernel keeps only the IRQ and packet decode |
| Pointer on the bus | `RelMotion`/`AbsMotion`/`Button`/`Scroll` kinds are reserved but have no producer and no defined encoding | an encoding, a producer for PS/2, and `inputd` as the consumer |
| Compositor input | `xuid` gets keys through `inputd` (shell client) but pointer through the kernel | `xuid` receives pointer from `inputd` too; it still owns hit-testing, focus and routing to windows |
| PCI/DMA/IRQ for drivers | Done: syscall 23 `claim`/`map_bar`/`dma_alloc`/`irq_*`, contiguous DMA pool, INTx via the PIC, polling fallback (`sndd` and `netdrv` are the models) | verify the PCI class mapping covers xHCI (`0C0330`) |
| Test rig | `qemu_qmp.py` already types, clicks and moves; `--tablet` attaches `usb-tablet` | `-device qemu-xhci` in the harness |

## Key decisions

1. **Pointer policy lives in `inputd`; routing stays in `xuid`.** `inputd` owns
   what is device-independent and stateful: the cursor position and its
   clamping to the screen, absolute-to-pixel scaling, the held-button set
   across all devices, and wheel accumulation. `xuid` keeps what needs the
   window tree: hit-testing, which surface is under the cursor, focus on click,
   drags, and the existing `display.v1` `PointerMove/Down/Up/Wheel` events to
   clients (frozen, unchanged). `inputd` does not deliver pointer events to
   apps directly, because the right recipient depends on window geometry that
   only the compositor knows. Pointer content therefore goes only to the
   compositor, never to arbitrary clients, which is the keyboard model's
   "only the focused consumer" rule applied to a device with no focus of its
   own.
2. **A device-independent pointer encoding on the raw bus.** One compact record
   per hardware report, so a fast mouse costs one slot rather than several, and
   so records can be merged (decision 3). Fixed 24-byte records, as today:

   | `kind` | `code` | `value` | Notes |
   |---|---|---|---|
   | `REL_MOTION` | 0 | `dx` in the low `i16`, `dy` in the high `i16` | counts, not pixels |
   | `ABS_MOTION` | 0 | `x` in the low `u16`, `y` in the high `u16` | normalized `0..=0xFFFF`; the driver scales from the device's logical range, so `inputd` never sees device ranges |
   | `BUTTON` | HID button usage (page 0x09): 1 left, 2 right, 3 middle, 4 back, 5 forward | 0 release, 1 press | edges only; never merged |
   | `SCROLL` | 0 vertical, 1 horizontal | signed notches | |

   Deltas wider than `i16` are split across records by the producer. The kernel
   never decides what a motion means in pixels.
3. **Pointer floods must not evict key events.** The consumer ring is 256
   records and a `Dropped` marker makes `inputd` release every held key. A
   mouse is much chattier than a keyboard, so left alone a drag would cause
   spurious key releases. The bus therefore **merges a pointer record into the
   newest queued record** when device, kind and code match (add the deltas
   with saturation for `REL_MOTION` and `SCROLL`; replace for `ABS_MOTION`),
   and never merges across a different record, so button and key ordering is
   exact. Merging keeps global gapless `seq` (a merged record keeps the first
   `seq`; the absorbed events consume no `seq`). The ring behaviour, the
   `Dropped` marker and the key rules are otherwise unchanged. If testing
   shows tail merging is not enough, a separate pointer ring per consumer is the
   fallback; it is a bus-internal change that `inputd` does not see.
4. **Userspace driver, `usbd`.** Driver-plan D1 makes new drivers userspace by
   default: a crash is an `init` restart, and the surface is ordinary Messenger
   and ACL. The kernel needs no USB knowledge (D2).
5. **xHCI only.** It is the one controller QEMU `q35`/`pc` and every modern
   PC share, and it needs no companion controllers. UHCI/OHCI/EHCI are not
   worth a driver each for a hobby OS.
6. **A new, narrow source capability on the raw bus.** Add syscall 25 ops
   `register_source(class) -> source_id` and `publish(source_id, buf, n)`, gated by a new
   capability bit `input.source` (next free bit in `credentials.rs`; held
   only by `usbd`, stripped from everyone else exactly as `CAP_INPUT_RAW` is).
   - The **kernel assigns the source id** at `register_source`; it doubles as
     the device id stamped on every event of that source. `publish` and
     `close_source(source_id)` both name the source explicitly, and the kernel
     rejects a call whose caller does not own that id (`EBADF`; a stale id from a
     closed source fails closed). Because `usbd` is one task with many sources,
     the id is what selects the class check, the stamped device id and the
     held-key and held-button state for each batch. The kernel stamps it on
     every event, so a driver can never impersonate the PS/2 keyboard, the PS/2
     mouse or another device (input-plan security property 5).
   - `class` is a **source class**, not an event kind. Each class has a fixed
     set of permitted record kinds, enforced by the kernel:

     | Class | Permitted record kinds |
     |---|---|
     | `Keyboard` | `KEY` |
     | `Pointer` (relative) | `REL_MOTION`, `BUTTON`, `SCROLL` |
     | `Tablet` (absolute) | `ABS_MOTION`, `BUTTON`, `SCROLL` |

     `usbd` registers **one source per HID interface**, so a composite
     keyboard-plus-mouse device is two sources and each stays inside its own
     class. `publish` rejects (drops and counts) any record whose kind is not in
     the registering source's class, then validates the rest: key `code` in `0x04..=0xE7`; button `code` in `1..=5`; `value` in
     `{0,1}` for edges; scroll and motion magnitudes bounded; bounded batch
     size; per-task rate limit. Anything else is dropped and counted.
   - The kernel tracks held keys **and held buttons** per source; **closing a
     source (or the driver dying, via `teardown_task`) publishes a release for
     each**, so unplug and crash cannot stick a key or a button.
   - Alternative rejected: a Messenger interface from `usbd` to `inputd`. It
     would lose the global gapless `seq`, kernel timestamps and `Dropped`
     accounting that the bus gives for free.
7. **The PS/2 mouse is re-fed through the bus, not rewritten.** IRQ12 and the
   packet decoder (`decode_packet`, wheel negotiation) stay in the kernel, since
   the i8042 is boot-critical and the PS/2 keyboard does the same. After decode
   it publishes `REL_MOTION`, `BUTTON` and `SCROLL` with a new
   `device::PS2_MOUSE` id, alongside the legacy display stream (exactly how I0
   did it for keys) until the legacy path is removed.
8. **Boot protocol first.** `SET_PROTOCOL(boot)` gives fixed 8-byte keyboard
   and 3/4-byte mouse reports, so v1 needs no report-descriptor parser. A small
   parser arrives in U4, only because `usb-tablet` (absolute pointer) has no
   boot protocol.
9. **Devices are hostile input.** Descriptors and reports come from hardware
   (or a malicious stick) and are parsed by pure `no_std` libraries with
   seeded fuzz tests, per the project's code standards. Every length is checked
   before use, every loop is bounded, and a misbehaving device is disabled
   rather than trusted. `inputd` treats pointer records the same way: unknown
   codes are dropped and counted, and the cursor is always clamped.

## Architecture

```
 usb-kbd / usb-mouse / usb-tablet            (QEMU: -device qemu-xhci -device usb-kbd ...)
        |  xHCI root ports
 usbd   (ring 3, uid _usb, CAP_DEV_CLAIM + input.source, supervised by init)
        |  libs/xhci   : registers, TRBs, rings, slot/endpoint contexts
        |  libs/usbhid : descriptors, boot + report decoders, report diff
        |  syscall 25 publish: KEY / REL_MOTION / ABS_MOTION / BUTTON / SCROLL
        v
 kernel raw bus   <-- PS/2 keyboard tap, PS/2 mouse tap (IRQ12 decode, then publish)
        |  gapless seq, kernel timestamps, device ids, tail-merged pointer records
        v
 inputd (CAP_INPUT_RAW)
   keyboard: keymap, modifiers, repeat, hotkeys, focus routing      [existing]
   pointer : libs/inputmap::pointer  cursor, clamp, abs scaling,
             held buttons, wheel                                    [new]
        |  keys   -> the focused client's own endpoint
        |  pointer -> the shell endpoint only (os.lazy.input.shell.v1 PointerEvent)
        v
 xuid (compositor): hit-test, focus-on-click, drag, software cursor,
        display.v1 PointerMove/Down/Up/Wheel to clients            [unchanged]
```

`usbd` serves no Messenger interface in v1 beyond what `init`'s health checks
need; a read-only `os.lazy.usb.v1` (list devices, for `usbctl`) is added in U5
and, per the repo rule, **defined in `idl/usb.midl` and generated with
`midlc`**, never hand-written. The same rule covers the pointer additions to
`idl/input.midl` below.

### `inputd` pointer module

Pure logic in a new `libs/inputmap/src/pointer.rs` (host-tested like the
keyboard engine; no Messenger, no clock), so the state machine is deterministic:

- **State:** `x`, `y` (pixels, clamped to `0..width`, `0..height`), the held
  button set as a bitmask with a per-button press count so two mice holding the
  same button do not release it early, and bounds.
- **Input:** one raw pointer record. `REL_MOTION` adds counts (1:1 with today's
  kernel behaviour; acceleration is a later `confd` setting); `ABS_MOTION`
  scales `0..=0xFFFF` to the bounds; `BUTTON` sets or clears; `SCROLL`
  accumulates notches. A `Dropped` marker leaves the position alone (the
  cursor is state, not an edge stream) but **releases every held button**,
  emitting a `PointerEvent` with the button mask cleared. `Dropped` carries no
  device or button snapshot, and button records are edges only, so a lost
  release could otherwise stick a button forever. This mirrors what the
  keyboard engine does (release every held key); the cost is that a button
  physically still held during an overflow reads as released until it is
  pressed again, which fails safe.
- **Output:** `PointerOut { x, y, buttons, wheel_v, wheel_h, ts_ns, seq }` after
  each applied record, with motion coalesced per drain so a burst yields one
  move.
- **Glue** in a new `user/src/bin/inputd/pointer.rs`. `hub.rs` is already 466
  lines, so it only gains a call into this module (the 500-line rule).

### Interface additions (`idl/input.midl`, generated with `midlc`)

On `os.lazy.input.shell.v1`, compositor only, accepted from the display grant
holder exactly like the existing shell calls:

- `method SetBounds(width: U32, height: U32) -> () = <next id>`: the screen size
  the cursor is clamped to. It replaces the kernel's `mouse::set_bounds` hook.
  `inputd` also re-clamps the cursor on every change.
- `method GetPointer() -> (x: I32, y: I32, buttons: U32) = <next id>`: seeds the
  compositor's cursor on `Attach`, replacing the synthetic `PointerMove` the
  kernel pushes at display bind.
- Event `PointerEvent(x: I32, y: I32, buttons: U32, wheel: I32, wheel_h: I32,
  ts_ns: U64, seq: U64)`, `oneway`: absolute screen coordinates, the button
  bitmask in force after this event (1 left, 2 right, 4 middle, 8 back,
  16 forward), and wheel notches since the last event. `xuid` diffs `buttons`
  to find the edge.

Method ids follow the existing numbering in `idl/input.midl`; the exact values
are chosen when the file is edited. The client-facing `os.lazy.input.v1` does
**not** gain pointer events in this plan.

## Phases

Each is independently shippable. Every kernel-facing phase ships correctness
**and** stress tests (AGENTS.md testing requirement) and must pass
`python tools/test/run.py --accel none`. **P0-P2 move the existing PS/2 mouse
onto `inputd` and need no USB at all**, so they can land first and de-risk the
pipeline before `usbd` exists.

| Phase | Deliverable | Tests |
|---|---|---|
| **P0** Bus pointer records | Encoding from decision 2 in `kernel/src/input/bus.rs`, `device::PS2_MOUSE`, tail merging for `REL_MOTION`/`SCROLL`/`ABS_MOTION`, and the PS/2 mouse tap publishing after `decode_packet`. The legacy display stream is unchanged and fed in parallel. | New `input_bus_suite` cases: encode/decode round-trips including negative and saturated deltas, merging only at the tail and never across a key or button record, `seq` stays gapless across merges, no key loss or spurious `Dropped` under a motion flood. Stress: millions of mixed key and pointer records across producers |
| **P1** `inputd` pointer | `libs/inputmap/src/pointer.rs`, `inputd/pointer.rs`, `source.rs` consuming the pointer kinds, `idl/input.midl` additions (`SetBounds`, `GetPointer`, `PointerEvent`), `libs/generated` regenerated with `midlc`, and `rhai-lazy`'s schema regenerated (`tools/midlc/midlc.py --schema ...`). | `cargo test -p inputmap -p messenger-generated`: clamping at all four edges, absolute scaling, two-device button counting, `Dropped` releasing every held button, wheel accumulation, malformed records dropped and counted. A seeded fuzz entry for the pointer state machine under `fuzz/` with checked-in seeds (`fuzz/gen_corpus.py --check` stays green) |
| **P2** `xuid` switches over | `xuid` calls `SetBounds` and `GetPointer` on attach and takes pointer from `PointerEvent`; it stops draining pointer records from `display_input_poll` once `inputd` is attached, and falls back to the kernel path only when no `inputd` is running. Hit-testing, drags, focus-on-click and `display.v1` events are untouched. | The existing `display_suite` passes unchanged; desktop screenshot sessions (move, click, drag a window, wheel in the Docs app, `xui_docs.json`) before and after must match; a latency check that cursor motion is not slower than the old direct path (one extra Messenger hop) |
| **U0** Libraries | `libs/xhci` (register/TRB/context layouts, command and event ring state machines behind an MMIO/DMA trait so they run on the host) and `libs/usbhid` (device/config/interface/endpoint descriptor parser, boot keyboard and mouse decoders, previous-report diff to press/release edges, ignoring the `0x01` rollover-error report). Added to the workspace. | `cargo test -p xhci -p usbhid`: golden descriptors from QEMU devices, ring wrap-around, cycle-bit handling. Seeded fuzz for `usbdesc` and `hidreport` with checked-in seeds |
| **U1** Kernel source op | Syscall 25 `register_source(class)`/`publish(source_id, ...)`/`close_source(source_id)`, the `input.source` capability, kernel-stamped device ids, release of held keys **and buttons** on close, validation of every record against its source class. `init` grants the bit to `usbd` only. | New `input_bus_suite` cases: capability gate, forged or out-of-range codes dropped and counted, per-source release on close and on task death, two sources interleaved. Stress: repeated register/close generations, many producers |
| **U2** `usbd` keyboard | Claim the xHCI function (class `0C0330`), reset and start the controller, scratchpad, DCBAA, command and event rings (polled, as in `sndd`), detect connected root ports, reset, `Enable Slot`, `Address Device`, read descriptors, `Configure Endpoint`, `SET_PROTOCOL(boot)`, `SET_IDLE(0)`, interrupt-IN polling, publish key edges. Boot markers `USBD:XHCI`, `USBD:PORT`, `USBD:HID:KBD`. | `tools/usb/run.py` (below): QEMU with `qemu-xhci` + `usb-kbd` and **no PS/2 input**, type text over QMP, verify the serial trace `tools/input/verify_trace.py` checks today, plus a Terminal screenshot. Kernel `dev_suite` gains an xHCI class-mapping check |
| **U3** USB mouse and hot-plug | Boot mouse decoding (3 or 4 byte, wheel, buttons) published as `REL_MOTION`/`BUTTON`/`SCROLL`; port-status-change events; `device_add`/`device_del` at runtime; detach releases keys and buttons; slot and endpoint teardown frees DMA only after the controller has stopped using it (the `sndd` DMA-lifetime lesson in `architecture/audio.md`). `usbd` contains **no pointer policy**: no cursor, no clamping. | QMP `device_add usb-kbd`/`usb-mouse` and `device_del` mid-session: type, unplug while a key or button is held (nothing stuck), replug; mouse move/click/scroll screenshot with the PS/2 mouse also present (two devices, one cursor). Stress: 200 plug/unplug cycles with no DMA or slot leak (`DmaMemory` returns to baseline) |
| **U4** Report protocol, tablet | Minimal HID report-descriptor parser (Input items; Generic Desktop X/Y/Wheel, Button, Keyboard pages; logical min/max), `usb-tablet` published as `ABS_MOTION` normalized to `0..=0xFFFF`. This is what makes `qemu_session.py --tablet` / `mouse_abs` position the guest cursor, and the cursor lands in the same `inputd` pointer module as every other device. | Descriptor parser unit and fuzz tests; screenshot session using `mouse_abs` to click a known desktop target at several screen sizes |
| **U5** Supervision, docs, CI | `init` manifest entry (`_usb`, next free uid after `_snd` 901, only `CAP_DEV_CLAIM` + `input.source`), ACL policy rule for the `os.kernel.dev.usb` class, restart test (kill `usbd`, keys and buttons released, device re-enumerated), optional `usbctl` plus `idl/usb.midl`, `docs/architecture/usb.md`, edits to `input-plan.md` (pointer migrated; I5 note) and `driver-plan.md` (non-goals), `.github/workflows/usb-hid.yml`. | `tools/usb/run.py --services`, `--machine q35`, `--virtio-disk` variants like `tools/sound/run.py` and `tools/net/run.py` |

Once P2 has shipped and the kernel stream is no longer the pointer source, the
kernel's cursor state (`MouseState`, `set_bounds`, the bind-time seed in
`display.rs`) and the pointer events in `display_input_poll` are removed with
the rest of the legacy path in input-plan I5. Until then both paths coexist,
and `xuid` prefers `inputd`.

Interrupts: poll the event ring first (CI-safe, no PIC routing question), then
arm the INTx line when the PCI interrupt pin is routable and keep polling as
the fallback, exactly as `sndd` does. MSI is out of scope.

## Verification harness (`tools/usb/`)

Modelled on `tools/sound/run.py` and `tools/net/run.py`; the verdict is what
the guest *did*, not that a marker printed:

- `run.py` builds, boots headless with `-device qemu-xhci` and the requested
  `usb-kbd`/`usb-mouse`/`usb-tablet`, drives it through `qemu_qmp.py`
  (`type_text`, `mouse_move`, `mouse_abs`, `device_add`, `device_del`), then
  judges the serial trace and the screenshots (`pngstats.py`; read the PNGs).
- To prove the USB path (and not a silent PS/2 fallback), a run mode disables
  the PS/2 taps in the guest (a `cfg(lazyos_tests)`-style switch or boot flag)
  and requires keystrokes and pointer movement to still arrive.
- The P-phases reuse the existing `qemu_session.py` desktop scripts, since they
  change no hardware, only who owns the cursor.
- `test_*.py` checks the judge itself fails when it should, like the other
  tools.

## Risks and open questions

1. **QEMU is forgiving; real xHCI is not.** Real controllers need the USB
   legacy-support handoff (xHCI extended capability), correct port power and
   reset timing, and a USB-legacy i8042 emulation that can double-deliver keys.
   v1 targets QEMU only; real hardware is a named follow-up, not a promise.
2. **An extra hop for the cursor.** Pointer now goes kernel -> `inputd` ->
   `xuid` instead of kernel -> `xuid`. `inputd` coalesces motion per drain and
   `xuid` already coalesces runs of moves, so the cost should be small, but P2
   measures it and the change is not merged if the cursor visibly lags. The
   rollback is the kernel fallback path, kept until I5.
3. **Single point of failure.** If `inputd` dies, keys already stop; now the
   pointer does too. `init` restarts it, and `xuid` re-`Attach`es and calls
   `GetPointer`; a restart test covers this in P2.
4. **Ring pressure from the pointer.** Tail merging (decision 3) is the first
   defence; a dedicated pointer ring is the planned escalation. The P0 flood
   test is what decides. Merging has one visible cost, seen in a loaded TCG
   guest: deltas summed before `inputd` clamps them, so motion that pushes past
   an edge and comes back *within one drain* lands short of where per-packet
   clamping would put it (`-2000` then `+30` from the corner stays at 0
   instead of 30). With `inputd` draining every 20 ms this needs a very fast
   flick; if it is ever noticeable, the producer can stop merging when a delta
   changes sign.
5. **Rate limit and DMA trust.** An xHCI driver holds a `DMA` right, so it is
   trusted like the kernel until an IOMMU exists (driver-plan D5). This plan
   adds no new exposure but should not be oversold as sandboxing.
6. **DMA pool pressure.** Rings and contexts are small (tens of KiB), well
   inside the 4 MiB-per-allocation and 16 MiB pool limits, but U3's churn test
   exists to prove nothing leaks.
7. **Keyboard LEDs and typematic.** `inputd` does not drive LEDs yet; USB
   `SET_REPORT` output is a trivial add once it does. Typematic is a non-issue
   because USB keyboards report state, not repeats.
8. **Composite and multi-interface devices.** v1 binds the first HID
   interface of a boot-capable device and ignores the rest (extra buttons on
   gaming mice, consumer-control keys). Report-protocol coverage grows from U4.
9. **Multiple pointing devices.** Relative devices simply sum into one cursor.
   A tablet and a mouse together is well-defined (the last report wins for
   position); per-device cursors and seats are out of scope.
10. **Naming.** `input.source` vs a per-class split is a judgement call to
    settle with the security-model owner before U1 lands.

## Suggested order of work

P0 to P2 are the PS/2 mouse moving onto `inputd`; they are useful by
themselves, change no hardware, and prove the pointer pipeline under the
existing desktop screenshot sessions. U0 and U1 are independent of them and
can proceed in parallel. U2 is the first user-visible USB milestone (a USB
keyboard typing into the desktop); U3 and U4 each add one device class and, by
then, only have to publish records the pipeline already understands; U5 makes
it production-shaped. A plausible first PR is P0 + P1, since both are testable
without touching a single USB register.
