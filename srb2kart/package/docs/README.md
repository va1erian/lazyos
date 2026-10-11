# SRB2Kart

SRB2Kart v1.6, the kart racer by **Kart Krew** built on the Sonic Robo Blast 2
engine (Sonic Team Junior), running offline on the software renderer: Grand
Prix, Match Race and Time Attack against bots.

## Controls

These are SRB2Kart v1.6's default controls for player 1; the game's Setup
menu (Player 1 Controls) lists and changes them.

| Key | Action |
|---|---|
| Left / Right arrows | steer (and move through menus) |
| A | accelerate |
| S | drift |
| D | brake / reverse |
| Space | use item |
| Left Shift | look back |
| Tab | scoreboard |
| Pause | pause |
| ` (backquote) | console |
| Enter | confirm in menus |
| Esc | menu / back |

Sound and music are not in this version.

## Game data and credits

This package holds the engine only. The game data (`srb2.srb`, `*.kart`) is
(c) Kart Krew and was downloaded unmodified from Kart Krew's own v1.6 release by
the build of the LazyOS image you are running; it lives in
`/system/share/srb2kart` beside `CREDITS.txt`, and LazyOS does not
redistribute it. Sonic the Hedgehog and related characters are trademarks of
SEGA. SRB2Kart is a fan game, not affiliated with or endorsed by SEGA or Sonic
Team.

## Files

Settings (`kartconfig.cfg`), progress (`kartdata.dat`) and replays are kept in
`~/.apps/org.lazy.srb2kart`. A session with no home keeps them in
`/tmp/srb2kart`, which is cleared at reboot.

## Licences

The engine is GPL-2.0-or-later (Kart Krew, Sonic Team Junior, the Doom Legacy
team, id Software), the LazyOS layer GPL-3.0-or-later; see `NOTICE` in the
install directory. The game data is not covered by either.
