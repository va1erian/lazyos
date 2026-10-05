# Archiver: a 7-Zip-style archive manager

**Status:** implemented (phases A1–A6 below). User-facing description:
[`xui-archiver.md`](xui-archiver.md).

LazyOS has no way to look inside a `.zip` or a `.tar.gz` from the desktop.
Archiver (`os.lazy.archiver`) is a core desktop app in the vein of the 7-Zip
File Manager: it browses an archive like a folder, extracts all of it or a
selection, tests it, creates new archives, adds and deletes entries, and
takes part in drag and drop with Files in both directions.

## Goals

* Browse an archive's tree: one folder at a time, a report list (name, size,
  packed size, modified, method), sortable columns, Up/Backspace, folders
  first. Double-clicking a file extracts it to a temporary folder and opens it
  with the handler `mimed` names (as 7-Zip's "open inside").
* Read **zip** (stored, deflate; zip64), **tar** (ustar, pax, GNU long
  names), **tar.gz / tgz**, **tar.xz / txz**, **tar.zst**, single-file **gz**,
  **xz** and **zst**, and **7z** (LZMA, LZMA2 and copy coders, encoded headers;
  read-only).
* Write zip (deflate or stored, by level), tar, tar.gz, tar.xz, tar.zst and the
  single-file gz/xz/zst. Add files to and delete entries from every writable
  format; a zip rewrite copies kept entries' compressed bytes without
  recompressing them.
* Test: decompress everything and check every CRC/checksum without writing.
* Long operations run on a worker thread with a progress bar and Cancel; the
  window stays responsive.
* **Drag and drop**, through the compositor's existing protocol
  (`os.lazy.display.v1` methods 11–17, issue #145) and `clipboardd` tokens:
  * drop files on Archiver: an archive opens; anything else is added to the
    open archive's current folder, or, with no archive open, becomes a new
    archive (a Save dialog picks its name and format);
  * drag entries out of Archiver: they are extracted to a private temporary
    folder and offered as a `text/uri-list` of those paths;
  * Files becomes a drag source (its selected tiles) and a drop target
    (dropped paths are copied into the folder), so the two apps work together.
* Security first: an archive is untrusted input. No entry is written outside
  the chosen folder (absolute paths, `..`, and paths through a symlink the
  archive itself created are refused), symlinks are created only when their
  target stays inside the destination, sizes and counts are never trusted to
  pre-allocate, and every parser is fuzzed with seeded inputs on the host.

## Non-goals (for now)

* Encryption (zip ZipCrypto/AES, 7z AES): listed with a lock note, refused on
  extraction.
* Writing 7z, rar (proprietary), bzip2 (no pure-Rust permissive crate in the
  lock), multi-volume archives, BCJ/delta 7z filters.
* Lazy (on-drop) extraction for drags: the clipboard's lazy offers need a
  served endpoint; a drag extracts before `DragStart` instead.

## Architecture

| Piece | Where | What |
|---|---|---|
| Formats | `xui-app/crates/archive` (`lazyarc`) | Pure Rust, no UI: detection, listing, streaming extraction, writers, rewrite (add/delete), test, path safety, progress/cancel. Host-tested and fuzzed. |
| App core | `xui-app/crates/archiver` (`xui-archiver`) | The xui `App`: model (folder view over the entry list, sorting, selection), commands, dialogs, the worker-thread job runner, the drag bridge. Host-tested offscreen. |
| Binary | `xui-app/src/bin/archiver.rs` | LazyOS side: backend, theme, argv path, launcher (`mimed`), drag-and-drop wiring, serial evidence `ARCHIVER:*`. |
| DnD in the backend | `xui-app/src/backend/dnd.rs`, `src/display/dnd.rs`, `src/platform/dnd.rs` | Decodes `DragEnter/Over/Leave/Drop/DragEnded`, pastes a drop's token, detects a press-and-drag gesture, offers a payload and calls `DragStart`; `text/uri-list` encoding. |
| Files DnD | `xui-app/src/bin/files.rs`, `crates/explorer` | Selection as a drag source; a folder window as a drop target (recursive copy with `name (2)` on conflicts). |
| Package | `xui-app/packages/archiver` | Manifest (display, input, clipboard, mimed), icons, README. |
| MIME | `user/src/bin/mimed/{db,apps}.rs` | `.zip .tar .gz .tgz .xz .txz .zst .7z` types, opened by Archiver. |

xui-core has no drag-and-drop vocabulary, so drag and drop stays a LazyOS
backend feature: the backend calls app-registered hooks
(`on_drag_event`, `on_drag_gesture`) and the binaries turn them into their
apps' messages through `Ui::proxy` (drained in the same loop tick).

### Drag gesture

The backend remembers a left press (window, node, point). When the pointer
moves more than 6 px (scaled) with the button still down, it asks the app's
gesture hook for a payload (`mime`, `bytes`). With one, it offers it to
`clipboardd`, calls `DragStart(surface, token, mime)`, and sends the pressed
node a `MouseUp`/`CaptureChanged` so the widget leaves its pressed state (the
compositor owns the pointer until `DragEnded`). A press that collapsed a
multi-selection (xui's list and icon views select on press) is undone by the
app: when the selection just went from several rows to one of them, the drag
carries the earlier selection.

### Threading

A job (`open`, `extract`, `test`, `create`, `add`, `delete`) runs on a
`std::thread` with an `Arc<Progress>` (bytes done/total, current name,
cancel flag) and a result slot; the app polls it on a 100 ms timer while a job
runs (LazyOS's backend has no cross-thread waker; Net Tools uses the same
pattern). Writes go to a temporary sibling and are renamed over the target only
when the job succeeds, so a cancelled or failed rewrite leaves the original
intact.

## Phases

* **A1 formats** — `lazyarc`: zip/tar/gz/xz/zst readers and writers, 7z
  reader, path safety, extract/test/create/rewrite, unit tests, round trips,
  hostile-archive tests, seeded fuzz.
* **A2 app core** — model, commands, worker jobs, dialogs, toolbar, status
  and progress; offscreen window tests.
* **A3 backend drag and drop** — display decode, gesture, offer/paste,
  uri-list; host tests of decoding and gesture thresholds.
* **A4 Files** — drag source and drop target.
* **A5 integration** — package, icons, `core_packages.py`, `build.py`,
  `xui_embed.rs`, `core_packages.rs`, `run_demo.py`, lazygui catalog and tests,
  mimed types, start-menu category, `xui_archiver.json` session.
* **A6 verification** — host tests, then a QEMU session that opens a sample
  archive, extracts, creates a new archive, and drags between Files and
  Archiver, judged from serial markers and screenshots.
