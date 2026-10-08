# Picture Viewer

Look at pictures the way Windows XP's Picture and Fax Viewer did: one at a
time, fitted to the window, with a toolbar to page through the folder, zoom,
turn them and run a slide show. It is a LazyRAD app: a form and a Rhai
script on the `PictureBox` control, running on the LazyRAD player.

## Opening pictures

* Double-click a `.png`, `.jpg`, `.bmp` or `.gif` in Files: `mimed` hands it
  to the viewer, which may then read the rest of that folder.
* **Open...** (or `O`) picks a picture anywhere; its folder comes with it.
* Started from the menu, it shows the sample pictures (the desktop
  wallpapers).

A damaged picture, or one larger than 32 megapixels, is named in the status
line instead of shown.

## Keys and mouse

| Keys | Does |
|---|---|
| `Right`, `Down`, `Page Down`, `Space`, wheel down, **Next** | Next picture (wraps around) |
| `Left`, `Up`, `Page Up`, `Backspace`, wheel up, **Previous** | Previous picture |
| `Home` / `End` | First / last picture |
| `0` or `B`, **Best Fit** | Fit the picture to the window |
| `1` or `A`, **Actual Size** | One picture pixel per screen pixel |
| `Ctrl`+wheel, **Zoom In** / **Zoom Out** | Zoom by a step (1% to 6400%) |
| Double-click | Switch between best fit and actual size |
| Drag | Move a picture larger than the window |
| `K` / `L`, **Rotate Right** / **Rotate Left** | Turn the picture a quarter turn |
| `F11`, **Slide Show** | A new picture every four seconds; `Escape` stops |
| `E`, **Edit** | Open the picture in its editor (Paint, for a PNG) |

Turning a picture changes only how it is shown; the file is never written.
