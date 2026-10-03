# HiDPI plan: a 720p desktop at double density

**Goal.** A 2560x1440 framebuffer is used as a 1280x720 desktop at scale 2.
Every pixel of text, chrome and widgets is drawn natively at 2x, never
upscaled from 720p. On a 4K monitor the QEMU window then looks sharp.
Today LazyOS draws everything at 96 DPI in physical pixels, so a 1440p
screen shows half-size UI (`docs/xui-plan.md`: "DPI fixed 96").

## Where things stand

| Layer | Today | Blocker for scale 2 |
|---|---|---|
| Mode | BIOS stage 2 of `bootloader` 0.11.17 caps VESA at 1280x720 (`bios-stage-2/src/main.rs:131`); QEMU's default std VGA (Bochs DISPI, 16 MiB VRAM) can do 2560x1600 | no 1440p mode at all |
| Kernel console | JetBrains Mono 20 px atlas, 12x16 cursor sprite | tiny at 1440p |
| Limits | already derived from the screen (`limits/derive.rs`); 4K binds (#537) | none, if re-derived after a mode change |
| `xuid` | every metric a px `const` (`theme.rs`), 13 px Droid atlases (`user/build.rs`), 10 px cursor | no scale concept |
| Protocol | `os.lazy.display.v1`: sizes, damage, pointer all physical px | clients cannot learn the scale |
| xui | every Dip goes through `to_px(dpi)`, text shaped at `size.to_px(dpi)`; `Event::DpiChanged` exists | `LazyOSBackend` pins `DEFAULT_DPI = 96` |
| xui-app | Terminal, dashboard family, LazyShell, desktop icons use raw px constants next to Dip text | text doubles, geometry does not |

## Design

1. **The wire stays physical.** Surface sizes, buffers, damage, pointer
   coordinates and size hints in `os.lazy.display.v1` remain physical
   pixels. Nothing existing changes meaning, and a client unaware of scale
   still works (it draws small). This is the Windows "per-monitor aware"
   model rather than Wayland's `buffer_scale`. All our real apps are xui
   apps, so a native 2x path is worth more than compositor upscaling.
2. **Integer scale, decided once by `xuid`.**
   - `sys/ui/scale` in `confd` is `auto` (default), `1` or `2`.
   - `auto` is the largest scale `s <= 2` that leaves a logical screen of at
     least 1280x720, so 2560x1440 gives 2 and 1280x720 gives 1.
   - The rule lives in `libs/uitheme` (`auto_scale`), shared by `xuid`,
     Settings and the owner-mode xui backend.
   - It is read at `xuid` start. Live change is a follow-up (it needs
     `DpiChanged` plus a relayout in every app).
3. **`GetOutput() -> (width, height, scale)`** is a new display method (id 43).
   - `LazyOSBackend` asks it on connect and runs the app at
     `dpi = 96 * scale`.
   - An owner-mode app (no compositor) uses `auto_scale` of its screen.
4. **The mode is set by the kernel.**
   - `display.mode=<W>x<H>` in `lazyos.cfg` makes the kernel program the
     Bochs DISPI registers (PCI `1234:1111`, ports `0x1CE`/`0x1CF`) right
     after the boot volume is read.
   - It validates the request against the VRAM the device reports (index
     `0x0A`) and maps the linear framebuffer at BAR0 through the physical
     map, which covers the first 4 GiB.
   - It then moves the console, the display grant geometry, the limits and
     the mouse bounds to the new mode.
   - Any failure keeps the firmware mode and logs `display: mode ... refused`.
     A mode the adapter does not keep is undone: the saved mode registers
     (stride included) are written back without clearing video memory, so
     the console keeps its pixels. QEMU rounds the width down to a multiple
     of 8, so a mode like 1366x768 takes this path.
   - `LAZYOS_DISPLAY_MODE=2560x1440` writes the line at build time, the same
     way `LAZYOS_LIMIT_*` does.
   - A patched bootloader or UEFI GOP is the real-PC route
     (`docs/real-pc-boot-plan.md`). This is the QEMU route and costs one
     small driver.
5. **The kernel console scales too.**
   - `display.scale=auto|1|2` uses the same rule.
   - The console pixel-doubles its 20 px glyphs and the cursor. That stays
     crisp because each source pixel becomes a 2x2 block, and the boot log
     is readable on a 4K monitor.

## Stages

| Stage | Work | Verification |
|---|---|---|
| **D1** Mode | `kernel/src/display/bochs.rs`: detect, validate, set, read back. `display.mode`/`display.scale` parsed in `display/modecfg.rs` (pure). `console::reinit`, `display::init`, `limits::init_for_machine`, `mouse::set_bounds` after a switch. Console glyph and cursor scale. `LAZYOS_DISPLAY_MODE` in `build_support/os_image.rs`. | `display_suite`: parser (hostile text, bounds, duplicates); mode validation against VRAM; a real switch 1280x720 <-> 2560x1440 with register readback and pixel writes at the far corner; a soak of 200 switches; console scale arithmetic. Screenshot of the console at 1440p. |
| **D2** Compositor | `GetOutput` in `idl/display.midl` (regenerated). `xuid` scale from `sys/ui/scale`/`auto`. `theme.rs` metrics become `scale()`-multiplied accessors. `render.rs`, `anim.rs`, `powerfeed.rs`, `drag.rs`, `icons.rs` literals scaled. A 26 px Droid atlas pair chosen by `typeface::set_scale`. Cursor drawn at scale. | Session screenshots at 1440p: title bars, buttons, Alt+Tab, power feed, drag label all 2x and crisp. Hit-testing of buttons and resize grips at their new size. |
| **D3** xui apps | `LazyOSBackend`: `dpi = 96 * scale` per window, size hints and `DOUBLE_CLICK_DISTANCE`/`PAINT_SPILL` scaled, `window_size` in logical units for owner mode. Terminal cell metrics from the shaped font size. Dashboard family (`dashboard.rs`, sysmon, fabricmon, devices, compact) and LazyShell (taskbar, menu, desktop icons, which pick the 128 px icon) multiply their px constants by the scale. | `core_apps.json` at 1440p: every core app opens at its 720p logical size, text and geometry agree, taskbar and menu clickable. `cargo test` for `lazyshell` with scale 1 and 2. |
| **D4** Launchers | `run_demo.py --hidpi` (sets `LAZYOS_DISPLAY_MODE=2560x1440`); a *HiDPI (1440p, 2x)* checkbox on the GUI Simple tab, a mode field on the Advanced tab, `catalog.py` + tests. Screenshot tools need nothing (they read whatever mode the guest set). | `test_catalog.py`; a desktop session screenshot judged with `pngstats.py --expect-width 2560 --expect-height 1440`. |

## Status

D1 to D4 landed together.
- `python tools/run_demo.py --desktop --hidpi` boots a 2560x1440 screen with
  the desktop at scale 2. Serial shows `display: mode 2560x1440`, then
  `display: console scale 2` and `XUID:SCALE:2 setting=auto
  screen=2560x1440`.
- `tools/screenshot/examples/hidpi_apps.json` opens Settings, System
  Monitor, Files, Config and the Editor. Every one lays out at its 720p
  logical size, and its text is rasterized at 2x.
- `display_suite` (`display_mode_*`) covers the parser, the scale rule,
  the adapter checks, real switches and a 200-switch soak.

- The window animation used to recompose and present the whole trail's
  bounding box every frame, and that box has four times the pixels at 2x.
  Large windows took 24 to 97 ticks per phase instead of the paced 12. It
  now restores and presents only the outline strips, so every phase takes
  12 ticks at either scale.

How code reaches 2x:

| Code | How it scales |
|---|---|
| xui widgets, `Dip` layouts, arrange | `96 * scale` DPI, nothing else |
| painters written in pixel constants (dashboards) | `xui_app::hidpi::design_bounds`: the canvas transform scales geometry, text keeps its DPI size |
| widget layouts in pixel constants (Network, Net Tools, Installer, Settings, Config) | their `rect()` helper multiplies by the layout scale |
| LazyShell | model in design pixels; `Ctx::to_screen`/`to_design` at the protocol and pointer edges |
| `xuid` chrome | `theme::px` and scaled metric accessors; 26 px atlases |

## Out of scope (follow-ups)

- Live scale change (`DpiChanged`, relayout) and fractional scales (1.5x for
  a 1080p-on-4K look).
- Compositor upscaling of scale-unaware raw clients (`xdemo`, `dragdemo`,
  `shellprobe`). These demos stay 1x and small.
- Scaling of the kernel mux's fallback windows (desktop images bind the
  display before the mux paints).
- Cosmetic 1-2 px constants inside upstream `xui-core` widgets (corner radii,
  separator widths), which belong upstream in `va1erian/xui`.
- Real-hardware high-resolution modes (UEFI GOP, `real-pc-boot-plan.md` H1).
