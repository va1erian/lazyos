# PDF Viewer

Read PDF documents: pages in one scrolling column, drawn as you reach them.

## Opening a document

* Double-click a `.pdf` in Files, or open one from another app: `mimed`
  hands `application/pdf` to the viewer.
* **Open** on the toolbar, or `Ctrl+O`.
* Drop a PDF on the window.

A password-protected document asks for its password. A file that is not a
PDF, or is damaged beyond repair, says so in the window instead of pages.

## Moving around

| Keys | Does |
|---|---|
| Mouse wheel, arrow keys | Scroll |
| `Shift`+wheel | Scroll sideways |
| `Page Up` / `Page Down`, `Space` / `Shift+Space` | Scroll by a screen |
| `N` / `P`, `Ctrl+Page Down` / `Ctrl+Page Up`, the toolbar's arrows | Next / previous page |
| `Home` / `End` | First / last page |
| Drag a scroll bar, or click its track | Scroll, or page towards the click |

## Zoom

| Keys | Does |
|---|---|
| `Ctrl+=` / `Ctrl+-`, `Ctrl`+wheel, the toolbar's `+` / `-` | Zoom in / out by a step (25% to 800%) |
| `Ctrl+1`, **Width** | Fit the current page's width (the default) |
| `Ctrl+2`, **Page** | Fit the whole current page |
| `Ctrl+0` | Printed size (100%) |

The status bar shows the file, the page and the zoom.

## Safety

The viewer reads PDFs and nothing else: it runs no JavaScript, submits no
forms, opens no attachments or links, and reaches no network. It may use
the display, input and the clipboard (for files dropped on it).

Design and plan: `docs/pdf-reader-plan.md`.
