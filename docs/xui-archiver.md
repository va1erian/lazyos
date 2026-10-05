# Archiver

Archiver (`os.lazy.archiver`, cargo bin `xui-archiver`) is the desktop's
archive manager, in the vein of the 7-Zip File Manager: it browses an archive
like a folder, extracts all of it or a selection, tests it, creates archives,
adds and deletes entries, and exchanges files with Files by drag and drop.
The plan and its decisions are [`archiver-plan.md`](archiver-plan.md); the
user's guide ships with the package
([`xui-app/packages/archiver/docs/README.md`](../xui-app/packages/archiver/docs/README.md)).

Every `LAZYOS_DESKTOP=1` image ships it as a core package, listed first in the
start menu's installed section under **Accessories** (so no other row moves).
`mimed` opens `.zip .tar .gz .tgz .xz .txz .zst .tzst .7z` files in it.

```bash
python tools/run_demo.py --desktop                       # then Start -> Accessories -> Archiver
LAZYOS_DESKTOP=1 LAZYOS_XUI_AUTOSTART=archiver LAZYOS_RESET_OS=1 cargo build
python tools/screenshot/qemu_session.py --image target/lazyos.img \
    --out shots/archiver --script tools/screenshot/examples/xui_archiver.json
cargo test --manifest-path xui-app/Cargo.toml -p lazyarc -p xui-archiver -p xui-explorer
FUZZ_CASES=20000 cargo test --manifest-path xui-app/Cargo.toml -p lazyarc --release seeded
python xui-app/crates/archive/tests/fixtures/make.py      # regenerate the 7-Zip/tar/xz fixtures
```

## Pieces

| Piece | Where |
|---|---|
| Format library (`lazyarc`) | `xui-app/crates/archive`: zip, tar (+gz/xz/zst), gz/xz/zst, 7z; detection, listing, streaming extraction, writers, rewrite, test, path safety, progress, seeded fuzz |
| App core (`xui-archiver`) | `xui-app/crates/archiver`: folder view, cells, jobs, commands, dialogs, drag bridge; offscreen window tests |
| Binary | `xui-app/src/bin/archiver.rs`: backend, `$HOME`, `/tmp/archiver-<pid>`, the `mimed` launcher, drag-and-drop hooks |
| Drag and drop | `xui-app/src/backend/dnd.rs`, `src/display/dnd.rs`, `src/platform/urilist.rs`, `clipboard::{offer, paste}` |
| Files as a peer | `xui-app/src/bin/files.rs`, `crates/explorer` (`shell/views.rs`, `std_platform/copy.rs`) |
| Package | `xui-app/packages/archiver` (display, input, clipboard, mimed) |
| MIME | `user/src/bin/mimed/{db,apps,selftest}.rs` |
| Samples | `/system/share/samples/archiver-sample.{zip,7z}` (`fhs::share::ARCHIVER_SAMPLE_*`) |

## Formats

| Format | Read | Write | Notes |
|---|---|---|---|
| zip | yes | yes | stored and deflate (miniz_oxide via flate2); zip64; UTF-8 and CP437 names; Unix modes, symlinks, extended timestamps; CRC checked; other methods (Deflate64, BZip2, LZMA) list and fail only their own entries |
| tar | yes | yes | ustar, pax (`path`, `linkpath`, `size`, `mtime`), GNU long names and base-256 sizes; written as ustar with a pax header when a field does not fit |
| tar.gz / gz | yes | yes | multi-member gzip; a `.gz` names its entry by the header's file name |
| tar.zst / zst | yes | yes | `ruzstd`; several frames; written at its `Fastest` level (its only implemented one) |
| tar.xz / xz | yes | no | `lzma-rs`, block checks verified; its encoder writes literals only, so the app never writes xz. `lzma-rs` holds one xz block in memory while it checks it |
| 7z | yes | no | LZMA, LZMA2, Deflate and Copy folders, encoded headers, CRCs; AES-encrypted entries list with `*`; BCJ/BZip2/delta chains list and fail their own reads |

Detection trusts content, not names: the magic bytes pick zip, 7z, gzip, xz
or zstd, and a compressed stream counts as a tarball only when its first
decompressed block is a tar header (or all zeros, an empty tarball).

## Behaviour worth knowing

* **Jobs** (open, extract, test, create, add, delete, open inside) run on a
  worker thread with an `Arc<Progress>`; the window polls it on a 100 ms timer
  (the LazyOS backend has no cross-thread waker) and shows a bar with Cancel.
* **Rewrites** go to `.<name>.partial-<pid>` beside the archive and are
  renamed over it only on success; a zip rewrite copies kept members' bytes
  raw. An added path replaces the member of the same path.
* **Extraction safety** (`lazyarc::safety`): names with `..`, a leading `/` or
  a drive letter are listed but never written; every folder on the way must
  be a real directory (never a link); files are created with `create_new`
  after removing what they replace; symlinks are made last and only when
  their target resolves inside the destination; set-id bits are dropped.
* **Default destinations** avoid `/system` (`Host::read_only_roots`): an
  archive opened from `/system/share/samples` extracts to `$HOME/<stem>`.

## Drag and drop

xui has no drag-and-drop vocabulary, so it is a LazyOS backend feature over
the compositor's protocol (`os.lazy.display.v1` 11–17,
[`architecture/display.md`](architecture/display.md)):

* `LazyOSBackend::on_drag_gesture(hook)`: a left press that travels more than
  6 design pixels asks the hook what the pressed widget drags. With a
  `DragOffer` the backend offers it to `clipboardd`, calls `DragStart` with the
  token (the compositor accepts it only from the surface's owner with a
  button held) and sends the widget `CaptureChanged`.
* `LazyOSBackend::on_drag_event(hook)`: `Enter`/`Over`/`Leave`, `Drop` with
  the token already pasted through `clipboardd` (so a cross-session drop is
  refused there), `Started`, `Refused` and `Ended`.
* The payload is `text/uri-list` (`platform::urilist`): `file://` URIs with
  every byte outside the unreserved set percent-encoded; decoding accepts only
  absolute local paths, no NUL, at most 1024 paths, within the clipboard's
  8 KiB inline cap.
* xui's list and icon views select on press, so pressing a row of a
  multi-selection collapses it before the drag is recognised. Both apps keep
  the selection before the latest change and carry it when the current one is
  a single row of it, then restore it on screen.
* **Archiver as a source** extracts the dragged rows into
  `/tmp/archiver-<pid>/drag-<n>` before `DragStart` (a lazy offer would need a
  served endpoint) and offers those paths. **As a target** it adds the dropped
  paths to the current folder, opens a single dropped archive when none is
  open (or the open one is read-only), and otherwise asks for a new archive's
  name.
* **Files as a source** offers the window's selected tiles; **as a target**
  it copies the dropped paths into the window's folder (`copy_into`: never a
  folder into itself, `name (2)` on a clash, links copied as links) and
  refreshes.

Serial evidence: `ARCHIVER:UP:PASS`, `ARCHIVER:OPEN:PASS:<format>:<entries>`,
`ARCHIVER:EXTRACT:PASS:<files>:<skipped>`, `ARCHIVER:TEST:PASS:<files>` (or
`FAIL:<problems>`), `ARCHIVER:CREATED|ADDED|DELETED:PASS:<files>`,
`ARCHIVER:OPENINSIDE:PASS`, `ARCHIVER:DROP:<n>`, `ARCHIVER:DRAGSTART:PASS`,
`ARCHIVER:DRAG:PASS:<rows>`, `ARCHIVER:DRAGEND:<dropped>`,
`ARCHIVER:FAIL:<verb>:<error>`; Files adds `FILES:DRAG:PASS:<n>` and
`FILES:DROP:PASS:<copied>:<failed>`.
