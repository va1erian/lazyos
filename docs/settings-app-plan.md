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
| `sys/ui/wallpaper` | string: absolute path of a PNG or JPEG desktop picture (absent: the plain `sys/ui/bg` colour); read by LazyShell, see `docs/shell-plan.md` |
| `sys/input/layout` | string `us` / `fr` |
| `sys/time/zone` | string (existing) |
| `sys/time/clock24`, `sys/time/show_seconds` | bool |

Missing key means the compiled-in default.

**Persistence.** confd store directory order: `/system/confd`, `/data/confd`,
`/tmp/confd`. Falling back to `/tmp` keeps the service `degraded` and the app
shows a "settings will not survive reboot" banner.

**Per-user theme.** Every account (any uid but 0, which no session runs
as) may shadow each `sys/ui/<name>` with `user/<uid>/ui/<name>` (phase 7);
the machine default changes through Appearance's "Make this the default for
everyone", which asks an administrator (docs/accounts-plan.md).

**Theme.** xuid replaces its color consts (`user/src/bin/xuid/theme.rs`) with a
`Theme` struct loaded from `sys/ui/*`, re-read on the confd change topic.
`GetTheme` in `idl/display.midl` reports `mode` and `accent`; the stock-widget
xui apps (Config, Editor, Files, Installer, Paint) fetch it at start-up
(`LazyOSBackend::desktop_theme`) and map it onto xui's light/dark `Theme` with the
desktop accent (`theme_ops::xui_theme`); Settings re-themes itself live. Chrome
text is picked from the surface it sits on (`uitheme::text_on`), so a dark
accent title bar keeps light text in light mode.

**Time & Date.** The clock and zone go through `timed` (`SetTime` needs
`CAP_SYS_TIME`; `wallclock::set` writes the CMOS RTC back, so a set time survives
a reboot). The taskbar clock format is `sys/time/clock24` and
`sys/time/show_seconds`, which the LazyShell taskbar (issue #157) re-reads
with the theme keys every few seconds and lays its clock slot out again.

**About.** Version (`uname`), uptime (`sysinfo` ticks) and the confd store
directory and persistence (`Info`).

**Keyboard.** UI only. The section shows a two-item single-select `ListView` (`English (US)`, `Français (AZERTY)`; `ListView::new(ui, bounds, &[...])`, `.selection_mode(Single)`, `.on_select(...)`) and a test text field and writes `sys/input/layout`. No kernel syscall or `init` wiring is needed: `inputd` (merged from `docs/input-plan.md`) already reads confd `sys/input/layout` and applies a change live, so the section shows the stored value and the effect is immediate.

**App.** `xui-app/crates/settings` (host-testable model + reducer + schema) and
`xui-app/src/bin/settings.rs`. Sidebar is an `IconView` with a `SectionsModel`
(Appearance, Windows & Taskbar, Time & Date, Keyboard, About). Color input is
preset swatches plus RGB sliders. Registration: `xui-app/Cargo.toml`,
`tools/xui/build.py`, `build.rs`, `user/src/bin/init/apps.rs` and the core
package's menu `category` (the start menu is LazyShell's since issue #157; it
was `user/src/bin/xuid/menu.rs`).

## Phases

1. **Persistence** (done): confd falls back `/system` -> `/data` -> `/tmp` (`libs/confd/src/dir.rs`, host-tested). Still to verify on a booted image with a data disk.
2. **Runtime theme in xuid** (done): `libs/uitheme` + `xuid/themefeed.rs`; verified live by `tools/screenshot/examples/theme_live.json`. `GetTheme` reports `mode` and `accent` (done).
3. **Keyboard** (done, UI only): `inputd` applies `sys/input/layout`.
4. **App scaffold** (done): `xui-app/crates/settings` + `xui-settings` binary, `IconView` sidebar, registered in `tools/xui/build.py`, `build.rs`, `init/apps.rs`, `xuid/menu.rs`.
5. **Sections** (done): Appearance, Windows (full `ColorPanel`), Keyboard, Menu, Hidden apps, Time & Date, About. Hidden apps (issue #509) writes `user/<uid>/menu/hidden/<id>` per app over the machine default `sys/menu/hidden/<id>` (`libs/deskmenu/src/hidden.rs`); LazyShell leaves those apps out of the start menu, and they still launch and open files.
6. **Polish** (done): animations toggle (`sys/ui/anim` gates `xuid`'s zoom), 12/24-hour and seconds, title contrast.
7. **Per-user theme** (done, issue #407): for every account (uid 0 aside), every theme key `sys/ui/<name>` (the desktop picture included) may be shadowed by `user/<uid>/ui/<name>` (`uitheme::user_key`); the user key wins when present. Settings edits the user's copy (`settings::user_theme::UserTheme`; Reset deletes the user's keys, and "Make this the default for everyone", `user_theme::make_default`, writes the keys that differ to `sys/ui/*` through `elevd`, one approval each, then drops the user's copy). `xuid` paints the chrome for the uid that runs the shell (`ThemeFeed::follow_user`, from the shell's `Subscribe`) and follows `user/<uid>/confd/changed/ui/#`; LazyShell overlays the same keys. confd announces `user/<uid>/` changes in the kernel's per-uid topic namespace (`kernel/src/ipc/topics/private.rs`: only that uid and root may subscribe).
8. **Open**: the system-stat dashboards (sysmon, fabricmon) and the Terminal still paint a fixed light palette.

Verified by `tools/screenshot/examples/xui_settings.json` (serial markers `SETTINGS:UP:PASS`, `SETTINGS:MSG:*`, `THEME:APPLIED`, `SETTINGS:CLOSE:PASS`).

**Toolkit dependency.** `ColorPicker` ignored clicks when not at its container's top-left (event coordinates are node-local, `Ui::bounds` is parent-relative). Fixed upstream in `va1erian/xui` (#248, `58c1a6e`); `xui-app` is pinned to that rev.

## Verification

- Host: `cargo test --manifest-path xui-app/Cargo.toml --workspace --lib`, `cargo test -p confd -p uitheme -p timezone`.
- Persistence: `fs_ext2_confd_store_*` run `confd` over the real ext2 driver (remount, a power cut at every write of a commit, a 120-generation soak with a block-leak check).
- Kernel: `python tools/test/run.py --accel none`.
- Visual: `tools/screenshot/examples/xui_settings.json` and `xui_settings_time.json` (zone, 12-hour clock with seconds, set time, light mode title contrast, animations off; serial `SETTINGS:MSG:Time(*)`) sessions; inspect PNGs, `pngstats.py`. `xui_settings_hidden.json` hides Paint, shows the start menu without it, then resets (serial `SETTINGS:MSG:Hidden(*)`). Per-user theme: `user_session_setup.json` then `user_session_login.json` on the same desktop image (`LAZYOS_DESKTOP=1 LAZYOS_XUI_AUTOSTART=term LAZYOS_UI_PROBE=1`): the admin sets `user/1000/ui/mode` light and hides apps, then a graphical login as `user` shows the light desktop on the dark machine theme (`THEME:USER uid=1000`), Paint (hidden by the admin for itself) in its menu and Calculator hidden by the machine default. `confd_user_topics.json` (`LAZYOS_SERVICES=1`): `user` is refused another uid's topics and receives its own.
- CI: clippy `-D warnings`, `cargo fmt`, `midlc --check`.

## Open risks

- xui toolkit is pinned to an external rev; use `Custom` painters instead of bumping it.
- The UI scale (`sys/ui/scale`) stays machine-wide: the compositor fixes it at start-up.
- No manifest enforcement yet; the app will later need `CAP_SYS_TIME`, `CAP_SYS_ADMIN`, confd write.
