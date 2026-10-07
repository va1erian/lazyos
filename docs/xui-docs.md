# The Docs app: Markdown rendered by Blitz

`xui-docs` (the core package `os.lazy.docs`, `xui-app/packages/docs/`) shows a Markdown file in a `xuid` window: wrapped text,
headings, lists, tables, code and block quotes, scrolled with the mouse wheel,
`PageUp`/`PageDown` or the arrow keys. With no argument it shows a built-in
welcome page that doubles as a syntax tour. It is the first user of
[`xui-blitz`](https://github.com/va1erian/xui/tree/main/crates/xui-blitz)
`BlitzView` on LazyOS and the intended renderer for documentation pages. It fetches
nothing: images are not loaded and links are drawn but not followed.

## How it works

```
file (or welcome.md) --pulldown-cmark--> HTML --Blitz (engine thread)--> RGBA frame
                                                                           |
                                       xuid window  <-- xui canvas <-- BlitzView (UI thread)
```

* **`xui-app/docs/src/lib.rs`** turns Markdown into one styled HTML page. Raw
  HTML in the source is escaped to text, so a document cannot inject markup into
  the layout engine, and input is truncated at 1 MiB on a character boundary.
  It is pure Rust and unit-tested on the host.
* **`xui-app/docs/src/main.rs`** creates a `BlitzView` filling the window on the
  LazyOS backend. Blitz lays the page out and paints it on an engine thread (a
  `std::thread`, so the Linux ABI's `clone`/`futex` matter) and posts a frame back
  to the UI thread, which blits it.
* **Fonts.** Blitz draws its own glyphs from font files and finds none by
  itself on LazyOS (no fontconfig), so the `webfonts` crate registers the
  Liberation fonts a desktop image installs (Sans, Serif, Mono, all four
  styles) with `xui_blitz::register_font` at start; without that a page shows
  no text. The window's own text (the toolbar) stays in Droid Sans.
* **Scrolling.** Blitz draws overlay scrollbars on the page (they fade after a
  scroll and can be dragged). The wheel needs the whole input chain; see
  *Mouse wheel* in [`architecture/display.md`](architecture/display.md).
* **Opening documents.** A toolbar above the page has an **Open...** button and
  shows the current path; `Ctrl+O` does the same. Both raise the portable
  `FileDialog` (the Editor's picker) over xui's `StdFileSystem`
  (`xui-app/docs/src/app.rs`). It filters to Markdown with an
  "All files" fallback and starts in the current document's folder. A file that
  cannot be read shows an error page (and `DOCS:OPEN:FAIL:<path>`) instead of
  ending the app. `app.rs` is the window; `main.rs` only starts the platform.
* **One view per document.** Opening a document creates a fresh view and drops
  the old one (which destroys its node and stops its engine thread): a
  carry-over from the litehtml view, whose painter kept per-document font ids
  across loads. `BlitzView::load_html` would also do.
* **Test document.** `xui-app/docs/testdata/testdoc.md` ships in every image as
  `/system/share/samples/testdoc.md` (embedded by `build.rs`). It covers every construct the viewer
  draws and is long enough to scroll; the unit tests render it and
  `tools/screenshot/examples/xui_docs_open.json` opens it through the dialog.
* **Start menu.** LazyShell's start menu lists Docs in the Office
  submenu (its package's `category`). A build that skipped Docs does not ship
  the app, and the launch is answered as unavailable like any unshipped app.

## Build

Docs renders with Blitz (`xui-blitz`), which is pure Rust: `tools/xui/build.py`
builds it with the other apps, with the toolchain's bundled lld and no zig. It
is still a package of its own (`xui-app/docs`) so that the other apps do not
link Blitz. (Until issue #649 it used litehtml, which is C++ and needed the zig
toolchain; zig is now only for Mail's SQLite, `tools/xui/zig.py`.)

## Known limits

* Links and images are not followed or loaded: the viewer has no fetching
  code; relative links to other documents are the natural next step.
* The LazyOS backend delivers a `Resize` when the window is resized
  (`xui-app/src/backend/input.rs`), but the app places its toolbar and view at
  fixed rectangles taken from the initial client size and does not handle it,
  so the view keeps that size.
* Keyboard scrolling (`PageDown`, arrows) needs the page to have focus, which a
  click on it gives; after a dialog closes the wheel works at once but the keys
  need that click (`BlitzView` does not expose a way to focus itself).
* Task-list checkboxes: the Markdown extension is off, a choice made when
  the engine (litehtml) did not render `<input>`; Blitz may, so it can be
  revisited.
* `mimed` registers Docs for the `open` and `view` verbs of `text/markdown`.
  Docs is an optional app, so `open` names the Editor as a fallback: an
  image without Docs opens Markdown in the Editor, which is also the `edit`
  verb.
