# xui-explorer

A small, **explorer-style** file manager built on xui's portable widget layer:
a window browses folders in place, like a web browser, with a toolbar, an
address bar and a history. It is written as a portable core with every
OS-specific operation behind two small traits, so it can be embedded by an
operating system with its own filesystem and shell — in particular **LazyOS**.

- `xui-core` only (no platform dependency, no `unsafe`).
- Opening a folder (double-click, Return, the address bar, Up) replaces the
  view in the same window; files go to the OS default handler. The window
  title is the folder's name.
- The toolbar: **Back**, **Forward** (a per-window history, `model::History`),
  **Up** (the folder we came from is selected), the **address bar** (a typed
  path, relative to the folder, `~` for home; Return goes, Escape reverts),
  **Sort** (a menu of Name/Size/Type/Modified and Descending), the folder's
  **Properties**, and the switch between the **icon view** and the
  **details view** (an `IconView` and a `ListView` stacked in one place, with
  sortable Name, Size, Type and Modified columns; a header click sorts, a
  second click reverses). Folders always come first.
- The context menu: Open, **Open in New Window** (the one way to a second
  window, which inherits the view and the sort), Copy, Paste, Delete,
  Properties (of the item, or of the folder on empty space), Refresh.
- Shortcuts: Alt+Left/Right/Up, Backspace (Up), Ctrl+L / Alt+D / F4 (the
  address bar), F5, Delete, Ctrl+C, Ctrl+V, Alt+Enter. While the address bar
  has the focus, the keys that edit text stay with it
  (`Explorer::set_focus_probe` tells the windows which widget has it).
- A `StatusBar` summary; confirm and Properties dialogs (`TaskDialog`/`Dialog`).
- Copy, paste and the selection go through an optional third seam,
  `Session` (below); `Explorer::reveal_root` opens a folder with one item
  selected.
- Tile icons: the multi-colour Global Village set when the opt-in
  `village-icons` feature is on (the LazyOS Files app enables it), else the
  single-colour Lucide fallback, which the details rows always use. The
  `xui-icons` crate stays in the xui repository and is a git dependency.

## Icons

With the `village-icons` feature enabled (as the LazyOS Files app does), tiles
are drawn with the `xui-icons` Global Village set, classified by kind and
extension (folder, image, music, archive, document). On a light theme the set's
own `Palette::GLOBAL_VILLAGE` is used; on a dark theme only its near-black ink
is retinted to a pale periwinkle, so the outlines stay visible, and both
palettes are built once. Leave the feature off for the single-colour Lucide
fallback (the crate default); the classification is shared by both paths.

## Many windows

A window navigates, so the shell does not map folders to windows. It keeps
what each open window shows (`shell::ViewState`: its folder, the view on
screen, the selection and a `Proxy`); after a delete or a paste it asks every
window at or below the folder to refresh, and a window whose folder is gone
climbs to the nearest folder that still exists.

## The seam

Everything OS-specific is in [`src/platform.rs`](src/platform.rs):

```rust
trait Platform {
    fn list(&self, dir: &Path) -> io::Result<Vec<RawEntry>>;
    fn metadata(&self, path: &Path) -> io::Result<Meta>;
    fn remove(&self, path: &Path, recursive: bool) -> io::Result<()>;
    fn home(&self) -> Option<PathBuf>;
}

trait Launcher {
    fn open(&self, path: &Path) -> io::Result<()>;
}
```

The trait signatures mention only `Path`/`PathBuf`, `OsString`, `io::Result`
and `SystemTime` — no `xui`, no `std::fs`, no `cfg`. The rest of the crate is
portable:

- `model/` is pure logic: sorting, `Listing` and its `IconModel`/`ListModel`
  view, the `History`, the address bar's path resolution, status summaries,
  the Properties rows, size and UTC time formatting and path helpers. It
  assumes nothing about drive letters or separators.
- `window.rs` (and `window/`: `chrome` the layout and menus, `nav` the
  navigation, `view` the two views and sorting, `keys` the shortcuts,
  `actions` delete and Properties, `clipboard`) is the per-window `App`: it
  owns the widgets and reacts to their messages; it never touches the
  filesystem directly.
- `shell.rs` holds the `Platform`, the `Launcher`, the `Session` and what each
  open window shows.

`testing::MemPlatform` is an in-memory `Platform` used by the tests, so no test
touches the real disk.

## Porting to LazyOS

Implement three things and nothing else:

1. **`Platform`** over LazyOS' filesystem. `metadata` must be symlink-aware (use
   the equivalent of `symlink_metadata`) and `remove` must delete a symlink as a
   link; `home` may return `None`.
2. **`Launcher`**, or a no-op if LazyOS has no default handler yet. The explorer
   reports a launcher error in the status bar, so a no-op that returns
   `io::ErrorKind::Unsupported` is honest.
3. **A `xui_core::backend::Backend`** (LazyOS' own), and start the app with
   `xui_core::run_app`.

Then build windows with `Explorer::new(platform, launcher).open_root(ui, path)`
and the portable crate is unchanged. The `std-platform` feature (on by default)
is the only part that uses `std::fs` and `#[cfg(...)]`; turn it off for a
no-std-ish target. `village-icons` is off by default; the LazyOS Files app
enables it so the boot image ships the coloured set (and `xui-icons` in its
dependency graph).

## Opening animation hint

`Launcher::hint_open_origin(window, tile)` is an optional method (default: do
nothing) called right before "Open in New Window" opens a folder window.
`window` is the backend's raw id of the source window and `tile` the tile's
approximate edge in device pixels (`shell::open_tile_px`, 64 dip). The backend
centres a `tile`-sized square on the window's last pointer position, which is
where the context menu was opened. The LazyOS Files app forwards it to
`LazyOSBackend::hint_open_origin`, which sends `HintOpenOrigin` (display
protocol method 30) so `xuid` zooms the new window open from there instead of
the taskbar. It is cosmetic and best-effort.

## Known limits

- **No filesystem watching.** Refresh happens on `F5`, the context menu, and
  after the explorer's own delete and paste. A change made outside the
  explorer shows only after a refresh.
- **Listing runs on the UI thread.** A very large or slow directory blocks the
  window while it loads.
- **Multi-selection** is Ctrl/Shift+click and Shift+arrows only: no
  rubber-band and no Ctrl+A yet, and a press on a tile collapses a
  multi-selection before a drag (worked around with `Msg::RestoreSelection`);
  va1erian/xui#289 asks the views for them.
- **The icon view takes a new model while hidden** (`window/view.rs`,
  `set_models`): xui's `IconView::set_model` re-enters the layout with its
  state borrowed when its scrollbar appears or goes.
- The details view's context menu needs a row: a right click on its empty
  space shows nothing (use the folder Properties button or F5).
- Out of scope: rename, cut/move by keyboard, new folder, recycle bin,
  hidden-file toggle, search, a folder tree.

## The session seam (copy, paste, selection)

```rust
trait Session {
    fn copy(&self, paths: &[PathBuf]) -> io::Result<()>;
    fn paste_into(&self, dir: &Path) -> io::Result<Pasted>;
    fn selection_changed(&self, dir: &Path, paths: &[PathBuf]);
}
```

Every method has a default (copy and paste report `Unsupported`, the
selection goes nowhere), and `Explorer::new` uses `NoSession`; pass a real one
with `Explorer::with_session`. Copy (context menu or `Ctrl+C`) hands the
selection's absolute paths to `copy`; Paste (`Ctrl+V`) asks `paste_into` to
copy the clipboard's files into the window's folder, then refreshes every
window showing it and reports the count (or the failures) in the status bar.
Every selection change, and every window that opens, navigates or refreshes,
calls `selection_changed`. The LazyOS Files app implements it over `clipboardd`
(`text/uri-list`), `std_platform::copy_into` and the `session/<id>/selection`
topic (`xui-app/src/bin/files/session.rs`, issue #488).

## Checks

```bash
cargo fmt --all --check
cargo clippy -p xui-explorer --all-targets -- -D warnings
cargo test -p xui-explorer
```

The snapshot test writes
`target/snapshots/xui-explorer-{icons,details}-{light,dark}.png` headlessly,
with no window.

## Drag and drop

`std_platform::drop_into` is what a drop into a folder window runs: a drag
that started in this explorer **moves** items on the folder's volume and
copies the rest; Ctrl held at the drop copies and Shift moves. A drag from
another app copies unless Shift is held. Nothing goes into itself, a clash
gets `name (2)`, links stay links, an item dropped into its own folder is
left alone, and a move across volumes copies first and removes the original
only once the copy succeeded (`std_platform/transfer.rs`).
