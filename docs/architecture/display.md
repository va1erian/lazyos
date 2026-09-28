# Display, input & mux

**What it is.** Rendering and input: the kernel text console, the two-window
multiplexer (kernel task), the userspace display grant, and the committed
compositor demo. Target toolkit design: [xui-plan.md](../xui-plan.md).

**Key files**

| Path | Role |
|---|---|
| `kernel/src/console.rs` | Framebuffer text console (anti-aliased glyphs) |
| `kernel/src/gfx.rs`, `surface.rs` | Framebuffer access; `RgbaBuffer`/`Surface` double buffering |
| `kernel/src/text.rs`, `font.rs`, `font-atlas/` | Positioned text draws; build-time glyph atlas |
| `kernel/src/cursor.rs` | Mouse cursor sprite overlay |
| `kernel/src/input/{keyboard,mouse}.rs` | PS/2 drivers (IRQ1/IRQ12) |
| `kernel/src/mux.rs` | Terminal multiplexer: paints task windows, Tab focus |
| `kernel/src/display.rs` | Display device grant, syscall 12, input event queue |
| `user/src/bin/xuid.rs`, `xdemo.rs` | Compositor and demo app (issue #113) |
| `user/src/messenger.rs` (`display` module) | `os.lazy.display.v1` client/server helpers |
| `xui-app/`, `tools/xui/build.py` | Ordinary xui app on the display grant (issue #114) |

**Kernel mux** (`mux.rs`)

- Runs as the kernel task (`task::KERNEL_TASK`, `Interactive` class). Renders up
  to two task windows side by side with titles; the focused window gets a green
  border and `*`. Repaints only when `NEEDS_REDRAW` is set (output, focus, task
  death), plus incremental mouse-cursor blits; sleeps `IDLE_TICKS = 2`.
- `Tab` cycles focus in `task::on_key`; Ctrl-C becomes `SIGINT` for the focused
  process group. While `display::bound()`, the mux stops painting and resumes
  with a full repaint when the compositor exits without unbinding.

**Display grant** (`display.rs`, issue #113; syscall 12)

| Op | Name | ABI |
|---|---|---|
| 0 | `bind` | out: `[width, height, stride, bpp, buffer, va, size]` |
| 1 | `unbind` | - |
| 2 | `input_poll` | in: buffer + capacity; out: event count |
| 3 | `present` | packed damage `x \| y<<16 \| w<<32 \| h<<48` |
| 4 | `create_buffer` | in: size; out: `[handle, va, size]` |
| 5 | `map_buffer` | in: handle; out: `va` |

- One owner at a time; the kernel task is refused; an owner re-binding gets its
  geometry back. On bind the kernel creates a screen-sized RGBA8 shared buffer
  via `ipc::shared` and maps it in; `present` blits a clamped damage rectangle
  to the real framebuffer (`console::with_framebuffer`). Direct scanout is not
  used because the bootloader framebuffer frames live outside the allocator's
  usable regions (zero-copy scanout is an S8 follow-up).
- Input events (`Event`, 16 bytes) are pushed by keyboard/mouse IRQs into a
  256-entry queue and drained only by the owner; kinds are pointer move/down/up
  and key down/up with `key::*` codes for non-printables. `bound()` checks owner
  liveness (`task::live`), so no scheduler teardown hook is needed.

**Display protocol / XUI current state**

- `xuid` binds the grant and implements `os.lazy.display.v1` over Messenger:
  clients attach a shared surface buffer and an event endpoint, the compositor
  composites and routes input, `xdemo` is the smallest client.
- `xui-app/` (issue #114) runs an ordinary `xui-core` + `xui-canvas` app on
  LazyOS for milestones M0-M2. It is built by `tools/xui/build.py` for
  `x86_64-unknown-linux-musl` and embedded as `XAPP.ELF` when `LAZYOS_XUID=1`
  and `LAZYOS_XUI_APP=<path>` are set; the kernel then boots it *instead of*
  the `xuid` + `xdemo` session, because the app owns the display grant itself
  (`bind`/`present`/`input_poll`) and paints full-screen. Default
  `LAZYOS_XUID=1` is unchanged. Running the app as a `xuid` client over the
  compositor protocol is the remaining step.
- Issue #153 adds the first windowed system-state viewers on that backend:
  `sysmon` renders the syscall-14 snapshot (frame/slab/heap gauges, uptime, the
  task table) and `fabricmon` renders the syscall-5 fabric (registry names with
  owners/interfaces, topics-broker counts, buffers/fences/handles, per-task
  usage). Each is one owner-drawn node with a one-second `ui` timer and `r`/`q`
  keys, prints `SYSMON:*`/`FABMON:*` serial markers, and is captured in
  `.github/workflows/xui.yml` as the display owner in turn (fabricmon over the
  `LAZYOS_SERVICES=1` session, so the registry and broker are live).
- Window management (issue #143) lives in `xuid`: the `surfaces` vector is the
  z-order (tail paints last), a title-bar press drags the window (clamped to the
  screen above the taskbar), the title bar carries close/minimize buttons, and a
  bottom taskbar lists live surfaces with the focused entry highlighted.
  Minimized surfaces are hidden and restored from the taskbar; `Tab` cycles
  focus skipping minimized ones. The close button sends the client a one-way
  `WindowClose` (method 10) event, which `xdemo` treats as "exit". Only
  `Commit` uses per-surface damage; WM layout changes repaint the full screen
  (a drag repaints the union of the old/new window rectangles).

**Invariants.** Mux is always the fallback: no compositor state is required to
paint. The screen buffer handoff app-to-compositor is zero-copy (shared
buffers); only the final composite is a memcpy per damage rectangle. Input
events go only to the bound compositor; `push_event` drops the oldest event when
the queue is full and is IRQ-safe (leaf lock).

**Status.** Working: demo mux, display grant, xuid + xdemo in headless captures
(`LAZYOS_XUID=1`), xuid window management (drag, z-order, buttons, taskbar,
focus cycling; `XUID:WM:PASS`), the xui app milestones M0-M2
(`XUIAPP:*:PASS` markers) and the sysmon/fabricmon viewers
(`SYSMON:*`/`FABMON:*` markers, screenshots in the `xui-app` workflow).
Open: zero-copy scanout, running the xui app as a compositor client,
multi-session compositors.
