# PictureBox

`PictureBox` shows a picture: PNG, JPEG, BMP or GIF. It fits the picture to the
box, zooms, rotates by quarter turns and lets the user drag a picture that is
larger than the box. It is the control behind the Picture Viewer app, and it is
all a picture viewer or an image preview needs. Default size 320 x 240.

The control never opens a file. A script reads the bytes (with `file_read_bytes`,
which applies the app's file sandbox) and hands them over with `load`:

```rhai
picture1.load(file_read_bytes(path));
```

```toml
[[node]]
kind = "PictureBox"
name = "picture1"
width = 920
height = 540
anchor = "fill"
```

## At a glance

| Property | Type | Default | Meaning |
|---|---|---|---|
| `background` | color | `"#ffffff"` | The color around the picture and under its transparent parts. |
| `fit` | bool | true | Best fit: shrink a picture larger than the box to fit it. |
| `stretch` | bool | false | With `fit`, also enlarge a picture smaller than the box. |
| `zoom` | float, 1.0 to 6400.0 | 100.0 | The zoom in percent. Setting it turns `fit` off. Reading it gives the zoom being shown. |
| `rotation` | int | 0 | Clockwise rotation in degrees: 0, 90, 180 or 270. |
| `image_width` | int, read-only | 0 | The picture's width in pixels, before rotation (0 when empty). |
| `image_height` | int, read-only | 0 | The picture's height in pixels, before rotation. |
| `has_image` | bool, read-only | false | Whether a picture is loaded. |
| `format` | text, read-only | empty | `"PNG"`, `"JPEG"`, `"BMP"` or `"GIF"`; empty when nothing is loaded. |

| Event | Handler | Arguments |
|---|---|---|
| `KeyDown` (default) | `fn <name>_key_down(key)` | `key`: the key's name (string) |
| `Wheel` | `fn <name>_wheel(delta, ctrl)` | `delta`: notches, positive away from the user (float); `ctrl`: whether Ctrl was held (bool) |
| `Click` | `fn <name>_click(x, y)` | the position of a left click that did not drag (floats) |
| `DoubleClick` | `fn <name>_double_click(x, y)` | the position of a left double-click (floats) |

| Method | Returns | Purpose |
|---|---|---|
| `load(data)` | nothing | Show the picture in `data` (a blob). An error if it is not a picture. |
| `clear()` | nothing | Remove the picture. |
| `best_fit()` | nothing | Turn `fit` on and center the picture. |
| `actual_size()` | nothing | Show the picture at 100%. |
| `zoom_in()` | nothing | Zoom in one step. |
| `zoom_out()` | nothing | Zoom out one step. |
| `rotate_cw()` | nothing | Turn the picture 90 degrees clockwise. |
| `rotate_ccw()` | nothing | Turn it 90 degrees anticlockwise. |
| `to_png()` | blob | The picture as shown (rotation included) encoded as PNG. |
| `focus()` | nothing | Give the picture box the keyboard focus. |

## Loading a picture

`load(data)` takes a blob, as `file_read_bytes(path)` returns. The format is
recognized from the file's first bytes, not its name:

| Format | Notes |
|---|---|
| PNG | Decoded by the toolkit's PNG decoder. |
| JPEG | Baseline and progressive. |
| BMP | 1, 4, 8, 16, 24 and 32 bits per pixel, uncompressed or with bit fields. Compressed BMPs are refused. |
| GIF | The first frame only; it does not animate. |

A file that is not one of these, is damaged, or has more than 32 million pixels
(about 8000 x 4000) is refused with a script error, and the picture on screen is
left as it was. The size is checked from the header before any memory is allocated,
so a small hostile file cannot ask for gigabytes. Wrap a load in `try` when the
file comes from the user:

```rhai
fn show_picture(path) {
    try {
        picture1.load(file_read_bytes(path));
        picture1.best_fit();
        status_label.text = `${picture1.image_width} x ${picture1.image_height} ${picture1.format}`;
    } catch (error) {
        picture1.clear();
        status_label.text = `Cannot show this picture: ${error}`;
    }
}
```

Loading a picture resets its rotation to 0 and its pan to the center, but keeps the
sizing mode: if the user had zoomed to 400%, the next picture is shown at 400%. Call
`best_fit()` after a `load` when you want every new picture to fit, as above.

The error text says what went wrong: `pic.load: load: not a PNG, JPEG, BMP or GIF
picture`, or that the picture is too large, or that a header is truncated.

## Fit, zoom and size

With `fit` on (the default) a picture larger than the box is shrunk to fit it, and a
smaller one is shown at its own size, centered. `stretch` also enlarges a smaller
one to fill the box. The picture is never distorted.

`zoom` is a percentage of the picture's own pixels, where 100 is one picture pixel
per screen pixel. Writing it turns `fit` off and shows the picture at that zoom;
reading it gives the zoom **being shown**, even while fitted, so a status line can
display it:

```rhai
fn update_status() {
    let zoom = picture1.zoom.round().to_int();
    status_label.text = `${zoom}%`;
}
```

`zoom` accepts 1 to 6400 (a number outside that range is an error). The methods
move between modes:

- `best_fit()` turns `fit` on and centers the picture.
- `actual_size()` shows it at 100%.
- `zoom_in()` and `zoom_out()` step through the classic viewer ladder, from the zoom
  currently shown (also while fitted): 1, 2, 3, 5, 8, 12, 16, 25, 33, 50, 66, 75,
  100, 150, 200, 300, 400, 600, 800, 1600, 3200 and 6400 percent.
- Setting `fit = true` also centers the picture.

## Rotation and saving

`rotation` is 0, 90, 180 or 270 degrees clockwise; `rotate_cw()` and `rotate_ccw()`
step it (270 plus a turn is 0). Rotating resets the pan. Any other value is an
error when a script writes it; a form file with one that is not a multiple of 90
loads as 0. Rotation changes only the view: the picture file is never touched. To
keep the turned picture, `to_png()` returns it as shown, rotation included, ready for
`file_write_bytes`:

```rhai
fn save_button_click() {
    try {
        file_write_bytes("turned.png", picture1.to_png());
        status_label.text = "Saved turned.png";
    } catch (error) {
        status_label.text = `Could not save: ${error}`;
    }
}
```

`to_png()` is an error when no picture is loaded. Where the script may write is the
app's file sandbox ([Standard library](stdlib.md)); on LazyOS that is the app's own
data folder.

## Panning

A picture larger than the box can be dragged with the left button; it never moves
past its own edges, and one that fits stays centered. There is no property for the
pan: it resets when you rotate, load, call `best_fit()` or set `fit`.

## Events

The box takes keyboard focus when the left button is pressed on it, and is a tab
stop; call `focus()` from `form_load` or after a toolbar button so that keys reach
it.

- `KeyDown(key)` gives the key's name, the same names a [`Canvas`](controls-canvas.md)
  uses (`"left"`, `"page_down"`, `"space"`, `"a"`, `"f11"`...). Unlike the canvas, a
  held key **repeats**, so holding an arrow keeps paging.
- `Wheel(delta, ctrl)` gives the notches of the vertical wheel (positive away from
  the user, so scrolling up) and whether Ctrl was held. The control does not zoom
  by itself: your script maps the wheel to the commands it wants. The classic viewer
  pages with the wheel and zooms with Ctrl and the wheel.
- `Click(x, y)` is a left press and release that did not turn into a drag (the
  pointer moved no more than a few pixels). `DoubleClick(x, y)` is a left
  double-click. Positions are logical pixels from the box's top-left corner.

```rhai
fn picture1_key_down(key) {
    switch key {
        "right" | "page_down" | "space" => next_picture(),
        "left" | "page_up" | "backspace" => previous_picture(),
        "k" => picture1.rotate_cw(),
        "l" => picture1.rotate_ccw(),
        "0" => picture1.best_fit(),
        "1" => picture1.actual_size(),
        _ => (),
    }
}

// The wheel pages through the folder; with Ctrl it zooms.
fn picture1_wheel(delta, ctrl) {
    if ctrl {
        if delta > 0 { picture1.zoom_in(); } else { picture1.zoom_out(); }
    } else if delta > 0 {
        previous_picture();
    } else {
        next_picture();
    }
}

// Switch between best fit and actual size.
fn picture1_double_click(x, y) {
    if picture1.fit { picture1.actual_size(); } else { picture1.best_fit(); }
}
```

## An example: open and show

A form with a `PictureBox` (`picture1`, anchored to `fill`), an **Open** button and a
status label:

```rhai
fn open_button_click() {
    open_file_dialog(
        "Open a picture",
        "Pictures|*.png,*.jpg,*.jpeg,*.bmp,*.gif;All files|*.*",
        |path| {
            if type_of(path) == "string" {
                show_picture(path);
            }
            picture1.focus();
        }
    );
}

fn show_picture(path) {
    try {
        picture1.load(file_read_bytes(path));
        picture1.best_fit();
        status_label.text = `${picture1.image_width} x ${picture1.image_height} ${picture1.format}`;
    } catch (error) {
        picture1.clear();
        status_label.text = `Cannot show this picture: ${error}`;
    }
}
```

The file dialog is asynchronous: the callback runs later with the chosen path, or
with `()` when the user cancels (hence the `type_of` check).

## The Picture Viewer

The Picture Viewer app (`os.lazy.pictures`) is this control and about two hundred
lines of script: paging through a folder, a toolbar, a slide show on a `Timer`, an
Edit button that hands the file to Paint. It is described in
[Picture Viewer](../lazyrad-pictures.md). It does not name its functions like the
box's methods (`best_fit`, `rotate_cw`): a script function with the same name and
arity as a method would be called for `picture1.best_fit()` too, and recurse.

## See also

- [Controls](controls.md): the overview and how scripts read and write properties.
- [Canvas](controls-canvas.md): drawing instead of showing a picture.
- [Display and layout](controls-display.md): `Timer`, for a slide show.
- [Standard library](stdlib.md): `file_read_bytes`, `file_write_bytes`, `open_file_dialog`.
- [Picture Viewer](../lazyrad-pictures.md): the sample app built on this control.
- [Forms](forms.md): anchors, so the box can fill the window.
