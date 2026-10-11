# emusic: native look and a working Add folder

Status: phases 0, 1 and 2 are done, phase 3 is done except the items under
"Open" in it; phase 4 (wiring) is mostly covered by the sessions.
Context: [`media-plan.md`](media-plan.md), [`../emusic/README.md`](../emusic/README.md).

## Findings

1. **"Add folder" does nothing.** Settings -> Library -> Add folder calls
   `emusic_ui::folder_picker::request()`, which runs
   `file_picker::pick_folder()` on a worker thread. The LazyOS build has no
   `rfd` and installs no `FilePicker`, so every dialog answers "cancelled".
   xui's `FileDialog` (at the pinned revision) has no folder mode, and it is a
   widget that lives on the UI thread, while the seam is a blocking call from
   a worker thread: a `FilePicker` impl in `lazyemusic` cannot just show it.
2. **It used the old dark mode.** emusic's portable frontend built its theme
   from `xui_core::Theme::dark()`/`light()` and its own accent, ignoring
   `Backend::system_theme()` (the desktop's mode and accent, which every other
   LazyOS app takes through `run_themed`). Result: a green desktop frame around
   grey content, and an orange accent.
3. **Glitches seen in a screenshot** (to confirm in a live run, phase 3):
   tofu box where the album art placeholder is; status bar with an orange dot,
   "Ready" floating mid-bar and empty segments; the Settings pane narrower than
   the window with Now Playing butted against it; the track table header clipped
   at the right edge; disabled buttons styled differently from enabled ones.
4. **The manifest declares `read:$HOME/Music` and `read:$HOME/Downloads`.**
   Today file permissions are not enforced (docs/packages.md, "file
   permissions are not enforced"), so a track opened from Files or `mimed`
   plays from any directory, and a folder chosen elsewhere would scan too. The
   manifest is what the consent screen shows, and enforcement may arrive, so
   the picker steers people to the declared folders.

## Decisions

- The change is made in the emusic repo (`va1erian/emusic`) and the LazyOS pin
  follows it.
- **Follow system is the default**, with Dark and Light as overrides.
- The manifest keeps its directories. A single track opens from anywhere:
  `mimed`/Files hand the path to the app, and nothing enforces file
  permissions today (finding 4). If enforcement arrives, opening a document
  must grant its path (as `lrplay` does for its document's folder).

## Phases

### 0. Baseline (done)

Screenshots of the old build (the report that started this) and a reproducible
session for the new one: `python tools/emusic/session.py --write` generates
`tools/screenshot/examples/emusic_theme.json`; it needs
`LAZYOS_DESKTOP=1 LAZYOS_EMUSIC=1 LAZYOS_UI_PROBE=1 LAZYOS_XUI_AUTOSTART=term
LAZYOS_RESET_OS=1` and takes screenshots in the desktop's dark theme, then
after `confctl set user/1000/ui/mode str light`.

### 1. Follow the desktop theme (done, emusic#lazyos-follow-system-theme)

- `Config.follow_system: Option<bool>` (`None`: the host decides), stored
  with the other appearance settings; `Command::SetFollowSystem`; the View
  menu's *toggle theme* leaves "follow" (it is a choice by hand).
- `emusic-frontend-portable`: `run_on` asks the host's backend for a system
  theme; when it has one, Appearance gets a **Follow system** radio (first,
  and selected unless the user chose a scheme), the window is painted in the
  host's theme as is (it carries the desktop's accent, so the accent swatches
  are disabled), and the host's theme is read about once a second so the window
  re-themes when the desktop does. Hosts without a system theme (Windows, the
  winit canvas) are unchanged.
- Verified by: `cargo test -p emusic-ui -p emusic-frontend-portable`, and the
  `emusic_theme.json` session.

### 1b. Resizable window (done)

`lazyemusic` declares size hints (`LazyOSBackend::set_size_hints`, minimum
900x540 design pixels, no maximum), so the compositor's frame resizes and
maximizes it and emusic's views re-flow on `Event::Resize`. The minimum is the
smallest size at which emusic's own layout does not overlap (Now Playing over
the track table below it); fixing that lets the minimum come down (phase 3).
Session: `tools/screenshot/examples/emusic_resize.json`.

### 2. A working Add folder (done)

Decided against a folder mode in xui's `FileDialog`: it would touch xui's
Win32 backend dialog request and need a pin bump across every xui consumer, for
one dialog. Instead emusic's portable frontend carries a small folder browser
(`crates/frontend-portable/src/dialogs/folder_picker.rs`) built from existing
xui widgets in a secondary window, like its other dialogs:

- Add folder opens it where there is no platform dialog
  (`file_picker::native_available()` is false: no `rfd`, no installed
  `FilePicker`); hosts with one are unchanged.
- It starts in `~/Music` (else `$HOME`), lists subfolders with `..` first,
  Enter/double-click enters a folder, **Choose** takes the selected or current
  one and dispatches `Command::LibraryAddFolder`, so it reaches the state, the
  config and the library, like Remove does.
- Verified by `tools/screenshot/examples/emusic_add_folder.json` (the folder
  appears in Settings -> Library and the status bar counts its track).
- Open: the other synchronous `pick_folder()`/`pick_file()` calls (MIDI,
  SID, visualization presets) still answer "cancelled" on LazyOS; they are
  off the music path.

### 3. Usability and glitch pass (done, see Open)

Findings, each fixed at its source in emusic's portable frontend:

- **Tofu boxes.** The UI face is Droid Sans (Latin, Greek, Cyrillic; 873
  glyphs); none of the fonts in the tree has the music note (U+266A, missing
  from Droid Sans), the stars (U+2605/2606) or the play triangle (U+25B6).
  A full Unicode font was not needed for these: they are drawn as icons now
  (the navigator's Music raster for the note placeholder, xui's `Star`,
  `StarFilled` and `Play` glyphs; the track table uses the row icon, so the
  playing row shows a play triangle instead of its star). The streaming
  status text lost its arrows.
- **Clipped, squeezed track table.** The data columns (about 760 dip) did not
  fit the 540 dip left beside the right panel, so Title was squeezed and the
  table clipped. `fitting_columns` shows the data columns in priority order
  (Artist, Album, Time, Genre, Year, Plays, Format, Last played, File) while
  Title keeps 140 dip; the others get width 0 and come back when the window
  grows.
- **Empty state.** An empty Music view shows "Your library is empty" and an
  **Add music folder...** button (below the table header) instead of a blank
  list.
- Session: `tools/screenshot/examples/emusic_library.json` (empty prompt, the
  folder browser, the fitted table, a playing row; needs a sound card).

Open:

- **Non-Latin tags** (CJK, Arabic, Hebrew, Thai, Korean) still render as tofu:
  Droid Sans has none of them. That is the case for a full Unicode fallback
  font in xui's text shaper (a CJK face is 10 to 16 MB, so it should be a
  `LAZYOS_` image option, not compiled into every xui app). Separate decision.
- **The 900x540 minimum** can come down once the right panel's summary no
  longer overlaps the track table below about 840 px wide.
- A playing track shows no star in the table (one icon per row); the star is
  in the Now Playing panel.
- HiDPI (`--hidpi`) not checked.

### 4. Verification and wiring

`emusic.json`/`emusic_theme.json` click `Add folder` by name through the
`UI:WIDGET` probe, pick `~/Music` and assert new markers
(`EMUSIC:LIBRARY:FOLDER:<path>`, `EMUSIC:SCAN:DONE:<n>`); a light-theme
session; `tools/emusic/run.py --app` still passes its sound judge; docs and
README lose the "no file dialogs" caveat.
