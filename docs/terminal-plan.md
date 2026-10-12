# Terminal: a customizable, convenient GUI terminal

Status: implemented (T0-T6). Scope: the desktop Terminal (`xui-term`, `xui-app/src/bin/term.rs`
and `term/`), a new portable terminal-engine crate, its `confd` settings, and
the small platform pieces it leans on (`clipboardd`, fonts, terminfo).

## 1. Goals

1. **Font size and family**: chosen per user, applied live, with zoom keys.
2. **Scrollback**: a large, configurable history, scrolled by wheel and keys.
3. **Select, copy, paste**: mouse selection (character, word, line, block),
   the system clipboard through `clipboardd`, safe pasting.
4. **What a GUI terminal is expected to do**: resize with the window (and tell
   the program), colours (16, 256, true colour), a real `TERM`, a window
   title, bell, links, find, a context menu, new windows, and a preferences
   dialog.

Non-goals for this plan: tabs and split panes (section 9),
ligatures, a GPU renderer, sixel/kitty images, serial-line use.

## 2. Where it stands today

`xui-term` is ~430 lines plus `term/` (grid 350, view 183, pty 96, perf 92):

- **Process side is solid.** A real pty (`term/pty.rs`), `busybox sh -i` as
  session leader, the kernel's line discipline, `TIOCSWINSZ` set once at
  spawn. The kernel already sends `SIGWINCH` to the foreground group on a size
  change (`kernel/src/process/linux/tty.rs`), so resizing is a userspace job.
- **The grid is minimal.** A fixed `80 x 24` `Vec<Vec<char>>`: no attributes
  (no colour, bold, inverse), no wide characters, no scroll regions, no
  insert/delete line, no wrap flag. Rows that scroll off are dropped
  (`line_feed` does `cells.remove(0)`). `TERM=vt100`.
- **Rendering**: one `draw_text` per row in JetBrains Mono at a hard-coded
  15 px, a cell width guessed as `0.6 em`, a 20 px line, a 3 px underline
  cursor; the palette is two constants (dark, light). Damage tracking
  (`term/view.rs`) repaints only changed rows.
- **Input**: characters, arrows/Home/End/PgUp/PgDn/Ins/Del, Ctrl+letter. No
  mouse handling at all, no Alt, no function keys, no modified arrows.
- **The window is not resizable** (no `set_size_hints`), and the pty is always
  80 columns wide because a narrower tty wrapped long commands and broke the
  one-`TERM:OUT`-per-command evidence (the comment in `main`).
- **Platform pieces that exist**: `clipboardd` and `Ui::clipboard_text` /
  `set_clipboard_text` (8 KiB cap, `xui-app/src/platform/clipboard.rs`);
  per-user settings under `user/<uid>/**` in `confd` with a live
  `user/<uid>/confd/changed/<path>` topic; xui mouse events (down, up, move,
  double-click, wheel), `Cursor::Text`, popup nodes; `text/uri-list` drag and
  drop (the Archiver work); resizable windows (`docs/window-resize-plan.md`);
  `mimed` for opening URLs; `os.lazy.notify` toasts.

## 3. Architecture

### 3.1 A portable engine crate: `lazyterm`

Move everything that is not LazyOS-specific into
`xui-app/crates/term` (crate `lazyterm`), the way `lazypdf`, `lazyvideo` and
`lazysky-core` are split: host-tested, fuzzed, no xui window type, no LazyOS
crate. The bin keeps the pty, the backend, confd and the evidence markers.

```
lazyterm/
  parser.rs     escape-sequence state machine (the `vte` crate, MIT/Apache-2.0)
  cell.rs       Cell { ch, width, fg, bg, flags } (bold, italic, underline,
                inverse, dim, strike, wide-spacer), compact (8-12 bytes)
  screen.rs     the visible rows, cursor, modes, scroll region, tab stops,
                saved cursor, the alternate screen
  history.rs    scrollback ring (rows with a `wrapped` flag), byte-budgeted
  reflow.rs     rewrap logical lines on a width change
  selection.rs  anchors in absolute line coordinates, word/line/block
  search.rs     find in screen + history
  links.rs      URL and OSC 8 hyperlink spans
  input.rs      key + modifiers -> bytes (DECCKM, keypad, Alt, F-keys, xterm
                modifier encoding), mouse -> bytes (X10/1002/1006)
  render.rs     runs of same-attribute cells for a painter (no drawing here)
```

Using `vte` for the parser (alacritty's, `no_std`-capable, no transitive
weight) replaces the hand-rolled `Parse` enum and its 16-byte CSI clamp and
gets intermediates, sub-parameters and OSC/DCS right; the grid semantics on
top stay ours. Width comes from `unicode-width` so CJK and emoji take two
cells. Every file stays under 500 lines.

### 3.2 The Terminal bin

`term.rs` becomes the glue: spawn, drain, feed `lazyterm`, map xui events to
`lazyterm::input`, paint `render.rs` runs, own settings and the window. Split
by responsibility as it grows (`term/settings.rs`, `term/mouse.rs`,
`term/menu.rs`, `term/prefs.rs`). Evidence goes through `libs/tracefmt` on the
`TERM` channel.

### 3.3 Settings in `confd`

Per user, live, under `user/<uid>/term/`. The Terminal reads them at start and
subscribes to `user/<uid>/confd/changed/term/**`, so the preferences dialog,
the Config app and `rhai` (`sys::confd::set`) all change open windows at once.

| Key | Type | Default | Meaning |
|---|---|---|---|
| `font/family` | str | `JetBrains Mono` | any installed monospace family |
| `font/size` | i64 | `15` | design pixels (times the UI scale) |
| `font/line_spacing` | i64 | `130` | line height, percent of the font size |
| `scrollback/lines` | i64 | `10000` | `0` disables; capped by memory, not by fiat |
| `cursor/shape` | str | `block` | `block`, `underline`, `bar` |
| `cursor/blink` | bool | `true` | honours `sys/ui/motion` = `none` |
| `colors/scheme` | str | `desktop` | `desktop` (follows dark/light), or a named scheme |
| `colors/bold_is_bright` | bool | `false` | |
| `select/copy_on_select` | bool | `false` | X11-style |
| `select/word_chars` | str | `-_./~:@` | extra characters a double-click keeps in a word |
| `paste/confirm_multiline` | bool | `true` | ask before pasting text with newlines |
| `bell` | str | `visual` | `none`, `visual`, `sound`, `notify` |
| `window/size` | str | (unset) | initial columns x rows, e.g. `80x24`; unset, `window/last_size` or the window decides |
| `shell` | str | (BusyBox `sh`) | a program path, validated through `libs/fhs` |

Zoom (`Ctrl+=`, `Ctrl+-`, `Ctrl+0`) is per window and not stored.

## 4. Phases

Each phase is shippable on its own and keeps the existing sessions green
(`TERM:UP|CMD|OUT|EXIT`).

### T0. Extract the engine (no visible change)

- Create `lazyterm`; port `grid.rs` onto it with attributes stored but still
  painted in one colour. Port its tests; add a `vttest`-style table of
  sequence -> expected screen, and a seeded fuzz entry (`fuzz::run(&[u8])`,
  `FUZZ_CASES`) shared with a cargo-fuzz target, as for `framering`.
- Cells carry width; wide characters and their spacer behave at the right
  margin, under erase and under cursor movement.
- Add the missing core: scroll regions (DECSTBM), IL/DL/ICH/DCH/ECH, IND/RI,
  DECSC/DECRC, tab stops, DECAWM, DECTCEM (cursor visibility), the
  `wrapped` flag on rows, `CSI 6 n` / DA replies written back to the master
  (programs that wait for them hang otherwise).
- **Evidence fix**: `TERM:OUT` is built from logical (unwrapped) lines, so
  a wrapped command echo is recognised in full. This removes the reason the
  pty is pinned to 80 columns. (Landed in T0: BusyBox's line editor does not
  rely on autowrap; at the last column it sends its own `CR CR LF`, so the
  engine cannot join those rows. `Term::feed` therefore returns `Line {
  text, ends_at_margin }` and `term/evidence.rs` joins full-width rows
  before matching the echo; autowrapped output is joined by the engine.)
- Offscreen snapshot tests of the painter through xui's headless backend,
  written to `xui-app/target/snapshots/term-*.png`.

### T1. Scrollback

- Rows that leave the top of the main screen (not the alternate screen, not a
  scroll region that is not the whole screen) go into `history`. Budget by
  lines from `scrollback/lines`, stored compactly (trailing blanks trimmed,
  attributes run-length coded per row) so 10 000 lines cost well under a
  few MiB.
- Scrolling: wheel (3 lines a notch, `Shift` for a page), `Shift+PgUp/PgDn`,
  `Shift+Home/End`. Any key that sends bytes, and new output when the view is
  at the bottom, return to the bottom; output while scrolled back keeps the
  view anchored (the offset grows with history).
- On the alternate screen with no mouse mode, the wheel sends Up/Down arrows
  (`less`, `vi` scroll as on other terminals).
- A thin overlay scrollbar while scrolled back (fades, `sys/ui/motion`).
- `CSI 3 J` and the menu's *Clear Scrollback* empty history; *Reset* is RIS.
- `view.rs` damage tracking keys on (history offset, rows) so scrolling
  invalidates once, not per row.

### T2. Selection, copy and paste

- **Selecting**: drag selects characters; double-click a word
  (`select/word_chars`); triple-click a logical line; `Alt`+drag a block;
  `Shift`+click extends. Anchors are absolute line numbers, so the selection
  survives output and scrolling and is dropped only when its lines are
  overwritten. Dragging past the top/bottom edge autoscrolls. The I-beam
  cursor (`Cursor::Text`) over the grid.
- **Copying**: `Ctrl+Shift+C` and `Ctrl+Insert` (plain `Ctrl+C` stays `^C`).
  Text joins wrapped rows, keeps hard newlines, trims trailing blanks, maps
  wide spacers away. `copy_on_select` copies on release. *Select All*
  (`Ctrl+Shift+A`) covers history.
- **Pasting**: `Ctrl+Shift+V`, `Shift+Insert`, middle click. Newlines become
  `\r`; when the program enabled bracketed paste (`CSI ? 2004 h`) the text is
  wrapped in `ESC [200~ ... ESC [201~` and any embedded `ESC [201~` is
  stripped (paste injection). Without bracketed paste, a multi-line paste
  asks first (`paste/confirm_multiline`). Control characters other than tab
  and newline are dropped.
- **Mouse reporting**: when a program enables `?1000`/`?1002`/`?1006`, clicks
  and the wheel go to it encoded by `lazyterm::input`; `Shift` held always
  selects instead.
- **Drag and drop**: a `text/uri-list` dropped from Files pastes the
  shell-quoted paths, space separated.
- **The clipboard cap**: `clipboardd` refuses over 8 KiB, which a scrollback
  copy passes easily. Large offers travel in a `Buffer` (shared pages)
  instead of inline, an `idl/clipboard.midl` change (read
  `docs/messenger.md` sections 1 and 5 first); small ones stay inline. The
  Terminal is the first client that needs it; every xui app benefits.
- OSC 52 (a program setting the clipboard, `tmux`, remote `vi`) is supported
  write-only and off by default.

### T3. Fonts and appearance

- **Metrics from the font**, not `0.6 em`: the cell width is the advance of
  `M`, the cell height `line_spacing` of the size, both rounded to whole
  physical pixels at the UI scale so columns never drift. This needs a
  measure call on `xui_canvas` for a family (check what xui exposes; add one
  upstream if it does not).
- **Families**: JetBrains Mono (built in), Liberation Mono (already shipped in
  desktop images for LazyWeb), and any monospace `.ttf`/`.otf` under
  `/system/share/fonts` and `~/.local/share/fonts` (paths in `libs/fhs`).
  A family is offered only if its `i` and `M` advances match. Add JetBrains
  Mono Bold and Italic to `assets/` (OFL-1.1, manifest lines) so bold is a
  real face; until then bold is drawn with a 1-px offset.
- **Painting runs**: one `draw_text` per run of equal attributes, background
  rectangles first, then text, then underline/strike, then the cursor;
  inverse and selection swap colours; dim halves alpha. Box-drawing and block
  characters (U+2500-259F) are drawn as geometry so lines join across cells
  at every size.
- **Cursor**: block, underline or bar; hollow when the window is unfocused;
  blink on a timer that stops when idle or unfocused, so an idle terminal
  still costs no frames.
- **Colours**: the 16-colour palette per scheme (`desktop` follows
  `sys/ui/mode` and the accent; ship a handful of named schemes), 256-colour
  cube, 24-bit SGR (`38;2`/`48;2`, with `:` sub-parameters). OSC 4/10/11/12
  query and set. The selection colour comes from the theme accent.
- **Zoom**: `Ctrl+=`/`Ctrl+-`/`Ctrl+0` and `Ctrl`+wheel change the size of
  this window; with a fixed window the grid changes, with T4 it reflows.

### T4. Resize and geometry

- The window becomes resizable (`backend.set_size_hints` with a minimum of
  20 x 5 cells) and maximizable. On a new client size (or a font change)
  compute columns and rows, reflow screen and history (`reflow.rs`, logical
  lines rewrap; the cursor follows its character), then `TIOCSWINSZ` on the
  master: the kernel sends `SIGWINCH`. Check BusyBox's
  `FEATURE_EDITING_WINCH` is on so ash redraws its line.
- The alternate screen is not reflowed (the program repaints after
  `SIGWINCH`), only cropped or padded.
- While the size changes, a centred `100 x 30` badge for a moment.
- `window/size` sets the initial geometry; the last size is remembered.
- Session images keep a known size: sessions set `window/size` explicitly so
  coordinates in scripts stay stable.

As built: columns and rows are `floor((client - 2 * pad) / cell)` with a
floor of 20 x 5 (`appearance.rs::grid_for`); a font, size, spacing or zoom
change recomputes them as well. `lazyterm::Term::resize` reflows the main
screen and the scrollback (`reflow.rs`: logical lines rewrap, the cursor stays
on its character, wide characters never split, the scrollback budget applies)
and returns a `Remap` the Terminal uses to keep a scrolled-back view on its
text; a selection is cleared by a resize (mapping columns per cell was not
worth it). The alternate screen is cropped or padded. Then `TIOCSWINSZ`.
`window/size` (unset by default; a value like `80x24`, validated 20..500 x
5..200) sets the first grid, which
the Terminal asks the compositor for (`request_size`); `window/last_size` is
written two seconds after the user last resized the window (never for a font
or zoom change, never while zoomed) and used when `window/size` is unset, so
**session images that must not inherit a size build with `LAZYOS_RESET_OS=1`**.
BusyBox `vi` re-reads the window size on its next key, not on `SIGWINCH`
(Ctrl+L redraws it at the new size); the shell's own line editor redraws
(`FEATURE_EDITING_WINCH`, `tools/abi/busybox.py`). Evidence:
`TERM:RESIZE:<cols>x<rows>`. Sessions: `term_resize.json`,
`term_window_size.json`.

Sessions that must not inherit a size: the remembered `window/last_size`
survives on a non-reset OS volume, so build the image with
`LAZYOS_RESET_OS=1` or give the session an explicit `window/size`.

### T5. `TERM`, keys and the rest of xterm's core

- Ship a terminfo entry and set `TERM=xterm-256color` (compiled entry under
  `/system/share/terminfo/x/`, `TERMINFO_DIRS` from `libs/fhs`), plus
  `COLORTERM=truecolor`. BusyBox's own `vi` and `less` ignore terminfo and
  keep working. Linux programs from `LAZYOS_LINUXAPPS=1` (`rg --color`,
  `sqlite3`) and any future ncurses program then get colour and keys.
- Keys: `Alt` as an ESC prefix, F1-F12, modified arrows/Home/End
  (`ESC [1;5C`), application cursor and keypad modes (DECCKM, DECKPAM),
  `Ctrl+Space`/`Ctrl+@`, `Ctrl+[`/`\`/`]`/`^`/`_`, `Shift+Tab` (`ESC [Z`).
- OSC 0/2 window title (shown in the title bar and taskbar; the default title
  is `Terminal` or the running command's name), OSC 7 working directory (for
  *New Window* in the same directory; enable it in the default `PS1`), OSC 8
  hyperlinks.
- Focus reporting (`?1004`), synchronized output (`?2026`, hold paints until
  the end mark) so full-screen redraws do not tear.

As built: `TERM=xterm-256color`, `COLORTERM=truecolor`,
`TERMINFO_DIRS=/system/share/terminfo` (`fhs::share::TERMINFO`) and a `PS1`
that is BusyBox's default prompt behind a non-printing OSC 7 (`\[\e]7;file://\w\a\]`,
`FEATURE_EDITING_FANCY_PROMPT`; `\w` abbreviates `$HOME` to `~`, which the
Terminal expands) are the child's environment. The entry's source is
`assets/terminfo/xterm-256color.src`: xterm-256color trimmed to what lazyterm
does (no alternate character set, no insert mode, no blink). The image build
compiles it with `libs/terminfo` (a source parser and a writer of the legacy
compiled format; its output is byte-identical to ncurses' `tic` for the same
source, which was checked once by hand; host tests round-trip it and keep
the key capabilities equal to what `lazyterm::keys` sends and the output
capabilities equal to what the parser does).

Keys are `lazyterm::keys` (xterm's encodings: Alt as an ESC prefix, F1-F12,
modified arrows and navigation keys as `CSI 1;<m>`, DECCKM, DECKPAM, Shift+Tab
as `CSI Z`, Ctrl+Space/@/[/\\/]/^/_//?); `term/chords.rs` maps xui's keys
(US layout) onto them. The keypad's DECKPAM encoding exists and is tested, but
xui's `Key` has no numeric-keypad codes, so nothing is wired to it. Ctrl+- is
still the zoom; Ctrl+Shift+- is Ctrl+_. OSC 0/2 set the window title
(printable characters, at most 256; an empty one or a reset restores
"Terminal"; the title is not reset when the shell returns to its prompt),
OSC 7 the working directory (`TERM:CWD:`), OSC 8 hyperlinks (an interned table
bounded to 8192 links, a link id per cell: rendering and clicking are T6).
Focus reporting (`?1004`) follows the *window's* keyboard focus
(`LazyOSBackend::on_keyboard_focus`, from the input session's Enter and
Leave), which also drives the hollow cursor; synchronized output (`?2026`)
holds the repaint for at most 150 ms. Device attributes: primary
`CSI ? 62 ; 22 c` (VT220 with ANSI colour, claiming only what is
implemented), secondary `CSI > 0 ; 10 ; 0 c` (a version low enough that no
program enables xterm extensions lazyterm lacks). `vte` is built without its
`std` feature so an unterminated OSC cannot grow past 4096 bytes. Known gaps:
xui's `Key` has no numeric-keypad codes (DECKPAM is encoded and tested but
nothing is wired to it), and Alt/Ctrl characters assume a US layout (`Key`
names the key, not the character a layout makes of it). Session:
`tools/screenshot/examples/term_xterm.json` (needs `rhai` and
`LAZYOS_LINUXAPPS=1` for the `rg` part).

### T6. Convenience

- **Context menu** (right click; `Shift+F10` / Menu key): Copy, Paste, Select
  All, Find..., Open Link / Copy Link (on a link), Clear Scrollback, Reset,
  New Window, Preferences..., Close.
- **Links**: URLs and `file://` paths detected per logical line, OSC 8 links
  as given; underlined on hover with `Ctrl`, `Ctrl+click` opens through
  `mimed` (`sys::mimed::open` path, `docs/lazyweb.md`); never on a plain click.
- **Find** (`Ctrl+Shift+F`): a small bar, matches highlighted in screen and
  history, Enter/Shift+Enter step, case toggle, plain text (regex later).
- **Bell**: `visual` flashes the frame; `sound` plays a short tone through
  `audiod` (`libs/audioclient`); `notify` posts an Info toast when the window
  is not focused. Rate-limited.
- **New Window** (`Ctrl+Shift+N`): launches another Terminal through `init`,
  in the OSC 7 directory.
- **Close with a job running**: if the pty's foreground group
  (`TIOCGPGRP`) is not the shell, ask before closing.
- **Preferences** (`Ctrl+,`): a dialog in the Terminal (font family list with
  a live preview line, size, line spacing, scheme swatches, cursor, scrollback
  lines, bell, copy-on-select, paste confirm) writing the `confd` keys of
  section 3.3; changes apply live through the confd topic, so there is no
  Apply button.
- **Save output** (menu): write screen + history to a file chosen in the Save
  dialog.

As built (T6): the menu, the find bar, the close confirmation and the
Preferences panel are drawn in the window by the Terminal itself (`chrome.rs`,
`menu.rs`, `findbar.rs`, `prefs.rs`) instead of xui popup nodes: they get
probe names and headless snapshots, stay clamped to the window, and never
block the pty drain (all state lives in a shared `Chrome`). Key routing is
prompt, then preferences, menu, find. Links: `urls::find_urls` per logical
line plus OSC 8 spans; Ctrl+click or the menu item opens them through `mimed`
(`launcher::open_url`) after `helpers::sanitize_url`; a link whose text
differs from its target asks first. Find is `lazyterm::search` (incremental,
budgeted, eviction-aware, matches across wrapped rows). Bell: `bell` =
none/visual/sound/notify, 500 ms rate limit. New Window runs
`xui_app::shell::launch` with the OSC 7 directory (validated, else home).
Closing while a job runs needs a veto from the compositor: display method
`SetCloseMode` (48, `idl/display.midl`) makes `xuid` ask the client instead of
forgetting the surface, but an app can never make its window unclosable: the
first close is passed on, and a close more than 3 s later drops the surface
whatever the app says (the app quits when its event endpoint closes;
`TERM:QUIT:PASS`); a close within 30 s of the first always wins, and one after that asks again. The busy test reads the kernel task table (`jobs.rs`),
since ash has no job control (evidence `TERM:CLOSE:CHECK`). Pointer events now
carry modifiers (Ctrl+click, Shift+right click). Evidence: `TERM:MENU:<item>`,
`TERM:LINK:OPEN:<url>`, `TERM:FIND:<n>`, `TERM:BELL:<kind>`,
`TERM:NEWWIN:<cwd>`, `TERM:CLOSE:CONFIRM|CANCEL|FORCE`,
`TERM:PREFS:SET:<key>`, `TERM:SAVE:<bytes>`. Session:
`tools/screenshot/examples/term_menu.json`.

Known gaps: BusyBox `vi` does not follow SIGWINCH; keypad and non-US layout
handling is basic; the kernel pty has no back-pressure past the line
discipline's 4096-byte input window (a separate task). Bold and italic faces
are no longer a gap (JetBrains Mono Bold, Italic, BoldItalic ship).

## 5. Security notes

- Everything the child prints is untrusted input: the parser bounds every
  parameter count and string (OSC payloads, titles) and is fuzzed; titles are
  sanitized before reaching the compositor.
- Paste injection: bracketed-paste end markers are stripped from pasted text;
  multi-line paste confirmation without it.
- No query that echoes attacker-controlled bytes back into the input (no
  title report `CSI 21 t`, no DECRQSS of arbitrary strings).
- OSC 52 read is never supported; write is opt-in.
- Links open only on `Ctrl+click`, only through `mimed`, and the menu shows
  the target first.
- `shell` from confd is a path under `/system/bin` or the user's home, run
  with no extra privilege (the Terminal has none to give).

## 6. Verification

Headless first, QEMU last.

- **Host**: `cargo test --manifest-path xui-app/Cargo.toml -p lazyterm`
  (sequence tables, reflow round-trips, selection extraction, search, input
  encoding, scrollback budget) and
  `FUZZ_CASES=20000 cargo test ... -p lazyterm --release seeded`.
- **Offscreen snapshots**: the painter on xui's headless backend: colours,
  attributes, wide characters, box drawing, each cursor shape, selection,
  font sizes at scale 1 and 2. Read the PNGs.
- **Sessions** (desktop, `LAZYOS_UI_PROBE=1`, `LAZYOS_XUI_AUTOSTART=term`),
  each with markers judged from the serial log and screenshots inspected:
  - `term_scrollback.json`: `seq 1 500`, wheel and `Shift+PgUp` back, a
    screenshot showing line 1; `TERM:SCROLL:<offset>`.
  - `term_select_copy.json`: drag, double- and triple-click, copy, paste into
    the shell; `TERM:COPY:<bytes>`, `TERM:PASTE:<bytes>`, then `TERM:OUT` of
    the pasted command.
  - `term_font.json`: set `user/1000/term/font/*` from `rhai`, watch the
    window change; `TERM:FONT:<family>:<size>:<cols>x<rows>`.
  - `term_resize.json`: drag the corner, `stty size` reports the new size;
    `TERM:RESIZE:<cols>x<rows>`; `vi` redraws.
  - `term_colors.json`: a 256-colour and true-colour test pattern.
  - `term_menu.json`: context menu, Find, Preferences, a link `Ctrl+click`.
- **Existing sessions** that type into the Terminal (`doom.json`,
  `rhai_msg*.json`, `pkg_install.json`, ...) must still pass unchanged after
  every phase; they are the regression suite for the evidence markers.
- **Performance**: `TERM:PERF` must not regress (key-to-paint and output
  throughput: `cat` of a 1 MiB file); idle costs zero frames with the cursor
  blink stopped.

## 7. Launcher and packaging

The Terminal is in every desktop image and is not a core package
(`docs/packages.md`), so no build switch, `run_demo.py` flag or GUI control is
needed. New shipped files (bold/italic faces, terminfo) go through
`assets/manifest.txt` and `/system/.image-manifest` as usual.

## 8. Order and size

| Phase | Depends on | Size |
|---|---|---|
| T0 engine | - | L |
| T1 scrollback | T0 | M |
| T2 selection/clipboard | T0, T1 (clipboard IDL change separable) | M |
| T3 fonts/appearance | T0 | M |
| T4 resize | T0 (reflow), T3 (metrics) | M |
| T5 TERM/keys | T0, T3 (colours) | M |
| T6 convenience | T2, T3 | M |

T1, T2 and T3 can proceed in parallel once T0 lands.

## 9. Decisions

1. **No tabs**: windows only (New Window, LazyShell's taskbar groups them).
2. **Preferences**: the in-app dialog (`Ctrl+,`), not a Settings page.
3. **`TERM=xterm-256color`**, with the shipped entry trimmed to what T0-T5
   implement.
4. **Clipboard**: large offers travel in a shared-memory `Buffer` through
   `clipboardd` (an `idl/clipboard.midl` change), not a raised inline cap.
