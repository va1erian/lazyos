# xui-explorer

A small, **spatial** file explorer built on xui's portable widget layer: one
window is one open folder, with no tree, address bar or in-window navigation.
It is written as a portable core with every OS-specific operation behind two
small traits, so it can be embedded by an operating system with its own
filesystem and shell — in particular **LazyOS**.

- `xui-core` only (no platform dependency, no `unsafe`).
- Folders open their own window (or reuse the one already showing them); files
  go to the OS default handler.
- `IconView` listing (folders first, then files), a `StatusBar` summary, a
  context `Menu`, Copy/Paste/Delete/Properties actions (`TaskDialog`/`Dialog`)
  and `Ctrl+C` / `Ctrl+V` / `Delete` / `Alt+Enter` / `F5` shortcuts.
- Copy, paste and the selection go through an optional third seam,
  `Session` (below); `Explorer::reveal_root` opens a folder with one item
  selected.
- Tile icons: the multi-colour Global Village set when the opt-in
  `village-icons` feature is on (the LazyOS Files app enables it), else the
  single-colour Lucide fallback. The `xui-icons` crate stays in the xui
  repository and is a git dependency. Opening a folder shows its open icon for
  two seconds, then reverts.

## Icons and the open-folder flash

With the `village-icons` feature enabled (as the LazyOS Files app does), tiles
are drawn with the `xui-icons` Global Village set, classified by kind and
extension (folder, image, music, archive, document). On a light theme the set's
own `Palette::GLOBAL_VILLAGE` is used; on a dark theme only its near-black ink
is retinted to a pale periwinkle, so the outlines stay visible, and both
palettes are built once. Leave the feature off for the single-colour Lucide
fallback (the crate default); the classification is shared by both paths.

Opening a folder (double-click or Enter) flags it in a per-window list of
`(name, deadline)` entries and shows its open icon for 2000 ms; several folders
can flash at once, each with its own deadline, and re-opening one restarts it.
A single repeating timer prunes expired names and repaints, and stops as soon
as nothing is flashing, so an idle window has no timer. The flash is visual
only — it never changes the opening behaviour, selection, focus or scroll — and
survives a refresh by name, dropping a folder that was deleted or renamed.

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

- `model/` is pure logic: sorting, `Listing`, the `IconModel` view, status
  summaries, the Properties rows, size and UTC time formatting and path
  helpers. It assumes nothing about drive letters or separators.
- `window.rs` is the per-window `App`: it owns the widgets and reacts to their
  messages; it never touches the filesystem directly.
- `shell.rs` holds the `Platform`, the `Launcher` and the path-to-window
  registry that makes an already-open folder a no-op and lets a delete close
  the windows below it.

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
nothing) called right before a folder window opens because a tile was activated
(not when the folder is already open, and not for files). `window` is the
backend's raw id of the source window and `tile` the tile's approximate edge in
device pixels (`shell::open_tile_px`, 64 dip). The explorer has no item-rect API
from `IconView`, so it does not compute the tile's rectangle: the backend
centres a `tile`-sized square on the window's last pointer position, which is
the tile that was just double-clicked. The LazyOS Files app forwards it to
`LazyOSBackend::hint_open_origin`, which sends `HintOpenOrigin` (display
protocol method 30) so `xuid` zooms the new window open from the tile instead of
the taskbar. It is cosmetic and best-effort: keyboard activation with the
pointer outside the window sends no hint.

## Known limits (v1)

- **No filesystem watching.** Refresh happens on `F5`, the context menu, and
  after the explorer's own delete. A change made outside the explorer shows only
  after a refresh.
- **Listing runs on the UI thread.** A very large or slow directory blocks the
  window while it loads.
- **No raise/focus API.** Opening a folder that is already open reports
  `"already open"` in the status bar instead of bringing that window forward.
- Out of scope: rename, cut/move by keyboard, new folder, recycle bin,
  hidden-file toggle, sorting options, search, navigating up.

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
Every selection change, and every window that opens or refreshes, calls
`selection_changed`. The LazyOS Files app implements it over `clipboardd`
(`text/uri-list`), `std_platform::copy_into` and the `session/<id>/selection`
topic (`xui-app/src/bin/files/session.rs`, issue #488).

## Checks

```bash
cargo fmt --all --check
cargo clippy -p xui-explorer --all-targets -- -D warnings
cargo test -p xui-explorer
```

The snapshot test writes `target/snapshots/xui-explorer-{light,dark}.png`
headlessly, with no window.

## Drag and drop

`std_platform::drop_into` is what a drop into a folder window runs: a drag
that started in this explorer **moves** items on the folder's volume and
copies the rest; Ctrl held at the drop copies and Shift moves. A drag from
another app copies unless Shift is held. Nothing goes into itself, a clash
gets `name (2)`, links stay links, an item dropped into its own folder is
left alone, and a move across volumes copies first and removes the original
only once the copy succeeded (`std_platform/transfer.rs`).
