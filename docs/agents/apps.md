# Desktop apps

Companion to [`AGENTS.md`](../../AGENTS.md); covers: Terminal, Docs, Writer, Archiver, PDF Viewer, Video Player, Paint, About, Picture Viewer, the MOD player, Mail and LazySky.

## Terminal (`xui-term`)

The desktop Terminal is `xui-app/src/bin/term.rs` and `term/` over the portable
engine `xui-app/crates/term` (`lazyterm`: parser, grid, scrollback with reflow,
selection, search, URL matching, settings); plan and as-built notes in
[`docs/terminal-plan.md`](../terminal-plan.md). It ships as the **core
package `os.lazy.terminal`** in every desktop image: a developer tool
(`kind = "developer-tool"`) that is **the user's shell** (`shell = true`), so
its shell and every command typed in it run under `tool:os.lazy.terminal`
(default-allow Messenger rules minus `devtool::DENY`) while `pkgd` and `init`
treat them as the session's owner (`pkgctl`, `powerctl`; recorded in the
app's row, core packages only: `pkgstore::shellrole`). Nothing else gets
that; the Playground stays refused. Its app id is `os.lazy.terminal` (the
bare `terminal` still launches it): a session waits for `INIT:AUTOSTART:PASS
app=os.lazy.terminal`. The proof session is `term_devtool.json`
(`docs/devtool-packages-plan.md` section 10). Settings are `confd` keys
under `user/<uid>/term/*`, changed live from Preferences (`Ctrl+,`). Menu,
find bar, close prompt and Preferences are drawn in the window (probe names
`menu:<item>`, `find:*`, `prefs:*`). Sessions: `term_xterm.json`,
`term_resize.json`, `term_font.json`, `term_select_copy.json`,
`term_scrollback.json`, `term_menu.json` (build with
`LAZYOS_DESKTOP=1 LAZYOS_UI_PROBE=1 LAZYOS_XUI_AUTOSTART=term`; `term_xterm`
also `LAZYOS_LINUXAPPS=1`). Markers: `TERM:UP|OUT|EXIT`, `TERM:MENU:<item>`,
`TERM:FIND:<n>`, `TERM:LINK:OPEN:<url>`, `TERM:BELL:<kind>`,
`TERM:NEWWIN:<cwd>`, `TERM:CLOSE:CONFIRM|CANCEL|FORCE`, `TERM:PREFS:SET:<key>`,
`TERM:SAVE:<bytes>`.

```bash
cargo test --manifest-path xui-app/Cargo.toml -p lazyterm
FUZZ_CASES=100000 cargo test --manifest-path xui-app/Cargo.toml -p lazyterm --release seeded
cargo test --manifest-path xui-app/Cargo.toml --bin xui-term   # Linux/WSL; snapshots in xui-app/target/snapshots
python tools/screenshot/qemu_session.py --image target/lazyos.img --out shots/term_menu \
    --script tools/screenshot/examples/term_menu.json
```

## Docs app and the zig toolchain

`xui-docs` turns Markdown into HTML (pulldown-cmark) and shows it in a **light view of `webd`**
(`xui_app::lightview::LightView`, over `xui_webd_client::Pane`), so `python tools/xui/build.py` builds it
with the other apps and no zig (see [`docs/xui-docs.md`](../xui-docs.md)). The OS has **one web engine,
WebKit in webd**: there is no Blitz, no `webfonts` and no in-process HTML renderer; an app that wants to show
HTML asks webd for a light view (a `LightView` in an xui window; one view for the life of the window, a new page
with `set_html`, never a view per page) and declares `os.lazy.webd.light.v1` in its manifest. zig
(`pip install ziglang==0.16.0`) is needed only for Mail's SQLite (`--mail`); without it the script skips Mail
with a warning and every other app still builds. `python tools/xui/test_zig.py` tests the toolchain helper.
`python tools/docs/run.py` builds an image with Docs and webd, runs `xui_docs.json` (wheel scrolling) and
`xui_docs_open.json` (Open dialog and `/system/share/samples/testdoc.md`) and judges the markers and pictures
(it needs webd built: `tools/webkit/webd.py`; `tools/docs/test_judge.py` is the judge's self-test).

### Writing documentation for the Docs app

Every `docs/**/*.md` ships at `/docs/os/<path>` and is read in the Docs app
(start at `docs/index.md`; the welcome page and the Home button lead there). The
app follows relative links to other `.md` files and keeps a Back/Forward/Home
history; a `#heading` link does nothing there (a light view cannot scroll to
it) but works elsewhere, so keep them correct (headings get GitHub-style ids and
the checker verifies them). It draws no images and shows raw HTML as
text, so write plain Markdown and link pages by relative path. A file over 256
KiB is not embedded. Every fenced `rhai` block must compile and every relative
link must resolve: `python tools/docs/check_snippets.py` checks both (CI:
`.github/workflows/docs.yml`; tag a deliberate fragment `rhai ignore`).
`docs/rhai/api/` (one page per `sys::` module) is generated from `idl/` by
`midlc --rhai-docs`; never edit it by hand. The guides: `docs/lazyrad/`
(LazyRAD: forms, controls, scripting, packaging, cookbook) and `docs/rhai/`
(language, the `rhai` command, `msg`, cookbook).

## LazyWriter (word processor)

`writer` (`os.lazy.writer`) is a core desktop app on xui's `xui-rich-text`
editor: `.lzw` documents, Markdown export, pictures; see
[`docs/xui-writer.md`](../xui-writer.md). Screenshot session:
`tools/screenshot/examples/xui_writer.json` (format, save, export, reopen; markers
`WRITER:UP|SAVE|EXPORT|OPEN:PASS`, build with `LAZYOS_XUI_AUTOSTART=writer`)
and `xui_writer_light.json` (light theme, build with `LAZYOS_XUI_AUTOSTART=term`).
Printing (`Ctrl+P`, `libs/ipp`, `libs/raster`, docs/printing-plan.md):
`python tools/print/run.py` builds, boots, prints through the `printd` spooler
to a fake IPP printer on the host and judges the PWG Raster page it gets
(`WRITER:PRINT:PASS:<pages>`); `--quit` quits LazyWriter once the job is
queued and checks the page still arrives whole. The queue is
`xui-app/crates/printd` (`cd xui-app && cargo test -p printd`).

## Archiver (archive manager) and desktop drag and drop

`xui-archiver` (`os.lazy.archiver`) is a 7-Zip-style archive manager shipped
in every desktop image: browse, extract, test, create, add, delete, for zip,
tar, tar.gz/zst/xz, gz/zst/xz and 7z (xz and 7z read-only). Formats live in
`xui-app/crates/archive` (`lazyarc`, host-tested and fuzzed, real 7-Zip/xz
fixtures in `tests/fixtures`), the app in `xui-app/crates/archiver`; see
[`docs/xui-archiver.md`](../xui-archiver.md). It added drag and drop for xui
apps: `LazyOSBackend::on_drag_gesture`/`on_drag_event` over the compositor's
`DragStart`/`Drop` and `clipboardd` tokens, carrying `text/uri-list`; Files
is a source and a target. The `xui-app` lib tests are Linux-only: on Windows
build them for musl (`tools/xui/build.py`'s rust-lld settings) and run the
ELF under WSL. Session: `tools/screenshot/examples/xui_archiver.json`
(`LAZYOS_XUI_AUTOSTART=archiver`; markers `ARCHIVER:UP|OPEN|EXTRACT|TEST:PASS`).

```bash
cargo test --manifest-path xui-app/Cargo.toml -p lazyarc -p xui-archiver -p xui-explorer
FUZZ_CASES=20000 cargo test --manifest-path xui-app/Cargo.toml -p lazyarc --release seeded
```

## PDF Viewer (`os.lazy.pdf`)

`xui-pdf` is a core desktop app in every desktop image: pages rendered in
tiles on worker threads by `lazypdf` (`xui-app/crates/pdf`, over the
pure-Rust `hayro`), the window in `xui-app/crates/pdfview`; plan and P0
findings in [`docs/pdf-reader-plan.md`](../pdf-reader-plan.md). The
sample (`/system/share/samples/lazyos-sample.pdf`) and the test files are
written by `tools/pdf/make_sample.py` (PyMuPDF; `--check`). Session:
`tools/screenshot/examples/xui_pdf.json` (`LAZYOS_XUI_AUTOSTART=pdf`;
markers `PDF:UP:PASS`, `PDF:OPEN:PASS:<path>:<pages>`,
`PDF:PAGE:DRAWN:<page>:<ms>:<render ms>`, `PDF:ZOOM:<percent>`).

```bash
cargo test --manifest-path xui-app/Cargo.toml -p lazypdf -p xui-pdfview
FUZZ_CASES=20000 cargo test --manifest-path xui-app/Cargo.toml -p lazypdf --release seeded
```

## Video Player (`os.lazy.video`)

`xui-video` is a core desktop app in every desktop image: a player thread
that demuxes, decodes and paces frames against a media clock
(`xui-app/crates/video`, `lazyvideo`, portable and host-tested), under an
xui window with a toolbar, seek bar and status bar (`xui-app/crates/videoview`).
It plays Motion JPEG in AVI today; H.264, MP4, audio, HTTPS and YouTube are
the phases of [`docs/video-player-plan.md`](../video-player-plan.md), which
also holds the feasibility numbers. The sample is
`/system/share/samples/lazyos-sample.avi` (`tools/video/make_sample.py`).
Session: `tools/screenshot/examples/xui_video.json` (`LAZYOS_XUI_AUTOSTART=video`;
markers `VIDEO:UP:PASS`, `VIDEO:OPEN:PASS:<path>:<w>x<h>:<fps>:<frames>:<codec>`,
`VIDEO:POS:<ms>:shown=<n>:dropped=<n>:fps=<n>`, `VIDEO:END:PASS`).

```bash
cargo test --manifest-path xui-app/Cargo.toml -p lazyvideo -p xui-videoview
python tools/video/make_sample.py --check
cargo run --release --manifest-path xui-app/Cargo.toml -p lazyvideo --example decode_bench -- clip.avi
```

## Paint (`os.lazy.paint`)

`xui-paint` is a core desktop app in every desktop image: a layered image
editor (selections, clipboard, shapes, gradient, text in Liberation, adjustments;
OpenRaster, PNG, JPEG, BMP) in `xui-app/crates/paint` (portable, host-tested and
snapshot-tested), launched by `xui-app/src/bin/paint.rs`; plan and status in
[`docs/paint-plan.md`](../paint-plan.md), keys and layout in the crate's
README. Sessions (`LAZYOS_XUI_AUTOSTART=paint LAZYOS_UI_PROBE=1`):
`tools/screenshot/examples/xui_paint.json` and `xui_paint_editor.json`; markers
`PAINT:UP:PASS`, `PAINT:OPEN:PASS:<path>:<w>x<h>:<layers>`,
`PAINT:SAVE:PASS:<path>:<format>`. In a session step `offset` goes inside the
`click_at` target and the window name is the current title.

```bash
cargo test --manifest-path xui-app/Cargo.toml -p xui-paint            # also --features text
python tools/screenshot/qemu_session.py --image target/lazyos.img --out shots/paint_editor     --script tools/screenshot/examples/xui_paint_editor.json
```

## About LazyOS (`os.lazy.about`)

`xui-about` is a core desktop app in every desktop image: a few pages on what
LazyOS is, over a real-time 3D scene (a chrome torus knot, torus, icosahedron
or icosphere above a synthwave floor) drawn by a small software rasteriser,
all in the portable crate `xui-app/crates/about` (`mesh`, `raster` with a
z-buffer and perspective-correct per-pixel shading, `backdrop`, `scene`, the
window in `view`/`panel`); the bin only launches it. The internal resolution
adapts to the frame cost. Keys: drag spins, wheel zooms, Space changes shape,
`W` cycles solid / solid + wire / wire, Left/Right turn pages, Esc closes.
Session `tools/screenshot/examples/xui_about.json`
(`LAZYOS_XUI_AUTOSTART=about LAZYOS_UI_PROBE=1`; markers `ABOUT:UP:PASS`,
`ABOUT:FPS:<fps>:<w>x<h>:div=<n>:work=<ms>:tris=<n>`, `ABOUT:SHAPE:`,
`ABOUT:STYLE:`, `ABOUT:PAGE:<n>`, `ABOUT:QUIT:PASS`).

```bash
cargo test --manifest-path xui-app/Cargo.toml -p xui-about   # snapshots in xui-app/target/snapshots/about-*.png
cargo run --release --manifest-path xui-app/Cargo.toml -p xui-about --example about --features desktop
python tools/screenshot/qemu_session.py --image target/lazyos.img --out shots/about \
    --script tools/screenshot/examples/xui_about.json
```

## Picture Viewer (`LAZYOS_PICTURES=1`)

`os.lazy.pictures` is a Windows XP-style picture viewer written in LazyRAD
(`lazyrad-os/samples/pictures`, LazyRAD's `PictureBox` control), shipped as a
core package that carries the player and the project: PNG, JPEG, BMP and GIF,
paging through the folder, fit/zoom/pan, rotation, a slide show, Edit through
`mimed`. A picture double-clicked in Files reaches the script as
`app.documents` (`lrplay` treats a positional path that is not a project as a
document and grants its folder read-only). See
[`docs/lazyrad-pictures.md`](../lazyrad-pictures.md).

```bash
python tools/run_demo.py --pictures             # desktop + the viewer
cd lazyrad-os && cargo test --test pictures     # the real form offscreen (snapshots in target/snapshots)
python tools/screenshot/qemu_session.py --image target/lazyos.img --out shots/pictures \
    --script tools/screenshot/examples/lazyrad_pictures.json   # image: LAZYOS_DESKTOP=1 LAZYOS_PICTURES=1 LAZYOS_UI_PROBE=1 LAZYOS_XUI_AUTOSTART=term
```

A form script must not give its own functions a control method's name and
arity (`fn best_fit()` beside `picture1.best_fit()`): Rhai calls the script
function for the method too, and it recurses (LazyRAD's lint warns about it in the IDE).

## LazyRAD MOD player (`modplay` module, `.lzp` package)

A ProTracker player written as a LazyRAD project (`lazyrad-os/samples/modplayer`)
over the player's `modplay` script module (`lazyrad-os/src/tracker`, mixing in
`libs/modplay`, sound through `audiod`); it is embedded as `/system/share/lazyrad/modplayer`
with LazyRAD and packaged as `/system/share/samples/modplayer.lzp` (`LAZYOS_MODPLAYER=1`); see
[`docs/lazyrad-modplay.md`](../lazyrad-modplay.md). The verdict on its sound is
the recording, judged against a host render:

```bash
python tools/run_demo.py --modplayer          # desktop, LazyRAD, the package, sound card
python tools/lazyrad/modplayer_run.py         # build, Terminal + installed sessions, record, judge
cd lazyrad-os && cargo test                   # tracker unit tests; tests/modplayer.rs runs the real form offscreen
python tools/lazyrad/gen_demo_song.py --check # the built-in song (an original, CC0) is current
```

## Mail (esMail over TLS, `LAZYOS_MAIL=1`)

`xui-mail` (`os.lazy.mail`, `xui-app/mail/`) is va1erian/esmail's IMAP/SMTP
core (a pinned git dependency, built with its `rustls` feature over
`nettls-crypto`) behind a xui window, its reading pane a light view of webd (HTML mail, scripts off, no
network: `reader.rs`, `tools/mail/hostile.py` is the hostile message the harness plants); zig builds
it for SQLite (`tools/xui/build.py --mail`). Passwords live only in memory (`secrets.rs`).
See [`docs/mail.md`](../mail.md).

```bash
python tools/run_demo.py --mail          # desktop + HTTPS + Mail
python tools/mail/run.py                 # mock IMAPS/SMTPS server under the test CA, session, judge
python tools/mail/run.py --label-trace   # the same, listing LABEL:DENY lines
cargo test --manifest-path xui-app/Cargo.toml -p xui-mail
```

## LazySky Bluesky client (`LAZYOS_LAZYSKY=1`)

LazySky (`os.lazy.lazysky`) is a Bluesky client built as a headless core
(`xui-app/crates/lazysky-core`: no xui type, no LazyOS crate, so it also runs
under `sky_cli`) and a portable xui window (`xui-app/crates/lazysky-ui`, which
also has a `desktop` example for Windows/Linux/macOS); `xui-app/src/bin/lazysky.rs`
only launches it and supplies the LazyOS `Platform`. Its feed is one
custom-painted, virtualized `FeedView` with anchored scrolling; text is shaped
on a layout thread and pictures decoded on image threads, and the UI thread
never does network I/O, JSON, decoding or shaping (the plan's R1 to R8,
`docs/lazysky-plan.md` section 5). **Never push a clip on the feed's normal
paint path**: an active clip made every draw several times dearer (the paint
bench catches it). The package (`xui-app/packages/lazysky`) is shipped only by
`LAZYOS_LAZYSKY=1` images, which need `LAZYOS_DESKTOP=1` and `LAZYOS_NETD=1`.
See [`docs/lazysky.md`](../lazysky.md).

```bash
python tools/run_demo.py --lazysky        # desktop + networking + the client
cd xui-app && cargo test -p lazysky-core -p lazysky-ui
cargo test -p lazysky-ui --release --test bench -- --ignored --nocapture   # paint p99 <= 4 ms
cargo run --release -p lazysky-ui --example desktop --features desktop     # the window on the host
python tools/nettls/licenses.py --lazysky # GPLv2-compatible tree (no ring/aws-lc/tokio/reqwest)
```

The offscreen window tests write `xui-app/target/snapshots/lazysky-*.png`:
Read them. Serial markers are `SKY:*` (listed in `docs/lazysky.md`); a
password or token must never appear in any log.
