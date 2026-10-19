# Plan: USB gamepads (HID gamepad and X-input) into the input pipeline

> **Status: draft proposal (2026-10-18); G0 and G1 are implemented** (the
> phase table marks what landed; G2-G7 are ahead). G0: `libs/usbhid` (the
> `report/` split, the gamepad layout and decoder, `xinput.rs`, golden
> descriptors, fuzz entry and seeds). G1: the kernel (`kind::AXIS`/
> `kind::DEVICE`, the `Gamepad` source class with 32 buttons, `bus_axis.rs`,
> and the extended source tests). G2: `usbd` binds a report-protocol pad
> (`has_pad_collection` probe, `usbhid::busrec` glue; host-tested only until G7).
> Builds on
> [usb-hid-plan.md](usb-hid-plan.md) (U0-U5: `libs/xhci`, `libs/usbhid`, `usbd`,
> the raw bus sources, the one cursor) and [input-plan.md](input-plan.md) (the
> kernel raw bus, `inputd`, sessions, focus, the key-state page). This plan adds
> a *second kind of device* to the same pipeline: a gamepad. Nothing here
> changes the keyboard or pointer rules, and no new kernel authority appears.
>
> Two facts were verified on the development box before writing this (see
> [Verification harness](#verification-harness-toolsusb)):
> QEMU 10.2.50 speaks the plain usbredir protocol over a socket
> (`usb-redir` + `-chardev socket`), and a minimal server that advertises the
> right capabilities gets a **fake device attached to the emulated xHCI bus**
> (`info usb`: `Device 0.1, Port 1, Speed 12 Mb/s`), with the guest's USB
> stack issuing the standard device-descriptor control transfer — so a
> scripted fake gamepad is possible in CI. Separately, `usb-host` passes a
> real 8BitDo Ultimate 2 (VID `2dc8`, PID `310b`) through to the emulated xHCI
> bus on Windows, so the pipeline can be exercised against real hardware in
> QEMU too.

## Goal and scope

Plug a USB gamepad into LazyOS and have it work in games and any focused
window: the driver turns reports into bus records, `inputd` owns the
device-independent state, and the focused session reads it as events plus a
shared state page — the same shape the keyboard and the pointer already have.

**In (v1):** report-protocol HID gamepads (Generic Desktop "Game Pad" /
"Joystick" / "Multi-axis Controller" collections: X, Y, Z, Rx, Ry, Rz,
Slider, Dial, Hat switch, up to 32 buttons); the Xbox 360-style **X-input**
vendor interface (class `FF`/`5D`/`01`, the fixed 20-byte report); hot-plug and
hot-unplug with no stuck button; a per-device state page and events for the
focused session; one game (Doom) as the first consumer; a CI harness that boots
QEMU against a scripted USB gamepad and judges what reached `inputd` and the
game.

**Out (v1):** rumble/force feedback and LED output (interrupt OUT reports),
Xbox One / Series GIP (the newer protocol needs a handshake and 64-byte
packets), wireless receivers as devices of their own, touchpads, gyroscopes,
motion, per-app remapping UI, SDL-style per-device mapping tables, multiple
players as a first-class concept, and Bluetooth HID. The seams for all of
these are left open, and the plan says where.

## What exists, what is missing

| Piece | State today | Needed |
|---|---|---|
| xHCI, enumeration, hot-plug | Done (`usbd`, U2-U5; hubs, full speed, two controllers) | none for gamepads |
| HID report descriptors | `libs/usbhid::report` parses a *pointer* layout (X, Y, wheel, ≤8 buttons) | axes (Z/Rx/Ry/Rz/Slider/Dial/Hat), buttons 1..=32, the gamepad application collection |
| HID decoding | `boot.rs` (keyboard, mouse) diffs reports to edges; `report::Decoder` does the same for a pointer | a gamepad decoder: changed axes out, button edges out, hat normalized |
| Bus vocabulary | `KEY`, `REL_MOTION`, `ABS_MOTION`, `BUTTON` (1..=5), `SCROLL`, `SYNC`, `DROPPED` | an `AXIS` kind; `BUTTON` up to usage 32 for gamepads |
| Source classes | `Keyboard`, `Pointer`, `Tablet` (`kernel/src/input/sources.rs`) | a `Gamepad` class with its own permitted kinds and 32-button release tracking |
| `inputd` | key engine, cursor (`inputmap::Pointer`), focus routing, delivery, key-state page | a gamepad engine (`inputmap::Gamepad`), delivery, a pad-state page |
| Client API | `os.lazy.input.v1`: `KeyEvent`, `TextInput`, `AttachKeyState`, ... | gamepad events and `AttachGamepadState` |
| Client library | `xui_app::input` (`Session`, `KeyStatePage`), games read keys | decode the gamepad events, a `GamepadStatePage`, a standard mapping helper |
| Consumer | Doom, Quake, SRB2Kart read keys (SRB2Kart's `I_NumJoys` returns 0) | one game driven by a pad |
| Test device | QEMU has `usb-kbd`, `usb-mouse`, `usb-tablet` — **no gamepad** | a scripted device (`usb-redir`) for CI, `usb-host` for a real pad |

## Key decisions

1. **Two wire formats, one vocabulary.** A standard HID gamepad is parsed from
   its report descriptor; an X-input device has a fixed 20-byte report and no
   descriptor to parse. Both are translated **in `usbd`** to the same
   canonical HID usages, so everything above the driver sees one device. This
   is the tablet's rule ("the driver normalizes `0..=0xFFFF`") applied to a
   richer device. X-input is in v1, not a footnote: it is the only thing the
   test 8BitDo speaks over its USB cable, and it is what Xbox 360 pads speak.
2. **The bus gains one record kind, `AXIS`; buttons reuse `BUTTON`.**

   | `kind` | `code` | `value` | Notes |
   |---|---|---|---|
   | `AXIS` (next free, 8) | Generic Desktop usage: `0x30` X, `0x31` Y, `0x32` Z, `0x33` Rx, `0x34` Ry, `0x35` Rz, `0x36` Slider, `0x37` Dial, `0x39` Hat | `-32768..=32767`, **0 is the axis's rest**: a HID axis is scaled bipolar over its logical range whatever its sign (most pads declare `0..=255` sticks centred mid-range, and HID has no "trigger" usage), an X-input trigger is `0..=32767`; Y and Ry are positive *down* (HID's direction; X-input's up-positive sticks are negated); hat `0..=7` clockwise from up, `8` centred | absolute state; only changed axes are published; tail-merged with *replace* |
   | `BUTTON` (4) | HID button usage; `1..=32` for a gamepad source | `0` release, `1` press | edges only; never merged |
   | `DEVICE` (next free, 9) | vendor id | `product | version << 16` | published once by the driver after `register_source`; diagnostics and the deferred mapping table; **never** a classification signal (every class may publish it) |
   | `SOURCE` (G3, next free, 10) | the source's class | `1` registered, `0` closed | **kernel-originated**: published from `release()` itself after its release burst (so `close`, task death and `register` reclaiming a dead owner's slot all announce it) and by `register`; refused from every driver. The ring evicts its oldest records, so a `SOURCE` can be lost like any other: it is the fast path, the source table query below is the truth |

   `AXIS` merges like `ABS_MOTION` (replace, same device/kind/code), so
   repeated updates of one axis behind a starved `inputd` cost one slot, not
   one per report. Buttons are edges and keep their order, exactly as keys do.
   Because every axis rests at 0 (the hat at 8), the kernel and both decoders
   can return an unplugged pad's axes to rest without knowing the device. The
   one casualty is a HID *analog trigger*: it reads `-32767` released and is
   "rested" at half travel; only the mapping table knows it is a trigger.
3. **A new source class, `Gamepad` (kernel).** Permitted kinds: `AXIS` (known
   usages only, value in `i16` range) and `BUTTON` (`1..=32`, edge). A gamepad
   source cannot publish a `KEY`, a pointer record or a `SCROLL`: the class
   gate is what stops a compromised gamepad driver from typing. Held-button
   tracking widens from `u8` to `u32`; `close` (and task death) publishes a
   release for every held button, so unplug and crash cannot stick one. Axes
   are state rather than edges, but the kernel also remembers which of them
   were last away from their rest value and returns them there on close and on
   task death, so a crashed driver cannot leave a stick deflected either.
   No new syscall: `register_source` takes the new class value, `publish`
   validates against it.
4. **Delivery: the focused session only, through `os.lazy.input.v1`.** A
   gamepad has no focus of its own, and the keyboard's rule ("only the focused
   consumer receives content") is the right one for it too. The compositor
   needs nothing: focus is already per surface. A new interface is not needed
   and would duplicate the session, grant and focus machinery.
5. **Edges as events, state as a page** (the I3 pattern). `GamepadAxis` and
   `GamepadButton` events carry changes; `AttachGamepadState` gives the
   session a shared page with every connected pad's axes and buttons, written
   while the session has focus and cleared before `GamepadLeave`, so a game
   polls it each frame with no Messenger round trip. A client without a page
   still gets edges.
6. **`inputd` owns the device-independent state, the client owns the
   mapping.** `inputd` applies the deadzone, converts the hat to d-pad
   directions, keeps one slot per device (up to 4) and resyncs on `Dropped`
   (release every button, centre every axis: fails safe). It gives a
   device's `BUTTON` records to exactly one engine, so a pad's A/B/X/Y
   (usages 1..=5) do not also click the cursor — and it **cannot** decide
   which from the records themselves: `DEVICE` is accepted from every class,
   a pad's first record is not guaranteed to reach `inputd` (a `Dropped`
   gap), and a device id is the source *slot's*, so a mouse that registers
   after a pad was unplugged gets the pad's old id. G3 therefore adds the
   kernel-originated `SOURCE` record (decision 2) and a `rawsys` query that
   returns the live source table (`device`, `class`, `generation` per slot;
   `CAP_INPUT_RAW` only). `inputd` reads the table when it opens the bus and
   again on every `Dropped` (a lost `SOURCE` is only ever inside such a gap),
   routes by the class, and frees a pad's slot when its source closes or its
   generation changes. The client library offers
   `xui_app::gamepad::Standard`, a typed view of the canonical usages
   (left/right stick, triggers, d-pad, 11 named buttons). A per-device mapping
   table (SDL's GameController DB approach) and a remap UI come later; the
   `DEVICE` record is what they will key on.
7. **Devices are hostile.** Every parse is bounded and fuzzed (as
   `libs/usbhid` already is), unknown usages and out-of-range values are
   dropped and counted, a descriptor the parser cannot place leaves the
   interface unbound (logged, not trusted), and `inputd` re-validates what the
   bus delivers.
8. **Verify against real hardware, not only QEMU's device models.** The
   harness has three modes: a **scripted fake device** over QEMU `usb-redir`
   (CI, deterministic), **`usb-host` passthrough** of a real pad on the
   development box (interactive, real descriptors and real reports), and the
   **NUC stick** (real xHCI, `dbgd` reads the log). Host tests cover the
   parsers with golden descriptors from real pads.

## Architecture

```
 usb gamepad (HID report protocol)      X-input pad (FF/5D/01, 20-byte report)
        |  xHCI root port / hub                |
        v                                      v
 usbd  (ring 3, uid _usb, CAP_DEV_CLAIM + CAP_INPUT_SOURCE)
        |  libs/usbhid : descriptors, report-layout walk (pointer | gamepad),
        |                report diff -> AXIS / BUTTON / DEVICE
        |  syscall 25 publish:  one source per interface, class Gamepad
        v
 kernel raw bus  (AXIS replaces at the tail; BUTTON edges never merge)
        |
        v
 inputd (CAP_INPUT_RAW)
    gamepad : inputmap::Gamepad   per-device state, deadzone, hat -> d-pad,
              coalescing, Dropped -> release + centre          [new]
        |  events -> the focused session's endpoint only
        |  state  -> the session's pad page (AttachGamepadState)
        v
 xui app (xui_app::input): events + GamepadStatePage + Standard mapping
        |
        v
 Doom (lazydoom::pad): the pad posts the same Doom keys the keyboard does
```

### `libs/usbhid`

- `report.rs` grew a **layout walk for gamepads** and split before it hit the
  500-line rule: `report/mod.rs` (the item walk — now tracking open
  collections — `Globals`, `Locals`, `Field`), `report/pointer.rs` (today's
  `Pointer`), `report/gamepad.rs` (the `Gamepad` layout and its `Decoder`).
  `MAX_BUTTONS` is per layout (8 for a pointer, 32 for a gamepad).
- The gamepad layout is found the way the pointer one is: the walk records
  the first Input item whose usage is X **inside a Game Pad / Joystick /
  Multi-axis Controller application collection**, then that report's other
  axes and buttons. A relative axis is not placed (the bus's `AXIS` is
  absolute); the "skip what cannot be placed" rules are unchanged. One
  inherited wart was fixed on the way: a layout carries a report-id byte only
  if a Report ID item appeared *before* its report, so a descriptor whose
  first report has none and a later one has one no longer reads the pad's
  reports with a spurious id byte (the same fix applies to the pointer).
- `xinput.rs` (new): the fixed 20-byte X-input report — byte 0 the message
  type (only `0x00` is input; the endpoint also carries LED and rumble status
  messages), byte 1 its length, then buttons, LT, RT, LX, LY, RX, RY —
  decoded to the canonical usages: d-pad bits to the hat axis, A/B/X/Y to
  buttons 1..=4, LB/RB 5/6, Back/Start 7/8, L3/R3 9/10, Guide 11, triggers
  to Z/Rz (`0..=32767`), and LY/RY negated so a stick pushed up reads
  negative Y as on a HID pad.
- Both decoders emit one `Axis` per *changed* axis (the first report emits
  every axis it holds) and one `Button` edge per changed button, and return
  every axis to its rest value on `release_all`, so an unplugged pad cannot
  leave a stick deflected.
- Host tests: golden descriptors inline (as today) for a generic 2-stick
  12-button pad, a four-way-hat pad, a pad behind a report id, and golden
  X-input reports; seeded fuzz for the walk and both decoders.

### Kernel (`kernel/src/input/`)

- `bus.rs`: `kind::AXIS`, `kind::DEVICE`; `bus_pointer.rs`'s merge table
  learns `AXIS` (replace).
- `sources.rs`: `class::GAMEPAD`; `permitted` gains the gamepad arm; held
  buttons widen to `u32` and `release` loops `1..=32`.
- `rawsys.rs`: no change (class value only).
- G3 (landed): `bus.rs` `kind::SOURCE` (10); `sources.rs` announces from
  `register` and from `release()` (after its release burst), refuses the kind
  from every class, and offers `snapshot()`; `rawsys.rs` op `SOURCES` (10,
  `CAP_INPUT_RAW`, four bytes per live source: device, class, generation
  `u16` LE, whole entries only). Tests: `kernel/src/tests/input_bus_suite/source_records.rs`,
  and the stress tests check the announcements against the table.
- Tests (`input_bus_suite`): class gate (a gamepad source publishing `KEY`,
  `REL_MOTION` or a bad usage is dropped and counted), range checks, `DEVICE`
  acceptance, release-on-close for all 32 buttons, `AXIS` tail-merge
  (replace; the pointer's turn rule never applies to an absolute axis), and
  the million-record `many_producers` stress, which now carries two gamepad
  sources among eight, asserts nothing is left held when every source closes,
  and fails if the consumer ring dropped anything.

### `inputd` and `libs/inputmap`

- `libs/inputmap/src/gamepad.rs`: `Gamepad` (state per device slot,
  `MAX_PADS = 4`), `PadOut { device, axes, buttons, hat, ts_ns, seq }`,
  `apply(RawRecord)`, `flush`, `resync`, `attach`/`detach` by device id.
  Deadzone (`DEADZONE = 0.15` of full scale, rescaled) applies around 0 on
  every axis (for an X-input trigger that is the release travel); the hat becomes `dpad`
  (`-1/0/1` per axis, diagonals set two).
- `user/src/bin/inputd/gamepad.rs` (new): answers `AttachGamepadState`,
  encodes the events, delivers to the focused session. `hub.rs` (471 lines)
  only gains calls, per the 500-line rule; `source.rs` keeps `AXIS`/`DEVICE`
  records and turns them into `Item`s; `trace.rs` gains `INPUTD:PAD` lines.
- The page (`inputmap::gamepad::SharedPads`, mirroring `keystate`): a
  seqlock word, the newest `seq`, a focused flag, a pad count and 4 slots of
  (device, buttons, hat, 8 axes) — a little over a hundred bytes with `i16`
  axes, well inside one 4 KiB page.

### Interfaces (`idl/input.midl`, generated with `midlc`)

On `os.lazy.input.v1` (client side; ids chosen when the file is edited, next
free after 7 / 15):

- `method AttachGamepadState(session: U64 = 1, state: Buffer = 2) -> () = 8`
  — same contract as `AttachKeyState`: while the session has focus `inputd`
  writes the pads' current state into it and clears it before `GamepadLeave`.
- Event `GamepadAxis(device: U32 = 1, code: U32 = 2, value: I32 = 3,
  ts_ns: U64 = 4, seq: U64 = 5) = 16 oneway` — a changed axis (coalesced per
  drain).
- Event `GamepadButton(device: U32 = 1, code: U32 = 2, pressed: Bool = 3,
  ts_ns: U64 = 4, seq: U64 = 5) = 17 oneway` — one edge per changed button.
- Event `GamepadEnter() = 18 oneway` / `GamepadLeave() = 19 oneway` — focus
  arrived (read the page to seed) / left (release every button, centre the
  axes).

`os.lazy.input.shell.v1` gains nothing: focus, surfaces and grants are
already there. `libs/generated` and the `rhai-lazy` schema are regenerated
(`tools/midlc/midlc.py --schema ... --rhai-api ...`).

### Client (`xui-app`)

- `xui-app/src/input.rs`: decode the three events; `Session::attach_gamepad_state()`
  returns a `GamepadStatePage` (seqlock reader like `KeyStatePage`), plus
  `Event::GamepadAxis/GamepadButton/GamepadEnter/GamepadLeave`.
- `xui-app/src/gamepad.rs` (new, portable, host-tested): `Standard` — a
  snapshot type (`left`, `right` in `-32768..=32767`, `trigger_l`,
  `trigger_r` in `0..=32767`, `dpad`, `buttons` as named bits) built from the
  canonical usages — its triggers are X-input's Z/Rz until the mapping table
  says which HID axes are triggers (risk 7; a HID trigger is bipolar on the
  bus and `Standard` would rescale it), and a small `poll(&GamepadStatePage)` helper. Games keep
  using `xui_app::input` directly (Doom already does), so no upstream `xui`
  change is needed.
- Not an app: no `tools/xui/new_app.py`, no core package, no launcher or GUI
  control is required by this plan (AGENTS.md's new-app rule applies only if
  the optional Settings page below is built).

## Phases

Each is independently shippable; kernel-facing phases ship correctness **and**
stress tests and must pass `python tools/test/run.py --accel none`.

| Phase | Deliverable | Tests |
|---|---|---|
| **G0** Libraries **(landed)** | `libs/usbhid`: `report/` split, gamepad layout, `Gamepad` decoder, `xinput.rs`; `MAX_BUTTONS` per layout | `cargo test -p usbhid`: golden descriptors (generic pad, four-way hat, report-id pad, X-input reports), hat and range normalization, change-only output, `release_all`; seeded fuzz + `python fuzz/gen_corpus.py --check` |
| **G1** Kernel **(landed)** | `kind::AXIS`/`kind::DEVICE`, the `Gamepad` source class, `u32` button tracking, axis rest tracking, `bus_axis.rs`, merge rules | `input_bus_suite`: class gate, ranges, `DEVICE` from every class, release-on-close (32 buttons and deflected axes), merge/replace, the million-record stress with two gamepads |
| **G2** `usbd` HID gamepad **(landed)** | Second-pass binding accepts a gamepad layout (trying `parse_gamepad` first, and never falling back to the pointer layout for an interface that declares a pad collection — the library gains a `has_pad_collection` probe, or a dedicated error, so the driver can tell "not a pad" from "a pad it cannot place"); `Hid` gains the gamepad decoder; publishes `AXIS`/`BUTTON`/`DEVICE`; detach releases buttons and centres axes; marker `USBD:HID:PAD vendor= product= interface= dci=` (an unplaceable pad: `USBD:HID:PAD:UNPLACED`, left unbound). As built: a `has_pad_collection` probe, not an error variant (`parse_gamepad`'s contract is unchanged); risk 6 is unchanged (the second pass still binds the first interface that yields a function) | Host-tested glue in `libs/usbhid`; the G7 harness drives it end to end; `tools/usb/judge.py --gamepad` |
| **G3** `inputd` (kernel half **landed**) | Kernel: `kind::SOURCE` (published by `register` and `release()`; refused from drivers) and the source-table query. `inputmap::Gamepad`, `inputd/gamepad.rs`, the page, the IDL additions, regenerated stubs and rhai schema, delivery to the focused session, routing by `SOURCE` class so a pad's buttons never reach the pointer engine and a slot reused by a mouse is a mouse again | `input_bus_suite`: `SOURCE` on register/close/teardown/dead-slot reclaim, after the releases, never from a driver, the table query (and its capability gate), existing tests updated for the extra records; `cargo test -p inputmap -p messenger-generated`: deadzone, hat→d-pad, two pads, `Dropped` resync (buttons released, axes centred), page seqlock, malformed records dropped, a pad's buttons leave the cursor alone, a pad's slot reused by a pointer, and the same with its `SOURCE` records lost in a `Dropped` gap (routing recovered from the table); a seeded fuzz entry |
| **G4** Client library | `xui_app::input` events + `GamepadStatePage`, `xui_app::gamepad::Standard` | `cargo test --manifest-path xui-app/Cargo.toml -p xui-app` (Linux/WSL, the crate's lib tests are Linux-only) |
| **G5** Doom | `lazydoom::pad`: the page read each frame, canonical state to Doom keys (left stick move/strafe, right stick turn, d-pad menu, A fire, B use, Y map, Start menu), markers `DOOM:PAD:*`; session `tools/screenshot/examples/doom_pad.json` | The session script drives the Doom menu with the fake pad (G7) and judges markers + screenshots |
| **G6** X-input | `usbd` binds `FF/5D/01`, decodes the fixed report, same records; marker `USBD:HID:XINPUT`; unplug releases | `libs/usbhid` golden reports; the fake X-input device in the harness; the real 8BitDo over `usb-host` |
| **G7** Harness, docs, CI | `tools/usb/fakepad.py` (usbredir server), `gamepad_run.py`, `judge.py --gamepad`, `capture.py`, `test_judge.py`, `tools/usb/README.md`, `docs/architecture/usb.md`, `.github/workflows/usb.yml` job | The harness itself; `test_judge.py` fails when it should; the full `python tools/usb/gamepad_run.py` pass |

Later, each independent: SRB2Kart's joystick seam (`I_NumJoys`,
`I_GetJoystickEvents`, `joyxmove`/`joybuttons`, up to 4 pads — its README
already names gamepads as missing), Quake, Settings → Controllers (a live
axes/buttons page, the natural home of a remap UI), rumble and LEDs
(interrupt OUT reports, `SET_REPORT`), Xbox One GIP, Bluetooth.

## Verification harness (`tools/usb/`)

The verdict is what the guest *did*, as with keyboard, mouse and tablet.

- **`fakepad.py`** — a usbredir protocol v0.7 **server**: it listens on a TCP
  port, answers QEMU's HELLO (verified: `qemu usb-redir guest 10.2.50`,
  capabilities `0x000000ff`), sends `ep_info`, `interface_info` and
  `device_connect`, serves the descriptor control transfers
  (`GET_DESCRIPTOR`, `SET_CONFIGURATION`), answers
  `start_interrupt_receiving` and then streams `interrupt_packet`s from a
  script (`{"t_ms": .., "report": ".."}`), so one run replays a whole session:
  stick sweep, trigger pull, button chords, a held button across an unplug.
  `--descriptor FILE` presents a captured real descriptor; `--xinput`
  presents the vendor interface instead. This is what CI runs — QEMU's
  `usb-redir` needs no host USB stack, so it works on every runner and on
  Windows. **Two details cost a day if missed** (both verified): the server
  must advertise `connect_device_version`, `ep_info_max_packet_size`,
  `64bits_ids` and `32bits_bulk_length` (caps `0x72`) and switch to the
  16-byte header with a 64-bit id after the HELLO — QEMU refuses a usb-redir
  host without the last three on an xHCI port (`hw/usb/redirect.c`), and the
  `ep_info` body is 160 bytes, not 96, whenever both sides have
  `ep_info_max_packet_size` (the `max_packet_size[32]` array). The guest's
  USB stack then issues a standard `GET_DESCRIPTOR(device, 8)` control
  transfer (SeaBIOS in a diskless probe, `usbd` in the real harness) that the
  server must answer before the device is usable.
- **`gamepad_run.py`** — builds (`LAZYOS_SERVICES=1 LAZYOS_USB=1
  LAZYOS_USB_TRACE=1`), boots QEMU with `qemu-xhci` +
  `-device usb-redir,chardev=pad0 -chardev socket,id=pad0,...` and **no
  i8042**, runs the script, then judges: `judge.py --gamepad` requires every
  `AXIS`/`BUTTON` `inputd` saw to be one `usbd` published (same usage, value
  and order, the keyboard judge's cross-check), the deadzone and hat
  conversions to match the script, no `USBD:FATAL`/`PANIC`, and — when a
  button is held across the unplug — a release for it. The Doom session
  (G5) is the app-level verdict. Variants: `--no-pad` (nothing binds),
  `--two-pads` (two `usb-redir` devices, one session), `--hotplug N`
  (plug/unplug cycles, DMA bounded as in `run.py --hotplug`).
- **`--real 2dc8:310b`** — swaps the fake device for
  `-device usb-host,vendorid=..,productid=..`, so the actual 8BitDo is
  enumerated by `usbd` in QEMU. Verified working on the Windows dev box
  (`info usb`: `Port 1, Speed 12 Mb/s, Product 8BitDo Ultimate 2 Wireless
  Cont`; libusb's `kernel_driver_active: NOT_SUPPORTED` lines are noise).
  Two things the README must say: over the USB cable the pad is **X-input
  only** (D-input is selected by holding B while powering on, and is
  available over the 2.4G dongle or Bluetooth), and `usb-host` needs a
  libusb-openable device (on Windows another machine may need Zadig; on
  Linux it just works). `--pcap FILE` records the enumeration and the
  reports, which `capture.py` turns into golden fixtures for `libs/usbhid`.
- **`capture.py`** — capture a real pad's descriptors once (from a
  `--pcap` run, or the HID report descriptor alone through `hidapi` on the
  host) and write the inline fixtures `libs/usbhid/src/tests/golden.rs` uses,
  the same way QEMU's keyboard, mouse and tablet descriptors are checked in
  today.
- **Real xHCI:** the NUC stick (`run_demo.py --desktop --usb-image`) with a
  pad plugged in, read back with `dbgctl log --source programs | grep USBD:`;
  the harness README documents the one-line command.
- **CI:** a `gamepad` job in `.github/workflows/usb.yml` runs
  `gamepad_run.py` (fake device) plus `cargo test -p usbhid -p inputmap` and
  the fuzz corpus check; the real-device modes stay manual.

## Risks and open questions

1. **Descriptor variety is the real risk.** Vendor-defined collections,
   pads whose axes are only meaningful through a mapping table, reports split
   by report id, 8-bit axes, hats with a different null value, pads that
   declare no Game Pad collection at all. Mitigation: the walk is bounded and
   skips what it cannot place; an unplaceable interface is left unbound and
   logged rather than half-driven; every new fixture found on real hardware
   becomes a golden test. A per-device mapping table is the deferred answer,
   keyed on the `DEVICE` record.
2. **Rate and ring pressure.** X-input is polled at 250 Hz and its report
   can change 6 axes, the hat and 11 buttons — 18 records, 4500/s against the
   kernel's token bucket (512 burst, 64 per 10 ms = 6400/s); a standard HID
   pad can change 9 axes and 32 buttons, at whatever rate its endpoint
   declares. Mitigation: only *changed* axes are published, `AXIS`
   tail-merges, and the harness measures the dropped/throttled counters;
   raising the refill is a one-line kernel change with tests.
3. **`Dropped` resync semantics.** Centring axes that are physically held is
   wrong until the next report — but a pad only sends changes, so a stick held
   still after a ring overflow reads as centred until it moves. That fails
   safe (the player stops) and matches the keyboard's "release what you cannot
   prove is held". The judge checks it happens.
4. **X-input is not one protocol.** v1 is the Xbox 360 style report. Xbox One
   / Series pads need GIP (handshake + 64-byte packets) and are explicitly
   out; a pad that needs a vendor init sequence will simply report nothing and
   must be logged, not retried forever.
5. **`usb-host` is a convenience, not CI.** It binds a real device on a real
   host OS; the CI verdict stays the scripted fake device. QEMU's `usb-redir`
   models the device, so the *electrical* side is not tested there — the NUC
   run is.
6. **One report-protocol function per device.** `bind_all`'s second pass
   stops at the first interface that yields a layout; a composite device with
   both a gamepad and a tablet interface (rare) binds one. Widening it is a
   `usbd` change with its own test if a fixture ever needs it.
7. **Mapping and naming.** Until a per-device table exists, "button 1" means
   "HID usage 1", and two pads of different makes may swap A and B. Axes
   differ more: X-input puts the triggers on Z/Rz, while most HID pads put
   the right stick on Z/Rz (and a DualShock its triggers on Rx/Ry), so
   `Standard`'s triggers are only right for X-input until the table exists. The
   client-side `Standard` view documents the convention the tested pads
   follow; the Settings page and the table come after.
8. **No vendor/product in the session API.** The `DEVICE` record is on the
   bus and in `inputd`, but not in the client API yet; a Settings page that
   shows names needs one more field and is part of that later phase.

## Suggested order of work

G0 and G1 are libraries and kernel only and prove themselves with host tests
and `tools/test/run.py`. G2 + G3 + G7 (fake device) make the pipeline
verifiable end to end before any game changes. G6 is what makes the *real*
8BitDo work in QEMU over its cable and is independent of G5, so it can land
right after G3 if the hardware is the priority; G5 (Doom) is the first
user-visible win. A plausible first PR is G0 + G1, mirroring `usb-hid-plan`'s
P0 + P1: both are testable without touching a USB register or a game.
