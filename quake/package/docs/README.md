# Quake

Quake on the quake-srp engine, playing id Software's freely redistributable
shareware: the whole first episode (Dimension of the Doomed) up to the
personal teleporter, the weapons it grants (the Shotgun at least), the
monsters and the hex-based walls of the Slipgate Complex to the House of
Chthon.

## Controls

| Key | Action |
|---|---|
| Arrow keys | move and turn |
| Ctrl | fire |
| Space | jump, swim up |
| `[` and `]` | select a weapon |
| Shift (held) | always run while held |
| Alt (held) + Left/Right | sidestep |
| `,` and `.` | sidestep left and right |
| Esc | menu |
| `~` (tilde) | console |
| F1 / F3 | save / load |

Without the input service (`inputd`) the window only receives plain keys, so
arrows and typed keys still reach the game.

## What is not here

The engine is quake-srp's port of WinQuake's single-player game: no
multiplayer, no bots, no mission packs. The shareware episode is id's whole
first episode; the registered game's `pak1.pak` is not redistributable and
is not in the package.

## Files

Saves (`s1.sav`..`s6.sav` in the save menu) and `config.cfg` are kept in
`~/.apps/org.lazy.quake` (your home directory), so they survive reboots and
reinstalling the package. A session with no home directory keeps them in
`/tmp/quake` instead, which is cleared at reboot.

## Licences

The engine is GPL-2.0-or-later, the LazyOS layer GPL-3.0-or-later, and the
shareware pak id's own terms; see `resources/NOTICE.txt` and
`resources/SLICNSE.TXT` in the install directory.
