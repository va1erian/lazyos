# Plan: basic USB HID support (keyboard, mouse, tablet)

> **Status: draft proposal (2026-10-01).** Builds on
> [input-plan.md](input-plan.md) (the raw event bus, `inputd`) and
> [driver-plan.md](driver-plan.md) (the device core, userspace drivers). It
> lists USB as a non-goal of the driver plan; this plan is the follow-up that
> lifts that for HID only.

## Goal and scope

Plug a USB keyboard, mouse or tablet into LazyOS (first on QEMU's `qemu-xhci`)
and have it work exactly like the PS/2 devices: keys reach `inputd` as
HID-coded events, the pointer moves, hot-unplug never leaves a key stuck.

**In:** xHCI host controller, root-port enumeration, HID boot-protocol
keyboard and mouse, then report-protocol for absolute pointers (`usb-tablet`),
hot-plug and hot-unplug.
**Out (v1):** hubs, mass storage and every non-HID class, isochronous
transfers, USB 3 streams, suspend/resume, MSI/MSI-X, real-hardware BIOS
handoff (see Risks). The seams for all of these are left open.

## What exists, what is missing

| Piece | State today | Needed |
|---|---|---|
| Key vocabulary | The raw bus already speaks USB HID usages (page 0x07), so a USB keyboard needs **no translation table** | none |
| Raw bus producers | `bus::publish` is kernel-internal and hard-wired to the PS/2 tap (`device::PS2_KEYBOARD`); syscall 25 is consumer-only (`open`/`poll`/`close`) | a way for an **unprivileged driver** to publish with a kernel-stamped device id |
| Pointer | PS/2 IRQ12 -> `kernel/src/input/mouse.rs` -> display grant. `RelMotion`/`AbsMotion`/`Button`/`Scroll` kinds are reserved on the bus but unused | a pointer entry point a USB driver can feed |
| PCI/DMA/IRQ for drivers | Done: syscall 23 `claim`/`map_bar`/`dma_alloc`/`irq_*`, contiguous DMA pool, INTx via the PIC, polling fallback (`sndd` and `netdrv` are the models) | verify the PCI class mapping covers xHCI (`0C0330`) |
| Test rig | `qemu_qmp.py` already types, clicks and moves; `--tablet` attaches `usb-tablet` | `-device qemu-xhci` in the harness |

## Key decisions

1. **Userspace driver, `usbd`.** Driver-plan D1 makes new drivers userspace by
   default: a crash is an `init` restart, and the surface is ordinary Messenger
   and ACL. The kernel needs no USB knowledge (D2). The cost is that the
   driver must be able to feed the input bus, covered by decision 3.
2. **xHCI only.** It is the one controller QEMU `q35`/`pc` and every modern
   PC share, and it needs no companion controllers. UHCI/OHCI/EHCI are not
   worth a driver each for a hobby OS.
3. **A new, narrow source capability on the raw bus.** Add syscall 25 ops
   `register_source(kind) -> device_id` and `publish(buf, n)`, gated by a new
   capability bit `input.source` (next free bit in `credentials.rs`; held
   only by `usbd`, stripped from everyone else exactly as `CAP_INPUT_RAW` is).
   - The **kernel assigns the device id** at `register_source` and stamps it on
     every event, so a driver can never impersonate the PS/2 keyboard or
     another device (input-plan security property 5).
   - `publish` validates every record: kind matches the registered source,
     key `code` in the HID range `0x04..=0xE7`, `value` in `{0,1}`, bounded
     batch size, per-task rate limit. Anything else is dropped and counted.
   - The kernel tracks held keys per source; **closing a source (or the driver
     dying, via `teardown_task`) publishes a release for each**, so unplug and
     crash cannot stick a key.
   - Alternative rejected: a Messenger interface from `usbd` to `inputd`. It
     would lose the global gapless `seq`, kernel timestamps and `Dropped`
     accounting that the bus gives for free.
4. **Boot protocol first.** `SET_PROTOCOL(boot)` gives fixed 8-byte keyboard
   and 3/4-byte mouse reports, so v1 needs no report-descriptor parser. A small
   parser arrives in U4, only because `usb-tablet` (absolute pointer) has no
   boot protocol.
5. **Pointer goes through the same source op.** `publish` accepts the reserved
   pointer kinds and `mouse.rs` grows an `inject_rel` / `inject_abs` entry that
   the syscall routes to, so USB and PS/2 pointers share one cursor and the
   display path is untouched. Migrating the pointer onto `inputd` stays
   deferred, as input-plan already decided.
6. **Devices are hostile input.** Descriptors and reports come from hardware
   (or a malicious stick) and are parsed by pure `no_std` libraries with
   seeded fuzz tests, per the project's code standards. Every length is checked
   before use, every loop is bounded, and a misbehaving device is disabled
   rather than trusted.

## Architecture

```
 usb-kbd / usb-mouse / usb-tablet            (QEMU: -device qemu-xhci -device usb-kbd ...)
        |  xHCI root ports
 usbd   (ring 3, uid _usb, CAP_DEV_CLAIM + input.source, supervised by init)
        |  claim xHCI PCI fn -> map_bar, dma_alloc, poll event ring (irq later)
        |  libs/xhci   : registers, TRBs, rings, slot/endpoint contexts
        |  libs/usbhid : descriptors, boot + report decoders, report diff
        |  syscall 25 publish(kind, code, value)   (device id stamped by the kernel)
 kernel raw bus  ->  inputd (keymap, repeat, focus)  ->  clients      [unchanged]
 kernel mouse.rs <-  pointer events                                   [new entry point]
```

`usbd` serves no Messenger interface in v1 beyond what `init`'s health checks
need; a read-only `os.lazy.usb.v1` (list devices, for `usbctl`) is added in U5
and, per the repo rule, **defined in `idl/usb.midl` and generated with
`midlc`**, never hand-written.

## Phases

Each is independently shippable. Every kernel-facing phase ships correctness
**and** stress tests (AGENTS.md testing requirement) and must pass
`python tools/test/run.py --accel none`.

| Phase | Deliverable | Tests |
|---|---|---|
| **U0** Libraries | `libs/xhci` (register/TRB/context layouts, command and event ring state machines behind an MMIO/DMA trait so they run on the host) and `libs/usbhid` (device/config/interface/endpoint descriptor parser, boot keyboard and mouse decoders, previous-report diff to press/release edges, ignoring the `0x01` rollover-error report). Added to the workspace. | `cargo test -p xhci -p usbhid`: golden descriptors from QEMU devices, wrap-around of rings, cycle-bit handling. Seeded fuzz in the style of `libs/fuzzkit` for `usbdesc` and `hidreport`, checked-in seeds under `fuzz/seeds/`, `fuzz/gen_corpus.py --check` stays green |
| **U1** Kernel source op | Syscall 25 `register_source`/`publish`/`close_source`, `input.source` capability, kernel-stamped device ids (`device::USB_BASE..`), release-on-close, pointer injection into `mouse.rs`. `init` grants the bit to `usbd` only. | New cases in `kernel/src/tests/input_bus_suite/`: capability gate, forged/out-of-range codes dropped and counted, per-source held-key release on close and on task death, ring and `Dropped` behaviour with two sources interleaved. Stress: millions of events from several producers with no loss lacking a `Dropped` marker, repeated register/close generations |
| **U2** `usbd` keyboard | Claim the xHCI function (class `0C0330`), reset and start the controller, scratchpad, DCBAA, command and event rings (polled, as in `sndd`), detect connected root ports, reset, `Enable Slot`, `Address Device`, read descriptors, `Configure Endpoint`, `SET_PROTOCOL(boot)`, `SET_IDLE(0)`, interrupt-IN polling, publish edges. Boot markers `USBD:XHCI`, `USBD:PORT`, `USBD:HID:KBD`. | `tools/usb/run.py` (below): QEMU with `qemu-xhci` + `usb-kbd` and **no PS/2 input**, type text over QMP, verify the same serial trace `tools/input/verify_trace.py` checks today, plus a Terminal screenshot. Kernel `dev_suite` gains an xHCI class-mapping check |
| **U3** Pointer and hot-plug | Boot mouse decoding, wheel, three buttons; port-status-change events; `device_add`/`device_del` at runtime; detach releases keys and the pointer buttons; slot and endpoint teardown frees DMA only after the controller has stopped using it (the `sndd` DMA-lifetime lesson in `architecture/audio.md`). | QMP `device_add usb-kbd` / `device_del` mid-session: type, unplug while a key is held (no stuck key), replug. Mouse move/click/scroll screenshot. Stress: 200 plug/unplug cycles, no DMA or slot leak (`DmaMemory` quota returns to baseline) |
| **U4** Report protocol, tablet | Minimal HID report-descriptor parser (Input items; Generic Desktop X/Y/Wheel, Button, Keyboard pages; logical min/max scaling), `usb-tablet` absolute events scaled to screen bounds. This is what makes `qemu_session.py --tablet` / `mouse_abs` actually position the guest cursor. | Descriptor parser unit and fuzz tests; screenshot session using `mouse_abs` to click a known desktop target |
| **U5** Supervision, docs, CI | `init` manifest entry (`_usb`, next free uid after `_snd` 901, only `CAP_DEV_CLAIM` + `input.source`), ACL policy rule for the `os.kernel.dev.usb` class, restart test (kill `usbd`, keys released, device re-enumerated), optional `usbctl` plus `idl/usb.midl`, `docs/architecture/usb.md`, edits to `input-plan.md` (I5 note) and `driver-plan.md` (non-goals), `.github/workflows/usb-hid.yml`. | `tools/usb/run.py --services`, `--machine q35`, `--virtio-disk` variants like `tools/sound/run.py` and `tools/net/run.py` |

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
  the PS/2 tap in the guest (a `cfg(lazyos_tests)`-style switch or boot flag)
  and requires the keystrokes to still arrive.
- `test_*.py` checks the judge itself fails when it should, like the other
  tools.

## Risks and open questions

1. **QEMU is forgiving; real xHCI is not.** Real controllers need the USB
   legacy-support handoff (xHCI extended capability), correct port power and
   reset timing, and a USB-legacy i8042 emulation that can double-deliver keys.
   v1 targets QEMU only; real hardware is a named follow-up, not a promise.
2. **Rate limit and DMA trust.** An xHCI driver holds a `DMA` right, so it is
   trusted like the kernel until an IOMMU exists (driver-plan D5). This plan
   adds no new exposure but should not be oversold as sandboxing.
3. **DMA pool pressure.** Rings and contexts are small (tens of KiB), well
   inside the 4 MiB-per-allocation and 16 MiB pool limits, but U3's churn test
   exists to prove nothing leaks.
4. **Keyboard LEDs and typematic.** `inputd` does not drive LEDs yet; USB
   `SET_REPORT` output is a trivial add once it does. Typematic is a non-issue
   because USB keyboards report state, not repeats.
5. **Composite and multi-interface devices.** v1 binds the first HID
   interface of a boot-capable device and ignores the rest (extra buttons on
   gaming mice, consumer-control keys). Report-protocol coverage grows from U4.
6. **Which uid and capability name** (`input.source` vs a per-class split) is a
   judgement call to settle with the security-model owner before U1 lands.

## Suggested order of work

U0 and U1 are independent and can proceed in parallel. U2 is the first
user-visible milestone (a USB keyboard typing into the desktop); U3 and U4 each
add one device class; U5 makes it production-shaped. A plausible first PR is
U0 + U1, since both are testable without touching a single USB register.
