# Canvas

`Canvas` is a surface your script draws on. It has a drawing API (rectangles,
circles, lines, text), a **frame loop** that calls your handler a given number of
times a second, and raw **keyboard and mouse** input. It is LazyRAD's basic game
graphics control: the Brick Breaker sample is a complete game on one canvas and a
script of about three hundred lines. Default size 320 x 240.

```toml
[[node]]
kind = "Canvas"
name = "stage"
width = 400
height = 300
anchor = "fill"
background = "#101018"
fps = 60
```

## At a glance

| Property | Type | Default | Meaning |
|---|---|---|---|
| `background` | color | `"#000000"` | The color painted under the drawing. |
| `fps` | int, 0 to 240 | 0 | Frames per second of the `Frame` event. 0 stops the frame loop. |
| `mouse_x` | float, read-only | 0.0 | The pointer's last x position over the canvas, in logical pixels. |
| `mouse_y` | float, read-only | 0.0 | The pointer's last y position over the canvas. |

| Event | Handler | Arguments |
|---|---|---|
| `Frame` (default) | `fn <name>_frame(dt)` | `dt`: seconds since the previous frame (float) |
| `KeyDown` | `fn <name>_key_down(key)` | `key`: the key's name (string) |
| `KeyUp` | `fn <name>_key_up(key)` | `key`: the key's name (string) |
| `MouseDown` | `fn <name>_mouse_down(x, y, button)` | pointer position (floats) and `"left"`, `"right"` or `"middle"` |
| `MouseUp` | `fn <name>_mouse_up(x, y, button)` | the same |
| `MouseMove` | `fn <name>_mouse_move(x, y)` | pointer position (floats) |

| Method | Returns | Purpose |
|---|---|---|
| `clear(color)` | nothing | Empty the drawing and fill the canvas. |
| `fill_rect(x, y, w, h, color)` | nothing | Fill a rectangle. |
| `stroke_rect(x, y, w, h, color, width)` | nothing | Outline a rectangle. |
| `fill_round_rect(x, y, w, h, radius, color)` | nothing | Fill a rectangle with rounded corners. |
| `fill_circle(cx, cy, r, color)` | nothing | Fill a circle. |
| `stroke_circle(cx, cy, r, color, width)` | nothing | Outline a circle. |
| `line(x1, y1, x2, y2, color, width)` | nothing | Draw a straight line. |
| `text(x, y, text, color, size)` | nothing | Draw text. |
| `text_width(text, size)` | float | Measure text. |
| `is_key_down(key)` | bool | Whether a key is held. |
| `focus()` | nothing | Give the canvas the keyboard focus. |

The rest of this page explains each part.

## Coordinates and colors

All positions and sizes are **logical pixels** (the same design units as the rest
of the form) measured from the canvas's top-left corner, as floats; integers are
accepted. They are scaled by the display scale when painted, so a game looks the
same on a normal and a HiDPI screen. Anything drawn outside the canvas is clipped.
A negative width or height makes a rectangle extend left or up from its origin.

Colors are passed in either of two forms:

- a string `"#rrggbb"` (`"#ff8800"`; the short `"#f80"` is not accepted), or
- an integer `0xRRGGBB`, which is what the standard library's `rgb(r, g, b)`
  returns (each channel 0 to 255, clamped), so a color can be computed:

```rhai
fn glow(level) {
    let c = rgb(80 + level, 40 + level / 2, 20);
    game_canvas.fill_circle(100, 100, 30, c);
}
```

A line width or radius must not be negative, and non-finite numbers (`NaN`) are
refused. Each argument error names the method and the argument:
`cv.fill_rect: argument 5 (`color`): `red` is not a `#rrggbb` colour`.

## Drawing

The canvas keeps a **display list**. Every drawing method appends one command and
asks for a repaint; it does not paint immediately. When the canvas paints it fills
`background` and replays the list in order, so later commands draw over earlier
ones.

`clear(color)` empties the list and starts it with a full fill. A game redraws its
whole scene every frame, beginning with `clear`:

```rhai
fn game_canvas_frame(dt) {
    game_canvas.clear("#101018");
    game_canvas.fill_round_rect(paddle_x, 480, 96, 14, 7, "#ecf0f1");
    game_canvas.fill_circle(ball_x, ball_y, 7, "#ffe066");
    game_canvas.text(12, 7, `Score: ${score}`, "#ffffff", 16);
}
```

A canvas does not need a frame loop to draw: commands persist until the next
`clear`, so a script can also add shapes in response to the mouse and leave them
there (a paint program). The list holds at most **100,000** commands; one more is an
error that says to call `clear()` at the start of each frame.

| Method | Arguments | Notes |
|---|---|---|
| `clear` | `color` | Empties the display list and fills the canvas with `color`. |
| `fill_rect` | `x, y, w, h, color` | Fills a rectangle whose corner is `(x, y)`. |
| `stroke_rect` | `x, y, w, h, color, width` | Outlines a rectangle with a line `width` pixels wide. |
| `fill_round_rect` | `x, y, w, h, radius, color` | Note the order: `radius` comes before `color`. |
| `fill_circle` | `cx, cy, r, color` | A circle centered on `(cx, cy)` with radius `r`. |
| `stroke_circle` | `cx, cy, r, color, width` | Outlines a circle. |
| `line` | `x1, y1, x2, y2, color, width` | A straight line `width` pixels wide. |
| `text` | `x, y, text, color, size` | Draws `text` starting at `(x, y)` with its top-left corner there; `size` is the text height in pixels, above 0 and at most 1000. |
| `text_width` | `text, size` | Returns the width the text takes at that size, in logical pixels. Use it to center or right-align. |

Text uses the system font; there is no font selection.

```rhai
// Draw `message` centered across the canvas, `y` pixels from the top.
fn centered(message, y, size, color) {
    let w = game_canvas.text_width(message, size);
    game_canvas.text((game_canvas.width - w) / 2.0, y, message, color, size);
}
```

## The frame loop

Setting `fps` above 0 starts a timer that raises `Frame(dt)` about `fps` times a
second; `dt` is the time since the previous frame in **seconds**, a float, so
movement is `x += speed * dt` and runs at the same speed whatever the frame rate.
The first frame reports about `1 / fps`, and `dt` is never more
than 0.1, so a pause (a breakpoint, a window being dragged) does not make a game
jump.

- Set `fps` in the designer or from a script. `fps = 0` stops the loop and a new
  value restarts it. The highest is 240; a handler that takes longer than a frame
  just gets fewer frames.
- The loop only starts if the form's script defines `fn <name>_frame`, since
  otherwise there would be nobody to tell. A design-mode canvas never runs one.
- If the frame handler raises an error the loop **stops** (the canvas's `fps`
  becomes 0) and the error is reported once, instead of 60 times a second. Fix the
  script and set `fps` again, or reload.
- Keep per-game state in the script's top-level variables, which every handler
  shares (the next section's example, and the Brick Breaker sample, do).

## Keyboard

The canvas receives keys while it has the keyboard focus. Pressing a mouse button
on it gives it the focus, and a script can call `focus()`, typically from
`form_load`:

```rhai
fn form_load() {
    game_canvas.focus();
}
```

If the user clicks a button on the form the canvas loses the focus, and the keys
that were held are forgotten (so none gets stuck down). Call `game_canvas.focus()`
again at the end of that button's handler.

`KeyDown` and `KeyUp` give a key's name, always lower-case ASCII:

| Keys | Names |
|---|---|
| Letters | `"a"` to `"z"` (whatever the shift state) |
| Digits | `"0"` to `"9"` |
| Arrows | `"left"`, `"right"`, `"up"`, `"down"` |
| Editing | `"space"`, `"enter"`, `"escape"`, `"tab"`, `"backspace"`, `"delete"`, `"insert"` |
| Moving | `"home"`, `"end"`, `"page_up"`, `"page_down"` |
| Modifiers | `"shift"`, `"control"`, `"alt"` |
| Function keys | `"f1"` to `"f12"` |

Any other key raises no event. `KeyDown` is raised once per press: the keyboard's
auto-repeat does not raise it again. To ask whether a key is held right now, call
`is_key_down("left")` (the name is case-insensitive), which stays true until the key
is released. That is the right tool for movement; use `KeyDown` for one-off actions.

```rhai
fn move_paddle(dt) {
    let direction = 0.0;
    if game_canvas.is_key_down("left") || game_canvas.is_key_down("a") { direction -= 1.0; }
    if game_canvas.is_key_down("right") || game_canvas.is_key_down("d") { direction += 1.0; }
    paddle_x += direction * 480.0 * dt;
}

fn game_canvas_key_down(key) {
    if key == "p" { toggle_pause(); }
}
```

Characters typed as text are not delivered to a canvas; use an `Edit` for that.
Modifier combinations are checked with `is_key_down("control")`.

## Mouse

`MouseDown`, `MouseUp` and `MouseMove` give the pointer position in logical pixels
relative to the canvas, as floats. The two button events add the button: `"left"`,
`"right"` or `"middle"` (other buttons raise nothing). The last position is also in
the read-only `mouse_x` and `mouse_y`, handy when a frame handler wants the pointer
without a `MouseMove` handler.

- Pressing a button **captures** the pointer: until the button is released,
  `MouseMove` and `MouseUp` keep arriving, even when the pointer has left the
  canvas, so a drag never loses the end. The positions can then be negative or
  larger than the canvas.
- A second click of a double-click raises `MouseDown` again.
- The canvas has no wheel event.

```rhai
fn game_canvas_mouse_move(x, y) {
    paddle_x = clamp(x - 48.0, 0.0, game_canvas.width - 96.0);
}

fn game_canvas_mouse_down(x, y, button) {
    if button == "left" {
        launch_ball();
    }
}
```

## An example

A ball that falls and bounces. The left and right arrow keys push it sideways,
Space pauses, and a click drops it where you click. The form is the canvas from the
top of this page.

```rhai
let x = 200.0;
let y = 60.0;
let vx = 140.0;
let vy = 0.0;
let paused = false;
const RADIUS = 12.0;
const GRAVITY = 600.0;

fn form_load() {
    stage.focus();                    // keys reach the canvas only when it has the focus
}

fn stage_frame(dt) {
    if !paused {
        step(dt);
    }
    draw();
}

fn step(dt) {
    let w = stage.width.to_float();
    let h = stage.height.to_float();
    vy += GRAVITY * dt;
    x += vx * dt;
    y += vy * dt;
    if x < RADIUS { x = RADIUS; vx = abs(vx); }
    if x > w - RADIUS { x = w - RADIUS; vx = -abs(vx); }
    if y > h - RADIUS { y = h - RADIUS; vy = -abs(vy) * 0.9; }
}

fn draw() {
    stage.clear("#101018");
    stage.fill_circle(x, y, RADIUS, "#ffe066");
    stage.text(8, 8, "Space pauses, click drops the ball", "#9aa0b4", 13);
}

fn stage_key_down(key) {
    if key == "space" { paused = !paused; }
    if key == "left" { vx -= 40.0; }
    if key == "right" { vx += 40.0; }
}

fn stage_mouse_down(px, py, button) {
    if button == "left" {
        x = px;
        y = py;
        vy = 0.0;
    }
}
```

The complete Brick Breaker game (paddle, bricks, lives, a title and game-over
screen, the arrow keys or the mouse) is the `brickbreaker` sample; its canvas is
exactly the node above with `anchor = "fill"` and `fps = 60`.

## Things to know

- **`width` and `height` are design values.** `stage.width` is the size the canvas
  was designed at, not its size after the user resizes the window, so a game that
  must adapt reads it once or keeps its own numbers. The canvas coordinates always
  start at its top-left corner.
- **Do not name your own functions like a canvas method** with the same number of
  arguments (a `fn clear(color)`, a `fn focus()`, a `fn text(a, b, c, d, e)`): Rhai
  also resolves `stage.clear(...)` to a script function of that name, and it calls
  itself. `draw`, `step` and `draw_bricks` are fine.
- **Redraw with `clear`.** A frame handler that only adds shapes grows the list
  until it reaches the 100,000-command limit.
- **Keep state in top-level variables.** They are shared by every handler and survive
  from one frame to the next; locals do not.
- **Canvas and design mode.** In the designer the canvas shows a placeholder with its
  name and receives no input; nothing in your script runs.
- A canvas is a tab stop, so the Tab key can move the focus to and from it.

## See also

- [Controls](controls.md): the overview and how scripts read and write properties.
- [Display and layout](controls-display.md): `Timer`, for work that is not drawing.
- [PictureBox](controls-picturebox.md): showing a picture rather than drawing one.
- [Forms](forms.md): anchors, so a canvas can fill the window.
- [Scripting](scripting.md): top-level variables, handlers, modules.
- [Standard library](stdlib.md): `rgb`, `clamp`, `rects_overlap`, `random`.
