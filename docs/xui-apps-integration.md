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

* **Clipboard across two windows.** Copy/paste is proven through `clipboardd`
  within one Editor (the paste duplicates the selection through the service).
  The scripted session does not open a *second* Editor and paste there; that is
  the same session-scoped service call, but it is not captured as a screenshot.
* **`set_window_title`** is still a no-op (no `os.lazy.display.v1` method); the
  Editor's title bar shows the static `Editor` name, not the file name.
* **Paint has no keyboard shortcuts** (upstream gap G6), so its session drives
  the toolbar with pointer clicks; the Save/Open paths are fixed at start-up.
* **Paths with spaces** still cannot be launched through `init` (Track B
  deferred); the apps validate what they do receive.
* The ABI bench was not re-run: no kernel/Linux-shim source changed (only the
  `clipboardd` userspace service and `xui-app`).

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
