# Track B: system side of the Editor / Paint / Files migration

> **History.** This describes the layout before filesystem F5 (issue #509): the 8.3 `X*.ELF` names and the app list `init` read are gone, and the desktop apps are core packages (`docs/packages.md`, core packages).

Branch `xui-apps-system`. Everything outside `xui-app/` that
`docs/xui-apps-migration.md` needs. Track A owns `xui-app/**`.

## What shipped

| Plan item | Change |
|---|---|
| P1.1 keyboard (G3) | PS/2 now decodes Delete, Insert and F1-F12 (`kernel/src/input/keyboard.rs`). Ctrl+letter reaches a compositor as the letter (not a C0 control) so Ctrl+H/I/M differ from Backspace/Tab/Enter. `xuid` ORs modifier bits (Shift/Ctrl/Alt/Super) into every forwarded `KeyDown`/`KeyUp`, and forwards a plain Tab to the focused client. Codes and rules: **`docs/architecture/display.md`, "Key codes clients receive"**. |
| G5 launch args | `init`'s `Launch(app, args, session)`: `args` is empty or one absolute path (<= 1024 bytes, no control char), appended after the registry's fixed args as ONE `argv` item; otherwise `EINVAL`. `spawnv` (fs F3) passes the `argv` vector unsplit, so paths with spaces (and quotes) work. Argument order seen by the app: `<ELF> --client <path> attempt=<n>` (`attempt=` is last; ignore unknown trailing `key=value` items). `idl/init.midl` doc updated (no wire change, no regeneration needed). |
| P2.4 registry / MIME (G9) | `init/apps.rs`: `editor` -> `XEDITOR.ELF`, `files` -> `XFILES.ELF`, new `paint` -> `XPAINT.ELF`, all `xui_app` rows (Linux ABI, `--client`, `Ship::Manifest`). `mimed`: `image/png` -> `paint` (`open`,`edit`), `files` `reveal`. `build.rs`: 8.3 names, `ON_DEMAND_XUI_STEMS` (the three never autostart, even under the default "autostart everything"), and the `SHIP_DOCUMENT_APPS` switch (below). `tools/lazygui/catalog.py` lists the sessions/viewers. |
| G6 shim FS | New musl fixture `tools/abi/fixtures/src/fsops.rs` (bench row `fsops`) exercises `read_dir` + `file_type`, `symlink_metadata`, `create_dir`, create/write/truncate, `rename` (new name and over an existing file), `remove_file`, `remove_dir`, `remove_dir_all` on `/tmp`, the FAT root `/` and `/data`. Only gap found: ext2 had no `rmdir` (`ENOSYS`). Implemented (then `kernel/src/fs/ext2/rmdir.rs`, now `libs/ext2fs/src/rmdir.rs`) with correctness + soak tests. |
| P3 evidence | `tools/screenshot/examples/xui_editor.json`, `xui_paint.json`, `xui_files.json`; `.github/workflows/xui.yml` builds the bins (when `tools/xui/build.py` emits them) and runs three sessions with the markers below; uploads/comments their shots. |

## Key code table location

`docs/architecture/display.md`, bullet "Key codes clients receive". Summary for
Track A's `input.rs`: `code = key & 0x00FF_FFFF`; modifiers = bits 24 (Shift),
25 (Ctrl), 26 (Alt), 27 (Super). `user::messenger::display::key` mirrors the
constants (`DELETE`, `INSERT`, `F1`.. `F12`, `MOD_*`, `CODE_MASK`,
`is_printable`, `with_modifiers`). Things Track A must do:

* Mask the code before matching (`key & CODE_MASK`); today `key_of` matches the
  raw value, so any chord with a modifier currently falls through to
  `Key::from_code(other as u16)`.
* Fill `Event::KeyDown.modifiers` from the bits instead of `Modifiers::NONE`.
* Ctrl+letter arrives as the lowercase letter plus the Ctrl bit; do not emit a
  `Char` event when the Ctrl or Alt bit is set.
* Add Delete (`0x10C`), Insert (`0x10D`), F1..F12 (`0x110 + n - 1`).
* **Tab now reaches the client** (it used to be swallowed by `xuid`), and
  `PageUp`/`PageDown` no longer need to cycle widget focus: make Tab /
  Shift+Tab the focus keys and let PageUp/PageDown scroll (Editor). The
  compositor keeps Alt+Tab and **Ctrl+Tab** (cycle windows; `xuid_wm.json`
  now uses Ctrl+Tab), Ctrl+Esc/Super, Alt+F4.

## Verification (this machine, Windows, TCG)

* `python tools/test/run.py --accel none`: PASS=492 FAIL=0 (new: `display_nav_and_function_keys`,
  `display_ctrl_letter_is_letter`, `display_key_decode_soak`, `spawn_argv_splitting_rules`,
  `spawn_argv_roundtrip_soak`, `fs_ext2_rmdir_rules`, `fs_ext2_rmdir_survives_remount`,
  `fs_ext2_soak_rmdir_generations`; the existing modifier test now expects the letter for Ctrl+A).
* `python tools/abi/build.py` + `python tools/abi/run.py --at 12 --accel none`: 18/18 including `fsops`
  on `/tmp`, `/`, `/data`. (On Windows the musl fixtures need
  `CARGO_TARGET_X86_64_UNKNOWN_LINUX_MUSL_LINKER=rust-lld` and
  `CARGO_TARGET_X86_64_UNKNOWN_LINUX_MUSL_RUSTFLAGS="-C linker-flavor=ld.lld"`;
  the same works for `tools/xui/build.py`.)
* `cargo clippy -p kernel -p user --target x86_64-unknown-none -Zbuild-std=core,alloc -- -D warnings` clean; `cargo fmt --all --check` clean.
* Boot evidence: desktop image serial has `INIT:LAUNCH:ARGS:PASS`,
  `INIT:APPS:PASS`, `XUID:KEYS:PASS` (client key encoding self-test); the
  desktop session (`xui_desktop.json`) and `xuid_wm.json` (Ctrl+Tab skipping a
  minimised window, screenshot read) still pass. `tools/services/evidence.py`
  now also requires `INIT:LAUNCH:ARGS:PASS`.
* Observation, not caused by this branch: in the desktop Terminal screenshot
  the glyph `o` is missing from rendered text (`Hell fr m LazyOS!`) while the
  serial `TERM:OUT:` lines are correct. That is the pre-existing xui text
  path (Track A's canvas bump area); worth a look when P0 lands.

## Not verified / cannot pass yet

* The three sessions (`EDITOR:UP/SAVE`, `PAINT:UP/SAVE`, `FILES:UP/OPEN`) need
  Track A's apps; they are syntactically valid, JSON-checked, and the workflow
  YAML parses. Their dialog interaction is a guess: Ctrl+S opens a save dialog,
  the typed path (`/tmp/note.txt`, `/tmp/drawing.png`) goes into its file-name
  field and Enter confirms; Files navigates with Down/Up/Home/End/Enter and
  Backspace. Adjust to what the apps actually do.
* Direct end-to-end proof of arrows/Home/F-keys inside a real xui client
  needs Track A's `input.rs`; the kernel queue (test) and the `xuid` encoding
  (boot self-test) are covered.

## Deferred

* `created` time / `statx` birth time (#348 area) not touched.
* Files with names containing `"` or control characters cannot be launched
  through `Launch` (rejected `EINVAL`); everything else, including spaces, works.
* `mimed` passes the path it was given; `init` now requires an absolute path,
  so `mimed`/Files must pass absolute paths (the boot self-test uses relative
  names but falls back to publish-only, as before).
* Keypad keys (other than Enter), Pause/PrintScreen, and AltGr chords other than
  the French layer are not decoded. F-keys/Delete/Insert are dropped when no
  compositor is bound (the kernel terminal never sees them).
* `xui-app/src/backend/input.rs` PageUp/PageDown focus cycling (Track A).

## Integration steps for the lead

1. Merge Track A's branch; ensure `python tools/xui/build.py` writes
   `target/xui/xui-editor.elf`, `xui-files.elf`, `xui-paint.elf` (names must
   match: `xui-<id>.elf`).
2. Flip `SHIP_DOCUMENT_APPS` to `true` in `build.rs` (it appends the three to
   the desktop default set; a missing ELF then fails the build on purpose).
   Add the three to `tools/xui/build.py` `BINS` if Track A did not.
3. Track A's `input.rs` changes listed above (mask, modifiers, Delete/F-keys,
   Tab focus, PageUp/Down scroll).
4. Apps print `EDITOR:UP:PASS`, `EDITOR:SAVE:PASS`, `PAINT:UP:PASS`,
   `PAINT:SAVE:PASS`, `FILES:UP:PASS`, `FILES:OPEN:PASS`; tune the three
   session JSONs to the real dialogs, then run each locally:
   `LAZYOS_DESKTOP=1 LAZYOS_XUI_APPS=<elf> LAZYOS_XUI_AUTOSTART=<stem> cargo build`
   and `python tools/screenshot/qemu_session.py --image target/lazyos.img --out shots/<app> --script tools/screenshot/examples/xui_<app>.json`.
5. `Files -> Editor/Paint`: the launcher calls `mimed` `Open` (or `init`
   `Launch(app, absolute_path, session)`); the app reads the path as the last
   non-flag `argv` item before `attempt=`.
6. Check the FAT boot image still fits with three more ELFs (plan risk).
7. Re-run `python tools/test/run.py --accel none`, the ABI bench and the xui
   workflow.

**Status: done (integration round).** All seven steps landed: `SHIP_DOCUMENT_APPS`
is `true`, `tools/xui/build.py` builds the three, `input.rs` masks modifiers and
maps Delete/Insert/F-keys with Tab/Shift+Tab focus and PageUp/PageDown scrolling,
and the three session scripts pass against the real apps. Two app-side bugs were
fixed on the way (the launcher resolved the `mimed` interface id instead of its
registered name; the platform service client closed its resolved endpoint, which
is peer death for the service). The evidence and remaining notes are in
[`xui-apps-integration.md`](xui-apps-integration.md).
