# LazyWriter

LazyWriter is a word processor. It edits rich documents in one continuous
column and saves them as `.lzw` files. Open one from Files with a
double-click, or from the start menu under **Office**.

## What it does

* Styled text: bold, italic, underline, strike-through, size, font (Sans,
  Serif or Mono), colour, highlight and links.
* Headings 1 to 3, quotes, left, centre, right and justified alignment,
  indents, and bullet and numbered lists.
* Pictures (PNG or JPEG), inline or floating left or right with the text
  flowing around them.
* Undo and redo, cut, copy and paste.

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
* There is no page view and no printing.
* Copying keeps the formatting only inside LazyWriter. Other apps receive
  plain text, at most 8 KiB of it.
* Documents larger than 32 MiB, picture files larger than 16 MiB and
  pictures of more than 4096 x 4096 pixels are refused.
* There is no italic face: italic text is slanted from the regular one.
* No import of Markdown, HTML, RTF or Word documents.
