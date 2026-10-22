# Display and layout controls

The controls that show something or organize the form rather than take input:
`Label`, `ProgressBar`, `Separator`, the two containers `GroupBox` and `Panel`, and
the non-visual `Timer`. A script mostly writes to the first two, and a timer is
how a form does something on its own schedule. The toolbox has tiles for
`Label`, `GroupBox` and `Timer` only; `ProgressBar`, `Separator` and `Panel` are
added by writing a `[[node]]` in the `.lfm` ([the overview](controls.md)).

## Label

Static text. Default size 120 x 20. A label raises no events.

| Property | Type | Default | Meaning |
|---|---|---|---|
| `text` | text | empty | The text shown. |

```rhai
fn name_edit_change(text) {
    echo_label.text = if text == "" { "Type your name" } else { `Hello, ${text}!` };
}
```

A label is the usual place for a result or a status line. Put one at the bottom of
the form with `anchor = "bottom_left"` and write to it from your handlers.

## ProgressBar

A bar that shows how far something has got. It is read-only for the user and has no
events. Default size 160 x 12.

| Property | Type | Default | Meaning |
|---|---|---|---|
| `value` | int | 0 | The current position, from 0 to `max`. A value outside that range is clamped. |
| `max` | int, at least 1 | 100 | The upper bound of the range `0..max`. |

Both are integers; assigning a float is an error, so convert with `.to_int()`:

```rhai
fn copy_step(done, total) {
    progress.max = total;
    progress.value = done;
    percent_label.text = `${(done * 100.0 / total).round().to_int()}%`;
}
```

Lowering `max` below the current value pulls the value down to it.

## Separator

A thin divider line. Default size 120 x 8. A separator has no events and nothing a
script needs to change.

| Property | Type | Default | Meaning |
|---|---|---|---|
| `orientation` | enum | `"horizontal"` | `"horizontal"` or `"vertical"`: the direction the line runs. Design time only. |

For a vertical divider set `orientation = "vertical"` and give the control a narrow
width and a tall height (`width = 8`, `height = 120`); the size you give is the
size it is.

## GroupBox

A titled frame that holds other controls. Default size 200 x 120. Children name the
box with `parent = "<group name>"` in the `.lfm`, and the designer puts a control
you drop on the frame inside it.

| Property | Type | Default | Meaning |
|---|---|---|---|
| `text` | text | empty | The title drawn on the frame. |

A group box has no events. A script can change its title:

```rhai
fn show_mode(index) {
    mode_group.text = if index == 0 { "Basic options" } else { "Advanced options" };
}
```

The children's `left` and `top` are measured from the group's top-left corner, and
the frame's title uses the top 15 units or so, so start the first row of children
around 20. The group's `anchor` moves and resizes it with the window, and what it
holds follows by its own anchors ([Forms](forms.md)).

## Panel

A plain container with no properties of its own besides the common ones. Default
size 200 x 120. Use it to move or resize a set of controls together or to give them
one parent for anchoring. The toolbox has no `Panel` tile: add one by writing a
`[[node]]` in the `.lfm`, as below (and give its children `parent = "<panel name>"`
by hand). The designer then shows the panel and lets you select and edit it.

```toml
[[node]]
kind = "Panel"
name = "toolbar_panel"
left = 0
top = 0
width = 400
height = 40
anchor = "stretch_horizontal"

[[node]]
kind = "Button"
name = "open_button"
parent = "toolbar_panel"
left = 8
top = 6
text = "Open"
```

A panel has no events. Its `visible`, `left`, `top`, `width` and `height` can be
changed from a script like any control's.

## Timer

A non-visual control that raises `Tick` every `interval` milliseconds while it is
enabled. It has no appearance at run time: in the designer it is a small label with
its name so you can find and select it. Default size 72 x 20 (the size only matters
to the designer).

| Property | Type | Default | Meaning |
|---|---|---|---|
| `interval` | int, 1 to 86400000 | 100 | The time between ticks, in milliseconds. One day is the longest; a larger number in a script is an error. |
| `enabled` | bool | **false** | Whether the timer runs. It starts stopped. |
| `visible` | bool | true | Whether the designer shows the placeholder. Design time only. |

| Event | Handler | Arguments |
|---|---|---|
| `Tick` (default) | `fn <name>_tick()` | none |

A timer starts when `enabled` becomes true and stops when it becomes false. Changing
`interval` while it runs restarts it at the new period. Turn one on in `form_load`
or from a button, and turn it off from its own handler when the work is done:

```rhai
fn form_load() {
    form.state.seconds = 0;
    clock_timer.enabled = true;
}

fn clock_timer_tick() {
    form.state.seconds += 1;
    clock_label.text = `${form.state.seconds} s`;
    if form.state.seconds >= 60 {
        clock_timer.enabled = false;
    }
}
```

Notes:

- If a `Tick` handler raises an error, that timer is **disabled** and the error is
  shown once, so a broken handler does not report itself every interval. Set
  `enabled = true` again to restart it.
- A form can have any number of timers; each has its own `interval`.
- The Picture Viewer's slide show is a `Timer` with `interval = 4000` that a button
  enables and disables.
- For animation use a `Canvas` and its `fps` instead of a short-interval timer
  ([Canvas](controls-canvas.md)).

## See also

- [Controls](controls.md): the overview and the common properties.
- [Forms](forms.md): containers, `parent` and anchors in the `.lfm`.
- [Input controls](controls-input.md): controls the user types into or presses.
- [Canvas](controls-canvas.md): frame-driven drawing.
- [Scripting](scripting.md): `form.state` and handlers.
