# Docs

This is the LazyOS documentation viewer. It renders **Markdown** with
[litehtml](https://github.com/litehtml/litehtml), so a page is laid out like a
web page: wrapped text, tables, lists and code, in a window you can scroll with
the mouse wheel, `PageUp`/`PageDown` or the arrow keys.

## Opening a document

Press `Ctrl+O` or click **Open...** in the toolbar to pick a Markdown file. The
image ships one to try, `/TESTDOC.MD`. You can also start the app with a path,
for example `XDOCS.ELF /TESTDOC.MD`. With no path it shows this page, which
doubles as a tour of what it can draw.

## Text

Plain paragraphs wrap to the window width. You can write **bold**, *italic*,
~~strikethrough~~ and `inline code`, and mix them: ***bold italic***, or a
**bold phrase with `code` inside**. A line that ends in two spaces  
continues on the next line without starting a new paragraph.

### Headings

Headings go from level 1 (the page title above) to level 4, each with its own
size and spacing. Levels 1 and 2 are underlined with a rule.

#### Level four

Body text follows a heading with a comfortable line height.

## Lists

- A bullet list
- with several items,
  - and a nested list
  - two levels deep
- and a last item.

1. A numbered list
2. counts upward,
3. for as many items as you like.

## Tables

| Markdown | Result | Notes |
|---|---|---|
| `# Heading` | a heading | levels 1 to 4 |
| `**bold**` | **bold** | strong emphasis |
| `*italic*` | *italic* | a synthesised slant |
| `` `code` `` | `code` | monospace face |
| `- item` | a bullet | nests by indentation |
| `> quote` | a block quote | see below |

## Code

Fenced blocks keep their spacing and are set in a monospace face:

```
fn main() {
    let notes = ["wrap", "scroll", "select"];
    for note in notes {
        println!("Docs can {note}");
    }
}
```

## Quotes and rules

> A block quote sits behind a bar and a muted colour. Quotes can hold **bold**
> text, `code` and more than one paragraph.
>
> Like this second one.

Three dashes make a horizontal rule:

---

## Moving around

| Input | Action |
|---|---|
| `Ctrl+O` | open a document |
| Mouse wheel | scroll a few lines |
| `PageUp` / `PageDown` | scroll a page |
| `Up` / `Down` | scroll a line |
| `Home` / `End` | jump to the top or bottom |
| `Ctrl+A`, `Ctrl+C` | select everything, copy |

## Limits

Raw HTML inside a document is shown as text, never interpreted. Documents over
1 MiB are cut off. Links are drawn but not followed yet, and there is no network:
images are not loaded.
