# xui apps integration round: Editor, Paint and Files

The final round of [`docs/xui-apps-migration.md`](xui-apps-migration.md): the
three portable apps built in Track A and the system side merged in Track B are
wired together, driven by real QEMU sessions, and shipped in every desktop
image. This file is the record of what was run, what passed and what remains.

## What was done

### 1. Client key handling (`xui-app/src/backend/input.rs`)

* The key code is masked with `key & 0x00FF_FFFF` before matching, so a chord
  with modifier bits no longer falls through `Key::from_code`.
* `Event::KeyDown`/`KeyUp.modifiers` are filled from the packed bits (Shift 24,
  Ctrl 25, Alt 26, Super 27) in client mode. Owner mode keeps the existing
  modifier-key tracking (the compositor never forwards modifier keys).
* `KeyDown` still carries the letter for a Ctrl/Alt chord, but a `Char` event is
  suppressed while Ctrl or Alt is held, so Ctrl+C is a command and not the
  character `c`.
* `Delete` (`0x10C`), `Insert` (`0x10D`) and `F1..F12` (`0x110 + n - 1`) map to
  the `xui` `Key` vocabulary; arrows/Home/End/PgUp/PgDn were already named.
* `Tab`/`Shift+Tab` are the widget-focus keys; `PageUp`/`PageDown` are delivered
  to the focused widget (the Editor scrolls) instead of cycling focus.
* Host unit tests cover masking, every modifier bit, Ctrl/Alt suppressing the
  character, unknown/non-printing codes, and the Delete/Insert/F-key mapping.

### 2. Ship the three apps (`build.rs`, `tools/xui/build.py`)

* `SHIP_DOCUMENT_APPS` is `true`: the desktop default set appends
  `xui-editor.elf`, `xui-files.elf`, `xui-paint.elf`, and a missing ELF now
  fails the build on purpose.
* `tools/xui/build.py` already listed the three in `BINS`; its Windows
  `build_env` now sets the target-specific linker and rustflags
  (`CARGO_TARGET_..._LINKER` / `..._RUSTFLAGS=-C linker-flavor=ld.lld`) instead
  of a bare `RUSTFLAGS`, which was leaking into host build scripts.
* A `LAZYOS_DESKTOP=1` image embeds all seven apps; `XAPPS.LST` contains

  ```
  XTERM.ELF autostart
  XSYSMON.ELF autostart
  XFABMON.ELF autostart
  XCOUNTR.ELF autostart
  XEDITOR.ELF
  XFILES.ELF
  XPAINT.ELF
  ```

  and `INIT:APPS:SHIPPED count=9` (the three are available; they never
  autostart). The FAT image grew from ~5.7 MiB to ~28.8 MiB and still builds and
  boots.

### 3. Sessions and app bugs

`tools/screenshot/examples/xui_editor.json`, `xui_paint.json` and `xui_files.json`
were rewritten against the real apps (the previous drafts guessed the dialogs):

* **Editor** — type `Hello from LazyOS` / `second line`, Ctrl+S, clear the
  suggested name with Ctrl+A, type `/tmp/note.txt`, Enter (save through the
  dialog), then Ctrl+O / Ctrl+A / path / Enter to reopen. A final
  Ctrl+A/Ctrl+C/Ctrl+End/Enter/Ctrl+V round-trips the selection through
  `clipboardd`.
* **Paint** — the app is mouse-only (no accelerators), so the script clicks the
  toolbar cells by coordinate: draw a stroke, click Undo, click Redo, click
  Save. `PAINT:SAVE:PASS:/tmp/xpaint.png`.
* **Files** — click the `HELLO.TXT` tile in the `/` icon view and press Enter;
  `mimed` resolves `text/plain` to the Editor, which opens the file
  (`FILES:OPEN:PASS:/HELLO.TXT`, `EDITOR:OPEN:PASS:/HELLO.TXT`). A final click
  raises the Editor to show the file's contents.

Three integration bugs were found and fixed:

* `xui-app/src/platform/launcher.rs` resolved `os.lazy.mimed.v1` (the interface
  id) instead of the service's *registered* name `os.lazy.mimed`, so open-with
  never found the service.
* `xui-app/src/platform/messenger.rs` closed its resolved endpoint on drop. The
  kernel opens every resolver onto the service's single endpoint, so closing one
  is peer death for the service: the first clipboard offer killed `clipboardd`.
  Endpoints are now cached per name for the process lifetime and evicted (so
  they re-resolve) only when a call finds the peer gone.
* `user/src/bin/clipboardd/serve.rs` treated an `-EPIPE` reply (a caller that
  closed its channel first, e.g. a short-lived probe) as fatal; it now keeps
  serving, like `-ENOENT`.

`xui_client.json` now uses `Tab` (not `PageDown`) to cycle widget focus, matching
the new input rules.

## Verification (Windows host, QEMU 10.2, WHPX/TCG)

| Check | Result |
|---|---|
| `python tools/xui/build.py` | 9 binaries incl. `xui-editor/paint/files.elf` |
| `cargo test --manifest-path xui-app/Cargo.toml --workspace --lib` | 239 passed |
| `cargo clippy … xui-app --workspace --all-targets --target x86_64-unknown-linux-musl -- -D warnings` | clean |
| `cargo clippy -p kernel -p user --target x86_64-unknown-none -Zbuild-std=core,alloc -- -D warnings` | clean |
| `cargo clippy -p libmessenger -p messenger-generated -p lazyos-crypto -p font-atlas -p confd -- -D warnings` | clean |
| `cargo fmt` (root and `xui-app`) | clean |
| host library tests (`libmessenger`, `messenger-generated`, `lazyos-crypto`, `font-atlas`, `confd`, `surfbuf`) | all passed |
| `python tools/test/run.py --accel none` | `PASS=492 FAIL=0` |
| Editor session | `EDITOR:UP/SAVE/OPEN:PASS`; pixels non-blank (`pngstats`) |
| Paint session | `PAINT:UP/SAVE:PASS`; drawn/undone/redone/saved shots |
| Files session | `FILES:UP/OPEN:PASS`, `EDITOR:UP/OPEN:PASS` |
| `xui_sysmon.json` (owner mode) | `SYSMON:UP/REFRESH/QUIT:PASS` |
| `xui_client.json` (client mode, Tab focus) | `XUIAPP:CLIENT/KEY:PASS`, `XUIAPP:COUNTER:1` |
| `xui_desktop.json` (default desktop, BusyBox) | `TERM`/`SYSMON`/`FABMON` `UP:PASS`, shell commands `42`, `Hello from LazyOS!`, `confctl` usage |

`python tools/screenshot/pngstats.py` reports every editor/paint/files shot as
1280x720 with >70 colours and >99.9% non-background.

Note: `cargo test --workspace` is not runnable on a host target here (the
`no_std` `kernel`/`user` crates' `#[alloc_error_handler]` conflicts with the test
harness); that is pre-existing and CI instead runs the host library packages and
the in-QEMU kernel suite above. The ABI bench was not re-run because no
kernel/Linux-shim source changed.

## The Terminal "missing `o`" glyph

Track B saw `Hell fr m LazyOS!` in a downscaled Terminal screenshot. Capturing
the Editor and dumping the glyph pixels as ASCII art shows every letter
(including `o`) rendered correctly, and the same canvas/font path paints all
three apps. The "missing" glyph was a viewer downscaling artifact, not a font or
rasteriser bug; no code change was needed.

## Remaining / not exercised

* **Paint has no keyboard shortcuts** (upstream gap G6), so its session drives
  the toolbar with pointer clicks; the Save/Open paths are fixed at start-up.
* **Paths with spaces** still cannot be launched through `init` (Track B
  deferred); the apps validate what they do receive.
* The ABI bench was not re-run: no kernel/Linux-shim source changed (only the
  `clipboardd` userspace service and `xui-app`).

## Remaining gaps closed

A follow-up round fixed the gaps listed above. Every item was checked against
real captured pixels (QEMU sessions, screenshots read), not source.

| Gap | Result | Evidence |
|---|---|---|
| Editor letter-spaced text, line numbers over the first column | Fixed at the root. Two causes: the Editor asked for the generic `monospace` family, which resolved to the proportional Droid Sans because only that face was registered (so the `MMMMMMMMMM` cell probe measured a wide cell), and the canvas aligned non-wrapped text twice (cosmic-text against the buffer width, then again at draw time), which pushed right-aligned line numbers by the gutter width. The Editor now registers JetBrains Mono (`xui_app::font::register_mono`, Droid Sans stays the default UI family) and names it; the canvas gives the shaper no paragraph alignment and applies it once, per line. The upstream `xui-canvas` now carries this per-line alignment (see [Upstream canvas switch](#upstream-canvas-switch)). | `xui_editor.json` `02_editor_typed`: `Hello from LazyOS` on a tight grid with `1`/`2` right-aligned in the gutter. Terminal (`xui_desktop.json`) and fabricmon right-aligned table columns re-read: unchanged/correct. |
| Open / Save As showed an empty list | The listing itself worked; two things hid it. The name field is a type-ahead prefix filter (the session had typed a path), and `read_dir("/")` on the FAT root omits the `/tmp` and `/data` mount points (the VFS does not synthesise a mount's entry in its parent). `LazyFileSystem` (`xui-app/src/platform/dialog_fs.rs`) adds the mount points that resolve as directories; the pickers start in `/tmp`. Folders sort first (portable dialog). | `03a_save_dialog_tmp_listing` (`..`, `confd/`), `03b_save_dialog_root_listing` (`tmp/`, `HELLO.TXT`, `NOTES.TXT`), `05a_open_dialog_listing` (`confd/`, `note.txt`). |
| `set_window_title` was a no-op | New `os.lazy.display.v1` method 29 `SetTitle(surface, title)` (`idl/display.midl`, regenerated with `midlc`, `--check` clean). `xuid` (`title.rs`): owner only (`EACCES`), unknown surface `ENOENT`, at most 128 bytes cut at a character boundary, control characters dropped, blank result keeps the old title, unchanged title is a no-op; it repaints the chrome/taskbar and sends the shell `SurfaceChanged(Title)` (the `title` field is now set for `Title` as well as `Created`). Old clients are unaffected; a new client on an old `xuid` gets `EINVAL`, ignored. The backend dedupes so the Editor's per-keystroke retitle costs no IPC. Boot self-test `XUID:TITLE:PASS`. | `04_editor_saved`: title bar and taskbar read `note.txt - Editor`; a modified buffer shows `*Untitled - Editor`. |
| Paste into a second Editor | Works; no bug found. `xui_editor.json` now copies in the first Editor, launches a second Editor process from the desktop context menu, clicks into it and pastes through `clipboardd`. | `12_second_editor_pasted`: the second window (`*Untitled - Editor`) holds the four lines copied from `*note.txt - Editor`; serial has two `EDITOR:UP:PASS`. |
| Paint and Files sessions re-read | Paint: no defect (toolbar, palette, canvas, undo/redo/save states). Files: the root showed files only, so `/tmp` was unreachable; `LazyPlatform` (`platform/files_fs.rs`) adds the mount points, folders first (`30 items (1 folder, 29 files)`). The Files session's click moved one tile right (`tmp` is now first). | `xui_files.json` `02_files_selected`: `tmp` Folder first, `HELLO.TXT` selected; `04_editor_raised`: `HELLO.TXT - Editor`. |

Checks run: `midlc --check` clean, `cargo fmt --all --check` clean (root and
`xui-app`), `cargo clippy -p kernel -p user ... -D warnings` clean, `xui-app`
clippy `-D warnings` clean (musl workspace, and the host `--lib`), `cargo test
--manifest-path xui-app/Cargo.toml --workspace --lib` 246 passed (new tests for
both mount-point wrappers), `python tools/test/run.py --accel none`
`PASS=500 FAIL=0`, and the editor, paint, files, desktop
(`LAZYOS_XUI_AUTOSTART=term,sysmon,fabricmon,counter`), sysmon and client
sessions pass their serial markers. Multi-app sessions on Windows need the
`LAZYOS_XUI_APPS` list in native form (`C:\...;C:\...`); a bash-style
`/e/...;...` list is silently skipped with a build warning.

Not fixed / limits: the mount-point list is fixed (`tmp`, `data`); a kernel-side
fix (the VFS synthesising mount points in `readdir`) would make the wrappers
unnecessary and is not done. Paint and Files keep their creation title (the
folder path for Files); only the Editor retitles. The alignment change is now
covered by the upstream `xui-canvas` tests and the LazyOS screenshots below
(the crate is a git dependency, so its own test suite is not part of the
`xui-app` workspace).

## Correctness checklist (this round)

1. **State invalidation** — the resolved endpoint cache drops a dead entry on
   `-EPIPE`/`-ENOENT`, so a restarted service is re-resolved; the Editor drops
   the find session and dialog flags on New/Open.
2. **Many-of-a-kind** — Files still opens one window per folder (Track A); the
   files session confirms a second process (Editor) launched by open-with
   coexists with Files.
3. **Failure atomicity** — saves build a temp file then rename (unchanged);
   `Service::call` returns the error without touching the cache on a service
   rejection.
4. **Re-entrancy** — no new borrow is held across a callback; the endpoint
   cache borrow is released before the call.
5. **Undo/history** — unchanged app code; Paint's undo/redo is exercised by the
   session.
6. **File safety** — the Editor still refuses symlink writes; the sessions save
   only under `/tmp`.
7. **Text handling** — the key mapping is byte/char-correct (ASCII printable
   range); Ctrl/Alt suppression is unit-tested; the Editor's UTF-8 path is
   upstream code.
8. **Domain** — multi-window, focus/close, unsaved prompt, atomic save, symlink
   refusal, open-with errors in the status bar, and scripted sessions are all
   either tested here or unchanged from Track A.

## Upstream canvas switch

The vendored `xui-canvas` fork is gone. Upstream PR
[va1erian/xui#246](https://github.com/va1erian/xui/pull/246) (merge commit
`35c818f9b187359927b1528c37d63f62604caa05`) put every LazyOS addition into
`crates/xui-canvas` behind a default-on `winit-backend` feature:

* `set_default_font`/`add_font`/`set_default_family` build the shaper's font
  database from the registered bytes only (no system scan, no file `mmap`), so
  LazyOS can shape text with its bundled TTFs;
* `TextStyle` horizontal alignment is applied once per line at draw time, so the
  Editor line-number gutter and every right-aligned table column are correct;
* `Surface::pixels` exposes the borrowed RGBA plane for a clone-free present;
* `default-features = false` drops `winit`, `softbuffer`, `glutin`, `glow`,
  `arboard`, the `windows` double-click metrics and `xui-gpu`, leaving the pure
  `tiny-skia`/`cosmic-text` painter plus `OffscreenBackend`.

### Change

* Deleted `xui-app/vendor/xui-canvas/` and `xui-app/crates/icons/` — **370
  tracked files removed** (32 and 338).
* `xui-app/Cargo.toml`: dropped `exclude = ["vendor/xui-canvas"]` and the
  `[patch."https://github.com/va1erian/xui"]` block; `xui-core` and `xui-canvas`
  are now git dependencies at `rev = "35c818f9…"`, the latter with
  `default-features = false`. Every manifest under `xui-app/crates/*` uses the
  same rev (`xui-canvas` as a dev-dependency; explorer's optional `xui-icons`),
  so a single `xui_core` is linked. `village-icons` stays off (the Files app
  does not enable it), so the Lucide fallback is still what ships.
* `xui-app/Cargo.lock` refreshed with `cargo fetch`: only the three xui packages
  changed (git sources), no unrelated upgrades.
* No source change was needed in `xui-app/src` or the three app crates: the
  symbols they use are a subset of the upstream API. `docs/xui-plan.md` records
  the pinned-rev bump procedure (bump `xui-core` and `xui-canvas` together).

### Verification

| Check | Result |
|---|---|
| `python tools/xui/build.py` | 9 binaries incl. `xui-editor/paint/files.elf` |
| `cargo test --manifest-path xui-app/Cargo.toml --workspace --lib` | 242 passed (29+136+37+40) |
| crate integration tests (`xui-code-editor`, `xui-paint`, `xui-explorer`) | all passed (incl. the snapshot suites) |
| `cargo clippy … xui-app --workspace --lib -- -D warnings` (host) | clean |
| `cargo clippy … xui-app --workspace --target x86_64-unknown-linux-musl -- -D warnings` | clean |
| `cargo fmt --all` (xui-app) | clean |
| `cargo tree --target x86_64-unknown-linux-musl` | single `xui-core`; no `winit`/`softbuffer`/`glutin`/`glow`/`arboard`/`windows`/`xui-gpu` |
| Editor session (`xui_editor.json`) | `EDITOR:UP/SAVE/OPEN:PASS`; `02_editor_typed`: `1`/`2` **right-aligned** in the gutter, text starts on a tight, non-overlapping grid |
| Paint session (`xui_paint.json`) | `PAINT:UP/SAVE:PASS`; drawn/undone/redone/saved shots non-blank |
| Files session (`xui_files.json`) | `FILES:UP/OPEN:PASS`, `EDITOR:OPEN:PASS:/HELLO.TXT` |
| Desktop session (`xui_desktop.json`, `LAZYOS_XUI_AUTOSTART=term,sysmon,fabricmon,counter`) | `TERM`/`SYSMON`/`FABMON` `UP:PASS`, shell `42` + `Hello from LazyOS!` + `confctl` usage |
| sysmon owner session (`xui_sysmon.json`) | `SYSMON:UP/REFRESH/QUIT:PASS`; numeric columns right-aligned |
| client session (`xui_client.json`) | `XUIAPP:CLIENT/KEY:PASS`, `XUIAPP:COUNTER:1`, `XUIAPP:CLOSE:PASS` |
| `pngstats.py` on the editor/desktop/sysmon/client/paint/files shots | 1280x720, ≥51 colours, >99.9% non-background |

The upstream alignment code is byte-for-byte the same logic the fork carried
(the `text::draw` per-line offset), so the Terminal grid and the
sysmon/fabricmon right-aligned columns are unchanged; they were re-read from the
captured pixels above. **No follow-up needed**: no `docs/xui-canvas-followup.md`
was created because upstream #246 already has the fix.
