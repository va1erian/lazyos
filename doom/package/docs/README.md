# Doom

Doom on the [doomgeneric](https://github.com/ozkl/doomgeneric) engine, playing
**Freedoom: Phase 1**, the free and complete replacement for the original
episodes.

## Controls

| Key | Action |
|---|---|
| Arrow keys | move and turn |
| Ctrl | fire |
| Space | open doors, press switches |
| Shift (held) | run |
| Alt (held) + Left/Right | strafe |
| `,` and `.` | strafe left and right |
| `1`..`7` | choose a weapon |
| Tab | automap |
| Esc | menu |
| F2 / F3 | save / load |

Without the input service (`inputd`) the window only receives plain keys, so
`F` fires and `R` runs instead.

## Files

Settings (`default.cfg`) and saved games (`.savegame/`) are kept in
`~/.doom` (your home directory), so they survive reboots and reinstalling the
package. A session with no home directory keeps them in `/tmp/doom` instead,
which is cleared at reboot.

## Licences

The engine is GPL-2.0-or-later, the LazyOS layer GPL-3.0-or-later, and
Freedoom BSD-3-Clause; see `resources/NOTICE.txt` and
`resources/COPYING-freedoom.txt` in the install directory.
