# emusic: native look and a working Add folder

Status: phases 0 and 1 are done in this change; phases 2 to 4 are open.
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

### 2. A working Add folder

1. xui: a folder mode for `FileDialog` (`FileDialogMode::Folder`).
2. emusic: a non-blocking in-window request. Keep `folder_picker::request()`
   and its channel (the library backend already drains it); let the portable
   shell register a UI-thread requester, show the `FileDialog` on the main
   window and send the result into the same channel. Route the other
   `pick_folder()`/`pick_file()` calls made on the UI thread through the same
   path or hide them on LazyOS, so none blocks the UI thread.
3. The dialog starts in `~/Music` with shortcuts to Music and Downloads and
   says why a folder outside the granted ones will not scan.
4. Stop-gap if (1) is slow: a typed-path field and an Add button on the
   Library page.

### 3. Usability and glitch pass

Fix each finding at its source, check the empty state (a prompt in the main
pane, not only in Settings), minimum window size, keyboard (Tab order, Enter,
Esc), HiDPI (`--hidpi`), and the whole flow: add folder, scan, appears in
Music, double-click plays, seek, volume.

### 4. Verification and wiring

`emusic.json`/`emusic_theme.json` click `Add folder` by name through the
`UI:WIDGET` probe, pick `~/Music` and assert new markers
(`EMUSIC:LIBRARY:FOLDER:<path>`, `EMUSIC:SCAN:DONE:<n>`); a light-theme
session; `tools/emusic/run.py --app` still passes its sound judge; docs and
README lose the "no file dialogs" caveat.
