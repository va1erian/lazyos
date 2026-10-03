# Plan: run `xui` apps on LazyOS in the tiny-skia (canvas) renderer

**Goal:** run an ordinary `xui` application (`xui-core` + a **tiny-skia software
renderer**) inside a LazyOS window, with real input — **without** `winit`,
`softbuffer`, `glutin`/`glow`, or any OS windowing system.

> **Status:** M0-M2 and the compositor-client milestone are done; see the
> "Status" section at the end. The design sections predate the display grant:
> the "one implicit window per task" `gfx_present`/`input_poll` surface was
> realised as native syscall 12 (`bind`/`present`/`input_poll`/`create_buffer`,
> [`architecture/display.md`](architecture/display.md)) and, for windowed apps,
> as the `os.lazy.display.v1` client mode inside `xuid`. The `xui-skia` split
> was not needed: upstream `xui-canvas` grew a default-on `winit-backend`
> feature, so building it with `default-features = false` gives the software
> painter core LazyOS wants (no vendor fork, no `[patch]`).

This builds on `docs/linux-abi-plan.md` (Rust `std` via the Linux x86_64 ABI
shim). `xui` is a `std` library (`Rc`, `Vec`, `format!`, `std::thread`), so
"run an xui app" presupposes `std` works on LazyOS.

## What "canvas renderer mode" is

`xui-canvas` contains two things:

1. **The software painter core** — `SkiaCanvas` (an impl of `xui_core`'s
   `Canvas` trait using `tiny-skia`), plus node/clip/cull compositing and the
   `TextShaper`. **This is portable** and is what we want.
2. **The window hosts** — `WinitBackend` (`winit` + `softbuffer` + optional
   `glow`/`glutin`) and `OffscreenBackend` (headless). The winit/GL parts need a
   real OS windowing system; `OffscreenBackend` avoids it but lives inside the
   same crate, which *unconditionally* depends on `winit`/`softbuffer`/`glutin`.

So the plan has **two halves**: a small `xui`-side split to expose the painter
core without windowing, and a LazyOS-side backend that presents pixels and feeds
input.

## The Counter target

The milestone app is the README's `Counter` (a `Label` + `Button`, `App::update`
on click). It exercises: `xui-core` runtime, painted widgets, layout, theming,
text measurement, and mouse input — the whole spine without native controls.

## Minimal OS surface required

Beyond `std` (see the Linux-ABI plan), a canvas `xui` app needs:

| Need | Minimal LazyOS surface |
|---|---|
| allocator | `mmap`/`brk` (from the std plan) |
| present pixels | a **window surface** the task can write and have shown: `gfx_present(ptr, len)` blits an RGBA buffer to the task's window |
| input | a per-task **event queue**: `input_poll(buf)` returns mouse move/click/scroll + key/char/enter/backspace, already translated from PS/2 |
| clock | `clock_gettime` (std plan) — for `Instant`, timers, repaint throttling |
| randomness | `getrandom` (std plan) — `HashMap` seeds inside xui |
| font file | a bundled TTF read through `std::fs` (LazyOS already vendors JetBrains Mono; `fontdb`/`cosmic-text` can be pointed at it) |
| DPI | `96 * scale`: the desktop's integer scale from `GetOutput` ([hidpi-plan.md](hidpi-plan.md)); `DpiChanged` deferred |
| resize | initially fixed size; a `Resize` event later |

Design choice: **one implicit window per task.** LazyOS already gives each task a
window in the multiplexer; the app addresses "its" window with no ids. This keeps
the syscall surface to two calls (`gfx_present`, `input_poll`) plus `gfx_info`
(width/height/format). Themes/multi-window can come later.

## Minimal `xui`-side surface

Add a **winit-free** path so a non-winit backend can reuse the painter core.
Preferred shape: a small crate `xui-skia` (or a `xui-canvas` feature that turns
`winit`/`softbuffer`/`glutin` off) exposing:

- `SkiaCanvas` — the `xui_core::backend::Canvas` implementation (tiny-skia).
- the **offscreen compositor** — the node/clip/cull logic that paints a window's
  node set into an RGBA surface (as `OffscreenBackend` already does).
- the **text shaper** (`cosmic-text`) behind `xui_core`'s `TextShaper`/
  `TextLayout` seam, plus a way to load a specific font file.

`WinitBackend` and `OffscreenBackend` then depend on `xui-skia`, and a new
LazyOS backend does too. This is the only change needed in the xui tree; it is a
refactor, not new rendering.

## The LazyOS backend

`LazyOSBackend` implements `xui_core::backend::Backend`, modelled directly on
`OffscreenBackend` (both are "painted for everything", `ImplKind::Painted`), and
adds a real window + event pump:

- **lifecycle** — `run_with`: ask the kernel for the window (`gfx_info`), call
  `on_ready` to build the app at DPI 96, then loop: `input_poll` → translate to
  `Event` → deliver to the installed `WidgetHost` sink; on `invalidate`/`Paint`,
  re-render via `xui-skia` and `gfx_present`.
- **nodes** — `create`/`destroy`/`apply_moves`/`set_visible`/`set_painter`/
  `invalidate` kept as an in-memory node table (exactly what OffscreenBackend
  keeps); no OS handles.
- **text** — `measure_text`/`text_shaper`/`layout_text` via the shaper; `dpi`
  returns 96; `client_rect` from `gfx_info`.
- **runtime** — `set_theme` stores the `Theme`; `set_timer` uses the std clock +
  the event loop (or a kernel timer syscall later).
- **unsupported** — `native_window` → `None`; `capture`/`run_modal` → the
  defaults (`Unsupported`). No GL.

Input translation (`input_poll` bytes → `xui_core::backend::Event`):

| LazyOS | xui `Event` |
|---|---|
| mouse move | `MouseMove { pos }` |
| press/release | `MouseDown` / `MouseUp { button }` |
| key scancode → `Key` | `KeyDown { key, modifiers }` |
| printable | `Char(c)` |
| focus change | `Resize`/`DisplayChange` as available |

## Milestones

- **M0 — present a buffer.** A `std` program fills an RGBA buffer (solid/gradient)
  and `gfx_present`s it in a LazyOS window. Proves the present syscall + window
  ownership, no xui yet.
- **M1 — xui paints headlessly.** Add `xui-core` + `xui-skia`; run the Counter with
  a fixed input (synthetic events), render to an image, `gfx_present`. Proves the
  painter core and text on LazyOS.
- **M2 — real input.** Wire `input_poll` → xui `Event`s. The Counter's button
  responds to real mouse clicks; a `Label`/`Edit` shows text. Focus follows the
  multiplexer.
- **M3 — polish.** Bundled font via `fontdb`, timers, resize, dark mode toggle,
  and (if needed) `std::thread`/`proxy()` for a worker.

## Dependencies (target)

`xui-core`, `xui-skia` (new), `tiny-skia 0.11`, `cosmic-text 0.19` — and
deliberately **not** `winit`, `softbuffer`, `glutin`, `glow`, `xui-gpu`.

## Risks

| Risk | Mitigation |
|---|---|
| `std` on LazyOS not ready | sequence after Linux-ABI L0–L2; M0 needs only the present syscall + a no_std demo |
| `winit` leaks into the build via `xui-canvas` | do the `xui-skia` split first; verify the dependency graph has no winit |
| `cosmic-text` weight / shaping | bundle one font and configure `fontdb` by path; fall back to a simple shaper for M1 |
| text layer needs system font dirs | point `fontdb` at the bundled file; ship it on the FAT volume |
| DPI/resize divergence | fix 96 dpi and a fixed window size first |
| xui's `Rc`/single-thread event loop vs. our tasks | run the UI in one task; use `proxy()` only when `std::thread` lands |

## Effort

- `xui-skia` split: a refactor of `xui-canvas` — small.
- M0 present syscall + window ownership: small, mostly kernel/mux work.
- M1 painter core on LazyOS: medium (depends on `std` maturity).
- M2 input mapping: medium.
- M3 polish: ongoing.

## Status (issues #114, #153, #168)

Landed in `xui-app/` (a standalone static-musl workspace built by
`tools/xui/build.py`, embedded as `/system/bin/xapp` with `LAZYOS_XUID=1` +
`LAZYOS_XUI_APP=<path>`):

- **M0** — `src/bin/m0.rs`: a `std` shim over syscall 12 (`bind`/`present`/
  `input_poll`) paints a gradient and presents. Prints `XUIAPP:PRESENT:PASS`.
- **M1** — `backend::LazyOSBackend`, modelled on `OffscreenBackend` (node
  table, painters composited into a `Surface`, `render`), with a real event
  loop for a display-owning task. The Counter (`src/bin/counter.rs`) paints and
  presents; `XUIAPP:COUNTER:PASS` follows the first frame.
- **M2** — `input_poll` records are translated to `xui` events (mouse
  move/down/up, key/char) and routed to the node under the pointer; a real
  left click increments the Counter and prints `XUIAPP:INPUT:PASS`.
- **Viewers** (issue #153) — `src/bin/sysmon.rs` (syscall-14 dashboard:
  frame/slab/heap gauges, uptime, task table; a Services tab, issue #489,
  lists `init`'s supervised services with `healthd`'s health through the
  generated stubs in `src/services.rs`) and `src/bin/fabricmon.rs`
  (syscall-5 panel: registry names with owners/interfaces, topics-broker
  counts, shared buffers/fences/handles, per-task usage; the registry and
  topics wires come from the generated `messenger-generated` stubs, issue #302,
  while the stats payload is a raw syscall snapshot). Both refresh on a
  one-second backend timer, route `r`/`q` through the backend's focused-node
  keyboard path, print `SYSMON:UP:PASS` / `FABMON:UP:PASS` (plus refresh and
  quit markers, and `SYSMON:VIEW:*` / `SYSMON:SERVICES:PASS` for the tabs), and are captured by `.github/workflows/xui.yml`.

Text uses the bundled `DroidSans.ttf` (Apache-2.0, see `assets/fonts/README.md`) via `include_bytes!` (the Terminal alone switches to JetBrains Mono for its fixed grid).
`xui-core` and `xui-canvas` are git dependencies on `va1erian/xui`, pinned to
the same `rev = "785ddada73e5fa2e6b5d1ba5587e9745a2ec238c"` (see
`xui-app/Cargo.toml`). `xui-canvas` is built with `default-features = false`: that
turns off its `winit-backend` feature (winit/softbuffer/glutin/glow/arboard/
windows/xui-gpu) and leaves the pure tiny-skia/cosmic-text software painter
core, including the in-memory font API (`set_default_font`/`add_font`/
`set_default_family`, which build the shaper database from registered bytes
without scanning the system font directories or memory-mapping files — LazyOS's
Linux ABI has anonymous `mmap` only), per-line horizontal alignment for
natural-width runs, and `Surface::pixels` for a clone-free present. `xui-icons`
(explorer's optional `village-icons`) is the git dependency at the same rev.
There is no vendored copy and no `[patch]`.

**Bumping the pinned rev:** both `xui-core` and `xui-canvas` (and the dev-only
`xui-canvas` plus optional `xui-icons` in the `crates/*` manifests) MUST move to
the same new commit together, or two different `xui_core` versions end up in the
graph. Bump every `rev = "..."` under `xui-app/`, then run `cargo fetch` in
`xui-app/` (or just build) to refresh `Cargo.lock`; confirm the diff touches
only the three xui packages, and re-run
`cargo tree --target x86_64-unknown-linux-musl | grep -E
'winit|softbuffer|glutin|glow|arboard|xui-gpu'` to confirm the windowing crates
are still absent.

**Compositor client (M3a, issue #168)** — `src/bin/client.rs` (`xui-client`)
runs the same counter + an `Edit` text field as a `xuid` client:
`LazyOSBackend::new_client` resolves `os.lazy.display.v1` through the raw
syscall-5 shim (wire codecs from the generated `messenger-generated` stubs), creates a surface, attaches a display shared buffer
(`create_buffer` op 4), commits damage rectangles per invalidated node, and
consumes pointer/key/`WINDOW_CLOSE` events from its event endpoint. The
compositor chrome (drag, minimize, close) is `xuid`'s, and a title-bar
drag works on the app window. Build with `LAZYOS_XUID=1`, `LAZYOS_XUI_CLIENT=1`
and `LAZYOS_XUI_APP=<xui-client.elf>`; the kernel then spawns `xuid` + the app
and no `xdemo`, so the app is the first surface at the top-left. Owner mode
(`LAZYOS_XUI_APP` without `LAZYOS_XUI_CLIENT`) is unchanged.

**Desktop session (issues #215/#216, `LAZYOS_DESKTOP=1` #217)** — one switch
expands to the whole recipe: a services session, `xuid`, and the xui apps.
Since F5 (issue #509) each desktop app is a core package
(`xui-app/packages/<short>/`, `/system/packages/os.lazy.<short>.lzp`) that
`pkgd` installs into `/apps` at boot; the Terminal, Devices, the Installer and
LazyShell stay built-in `/system/bin` programs. `init` opens the apps whose
manifest sets `autostart` as `xuid` clients once provisioning is done; by
default only the Terminal is autostarted (`LAZYOS_XUI_AUTOSTART` lists other
stems, `none` disables it) and the other apps open on demand from
LazyShell's start menu (grouped by package category) or desktop icons (each
package's own icon) (issue #157).
`sysmon`/`fabricmon`/`counter` pick client mode via `LazyOSBackend::connect`;
the new `xui-term` hosts BusyBox `sh` over a pipe pair (issue #254). The desktop
image also ships the migrated document apps **Editor**, **Paint** and **Files**
(on demand rather than autostarted): Files opens a
text file in the Editor through `mimed`'s open-with registry. The desktop profile starts no
demo/evidence programs (no `flaky`, `top` launch self-test or clipboard demo
pair). Captured by `tools/screenshot/examples/xui_desktop.json` in the `xui-app`
job.

**Keyboard focus routing (issue #151)** — the backend now tracks focus stops
(the Tab-order nodes plus button-like controls), moves focus on a pointer press
that lands on one, delivers `SetFocus`/`KillFocus`, and routes `KeyDown`,
`KeyUp` and `Char` to the focused node rather than the node under the pointer.
`Tab`/`Shift+Tab` cycle focus; with `xuid` running (client mode) a plain `Tab`
reaches the client (the compositor keeps only Alt+Tab/Ctrl+Tab), and
`PageUp`/`PageDown` are delivered to the focused widget (the Editor scrolls)
instead of cycling focus. The client session
clicks the `Edit`, moves the pointer away and types (`XUIAPP:KEY:PASS`), cycles
to the counter button and activates it with Space (`XUIAPP:COUNTER:1`), then
cycles back and types again.

`tools/screenshot/examples/xui_counter.json` scripts the M2 click;
`tools/screenshot/examples/xui_sysmon.json` and `xui_fabricmon.json` script the
viewers, and `tools/screenshot/examples/xui_client.json` the client-mode
session (focus routing, key-driven counter, drag, minimize/restore, close).
`.github/workflows/xui.yml` builds the app, boots each image headlessly, and
checks the serial markers and pixels.

**Remaining** (the M3 list; resize landed with #412, the backend turning
`Configure` into a `Resize` event): DPI *changes* (a fixed 2x scale landed
with [hidpi-plan.md](hidpi-plan.md)), zero-copy scanout, and
`std::thread` workers via `proxy()`. The two protocol gaps found while writing
the client (`PointerDown`/`PointerUp` without a button id, and screen-absolute
`PointerMove`) were closed by the MIDL migration of `os.lazy.display.v1`
(issue #287, `idl/display.midl`): every pointer event is surface-relative and
presses/releases carry the button id, so the client backend no longer recovers
the surface origin from the last press.

**Docs app.** `xui-docs` (`xui-app/docs/`) renders Markdown with `xui-litehtml`,
a `xuid` client like the other apps; see [`xui-docs.md`](xui-docs.md). The
mouse wheel now reaches xui apps (see `architecture/display.md`).

**LazyWriter.** `writer` (issue #533) is a word processor on xui's
`xui-rich-text` editor, ported from its `wordpad` example; see
[`xui-writer.md`](xui-writer.md).

## Smallest first step

Land **M0**: a `gfx_present` syscall so a task can own a LazyOS window and blit
an RGBA buffer, plus `input_poll` for mouse events. That is the entire LazyOS
surface an xui canvas backend needs; everything else is `std` (already planned)
and an `xui-skia` split (a refactor upstream).
