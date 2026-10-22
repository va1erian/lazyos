# Input controls

The controls a user presses, types into or drags: `Button`, `ToggleButton`,
`CheckBox`, `Hyperlink`, `Edit`, `MultilineEdit`, `NumberField` and `Slider`. Each
section lists the properties it adds to the [common ones](controls.md), the events
it raises with the arguments the handler receives, and what a script can do with
it.

Reading a property gives the current state (what the user typed, whether the box is
checked); writing one changes the control without raising its event. Handlers are
named `fn <control>_<event>` with the event in snake_case, as described in
[Forms](forms.md).

## Button

A push button with a caption and an optional icon. Default size 100 x 28.

| Property | Type | Default | Meaning |
|---|---|---|---|
| `text` | text | empty | The caption. |
| `icon` | text | empty | The name of an icon drawn before the caption (list below). Empty, or a name that is not in the list, shows no icon. |

| Event | Handler | Arguments |
|---|---|---|
| `Click` (default) | `fn <name>_click()` | none |

The designer starts a new button with its name as the caption. Both properties can
be changed by a script, so a button can turn into another:

```rhai
let running = false;

fn start_button_click() {
    running = !running;
    start_button.text = if running { "Stop" } else { "Start" };
    start_button.icon = if running { "pause" } else { "play" };
}
```

The icon names are the Lucide outlines LazyOS shares across its apps: `app-window`,
`arrow-down-a-z`, `between-horizontal-end`, `between-horizontal-start`,
`between-vertical-end`, `between-vertical-start`, `bold`, `book-open`, `box`,
`bug`, `check`, `chevron-down`, `chevron-left`, `chevron-right`, `chevron-up`,
`chevrons-up-down`, `circle-dot`, `circle-help`, `circle-x`, `clipboard-paste`,
`code`, `copy`, `disc`, `download`, `ellipsis`, `external-link`, `eye-off`, `eye`,
`file-code`, `file-plus`, `file-text`, `file`, `folder-open`, `folder`, `group`,
`heading-1`, `heading-2`, `heading-3`, `history`, `home`, `image`, `inbox`,
`indent-decrease`, `indent-increase`, `info`, `italic`, `layout-grid`, `link`,
`list-ordered`, `list-tree`, `list`, `lock`, `log-out`, `mail`, `menu`, `minus`,
`monitor`, `mouse-pointer-2`, `package`, `pause`, `pencil`, `play`, `plus`,
`printer`, `rectangle-horizontal`, `redo-2`, `refresh-cw`, `repeat`, `reply`,
`ruler`, `save-all`, `save`, `scissors`, `search`, `send`, `separator-horizontal`,
`settings`, `shuffle`, `skip-back`, `skip-forward`, `square-check`,
`square-mouse-pointer`, `square`, `star`, `strikethrough`, `table`, `tag`,
`terminal`, `text-align-center`, `text-align-end`, `text-align-justify`,
`text-align-start`, `text-cursor-input`, `text-quote`, `trash-2`,
`triangle-alert`, `type`, `underline`, `undo-2`, `unlock`, `upload`, `users`,
`volume-2`, `wrap-text`, `x` and `zap`. The Picture Viewer sample uses an
icon-only toolbar: buttons with an `icon` and no `text`.

## ToggleButton

A button that stays down when pressed, like a tool in a toolbar. Default size
100 x 28.

| Property | Type | Default | Meaning |
|---|---|---|---|
| `text` | text | empty | The caption. |
| `checked` | bool | false | Whether the button is latched down. |

| Event | Handler | Arguments |
|---|---|---|
| `Toggle` (default) | `fn <name>_toggle(checked)` | `checked`: the new state (bool) |

```rhai
fn wrap_toggle_toggle(checked) {
    status_label.text = if checked { "Wrap on" } else { "Wrap off" };
}
```

## CheckBox

A labelled box that is checked or not. Default size 120 x 24.

| Property | Type | Default | Meaning |
|---|---|---|---|
| `text` | text | empty | The label. |
| `checked` | bool | false | Whether the box is checked. |

| Event | Handler | Arguments |
|---|---|---|
| `Toggle` (default) | `fn <name>_toggle(checked)` | `checked`: the new state (bool) |

```rhai
fn agree_check_toggle(checked) {
    ok_button.enabled = checked;
}
```

To read the state elsewhere use `agree_check.checked`. Writing `checked` from a
script does not raise `Toggle`.

## Hyperlink

A label that acts like a link: it is drawn as one and raises `Click`. It has no
address property; the handler decides what the link does. Default size 120 x 20.

| Property | Type | Default | Meaning |
|---|---|---|---|
| `text` | text | empty | The link's text. |

| Event | Handler | Arguments |
|---|---|---|
| `Click` (default) | `fn <name>_click()` | none |

```rhai
fn help_link_click() {
    // On LazyOS, hand the address to whichever app handles web links.
    sys::mimed::open("http://example.com/", "open");
}
```

## Edit

A single-line text field. Default size 160 x 24.

| Property | Type | Default | Meaning |
|---|---|---|---|
| `text` | text | empty | What the field holds. |
| `cue` | text | empty | A placeholder shown while the field is empty ("What needs doing?"). Set it in the designer: a script can read it but not change it. |

| Event | Handler | Arguments |
|---|---|---|
| `Change` (default) | `fn <name>_change(text)` | `text`: the field's new text (string) |

`Change` is raised as the user types; setting `text` from a script does not raise
it. Setting `text` replaces the content, puts the caret at the end and clears the
field's undo history.

```rhai
fn name_edit_change(text) {
    let name = text;
    name.trim();   // trims in place and returns ()
    greet_button.enabled = name.len() > 0;
}

fn clear_button_click() {
    name_edit.text = "";
}
```

There is no `read_only`, password or selection property in this build.

## MultilineEdit

A text area that holds several lines. Default size 200 x 100.

| Property | Type | Default | Meaning |
|---|---|---|---|
| `text` | text | empty | The whole text; lines are separated by `\n`. |

| Event | Handler | Arguments |
|---|---|---|
| `Change` (default) | `fn <name>_change(text)` | `text`: the new text (string) |

```rhai
fn notes_edit_change(text) {
    let lines = text.split("\n").len();
    count_label.text = `${lines} lines`;
}
```

In a `.lfm` a multi-line `text` is written as a TOML `"""` block or with `\n`
escapes; both load the same.

## NumberField

A numeric field with up and down steppers. Default size 120 x 28.

| Property | Type | Default | Meaning |
|---|---|---|---|
| `value` | float | 0.0 | The current value. |
| `min` | float | 0.0 | The smallest allowed value. |
| `max` | float | 100.0 | The largest allowed value. |
| `step` | float | 1.0 | The amount the steppers and the arrow keys change the value by. Design time only. |

| Event | Handler | Arguments |
|---|---|---|
| `Change` (default) | `fn <name>_change(value)` | `value`: the new value (float) |
| `Commit` | `fn <name>_commit(value)` | `value`: the committed value (float) |

The value is always kept within `min..max`: setting it outside clamps it. A script
can also change `min` and `max`.

```rhai
fn quantity_field_commit(value) {
    total_label.text = `Total: ${value * 4.5}`;
}
```

## Slider

A horizontal range control. Default size 160 x 24.

| Property | Type | Default | Meaning |
|---|---|---|---|
| `value` | float | 0.0 | The current value. |
| `min` | float | 0.0 | The smallest value. |
| `max` | float | 100.0 | The largest value. |

| Event | Handler | Arguments |
|---|---|---|
| `Change` (default) | `fn <name>_change(value)` | `value`: the value while the thumb moves (float) |
| `Commit` | `fn <name>_commit(value)` | `value`: the value when the drag ends (float) |

Use `Change` to follow the thumb live and `Commit` for work you only want to do
once, when the user lets go. `value` is clamped into `min..max`, and changing
`min` or `max` from a script clamps the current value into the new range.

```rhai
fn volume_slider_change(value) {
    volume_label.text = `${value.round().to_int()}%`;
}

fn volume_slider_commit(value) {
    status_label.text = `Volume set to ${value.round().to_int()}`;
}
```

A slider has no `step` property: it moves continuously, so round the value
yourself when you need whole numbers.

## See also

- [Controls](controls.md): every control, the common properties, how scripts read and write.
- [Choices and lists](controls-choices.md): `RadioGroup`, `ComboBox`, `ListView`.
- [Display and layout](controls-display.md): `Label`, `ProgressBar` and the containers.
- [Forms](forms.md): the `.lfm` format and event naming.
- [Scripting](scripting.md): how handlers run.
