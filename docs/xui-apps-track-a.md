# TRACK A notes: Editor, Paint and Files (xui-app side)

Track A of the migration in [`docs/xui-apps-migration.md`](xui-apps-migration.md)
owns the `xui-app` side: the xui bump (P0), the clipboard/dialog/multi-window/Paint
platform work (P1 items 2, 3, 4, 6), the `Platform`/`Launcher` impls (P1 item 5,
app side), the three binaries and their support crates (P2 items 1-3) and the
app-side tests and serial markers (P3). The system side (kernel, `init`, `mimed`,
`keyd`, `build.rs`, CI) is Track B and was not touched here. The full shipped /
deferred / blocked record is in
[`docs/xui-apps-migration-status.md`](xui-apps-migration-status.md); this file is
the Track A checklist and the evidence gathered while finishing the branch.

## What Track A shipped

- **P0 — xui bump to `5efb730`.** `xui-core`/`xui-canvas` pins moved; the
  three LazyOS additions (`set_default_font`/`add_font`/`set_default_family`,
  natural-width horizontal alignment in `text::draw`, `Surface::pixels`) were
  carried in a vendored `xui-canvas` at the time. They have since been
  **upstreamed** (va1erian/xui#246, rev `35c818f9`) behind a default-on
  `winit-backend` feature: LazyOS now depends on `xui-canvas` with
  `default-features = false` and the vendor directory plus `[patch]` are gone.
  The windowed `winit`/GL backend, the `arboard` clip and `xui-gpu` stay out of
  the musl graph because they cannot link for the Windows-host musl target and
  LazyOS cannot host `winit` (anonymous `mmap` only). `patches/xui-core.patch`,
  `tools/xui/patch_core.py` and the `build.py` hook were deleted; the Button
  hover fix is upstream now.
- **P1.2 clipboard.** `xui-app/src/platform/clipboard.rs` is a
  `messenger-generated` client for `os.lazy.clipboard.v1` (scoped
  `Offer`/`Request`, `ALLOW_NESTED` header, bounded payload); wired into
  `LazyOSBackend::{clipboard_text,set_clipboard_text}` with the in-process
  fallback when `clipboardd` is absent.
- **P1.3 dialogs + title.** The portable `FileDialog`/`Dialog` overlays work on
  the LazyOS backend. `set_window_title` still has no `os.lazy.display.v1`
  method, so it is deferred (see below).
- **P1.4 multi-window.** Client mode now holds one `ClientWindow` (surface +
  event channel + damage + timers) per `Window`; `open_window` creates a real
  `xuid` surface, the loop ticks every window, and `close_window` drops only
  that window's damage, timers and nodes. This is what lets Files open one
  window per folder.
- **P1.5 (app side).** `StdPlatform` over the Linux shim (deletion and metadata
  use `symlink_metadata`, so a link is never followed) and `LazyLauncher`
  (`mimed.Open`, which resolves the MIME and launches through `init`), with
  `io::ErrorKind::Unsupported` for unknown file types shown in the status bar.
- **P1.6 Paint storage.** `platform/storage.rs` `PngStorage`: atomic save
  (temp file + `rename`, cleanup on failure) and symlink refusal.
- **P2 the three bins.** `xui-editor` (upstream notepad promoted: File/Edit
  menus, find/replace bar, status bar, dirty-state prompt, atomic save, `argv`
  open), `xui-paint` (`PaintApp` + `PngStorage`, `argv` open), `xui-files`
  (`Explorer` + `LazyLauncher`, start at `argv` path / `/`).
  `platform/argv.rs` validates every argument (absolute, NUL-free, bounded) and
  skips the `--client`/`attempt=` tokens.
- **P3 (app side).** Host tests for the copied crates and the platform impls;
  serial markers `EDITOR:UP/OPEN/SAVE:PASS`, `PAINT:UP/OPEN/SAVE:PASS`,
  `FILES:UP/OPEN:PASS`; `tools/xui/build.py` builds all three.

## Evidence (run on this branch)

- `python tools/xui/build.py` builds all **nine** binaries, including
  `xui-editor.elf`, `xui-paint.elf` and `xui-files.elf`.
- `cargo test -p xui-app --lib` → 16 passed; the four copied crates
  (`xui-code-editor`, `xui-paint`, `xui-explorer`, `xui-icons`) pass their full
  suites (`cargo test` under `xui-app/`).
- `cargo fmt` and `cargo clippy -- -D warnings` are clean for the `xui-app`
  workspace (lib, the three bins, and the copied crates), and for the root
  workspace's `kernel`/`user` (target `x86_64-unknown-none`) and
  `libmessenger`/`messenger-generated`/`lazyos-crypto`/`font-atlas`/`confd`
  packages.
- A `LAZYOS_DESKTOP=1` image embeds the three ELFs; a headless QEMU boot
  (`shots/final2/shot_45s.png`) shows the Editor, Paint and Files windows
  painting, with serial `EDITOR:UP:PASS`, `PAINT:UP:PASS`, `FILES:UP:PASS`.
  The pre-existing Terminal / sysmon / fabricmon viewers still render.

## Deferred / blocked (Track A scope)

- **`set_window_title` → `xuid` title bar.** `os.lazy.display.v1` has no
  title-update method; adding one is a MIDL + `xuid` change shared with Track B,
  so it was left out of this branch.
- **Scripted `xui_*.json` sessions and `.github/workflows/xui.yml`.** Track B
  owns `tools/screenshot/examples/**` and `.github/**`; wiring them there would
  conflict. The evidence above is a captured boot plus pixel inspection.
- **Ctrl+letter chords and F1–F3/F5–F12.** These need the kernel PS/2 layer to
  forward the letter/function key code with modifiers (Track B); the client now
  tracks Shift/Ctrl/Alt/Super and maps F4, so they start working once the key
  codes arrive.
- **Paths with spaces.** `init`'s launch line is space-split; an argv-based
  spawn is a kernel/`init` change (Track B). The apps validate what they do
  receive.
- **Copied upstream files keep their upstream size.** A handful of the
  verbatim-copied `xui-code-editor` sources (`lexer.rs` 1665 lines, `events.rs`
  983, `editor.rs` 976, `paint.rs` 778, `buffer.rs` 774) exceed the 500-line
  file convention. They are upstream code copied unchanged (MIT) and were not
  grown here; splitting them would be a further divergence from upstream. The
  LazyOS-written files in `xui-app/src/` are all under 500 lines.

## Correctness checklist walk-through (app side)

1. **State invalidation.** `close_window` drops that window's damage rectangles,
   timers, nodes and focus; the backend's `primary` is only cleared when it was
   the closed window.
2. **Many-of-a-kind.** State is per window (`HashMap<WindowId, Window>` with a
   `ClientWindow` each) and per node; Files' one-window-per-folder path is the
   test of this.
3. **Failure atomicity.** Paint and editor saves build a temp file then rename;
   a failed save removes the temp and leaves the old file and the dirty flag.
   `argv` validation runs before any filesystem/service call.
4. **Re-entrancy.** `composite` no longer holds the `windows` `RefCell` borrow
   while running a painter (the explorer queries DPI during paint); the surface
   is put back after the painter returns.
5. **Undo/history.** The editor's `write_atomically` and the paint history are
   unchanged apart from the symlink refusal; the copied crates' undo tests pass.
6. **File safety.** Deletion/metadata use `symlink_metadata`; writes refuse a
   symlink target and never write through one; tests cover the refusal.
7. **Text handling.** `argv.rs` rejects relative/empty/NUL/over-long paths and
   is byte-bounded; the editor handles UTF-8 via the upstream rope buffer.
8. **Domain.** Multi-window/focus/close are covered above; a failed Paint Open
   leaves the current bitmap (upstream model tests); Files surfaces launcher
   errors in the status bar.
