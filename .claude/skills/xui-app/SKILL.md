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
   python tools/xui/new_app.py notes --name "Notes" --description "Quick notes"
   ```

   It writes `xui-app/src/bin/<short>.rs` (a working app on a layout), the
   `[[bin]]`, the build and image lists (`tools/xui/build.py`,
   `build_support/xui_embed.rs`, `tools/run_demo.py`), the core package
   `xui-app/packages/<short>/` with icons, the GUI launcher entry and a session
   script `tools/screenshot/examples/xui_<short>.json`. It prints the
   follow-ups it cannot do: the package permissions (derive them from a run
   with `LAZYOS_LABEL_TRACE=1`, AGENTS.md "Packages and the label-policy
   trace") and the icon art.
2. Anything worth testing goes in a crate under `xui-app/crates/<name>/`
   behind a trait for the OS (`ConfigStore`, `System`): the bin only connects
   it. `crates/settings` and `crates/confd-editor` are the models. A crate runs
   and renders on the host; a bin is musl-only.

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

Read xui's `docs/cookbook.md` (copy-ready window shell, form, master/detail,
list with add/remove, settings page, live monitor). At the pinned revision it
is in cargo's checkout,
`~/.cargo/git/checkouts/xui-*/<first 7 of the rev>/docs/cookbook.md`; the
builders themselves are in `crates/xui-core/src/arrange/` there.

- **Describe, don't place.** `column()`, `row()`, `grid([Track::Auto, Track::Fill(1)])`,
  `group("Title", layout)`, `tabs().page(..)`, `split(a, b)`, `panel(layout)`
  (a card; `.plain()` for none), `scroll(layout)`; `absolute()` with
  `.at(x, y, w, h)` only for free positions. Mount with `ui.root(layout)`.
- **Widgets are builders:** `label`, `button(..).on_click(Msg::Save)`,
  `edit().on_change(Msg::Name)`, `checkbox(..).on_toggle(..)`,
  `combo_box(&[..]).on_select(..)`, `list().column(..).items(&[..])`,
  `radio_group(&[..]).on_select(..)`, `tree_view().rows(..)`,
  `color_picker(&colors)`, `color_panel()`, `icon_view_with(model)`,
  `toolbar().item_with_text(..)`, `menu_bar(|bar| ..)`, `status_bar(&[..])`.
- **Bind what you change:** a `Handle<W>` field, `.bind(&handle)`, then
  `handle.get()` in `update`.
- **Size only what must not be natural:** `.fill(1)`, `.width(n)`,
  `.height(n)`, `.size(w, h)`, `.max_width(n)`, `.align_x(..)`/`.align_y(..)`.
  Values are design pixels; HiDPI scales them.
- An option with no builder method goes through `.then(|w| w.option(..))`;
  a widget with no builder (an app's own painted `Control`, a `xui-code-editor`
  `Editor`) through `build(|ui| ..)`, with a small `Placeable` impl if the
  layout must size it.
- Never hand-place with `Rect`s: the rect constructors are crate-private in
  xui, and a layout re-flows on resize, DPI and theme changes.

## 3. Verify headlessly first

Run these in seconds on the host before any image build:

- **Offscreen renders:** `xui_canvas::snapshot::render_with(Snapshot::new(w, h), build, drive)`
  builds the real window, drives it with messages and returns the image. Render
  both themes, save to `xui-app/target/snapshots/`, and Read the PNGs.
  `crates/settings/tests/window.rs` is the model (fonts registered, a watchdog
  thread so a hang fails).
- **Layout report:** `ui.layout_report()` lists what each layout placed and
  warns about overlaps and text placed too narrow.
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
LAZYOS_DESKTOP=1 LAZYOS_RESET_OS=1 cargo build
python tools/screenshot/qemu_session.py --image target/lazyos.img \
    --out shots/notes --script tools/screenshot/examples/xui_notes.json
```

Check `shots/notes/summary.json` and the serial log for the markers, and Read
the PNGs. Sessions click at fixed coordinates: when a layout moves, re-run
the app's sessions and fix their coordinates in the same change.

## 5. Before the PR

- `cargo fmt`, musl `clippy -D warnings` for every bin and crate touched,
  host crate tests, WSL lib/bin tests, the app's QEMU sessions.
- A light and a dark render (or session shot) in the PR.
- Moving xui: bump every `va1erian/xui` rev together (`xui-app`, `doom`,
  `lazyrad-os` and its `[patch]`), then `python tools/xui/check_pin.py`.