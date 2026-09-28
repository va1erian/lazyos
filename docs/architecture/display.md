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
- The windowing/toolkit refactor is in flight (issue #114); this page describes
  only the committed `xuid`/`xdemo` state. The retained-widget toolkit is not
  implemented in this tree yet.

**Invariants.** Mux is always the fallback: no compositor state is required to
paint. The screen buffer handoff app-to-compositor is zero-copy (shared
buffers); only the final composite is a memcpy per damage rectangle. Input
events go only to the bound compositor; `push_event` drops the oldest event when
the queue is full and is IRQ-safe (leaf lock).

**Status.** Working: demo mux, display grant, xuid + xdemo in headless captures
(`LAZYOS_XUID=1`). Open: zero-copy scanout, userspace XUI toolkit, multi-session
compositors.
