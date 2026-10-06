# PDF Viewer: reading PDF documents on the desktop

**Status:** P0–P2 done: hayro chosen (findings below), `lazypdf` library,
and the viewer MVP shipped as a core package in every desktop image. P3–P7
are still plans. User-facing guide:
[`xui-app/packages/pdf/docs/README.md`](../xui-app/packages/pdf/docs/README.md).

LazyOS cannot show a `.pdf`. Users have them: downloaded with LazyWeb
(`~/Downloads`), attached to mail in `xui-mail`, unpacked from an archive by
Archiver, or copied onto the `/home` volume. The PDF Viewer (`os.lazy.pdf`,
cargo bin `xui-pdf`) is a core desktop app that opens them, lets you read,
search and copy from them and print them, and treats every file as hostile.

It follows the shape of the Archiver ([`archiver-plan.md`](archiver-plan.md)):
a pure-Rust library that is host-tested and fuzzed, an app crate tested
offscreen, a thin LazyOS binary, and a core package.

## Goals

* **Open** a PDF from the command line, `mimed` (`application/pdf`, verb
  `open`), the Open dialog (`Ctrl+O`, the portable `FileDialog`), or by dropping
  it on the window (`text/uri-list`, the Archiver's drag-and-drop path).
* **Read**: pages laid out top to bottom with continuous scrolling (wheel,
  scrollbar, `PageUp`/`PageDown`, `Home`/`End`, `Space`). Zoom by step
  (`Ctrl+=`/`Ctrl+-`, `Ctrl`+wheel), fit width, fit page and actual size.
  Rotate the view by 90°. A page box shows "*n* / *N*"; typing in it goes to
  that page. HiDPI: pages are rendered at physical pixels (`96 * scale` DPI
  base), so a 2x desktop is sharp, not upscaled.
* **Navigate**: a sidebar with the document outline (bookmarks) and page
  thumbnails. Internal links (`GoTo`, named destinations) jump; external `URI`
  links are handed to `mimed` (so `http(s)://` opens LazyWeb, `mailto:` Mail)
  after a confirmation that shows the full URL. Back/Forward (`Alt+Left`/
  `Alt+Right`) over the jump history.
* **Find** (`Ctrl+F`, `F3`/`Shift+F3`): search the text layer, highlight the
  matches on the page, count them, and scroll to each.
* **Select and copy**: drag to select text, `Ctrl+C` to copy through
  `clipboardd` as `text/plain;charset=utf-8`; `Ctrl+A` selects the page.
* **Print** (`Ctrl+P`): through `printd` like LazyWriter (the same printer bar
  and `confd` key layout), each page rendered at the printer's resolution and
  streamed as PWG Raster with `libs/raster`'s `PageEncoder`, one band at a time.
* **Document properties**: title, author, producer, page size, PDF version,
  encryption, page count.
* **Password-protected PDFs** (the standard security handler, RC4 and AES):
  ask for the password in a dialog. The password stays in memory, as in Mail.
* **Remember where you were**: the last page and zoom of the most recent
  documents in `confd` (`user/<uid>/pdf/recent`), restored when the file is
  opened again. A "Recent" menu lists them.
* **Presentation mode** (`F5`): full screen, one page at a time, arrow keys
  and click to advance.
* **Night mode**: invert the page's lightness, for dark themes.
* **Security first** (below): no JavaScript, no forms submission, no launch
  actions, no network, no file written except where the user asks.

## Non-goals (for now)

* Editing: annotations, form filling, signing, page reordering. Annotations
  that a document *already has* are drawn (their appearance streams); none are
  created.
* JavaScript and XFA forms. Neither is run, ever: a PDF reader that runs
  scripts is a browser with fewer guarantees.
* Digital signature validation (shown as "signed, not verified" when present).
* Embedded files and `Launch` actions: listed in Properties, never opened or
  executed.
* Rendering 3D, multimedia and rich-media annotations.
* Writing PDF. LazyWriter's "Export as PDF" is a separate plan; it would reuse
  this one's library only for its tests (round trips).

## The renderer: choosing one

A PDF renderer is a parser, a content-stream interpreter, a font engine (Type 1,
CFF, TrueType, Type 3), image codecs (Flate, LZW, DCT/JPEG, JPX/JPEG 2000,
JBIG2, CCITT G3/G4) and a 2D rasterizer with blend modes, soft masks and
shading patterns. Writing one is not the goal; picking one is.

| Option | Language / licence | For | Against |
|---|---|---|---|
| **hayro** (LaurenzV, the typst ecosystem) | Rust; MIT / Apache-2.0 | Pure Rust: no zig, builds for musl like every other xui app; its font stack is `skrifa`/`read-fonts`, already in our lock; written for untrusted input; actively developed, with its own JPX/JBIG2/CCITT decoders | Young: fidelity and speed on hard documents to be measured; text-extraction API to be checked |
| MuPDF via zig | C; **AGPL-3.0** | The most complete and fastest; text extraction, search, outlines all built in | AGPL for the app; a large C attack surface parsing hostile input, outside Rust's guarantees; vendored third-party C (freetype, openjpeg, jbig2dec, harfbuzz); zig in the build for a core app |
| pdfium | C++; BSD/Apache-2.0 | Chrome's renderer, complete | Chromium's build system; a large C++ port, for less than MuPDF |
| poppler | C++; GPL | Complete | Needs freetype, cairo or splash, and much of a Unix userland |
| pdf.js | JavaScript | Firefox's renderer | LazyOS has no JavaScript engine |
| `lopdf`/`pdf` crates + our own interpreter | Rust | Full control | A renderer is years of work; these are parsers, not renderers |

**Decision: hayro**, confirmed by the P0 spike (below). It keeps the
viewer pure Rust (ships in every desktop image, like the Archiver, without
zig), shares font crates we already link, and keeps hostile-input parsing in
memory-safe code. **Fallback: MuPDF** built with zig like the Docs app, behind
a `LAZYOS_PDF_MUPDF=1` switch, only if P0 finds hayro unusable for common
documents. The library crate (below) hides the renderer behind its own API so
the switch would not touch the app.

The project is GPL-3.0-or-later, so both licences are compatible, but the
AGPL's network clause would apply to the MuPDF build; another reason to prefer
hayro.

## Architecture

| Piece | Where | What |
|---|---|---|
| Library (`lazypdf`) | `xui-app/crates/pdf` | Pure Rust, no UI. `Document::open(bytes, password)`, page count, page boxes and rotation, `render(page, scale, rect) -> RgbaTile`, outline, links and destinations, text layer (glyph runs with Unicode and boxes), search, metadata, encryption state. Budgets and cancellation (below). Host-tested and fuzzed. |
| App core (`xui-pdfview`) | `xui-app/crates/pdfview` | The xui `App`: the page layout model (page rectangles at a zoom, visible range), the tile cache, the render job queue, find, selection, sidebar, dialogs, print job. Tested offscreen with `xui-app/crates/testkit`. |
| Binary | `xui-app/src/bin/pdf.rs` | LazyOS side: backend, theme, argv path, `mimed` launches, drag and drop, `confd` recents, `printd`, serial evidence `PDF:*`. |
| Package | `xui-app/packages/pdf` | `os.lazy.pdf`, category `office`, `[[mime]] application/pdf`; permissions derived from a `LAZYOS_LABEL_TRACE=1` run (display, input, clipboard, mimed, print, confd). No network. |
| MIME | `user/src/bin/mimed/db.rs` | `("pdf", "application/pdf")`; `selftest.rs` covers it. |
| Sample | `assets/samples/lazyos-sample.pdf` | An original document (outline, links, two fonts, an image, a rotated page, an encrypted twin), generated by a script in the repo so it is reproducible; one `assets/manifest.txt` line; an `fhs::share` constant. |

### Rendering pipeline

```
file bytes --lazypdf::Document (worker)--> RgbaTile(page, zoom, x, y)
                                                 |
           xuid window <-- xui canvas <-- PageView (UI thread, tile cache)
```

* **Tiles.** A page at a zoom is cut into 256×256 physical-pixel tiles, so a
  zoomed-in A0 poster never needs one huge bitmap and only what is visible is
  rendered. The cache is an LRU keyed by `(page, zoom, tile)` with a byte
  budget derived from the machine's memory (a fraction of free RAM at start,
  never a fixed small constant; see the limits note below).
* **Progressive display.** A page with no tiles at the current zoom is first
  drawn from the nearest cached zoom (scaled), then from its thumbnail, then a
  blank page of the right size; tiles replace it as they arrive. Scrolling
  never waits on the renderer.
* **Worker threads.** Rendering runs on a pool of `std::thread`s (one per CPU
  minus one, at least one), as the Docs app does with litehtml. The queue is
  ordered by distance from the viewport; a scroll or zoom bumps a generation
  counter and stale jobs are dropped before they start and cancelled while they
  run (the interpreter checks a cancel flag between operators). The window
  polls finished tiles on a timer as the Archiver does, since the LazyOS
  backend has no cross-thread waker; a waker would be a small backend
  improvement worth making here.
* **Text layer.** Extracted once per page on demand (find, selection) and
  cached; a page's runs carry Unicode (from `ToUnicode` maps or the font's
  encoding), a bounding box per glyph and reading order as the content stream
  gave it, with a simple line/column grouping pass for selection.

### Security

A PDF is untrusted input, and PDF is a rich format for attacks.

* **Memory safety.** Parsing and interpretation are Rust, `#![forbid(unsafe_code)]`
  in `lazypdf` (hayro's own `unsafe`, if any, audited in P0).
* **Budgets, not caps.** No limit on file size or page count (a 2,000-page
  manual must open). What is bounded is *work an attacker can multiply*:
  decompressed bytes per stream relative to the machine's memory (deflate
  bombs), nesting depth of form XObjects, patterns and Type 3 glyphs,
  reference-chain length, operators per page render (with cancellation, so a
  pathological page shows "this page took too long" instead of hanging), image
  dimensions against the memory budget. A budget hit fails that page, never
  the app, and prints `PDF:PAGE:FAIL:<n>:<reason>`.
* **No active content.** JavaScript, `Launch`, `SubmitForm`, `ImportData`,
  `GoToR` to other files and embedded files are ignored. `URI` actions only go
  to `mimed` after the user confirms the URL, and only for `http`, `https` and
  `mailto`.
* **Least privilege.** The package's label grants display, input, clipboard,
  `mimed`, print and `confd`; no network, no write anywhere but the user's own
  files through the Save dialog (Save a copy).
* **Later: a renderer sandbox.** Running `lazypdf` in a helper process with no
  permissions at all, passing pages back through a shared buffer
  (`shared_buffer_max`), is a P7 hardening step once the protocol is clear; it
  would get its own `.midl` interface under `idl/`.
* **Fuzzing.** A seeded fuzz entry point (`lazypdf::fuzz::run(&[u8])`, as in
  `lazyarc`) over open, every page's render at a small scale, text extraction
  and outline, with a corpus of real and mutated files; `FUZZ_CASES=… cargo
  test -p lazypdf --release seeded` runs it, CI runs a short soak.

## P0 findings (2026-10-06)

hayro 0.8.0 was measured against MuPDF 1.29 (PyMuPDF) on a corpus of real
and generated files: the LazyOS sample (all six pages) and its AES-256 and
RC4 twins, pdf.js's `tracemonkey`, `alphatrans`, `TAMReview` (Type 1 fonts),
`sizes` and `rotation`, the arXiv "Attention Is All You Need" paper (CFF
fonts, vector figures) and a generated 400-page document.

| Check | Result |
|---|---|
| Fidelity at 96 dpi vs MuPDF | Every page matches: mean absolute difference 0.7–5.9 levels, at most 0.48% of pixels differing by more than 96 levels (anti-aliasing at glyph edges). Transparency, dashes, JPEG, JPEG 2000, `/Rotate 90` and non-embedded standard 14 fonts render like MuPDF's. |
| Speed (host, release, one thread, 96 dpi) | 2–20 ms per typical page; the arXiv paper 40 ms on average, 220 ms worst (its big figure); the 400-page file 11 ms per page, 1.1 ms to open. |
| Speed (guest: QEMU with its accelerator auto-detected, 1 GiB) | The sample's first page appears 30 ms after opening (14 ms of render-thread time); a zoomed picture page draws in 90–120 ms. |
| Encryption | RC4 and AES-256 open with the user password; a wrong or missing one is reported as *password required* (hayro's own docs still say encryption is unsupported; they are out of date). |
| Standard 14 fonts | `embed-fonts` (default) bundles hayro's substitutes: Helvetica, Times and Courier render metric-compatible, so the "Liberation" risk below is closed. |
| Text | A no-op `Device` that records each glyph's `as_unicode()` and origin gives correct text, accents and typographic quotes included; word gaps come from glyph advances (TeX output has no space glyphs). `Renderer::page_text` is the start of P4. |
| Threads | `Document` is `Send + Sync`; each thread keeps its own `Renderer` (font caches are `Rc`). |
| Robustness | 2,000 seeded mutations of the samples (uncompressed copy included) opened, drawn and read: no panic. |
| `unsafe` | hayro and hayro-interpret forbid it; hayro-interpret turns on hayro-syntax's `unsafe` feature (`memchr`, `flate2`, SIMD in `zune-jpeg`, JBIG2 and JPEG 2000). |
| Licences | Every crate is MIT, Apache-2.0, BSD or Unicode: GPL-3.0-compatible. |
| musl | Builds for `x86_64-unknown-linux-musl` with `tools/xui/build.py`'s rust-lld settings; the app ELF is 7.8 MB. |

What hayro does not give, and the viewer works around: no cancellation
inside a page render (stale jobs are dropped before they start instead) and
no work budget (a pathological page can take as long as it takes; the P7
sandbox is the answer). The guest builds with `panic = "abort"`, so a panic
in hayro would end the app; the fuzz test is the guard until then.

## What P1–P2 built

| Piece | Where |
|---|---|
| `lazypdf` | `xui-app/crates/pdf`: `Document` (open with a password, page sizes, metadata, version), `Renderer` (any rectangle of a page at any scale to RGBA; the text layer), the seeded fuzz entry point. Tests: `tests/document.rs`. |
| Samples | `tools/pdf/make_sample.py` writes `assets/samples/lazyos-sample.pdf` (`fhs::share::PDF_SAMPLE`) and `xui-app/crates/pdf/testdata/` (the AES-256, RC4 and uncompressed twins); `--check` keeps them current. |
| App core | `xui-app/crates/pdfview` (`xui-pdfview`): `layout` (pages in one column, visible pages, tiles, fit modes, zoom steps), `cache` (LRU tile cache over a byte budget of six screens, at least 32 MiB), `worker` (the render pool: a job list replaced on every scroll or zoom, threads drawing from its front), `viewer` (scroll, zoom around an anchor, scheduling: previews first, then visible tiles nearest the centre, then half a screen ahead), `view` (painter, wheel, scroll bars, the drain timer), `app`/`ui` (toolbar, status bar, Open and password dialogs, shortcuts). Offscreen tests: `tests/window.rs`. |
| Binary | `xui-app/src/bin/pdf.rs`: backend, theme, argv file, files dropped as `text/uri-list`. |
| Package | `xui-app/packages/pdf` (`os.lazy.pdf`, Office), `application/pdf` in `mimed` (`db.rs`, `apps.rs`, a self-test line). Permissions: display, input and clipboard (drops); a `LAZYOS_LABEL_TRACE=1` run of `xui_pdf.json` printed no `LABEL:DENY`. |
| Session | `tools/screenshot/examples/xui_pdf.json` (`LAZYOS_XUI_AUTOSTART=pdf`): Open, next page, fit page, last page, back to the pictures, zoom in three steps. |

Not in P2 though the goals list them: the page box you can type into (the
status bar shows "Page *n* of *N*"; `N`/`P` and the toolbar move by page),
and a masked password field (xui's `Dialog::prompt` has none, so the
password shows as it is typed).

## Why Rust and not a LazyRAD form

AGENTS.md asks for Rhai first for small apps. A PDF viewer is the "heavy data
or threads" case: a tile renderer on worker threads, a custom painter and a
large parser. It starts from `python tools/xui/new_app.py pdf --name "PDF
Viewer" --description "Read, search and print PDF documents"`.

## Phases

**P0. Spike: does hayro do the job?** A host-only throwaway
(`xui-app/crates/pdf/examples/render.rs`) renders a corpus to PNG and times it:
the LazyOS sample, the PDF 2.0 and ISO 32000 examples, a scanned document
(JBIG2/CCITT), a JPX-heavy file, a large technical manual, arXiv papers (Type 1
and CFF fonts, many pages), a form with appearance streams, a slide deck with
transparency and shading, a file with non-embedded standard 14 fonts, an
encrypted file. Compare against `mutool draw`/`pdftoppm` output (a pixel diff
score). Check: the text-extraction surface (glyph positions and Unicode), the
standard-14 fallback fonts (bundled substitutes, or map them to Droid
Sans/Serif and JetBrains Mono), cancellation hooks, `unsafe` use, the
dependency tree's licences, and that it builds for `x86_64-unknown-linux-musl`
with `tools/xui/build.py`'s settings. **Exit:** a short report in
`docs/pdf-reader-plan.md` with the decision (hayro, or the MuPDF fallback) and
the per-page timings on the guest.

**P1. `lazypdf` library.** Open, page count and boxes, `render` to an RGBA tile,
metadata, encryption with password, budgets and cancellation, the fuzz entry
point. Host tests render the sample and compare to checked-in golden PNGs with
a tolerance; seeded fuzz.

**P2. Viewer MVP.** `new_app.py` scaffolding (a core package in every desktop image,
`run_demo.py`/GUI wiring through `tools/xui/core_packages.py` and
`tools/lazygui/catalog.py`, icons), `mimed` mapping, open from argv and
`Ctrl+O`, continuous scroll, zoom and fit modes, page box, tile cache, worker
pool, progressive display, error page for files that do not open. Evidence:
`PDF:UP:PASS`, `PDF:OPEN:PASS:<path>:<pages>`, `PDF:PAGE:DRAWN:<n>`,
`PDF:OPEN:FAIL:<path>:<reason>`. Session
`tools/screenshot/examples/xui_pdf.json` (`LAZYOS_XUI_AUTOSTART=pdf`): open the
sample, scroll, zoom, check pixels with `pngstats.py`. Permissions from a
traced run of `core_apps.json`.

**P3. Navigation.** Outline sidebar, thumbnails (rendered at low priority),
internal links, external links through `mimed` with confirmation, Back/Forward,
rotate. The LazyWeb session gains a step that downloads a PDF and opens it from
`about:downloads`.

**P4. Text.** Text layer, find with highlights, selection, copy to
`clipboardd`. Host tests: extraction from the sample matches its known text;
search hits land on the right glyph boxes. Session step: find a word, copy it,
paste it into the Editor.

**P5. Print.** Reuse LazyWriter's printer bar and `printd` path; pages rendered
banded at the printer's DPI into `PageEncoder`. `tools/print/run.py --app pdf`
judges the PWG Raster pages the fake printer receives against a host render.

**P6. Comfort.** Recents and last position in `confd`, Properties dialog,
presentation mode, night mode, drag and drop onto the window, Archiver's
"open inside" for PDFs in archives (falls out of `mimed`).

**P7. Hardening.** The renderer sandbox process (with its `.midl`), a longer
fuzz soak in CI, a performance pass measured with the latency harness (time to
first page, scroll smoothness on a 500-page file), and a user's guide in
`xui-app/packages/pdf/docs/README.md`.

## Testing summary

```bash
cargo test --manifest-path xui-app/Cargo.toml -p lazypdf -p xui-pdfview
FUZZ_CASES=20000 cargo test --manifest-path xui-app/Cargo.toml -p lazypdf --release seeded
python tools/pdf/make_sample.py --check
LAZYOS_DESKTOP=1 LAZYOS_XUI_AUTOSTART=pdf LAZYOS_RESET_OS=1 cargo build
python tools/screenshot/qemu_session.py --image target/lazyos.img     --out shots/pdf --script tools/screenshot/examples/xui_pdf.json
python tools/screenshot/pngstats.py shots/pdf/*.png --min-nonblack 0.01 --min-colors 16
```

Neither crate links LazyOS code, so both test suites run on any host
(Windows included). `make_sample.py` needs PyMuPDF and Pillow. P5 adds
`tools/print/run.py --app pdf`.

## Risks and open questions

* **Fidelity and speed of hayro** on the guest under TCG and WHPX/KVM: P0
  answers it. Interpretation is single-threaded per page; the pool parallelises
  across tiles and pages, not within one.
* **Text extraction** may need work in or around hayro (a device that records
  glyphs). If it is missing upstream, contribute it rather than fork.
* **Standard 14 fonts.** Closed by P0: hayro's `embed-fonts` substitutes are
  metric-compatible.
* **The password shows as it is typed** until xui's prompt dialog gains a
  masked mode.
* **CJK and right-to-left text** in PDFs is drawn from embedded glyphs, so it
  renders; selecting and searching it depends on `ToUnicode` maps and is best
  effort.
* **Memory.** At 1 GiB of guest RAM, a 600-DPI scanned page is ~140 MB as RGBA;
  tiles and the budgeted image decoder keep this bounded, and decoding images
  at the scale they are drawn (not full resolution) matters for scans.
* **No cross-thread waker** in the LazyOS backend: polling costs a little
  latency and idle CPU; adding a waker benefits every threaded xui app.
