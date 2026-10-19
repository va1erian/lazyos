# NES

Play NES game ROMs, with sound.

The emulation is [TetaNES](https://github.com/lukexor/tetanes)'s
`tetanes-core` (MIT OR Apache-2.0): CPU, PPU, APU (all five channels) and 30+
mappers, so most licensed and homebrew cartridges run. The window, the mixer
integration and the files are LazyOS's own; see
[`docs/nes-player.md`](../../../../docs/nes-player.md).

## Playing

Open a `.nes` cartridge with **File > Open ROM...** (`Ctrl+O`), or drop one on
the window; the system also opens one from Files (`.nes` is registered with
`mimed`). The welcome screen offers the sample cartridge the image ships
(`/system/share/samples/lazyos-sample.nes`), an original homebrew drawn and
assembled by `tools/nes/`.

| Key | Console |
|---|---|
| Arrows | D-pad |
| `Z` / `X` | A / B |
| `A` / `S` | turbo A / B (pulses while held) |
| `Enter` | Start |
| `Tab` or `Q` | Select |
| `Space` (hold) | fast forward (2x) |

| Key | App |
|---|---|
| `Ctrl+O` | open a ROM |
| `Ctrl+S` / `Ctrl+L` | save / load state (slot in the Emulation menu) |
| `Ctrl+1`..`Ctrl+4` | choose the save-state slot |
| `Esc` | pause |
| `Ctrl+R` | reset |
| `Ctrl+M` | mute |
| `Ctrl+Q` | quit |

Video and Audio menus: scale (auto, 1x-3x), pixel-perfect (8:7) or square
pixels, the composite (NTSC) filter, the FPS counter, and mute. Settings are
remembered in `~/.apps/os.lazy.nes/config.txt`.

## Files

Battery-backed cartridges save to `~/.apps/os.lazy.nes/saves/<name>.sram` (also
written every 30 s while playing and on quit); save states live in
`~/.apps/os.lazy.nes/states/<name>/slot-<n>.state`. No ROM is shipped except
the sample.

NES is a trademark of Nintendo. This app is not affiliated with or endorsed by
Nintendo.
