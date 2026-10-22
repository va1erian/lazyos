# The Docs app: Markdown shown in a light view of webd

`xui-docs` (the core package `os.lazy.docs`, `xui-app/packages/docs/`) shows a Markdown file in a `xuid` window: wrapped text,
headings, lists, tables, code and block quotes, scrolled with the mouse wheel,
`PageUp`/`PageDown` or the arrow keys. With no argument it shows a built-in
welcome page that doubles as a syntax tour. The page is drawn by WebKit, in
[`webd`](webd.md), the resident daemon that owns the web engine for the session: Docs asks it for a
**light view** (`os.lazy.webd.light.v1`, no network, no scripts, no storage, locked navigation) and
shows the picture it paints. Docs fetches nothing: images are not loaded.
(Until the webd plan's P3 it drew the page with Blitz, a pure-Rust engine in the Docs process, and before that
with litehtml; both are gone: the OS has one web engine, docs/webd-plan.md section 9.2.)

## How it works

```
file (or welcome.md) --pulldown-cmark--> HTML --LoadHtml--> webd (WebKit, light view) --Frame (shared slot)--> picture
                                                                                                                  |
                                       xuid window  <-- xui canvas <-- xui_app::lightview::LightView (UI thread) <-'
                                                                          ^-- mouse, wheel, keys, Ctrl+C --> webd
```

* **`xui-app/docs/src/lib.rs`** turns Markdown into one styled HTML page. Raw
  HTML in the source is escaped to text, so a document cannot inject markup into
  the layout engine, and input is truncated at 1 MiB on a character boundary.
  It is pure Rust and unit-tested on the host.
* **`xui-app/docs/src/links.rs`** says what a click on a link does (below).
* **`xui-app/docs/src/app.rs`** is the window: a toolbar and one `LightView` that fills the rest, laid out
  by xui (`column`, `fill(1)`), so it follows the window's size. `main.rs` only starts the platform.
* **`xui_app::lightview::LightView`** (`xui-app/src/lightview/`, shared with Mail) is the xui widget over
  `xui_webd_client::Pane` (`xui-app/crates/webd-client/src/pane.rs`, portable and host-tested against the
  service half): it creates the view at the control's size, keeps it, loads the newest page, follows
  resizes, forwards the pointer, the wheel and the keyboard (Ctrl+C asks the engine to copy the selection;
  the text arrives as `ClipboardWrite` and goes to the clipboard), and **recreates the view and loads the
  page again if webd loses it**. The view's page scrolls inside WebKit; the picture is a whole frame, copied
  out of a shared slot.
* **One view for the life of the window.** Opening a document is a new page in the same view
  (`LightView::set_html`). A view that is destroyed leaves a parked proxy behind in webd (docs/webd.md,
  "Limits"), so Docs never makes a view per document.
* **When webd is not running** (it starts with the session; the user can have stopped it from the tray, or
  the image has none) the window says "The web engine is not running", keeps trying every quarter
  second, and the page appears when the service does. After three losses in a row with no picture in
  between it says the engine stopped and tries again on the next key press. There is no *Start* button:
  starting a program needs `init`'s interface, which a light-view client is deliberately not given.
* **Links.** A light view loads nothing by itself: it reports the click and Docs decides
  (`xui_docs::links::classify`). A link to another Markdown file (`other.md`, `../x.md#part`) opens in
  this window, resolved against the folder of the document on show (the documentation root for the welcome
  page); any other file goes to `mimed` (`open_path`); `http:`, `https:` and `mailto:` go to the app
  registered for the scheme (`open_url`: the browser, Mail); a `#section` link does nothing (a light view
  cannot scroll to an anchor); every other scheme is refused. Markers: `DOCS:LINK:<href>` then
  `DOCS:LINK:OPEN:<path>`, `:FILE:<path>`, `:URL:<url>` or `:REFUSED:<href>`.
* **HiDPI.** A light view has no scale of its own; the widget asks for `html { zoom: <dpi/96> }` as the view's
  style, which lays the page out at the window's scale while the pointer stays in view pixels.
* **Opening documents.** A toolbar above the page has an **Open...** button and
  shows the current path; `Ctrl+O` does the same. Both raise the portable
  `FileDialog` (the Editor's picker) over xui's `StdFileSystem`. It filters to Markdown with an
  "All files" fallback and starts in the current document's folder. A file that
  cannot be read shows an error page (and `DOCS:OPEN:FAIL:<path>`) instead of
  ending the app.
* **History.** **Back**, **Forward** (`Alt+Left`, `Alt+Right`) and **Home** (the documentation
  index, `fhs::docs::INDEX`) walk the documents visited, kept by `history::History` (bounded, the
  welcome page is a place in it); opening what is already on show adds nothing. Markers:
  `DOCS:NAV:PASS:<back|forward|home>:<path>`.
* **Test document.** `xui-app/docs/testdata/testdoc.md` ships in every image as
  `/system/share/samples/testdoc.md` (embedded by `build.rs`). It covers every construct the viewer
  draws and is long enough to scroll; the unit tests render it and
  `tools/screenshot/examples/xui_docs_open.json` opens it through the dialog.
* **Start menu.** LazyShell's start menu lists Docs in the Office
  submenu (its package's `category`).
* **Permissions** (`xui-app/packages/docs/manifest.toml`): the display, the cursor, input, `os.lazy.webd.light.v1`
  and `os.lazy.mimed.v1` (for the links that leave the page). Nothing about the network or files.

## Build and test

Docs is pure Rust: `tools/xui/build.py` builds it with the other apps (no zig). It is a package of its own
(`xui-app/docs`) so its Markdown engine and tests stay out of `xui-app`. **Every desktop image ships webd**
(`LAZYOS_WEBD`, docs/build-switches.md): without the program Docs still starts and says the web engine is
not running.

```bash
cargo test --manifest-path xui-app/Cargo.toml -p xui-docs -p xui-webd-client   # Markdown, links, the pane (Linux/WSL for xui-docs)
python tools/docs/run.py                                                        # build, boot, the two sessions, judge (needs webd built: tools/webkit/webd.py)
```

`tools/docs/run.py` builds an image with Docs and webd, runs `xui_docs.json` (wheel scrolling) and
`xui_docs_open.json` (Open dialog and `/system/share/samples/testdoc.md`) and judges the markers and the
pictures (`tools/docs/judge.py`, self-tested by `test_judge.py`). The CI job of the `docs` shard cannot build
WebKit, so it only checks that Docs starts (`DOCS:UP:PASS`).

## Known limits

* A light view has no `Find`, `Zoom` or `Scroll` yet (docs/webd-plan.md, section 3.6): no find bar, no
  text zoom, no jump to an anchor. Selecting text with the mouse and `Ctrl+C` work.
* Images are not loaded (a light view has no network and Docs passes no `data:` pictures); relative links to
  files other than Markdown go to `mimed`.
* Task-list checkboxes: the Markdown extension is off.
* `mimed` registers Docs for the `open` and `view` verbs of `text/markdown`.
  Docs is an optional app, so `open` names the Editor as a fallback: an
  image without Docs opens Markdown in the Editor, which is also the `edit`
  verb.
