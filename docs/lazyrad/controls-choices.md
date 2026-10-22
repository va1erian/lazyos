# Choices and lists

Three controls let the user pick from a list of strings: `RadioGroup` (all the
options visible, exactly one chosen), `ComboBox` (a drop-down) and `ListView`
(a scrolling list that can also be changed by a script). All three have `items`,
an array of strings, and `selected`, the index of the chosen one.

Writing `selected` from a script does not raise the `Select` event; only the user's
choice does.

## RadioGroup

A vertical set of radio options in one control: one option is always chosen. A
form does not hold one radio control per option; it holds one `RadioGroup` whose
`items` are the labels. Default size 160 x 96.

| Property | Type | Default | Meaning |
|---|---|---|---|
| `items` | list | empty | The option labels, top to bottom. Design time only: a script can read them but not replace them. |
| `selected` | int, at least 0 | 0 | The index of the chosen option. |

| Event | Handler | Arguments |
|---|---|---|
| `Select` (default) | `fn <name>_select(index)` | `index`: the chosen option (int) |

```toml
[[node]]
kind = "RadioGroup"
name = "size_group"
left = 16
top = 16
items = ["Small", "Medium", "Large"]
selected = 1
```

```rhai
fn size_group_select(index) {
    status_label.text = `Size: ${size_group.items[index]}`;
}

fn reset_button_click() {
    size_group.selected = 1;      // an index outside the items is ignored
}
```

A negative `selected` is an error, and an index past the last option does nothing.
There is no "nothing chosen" state; for that use a `ListView`.

## ComboBox

A drop-down list of choices. Default size 160 x 24.

| Property | Type | Default | Meaning |
|---|---|---|---|
| `items` | list | empty | The choices. Design time only: a script can read them but not replace them. |
| `selected` | int, at least 0 | 0 | The index of the chosen item. The field shows that item's text. |

| Event | Handler | Arguments |
|---|---|---|
| `Select` (default) | `fn <name>_select(index)` | `index`: the chosen item (int) |

To get the chosen text, index into `items`. The Hello sample does it with a guard
for the moment before any choice is made (this is a shortened version of it):

```rhai
fn hello_button_click() {
    let words = greeting_combo.items;
    let index = greeting_combo.selected;
    let word = if index >= 0 && index < words.len() { words[index] } else { "Hello" };
    result_label.text = `${word}, ${name_edit.text}!`;
}

fn greeting_combo_select(index) {
    result_label.text = `Greeting: ${greeting_combo.items[index]}`;
}
```

Setting `selected` to an index past the last item does nothing. If you need a list a
script can fill, use a `ListView`.

## ListView

A scrolling list of rows with a selection. Unlike the other two it is meant to be
changed at run time: a script replaces `items` to add, remove or reload rows.
Default size 240 x 140.

| Property | Type | Default | Meaning |
|---|---|---|---|
| `items` | list | empty | The rows, one string each. A script may replace them at run time. |
| `selected` | int | -1 | The selected row, or -1 for none. Writing a negative number, or an index past the last row, clears the selection. |
| `multi_select` | bool | false | Whether the user can select more than one row. Design time only. |

| Event | Handler | Arguments |
|---|---|---|
| `Select` (default) | `fn <name>_select(index)` | `index`: the primary selected row (int) |
| `Activate` | `fn <name>_activate(index)` | `index`: the row activated by Return or a double-click (int) |

`items` is an array of strings; every element must be a string, so convert numbers
first (`n.to_string()`). A script reads the array, changes it and assigns it back:

```rhai
fn add_button_click() {
    let text = todo_edit.text;
    if text != "" {
        let items = todo_list.items;
        items.push(text);
        todo_list.items = items;
        todo_edit.text = "";
    }
}

fn remove_button_click() {
    let index = todo_list.selected;
    let items = todo_list.items;
    if index >= 0 && index < items.len() {
        items.remove(index);
        todo_list.items = items;
    }
}

fn todo_list_activate(index) {
    status_label.text = `Opened ${todo_list.items[index]}`;
}
```

The first two handlers are the To-do sample's: an `Edit`, two `Button`s and a
`ListView`.

Notes:

- Replacing `items` keeps the selection when it is still in range. When nothing was
  selected, or the selection is past the new end, the **last row becomes selected**,
  and the list scrolls back to the top. Set `selected = -1` afterwards if you want
  no selection.
- With `multi_select` on, `selected` and the `Select` argument are the primary row;
  a script has no way to list every selected row in
  this build.
- Setting `items` or `selected` does not raise `Select`.

## See also

- [Controls](controls.md): the overview and the common properties.
- [Input controls](controls-input.md): `Edit`, `Slider` and the other controls that take values.
- [Display and layout](controls-display.md): `Label`, `GroupBox` and `Panel`.
- [Forms](forms.md): how lists are written in the `.lfm`.
- [Scripting](scripting.md): handlers and shared state.
