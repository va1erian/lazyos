# The Docs app: Markdown rendered by litehtml

`xui-docs` (`XDOCS.ELF`) shows a Markdown file in a `xuid` window: wrapped text,
headings, lists, tables, code and block quotes, scrolled with the mouse wheel,
`PageUp`/`PageDown` or the arrow keys. With no argument it shows a built-in
welcome page that doubles as a syntax tour. It is the first user of
[`xui-litehtml`](https://github.com/va1erian/xui/tree/main/crates/xui-litehtml)
on LazyOS and the intended renderer for documentation pages. There is no
network: images are not loaded and links are drawn but not followed.

## How it works

```
file (or welcome.md) --pulldown-cmark--> HTML --litehtml (worker thread)--> display list
                                                                              |
                                          xuid window  <-- xui canvas <-- HtmlView (UI thread)
```

* **`xui-app/docs/src/lib.rs`** turns Markdown into one styled HTML page. Raw
  HTML in the source is escaped to text, so a document cannot inject markup into
  the layout engine, and input is truncated at 1 MiB on a character boundary.
  It is pure Rust and unit-tested on the host.
* **`xui-app/docs/src/main.rs`** creates an `HtmlView` filling the window on the
  LazyOS backend. litehtml lays the page out on a worker thread (a
  `std::thread`, so the Linux ABI's `clone`/`futex` matter) and posts a frame back
  to the UI thread, which draws the display list.
* **Text shaping.** `xui-litehtml` measures text off the UI thread, so it needs a
  `Send + Sync` shaper. `LazyOSBackend::text_shaper` hands out the shared
  `xui-canvas` shaper (one font system, built from the fonts the app registered
  before the backend was created). Without that override the backend inherits a
  stub shaper that returns empty layouts and litehtml draws boxes with no text.
* **Fonts.** Droid Sans, Droid Sans Bold (`assets/fonts/`, Apache-2.0) and
  JetBrains Mono. There is no italic face, so italics are synthesised.
* **Scrolling.** The wheel needs the whole input chain; see *Mouse wheel* in
  [`architecture/display.md`](architecture/display.md).

## The C++ toolchain (zig)

litehtml is C++. Every other xui app is pure Rust and links with the toolchain's
bundled lld, but a static `x86_64-unknown-linux-musl` binary containing C++ needs
a musl C++ compiler, a C++ standard library and a linker for that target, which
neither a Windows host nor a stock Linux runner has. `zig c++` is a clang driver
that bundles musl, libc++ and libunwind, so one recipe builds the app on both:

```bash
pip install ziglang==0.16.0        # a wheel that bundles zig, Windows and Linux
python tools/xui/build.py          # builds every app, Docs last, with zig
```

`tools/xui/zig.py` finds zig (`LAZYOS_ZIG`, then `zig` on `PATH`, then
`python -m ziglang`), writes small compiler wrappers (cc-rs and rustc each take
one executable) and returns the cargo environment. Rust still compiles all Rust
code; zig compiles the C/C++ objects and does the final link. What the recipe
needs, each found the hard way:

| Setting | Why |
|---|---|
| `CRATE_CC_NO_DEFAULTS=1` | cc-rs would add a clang `--target=x86_64-unknown-linux-musl`, which zig cannot parse |
| `-C link-self-contained=no` | rustc's own musl start files and zig's both define `_start` |
| `-C link-arg=-pie`, `-fPIC` | LazyOS's loader takes static-PIE linked at address 0, not a fixed-address executable |
| no `-fno-rtti` | litehtml uses `dynamic_cast` |

The Docs app is a package of its own (`xui-app/docs`) because Cargo builds every
dependency of a package for every target in it: keeping litehtml here means the
other apps never need a C++ toolchain. It builds into its own target directory
(`target/xui-zig`), since its `RUSTFLAGS` differ. Without zig, `build.py` skips it
with a warning and `build.rs` embeds every other app (Docs is optional there);
with zig it ships in the desktop image and is opened from the Start menu.

`tools/xui/test_zig.py` unit-tests the helper without needing zig.

## Known limits

* Links and images are not followed or loaded (no network); relative links to
  other documents are the natural next step.
* The LazyOS backend sends no resize events, so the view keeps the window's
  initial size.
* Task-list checkboxes are not drawn (litehtml does not render `<input>`), so
  that Markdown extension is off.
* `mimed` registers Docs for the `view` verb of `text/markdown`; `open` stays
  with the Editor until every image ships Docs.
