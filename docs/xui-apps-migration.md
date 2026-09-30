# Plan: migrate Editor, Paint and Files from XUI into LazyOS

**Goal:** the three portable xui apps that live upstream in `va1erian/xui`
become first-class LazyOS desktop applications that every desktop image ships:

| LazyOS app id | Binary / disk name | Upstream source (xui `main`) | Registry today |
|---|---|---|---|
| `editor` | `xui-editor` / `XEDITOR.ELF` | `crates/xui-code-editor` (+ `examples/notepad`) | row exists, `EDITOR.ELF`, never shipped |
| `paint` | `xui-paint` / `XPAINT.ELF` | `crates/xui-paint` (`xpaint`) | none (new row) |
| `files` | `xui-files` / `XFILES.ELF` | `crates/xui-explorer` | row exists, `FILES.ELF`, never shipped |

Once each is migrated and green in LazyOS CI, the crate is deleted from the xui
repository (a follow-up PR **in that repo**, not part of this work).

This closes the app half of issues #159 (Files), #162 (Editor, Paint) and the
"registry lists EDITOR/FILES ELFs that are not shipped" gap from #216. It builds
on `docs/xui-plan.md` (the `LazyOSBackend`), `docs/shell-plan.md` and
`docs/linux-abi-plan.md`.

## What the apps already give us

All three are written against `xui-core` only and expose their OS seams as small
traits, so the port is "supply the platform", not "rewrite the app":

- **Files** — `Platform { list, metadata, remove, home }` + `Launcher { open }`.
  The crate has a `std-platform` feature (`std::fs`) that is *probably usable
  as-is* on LazyOS, because our apps are static-musl `std` programs and the
  Linux shim serves `getdents64`, `lstat`, `unlink`, `rmdir`.
- **Paint** — `Storage` trait (`MemoryStorage`, `FsStorage` behind `fs`). PNG
  encode/decode goes through xui's portable `Image`. Pure model + custom
  `Control` nodes; no thread, timer or process use.
- **Editor** — `Editor` widget (ropey buffer, undo, find/replace) with a
  `Clipboard` trait defaulting to the backend clipboard. The Notepad example
  supplies the menu bar, find bar, status bar and file dialogs; that example is
  the app we ship (as `xui-editor`), reading and writing through `std::fs`.

## Gaps to fill (found by reading both sides)

Each is a work item below; the list is the reason this is a plan and not a copy.

| # | Gap | Where |
|---|---|---|
| G0 | The pinned xui (`2747818`, 2026-09-26) had **none** of these crates and predated `IconView`, `StatusBar`, `FileDialog`, `TaskDialog`. **Resolved:** the pin moved to `5efb730` for the migration and later to `35c818f9` (upstream PR va1erian/xui#246); `xui-canvas`/`xui-icons` are git dependencies at that rev (`xui-canvas` with `default-features = false`), not vendored. Issue #351 is closed. | `xui-app/Cargo.toml`, `xui-app/crates/*/Cargo.toml` |
| G1 | `LazyOSBackend` does not override `clipboard_text`/`set_clipboard_text`, `file_dialog`, `run_modal`, `set_window_title`, `set_cursor`, `minimize`. Defaults are in-process or unsupported. | `xui-app/src/backend/handlers.rs` |
| G2 | **Multiple windows per process.** Files opens one window per folder. The client backend creates a single surface. | `xui-app/src/backend*`, `client_window.rs`, `os.lazy.display.v1` |
| G3 | **Keyboard coverage.** Editor/Files need arrows, Home/End, PgUp/PgDn, Delete, F-keys (F5), Ctrl/Alt/Shift modifiers and Ctrl+letter chords delivered as `Event::KeyDown{key, modifiers}`. `docs/xui-plan.md` records that the PS/2 path does not decode function keys, and `xuid` consumes Tab/PageUp/PageDown. | kernel PS/2 decode, `keyd`, `xuid`, `input.rs` |
| G4 | **Clipboard** must go to `clipboardd` (`idl/clipboard.midl`, caps `clipboard.read/write`), not stay in-process. | new `xui-app` clipboard client over `messenger-generated` |
| G5 | **Launching with a file argument** (Files → Editor/Paint, mimed open-with). `AppSpec.args` is a fixed prefix; `Launch(app, args, session)` has an `args` string but the registry must append the path safely (no shell splitting). Apps must accept `argv` paths. | `user/src/bin/init/apps.rs`, `idl/init.midl`, `mimed` |
| G6 | **Filesystem semantics under the Linux shim:** `read_dir` `d_type`, `symlink_metadata`, `created` time (`statx`, #348), `rename`/`unlink`/`rmdir` on FAT+overlay and `/data` (ext2), atomic save (write temp + rename). `home()` must resolve the session user's home. | `kernel/src/process/linux/*`, `tools/abi` |
| G7 | **Modal/dialog UX:** xui's portable `FileDialog`, `Dialog` and `TaskDialog` must work as in-window overlays when the backend has no native picker. | verify on `LazyOSBackend` |
| G8 | **Fonts/icons:** monospace face for the editor (JetBrains Mono is already bundled for `xui-term`), Droid Sans for the rest; Files icons (`xui-icons` Global Village, or the Lucide fallback). | `xui-app/src/font.rs`, `assets/fonts` |
| G9 | **Registry, image, CI:** register `paint`; point `editor`/`files` at the new ELFs; MIME rows for `image/png`; 8.3 names in `build.rs`; default desktop set; manifest `XAPPS.LST`; FAT image size; CI. | `build.rs`, `init/apps.rs`, `mimed/apps.rs`, `tools/xui/build.py`, `.github/workflows/xui.yml` |
| G10 | Cursor shapes (Paint wants a crosshair) — **non-goal**: Paint paints its own brush ring; no protocol change. | — |

## Layout in the repo

The upstream library crates are copied (with their tests) into the standalone
`xui-app` workspace, so they build for musl **and** test on the host:

```
xui-app/crates/code-editor/   (from xui-code-editor, lib only)
xui-app/crates/paint/         (from xui-paint, lib only)
xui-app/crates/explorer/      (from xui-explorer, lib only)
xui-app/src/bin/editor.rs  files.rs  paint.rs   (+ editor/ files/ paint/ submodules)
xui-app/src/platform/         LazyOS impls: fs.rs, launcher.rs, clipboard.rs, storage.rs
```

`xui-core`, `xui-canvas` and `xui-icons` are **not** copied: they are git
dependencies on `va1erian/xui` at one pinned rev (`xui-canvas` with
`default-features = false`, `xui-icons` optional behind explorer's
`village-icons`). This file keeps the historical plan; the switch to upstream
canvas is recorded in
[`xui-apps-integration.md`](xui-apps-integration.md#upstream-canvas-switch).

Keep every file under 500 lines (AGENTS.md); split by responsibility as you go.
Copied files keep their upstream licence headers (MIT).

## Phases (do in order; each ends green before the next starts)

### P0 — bump xui (prerequisite, issue #351)
1. Pick the xui commit: newest `main` that has the three crates and still
   compiles for musl (`5efb730` at time of writing; re-check `gh api`).
2. Add the LazyOS additions upstream rather than vendoring: upstream PR
   va1erian/xui#246 (`35c818f9`) puts `set_default_font`/`add_font`/
   `set_default_family`, per-line horizontal alignment for natural-width runs
   and `Surface::pixels` into `crates/xui-canvas`, behind a default-on
   `winit-backend` feature. Depend on `xui-canvas` with `default-features =
   false` and there is no vendor copy to port.
3. Bump `xui-core`/`xui-canvas` pins; implement new `Backend` trait items in
   `handlers.rs` (no-ops where LazyOS has no equivalent, each with a comment).
4. Delete `patches/xui-core.patch`, `tools/xui/patch_core.py` and its hook in
   `tools/xui/build.py`.
5. **Evidence:** `python tools/xui/build.py` builds all existing bins; boot the
   existing xui desktop session and Read the screenshots (sysmon/fabricmon/term
   text alignment and fonts unchanged).

### P1 — platform layer (fills G1, G3, G4, G6, G7)
1. **Keyboard:** trace a key from PS/2 → kernel → `keyd`/`xuid` → client
   `KeyDown` → `input.rs`. Make arrows, Home/End, PgUp/PgDn, Delete, F1–F12 and
   Ctrl/Alt/Shift reach the focused xui node. If the fix is in the kernel PS/2
   decoder, it needs the AGENTS.md correctness + soak test pair
   (`kernel/src/tests/`). Keep `xuid`'s own reserved chords working.
2. **Clipboard:** `xui-app/src/platform/clipboard.rs` — a Messenger client for
   `os.lazy.clipboard.v1` using `messenger-generated` (never hand-written
   fields). Wire it into `LazyOSBackend::{clipboard_text,set_clipboard_text}`;
   fall back to in-process storage when `clipboardd` is absent (console images).
   Requests are session-scoped by the kernel; bound payload size.
3. **Dialogs:** verify portable `FileDialog`/`Dialog`/`TaskDialog` work with the
   backend's default `file_dialog`/`run_modal`; implement whatever is missing
   (in-window overlay), plus `set_window_title` → the `xuid` surface title.
4. **Multi-window** (G2): support `open_window` in client mode — extra surfaces
   via `os.lazy.display.v1`, per-window node tables, per-window focus and close.
   If the protocol needs a change, edit `idl/display.midl` and regenerate with
   `midlc`; do not hand-write. Closing the last window exits the process.
5. **Filesystem (G6):** write `platform/fs.rs` implementing the explorer
   `Platform` over `std::fs` with the semantics the trait documents (never
   follow symlinks on delete/metadata). Try the crate's own `StdPlatform` first;
   only keep a LazyOS wrapper for real divergences (`home()`: `$HOME`, else
   `/home/<user>`, else `None`). A shim gap (missing `d_type`, `lstat`,
   `rename`) is fixed in `kernel/src/process/linux/` with kernel tests and an
   ABI-bench fixture under `tools/abi/`.
6. **Storage for Paint:** `platform/storage.rs` — a `Storage` that reads/writes
   PNG through `std::fs`, refuses symlinks, writes to a temp file then renames.

### P2 — the three apps (G5, G8, G9)
1. `xui-editor`: Notepad example promoted to a bin. File > New/Open/Save/
   Save As/Quit, Edit menu, find/replace bar, status bar, dirty-state prompt on
   close, argv path opens the file, atomic save. Monospace font = JetBrains Mono.
2. `xui-paint`: `PaintApp::build` with the LazyOS `Storage`; argv path opens a
   PNG; Save/Open through `FileDialog` rooted at `$HOME`.
3. `xui-files`: `Explorer::new(platform, launcher).open_root(...)`; start at
   `$HOME` (fall back to `/`). `LazyLauncher::open` resolves the MIME type and
   calls `init.Launch(app, path)` through the existing `mimed` open-with
   registry; unsupported/unknown types return `io::ErrorKind::Unsupported`
   (shown in the status bar). Icons: the `xui-icons` crate is a git dependency
   (not vendored), and `xui-app/Cargo.toml` enables `village-icons` for the
   Files app; a target that leaves it off falls back to Lucide.
4. Registry (G9): `init/apps.rs` `editor`→`XEDITOR.ELF`, `files`→`XFILES.ELF`,
   new `paint`→`XPAINT.ELF` as `xui_app(...)` rows; make `Launch` pass a file
   path argument as a single argv item (validated: absolute, no NUL, bounded);
   `mimed/apps.rs` adds `image/png` → `paint` (`open`,`edit`) and `files`
   `reveal`; `build.rs` `xui_disk_name` gets the new 8.3 names; the three
   ELFs join `DESKTOP_XUI_APPS` so **every desktop image ships them**;
   `tools/xui/build.py` `BINS`; `tools/lazygui` app list; check the FAT boot
   image still fits (raise `mkdisk` size in the same PR if not).
5. Start menu: apps appear via `ListApps` with no extra code; verify.

### P3 — tests and evidence
- **Host unit tests:** the copied crates' test suites run under
  `cargo test --manifest-path xui-app/Cargo.toml` (host target) — keep them all.
  Add tests for every LazyOS platform impl (symlink refusal, atomic-save
  failure leaves the old file, `Launch` argument validation, clipboard bounds
  and absent-service fallback).
- **Kernel:** any kernel/shim change ships correctness + soak tests
  (`python tools/test/run.py --accel none` passes), per AGENTS.md.
- **Scripted sessions** (`tools/screenshot/examples/xui_editor.json`,
  `xui_paint.json`, `xui_files.json`, driven by `qemu_session.py`) with serial
  markers `EDITOR:UP:PASS`, `EDITOR:SAVE:PASS`, `PAINT:UP:PASS`,
  `PAINT:SAVE:PASS`, `FILES:UP:PASS`, `FILES:OPEN:PASS`. Sessions: type text,
  save, reopen; draw + undo + save PNG; navigate a folder, open a text file in
  Editor via open-with, copy text between apps through `clipboardd`.
  Read the resulting PNGs and assert with `pngstats.py`.
- Extend `.github/workflows/xui.yml` to build the new bins and run the sessions.
- Re-run the ABI bench if the shim changed.

### P4 — docs and hand-off
- Update `docs/xui-plan.md` (status), `docs/shell-plan.md` (S5.1/S5.3 mapping),
  `docs/architecture/display.md` (multi-window), `tools/screenshot/README.md`.
- Write `docs/xui-apps-migration-status.md` listing what shipped, what was
  deferred, and the exact text of the follow-up issue to delete the three crates
  from `va1erian/xui` (do not touch that repository).

## Non-goals
Rename/copy/move/drag-drop in Files (upstream v1 limits), syntax highlighting
beyond `PlainText`, cursor-shape protocol, zoom in Paint, file watching, Linux
`inotify`.

## Risks
| Risk | Mitigation |
|---|---|
| P0 bump breaks text rendering | screenshot diff of sysmon/term before and after |
| Multi-window needs a display-protocol change | do it via MIDL; if it balloons, fall back to one-window navigation for Files and record it in the status doc |
| Function keys never reach the client | fix at the earliest layer; kernel change carries tests |
| ELF size vs FAT image | measure after P2.4; grow the image if needed |
| Shim missing `rename`/`lstat` semantics | fix in the shim with ABI fixtures, not in the app |

## Outcome

Implemented: P0 (bump to `5efb730`, then `35c818f9` with the upstream canvas and
the vendored copy + `[patch]` deleted, drop the core patch), the
platform layer, multi-window client mode, the three apps, registry/MIME/image
integration, and the host tests. Captured pixels of the Editor, Paint and Files
windows on a real `xuid` desktop. The plan was followed except for the
deviations recorded in
[`xui-apps-integration.md`](xui-apps-integration.md#upstream-canvas-switch):
`xui-canvas` is built with `default-features = false` so its windowed/GL backend
never enters the musl graph, the Files app enables explorer's `village-icons`
(the multi-colour Global Village tiles), Paint uses a fixed save path, the Files
launcher calls `mimed.Open`, the editor refuses symlink writes, and the three
apps ship without boot-autostart.
Deferred, with reasons, are Ctrl+letter chords and F1–F3/F5–F12 (kernel PS/2
decoding), paths with spaces, `set_window_title` (a display-MIDL addition), and
the scripted-session/CI wiring.
