# Controls reference

LazyRAD has nineteen kinds of control. Eleven have a tile in the designer's
toolbox (`Button`, `Edit`, `Label`, `CheckBox`, `RadioGroup`, `GroupBox`,
`ListView`, `ComboBox`, `Canvas`, `PictureBox` and `Timer`) and are drawn on a
form from it. The other eight (`ToggleButton`, `Hyperlink`, `MultilineEdit`,
`NumberField`, `Slider`, `ProgressBar`, `Separator` and `Panel`) have no tile:
you add one by writing a `[[node]]` in the [`.lfm` file](forms.md) by hand, and
the designer then shows it and edits its properties like any other. Either way
you set properties in the property grid (or in the `.lfm`), and
reach them from the form's script by name: `name_edit.text`, `list1.selected`,
`game_canvas.fill_rect(...)`. This page is the overview: the list of every kind,
the rules that apply to all of them, and how a script reads, writes and calls
them. The details of each kind are on the group pages linked below.

Control kinds and property names are the toolkit's own (`Button`, `Edit`, `text`,
`selected`, `checked`); LazyRAD adds no VB6 aliases. A `CommandButton` is a `Button`
and a `TextBox` is an `Edit`; see the table at the end.

## Every control at a glance

The size is what a new control gets from the designer (width x height, in design
units) and what a node without `width` or `height` in the `.lfm` is.

| Kind | What it is | Size | Default event | Page |
|---|---|---|---|---|
| `Button` | A push button with a caption and an optional icon. | 100 x 28 | `Click` | [Input](controls-input.md) |
| `ToggleButton` | A button that latches down. | 100 x 28 | `Toggle` | [Input](controls-input.md) |
| `CheckBox` | A labelled check box. | 120 x 24 | `Toggle` | [Input](controls-input.md) |
| `Hyperlink` | A clickable link label. | 120 x 20 | `Click` | [Input](controls-input.md) |
| `Edit` | A single-line text field. | 160 x 24 | `Change` | [Input](controls-input.md) |
| `MultilineEdit` | A multi-line text area. | 200 x 100 | `Change` | [Input](controls-input.md) |
| `NumberField` | A numeric field with steppers. | 120 x 28 | `Change` | [Input](controls-input.md) |
| `Slider` | A horizontal range control. | 160 x 24 | `Change` | [Input](controls-input.md) |
| `RadioGroup` | A vertical set of radio options. | 160 x 96 | `Select` | [Choices](controls-choices.md) |
| `ComboBox` | A drop-down list of choices. | 160 x 24 | `Select` | [Choices](controls-choices.md) |
| `ListView` | A list of rows with a selection. | 240 x 140 | `Select` | [Choices](controls-choices.md) |
| `Label` | Static text. | 120 x 20 | none | [Display and layout](controls-display.md) |
| `ProgressBar` | A read-only progress indicator. | 160 x 12 | none | [Display and layout](controls-display.md) |
| `Separator` | A divider line. | 120 x 8 | none | [Display and layout](controls-display.md) |
| `GroupBox` | A titled frame that holds other controls. | 200 x 120 | none | [Display and layout](controls-display.md) |
| `Panel` | A plain container. | 200 x 120 | none | [Display and layout](controls-display.md) |
| `Timer` | A non-visual timer that raises `Tick`. | 72 x 20 | `Tick` | [Display and layout](controls-display.md) |
| `Canvas` | A drawing surface with a frame loop and keyboard and mouse input. | 320 x 240 | `Frame` | [Canvas](controls-canvas.md) |
| `PictureBox` | Shows a picture with fit, zoom, rotation and panning. | 320 x 240 | `KeyDown` | [PictureBox](controls-picturebox.md) |

The default event is the one the designer opens when you double-click a control:
the code window jumps to `fn <name>_<event>()` and creates it if it is missing.
A control with no default event has no events at all. `GroupBox` and `Panel` are
the only containers; the window's own events are in [Forms](forms.md).

## Properties every control has

These eight are on every kind and are described in full in [Forms](forms.md):

| Property | Type | Default | Script notes |
|---|---|---|---|
| `left`, `top` | int | 0 | Design units from the parent's corner. Writing one moves the control. |
| `width`, `height` | int | the kind's size | Design units. Writing one resizes it. |
| `anchor` | enum | `"top_left"` | One of the twelve names in [Forms](forms.md). |
| `visible` | bool | true | Hides or shows the control. |
| `enabled` | bool | true | A disabled control is dimmed and ignores input. A `Timer` starts disabled. |
| `tab_index` | int | 0 | Stored, but does not order keyboard focus in this build. |

`left`, `top`, `width` and `height` read the design rectangle, not the size on
screen after the user resizes the window. `visible` on a `Timer` is design time
only.

## How a script uses a control

Every control is a variable named after it. Properties are read and written with a
dot, methods are called with a dot, and events call the handler
`fn <name>_<event>`:

```rhai
fn save_button_click() {
    status_label.text = `Saved ${name_edit.text}`;   // read one control, write another
    save_button.enabled = false;
    name_edit.focus();                                // see the page of the control for methods
}
```

A property name can also be a string, so a function can take a control and a
property as parameters:

```rhai
fn set_prop(control, name, value) {
    control[name] = value;
}

fn clear_form() {
    set_prop(first_edit, "text", "");
    set_prop(second_edit, "text", "");
    for box in [first_edit, second_edit] {
        box.enabled = true;
    }
}
```

### Values and their types

A property has a type, and what a script may write depends on it. Reading always
gives the natural Rhai value.

| Type | You read | You may write |
|---|---|---|
| bool | a bool | a bool |
| int | an integer | an integer. A float is an error: use `.to_int()`. |
| float | a float | a float or an integer |
| text | a string | a string. A number is an error: use `${n}` or `.to_string()`. |
| enum | a string | one of the allowed strings |
| color | `"#rrggbb"` | `"#rrggbb"`, `"rrggbb"` or `"#rrggbbaa"` (the alpha is dropped), or an integer `0xRRGGBB` such as `rgb(r, g, b)` returns |
| list | an array of strings | an array whose items are all strings |
| bytes | a blob | a blob |

A value outside the property's range is refused with the same kind of error (a
`Canvas`'s `fps` above 240, a `PictureBox`'s `zoom` below 1). Some controls clamp
a value into their own range instead: the `value` of a `Slider`, a `NumberField`
and a `ProgressBar` never leaves `min..max` (`0..max` for the bar).

A script error names the control and the property:

```text
unknown property 'Fps' on cv (Canvas); did you mean 'fps'?
unknown property 'nope' on s (Slider); properties: value, min, max, left, top, ...
cannot set `mouse_x` on `cv`: the property is read-only here
cannot set `selected` on `c`: the value has the wrong type
expected text, found i64
```

"The value has the wrong type" is also what an out-of-range number says. An error
in a handler stops that handler and opens a message box with the located error;
the rest of the form keeps running.

### Changing a value does not raise its event

When a script sets `text`, `checked`, `value` or `selected`, the control's own
`Change`, `Toggle` or `Select` event is **not** raised: events are for what the user
does. A handler that sets the control it handles cannot loop, and a script that
wants the effect of the event calls its own function.

### Methods

Most controls have no methods; you work with their properties. The `Canvas` and
the `PictureBox` have methods, and both have `focus()`. A method is called with a
dot and positional arguments:

```rhai
game_canvas.fill_rect(10, 10, 80, 20, "#ff8800");
let w = game_canvas.text_width("Score", 16);
```

Method arguments are more forgiving than property writes: a float is truncated
for an integer parameter and an integer is widened for a float parameter, because
games compute with floats. A wrong number of arguments is an error that gives the
real signature, `cv.fill_rect takes 5 arguments (x, y, w, h, color), got 3`, and a
method the control does not have lists the ones it does.

### Events and their arguments

An event reaches the handler with its values in order. The tables on the group
pages give each event's arguments; the handler's parameter names are your choice.
No event is shared by all controls: each kind has its own set, and the window has
`Load` and `Close` ([Forms](forms.md)). The full set:

| Kind | Event | Arguments |
|---|---|---|
| `Button` | `Click` | none |
| `ToggleButton`, `CheckBox` | `Toggle` | `checked` (bool) |
| `Hyperlink` | `Click` | none |
| `Edit`, `MultilineEdit` | `Change` | `text` (string) |
| `NumberField`, `Slider` | `Change`, `Commit` | `value` (float) |
| `RadioGroup`, `ComboBox` | `Select` | `index` (int) |
| `ListView` | `Select`, `Activate` | `index` (int) |
| `Timer` | `Tick` | none |
| `Canvas` | `Frame` | `dt` (float, seconds) |
| `Canvas` | `KeyDown`, `KeyUp` | `key` (string) |
| `Canvas` | `MouseDown`, `MouseUp` | `x`, `y` (float), `button` (string) |
| `Canvas` | `MouseMove` | `x`, `y` (float) |
| `PictureBox` | `KeyDown` | `key` (string) |
| `PictureBox` | `Wheel` | `delta` (float), `ctrl` (bool) |
| `PictureBox` | `Click`, `DoubleClick` | `x`, `y` (float) |

Scope, shared variables, `form.state`, modules and the rest of the scripting model
are in [Scripting](scripting.md).

## If you know VB6

LazyRAD takes VB6's workflow (draw, double-click, write the handler, press F5), not
its names. The closest control for each VB6 one:

| VB6 | LazyRAD | Notes |
|---|---|---|
| `CommandButton` | `Button` | `Caption` is `text`. |
| `Label` | `Label` | `Caption` is `text`. |
| `TextBox` | `Edit`, `MultilineEdit` | One line or many; `Text` is `text`. |
| `CheckBox` | `CheckBox` | `Value` is `checked`, a bool. |
| `OptionButton` | `RadioGroup` | One control holds all the options; `selected` is the index. |
| `ListBox` | `ListView` | `ListIndex` is `selected`, -1 for none. |
| `ComboBox` | `ComboBox` | `ListIndex` is `selected`. |
| `Frame` | `GroupBox` | Holds children with `parent`. |
| `PictureBox` | `PictureBox`, `Canvas` | `PictureBox` shows a picture; `Canvas` draws. |
| `Timer` | `Timer` | `Interval` is `interval`, `Enabled` is `enabled`. |

## See also

- [Input controls](controls-input.md): `Button`, `ToggleButton`, `CheckBox`, `Hyperlink`, `Edit`, `MultilineEdit`, `NumberField`, `Slider`.
- [Choices and lists](controls-choices.md): `RadioGroup`, `ComboBox`, `ListView`.
- [Display and layout](controls-display.md): `Label`, `ProgressBar`, `Separator`, `GroupBox`, `Panel`, `Timer`.
- [Canvas](controls-canvas.md): drawing, the frame loop, keyboard and mouse.
- [PictureBox](controls-picturebox.md): pictures, zoom and rotation.
- [Forms](forms.md): the `.lfm` format, anchors and containers.
- [Scripting](scripting.md): handlers, scope and `form`.
