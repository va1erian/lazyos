# Forms: the `.lfm` file

A LazyRAD form is a plain TOML file with the extension `.lfm`: one `[window]`
table and one `[[node]]` table per control. The designer reads and writes it,
the player builds live widgets from it, and you can edit it by hand. This page
describes the whole format: the window, the properties every control has, how
values are written, containers, anchors, naming, validation and how events are
named. What each kind of control adds is in the [controls reference](controls.md).

A form is half of a pair: the layout lives in `<name>.lfm` and the code in
`<name>.rhai`, listed together in the project's `.lrp` file
([Projects](projects.md)). The layout says what is on the form; the script says
what happens ([Scripting](scripting.md)).

## A first look

```toml
format = 1

[window]
name = "main_form"
title = "Hello"

[[node]]
kind = "Button"
name = "hello_button"
left = 16
top = 16
width = 120
height = 32
text = "Say hello"
```

| Part | What it is |
|---|---|
| `format = 1` | The file format version. A newer number is refused. |
| `[window]` | The form itself: its name and its window properties. |
| `[[node]]` | One control. `kind` and `name` are required; everything else is a property. |

The nodes are a flat list. A control inside a container says so with
`parent = "<container name>"`; nothing is nested in the TOML itself, which keeps
the file easy to diff and merge.

## The window

`[window]` needs a `name` (the form's name) and may set these properties:

| Property | Type | Default | Meaning |
|---|---|---|---|
| `name` | text | required | The form's name: an identifier (see Names below). |
| `title` | text | empty | The title in the window's title bar. When empty, the player uses the form's name. |
| `width` | int, at least 1 | 320 | The width of the client area, in design units. |
| `height` | int, at least 1 | 200 | The height of the client area, in design units. |
| `resizable` | bool | true | Stored with the form. The runtime in this build builds the window from `title`, `width` and `height` only and does not read it. |

The window raises four events (a handler is `fn form_<event>`):

| Event | Handler | When |
|---|---|---|
| `Load` | `form_load()` | Once, after the form is built. This is where you set up state and give a canvas the focus. |
| `Close` | `form_close()` | When the window is asked to close. The window then closes; the handler cannot cancel it. |
| `Resize` | `form_resize(width, height)` | Declared in the schema; the runtime in this build does not raise it. |
| `Activate` | `form_activate()` | Declared in the schema; the runtime in this build does not raise it. |

`form_load` and `form_close` are the two that run today. A script may also define
`fn form_reload(old_state)`, which the player calls after a hot reload
([Scripting](scripting.md)).

## Nodes and the common properties

Every `[[node]]` has three structural keys and then properties:

| Key | Meaning |
|---|---|
| `kind` | The control kind, exactly as in the [controls reference](controls.md): `Button`, `Edit`, `ListView`... Case matters. There are no aliases. |
| `name` | The control's name, unique in the form. It is also the variable a script uses to reach the control. |
| `parent` | Optional. The name of the container this control sits in. Absent means the window. |

Eight properties exist on every control, whatever its kind:

| Property | Type | Default | Meaning |
|---|---|---|---|
| `left` | int | 0 | Distance from the parent's left edge, in design units. May be negative. |
| `top` | int | 0 | Distance from the parent's top edge. |
| `width` | int | the kind's default width | Width in design units. |
| `height` | int | the kind's default height | Height in design units. |
| `anchor` | enum | `top_left` | How the control follows its parent when the parent is resized (below). |
| `visible` | bool | true | Whether the control is shown. |
| `enabled` | bool | true | Whether it accepts input. A `Timer` starts disabled instead. |
| `tab_index` | int, at least 0 | 0 | The control's place in the tab order among its siblings (below). |

`width` and `height` default to the size the toolbox gives that kind (a `Button`
is 100 x 28, a `Panel` 200 x 120); the table is in
[the controls overview](controls.md). A control that
leaves them out is that size, and a file that spells out the default drops it on
the next save.

Design units are device-independent pixels: one unit is one pixel at the
standard 96 dpi and scales with the display scale, so a form looks the same on a
normal and a HiDPI screen. Coordinates are measured from the parent's top-left
corner, so moving a container moves everything in it.

Properties specific to a kind (`text`, `items`, `value`, ...) sit in the same
table. [Controls](controls.md) lists them for every kind. A key the kind does not
have is a load error.

## Value types

Every property has a type from the schema, and the same type decides how the
value is written in the file, how a script reads and writes it, and what is
refused.

| Type | In the file | Notes |
|---|---|---|
| bool | `true` or `false` | |
| int | `16`, `-4` | An optional range: `tab_index` is at least 0, a `Timer`'s `interval` is 1 to 86400000. A value outside the range is a load error. |
| float | `75.0` or `75` | A whole number is accepted where a float is expected. The writer always writes a decimal point. |
| text | `"Say hello"` | A TOML string. `text` of a `MultilineEdit` may span lines. |
| enum | `"top_left"` | One of a fixed set of strings (an `anchor`, a `Separator`'s `orientation`). Anything else fails validation. |
| color | `"#101018"` | `#rrggbb`. The reader also accepts `rrggbb` and `#rrggbbaa` (the alpha is ignored); the writer emits lower-case `#rrggbb`. The short `#rgb` form and names like `"red"` are not colors. |
| list | `["Hello", "Hi", "Hey"]` | A list of strings: the rows of a `ListView`, the options of a `ComboBox`. |
| bytes | `[0, 255]` | A list of 0..255 integers. Only method arguments use it (a `PictureBox` loads a blob); no control stores one in a form. |

There are no font, picture, menu or border properties in the catalog: text uses
the system font, pictures are loaded by a script into a `PictureBox`
([PictureBox](controls-picturebox.md)), and the catalog has no menu.

## Containers and nesting

Only `Panel` and `GroupBox` can hold other controls. Any kind may go inside them,
including another container. Everything else rejects children.

```toml
[[node]]
kind = "GroupBox"
name = "levels_group"
left = 12
top = 12
width = 356
height = 112
text = "Levels"

[[node]]
kind = "Label"
name = "volume_label"
parent = "levels_group"
left = 12
top = 34
text = "Volume"
```

- A child's `left` and `top` are measured from its container's top-left corner.
  A `GroupBox` draws its frame a few units below its top edge and its title
  across it, so leave about 20 units at the top of the group.
- A child may stick out of its container; it is clipped, and it stays where you
  put it.
- The order of the nodes is the z-order: a later node paints over an earlier one.
  The designer writes a container before its children, and you should too, but
  the builder creates every container before the controls inside it whatever the
  file order.
- The designer drops a new control into the container under the pointer.
  Deleting a container deletes everything in it.

## Anchors and resizing

A form is laid out in absolute positions: each control sits at its `left`, `top`,
`width` and `height`. When the window is resized, `anchor` decides what happens to
each control. The change of size of the parent, from the size the form was
designed at (the window's `width` and `height`, or the container's), is
`dx` across and `dy` down:

| `anchor` | Effect |
|---|---|
| `top_left` | Stays where it is. The default. |
| `top` | Moves `dx / 2` to the right (centered horizontally, pinned to the top). |
| `top_right` | Moves `dx` to the right. |
| `left` | Moves `dy / 2` down (centered vertically, pinned to the left). |
| `center` | Moves `dx / 2` right and `dy / 2` down. |
| `right` | Moves `dx` right and `dy / 2` down. |
| `bottom_left` | Moves `dy` down. |
| `bottom` | Moves `dx / 2` right and `dy` down. |
| `bottom_right` | Moves `dx` right and `dy` down. |
| `stretch_horizontal` | Keeps its left and top; the width grows by `dx`. |
| `stretch_vertical` | Keeps its left and top; the height grows by `dy`. |
| `fill` | Keeps its left and top; the width grows by `dx` and the height by `dy`. |

The result is kept at least one pixel wide and high, and inside the parent, so a
control is never inverted or pushed away when the window shrinks below it. A
container resizes by its own anchor, and what it holds then follows it by theirs.

Typical choices: an edit that should widen with the window is
`stretch_horizontal`; a drawing surface or a list that should take the room is
`fill`; an OK button in the bottom-right corner is `bottom_right`; a status line
along the bottom is `bottom_left`.

The `left`, `top`, `width` and `height` a script reads are the design values, not
the size on screen. They do not change when the user resizes the window. Write one
and the control is placed again from the new design value.

## Tab order

`tab_index` numbers the controls of one parent in the order the Tab key should
visit them. The designer gives a new control the next free number among its
siblings. In this build the number is stored, validated and readable from a
script, but the runtime does not reorder keyboard focus by it. Two siblings that
both write the same `tab_index` are reported by the project check, so keep them
unique.

## Names

A name is an ASCII letter or `_`, followed by letters, digits and `_`. Names are
case-sensitive and unique across all the controls of the form; the form's own name
follows the same rule. The convention is snake_case.

- The designer names a new control after its kind plus a number: `button1`,
  `edit1`, `check_box1`, `multiline_edit1`, `number_field1`, `progress_bar1`,
  `combo_box1`, `list_view1`, `group_box1`, `picture_box1`... A new `Button`,
  `Label`, `CheckBox` or `GroupBox` also starts with its name as its text.
- A name is a variable in the script. The script's own top-level variables win
  over a control of the same name, so do not reuse one. A control named `form`
  would hide the form object, and a name that is a Rhai keyword or reserved word
  (`go`, `new`, `use`, `match`, `case`, `module`, `package`, `public`, `default`,
  `is`, `in`, `as`, `do`, `for`, `print`, `debug`, `type_of`, ...) fails to
  compile where the script uses it.
- Handlers are named after the control (`save_button_click`), so a name that says
  what the control is for makes the code read well. Renaming a control in the
  property grid rewrites its `fn <old>_<event>(` definitions in the script to the
  new name.

## Events and handlers

Events are bound by name, not by anything in the `.lfm`. For each event a control
raises, LazyRAD looks in the form's script for

```text
fn <control>_<event>(arguments)
```

with the event name in snake_case: `Click` is `click`, `KeyDown` is `key_down`,
`DoubleClick` is `double_click`. So the `Click` of `hello_button` is
`hello_button_click`, the `Change` of `name_edit` is `name_edit_change`, and the
window's `Load` is `form_load`. If the function exists the event is wired;
otherwise it does nothing.

The arguments are positional and the names are yours. A handler may declare fewer
parameters than the event carries (the extra values are dropped) or more (the
missing ones are `()`), and a canvas frame handler is simply `fn game_canvas_frame(dt)`.
Each control's page lists its events with their arguments, and the one the
designer opens when you double-click the control (its default event).

```rhai
fn hello_button_click() {
    result_label.text = `Hello, ${name_edit.text}!`;
}

fn name_edit_change(text) {
    hello_button.enabled = text != "";
}
```

An error in a handler is not fatal: the player shows it in a message box with
the file, line and column. Scope, shared state, `form.state`, timers, other forms
and the rest of the model are in [Scripting](scripting.md).

## Saving: the byte-stable rule

The file is written in a fixed, canonical shape, so loading and saving an
unchanged form gives back the same bytes, and a change to one property is a
one-line diff.

- Only **non-default** values are written. A property equal to its default is
  dropped on save, including one you wrote by hand (`visible = true`,
  `left = 0`, a `Button` whose `width = 100`).
- The keys of a node come in a fixed order: `kind`, `name`, `parent`, then the
  common properties in the order `left`, `top`, `width`, `height`, `anchor`,
  `visible`, `enabled`, `tab_index`, then the kind's own properties in
  alphabetical order. The window's properties are alphabetical after `name`.
- Nodes are written in their creation order, one blank line between tables.
- The writer is the TOML library's: a string may come back in single quotes or,
  with line breaks, as a `"""` block, and a number may change its spelling
  (`1` for a float becomes `1.0`). Both read back to the same value.
- **Comments are not kept.** The file is parsed into a model and written anew, so
  a `# comment` you add by hand disappears the next time the IDE saves the form.
- The IDE does not touch the disk when saving would not change a byte.

A hand-written file does not have to be canonical; it only becomes canonical when
the IDE next saves it.

## Validation and errors

The player checks every form before it opens a window, and the IDE runs the same
check before it starts a program. All the problems are reported at once, and a
project with any of them does not run.

**Load errors** stop the file from being read. They carry the line:

| Message | Cause |
|---|---|
| `` missing or non-integer `format` `` | No `format = 1` at the top. |
| `` unsupported form format 2; this build understands format 1 `` | A newer file. |
| `` missing `[window]` table `` | There is no `[window]`. |
| `` `[window]` must have a string `name` `` | The window has no name. |
| `` `Widget` is not a known widget kind `` | A misspelt or unsupported `kind`. |
| `` `txt` is not a property of Button (node `a`) `` | A key the kind does not have. |
| `` invalid `value` on `a`: expected float, found a string `` | A value of the wrong type. |
| `` invalid `fps` on `a`: expected an int in 0..=240, found `300` `` | A value outside its range. |
| `` invalid `background` on `a`: expected a `#rrggbb` colour, found `red` `` | A malformed color. |
| a TOML syntax message | Broken TOML (a missing `=`, an unclosed string). |

Each is shown with its line number (`line 9: ...`) when the line can be found.

**Validation errors** come after a clean load. They name the control and the
property but have no line:

| Message | Cause |
|---|---|
| `` `1x` is not a valid node name `` | A name that is not an identifier. The same for the form's name. |
| `` node name `a` is used more than once `` | Two controls share a name. |
| `` `anchor` expects enum, found enum `` | An enum value that is not one of the allowed names. |
| `` parent `ghost` does not exist `` | `parent` names no control. |
| `` Button `a` cannot contain children `` | `parent` is not a `Panel` or `GroupBox`. |
| `` `p` is part of a parent cycle `` | Containers that contain each other. |
| `` tab_index 3 is also used by `s1` `` | Two siblings share a `tab_index`. Reported as a warning by the form checker, but the project check lists it with the errors. |

A problem is shown as `<file>: <control>.<property>: <message>`.

## Design time and run time

The designer builds the form from the same file with the same widgets, in
*design mode*:

- No handler is bound, so nothing in your script runs while you draw.
- A `Canvas` and a `PictureBox` show a placeholder with the control's name, and a
  `Timer` shows a small label with its name. At run time the timer is invisible.
- Properties have an access rule: most are read and write in both places; some
  can only be set in the designer, some only by a script, some by neither.

| Rule | Examples |
|---|---|
| Design time and run time | `text`, `checked`, `value`, `selected`, a `ListView`'s `items`, a `Canvas`'s `background` and `fps`, every common property. |
| Design time only (a script cannot set it) | `items` of a `ComboBox` or `RadioGroup`, `multi_select`, a `NumberField`'s `step`, a `Separator`'s `orientation`, an `Edit`'s `cue`, a `Timer`'s `visible`. |
| Read-only (a script can read it) | A `Canvas`'s `mouse_x` and `mouse_y`; a `PictureBox`'s `image_width`, `image_height`, `has_image` and `format`. |

A property that is read-only for a script still reads back what the form holds.
Writing one is a script error that names the control and the property.

## An annotated example

A small mixer panel: a group that stretches with the window, a slider, a meter
driven by a timer, a check box, a combo box and buttons pinned to the corners. This
is a canonical file: loading and saving it returns the same bytes.

```toml
format = 1

[window]
name = "main_form"
height = 250
title = "Mixer"
width = 380

[[node]]
kind = "GroupBox"
name = "levels_group"
left = 12
top = 12
width = 356
height = 112
anchor = "stretch_horizontal"
text = "Levels"

[[node]]
kind = "Label"
name = "volume_label"
parent = "levels_group"
left = 12
top = 34
width = 60
text = "Volume"

[[node]]
kind = "Slider"
name = "volume_slider"
parent = "levels_group"
left = 80
top = 32
width = 260
anchor = "stretch_horizontal"
tab_index = 1
value = 75.0

[[node]]
kind = "Label"
name = "meter_label"
parent = "levels_group"
left = 12
top = 72
width = 60
text = "Meter"

[[node]]
kind = "ProgressBar"
name = "meter"
parent = "levels_group"
left = 80
top = 76
width = 260
anchor = "stretch_horizontal"
max = 64

[[node]]
kind = "CheckBox"
name = "mute_check"
left = 12
top = 140
tab_index = 1
text = "Mute"

[[node]]
kind = "ComboBox"
name = "output_combo"
left = 200
top = 140
width = 168
anchor = "top_right"
tab_index = 2
items = ["Speakers", "Headphones", "HDMI"]
selected = 1

[[node]]
kind = "Label"
name = "status_label"
left = 12
top = 214
width = 250
anchor = "bottom_left"
text = "Ready"

[[node]]
kind = "Button"
name = "reset_button"
left = 288
top = 208
width = 80
anchor = "bottom_right"
tab_index = 3
text = "Reset"

[[node]]
kind = "Timer"
name = "meter_timer"
left = 12
top = 180
enabled = true
interval = 250
```

What to notice:

- The window sets `height`, `title` and `width` in alphabetical order. A 380 x 250
  client area is not the default 320 x 200, so both are written; `resizable` is
  true, the default, so it is not.
- `levels_group` is a container. Its children name it with `parent`, and their
  `left` and `top` count from its corner. Because the group is
  `stretch_horizontal`, it widens with the window, and the two controls inside it
  that are `stretch_horizontal` widen with the group.
- `volume_label` has no `height`, so it gets a `Label`'s default of 20. The
  `Slider` has no `min` or `max`: its defaults are 0 and 100. Its `value` is a
  float and is written with a decimal point.
- `mute_check` has no `width`: a `CheckBox` is 120 units wide by default.
- `output_combo` is `top_right`, so it keeps its distance from the right edge. Its
  `items` is a list of strings, and `selected = 1` is the second item, because the
  default is the first (0).
- `status_label` and `reset_button` are pinned to the bottom. `reset_button` is
  `bottom_right`: it moves with both edges as the window grows.
- `meter_timer` is a `Timer`: it has a `left` and `top` only so the designer has
  a place to draw it. It starts disabled, so `enabled = true` is written, and
  `interval` is in milliseconds.
- `tab_index` appears only where it is not 0.

The script that goes with it, `main_form.rhai`:

```rhai
fn volume_slider_change(value) {
    status_label.text = `Volume ${value.round().to_int()}`;
}

fn mute_check_toggle(checked) {
    volume_slider.enabled = !checked;
    status_label.text = if checked { "Muted" } else { "Unmuted" };
}

fn output_combo_select(index) {
    status_label.text = `Output: ${output_combo.items[index]}`;
}

fn reset_button_click() {
    volume_slider.value = 75;
    mute_check.checked = false;
    volume_slider.enabled = true;
    output_combo.selected = 0;
    status_label.text = "Reset";
}

fn meter_timer_tick() {
    meter.value = (meter.value + 7) % 65;
}
```

Every handler is named after a control and an event in the file: `volume_slider`
raises `Change(value)`, `mute_check` raises `Toggle(checked)`, `output_combo`
raises `Select(index)`, the button raises `Click` and the timer raises `Tick`.
Setting `value`, `checked` or `selected` from a script does not raise those events
again. The `Reset` button changes the slider and the check box without going
through `volume_slider_change` or `mute_check_toggle`.

## See also

- [Controls](controls.md): every control, its properties and events.
- [Scripting](scripting.md): how handlers run, shared state and `form`.
- [Projects](projects.md): the `.lrp` file that lists forms and modules.
- [The IDE](ide.md): the designer, the toolbox and the property grid.
- [LazyRAD documentation](index.md): the hub.
