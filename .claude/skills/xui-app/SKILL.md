---
name: xui-app
description: Start, build and verify a LazyOS desktop app from a known-good template. Use when adding a new app (a dialog, a settings pane, a monitor, an editor) or changing an xui-app window in lazyOS: it picks Rhai (a LazyRAD form) or Rust (an xui-app bin), scaffolds it, lays it out with xui's builders, and verifies it headlessly before any QEMU session.
---

# A LazyOS desktop app

Two kinds of desktop app ship in lazyOS: LazyRAD forms (`.lfm` layout plus
`.rhai` script, run by `lrplay`) and Rust programs on xui (`xui-app`). Pick
one first, then follow its path. Verification is the same order for both:
host tests and offscreen renders first, QEMU last.

## 1. Rhai or Rust

Default to **Rhai** for utility-class apps: dialogs, settings panes, monitors,
small tools that read and write `confd` or call a Messenger service. They need
no Rust build, the IDE edits them, and `sys::*` (generated from `idl/`) gives
them every service.

Write **Rust** only when the app needs it:

- an editor, Paint, a browser: a custom painter or a large document model;
- heavy data, its own threads, or a Rust library (archives, litehtml, NetSurf);
- an app the image boots before LazyRAD is installed (Terminal, LazyShell).

## 2a. A Rhai app

1. Start from a working project: `lazyrad-os/samples/messenger/`
   (`messenger.lrp` names the forms, `main_form.lfm` lays them out,
   `main_form.rhai` handles the events). Copy it, rename the `.lrp` and its
   `name`.
2. Edit it in the LazyRAD IDE (`python tools/run_demo.py --lazyrad`) or by
   hand; script Messenger calls through `sys::<service>::...`
   (`libs/rhai-lazy/api/`, `docs/rhai/msg.md`).
3. Package it: *File -> Make LazyOS App* in the IDE, or on the host
   `python tools/lazyrad/package.py --project <dir> --out <name>.lzp --system-name user.<you>.<name>`.
   The manifest's permissions are derived from what the scripts call.
4. Host test: `cd lazyrad-os && cargo test` runs real forms offscreen
   (`tests/modplayer.rs` is the model for a form test).

## 2b. A Rust app

1. Scaffold it in one command:

   ```bash
   python tools/xui/new_app.py notes --name "Notes" --description "Quick notes" --category accessories
   ```

   It writes `xui-app/src/bin/<short>.rs` (a working app on a layout), the
   `[[bin]]`, the build and image lists (`tools/xui/build.py`,
   `build_support/xui_embed.rs`, `tools/run_demo.py`), the core package
   `xui-app/packages/<short>/` with icons, the GUI launcher entry and a session
   script `tools/screenshot/examples/xui_<short>.json`. It prints the
   follow-ups it cannot do: the package permissions (derive them from a run
   with `LAZYOS_LABEL_TRACE=1`, AGENTS.md "Packages and the label-policy
   trace") and the icon art. The art is a placeholder (`AppWindow` on teal) in
   `xui-app/crates/app-icons/src/lib.rs`: pick a Lucide glyph and tile colour
   there and redraw (`cargo run -p app-icons` in `xui-app/`).
2. Anything worth testing goes in a crate under `xui-app/crates/<name>/`
   behind a trait for the OS (`ConfigStore`, `System`): the bin only connects
   it. The scaffolder writes only the bin, so add the crate by hand: copy the
   layout of `crates/calc` (the smallest complete example: an engine with unit
   tests, a `build` function for the window, `tests/window.rs` with fonts,
   watchdog and both-theme renders) or `crates/settings`, add it to
   `[workspace] members` and `[dependencies]` in `xui-app/Cargo.toml`, and
   have the bin call its `build`. A crate runs and renders on the host; a bin
   is musl-only.
3. The generated bin quits on a bare `q`, which suits a window with no text
   field; drop that binding in one that has one.

### Startup and serial evidence

`xui_app::launch::run` is the whole `main`: it connects (display owner or
`xuid` client), opens the window in the desktop's theme, builds the app,
runs it, releases the display and exits, printing `<MARKER>:BIND:FAIL:<errno>`
or `<MARKER>:RUN:FAIL:<error>` when it cannot.

```rust
fn main() {
    launch::run("NOTES", "Notes", WINDOW, |ui, backend| {
        backend.on_first_frame(|| println!("NOTES:UP:PASS"));
        let app = Notes::default();
        ui.root(/* the layout */)?;
        ui.on_close(|| Some(Msg::Quit));
        Ok(app)
    })
}
```

Markers are `<MARKER>:<STEP>:PASS|FAIL[:detail]`, printed once per event the
session waits on (`UP`, `APPLY`, `QUIT`, ...). Printing each message in
`update` (`println!("SETTINGS:MSG:{msg:?}")`) lets a session confirm a click
landed (`until`) instead of sleeping.

### Layouts and builders

The API is whatever the **pinned** xui revision has (the `rev` in
`xui-app/Cargo.toml`), not xui's `main`. Read it in cargo's checkout,
`~/.cargo/git/checkouts/xui-*/<first 7 of the rev>/`: the builders are in
`crates/xui-core/src/arrange/` (`mod.rs`, `containers.rs`, `widgets.rs`,
`build.rs`), the widgets and their options in `crates/xui-core/src/widget/`
and `docs/widgets.md`. The newer `docs/cookbook.md` exists only on xui's
`main` and uses builders the pin lacks; to read it anyway,
`git -C <checkout> show origin/HEAD:docs/cookbook.md` (a plain `git clone`
of xui fails on Windows with "Filename too long": add
`-c core.longpaths=true`). The working apps in `xui-app/crates/*/src` are the
best examples of what compiles at the pin.

What the pin (48e504e) has:

- **Containers:** `column()`, `row()`, `grid([Track::Auto, Track::Fill(1)])`,
  each with `.gap(n)`, `.padding(n)`, `.align(..)`, `.justify(..)`,
  `.child(..)`, `.children((a, b, ..))`; `group("Title", layout)`;
  `tabs().page("Title", layout).on_change(..)`; `spacer()`. Mount with
  `ui.root(layout)`, or `ui.mount_in(container, layout)` for a page inside a
  container you hold. There is no `panel`, `scroll`, `split` or `absolute`
  builder.
- **Widget builders:** `label(..)` (`.title()`, `.caption()`),
  `button(..).on_click(msg)` (`.on_click_with(..)`), `hyperlink`,
  `checkbox(..).checked(b).on_toggle(..)`, `toggle_button`,
  `edit().text(..).placeholder(..).password().on_change(..)`,
  `multiline_edit`, `number_field`, `slider`, `combo_box(&[..]).on_select(..)`,
  `progress().value(n)`, `separator()`, `status_bar(&[..])`,
  `list().column(..).column_right(..).on_select(..).on_activate(..)` (fill its
  rows through the bound `ListView`).
- **Everything else** (radio group, tree view, icon view, toolbar, colour
  picker, scroll view, a menu bar, an app's own painted widget) has no
  builder: create it in `build(|ui| Widget::new(ui, Rect::default(), ..))`
  and the layout places it (it sets the real bounds); give it a small
  `Placeable` impl if its natural size matters.
  `xui-app/crates/settings/src/menu_page.rs` (`ListView`) and
  `crates/calc/src/display.rs` (a painted widget) show both.
- **Bind what you change:** a `Handle<W>`, `.bind(&handle)`, then
  `handle.get()` (keep the `Rc<W>` in your app).
- **Options with no builder method** go through `.then(|w| w.option(..))`,
  e.g. `button("=").then(|b| b.primary())`.
- **Size only what must not be natural** (on any entry, `LayoutExt`):
  `.fill(weight)`, `.fixed(n)`, `.min(n)`, `.width(n)`, `.height(n)`,
  `.max_width(n)`, `.max_height(n)`, `.align(Align::..)`, `.span(columns)` in a
  grid. Values are design pixels; HiDPI scales them.
- There is no layout report at the pin: check placement in the offscreen
  renders, and with a test that clicks where you expect a control and asserts
  the message it raised (`crates/calc/tests/window.rs`).
- Never hand-place with `Rect`s: a layout re-flows on resize, DPI and theme
  changes.

### Keys and focus

- Window-level shortcuts: `ui.on_key(|key, modifiers| -> Option<Msg>)`. It
  sees key codes (US positions), not typed characters, so a symbol such as
  `*` is `Shift` + the `8` key and depends on the keyboard layout; map the
  keypad codes too.
- A clicked button keeps the keyboard focus, so a window-level Enter also
  presses it. After handling a click, give the focus back to the widget that
  should own the keyboard (`widget.focus()`), as the calculator does.

## 3. Verify headlessly first

Run these in seconds on the host before any image build:

- **Offscreen renders:** `xui_canvas::snapshot::render_with(Snapshot::new(w, h), build, drive)`
  builds the real window, drives it with messages and returns the image. Render
  both themes, save to `xui-app/target/snapshots/`, and Read the PNGs.
  `crates/calc/tests/window.rs` and `crates/settings/tests/window.rs` are the
  models (fonts registered, a watchdog thread so a hang fails). Hover states
  stay lit in a render after a simulated click (no mouse-leave is sent).
- **Crate tests:** `cargo test --manifest-path xui-app/Cargo.toml -p <crate>`.
- **musl build and lints** (the bins only build for musl):
  `python tools/xui/build.py` builds every app; for a quick check run cargo
  with that script's environment (`build.build_env()` sets the bundled
  `rust-lld` on Windows): `cargo clippy --target x86_64-unknown-linux-musl --all-targets -- -D warnings`
  in `xui-app/`.

### musl-only tests on Windows: WSL

The `xui-app` lib and bin tests are Linux-only. Run them in WSL with a target
directory of their own, so the Linux build never fights the Windows one in
`xui-app/target`:

```bash
wsl -e bash -lc 'cd /mnt/e/repos/lazyos/xui-app && RUSTC_BOOTSTRAP=1 CARGO_TARGET_DIR=$HOME/xui-app-target cargo -Zbindeps test --locked --lib --bins'
```

## 4. QEMU for integration only

A session proves the app installs, starts under its label, talks to the real
services and answers clicks. Build a desktop image (BusyBox gives it a shell;
`LAZYOS_BUSYBOX` points at one, `tools/abi/busybox.py` builds it), then run the
app's session headless:

```bash
python tools/xui/build.py
LAZYOS_DESKTOP=1 LAZYOS_XUI_AUTOSTART=notes LAZYOS_RESET_OS=1 cargo build
python tools/screenshot/qemu_session.py --image target/lazyos.img \
    --out shots/notes --script tools/screenshot/examples/xui_notes.json
```

Check `shots/notes/summary.json` and the serial log for the markers, and Read
the PNGs.

- **What opens at boot:** nothing, unless `LAZYOS_XUI_AUTOSTART` lists it
  (`notes` above; `term` for a session that types into the Terminal). The
  core packages are installed before the desktop appears; start the session
  with `{"wait_for": "PKGD:PROVISION:DONE"}` and then the app's `UP` marker.
- **Where the window opens** (`place_window` in `user/src/bin/xuid/layout.rs`):
  in the first cell of a grid sized to the window, from the work area's
  top-left, that no open window covers; when every cell is covered, cascaded
  from the top-left by the number of open windows. At scale 1 on 1280x720,
  with nothing else open, the frame is at (48, 48) and the content at
  (50, 70) (2-pixel border, 22-pixel title bar). A session's coordinates
  depend on what else is open, so build the image with only the app
  autostarted, or open it from a known state.
- **Clicking:** move to the top-left corner first (several `[-300, -300]`
  moves) so relative moves start from (0, 0), then click with
  `"until": "<MARKER>:MSG:..."` and no retry where a double press would
  change the result (a digit, a toggle).
- **The start menu:** apps live in category submenus; the menu module
  (`xui-app/crates/shell/src/menu.rs`) documents the row coordinates.

Sessions click at fixed coordinates: when a layout moves, re-run the app's
sessions and fix their coordinates in the same change.

## 5. Before the PR

- `cargo fmt`, musl `clippy -D warnings` for every bin and crate touched,
  host crate tests, WSL lib/bin tests, the app's QEMU sessions.
- A light and a dark render (or session shot) in the PR.
- Moving xui: bump every `va1erian/xui` rev together (`xui-app`, `doom`,
  `lazyrad-os` and its `[patch]`), then `python tools/xui/check_pin.py`.
