# xui-code-editor

A reusable code-editor widget for [xui](https://github.com/va1erian/xui).

It is one `xui-core` `Custom` node with a painter and an event mapper, built
over pure modules: a `ropey`-backed text buffer with transactional undo, a
monospace-grid view, caret and selection, scrolling with scrollbars,
find/replace (plain or regex), diagnostics markers and incremental syntax
highlighting. It stays pure Rust, so it needs no C++ toolchain, and it copies
and pastes through the backend's portable clipboard.

Highlighting is pluggable. The editor holds a `Highlighter` behind a box, so it
is not generic over the language. `PlainText` is the default and emits no
tokens; the hand-written, line-incremental Rhai lexer ships as `RhaiHighlighter`
behind the `rhai-syntax` feature. Bracket matching works with any highlighter.

## Embedding

```rust,no_run
use xui_core::app::Ui;
use xui_core::geometry::Rect;
use xui_code_editor::Editor;

fn build<M: 'static>(ui: &Ui<M>) -> xui_core::backend::Result<()> {
    let editor = Editor::new(ui, Rect::new(0, 0, 640, 400))?
        .on_change(|text| {
            // Return `Some(message)` to raise an app message, or `None`.
            let _ = text;
            None
        });
    editor.set_text("fn main() {\n}\n");
    editor.goto(1, 0);
    Ok(())
}
```

`Editor::new` gives a plain-text editor. To colour Rhai, enable the feature and
select the highlighter:

```rust,no_run
use xui_core::app::Ui;
use xui_core::geometry::Rect;
use xui_code_editor::{Editor, RhaiHighlighter};

fn build<M: 'static>(ui: &Ui<M>) -> xui_core::backend::Result<Editor<M>> {
    Ok(Editor::new(ui, Rect::new(0, 0, 640, 400))?.with_highlighter(RhaiHighlighter))
}
```

## The LazyOS Editor app

This copy is the library only. The upstream Notepad example became the
`xui-editor` binary in `xui-app/src/bin/editor.rs` (with
`xui-app/src/bin/editor/`), which the desktop image ships as `/system/bin/editor`. It
runs on LazyOS's own backend (`xui-app/src/backend*`) over the `xuid`
compositor, opens an optional absolute path passed as the first argument (as
Files' open-with does), and titles its window after the document. Build it with
`python tools/xui/build.py`; see `docs/xui-apps-track-a.md` for how it is
launched. The file dialogs are xui's portable `FileDialog`, listing through
LazyOS's filesystem.

The app is a menu bar, an editor filling the middle, a find/replace bar and a
status bar. Features:

* File: New, Open..., Save, Save As..., Quit. Open and Save As use the portable
  `xui_core::FileDialog`, which lists directories and confirms an overwrite; a
  path can also come from the command-line argument.
* Edit: Undo, Redo, Cut, Copy, Paste, Select All, Find..., Replace...
* Find/replace bar with Next, Previous, Replace, Replace all, a Regex and a
  Match case check box, and a "3 of 17" or error label. Replace all is a single
  undo step.
* Status bar: `Ln X, Col Y`, selection length, `LF`/`CRLF` and dirty state.
* Unsaved changes are confirmed before New, Open, Quit and the window's close
  button; a failed open or save shows the error and changes nothing. Files are
  written atomically, line endings and a UTF-8 BOM round-trip, and an invalid
  UTF-8 or oversized file is refused.

Shortcuts (the command modifier is Ctrl on Windows/Linux and Cmd on macOS):

| Shortcut | Action |
|---|---|
| Ctrl/Cmd+N | New |
| Ctrl/Cmd+O | Open... |
| Ctrl/Cmd+S | Save |
| Ctrl/Cmd+Shift+S | Save As... |
| Ctrl/Cmd+F | Find... |
| Ctrl/Cmd+H | Replace... |
| F3 / Shift+F3 | Next / previous match |
| Escape | Close the find bar |
| Editor defaults | Ctrl/Cmd+C, X, V, Z, Y, A and navigation |

The pure file and find models live in `document` and `search`, so they are
tested without a window; `tests/notepad_ui.rs` renders the editor and find bar
headlessly in light and dark into `target/snapshots/`.

## Features

* `rhai-syntax`: the `RhaiHighlighter`.

Copy, cut and paste go through the window's portable clipboard
(`Backend::clipboard_text` / `set_clipboard_text`, reached through `Ui`): the
real OS clipboard on the Win32 and canvas backends, an in-process store on a
backend without one. To use your own clipboard, implement the two-method
`Clipboard` trait (`text`, `set_text`) and pass it to `Editor::with_clipboard`:

```rust,no_run
use xui_code_editor::{Clipboard, Editor};

struct MyOsClipboard;

impl Clipboard for MyOsClipboard {
    fn text(&self) -> Option<String> {
        None // read your OS clipboard here
    }
    fn set_text(&self, _text: &str) {
        // write your OS clipboard here
    }
}

fn install<M: 'static>(editor: Editor<M>) -> Editor<M> {
    editor.with_clipboard(MyOsClipboard)
}
```

## Writing a highlighter

Implement `Highlighter`. It is line-incremental: given the `LineState` carried
from the previous line and the line's text, return this line's `Token`s and the
state to carry on. Pack your state into the opaque `LineState` (a raw `u64`);
the cache compares states to decide when a re-lex has settled into state the
rest of the file already had. Then select it with `Editor::with_highlighter` (or
`Editor::set_highlighter` on a live editor, which re-lexes the whole buffer).

`PlainText` is the whole interface in six lines of logic and a good template.

## Porting checklist

An app that embeds the editor needs to provide:

* An xui backend. `xui-canvas`'s `WinitBackend` on a desktop, or another
  `Backend` implementation on a smaller target.
* One monospace font, through `FontConfig`. The grid measures one advance and
  one line height per font and DPI; a proportional font will not line up.
* Timers for the caret blink. The editor starts its own per-widget timer
  (`Control::set_timer`) and stops it on drop; the host forwards nothing.
* Keyboard events with modifiers. The editor handles `KeyDown` for navigation,
  selection, editing and the usual shortcuts, and needs the backend to report
  Ctrl and Shift.
* A clipboard: the backend's `clipboard_text` / `set_clipboard_text` (the
  default), or your own `Clipboard` implementation passed to
  `Editor::with_clipboard`.

## Limitations

* A monospace grid only; proportional fonts do not line up.
* No word wrap.
* No IME or composition events, so it cannot enter CJK text.
* Columns are char counts, so wide CJK characters and emoji do not line up with
  the grid; use a monospace font with predictable advances.
