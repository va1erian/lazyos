# Settings app plan

A desktop Settings (configuration panel) app: a vertical `IconView` of sections
on the left, the active section on the right. Settings persist through `confd`
onto the ext2 data volume.

## Scope

| Parameter | Mechanism |
|---|---|
| Date and time, timezone | `timed` (`SetTime`, `SetZone`, `Now`) |
| Keyboard layout (US, FR only) | UI only: writes `sys/input/layout` to confd; `inputd` applies it live |
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

**Keyboard.** UI only. The section shows a two-item single-select `ListView` (`English (US)`, `Français (AZERTY)`; `ListView::new(ui, bounds, &[...])`, `.selection_mode(Single)`, `.on_select(...)`) and a test text field and writes `sys/input/layout`. No kernel syscall or `init` wiring is needed: `inputd` (merged from `docs/input-plan.md`) already reads confd `sys/input/layout` and applies a change live, so the section shows the stored value and the effect is immediate.

**App.** `xui-app/crates/settings` (host-testable model + reducer + schema) and
`xui-app/src/bin/settings.rs`. Sidebar is an `IconView` with a `SectionsModel`
(Appearance, Windows & Taskbar, Time & Date, Keyboard, About). Color input is
preset swatches plus RGB sliders. Registration: `xui-app/Cargo.toml`,
`tools/xui/build.py`, `build.rs`, `user/src/bin/init/apps.rs`,
`user/src/bin/xuid/menu.rs`.

## Phases

1. **Persistence** (done): confd falls back `/system` -> `/data` -> `/tmp` (`libs/confd/src/dir.rs`, host-tested). Still to verify on a booted image with a data disk.
2. **Runtime theme in xuid** (done): `libs/uitheme` + `xuid/themefeed.rs`; verified live by `tools/screenshot/examples/theme_live.json`. `GetTheme` extension (`mode`, `accent`) still open.
3. **Keyboard** (done, UI only): `inputd` applies `sys/input/layout`.
4. **App scaffold** (done): `xui-app/crates/settings` + `xui-settings` binary, `IconView` sidebar, registered in `tools/xui/build.py`, `build.rs`, `init/apps.rs`, `xuid/menu.rs`.
5. **Sections**: Appearance, Windows (full `ColorPanel`), Keyboard done; **Time & Date** and **About** open.
6. **Polish and docs**: open (animations toggle, clock format options, About).

Verified by `tools/screenshot/examples/xui_settings.json` (serial markers `SETTINGS:UP:PASS`, `SETTINGS:MSG:*`, `THEME:APPLIED`, `SETTINGS:CLOSE:PASS`).

**Toolkit dependency.** `ColorPicker` ignored clicks when not at its container's top-left (event coordinates are node-local, `Ui::bounds` is parent-relative). Fixed in the xui repo (`ColorPicker: hit-test in node-local coordinates`); the pinned rev in `xui-app/Cargo.toml` and `xui-app/crates/*/Cargo.toml` must be bumped to a commit containing it for the swatch grids to respond.

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
