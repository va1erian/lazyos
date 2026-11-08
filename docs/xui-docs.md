# The Docs app: Markdown in xui's Markdown view

`xui-docs` (the core package `os.lazy.docs`, `xui-app/packages/docs/`) shows a Markdown file in a `xuid` window: wrapped text,
headings, lists, task lists, tables, code and block quotes, scrolled with the mouse wheel, its scroll bar,
`PageUp`/`PageDown`, `Home`/`End` or the arrow keys. With no argument it shows a built-in welcome page that
doubles as a syntax tour. The page is drawn in the app by **`xui-markdown`**, xui's small read-only Markdown
view: it parses CommonMark with the GFM tables, strike-through and task lists, lays out the blocks on screen
first and the rest in slices on a timer, and paints from cached layouts, so a long document opens at once and
scrolls without shaping text. Docs needs no other service to show a page.
(Until this change the page was HTML drawn by a light view of [`webd`](webd.md), WebKit; before that by Blitz
and litehtml. The OS still has one web engine; a Markdown page does not need one.)

## How it works

```
file --worker thread: read (1 MiB at most) + xui_markdown::Document::parse--> Arc<Document>
                                                                                  |
                       xuid window <-- xui canvas <-- MarkdownView (UI thread) <--'
                                                        ^-- mouse, wheel, keys, Ctrl+A/Ctrl+C
```

* **`xui-app/docs/src/lib.rs`** reads a document bounded at 1 MiB (cut on a character boundary, with a
  notice) and parses it, and writes the page for a document that cannot be read (the message escaped, so a
  path shows as written). Raw HTML in a document is shown as text: nothing in it is markup to any engine. It
  is pure Rust and unit-tested on the host.
* **`xui-app/docs/src/links.rs`** says what a click on a link does (below).
* **`xui-app/docs/src/app.rs`** is the window: a toolbar and one `MarkdownView` that fills the rest, laid out
  by xui (`column`, `fill(1)`), so it follows the window's size. `main.rs` only starts the platform.
* **The UI thread does no I/O.** Opening a document starts a worker thread that reads and parses it and hands
  the `Document` back through the window's proxy; a slower earlier load that finishes after a newer one is
  dropped. The built-in welcome page is parsed in place.
* **Styling** comes from `xui_app::markdown`, shared with the Messenger Playground's doc pane: the UI face for
  text, JetBrains Mono for code, and ```` ```rhai ```` blocks coloured by the code editor's Rhai lexer
  (`xui_app::markdown::RhaiCode`). A code line wider than the page wraps onto more rows. Colours follow the
  desktop theme (light or dark) live.
* **Selecting and copying** are the view's: drag, Shift+click, a double-click for a word, a triple-click for
  a block, `Ctrl+A`; `Ctrl+C` copies plain text (list markers kept, code verbatim, tables as tab-separated
  rows) to the session clipboard.
* **Links.** The view reports a click and Docs decides (`xui_docs::links::classify`), except a `#section`
  link to one of the page's own headings, which the view scrolls to (GitHub's heading anchors). A link to
  another Markdown file (`other.md`, `../x.md#part`) opens in this window, resolved against the folder of the
  document on show (the documentation root for the welcome page), and scrolls to the heading its fragment
  names; any other file goes to `mimed` (`open_path`); `http:`, `https:` and `mailto:` go to the app
  registered for the scheme (`open_url`: the browser, Mail); every other scheme is refused. Markers:
  `DOCS:LINK:<href>` then `DOCS:LINK:OPEN:<path>`, `:FILE:<path>`, `:URL:<url>` or `:REFUSED:<href>`.
* **Opening documents.** A toolbar above the page has an **Open...** button and
  shows the current path; `Ctrl+O` does the same. Both raise the portable
  `FileDialog` (the Editor's picker) over xui's `StdFileSystem`. It filters to Markdown with an
  "All files" fallback and starts in the current document's folder. A file that
  cannot be read shows an error page (and `DOCS:OPEN:FAIL:<path>`) instead of
  ending the app.
* **History.** **Back**, **Forward** (`Alt+Left`, `Alt+Right`) and **Home** (the documentation
  index, `fhs::docs::INDEX`) walk the documents visited, kept by `history::History` (bounded, the
  welcome page is a place in it); opening what is already on show only scrolls to the link's heading.
  Markers: `DOCS:NAV:PASS:<back|forward|home>:<path>`.
* **Evidence.** `DOCS:RENDER:PASS` once the welcome page has painted, `DOCS:OPEN:PASS:<path>` when a
  document is read and `DOCS:RENDER:PASS:<path>` once it has painted (the view's `on_painted`).
* **Test document.** `xui-app/docs/testdata/testdoc.md` ships in every image as
  `/system/share/samples/testdoc.md` (embedded by `build.rs`). It covers every construct the viewer
  draws and is long enough to scroll; the unit tests parse it and
  `tools/screenshot/examples/xui_docs_open.json` opens it through the dialog.
* **Start menu.** LazyShell's start menu lists Docs in the Office
  submenu (its package's `category`).
* **Permissions** (`xui-app/packages/docs/manifest.toml`): the display, the cursor, input, the clipboard and
  `os.lazy.mimed.v1` (for the links that leave the page). Nothing about the network, files beyond the
  user's own, or the web engine.

## Build and test

Docs is pure Rust: `tools/xui/build.py` builds it with the other apps (no zig, no webd).

```bash
cargo test --manifest-path xui-app/Cargo.toml -p xui-docs --lib   # reading, links, history (Linux/WSL)
python tools/docs/test_judge.py                                     # the harness judges fail when they should
python tools/docs/run.py                                            # build, boot, the two sessions, judge
```

`tools/docs/run.py` builds a desktop image with Docs, runs `xui_docs.json` (wheel scrolling) and
`xui_docs_open.json` (Open dialog and `/system/share/samples/testdoc.md`) and judges the markers and the
pictures (`tools/docs/judge.py`). The `docs` shard of the `xui` CI workflow runs the wheel session and its
picture check on every change.

## Known limits

* No find bar and no text zoom yet.
* Images show as their alt text (the view does not load pictures); relative links to files other than
  Markdown go to `mimed`.
* `mimed` registers Docs for the `open` and `view` verbs of `text/markdown`.
  Docs is an optional app, so `open` names the Editor as a fallback: an
  image without Docs opens Markdown in the Editor, which is also the `edit`
  verb.
