# Plan: a system-wide input subsystem

> **Status: I0-I2 implemented (first cut); I3 implemented (issue #397,
> [key-state page, grabs and the escape chord](#i3-key-state-page-keyboard-grabs-and-the-escape-chord));
> the I5 console login landed (issue #396, [sessionless Open](#the-login-console-sessionless-open));
> I4 and the rest of I5 proposed.** See
> [Implementation status](#implementation-status-first-cut) for what landed and
> where it deviates from the sketch below. Original note: draft proposal (2026-09-30). Motivated by the Doom port
> ([doom-port-plan.md](doom-port-plan.md) D3), but designed for every consumer:
> the compositor, xui apps, Linux-ABI programs, the login console and games.

## What exists today, and why it is not enough

Path today: PS/2 IRQ1 -> `kernel/src/input/keyboard.rs` (scancode set 1, layout
applied in `layout.rs`, modifier tracking) -> `display_input_poll` syscall ->
**one** bound consumer (`xuid`, or an app that owns the display grant) -> `KeyDown`/`KeyUp`
(`os.lazy.display.v1` methods 8/9) with a *character-oriented* `u32`.

| Limitation | Consequence |
|---|---|
| Layout is applied **in the kernel**; the physical key is discarded | Games (WASD, "the key left of 1") and shortcut remapping cannot work on AZERTY; layout policy is unchangeable without a kernel rebuild |
| Typematic repeat arrives as repeated `KeyDown` with no marker | Clients cannot tell press from repeat; edge-driven code (games, chords) misbehaves |
| Modifier keys are never forwarded | No "Shift alone" gestures, no accurate held-key state |
| No timestamps, no device id, no sequence numbers | Cannot measure latency, order across devices, or detect loss |
| One consumer, the display grant | The login console, Linux-ABI programs and background services cannot get input on their own terms; F-keys are dropped with no compositor |
| Queue overflow is silent | A stuck-key state after a burst is unrecoverable |
| Text and key are one stream | No path to IME/compose/dead keys without breaking games |
| No focus-loss resync | Keys "stick" after Alt+Tab |
| PS/2 only | USB HID and virtio-input (`docs/driver-plan.md`) would each need their own bespoke path |

## Design

Three layers, each with one job. Policy moves out of the kernel; the kernel keeps
only what must be privileged.

```
  hardware drivers (PS/2 now; USB HID, virtio-input later; userspace drivers per driver-plan.md)
        |  raw, timestamped events        (kernel event bus, capability-gated)
        v
  inputd  (userspace service; os.lazy.input.v1 + os.lazy.input.shell.v1)
        |  keymap, repeat, modifiers/locks, hotkeys, grabs, focus routing
        |  events go DIRECTLY to each client's own endpoint (not via xuid)
        v
  apps / logind / Linux-ABI evdev nodes / any client holding an input session
        ^
        |  focus + hotkeys (os.lazy.input.shell.v1)
  xuid (compositor; owns windows only, no longer carries keystrokes)
```

Not `keyd`: that service is the secrets/crypto daemon.

### Layer 1 - kernel: raw event bus

The kernel input layer becomes device-agnostic and dumb.

- Event record (fixed 24 bytes, `repr(C)`):
  `seq: u64`, `ts_ns: u64` (monotonic; PIT-tick resolution today), `device: u8`,
  `kind: u8` (`Key`, `RelMotion`, `AbsMotion`, `Button`, `Scroll`, `Sync`,
  `Dropped`), `code: u16`, `value: i32`. (`device`/`kind` are one byte so the
  record stays 24 bytes with a full `i32` value.)
- **Key `code` is a physical USB HID usage** (page 0x07), not a character. PS/2
  scancode set 1/2 is translated to HID at the driver boundary (one table,
  including E0 prefixes and Pause/PrintScreen quirks). Every future device
  speaks the same vocabulary.
- **`value`: 0 = release, 1 = press.** The kernel never synthesises repeat.
  The PS/2 tap also swallows the keyboard's own typematic make codes (a make for
  a key already down), so the bus carries only edges and `inputd` owns rate and
  delay. Actually switching typematic off (PS/2 command `0xF3`) waits for I5:
  the legacy `display_input_poll` stream still relies on it.
- Per-consumer **bounded ring** (256 events); on overflow the oldest events are
  dropped and the next drain starts with one `Dropped` record (`seq` = first lost
  sequence number, `value` = count) so the consumer can resynchronise instead of
  guessing. Sequence numbers are global and gapless, so a consumer can prove it
  missed nothing. Consumers drain with syscall 25 (`open`/`poll`/`close`).
- **Driver sources** ([usb-hid-plan.md](usb-hid-plan.md) U1): a task holding
  `CAP_INPUT_SOURCE` (bit 10, which grants no reading) registers a source of
  a class (keyboard, pointer, tablet) with syscall 25 op 4 and publishes 8-byte
  `(kind, 0, code, value)` records with op 5 (`rdx = id << 16 | count`, at
  most 64). The kernel stamps the source's device id (`0x10 + slot`, so a
  driver cannot pose as the PS/2 devices or another driver), drops and counts
  records of the wrong kind for the class or out of range, and rate-limits
  each source (512 burst, 64 per tick). It remembers held keys and buttons per
  source and publishes their releases when the source is closed (op 6) or its
  task dies. Ids carry a generation, so a stale id fails with `-EBADF`.
- **Pointer records** (`RelMotion`, `AbsMotion`, `Button`, `Scroll`) use the
  encoding in [usb-hid-plan.md](usb-hid-plan.md) (decision 2): packed `i16`
  deltas or normalised `u16` positions, HID button usages, signed notches. A
  motion or scroll record is **merged into the newest queued record** when that
  is the bus's last publication from the same device, kind and code and still
  sits at the tail of every live ring; it then consumes no `seq`. Keys and
  buttons are edges and never merge, so a mouse flood costs one slot instead
  of evicting key events.
- Access is a **capability** (`input.raw`, kernel bit `CAP_INPUT_RAW`; per device
  class later), granted at spawn to `inputd` only (`init` strips it from every
  other service). This replaces "whoever binds the display grant gets everything"
  and removes ambient keylogging authority.
- Kernel debug console keeps a minimal built-in path (US layout) for panics and
  the early boot, independent of `inputd`.

### Layer 2 - `inputd`: policy in userspace (`idl/input.midl`)

Owns everything that was hard-coded in `layout.rs` and `keyboard.rs`.

- **Keymaps:** data-driven tables (file format: HID usage -> level 1..4 symbols,
  dead keys, compose sequences), loaded from `/etc/keymaps/*.kmap`, selected via
  `confd` (`input.keyboard.layout`). US, FR (AZERTY) first, matching what the
  kernel has today. Changing layout is a config write, live, no reboot.
- **Modifier/lock state** machine including Caps/Num/Scroll lock LEDs, AltGr,
  sticky-key hooks (accessibility), and *all* modifiers reported as real key
  events (fixing "modifiers never forwarded").
- **Repeat:** generated in `inputd` with configurable delay/rate (`confd`), and
  every repeated event is flagged `Repeat` (never confused with `Down`).
  Repeat cancels on release, focus change, or grab change.
- **Hotkeys and grabs:** the compositor registers chords (Alt+Tab, Ctrl+Esc,
  Super, Alt+F4) with `inputd`; matches are consumed and delivered only to the
  registrant. A client may request a **keyboard grab** (fullscreen game, remote
  desktop) subject to compositor approval. A reserved **escape chord** (e.g.
  hold Ctrl+Alt+Esc) always reverts a grab and cannot be registered by anyone
  else, so a grabbing app can never trap the user.
- **Seats and focus:** `inputd` tracks the focused consumer (from `xuid`'s
  `FocusChanged`, and the login console when no session exists) and routes to
  exactly that consumer. Nobody else receives keys. A click can move focus,
  but which window it focuses is `xuid`'s decision, made after `inputd` has
  forwarded the press: so after a press `inputd` holds the key content that
  follows, in order, until `xuid` sends `NoteInputDone` (it has handled that
  pointer event and noted the focus it led to), at most 250 ms
  (`libs/inputmap/src/barrier.rs`, `user/src/bin/inputd/settle.rs`). Without
  it a key typed right after a click went to the window the click left.
  Session: `tools/screenshot/examples/focus_click_type.json`.
- **Topics** for observers that must not see content: `os.lazy.input.state`
  (layout changed, device added/removed, lock LEDs) declared in MIDL topics
  (`idl/topics.midl`). No keystroke content is ever published on a topic.

### Layer 3 - client delivery: its own namespace, key and text separate

Input does **not** grow `os.lazy.display.v1`. Display stays about surfaces,
buffers and windows; input gets its own MIDL interfaces in `idl/input.midl`, so
the two evolve, version and are authorised independently (a login console or a
service with no window can use input; a display client needs no input rights).

| Interface | Audience | Capability | Role |
|---|---|---|---|
| `os.lazy.input.v1` | any client | `input.session` (default for windowed apps) | Open a session, receive events on your own endpoint, query state |
| `os.lazy.input.shell.v1` | the compositor / session shell only | `input.shell` (held by `xuid`, `logind`) | Tell `inputd` who has focus, register hotkeys, approve grants |
| kernel raw bus | `inputd` only | `input.raw` | Layer 1 above; not a Messenger interface |

**Client flow.** A display client keeps creating its surface through
`os.lazy.display.v1` unchanged. To get keys it calls `os.lazy.input.v1`
`Open(surface: Option<U64>) -> (session: U64)` and transfers an event endpoint
in the parcel (endpoint transfers stay in `handles`, as in `display.midl`).
`inputd` binds the session to the kernel-stamped sender task, and the shell
side tells it which session owns which surface (`xuid` already knows the
surface's creator, so it registers `(surface, owner task)` and `inputd`
matches the `Open`). A client can never claim a surface it does not own.
Events then flow `inputd` -> client directly, which is one Messenger hop
fewer than today's kernel -> `xuid` -> client path.

`os.lazy.input.v1` (client -> `inputd`): `Open`, `Close`, `GetState`
(layout, repeat timing), `RequestGrant(kind)`, `ReleaseGrant`, `Ping`.

`os.lazy.input.v1` events (`inputd` -> client, `oneway`):

| Event | Fields | For |
|---|---|---|
| `KeyEvent` | `code` (HID), `sym` (keysym under active layout, or 0), `mods`, `state` (`Down`/`Up`/`Repeat`), `ts_ns`, `seq` | shortcuts, games, terminals |
| `TextInput` | `utf8` (composed result, dead keys and compose applied) | text widgets; the IME hook |
| `KeyboardEnter` | `down: Array<U16>` currently held keys | focus gained: seed state |
| `KeyboardLeave` | none | focus lost: **client must release all keys**; `inputd` also cancels repeat |
| `LayoutChanged` | layout name | UI hints |

`os.lazy.input.shell.v1` (compositor -> `inputd`): `SetFocus(session or
surface)`, `RegisterSurface(surface, owner)`, `RegisterHotkey(chord) ->
(id)`, `UnregisterHotkey`, `ApproveGrant(session, allow)`. Shell events
(`inputd` -> compositor): `HotkeyFired(id)`, `GrantRequested(session, kind)`,
`EscapeChord`.

Rules: physical `code` always present; `TextInput` only for character-producing
presses (not for Ctrl/Alt chords, mirroring today's rule 5 in `display.md`).
`KeyEvent.Down` carries no text, so games ignore `TextInput` and editors ignore raw
codes, and neither has to reverse-engineer the other.

**Legacy bridge.** `display.v1` methods 8/9 (`KeyDown`/`KeyUp`) are frozen, not
extended. While old clients exist, `xuid` synthesises them by subscribing to
`inputd` as an ordinary shell-side session for legacy surfaces only. New code
never uses them, and they are removed in I5. No new display method is added by
this plan.

**Low-latency poll path for games.** Optionally `inputd` (not the compositor)
hands a focused session a `SHARE_ONLY` shared buffer holding a 256-bit *down
bitmap* plus a `seq: AtomicU64`. Reading `is_down(HID_W)` is a memory read, no
Messenger round trip and no event queue. The bitmap is cleared and the client
notified on `KeyboardLeave`. Events still flow for edge-triggered actions.

### Linux ABI: `/dev/input/event*` for unmodified programs

The shim gains **evdev-compatible character nodes** backed by `inputd`
(`EVIOCGVERSION`, `EVIOCGBIT`, `EVIOCGNAME`, `EVIOCGRAB`, `read()` of
`struct input_event`, `poll`/`epoll`). Because the key code space is HID and the
translation to Linux `KEY_*` constants is a fixed table, prebuilt musl programs
(SDL, libinput-style code, Doom ports that read evdev) run unchanged. An open
of `/dev/input/eventN` yields events **only while the process's window is
focused**; there is no way to read another app's keystrokes. A dedicated
capability is required for the raw, unfocused device (console/login only).

### Security properties (the point of doing this centrally)

1. Only the focused consumer receives key content; enforced in `inputd`, not by convention.
2. The raw stream is a capability held by one service; a compromised app cannot open it.
3. Grabs are compositor-approved and always escapable by a reserved chord.
4. **Secure attention sequence** (Ctrl+Alt+Del or equivalent) is handled by `inputd`
   and delivered only to `logind`/the lock screen, so a fake login window cannot
   intercept it. Password fields can mark a surface `secure`, suppressing debug
   logging and screenshot capture of key events.
5. Every event carries a kernel-stamped source device and timestamp; no client
   can forge them.
6. Unknown or malformed events are dropped and counted, never trusted (input is
   untrusted, per the project's code standards).

## Migration (no flag day)

| Phase | Deliverable | Tests (per the kernel testing rule) |
|---|---|---|
| **I0** | Kernel raw event bus + HID translation table + `Dropped` marker; PS/2 driver emits both old and new streams. `display_input_poll` unchanged. | Correctness: table round-trips for every scancode incl. E0, Pause, PrintScreen; release/press pairing; ring wraparound and overflow marker. Stress: millions of events across producers, no loss without a marker |
| **I1** | `idl/input.midl` + `inputd` skeleton: consumes raw events, ports `layout.rs` (US, FR) to data files, generates repeat, reports modifiers. Runs beside the old path behind `LAZYOS_INPUTD=1`. | Host-run keymap unit tests; QEMU `send-key` scripts via `qemu_qmp.py` typing all printable keys on both layouts |
| **I2** | `idl/input.midl` (`os.lazy.input.v1`, `os.lazy.input.shell.v1`) is generated with `midlc`; `xuid` becomes a shell client (focus, surface registration, hotkeys) and stops carrying keystrokes; apps `Open` a session and receive `KeyEvent`/`TextInput`/`KeyboardEnter/Leave` directly from `inputd`; `KeyDown/KeyUp` are synthesised for legacy surfaces only. xui backend migrated to `TextInput` for text widgets. | Existing display suite (`kernel/src/tests/display_suite/`) must still pass unchanged; new focus-change, stuck-key and grab tests |
| **I3** | Poll bitmap buffer; keyboard grab + escape chord; Doom (`lazydoom`) adopts `KeyEvent` + bitmap. | Screenshot session driving WASD, fire, automap in the Doom title/demos; grab/escape scenario |
| **I4** | `/dev/input/event*` on the Linux shim; `input` ABI fixture (`ABI:input:PASS`). | Fixture: open, `EVIOCG*` ioctls, read events under injected QMP keys, focus-gated reads, `EVIOCGRAB` |
| **I5** | Remove the kernel layout code and the old bound-consumer path; USB HID / virtio-input drivers plug into the bus with no other changes (USB HID already does: `usbd` publishes through syscall-25 input sources, [usb-hid-plan.md](usb-hid-plan.md) U1-U5); `logind` console login moves onto `inputd`. | Full `tools/test/run.py --accel none`, desktop screenshots, login session |

Each phase is independently shippable and reversible until I5.

## What this buys the Doom port

D3 in the Doom plan collapses to: consume `KeyEvent` (physical HID codes map 1:1
to Doom keys, so AZERTY players get WASD-equivalent positions), use the down
bitmap for movement, release on `KeyboardLeave`, ask for a keyboard grab when
fullscreen. No repeat filtering, no chord workarounds, and the same code works on
any layout. A Doom build reading `/dev/input/event*` also works after I4 with no
LazyOS-specific keyboard code.

## Decisions (simplest option, enrich later)

The goal of the first landing is the **abstraction boundary**: raw HID-coded
events in the kernel, policy in `inputd`, key/text split at the client. Anything
that can be added behind that boundary later is deferred.

| Question | Decision | Deferred enrichment |
|---|---|---|
| Scancode set | Keep **set 1** as today; translate to HID in the PS/2 driver | Set 2 / USB HID / virtio-input drivers just emit HID into the same bus |
| Keymap format | **Compiled-in tables** in `inputd` for US and FR (ported from `layout.rs`); layout chosen by a `confd` key | `.kmap` files under `/etc/keymaps`, XKB import, dead keys/compose |
| Key repeat | In **`inputd`**, fixed delay/rate constants | Configurable via `confd`, per-device rates |
| Secure attention sequence | **Out of scope** for now | Follows the session/lock-screen work in `security-model.md` |
| Pointer devices | **Migrated** by [usb-hid-plan.md](usb-hid-plan.md): the PS/2 mouse publishes `RelMotion`/`Button`/`Scroll` on the bus (tail-merged so motion cannot evict keys; a turn merges only under ring pressure) and `inputd` owns the one cursor (P0, P1), which `xuid` takes from it, falling back to the legacy display stream only while `inputd` is away (P2). USB keyboards, mice and tablets (`usbd`, U2-U5) publish through kernel input sources into the same bus and cursor. The kernel cursor state is removed with I5 | Pointer capture via the grant mechanism |

## Minimal first cut (what "v1" means)

In: I0 (kernel bus, HID translation, `Dropped` marker), I1 (`inputd` with
US/FR tables, modifiers, repeat), and I2 (the two `os.lazy.input` interfaces with `xuid` as shell client, `KeyEvent` /
`TextInput` / `KeyboardEnter` / `KeyboardLeave`, legacy `KeyDown`/`KeyUp`
synthesised for old clients). That is enough for the Doom port: physical codes,
explicit press/release/repeat, no stuck keys.

Later, each independent and additive: down-state bitmap (I3), keyboard grab and
escape chord (I3), `/dev/input/event*` (I4), removal of the old kernel layout
path (I5), data-driven keymaps, hotkey registration UI, SAS, pointer migration.
Hotkey registration stays a small internal table in `xuid` until then.

## Implementation status (first cut)

I0, I1 and I2 are in. Where the code differs from the sketch above:

| Area | As built |
|---|---|
| Kernel bus | `kernel/src/input/{hid,raw_tap,bus,rawsys,sources}.rs`; syscall 25 (`open`/`poll`/`close`) gated by `CAP_INPUT_RAW` (bit 9), driver sources (ops 4-6) by `CAP_INPUT_SOURCE` (bit 10). Records are 24 bytes with `device: u8`, `kind: u8` (see layer 1). Timestamps have PIT-tick (10 ms) resolution; ordering is by `seq`. Pause is reported as an immediate press+release (it has no break code). The legacy `display_input_poll` stream is unchanged and fed in parallel. |
| Capability | `init` starts `inputd` with `CAP_INPUT_RAW` only and strips the bit from every other manifest service; the kernel strips it from every boot-spawned program except `init` (`credentials::drop_caps`). |
| `inputd` | `user/src/bin/inputd*`, logic in `libs/inputmap` (host-tested: keymaps cross-checked against the kernel's old tables, modifier/lock state, repeat, hotkeys, resync after `Dropped`, session/focus routing). Compiled-in US and FR keymaps; layout from `confd` key `sys/input/layout`, boot default `LAZYOS_KBD_LAYOUT`, overridden for the logged-in user by the layout `xuid` names (`NoteSessionLayout`: the user's `user/<uid>/input/layout`, which `inputd` cannot read itself; `inputmap::session_layout`, `inputd/layoutsel.rs`). Repeat: 500 ms delay, 30 ms interval, fixed. NumLock starts on; LEDs are not driven. |
| Keysyms | Unicode scalars for character keys, X11 `0xFFxx` values otherwise. With Ctrl held a letter's `sym` is its unshifted form. `mods` is the state *after* the event. |
| Interfaces | `idl/input.midl`. `KeyboardEnter.down` is `Array<U32>` (MIDL has no `U16`). `Open` without a surface is the login console's session (issue #396, below); `Attach`, `UnregisterSurface`, `UnregisterHotkey`, `SessionOpened`/`SessionClosed` were added to the shell interface. I3 added `RequestGrant`/`ReleaseGrant`/`Ping`/`AttachKeyState` and the `GrantChanged` event to the client interface, a real `ApproveGrant`, a `surface` field on `GrantRequested` and the `GrabChanged` shell event (below). |
| Shell authority | `inputd` accepts shell calls only from the task that holds the display grant, which it asks the kernel for (`rawsys` op 3, `display::owner()`; the grant itself needs `CAP_SYS_ADMIN`), so no capability bit beyond `input.raw` was needed yet. (The registry's name list is privileged, and a name is not an identity anyway.) |
| Legacy bridge | `xuid` keeps its own hotkeys and the kernel key stream for surfaces without a session, and suppresses `KeyDown`/`KeyUp` for surfaces `inputd` reports a session for. It does not subscribe to `inputd` for legacy surfaces; that comes with the removal of the kernel stream (I5). |
| xui apps | The static-musl backend opens one session per window (`xui-app/src/input.rs`, `backend/session_input.rs`), maps `KeyEvent` to `KeyDown`/`KeyUp` and `TextInput` to `Char`, and releases held keys on `KeyboardLeave`. |
| Delivery robustness | A client whose endpoint (64 messages, about 20 typed characters) fills up gets the rest from a per-session backlog in order as it drains (`inputd/delivery.rs`, policy `inputmap::outbox`, host-tested). Past 1024 waiting events the backlog is dropped explicitly (`INPUTD:DROP session=<id> lost=<n>`) and the client gets `KeyboardLeave` + `KeyboardEnter` before anything newer, so a dropped release cannot leave a stuck key; a drained backlog that peaked at 64 or more is logged as `INPUTD:BACKLOG`. |
| i8042 intake | Syscalls run with interrupts off, and a large `write_file`/`append_file` (1 MiB into the ext2 block cache, about 40 ms) outlasted the controller's 16-byte queue, which drops keys silently (lost typing during first-boot provisioning). `kernel/src/input/ps2.rs` collects the controller's bytes into a 1024-byte FIFO from IRQ1/IRQ12 and the timer tick; decoding stays in interrupt context outside interrupt windows. Long syscalls take interrupts at poll points (`arch::irq_window`: the ext2 library paces itself per block, frames, page mappings, user copies), so no syscall of the provisioning workload keeps them off for 2 ms or more (`IRQOFF:MAX` reports any that does, `arch::irqoff`). A full FIFO is counted and logged (`PS2:DROP`) and releases every held key; each new longest service gap over 40 ms is logged as `PS2:GAP max_ms=<n> last_syscall=<nr>`. `xuid` retries an `inputd` call that timed out instead of dropping its link (which flipped windows to legacy keys and back mid-typing). Repro: `tools/screenshot/examples/type_during_provision.json` on a fresh image. |

| Injection loss (issue #400) | Measured with QEMU's own trace (`-trace ps2_put_keycode,pckbd_kbd_read_data`: bytes QEMU queued against bytes the guest read) and `inputd`'s `seq=`-numbered trace. Three causes, none silent any more: **(1) QEMU** queues 16 PS/2 bytes and discards the rest, and every event of one `input-send-event` call is queued before the guest reads a byte (12 keys in one call: 24 queued, 16 read); under TCG even one key per call outruns the emulated CPU (12-28 of 658 bytes lost). `qemu_qmp.send_events` now splits key events into calls of at most 16 bytes (`qemu_keys.ps2_batches`) and spaces key calls 10 ms apart under TCG (`TCG_KEY_INTERVAL`). **(2) The kernel** held interrupts off for up to 49 ms while a program's write drained to the UART at its baud rate (`IRQOFF:MAX nr=1`, `PS2:GAP max_ms=55`), long enough for QEMU's queue to overflow; the serial drain now takes interrupts at poll points (`serial/tx.rs`, tests `irqwin_serial_*`). **(3) `inputd`'s own trace** wrote each evidence line inline, slower than a burst arrives, so its 256-event raw ring overflowed (`Dropped`, resynced); lines are now queued and written a chunk per loop pass (`inputd/trace.rs`), the bus drained in between, and a `Dropped` marker is always logged (`INPUTD:RESYNC seq= lost=`). **A lost release** (a byte gone anyway) no longer leaves a key held until its next release: the PS/2 tap turns a make for a held key that arrives later than typematic could (`raw_tap::STALE_NS`, 1.5 s) into release + press (tests `input_raw_stale_*`, `input_raw_stress_lost_releases_balance`). With that, `input_keys.json` is unpaced, and `input_burst.json` types 324 characters at QMP speed with none lost (`verify_trace.py --burst`, which also fails on `INPUTD:RESYNC`, `INPUTD:TRACE:LOST` or `PS2:DROP`), under WHPX and TCG alike. |

The compositor's legacy `KeyDown` used a character's code as the Windows virtual
key, which made `& \" ' ( % $ #` arrive as arrow/page/home/end keys. Session
clients map character keys by physical position; the legacy fallback
(`xui-app/src/backend/input.rs::char_key`) now maps punctuation to the US key
that types it and never yields a navigation code.

Verification: `python tools/test/run.py --accel none` (bus, HID table, ring,
capability gate, stress), `cargo test -p inputmap -p messenger-generated`,
`tools/screenshot/examples/input_keys.json` + `tools/input/verify_trace.py`
(both layouts, serial), `input_burst.json` + `verify_trace.py --burst` (an
unpaced burst, issue #400), `python tools/screenshot/test_qmp_keys.py` (the
injector's batching), and the desktop sessions (typing in the Terminal and the
Editor on both layouts).

## I3: key-state page, keyboard grabs and the escape chord

Issue #397. Decisions in `libs/inputmap` (`keystate.rs`, `grab.rs`, the
chord in `engine.rs`; host-tested in `src/tests/grab.rs`), services in
`inputd/{keypages,grants}.rs`, the compositor side in `xuid/grabs.rs`, the
client side in `xui-app/src/input/grab.rs`.

* **Key-state page.** A session hands `inputd` a shared buffer it created
  (`AttachKeyState`, the parcel's `buffers[0]`). While the session has
  keyboard focus `inputd` writes the keys held right now into it: a seqlock
  word, the raw `seq` of the newest edge, a focused flag and a 256-bit bitmap
  of HID usages (`inputmap::keystate::SharedKeys`, 56 bytes). Focus leaving
  clears it *before* `KeyboardLeave` is sent. The plan said `SHARE_ONLY`, but
  the kernel's `SHARE_ONLY` means nobody but the creator may map a buffer
  (`kernel/src/ipc/shared.rs`), which is the opposite of what is needed, and
  buffer flags are per buffer, not per mapping. So the page is an ordinary
  buffer the client owns: `inputd` only ever writes it, keeps its own seqlock
  counter (`keystate::Writer`, never reading the page back), and the page
  carries nothing the session's `KeyEvent`s did not already tell it, so a
  client scribbling on it confuses only itself. `Ping(session, token)` returns the newest
  processed `seq` (0 unless that session is focused, and `EACCES` for another
  task's session), so a poller can tell its page is current without learning
  when keys are typed elsewhere.
* **Grabs.** `RequestGrant(session, Keyboard)` from the caller's own focused
  session (`EACCES` otherwise) goes to the compositor as `GrantRequested
  (session, kind, surface)`; with no compositor attached it is denied, never
  self-granted. `xuid` approves only for the window that has focus and is on
  screen (`XUID:GRAB:ASK`). While a grab is held `inputd` matches no hotkey
  (`Engine::set_grabbed`) and `xuid` acts on none of its chords from the
  kernel key stream (`GrabChanged`): Alt+Tab, Ctrl+Esc and Super go to the
  grabbing window. A grab ends on `ReleaseGrant`, on focus moving (a click on
  another window), on the session closing, and on the escape chord. The
  client hears `GrantChanged(kind, active, reason)`.
* **Escape chord.** Ctrl+Alt+Esc (left Alt; exact modifiers) is matched in
  the engine before the hotkey table and before any grab, consumed (press and
  release), and `RegisterHotkey` refuses it with `EACCES`, so no client and
  no compositor can take it. It reverts any grab and request and sends the
  compositor `EscapeChord`. Serial: `INPUTD:GRAB:REQUEST|ON|OFF|ESCAPE`.
* **Doom** (`doom/src/{session,window_input}.rs`) attaches a page and treats
  it as the authority on held keys: a key the page says is up is released
  even if its `Up` event never arrived (`DOOM:KEYSTATE:RELEASED`). Its window
  is now resizable, and while it is maximized and focused it holds the
  keyboard grab (so Ctrl fires and Esc opens the menu even with Ctrl held, and
  Alt strafes without Alt+Tab switching windows); it does not grab again by
  itself after the user escaped until it is re-maximized or refocused.

Tests: `cargo test -p inputmap` (grab/escape scenario, focus and approval
races, a stuck key across a focus change with both pages, the page's
seqlock, a grab soak), `cargo test -p messenger-generated --test
input_grants`, `cargo test --manifest-path doom/Cargo.toml --lib`, and the
session `tools/screenshot/examples/doom_grab.json` judged by
`tools/input/verify_grab.py` (`test_verify_grab.py` checks the judge).

## The login console: sessionless Open

Issue #396 (part of I5). Focus is per surface, so a session without one needs
its own routing rule:

* **Routing.** The console session is the fallback target: it receives key
  content while **no compositor is attached** to `inputd` (`Attach`), which is
  exactly when the console is what the screen shows and nothing can have
  focus. Under a compositor every key goes to the focused window or nowhere,
  never to a prompt the user cannot see; the compositor going away gives the
  console the keyboard back (`KeyboardEnter`), and its return takes it
  (`KeyboardLeave`). The decisions are `inputmap::Router` (`open_console`,
  `set_compositor`), host-tested with a soak.
* **Gate.** A new capability, `CAP_INPUT_CONSOLE` (bit 13), which `init`
  stamps onto `logind` alone, authorises the kernel's console *claim*
  (syscall 25 ops 7-8, `kernel/src/input/console.rs`). `inputd` admits
  `Open(None)` only from the claim's holder, which it learns from the kernel
  (op 9, `CAP_INPUT_RAW` only), so no client can open a console session. One
  holder at a time; a dead or demoted holder holds nothing.
* **No double delivery.** While the claim stands, the PS/2 path keeps typed
  keys off the kernel terminal queue (`inputd` still reads them from the raw
  bus), so the console shell `logind` starts afterwards never replays the
  user name or the password. `logind` claims and opens the session for each
  prompt and closes it and releases the claim before it starts a shell; it
  falls back to the kernel terminal (`sys::read_char`) when `inputd` is
  unavailable (`LOGIN:CONSOLE:KERNEL`). It starts after `inputd`.

Tests: `cargo test -p inputmap console`, `LAZYOS_TEST_FILTER=input_console
python tools/test/run.py --accel none` (gate, lifecycle, stale holders, keys
off the terminal queue, a claim soak), and the session
`tools/screenshot/examples/console_login_inputd.json` on a `LAZYOS_SERVICES=1`
image (log in through `inputd`, the shell's first `$?` is 0, a refused login),
run with `--fail-on "(user|lazy): not found"`.
