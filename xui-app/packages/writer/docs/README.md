# LazyWriter

LazyWriter is a word processor. It shows a document as the pages it would
print on and saves it as a `.lzw` file. Open one from Files with a
double-click, or from the start menu under **Office**.

## What it does

* Styled text: bold, italic, underline, strike-through, size, font (Sans,
  Serif or Mono), colour, highlight and links.
* Headings 1 to 3, quotes, left, centre, right and justified alignment,
  indents, and bullet and numbered lists.
* Pictures (PNG or JPEG), inline or floating left or right with the text
  flowing around them.
* Undo and redo, cut, copy and paste.
* **Page view** (the book button, on by default): white A4 or Letter sheets
  with the text inside their margins, lines and pictures that do not fit
  moving to the next page. Turn it off for a continuous draft column. The
  status bar shows which page the caret is on, out of how many.
* **Page setup** (the ruler button): paper (A4 or Letter), portrait or
  landscape, and Normal, Narrow or Wide margins. It is saved with the
  document and can be undone.
* **Page break** (`Ctrl+Enter`, or the toolbar's Page break): the rest of the
  paragraph starts a new page. Backspace at the start of that page removes
  the break.
* **Tables** (the table button): insert a table of 2 x 2 to 5 x 5 cells,
  then add rows above or below, columns left or right, delete rows, columns
  or the whole table, and turn the header row and the thin grid borders on
  or off. `Tab` moves to the next cell and adds a row after the last one;
  `Shift+Tab` goes back. Drag a column's edge to make it wider or narrower.
  On pages a row is never cut in two (unless it is taller than a page), and
  a header row is repeated at the top of the next page. The status bar shows
  the caret's row and column.

## Shortcuts

| Keys | Action |
|---|---|
| `Ctrl+N` | New document |
| `Ctrl+O` | Open |
| `Ctrl+S` | Save |
| `Ctrl+Shift+S` | Save as |
| `Ctrl+E` | Export to Markdown |
| `Ctrl+Q` | Quit |
| `Ctrl+B`, `Ctrl+I`, `Ctrl+U` | Bold, italic, underline |
| `Ctrl+Z`, `Ctrl+Y` | Undo, redo |
| `Ctrl+X`, `Ctrl+C`, `Ctrl+V` | Cut, copy, paste |
| `Ctrl+A` | Select all |
| `Ctrl+Enter` | Page break |
| `Tab`, `Shift+Tab` | Next or previous table cell |

New, Open, Quit and closing the window ask whether to save a modified
document first.

## Files

* **`.lzw`** is LazyWriter's own format: the whole document, pictures
  included. Saving writes a temporary file and renames it over the old one,
  so a failed save never leaves half a document.
* **Export** writes GitHub Flavored Markdown (`<name>.md`). A document with
  pictures also gets a `<name>_images/` folder beside it, holding them as
  `1.png`, `2.png`, and so on.
* **Plain text** (`.txt`) can be opened from the Open dialog. Save never
  writes over the `.txt`: it asks for a `.lzw` name instead. LazyWriter does not take `.txt` or `.md` files from Files: those
  open in the Editor and Docs.

## Known limits

* A file whose path contains a space cannot be opened from Files or by
  `pkgctl open`; open it from LazyWriter's Open dialog instead.
* There is no printing, and no headers, footers or page numbers on the page.
* Copying keeps the formatting only inside LazyWriter. Other apps receive
  plain text, at most 8 KiB of it.
* Documents larger than 32 MiB, picture files larger than 16 MiB and
  pictures of more than 4096 x 4096 pixels are refused.
* There is no italic face: italic text is slanted from the regular one.
* No import of Markdown, HTML, RTF or Word documents.
* Table cells cannot be merged or shaded, and tables cannot be nested.
  Pictures in a cell sit in the text, not floating.
