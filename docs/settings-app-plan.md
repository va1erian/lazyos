# Settings app plan

A desktop Settings (configuration panel) app: a vertical `IconView` of sections
on the left, the active section on the right. Settings persist through `confd`
onto the ext2 data volume.

## Scope

| Parameter | Mechanism |
|---|---|
| Date and time, timezone | `timed` (`SetTime`, `SetZone`, `Now`) |
| Keyboard layout (US, FR only) | new `kbd_layout` syscall, applied at boot by `init` |
| Background, active/inactive window, taskbar, accent colors | runtime `Theme` in `xuid`, driven by confd |
| Dark / light theme | preset that resolves into the same `Theme` |
| Extras | animations toggle, 12/24-hour clock, show seconds, per-section reset, About page |

## Architecture

**confd is the single source of truth.** Keys live under `sys/` (world-readable,
uid-0 writable, emits `system/confd/changed/{path...}`).

| Key | Type |
|---|---|
| `sys/ui/mode` | string `dark` / `light` |
| `sys/ui/bg`, `sys/ui/accent`, `sys/ui/title_active`, `sys/ui/title_inactive`, `sys/ui/taskbar` | u64 0xRRGGBB |
| `sys/ui/anim` | bool |
| `sys/input/layout` | string `us` / `fr` |
| `sys/time/zone` | string (existing) |
| `sys/time/clock24`, `sys/time/show_seconds` | bool |

Missing key means the compiled-in default.

**Persistence.** confd store directory order: `/system/confd`, `/data/confd`,
`/tmp/confd`. Falling back to `/tmp` keeps the service `degraded` and the app
shows a "settings will not survive reboot" banner.

**Theme.** xuid replaces its color consts (`user/src/bin/xuid/theme.rs`) with a
`Theme` struct loaded from `sys/ui/*`, re-read on the confd change topic.
`GetTheme` in `idl/display.midl` gains `mode` and `accent` (midlc regenerated).

**Keyboard.** Syscall `kbd_layout(op, arg)`; set needs `CAP_SYS_ADMIN`. `init`
applies `sys/input/layout` after confd is up.

**App.** `xui-app/crates/settings` (host-testable model + reducer + schema) and
`xui-app/src/bin/settings.rs`. Sidebar is an `IconView` with a `SectionsModel`
(Appearance, Windows & Taskbar, Time & Date, Keyboard, About). Color input is
preset swatches plus RGB sliders. Registration: `xui-app/Cargo.toml`,
`tools/xui/build.py`, `build.rs`, `user/src/bin/init/apps.rs`,
`user/src/bin/xuid/menu.rs`.

## Phases

1. **Persistence**: confd falls back `/system` -> `/data` -> `/tmp`; tests incl. soak.
2. **Runtime theme in xuid**: `Theme` struct, confd load + subscribe, `GetTheme` extension, `THEME:*` markers.
3. **Keyboard syscall**: syscall, init apply, `keyboard_suite` tests + soak.
4. **App scaffold**: crate, window, `IconView` sidebar, registration, menu entry, `SETTINGS:UP:PASS`.
5. **Sections**: Appearance, Windows, Time, Keyboard, About.
6. **Polish and docs**: reset buttons, persistence banner, doc updates.

## Verification

- Host: `cargo test --manifest-path xui-app/Cargo.toml --workspace --lib`, `cargo test -p confd`.
- Kernel: `python tools/test/run.py --accel none`.
- Visual: `tools/screenshot/examples/xui_settings.json` session; inspect PNGs, `pngstats.py`.
- CI: clippy `-D warnings`, `cargo fmt`, `midlc --check`.

## Open risks

- Whether `wallclock::set` writes the CMOS RTC (time may reset on reboot).
- xui toolkit is pinned to an external rev; use `Custom` painters instead of bumping it.
- `sys/` theme is machine-wide; per-user themes need `user/<uid>/` topic coverage.
- No manifest enforcement yet; the app will later need `CAP_SYS_TIME`, `CAP_SYS_ADMIN`, confd write.
