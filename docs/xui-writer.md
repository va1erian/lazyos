# LazyWriter: a word processor on xui-rich-text

LazyWriter (`writer`, the core package `os.lazy.writer`,
`xui-app/packages/writer/`) edits rich documents in a `xuid` window: bold,
italic, underline and strike-through, sizes, three font families, colour,
highlight and links; headings, quotes, alignment including justify, indents,
bullet and numbered lists; and PNG or JPEG pictures, inline or floating with
the text flowing around them. It saves its own format (`.lzw`), exports
GitHub Flavored Markdown, and opens plain text. Every toolbar action has a
Lucide icon, and the window follows the desktop's light or dark theme. It
ships in every `LAZYOS_DESKTOP=1` image, is listed in the start menu under
**Office**, and Files opens `.lzw` documents in it with a double-click. The
user-facing summary (features, shortcuts, limits) is the package README,
[`xui-app/packages/writer/docs/README.md`](../xui-app/packages/writer/docs/README.md);
the plan and its decisions are issue #533.

## How it works

```
keys, pointer --> LazyOSBackend --> Ui --> RichTextEditor<Msg> --exec(Command)--> Document
                                      |            |  on_change / on_selection
                    toolbar, format row, dialogs   +--> Msg --> app: title, status bar, toggles
```

* **The editor is xui's.** The document model, layout, painting, caret,
  selection, undo and the in-process clipboard all come from
  [`xui-rich-text`](https://github.com/va1erian/xui/tree/main/crates/xui-rich-text)
  (`RichTextEditor<Msg>`). LazyWriter is a port of that crate's `wordpad`
  example (`examples/wordpad/`) onto `xui_app::backend::LazyOSBackend`, with
  the Editor app's skeleton (`xui-app/src/bin/editor.rs`): `main()` binds the
  backend, opens a 960x600 window titled `LazyWriter` (minimum 480x320; the
  issue's 960x680 is taller than the 1280x720 desktop leaves above the
  taskbar once `xuid` adds its title bar, which hid the status bar), prints
  `WRITER:UP:PASS` on the first frame, and opens the path given on the
  command line (`writer --client <path>`, read with
  `xui_app::platform::argv::file_arg`). The app itself (state, commands,
  file logic, widget tree) is the portable crate `xui-app/crates/writer`
  (`xui-writer`); `xui-app/src/bin/writer.rs` supplies the LazyOS side
  (backend, fonts, atomic writes, start folder) through its `Host`.
* **Every action is a `Command`.** The toolbar (New, Open, Save, Export;
  Undo, Redo; Cut, Copy, Paste; Image, Link), the format row (block style,
  font family, size, B/I/U/S, the four alignments, bullets, numbers, indent,
  outdent, image wrap) and the shortcuts all go through
  `RichTextEditor::exec(Command::...)`; the app never edits the document
  itself, so undo covers everything. After a command the editor gets focus
  back and the toggles are re-synced from the last `StyleSummary` (a mixed
  attribute shows as off); the image wrap picker reads the selected image's
  wrap from the document, since the summary carries none.
* **Shortcuts.** `Ctrl+N` (New), `Ctrl+O` (Open), `Ctrl+S` (Save),
  `Ctrl+Shift+S` (Save As), `Ctrl+E` (Export as Markdown) and `Ctrl+Q`
  (Quit) are the app's (`ui.on_key`; the Windows key counts as `Ctrl`, and
  any combination with `Alt`, which includes AltGr, is left as typing);
  `Ctrl+B/I/U/Z/Y/X/C/V/A` are the editor's own. The toolbar tooltips name
  the shortcuts (`Save (Ctrl+S)`), and each icon-only button in the format
  row has a tooltip with its name. A `dialog_open` flag (the Editor's
  pattern) keeps `Esc`, `Enter` and the app's shortcuts from reaching the
  document while a dialog is up, and stops a second dialog opening over it.
* **Status and title.** The status bar shows the file name (or "Untitled"),
  Saved or Modified, and a word count taken from the plain text on each
  change; the title is `LazyWriter: <name>`, `LazyWriter: *<name>` while
  modified. A document is modified once `on_change` fires after a save,
  open or new. Clicking a link shows its address in the status bar.
* **Dialogs.** Open, Save, Export and Insert image are xui's portable
  `FileDialog` over `StdFileSystem`, held for the window's lifetime. They
  start in `$HOME` when it is an absolute, existing directory, else in
  `fhs::mount::TRANSIENT` (the rule the Installer's picker uses, shared
  rather than copied). New, Open, Quit and closing the window ask first
  when the document is modified: a "Save changes?" prompt with **Save**,
  **Discard** and **Cancel** (`Esc` cancels too). Save on an untitled
  document goes through Save As and then carries on; cancelling that Save
  As cancels the New, Open or Quit as well.
* **Link.** The toolbar's Link button opens a "Link" prompt, pre-filled
  with `https://`, for the address of the selected text; accepting an empty
  address removes the link. It is one `Command::SetCharStyle`, so undo
  removes it.

## Formats

| Format | Direction | What |
|---|---|---|
| `.lzw` (`application/x-lazywriter`) | open, save | xui-rich-text's versioned JSON (`format::to_json` / `from_json`, crate feature `serde`) with every picture embedded as PNG. |
| `.md` | export only | GitHub Flavored Markdown (`format::to_markdown`). A document with pictures also gets `<name>_images/1.png`, `2.png`, ... beside the `.md`; the folder is created only when there is a picture. |
| `.txt` | open only | Plain text (`Document::from_plain_text`), from the Open dialog. Save on a document opened from `.txt` never writes JSON over it: it opens Save As, suggesting `<name>.lzw`. |

* **Names.** Save As suggests `<name>.lzw` (`Untitled.lzw` for a new
  document) and Export `<name>.md`; a name typed without an extension gets
  `.lzw` (or `.md`), one typed with another extension is kept.
* **Saving is atomic.** The document is written to a temporary file that is
  then renamed over the target, and a symlink target is refused, through the
  existing atomic-write helper (not a third copy of it). The serial line is
  `WRITER:SAVE:PASS:<path>` or `WRITER:SAVE:FAIL:<path>`. ext2 writes reach
  the disk at the next block-cache commit (at most 5 s later): a session that
  reboots to check a saved file waits past that before it quits.
* **Opening is bounded.** At most 32 MiB is read (the Editor's cap); a larger
  file, a malformed `.lzw` or an unknown format version is shown in a dialog
  as text (`WRITER:OPEN:FAIL:<path>`), never a panic. A good open prints
  `WRITER:OPEN:PASS:<path>`, an export `WRITER:EXPORT:PASS:<path>`.
* **Pictures.** Insert image reads at most 16 MiB and checks the PNG or
  JPEG header before decoding: a picture of more than 4096 x 4096 pixels
  (16 777 216 in all, 64 MiB decoded) is refused with a message instead of
  being decoded. It then decodes with `xui_core::Image::decode` and scales
  the picture to at most 360 dip wide, keeping its aspect ratio
  (`WRITER:IMAGE:PASS:<path>` or `WRITER:IMAGE:FAIL:<path>`). The new
  picture is left selected, so the next key typed replaces it: move the
  caret first (`End`, an arrow or a click). The image wrap picker (Inline, Float left, Float
  right, Top and bottom) is enabled only while a picture is selected.
* **MIME.** `mimed` maps `.lzw` to `application/x-lazywriter` (a manifest
  cannot name extensions) and defaults its `open` and `edit` verbs to
  `os.lazy.writer`. LazyWriter does not register for `text/plain` or
  `text/markdown`: those stay with the Editor and Docs.

## Fonts

Three families, registered by `xui_app::font::register_writer()`: **Sans**
(Droid Sans regular and bold, also the UI font), **Serif** (Droid Serif
regular, `assets/fonts/DroidSerif-Regular.ttf`) and **Mono** (JetBrains Mono).
Fonts are `include_bytes!`'d into the binary, since LazyOS files cannot be
memory-mapped. There is no italic face in any family, so italic is
synthesised (slanted) by cosmic-text from the regular face. A real italic face is a follow-up that has
to fit the package's 8 MiB file limit.

## Clipboard

There is no clipboard code in the app. Copy and paste inside LazyWriter keep
the formatting through xui-rich-text's in-process fragment store. What other
apps see goes `RichTextEditor` -> `Ui` -> `LazyOSBackend` -> `clipboardd`
(`xui-app/src/platform/clipboard.rs`), which carries plain text only, capped
at 8 KiB; pasting from another app inserts plain text.

## Known limits

* One continuous column: no page view, no printing, no IME or bidi (the v1
  scope of `xui-rich-text`).
* No import of Markdown, HTML, RTF or Word documents; Markdown is export only.
* The clipboard between apps is plain text, at most 8 KiB.
* A path containing a space cannot be opened from Files or `pkgctl open` (a
  launcher limit, `xui-app/src/platform/argv.rs`); the Open dialog can.
* Italic is synthesised, so it looks rougher than a designed face.

## Verification

Serial markers, one line each: `WRITER:UP:PASS` (first frame),
`WRITER:BIND:FAIL:<code>` (no display), `WRITER:RUN:FAIL:<err>` (the loop
failed), and `WRITER:OPEN|SAVE|EXPORT|IMAGE:PASS|FAIL:<path>` as files are
used.

```bash
cargo test --manifest-path xui-app/Cargo.toml --workspace --lib   # file logic: default dir, names, caps, open errors
cargo test --manifest-path xui-app/Cargo.toml -p xui-writer       # plus the offscreen window snapshots and message-loop tests
python tools/xui/build.py && LAZYOS_DESKTOP=1 LAZYOS_XUI_AUTOSTART=writer cargo build
python tools/screenshot/qemu_session.py --image target/lazyos.img --out shots/writer --timeout 300 \
    --script tools/screenshot/examples/xui_writer.json \
    --fail-on "WRITER:[A-Z]+:FAIL" --fail-on "INIT:AUTOSTART:FAIL"
python tools/screenshot/pngstats.py shots/writer/*.png --min-nonblack 0.01
```

Then read the screenshots with the Read tool. The session (`xui_writer.json`)
waits for `PKGD:PROVISION:DONE` and `WRITER:UP:PASS`, types a line and bolds
its last word (`Ctrl+Shift+Left`, `Ctrl+B`: `shot_02_bold_word`, with the B
toggle checked), centres the next line from the format row (`03_centred`),
makes a two-item bullet list (`04_list`), inserts
`/system/share/samples/writer-sample.png` (`fhs::share::WRITER_SAMPLE_IMAGE`)
from the toolbar (`05_image_dialog`, gated on `WRITER:IMAGE:PASS`), floats it
left with the wrap picker (`06a_wrap_menu`, `06b_wrap_float`) and types a
paragraph that wraps beside it (`06_document`). It then saves
`/tmp/writer.lzw` (`Ctrl+S`, `07*`, `08_saved`), exports `/tmp/writer.md`
(`Ctrl+E`, `09*`, `10_exported`), starts a new document (`Ctrl+N`,
`11_new_document`) and reopens the saved one (`Ctrl+O`, `12*`,
`13_reopened`, which must look like `06_document`). Each file step is
confirmed by its serial marker and re-sent if the marker does not come.
The steps are timed for TCG (a typed character can take a third of a second
to land there), so the session runs about five minutes either way.

The toolbar and format-row clicks are absolute screen positions (the pointer
is first pinned to the top-left corner, then moved along the top edge and
down so it crosses no other control): `xuid` places the window at (48, 48),
so its client area starts at (49, 70), the toolbar's centre line is at
y = 87 and the format row's at y = 122. If the layout changes, adjust the
steps marked with a `note`.

The desktop starts dark. `xui_writer_light.json` checks the light theme on
an image with the Terminal autostarted instead
(`LAZYOS_XUI_AUTOSTART=term`, BusyBox for its shell): it runs
`confctl set sys/ui/mode str light`, waits for `xuid`'s `THEME:APPLIED`,
starts LazyWriter from the Terminal (`/apps/os.lazy.writer/*/bin/writer.elf
--client &`; an app reads the theme once, at start, and its
`WRITER:UP:PASS` reaches serial as the Terminal's `TERM:OUT`), shows it
empty (`01_light_started`), types a line and bolds its last word
(`02_light_bold`), then opens the unsaved-changes prompt with `Ctrl+N`
(`03_light_unsaved_prompt`: Cancel, Save, Discard) and cancels it with
`Esc` (`04_light_prompt_cancelled`). `xuid` cascades this window below the
Terminal's, at (112, 64).

```bash
LAZYOS_DESKTOP=1 LAZYOS_XUI_AUTOSTART=term cargo build
python tools/screenshot/qemu_session.py --image target/lazyos.img --out shots/writer_light --timeout 300 \
    --script tools/screenshot/examples/xui_writer_light.json \
    --fail-on "WRITER:[A-Z]+:FAIL" --fail-on "INIT:AUTOSTART:FAIL"
```

The `xui` CI workflow runs both sessions ("Capture LazyWriter" and "Capture
LazyWriter in the light theme"). The first requires the four markers, fails on
any `WRITER:*:FAIL` or a failed `init` launch, and checks that the reopened
document's area (below the format row, above the status bar) differs from the
empty one and is not blank; the second requires `THEME:APPLIED` and a light
document area. `tools/screenshot/examples/core_apps.json` launches the app under its
label for the `LAZYOS_LABEL_TRACE=1` permission check (docs/packages.md).
