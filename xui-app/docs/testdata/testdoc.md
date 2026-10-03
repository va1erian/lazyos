# Docs test document

This file ships in every desktop image as `/system/share/samples/testdoc.md`. The Docs screenshot
session opens it through the **Open** dialog, and the unit tests render it, so
it exercises everything the viewer draws and is long enough to scroll.

## Text

Plain paragraphs wrap to the window width. Words can be **bold**, *italic*,
~~struck through~~ or `inline code`, and they combine: ***bold italic***, or a
**bold phrase with `code` inside**. Accented Latin text renders too: café,
naïve, Zoë, señor, Ångström, œuvre.

A hard line break ends this line  
and continues here, in the same paragraph.

### Third-level heading

Headings keep their own size and spacing. Levels one and two carry a rule.

#### Fourth-level heading

Body text follows with the normal line height.

## Lists

- First bullet
- Second bullet
  - a nested bullet
  - another one, two levels deep
- Third bullet

1. First step
2. Second step
3. Third step

## Table

| Key | Action | Where |
|---|---|---|
| Mouse wheel | scroll a few lines | over the page |
| `PageDown` | scroll a page | after a click on the page |
| `Ctrl+O` | open a document | anywhere |
| `Ctrl+C` | copy the selection | after selecting text |

## Code

```
fn main() {
    // Fenced code keeps its spacing and uses a monospace face.
    for step in 1..=3 {
        println!("step {step}");
    }
}
```

## Quote and rule

> A block quote sits behind a bar. It can hold **bold** text, `code`, and a
> second paragraph.
>
> Like this one.

---

## Raw HTML stays text

<script>alert("this is shown, never run")</script>

The line above is a literal string: Docs never hands markup from a document to
the layout engine.

## A link

[The litehtml project](https://github.com/litehtml/litehtml) is drawn as a link;
there is no network, so it is not followed.

## Scrolling target

Everything from here to the end exists so the page is taller than the window.

Lorem ipsum dolor sit amet, consectetur adipiscing elit, sed do eiusmod tempor
incididunt ut labore et dolore magna aliqua. Ut enim ad minim veniam, quis
nostrud exercitation ullamco laboris nisi ut aliquip ex ea commodo consequat.

Duis aute irure dolor in reprehenderit in voluptate velit esse cillum dolore eu
fugiat nulla pariatur. Excepteur sint occaecat cupidatat non proident, sunt in
culpa qui officia deserunt mollit anim id est laborum.

Sed ut perspiciatis unde omnis iste natus error sit voluptatem accusantium
doloremque laudantium, totam rem aperiam, eaque ipsa quae ab illo inventore
veritatis et quasi architecto beatae vitae dicta sunt explicabo.

Nemo enim ipsam voluptatem quia voluptas sit aspernatur aut odit aut fugit, sed
quia consequuntur magni dolores eos qui ratione voluptatem sequi nesciunt.

### The last section

If you can read this heading, you reached the end of the test document.
