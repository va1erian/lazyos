# LazyRAD cookbook

Short, working recipes for LazyRAD forms and scripts on LazyOS. Each one states a
goal, gives the form (the `[[node]]` entries to add to a `.lfm`), the Rhai script
for the form's `.rhai` file, and says what to expect or what to watch for. They
are meant to be copied, so they are small and use the same names as the rest of
the documentation. The rules behind them are in [Forms](forms.md),
[Controls](controls.md), [Scripting](scripting.md) and [the standard library](stdlib.md).

## How to use a recipe

* **Form.** The fragment holds `[[node]]` tables only. Put them under the
  `format = 1` line and a `[window]` table, as in [Forms](forms.md). A recipe
  says "window 320 x 140" when the size matters; the window's `name` is
  `main_form` everywhere, so the script file is `main_form.rhai`.
* **Names.** A control's name is a variable in the script, so recipes name
  controls after what they do (`name_edit`, `save_button`, `status_label`) and
  never reuse a name for a script variable.
* **Handlers.** `fn name_edit_change(text)` is the `Change` event of `name_edit`;
  setting a control from a script never raises its own event.
* **LazyOS recipes** list the interface the installed app needs. Make LazyOS App
  works that out from the script, see [Packaging](packaging.md) and recipe 49.
* **Run it.** Create a project in the IDE ([The IDE](ide.md)), paste the nodes
  into the form's `.lfm` and the script into its `.rhai`, and press F5.

The recipes are grouped: [Basics](#basics), [Input handling](#input-handling),
[Lists and data](#lists-and-data), [Timers and animation](#timers-and-animation),
[Files](#files), [Dialogs](#dialogs), [Pictures](#pictures),
[LazyOS integration](#lazyos-integration), [Packaging](#packaging) and
[Debugging](#debugging).

## Basics

### 1. Say hello

Goal: a button writes a greeting that includes what the user typed.

Form (window 320 x 140):

```toml
[[node]]
kind = "Edit"
name = "name_edit"
left = 16
top = 16
width = 200
cue = "Your name"

[[node]]
kind = "Button"
name = "hello_button"
left = 16
top = 52
width = 120
height = 32
text = "Say hello"

[[node]]
kind = "Label"
name = "result_label"
left = 16
top = 96
width = 280
```

Script:

```rhai
fn hello_button_click() {
    let who = name_edit.text;
    result_label.text = if who == "" { "Hello, stranger!" } else { `Hello, ${who}!` };
}
```

The click handler runs once per click. `if` is an expression, so it can pick the
text directly.

### 2. Read, change and clear text

Goal: transform what is in an edit and write it somewhere else.

Form (window 340 x 170):

```toml
[[node]]
kind = "Edit"
name = "source_edit"
left = 16
top = 16
width = 300

[[node]]
kind = "Button"
name = "upper_button"
left = 16
top = 52
width = 90
height = 32
text = "Upper case"

[[node]]
kind = "Button"
name = "trim_button"
left = 114
top = 52
width = 90
height = 32
text = "Trim"

[[node]]
kind = "Button"
name = "clear_button"
left = 212
top = 52
width = 90
height = 32
text = "Clear"

[[node]]
kind = "Label"
name = "result_label"
left = 16
top = 100
width = 300
```

Script:

```rhai
fn upper_button_click() {
    result_label.text = source_edit.text.to_upper();
}

fn trim_button_click() {
    let text = source_edit.text;
    text.trim();                      // trims `text` itself and returns ()
    source_edit.text = text;
    result_label.text = `${text.len()} characters`;
}

fn clear_button_click() {
    source_edit.text = "";
    result_label.text = "";
}
```

`trim`, `replace` and `pad` change a string in place and return `()`. Copy the
control's text into a variable first: `source_edit.text.trim()` would write the
trimmed text back into the control and still return `()`.

### 3. Enable and disable a control

Goal: keep a button disabled until the edit has something in it.

Form (window 320 x 130):

```toml
[[node]]
kind = "Edit"
name = "name_edit"
left = 16
top = 16
width = 200

[[node]]
kind = "Button"
name = "greet_button"
left = 16
top = 52
width = 120
height = 32
enabled = false
text = "Greet"

[[node]]
kind = "Label"
name = "status_label"
left = 16
top = 96
width = 280
```

Script:

```rhai
fn name_edit_change(text) {
    let typed = text;
    typed.trim();                     // in place: spaces alone do not count
    greet_button.enabled = typed != "";
}

fn greet_button_click() {
    status_label.text = `Hello, ${name_edit.text}!`;
}
```

Do not write `text.trim() != ""`: `trim` returns `()`, so that comparison is
always true. `Change` is raised as the user types, so the button follows the field.

### 4. Show and hide a group of controls

Goal: a toggle button reveals and hides a panel and what is inside it.

Form (window 320 x 180):

```toml
[[node]]
kind = "ToggleButton"
name = "details_toggle"
left = 16
top = 16
width = 120
height = 32
text = "Show details"

[[node]]
kind = "Panel"
name = "details_panel"
left = 16
top = 60
width = 280
height = 80
visible = false

[[node]]
kind = "Label"
name = "address_label"
parent = "details_panel"
left = 8
top = 12
width = 60
text = "Address"

[[node]]
kind = "Edit"
name = "address_edit"
parent = "details_panel"
left = 72
top = 8
width = 190
```

Script:

```rhai
fn details_toggle_toggle(checked) {
    details_panel.visible = checked;
    details_toggle.text = if checked { "Hide details" } else { "Show details" };
}
```

Hiding a `Panel` hides its children with it. `form.hide()` and `form.show()` do the
same for every control of the form at once; the window itself stays.

### 5. A check box that gates a button

Goal: the OK button works only after the user agrees.

Form (window 320 x 140):

```toml
[[node]]
kind = "CheckBox"
name = "agree_check"
left = 16
top = 16
width = 220
text = "I agree to the terms"

[[node]]
kind = "Button"
name = "ok_button"
left = 16
top = 52
width = 100
height = 32
enabled = false
text = "OK"

[[node]]
kind = "Label"
name = "status_label"
left = 16
top = 96
width = 280
```

Script:

```rhai
fn agree_check_toggle(checked) {
    ok_button.enabled = checked;
}

fn ok_button_click() {
    status_label.text = if agree_check.checked { "Accepted" } else { "Not accepted" };
}
```

`Toggle` carries the new state; elsewhere read it with `agree_check.checked`.

### 6. Radio-style choices

Goal: pick exactly one option out of several.

Form (window 320 x 190):

```toml
[[node]]
kind = "RadioGroup"
name = "size_group"
left = 16
top = 16
items = ["Small", "Medium", "Large"]
selected = 1

[[node]]
kind = "Button"
name = "large_button"
left = 190
top = 16
width = 110
height = 32
text = "Pick Large"

[[node]]
kind = "Label"
name = "status_label"
left = 16
top = 130
width = 280
```

Script:

```rhai
fn form_load() {
    show_size(size_group.selected);
}

fn size_group_select(index) {
    show_size(index);
}

fn large_button_click() {
    size_group.selected = 2;          // does not raise Select...
    show_size(2);                     // ...so show it yourself
}

fn show_size(index) {
    status_label.text = `Size: ${size_group.items[index]}`;
}
```

LazyRAD has one `RadioGroup` control holding every option, not one control per
option. Its `items` are fixed in the form (a script can read them, not replace
them) and one option is always chosen.

### 7. A combo box and its chosen text

Goal: read the text of the chosen item, and choose one from the script.

Form (window 320 x 150):

```toml
[[node]]
kind = "ComboBox"
name = "greeting_combo"
left = 16
top = 16
width = 200
items = ["Hello", "Hi", "Hey"]

[[node]]
kind = "Button"
name = "last_button"
left = 16
top = 52
width = 120
height = 32
text = "Pick the last"

[[node]]
kind = "Label"
name = "status_label"
left = 16
top = 100
width = 280
```

Script:

```rhai
fn greeting_combo_select(index) {
    status_label.text = `Chosen: ${greeting_combo.items[index]}`;
}

fn last_button_click() {
    let words = greeting_combo.items;
    greeting_combo.selected = words.len() - 1;
    status_label.text = `Chosen: ${words[words.len() - 1]}`;
}
```

The `items` of a combo box are set in the form only. For a list that a script
fills, use a `ListView` (next recipe).

### 8. Fill a list and follow its selection

Goal: fill a `ListView` from the script, react to a click and a double-click, and
clear the selection.

Form (window 320 x 250):

```toml
[[node]]
kind = "ListView"
name = "fruit_list"
left = 16
top = 16
width = 180
height = 150

[[node]]
kind = "Button"
name = "reload_button"
left = 208
top = 16
width = 90
height = 32
text = "Reload"

[[node]]
kind = "Button"
name = "none_button"
left = 208
top = 56
width = 90
height = 32
text = "Select none"

[[node]]
kind = "Label"
name = "status_label"
left = 16
top = 180
width = 280
```

Script:

```rhai
const FRUITS = ["Apple", "Banana", "Cherry", "Date", "Elderberry"];

fn form_load() {
    reload();
}

fn reload() {
    fruit_list.items = FRUITS;
    fruit_list.selected = -1;         // setting items selects the last row; undo that
    status_label.text = "Nothing selected";
}

fn fruit_list_select(index) {
    status_label.text = `Selected ${fruit_list.items[index]}`;
}

fn fruit_list_activate(index) {
    status_label.text = `Opened ${fruit_list.items[index]}`;
}

fn reload_button_click() { reload(); }
fn none_button_click() { fruit_list.selected = -1; }
```

When nothing was selected, replacing `items` selects the last row and scrolls the
list to the top; assign `selected = -1` afterwards for no selection.

### 9. Share state between handlers

Goal: keep a value that several handlers read and change.

Form (window 320 x 140):

```toml
[[node]]
kind = "Button"
name = "plus_button"
left = 16
top = 16
width = 100
height = 32
text = "Add one"

[[node]]
kind = "Button"
name = "reset_button"
left = 124
top = 16
width = 100
height = 32
text = "Reset"

[[node]]
kind = "Label"
name = "count_label"
left = 16
top = 64
width = 290
```

Script:

```rhai
let clicks = 0;                       // top level: shared by every handler

fn form_load() {
    form.state.total = 0;             // form.state: shared too, and kept by a hot reload
}

fn plus_button_click() {
    clicks += 1;
    form.state.total += clicks;
    count_label.text = `clicks ${clicks}, running total ${form.state.total}`;
}

fn reset_button_click() {
    clicks = 0;
    form.state.total = 0;
    count_label.text = "reset";
}

fn form_reload(old_state) {
    form.state.total = old_state.total;   // after a hot reload `clicks` starts again at 0
}
```

Declare shared variables with `let` at the top level; assigning to a name that was
never declared is `Variable not found`. Keep what must survive a hot reload in
`form.state` ([Scripting](scripting.md)).

### 10. A helper module in a second file

Goal: put reusable functions in `util.rhai` and call them from the form.

Project: add the module to the `.lrp` ([Projects](projects.md)):

```toml
[[items]]
kind = "module"
name = "util"
code = "util.rhai"
```

Module `util.rhai`:

```rhai
// util.rhai
const GREETING = "Hello";

fn title_case(text) {
    let out = "";
    let up = true;
    for ch in text {
        out += if up { ch.to_upper() } else { ch };
        up = ch == ' ';
    }
    out
}

fn greeting(name) {
    `${global::GREETING}, ${title_case(name)}!`
}
```

Form (window 340 x 200):

```toml
[[node]]
kind = "Edit"
name = "name_edit"
left = 16
top = 16
width = 220

[[node]]
kind = "Button"
name = "hello_button"
left = 16
top = 52
width = 120
height = 32
text = "Go"

[[node]]
kind = "Label"
name = "result_label"
left = 16
top = 100
width = 300

[[node]]
kind = "Label"
name = "qualified_label"
left = 16
top = 128
width = 300

[[node]]
kind = "Label"
name = "bare_label"
left = 16
top = 156
width = 300
```

Script `main_form.rhai`:

```rhai
fn hello_button_click() {
    result_label.text = util::greeting(name_edit.text);       // qualified by the module name
    qualified_label.text = util::title_case(name_edit.text);
    bare_label.text = title_case(name_edit.text);             // bare: modules are global too
}
```

A module's top-level `const` is not visible inside its own functions, so
`greeting` reads it as `global::GREETING`. `import "util" as u;` at the top of the
form script gives the same functions as `u::title_case(...)`. Two modules that
define the same name and arity clash when called bare; qualify the call.

## Input handling

### 11. Validate an edit as the user types

Goal: enable OK only for a whole number from 0 to 130, and say what is wrong.

Form (window 320 x 150):

```toml
[[node]]
kind = "Edit"
name = "age_edit"
left = 16
top = 16
width = 120
cue = "Age"

[[node]]
kind = "Label"
name = "error_label"
left = 16
top = 48
width = 290

[[node]]
kind = "Button"
name = "ok_button"
left = 16
top = 80
width = 100
height = 32
enabled = false
text = "OK"
```

Script:

```rhai
fn age_edit_change(text) {
    let age = parse_age(text);
    ok_button.enabled = age >= 0;
    error_label.text = if age >= 0 || text == "" { "" } else { "Enter a whole number from 0 to 130" };
}

// The age, or -1 when `text` is not a whole number from 0 to 130.
fn parse_age(text) {
    try {
        let age = parse_int(text);
        if age < 0 || age > 130 { return -1; }
        return age;
    } catch {
        return -1;
    }
}
```

`parse_int` of text that is not a number is an error, so the `try` turns it into
a value the handler can test.

### 12. Parse numbers safely

Goal: divide two typed numbers and never fail on bad input.

Form (window 320 x 190):

```toml
[[node]]
kind = "Edit"
name = "a_edit"
left = 16
top = 16
width = 100
cue = "A"

[[node]]
kind = "Edit"
name = "b_edit"
left = 130
top = 16
width = 100
cue = "B"

[[node]]
kind = "Button"
name = "divide_button"
left = 16
top = 52
width = 120
height = 32
text = "A / B"

[[node]]
kind = "Label"
name = "result_label"
left = 16
top = 100
width = 290
```

Script:

```rhai
fn divide_button_click() {
    let a = to_number(a_edit.text);
    let b = to_number(b_edit.text);
    if a == () || b == () {
        result_label.text = "Type two numbers";
    } else if b == 0.0 {
        result_label.text = "Cannot divide by zero";
    } else {
        result_label.text = `${a} / ${b} = ${a / b}`;
    }
}

// The number in `text` as a float, or () when it is not one.
fn to_number(text) {
    try {
        return parse_float(text);
    } catch {
        return ();
    }
}
```

Whole-number division truncates (`7 / 2` is `3`), which is why this recipe parses
floats. `()` is the value for "nothing", so a function can return a number or `()`
and the caller tests it.

### 13. Key events and held keys

Goal: show which key was pressed and whether Shift is held.

Form (window 340 x 230):

```toml
[[node]]
kind = "Canvas"
name = "pad_canvas"
left = 10
top = 10
width = 320
height = 150
background = "#101018"

[[node]]
kind = "Label"
name = "status_label"
left = 10
top = 172
width = 320
text = "Click the pad, then press a key"
```

Script:

```rhai
fn form_load() {
    pad_canvas.focus();               // keys reach a canvas only while it has the focus
}

fn pad_canvas_key_down(key) {
    let shift = if pad_canvas.is_key_down("shift") { " (shift held)" } else { "" };
    status_label.text = `pressed ${key}${shift}`;
    pad_canvas.clear("#101018");
    pad_canvas.text(12, 12, key, "#ffffff", 48);
}

fn pad_canvas_key_up(key) {
    status_label.text = `released ${key}`;
}
```

Key names are lower-case (`"a"`, `"left"`, `"space"`, `"f5"`). `KeyDown` is raised once
per press, not on auto-repeat. An `Edit` raises only `Change`; there is no Enter
event, so give the form a button for "submit".

### 14. A slider and a number field kept in step

Goal: two controls edit the same value without feeding each other.

Form (window 340 x 150):

```toml
[[node]]
kind = "Slider"
name = "volume_slider"
left = 16
top = 16
width = 220
value = 40.0

[[node]]
kind = "NumberField"
name = "volume_field"
left = 16
top = 52
width = 120
value = 40.0

[[node]]
kind = "Label"
name = "status_label"
left = 16
top = 100
width = 300
```

Script:

```rhai
fn volume_slider_change(value) {
    volume_field.value = value.round();   // no Change comes back: no loop
}

fn volume_field_change(value) {
    volume_slider.value = value;
}

fn volume_slider_commit(value) {
    status_label.text = `Volume set to ${value.round().to_int()}`;
}
```

`Change` follows the thumb live; `Commit` fires once when the drag ends, which is the
place for work you do not want to repeat. Both controls clamp `value` into
`min..max`.

## Lists and data

### 15. A to-do list

Goal: add, tick off and remove rows.

Form (window 280 x 290):

```toml
[[node]]
kind = "Edit"
name = "todo_edit"
left = 12
top = 12
width = 160
cue = "What needs doing?"

[[node]]
kind = "Button"
name = "add_button"
left = 180
top = 12
width = 64
height = 24
text = "Add"

[[node]]
kind = "Button"
name = "remove_button"
left = 180
top = 44
width = 64
height = 24
text = "Remove"

[[node]]
kind = "ListView"
name = "todo_list"
left = 12
top = 80
width = 232
height = 140

[[node]]
kind = "Label"
name = "count_label"
left = 12
top = 232
width = 232
text = "0 items"
```

Script:

```rhai
fn add_button_click() {
    let text = todo_edit.text;
    text.trim();
    if text != "" {
        let items = todo_list.items;
        items.push(text);
        todo_list.items = items;
        todo_edit.text = "";
        show_count();
    }
}

fn remove_button_click() {
    let index = todo_list.selected;
    let items = todo_list.items;
    if index >= 0 && index < items.len() {
        items.remove(index);
        todo_list.items = items;
        show_count();
    }
}

// Double-click or Return: tick the row off, or untick it.
fn todo_list_activate(index) {
    let items = todo_list.items;
    let item = items[index];
    items[index] = if item.starts_with("[x] ") { item.sub_string(4) } else { "[x] " + item };
    todo_list.items = items;
}

fn show_count() {
    count_label.text = `${todo_list.items.len()} items`;
}
```

This is the To-do sample with a ticked state. The pattern for a `ListView` is always
read `items`, change the copy, assign it back.

### 16. Change a list the right way

Goal: understand why `let a = list.items; a.push(x)` does nothing.

Form (window 320 x 220):

```toml
[[node]]
kind = "ListView"
name = "names_list"
left = 16
top = 16
width = 160
height = 150
items = ["first"]

[[node]]
kind = "Button"
name = "add_button"
left = 190
top = 16
width = 110
height = 28
text = "Push in place"

[[node]]
kind = "Button"
name = "broken_button"
left = 190
top = 52
width = 110
height = 28
text = "Copy only"

[[node]]
kind = "Button"
name = "fixed_button"
left = 190
top = 88
width = 110
height = 28
text = "Copy and assign"
```

Script:

```rhai
fn add_button_click() {
    names_list.items.push(`item ${names_list.items.len() + 1}`);   // written back
}

fn broken_button_click() {
    let items = names_list.items;
    items.push("never shown");        // changes the copy in `items` only
}

fn fixed_button_click() {
    let items = names_list.items;
    items.push("shown");
    names_list.items = items;         // assign the changed copy back
}
```

Reading a collection property gives a copy. A path that starts at the control
(`names_list.items.push(...)`) is written back; a variable holding the copy is not.
Every element of `items` must be a string: convert numbers with `n.to_string()`.

### 17. Sort a list

Goal: sort rows, ignoring case, and reverse them.

Form (window 320 x 250):

```toml
[[node]]
kind = "ListView"
name = "word_list"
left = 16
top = 16
width = 160
height = 170
items = ["pear", "Apple", "fig", "banana", "Cherry"]

[[node]]
kind = "Button"
name = "sort_button"
left = 190
top = 16
width = 110
height = 28
text = "Sort A-Z"

[[node]]
kind = "Button"
name = "nocase_button"
left = 190
top = 52
width = 110
height = 28
text = "Ignore case"

[[node]]
kind = "Button"
name = "reverse_button"
left = 190
top = 88
width = 110
height = 28
text = "Reverse"
```

Script:

```rhai
fn sort_button_click() {
    let items = word_list.items;
    items.sort();                     // plain order: capitals come first
    word_list.items = items;
}

fn nocase_button_click() {
    let items = word_list.items;
    items.sort(|a, b| compare_text(a.to_lower(), b.to_lower()));
    word_list.items = items;
}

fn reverse_button_click() {
    let items = word_list.items;
    items.reverse();
    word_list.items = items;
}

// -1, 0 or 1, as `sort` expects from a comparison.
fn compare_text(a, b) {
    if a < b { -1 } else if a > b { 1 } else { 0 }
}
```

`sort` and `reverse` change the array in place, so sort the copy and assign it
back. The comparison function must return an integer.

### 18. Filter a list as the user types

Goal: show only the rows that contain the typed text.

Form (window 300 x 280):

```toml
[[node]]
kind = "Edit"
name = "filter_edit"
left = 12
top = 12
width = 200
cue = "Filter"

[[node]]
kind = "ListView"
name = "result_list"
left = 12
top = 48
width = 200
height = 170

[[node]]
kind = "Label"
name = "count_label"
left = 12
top = 228
width = 200
```

Script:

```rhai
const WORDS = ["apple", "apricot", "banana", "blueberry", "cherry", "date",
               "elderberry", "fig", "grape"];

fn form_load() {
    show_matches("");
}

fn filter_edit_change(text) {
    show_matches(text);
}

fn show_matches(query) {
    let needle = query.to_lower();
    let found = WORDS.filter(|word| word.contains(needle));
    result_list.items = found;
    result_list.selected = -1;
    count_label.text = `${found.len()} of ${WORDS.len()}`;
}
```

The full list stays in a constant; the control only ever holds the filtered copy.
The closure captures `needle`, a local variable, by value.

### 19. Master and detail

Goal: pick a row and show that record's fields.

Form (window 340 x 200):

```toml
[[node]]
kind = "ListView"
name = "item_list"
left = 12
top = 12
width = 140
height = 150

[[node]]
kind = "Label"
name = "name_label"
left = 168
top = 16
width = 150

[[node]]
kind = "Label"
name = "price_label"
left = 168
top = 44
width = 150

[[node]]
kind = "Label"
name = "stock_label"
left = 168
top = 72
width = 150
```

Script:

```rhai
let products = [
    #{ name: "Widget", price: 2.5, stock: 12 },
    #{ name: "Gadget", price: 19.9, stock: 3 },
    #{ name: "Gizmo", price: 0.75, stock: 40 },
];

fn form_load() {
    item_list.items = products.map(|p| p.name);
    item_list.selected = 0;
    item_list_select(0);              // setting `selected` raises no event
}

fn item_list_select(index) {
    let product = products[index];
    name_label.text = product.name;
    price_label.text = `Price: ${product.price}`;
    stock_label.text = `In stock: ${product.stock}`;
}
```

Rows are maps in an array; the list shows one field and the handler looks the
whole record up by the row's index.

### 20. A table on a canvas

Goal: draw a table with a header and a clickable row. There is no grid control, so
a `Canvas` is the way to a real table.

Form (window 360 x 230):

```toml
[[node]]
kind = "Canvas"
name = "table_canvas"
left = 10
top = 10
width = 340
height = 150
background = "#ffffff"

[[node]]
kind = "Label"
name = "status_label"
left = 10
top = 172
width = 340
text = "Click a row"
```

Script:

```rhai
const HEADERS = ["Item", "Qty", "Price"];
const COLUMN_X = [10, 170, 240];
const ROW_H = 24;

let rows = [["Widget", "12", "2.50"], ["Gadget", "3", "19.90"],
            ["Gizmo", "40", "0.75"], ["Doohickey", "7", "5.00"]];
let chosen = -1;

fn form_load() {
    draw_table();
}

fn draw_table() {
    table_canvas.clear("#ffffff");
    table_canvas.fill_rect(0, 0, 340, ROW_H, "#dde3f0");
    for c in 0..HEADERS.len() {
        table_canvas.text(COLUMN_X[c], 4, HEADERS[c], "#000000", 15);
    }
    for r in 0..rows.len() {
        let y = ROW_H * (r + 1);
        if r == chosen {
            table_canvas.fill_rect(0, y, 340, ROW_H, "#ffe9a8");
        }
        for c in 0..HEADERS.len() {
            table_canvas.text(COLUMN_X[c], y + 4, rows[r][c], "#202020", 15);
        }
        table_canvas.line(0, y + ROW_H, 340, y + ROW_H, "#cccccc", 1);
    }
}

fn table_canvas_mouse_down(x, y, button) {
    let r = (y / ROW_H).to_int() - 1;
    if button == "left" && r >= 0 && r < rows.len() {
        chosen = r;
        draw_table();
        status_label.text = `Row ${r + 1}: ${rows[r][0]}`;
    }
}
```

The canvas keeps its drawing until the next `clear`, so a table that changes only
on events needs no frame loop. For plain rows of text, a `ListView` is simpler.

## Timers and animation

### 21. A clock

Goal: show the time once a second.

Form (window 280 x 90):

```toml
[[node]]
kind = "Label"
name = "clock_label"
left = 16
top = 16
width = 240
height = 24

[[node]]
kind = "Timer"
name = "clock_timer"
left = 16
top = 50
interval = 1000
enabled = true
```

Script:

```rhai
fn form_load() {
    show_time();
}

fn clock_timer_tick() {
    show_time();
}

fn show_time() {
    clock_label.text = now();
}
```

`now()` is `YYYY-MM-DD HH:MM:SS`. On LazyOS there is no time zone database, so it
is UTC. A `Timer` starts stopped; the form turns it on with `enabled = true`.

### 22. A countdown

Goal: count down from a chosen number of seconds with a progress bar, then say so.

Form (window 320 x 190):

```toml
[[node]]
kind = "NumberField"
name = "seconds_field"
left = 16
top = 16
width = 120
min = 1.0
max = 3600.0
value = 10.0

[[node]]
kind = "Button"
name = "start_button"
left = 150
top = 14
width = 100
height = 32
text = "Start"

[[node]]
kind = "ProgressBar"
name = "countdown_bar"
left = 16
top = 64
width = 280
height = 16

[[node]]
kind = "Label"
name = "time_label"
left = 16
top = 92
width = 280

[[node]]
kind = "Timer"
name = "countdown_timer"
left = 16
top = 130
interval = 1000
```

Script:

```rhai
fn form_load() {
    form.state.remaining = 0;
}

fn start_button_click() {
    form.state.remaining = seconds_field.value.to_int();
    countdown_bar.max = form.state.remaining;
    show_remaining();
    start_button.enabled = false;
    countdown_timer.enabled = true;
}

fn countdown_timer_tick() {
    form.state.remaining -= 1;
    show_remaining();
    if form.state.remaining <= 0 {
        countdown_timer.enabled = false;
        start_button.enabled = true;
        msg_box("Time is up!", "Countdown");
    }
}

fn show_remaining() {
    countdown_bar.value = form.state.remaining;
    time_label.text = `${form.state.remaining} s left`;
}
```

A `ProgressBar` takes integers, hence `.to_int()` on the number field's float.
Turn the timer off from its own handler when the work is done.

### 23. A stopwatch

Goal: Start, Stop and Reset with tenths of a second.

Form (window 280 x 150):

```toml
[[node]]
kind = "Label"
name = "time_label"
left = 16
top = 16
width = 240
height = 28
text = "0.0 s"

[[node]]
kind = "Button"
name = "run_button"
left = 16
top = 56
width = 100
height = 32
text = "Start"

[[node]]
kind = "Button"
name = "reset_button"
left = 124
top = 56
width = 100
height = 32
text = "Reset"

[[node]]
kind = "Timer"
name = "watch_timer"
left = 16
top = 100
interval = 100
```

Script:

```rhai
fn form_load() {
    form.state.ticks = 0;
}

fn run_button_click() {
    watch_timer.enabled = !watch_timer.enabled;
    run_button.text = if watch_timer.enabled { "Stop" } else { "Start" };
}

fn reset_button_click() {
    form.state.ticks = 0;
    show_ticks();
}

fn watch_timer_tick() {
    form.state.ticks += 1;
    show_ticks();
}

fn show_ticks() {
    let ticks = form.state.ticks;
    time_label.text = `${ticks / 10}.${ticks % 10} s`;
}
```

This counts ticks, so it drifts if the window is busy. For exact time, accumulate
the `dt` a `Canvas` `Frame` handler receives (next recipe).

### 24. A frame loop: a bouncing ball

Goal: animate a shape at the same speed whatever the frame rate.

Form (window 340 x 230):

```toml
[[node]]
kind = "Canvas"
name = "stage"
left = 10
top = 10
width = 320
height = 200
background = "#101018"
fps = 60
```

Script:

```rhai
let x = 40.0;
let y = 40.0;
let vx = 150.0;                       // pixels per second
let vy = 90.0;

fn stage_frame(dt) {
    x += vx * dt;
    y += vy * dt;
    if x < 10.0 || x > 310.0 { vx = -vx; x = clamp(x, 10.0, 310.0); }
    if y < 10.0 || y > 190.0 { vy = -vy; y = clamp(y, 10.0, 190.0); }
    stage.clear("#101018");           // redraw the whole scene every frame
    stage.fill_circle(x, y, 10, rgb(255, 200 - (y / 2.0).to_int(), 60));
}
```

`dt` is the seconds since the last frame (never more than 0.1), so movement is
speed times `dt`. A frame handler that raises an error stops the loop and the
canvas's `fps` becomes 0; set it again to restart.

### 25. Keyboard and mouse on a canvas

Goal: move a square with the arrow keys or WASD, drop it with a click, and draw a
crosshair at the pointer.

Form (window 340 x 230):

```toml
[[node]]
kind = "Canvas"
name = "arena"
left = 10
top = 10
width = 320
height = 200
background = "#1b2030"
fps = 60
```

Script:

```rhai
let px = 150.0;
let py = 90.0;
const SPEED = 160.0;

fn form_load() {
    arena.focus();
}

fn arena_frame(dt) {
    if arena.is_key_down("left") || arena.is_key_down("a") { px -= SPEED * dt; }
    if arena.is_key_down("right") || arena.is_key_down("d") { px += SPEED * dt; }
    if arena.is_key_down("up") || arena.is_key_down("w") { py -= SPEED * dt; }
    if arena.is_key_down("down") || arena.is_key_down("s") { py += SPEED * dt; }
    px = clamp(px, 0.0, 300.0);
    py = clamp(py, 0.0, 180.0);

    arena.clear("#1b2030");
    arena.fill_rect(px, py, 20, 20, "#ffd166");
    let mx = arena.mouse_x;
    let my = arena.mouse_y;
    arena.line(mx - 8, my, mx + 8, my, "#ffffff", 1);
    arena.line(mx, my - 8, mx, my + 8, "#ffffff", 1);
}

fn arena_mouse_down(x, y, button) {
    if button == "left" {
        px = x - 10.0;
        py = y - 10.0;
    }
}
```

`is_key_down` is the tool for movement (true while the key is held); `KeyDown` is
for one-off actions. A click on another control takes the keyboard focus away, so
end that control's handler with `arena.focus()`.

### 26. Draw shapes and centered text

Goal: paint a small scene once, with text centered by measuring it.

Form (window 360 x 250):

```toml
[[node]]
kind = "Canvas"
name = "scene"
left = 10
top = 10
width = 340
height = 220
```

Script:

```rhai
fn form_load() {
    scene.clear("#0b1020");
    scene.fill_rect(0, 160, 340, 60, "#2d6a4f");              // ground
    scene.fill_circle(280, 50, 24, "#ffd166");                // sun
    scene.stroke_circle(280, 50, 32, "#ffd166", 2);           // its halo
    scene.fill_round_rect(60, 100, 110, 70, 8, "#9c6644");    // house: radius before color
    scene.line(50, 102, 115, 60, "#7f5539", 6);               // roof
    scene.line(115, 60, 180, 102, "#7f5539", 6);
    scene.stroke_rect(100, 130, 30, 40, "#ffffff", 2);        // door
    centered("My house", 12, 22, "#ffffff");
}

fn centered(message, y, size, color) {
    let w = scene.text_width(message, size);
    scene.text((scene.width - w) / 2.0, y, message, color, size);
}
```

Colors are `"#rrggbb"` text or the integer `rgb(r, g, b)` returns. Drawing methods
queue commands that the canvas replays on each paint. `scene.width` is the
designed width, not the size after a resize.

### 27. A paint program

Goal: draw with the mouse in a chosen color, and clear the page.

Form (window 380 x 300):

```toml
[[node]]
kind = "Canvas"
name = "paint_canvas"
left = 10
top = 10
width = 360
height = 220
background = "#ffffff"

[[node]]
kind = "Button"
name = "black_button"
left = 10
top = 244
width = 70
height = 28
text = "Black"

[[node]]
kind = "Button"
name = "red_button"
left = 88
top = 244
width = 70
height = 28
text = "Red"

[[node]]
kind = "Button"
name = "blue_button"
left = 166
top = 244
width = 70
height = 28
text = "Blue"

[[node]]
kind = "Button"
name = "clear_button"
left = 300
top = 244
width = 70
height = 28
text = "Clear"
```

Script:

```rhai
let pen = "#000000";
let drawing = false;
let last_x = 0.0;
let last_y = 0.0;

fn paint_canvas_mouse_down(x, y, button) {
    if button == "left" {
        drawing = true;
        last_x = x;
        last_y = y;
    }
}

fn paint_canvas_mouse_move(x, y) {
    if drawing {
        paint_canvas.line(last_x, last_y, x, y, pen, 3);
        last_x = x;
        last_y = y;
    }
}

fn paint_canvas_mouse_up(x, y, button) {
    drawing = false;
}

fn black_button_click() { pen = "#000000"; }
fn red_button_click() { pen = "#cc2222"; }
fn blue_button_click() { pen = "#2244cc"; }

fn clear_button_click() {
    paint_canvas.clear("#ffffff");
}
```

Pressing a button captures the pointer, so `MouseMove` and `MouseUp` keep arriving
when the pointer leaves the canvas. The canvas holds at most 100,000 drawing
commands; a drawing that long needs a `clear` and a repaint from your own data.

## Files

### 28. Save and load a text file

Goal: store the text of an editor in the app's data folder.

Form (window 360 x 270):

```toml
[[node]]
kind = "MultilineEdit"
name = "notes_edit"
left = 12
top = 12
width = 336
height = 170

[[node]]
kind = "Button"
name = "save_button"
left = 12
top = 192
width = 100
height = 30
text = "Save"

[[node]]
kind = "Button"
name = "load_button"
left = 120
top = 192
width = 100
height = 30
text = "Load"

[[node]]
kind = "Label"
name = "status_label"
left = 12
top = 232
width = 336
```

Script:

```rhai
fn save_button_click() {
    try {
        file_write_text("notes.txt", notes_edit.text);
        status_label.text = "Saved notes.txt";
    } catch (error) {
        status_label.text = `Could not save: ${error}`;
    }
}

fn load_button_click() {
    if !file_exists("notes.txt") {
        status_label.text = "There is no notes.txt yet";
        return;
    }
    try {
        notes_edit.text = file_read_text("notes.txt");
        status_label.text = "Loaded notes.txt";
    } catch (error) {
        status_label.text = `Could not load: ${error}`;
    }
}
```

A relative path means the app's private folder: `~/.apps/<system_name>/notes.txt`
for an installed app, `~/.apps/os.lazy.lazyrad/data/notes.txt` when you run the
project from the IDE ([The standard library](stdlib.md)). Any path outside it fails
with `path is outside the sandbox`, which the `try` shows.

### 29. Remember settings between runs

Goal: restore the form's choices at start and write them back when they change.

Form (window 320 x 200):

```toml
[[node]]
kind = "Edit"
name = "name_edit"
left = 16
top = 16
width = 200
cue = "Your name"

[[node]]
kind = "CheckBox"
name = "dark_check"
left = 16
top = 52
width = 160
text = "Use dark colors"

[[node]]
kind = "ComboBox"
name = "size_combo"
left = 16
top = 88
width = 160
items = ["Small", "Medium", "Large"]

[[node]]
kind = "Label"
name = "status_label"
left = 16
top = 130
width = 280
```

Script:

```rhai
const SETTINGS_FILE = "settings.txt";

fn form_load() {
    let settings = read_settings();
    if "name" in settings { name_edit.text = settings.name; }
    if "dark" in settings { dark_check.checked = settings.dark == "yes"; }
    if "size" in settings { size_combo.selected = to_index(settings.size); }
}

fn name_edit_change(text) { save_settings(); }
fn dark_check_toggle(checked) { save_settings(); }
fn size_combo_select(index) { save_settings(); }

// settings.txt holds one `key=value` per line.
fn read_settings() {
    let settings = #{};
    if file_exists(SETTINGS_FILE) {
        for line in file_read_text(SETTINGS_FILE).split("\n") {
            let at = line.index_of("=");
            if at > 0 {
                settings[line.sub_string(0, at)] = line.sub_string(at + 1);
            }
        }
    }
    settings
}

fn save_settings() {
    let dark = if dark_check.checked { "yes" } else { "no" };
    let text = "name=" + name_edit.text + "\n" + "dark=" + dark + "\n" + "size=" + size_combo.selected + "\n";
    try {
        file_write_text(SETTINGS_FILE, text);
        status_label.text = "Settings saved";
    } catch (error) {
        status_label.text = `Settings not saved: ${error}`;
    }
}

fn to_index(text) {
    try { return parse_int(text); } catch { return 0; }
}
```

Setting `text`, `checked` or `selected` from `form_load` raises no event, so
restoring does not write the file back. Save on change rather than in `form_close`:
that runs only when the user closes the window, not on `app.quit()`. The text is
built with ordinary strings because `\n` is not an escape inside a backtick
string: there it stays a backslash and an `n`.

### 30. Append to a log file

Goal: add a timestamped line each time something happens, and show the log.

Form (window 340 x 240):

```toml
[[node]]
kind = "Button"
name = "log_button"
left = 12
top = 12
width = 120
height = 30
text = "Log a line"

[[node]]
kind = "MultilineEdit"
name = "log_edit"
left = 12
top = 52
width = 316
height = 150
```

Script:

```rhai
fn form_load() {
    show_log();
}

fn log_button_click() {
    try {
        file_append_text("activity.log", now() + " button clicked\n");
    } catch (error) {
        log_edit.text = `${error}`;
        return;
    }
    show_log();
}

fn show_log() {
    if file_exists("activity.log") {
        log_edit.text = file_read_text("activity.log");
    }
}
```

`file_append_text` creates the file when it is missing. Timestamps are UTC on
LazyOS.

### 31. List a folder and create one

Goal: show the entries of a folder in the data directory and create subfolders.

Form (window 320 x 300):

```toml
[[node]]
kind = "Edit"
name = "folder_edit"
left = 12
top = 12
width = 190
cue = "New folder name"

[[node]]
kind = "Button"
name = "create_button"
left = 210
top = 10
width = 90
height = 28
text = "Create"

[[node]]
kind = "ListView"
name = "entry_list"
left = 12
top = 48
width = 288
height = 190

[[node]]
kind = "Label"
name = "status_label"
left = 12
top = 250
width = 288
```

Script:

```rhai
fn form_load() {
    refresh();
}

fn create_button_click() {
    let name = folder_edit.text;
    if name == "" { return; }
    try {
        dir_create(`notes/${name}`);        // also creates `notes` itself
        folder_edit.text = "";
    } catch (error) {
        status_label.text = `${error}`;
        return;
    }
    refresh();
}

fn refresh() {
    try {
        dir_create("notes");
        entry_list.items = dir_list("notes");     // names only, sorted
        status_label.text = `${entry_list.items.len()} entries in notes`;
    } catch (error) {
        status_label.text = `${error}`;
    }
}
```

`dir_list` returns names, not paths, and does not say which are folders; there is
no file size or rename either. On LazyOS the sandbox refuses a name such as `../x`
that would leave the data folder.

### 32. Open the document the app was started with

Goal: show the text file a user double-clicked in Files.

Form (window 360 x 260):

```toml
[[node]]
kind = "Label"
name = "path_label"
left = 12
top = 12
width = 336

[[node]]
kind = "MultilineEdit"
name = "text_edit"
left = 12
top = 40
width = 336
height = 170
```

Script:

```rhai
fn form_load() {
    if app.documents.len() == 0 {
        path_label.text = "No document: open a file from Files";
        return;
    }
    let path = app.documents[0];
    path_label.text = path;
    try {
        text_edit.text = file_read_text(path);
    } catch (error) {
        text_edit.text = `Cannot read this file: ${error}`;
    }
}
```

`app.documents` holds the files the player was started to open, as absolute paths
(at most 64). They and the files beside them are readable but never writable; to
save a changed copy write to the data folder with a relative path. From the
Terminal, `lrplay --client <project> /home/user/a.txt` passes a document
([Packaging](packaging.md)).

### 33. Read a data file that ships with the project

Goal: a "quote of the day" button that reads a file shipped inside the app.

Project: list the file in the `.lrp` `assets` ([Projects](projects.md)):

```toml
assets = ["data/*.txt"]
```

File `data/quotes.txt`:

```text
Make it work, then make it right.
Simple is better than complex.
Name things after what they do.
```

Form (window 360 x 120):

```toml
[[node]]
kind = "Label"
name = "quote_label"
left = 12
top = 12
width = 336
height = 28

[[node]]
kind = "Button"
name = "next_button"
left = 12
top = 52
width = 120
height = 30
text = "Another"
```

Script:

```rhai
let quotes = [];

fn form_load() {
    quotes = file_read_text("data/quotes.txt").split("\n");
    quotes = quotes.filter(|q| q != "");      // a final newline leaves an empty entry
    show_quote();
}

fn next_button_click() {
    show_quote();
}

fn show_quote() {
    quote_label.text = quotes[random_range(0, quotes.len())];   // end is excluded
}
```

A relative read looks in the project first (its items and `assets`), so the same
path works from the IDE and from an installed app. Assets are read-only; write to
the data folder instead.

## Dialogs

### 34. A message box and its answer

Goal: tell the user something, and ask a yes or no question.

Form (window 320 x 140):

```toml
[[node]]
kind = "Button"
name = "info_button"
left = 16
top = 16
width = 120
height = 32
text = "Show message"

[[node]]
kind = "Button"
name = "quit_button"
left = 150
top = 16
width = 120
height = 32
text = "Quit..."

[[node]]
kind = "Label"
name = "status_label"
left = 16
top = 70
width = 280
```

Script:

```rhai
fn info_button_click() {
    msg_box("The file was saved.", "Saved");
}

fn quit_button_click() {
    msg_box("Quit without saving?", "Quit", "yes_no", |answer| {
        if answer == "yes" {
            app.quit();
        } else {
            status_label.text = "Still here";
        }
    });
    status_label.text = "Waiting for your answer";   // runs BEFORE the answer arrives
}
```

A message box never blocks the script: the call returns at once and the answer
(`"ok"`, `"cancel"`, `"yes"` or `"no"`) arrives later, in a callback that takes
exactly one parameter. Put what depends on the answer inside the callback.

### 35. Confirm before deleting a row

Goal: ask first, then remove the row that was selected when the button was pressed.

Form (window 300 x 240):

```toml
[[node]]
kind = "ListView"
name = "item_list"
left = 12
top = 12
width = 180
height = 150
items = ["alpha", "beta", "gamma"]

[[node]]
kind = "Button"
name = "delete_button"
left = 200
top = 12
width = 80
height = 28
text = "Delete"
```

Script:

```rhai
fn delete_button_click() {
    let index = item_list.selected;
    if index < 0 {
        return;
    }
    let label = item_list.items[index];
    msg_box(`Delete "${label}"?`, "Confirm", "yes_no", |answer| {
        if answer == "yes" {
            let items = item_list.items;
            items.remove(index);              // `index` was captured when the box opened
            item_list.items = items;
        }
    });
}
```

The closure captures `index` by value. If the user could change the selection
while the box is open, that is the row that is deleted, which is what the question
asked.

### 36. Ask for a file, and handle "no"

Goal: let the user pick a file, and cope with a cancel or with no dialog.

Form (window 360 x 120):

```toml
[[node]]
kind = "Button"
name = "open_button"
left = 12
top = 12
width = 120
height = 30
text = "Open..."

[[node]]
kind = "Label"
name = "path_label"
left = 12
top = 56
width = 336
```

Script:

```rhai
fn open_button_click() {
    open_file_dialog("Open a text file", "Text|*.txt,*.md;All files|*.*", |path| {
        if type_of(path) == "string" {
            path_label.text = path;
        } else {
            path_label.text = "No file chosen";
        }
    });
}
```

The callback gets the path, or `()` for a cancel. On LazyOS the player installs
no file-dialog backend (from its source; not confirmed on a running guest), so the
answer may be `()` at once: always handle it, and let the app also take a file from
`app.documents` (recipe 32) or its data folder.

## Pictures

### 37. Show a picture from a file

Goal: load a PNG, JPEG, BMP or GIF into a `PictureBox` and say what it is.

Form (window 420 x 330):

```toml
[[node]]
kind = "Edit"
name = "path_edit"
left = 10
top = 10
width = 300

[[node]]
kind = "Button"
name = "show_button"
left = 318
top = 8
width = 90
height = 28
text = "Show"

[[node]]
kind = "PictureBox"
name = "picture1"
left = 10
top = 44
width = 398
height = 240
anchor = "fill"
background = "#202020"

[[node]]
kind = "Label"
name = "status_label"
left = 10
top = 294
width = 398
anchor = "bottom_left"
```

Script:

```rhai
fn form_load() {
    if app.documents.len() > 0 {
        path_edit.text = app.documents[0];
        show_picture(app.documents[0]);
    }
}

fn show_button_click() {
    show_picture(path_edit.text);
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

The control never opens a file: the script reads the bytes (through the sandbox)
and hands them over. A path outside the data folder, the project and
`app.documents` is refused; a file that is not a picture, or is damaged, is
reported by the `catch`.

### 38. Zoom, rotate and save the turned picture

Goal: toolbar buttons and the mouse wheel to zoom and turn, and write the result
as a PNG.

Form (window 440 x 350):

```toml
[[node]]
kind = "Button"
name = "zoom_in_button"
left = 10
top = 8
width = 70
height = 28
text = "Zoom in"

[[node]]
kind = "Button"
name = "zoom_out_button"
left = 86
top = 8
width = 76
height = 28
text = "Zoom out"

[[node]]
kind = "Button"
name = "fit_button"
left = 168
top = 8
width = 60
height = 28
text = "Fit"

[[node]]
kind = "Button"
name = "turn_button"
left = 234
top = 8
width = 70
height = 28
text = "Turn"

[[node]]
kind = "Button"
name = "save_button"
left = 310
top = 8
width = 120
height = 28
text = "Save turned"

[[node]]
kind = "PictureBox"
name = "picture1"
left = 10
top = 44
width = 418
height = 260
anchor = "fill"

[[node]]
kind = "Label"
name = "status_label"
left = 10
top = 314
width = 418
anchor = "bottom_left"
```

Script:

```rhai
fn form_load() {
    if app.documents.len() > 0 {
        try {
            picture1.load(file_read_bytes(app.documents[0]));
            picture1.best_fit();
        } catch (error) {
            status_label.text = `${error}`;
        }
    }
    picture1.focus();
    update_status();
}

fn zoom_in_button_click() { picture1.zoom_in(); update_status(); }
fn zoom_out_button_click() { picture1.zoom_out(); update_status(); }
fn fit_button_click() { picture1.best_fit(); update_status(); }
fn turn_button_click() { picture1.rotate_cw(); update_status(); }

// The wheel zooms with Ctrl held; the control does not zoom by itself.
fn picture1_wheel(delta, ctrl) {
    if ctrl {
        if delta > 0 { picture1.zoom_in(); } else { picture1.zoom_out(); }
        update_status();
    }
}

fn save_button_click() {
    try {
        file_write_bytes("turned.png", picture1.to_png());   // as shown, rotation included
        status_label.text = "Saved turned.png in the data folder";
    } catch (error) {
        status_label.text = `Could not save: ${error}`;
    }
}

fn update_status() {
    status_label.text = `${picture1.zoom.round().to_int()}%  turned ${picture1.rotation} degrees`;
}
```

Never name your own function like a `PictureBox` method with the same number of
arguments (`fn best_fit()`, `fn rotate_cw()`): Rhai calls it for
`picture1.best_fit()` too and it recurses. `to_png` is an error when no picture is
loaded, and rotating changes only the view, never the original file.

## LazyOS integration

These recipes use `msg` and `sys::*`, which exist only inside the LazyOS player
(`lrplay`). The IDE's check may warn that they name modules the project does not
define; it is only a warning ([LazyOS extras](lazyos.md)). Each recipe lists the
interface Make LazyOS App will declare for it. Function names and arguments are in
the [Messenger API reference](../rhai/api/index.md).

### 39. Read a setting from confd

Needs `os.lazy.confd.v1`. Goal: show the current theme.

Form (window 340 x 100):

```toml
[[node]]
kind = "Label"
name = "theme_label"
left = 16
top = 16
width = 300
```

Script:

```rhai
fn form_load() {
    theme_label.text = `theme: ${conf_text("sys/ui/theme", "(unset)")}`;
}

// The string at `path`, or `fallback` when the key is absent or not a string.
fn conf_text(path, fallback) {
    let value = sys::confd::get(path);
    if value == () || value.kind != 3 { fallback } else { value.str_value }
}
```

`get` returns `()` for a key that is not set. A value is a map whose `kind` says
which field holds it: 0 bool, 1 i64, 2 u64, 3 string, 4 bytes
([`sys::confd`](../rhai/api/confd.md)). Any session may read `sys/**`.

### 40. React when a setting changes

Needs `os.lazy.confd.v1` and `subscribe:system/confd/changed/sys/ui/#`. Goal:
update the form when the theme key changes.

Form (window 340 x 120):

```toml
[[node]]
kind = "Label"
name = "theme_label"
left = 16
top = 16
width = 300

[[node]]
kind = "Label"
name = "change_label"
left = 16
top = 44
width = 300
text = "waiting for changes"
```

Script:

```rhai
fn form_load() {
    show_theme();
    sys::confd::on_changed("sys/ui/#", Fn("ui_changed"));
}

fn ui_changed(event) {
    change_label.text = `changed: ${event.payload.path}`;
    if event.payload.path == "sys/ui/theme" {
        show_theme();                 // the event carries the path, never the value
    }
}

fn show_theme() {
    let value = sys::confd::get("sys/ui/theme");
    theme_label.text = if value == () { "theme: (unset)" } else { `theme: ${value.str_value}` };
}
```

The handler runs on the form's window, between window events; a form needs no
`msg::run()` loop (it is an error there). Literal arguments narrow the topic rule
an installed app is granted: `"sys/ui/#"` declares only that subtree.

### 41. Write a setting, and cope with the refusal

Needs `os.lazy.confd.v1`. Goal: try to write a key and report the result.

Form (window 340 x 120):

```toml
[[node]]
kind = "Button"
name = "set_button"
left = 16
top = 16
width = 220
height = 32
text = "Set sys/lazyrad/demo"

[[node]]
kind = "Label"
name = "status_label"
left = 16
top = 64
width = 300
```

Script:

```rhai
fn set_button_click() {
    let value = sys::confd::new_value();       // every field at its zero value
    value.kind = 3;
    value.str_value = `clicked at ${now()}`;
    try {
        sys::confd::set("sys/lazyrad/demo", value);
        status_label.text = "Set sys/lazyrad/demo";
    } catch (error) {
        status_label.text = `${error}`;        // sys/ is writable by an administrator only
    }
}
```

A session user cannot write `sys/**`; the call fails and the `catch` shows the
service's reason. For a setting that belongs to the app, a file in its data folder
(recipe 29) needs no permission at all. Per-user keys live under `user/<uid>/**`;
the [Rhai cookbook](../rhai/cookbook.md) shows how to find your uid.

### 42. Show a notification toast

Needs `os.lazy.notify.v1`. Goal: post a toast, then update the same toast.

Form (window 320 x 140):

```toml
[[node]]
kind = "Button"
name = "notify_button"
left = 16
top = 16
width = 120
height = 32
text = "Notify"

[[node]]
kind = "Button"
name = "update_button"
left = 150
top = 16
width = 120
height = 32
text = "Update it"

[[node]]
kind = "Label"
name = "status_label"
left = 16
top = 70
width = 280
```

Script:

```rhai
fn form_load() {
    form.state.toast = ();
}

fn notify_button_click() {
    try {
        form.state.toast = sys::notify::notify(sys::notify::LEVEL_INFO, "Backup",
            "Copying 12 files...", (), [], 0, (), ());
        status_label.text = `posted toast ${form.state.toast}`;
    } catch (error) {
        status_label.text = `${error}`;
    }
}

fn update_button_click() {
    try {
        // replaces_id names the live toast, which is updated in place.
        sys::notify::notify(sys::notify::LEVEL_SUCCESS, "Backup", "Done: 12 files",
            (), [], (), form.state.toast, ());
    } catch (error) {
        status_label.text = `${error}`;
    }
}
```

The arguments are `level, title, body, icon, actions, progress, replaces_id,
timeout`; `()` means "none". The first toast passes `progress` 0, which shows a
progress bar and keeps the toast on screen until it is updated; the second replaces
it by id with a Success toast. The title is at most 64 characters and the body 256.
A toast never takes keyboard focus, and a missed toast is gone, so never make a
toast the only way to learn something.

### 43. Open a file or a link through mimed

Needs `os.lazy.mimed.v1`. Goal: hand a path or URL to whichever app handles it.

Form (window 360 x 170):

```toml
[[node]]
kind = "Edit"
name = "target_edit"
left = 12
top = 12
width = 250
text = "http://example.com/"

[[node]]
kind = "Button"
name = "open_button"
left = 270
top = 10
width = 80
height = 28
text = "Open"

[[node]]
kind = "Hyperlink"
name = "help_link"
left = 12
top = 52
width = 160
text = "Open the home page"

[[node]]
kind = "Label"
name = "status_label"
left = 12
top = 84
width = 336
```

Script:

```rhai
fn open_target(target) {
    try {
        let result = sys::mimed::open(target, "open");
        status_label.text = if result.launched {
            `opened with ${result.app} (${result.mime})`
        } else {
            `${result.app} did not start`
        };
    } catch (error) {
        status_label.text = `${error}`;
    }
}

fn open_button_click() {
    open_target(target_edit.text);
}

fn help_link_click() {
    open_target("http://example.com/");
}
```

`open` guesses the type, resolves the app for the verb, asks `init` to start it
and reports whether it did. A `Hyperlink` has no address of its own: its `Click`
handler decides what the link does.

### 44. Launch another app

Needs `os.lazy.init.v1`. Goal: list the apps the system knows and start one.

Form (window 340 x 300):

```toml
[[node]]
kind = "ListView"
name = "app_list"
left = 12
top = 12
width = 220
height = 220

[[node]]
kind = "Button"
name = "launch_button"
left = 242
top = 12
width = 80
height = 28
text = "Launch"

[[node]]
kind = "Label"
name = "status_label"
left = 12
top = 244
width = 310
```

Script:

```rhai
fn form_load() {
    form.state.ids = [];
    let names = [];
    try {
        for entry in sys::init::list_apps() {
            if !entry.hidden {
                names.push(entry.name);
                form.state.ids.push(entry.id);
            }
        }
    } catch (error) {
        status_label.text = `${error}`;
    }
    app_list.items = names;
    app_list.selected = -1;
}

fn launch_button_click() {
    let index = app_list.selected;
    if index < 0 {
        return;
    }
    try {
        let started = sys::init::launch(form.state.ids[index], "", 0);
        status_label.text = if started.existing { "already running" } else { `started, pid ${started.pid}` };
    } catch (error) {
        status_label.text = `${error}`;
    }
}
```

`launch(app, args, session)` takes the app id; `args` is empty or one absolute
path; session 0 is your own. The loop variable is `entry` because `app` is the
global that describes this program. A resident app that is already running is
reopened, not started twice.

### 45. Serve a method to other programs

Needs nothing when installed (the call is refused there). Goal: answer
`os.lazy.echo.v1` calls from the Terminal while the form is open.

Form (window 340 x 100):

```toml
[[node]]
kind = "Label"
name = "served_label"
left = 16
top = 16
width = 300
text = "not serving yet"
```

Script:

```rhai
fn form_load() {
    form.state.calls = 0;
    try {
        msg::serve("demo.lazyrad", "os.lazy.echo.v1", #{
            Echo: |text, count| {
                form.state.calls += 1;
                served_label.text = `Echo("${text}", ${count}), call ${form.state.calls}`;
                let reply = "";
                for i in 0..count {
                    reply += text;
                }
                reply
            },
            Ping: || true,
        });
        served_label.text = "serving demo.lazyrad";
    } catch (error) {
        served_label.text = `not serving: ${error}`;
    }
}
```

From the Terminal, `rhai -e 'msg::connect("os.lazy.echo.v1", "demo.lazyrad").echo("hi", 2)'`
answers `hihi`. Serving a name is a development feature: an installed app owns
only names beginning `app.<system_name>.`, so the call is refused there, and an
error escaping `form_load` would end the app. That is why it sits in a `try`.

### 46. Live system numbers from sysmond

Needs `subscribe` on the `system/stats/memory` and `system/stats/tasks` topics
(no method call, so no interface of its own). Goal: show memory counters and the
task list without polling.

Form (window 360 x 300):

```toml
[[node]]
kind = "Label"
name = "memory_label"
left = 12
top = 12
width = 336

[[node]]
kind = "Label"
name = "tasks_label"
left = 12
top = 38
width = 336

[[node]]
kind = "Button"
name = "show_button"
left = 12
top = 66
width = 140
height = 28
text = "Show the tasks"

[[node]]
kind = "ListView"
name = "task_list"
left = 12
top = 102
width = 336
height = 180
```

Script:

```rhai
fn form_load() {
    form.state.tasks = [];
    try {
        sys::sysmond::on_stats_memory(|event| {
            let m = event.payload;
            memory_label.text = `frames: ${m.frames_live} in use, ${m.frames_free} free of ${m.frames_total}`;
        });
        sys::sysmond::on_stats_tasks(|event| {
            tasks_label.text = `${event.payload.live} live tasks`;
            form.state.tasks = event.payload.tasks;
        });
    } catch (error) {
        memory_label.text = `${error}`;
    }
}

fn show_button_click() {
    let rows = [];
    for task in form.state.tasks {
        rows.push(`${task.pid}  ${task.name}  ${task.state}`);
    }
    task_list.items = rows;
}
```

Both topics are retained, so the first values arrive as soon as you subscribe.
The handlers only store the latest table; the button copies it into the list,
because replacing a `ListView`'s `items` on every update would scroll it back to
the top.

### 47. Service health from healthd

Needs `os.lazy.healthd.v1`. Goal: list each service and its status.

Form (window 340 x 300):

```toml
[[node]]
kind = "Button"
name = "refresh_button"
left = 12
top = 12
width = 100
height = 28
text = "Refresh"

[[node]]
kind = "Label"
name = "summary_label"
left = 12
top = 48
width = 316

[[node]]
kind = "ListView"
name = "health_list"
left = 12
top = 78
width = 316
height = 200
```

Script:

```rhai
fn form_load() {
    refresh_button_click();
}

fn refresh_button_click() {
    try {
        let health = sys::healthd::status();
        summary_label.text = `overall: ${health.summary.status}`;
        let rows = [];
        for record in health.records {
            rows.push(`${record.name}: ${record.status}`);
        }
        health_list.items = rows;
    } catch (error) {
        summary_label.text = `${error}`;
    }
}
```

`status()` returns a map with `summary` (the aggregate row) and `records`, one row
per service, each with `name`, `status` (`ok`, `degraded` or `down`), `detail` and
`tick`. A handler can be called like any function, as `form_load` does here.

### 48. Play a song with modplay

Needs `os.lazy.audio.v1` (declared by the text `modplay::play`). Goal: play a
ProTracker module shipped with the app, with pause and level meters.

Project: ship the song and list it as an asset:

```toml
assets = ["music/*.mod"]
```

Form (window 340 x 230):

```toml
[[node]]
kind = "Label"
name = "title_label"
left = 12
top = 12
width = 316

[[node]]
kind = "Button"
name = "play_button"
left = 12
top = 40
width = 100
height = 30
text = "Play"

[[node]]
kind = "ProgressBar"
name = "bar_1"
left = 12
top = 90
width = 316
max = 64

[[node]]
kind = "ProgressBar"
name = "bar_2"
left = 12
top = 108
width = 316
max = 64

[[node]]
kind = "ProgressBar"
name = "bar_3"
left = 12
top = 126
width = 316
max = 64

[[node]]
kind = "ProgressBar"
name = "bar_4"
left = 12
top = 144
width = 316
max = 64

[[node]]
kind = "Label"
name = "status_label"
left = 12
top = 170
width = 316
```

Script:

```rhai
let deck = ();
let started = false;

fn play_button_click() {
    if !started {
        start_song();
    } else if deck.playing {
        deck.pause();
        play_button.text = "Resume";
    } else {
        deck.resume();
        play_button.text = "Pause";
    }
}

fn start_song() {
    try {
        let song = modplay::load(app.path + "/music/tune.mod");   // a song shipped in the project
        title_label.text = song.title;
        deck = modplay::play(song, #{ volume: 80 });
        deck.on_update(Fn("show_deck"));
        deck.on_end(Fn("deck_ended"));
        started = true;
        play_button.text = "Pause";
        status_label.text = if modplay::sound_available() { "playing" } else { "no sound card: playing silently" };
    } catch (error) {
        status_label.text = `${error}`;
    }
}

fn show_deck(d) {
    bar_1.value = d.levels[0];
    bar_2.value = d.levels[1];
    bar_3.value = d.levels[2];
    bar_4.value = d.levels[3];
}

fn deck_ended(d) {
    started = false;
    play_button.text = "Play";
    status_label.text = "finished";
}
```

`modplay::load` reads through the sandbox but not through the project's file
view, so a song shipped with the app is addressed from `app.path`. `levels` are
`0..=64` per channel. A handler passed to `on_update` or `on_end` takes exactly one
parameter, the deck, and `on_update` runs at most every 80 ms. See
[the MOD player](../lazyrad-modplay.md).

## Packaging

### 49. What permissions will my app ask for?

Goal: know in advance what the consent screen will list.

Make LazyOS App reads the text of every script and declares what it finds
([Packaging](packaging.md)). Results for typical calls, checked against the
platform's own derivation:

| The script contains | The package declares |
|---|---|
| `sys::confd::get("sys/ui/theme")` | `os.lazy.confd.v1` |
| `sys::confd::on_changed("sys/ui/#", Fn("f"))` | `subscribe:system/confd/changed/sys/ui/#`; `os.lazy.confd.v1` too once the script also calls `get`, `set`, `list` or `delete` |
| `sys::sysmond::on_stats_memory(f)` | `subscribe:system/stats/memory` |
| `sys::confd::on_changed(path, Fn("f"))` with a computed `path` | the bare pattern `subscribe:system/confd/changed/#`, which the kernel refuses past its wildcard |
| `sys::notify::notify(...)` | `os.lazy.notify.v1` |
| `sys::mimed::open(...)` | `os.lazy.mimed.v1` |
| `msg::connect("os.lazy.echo.v1")` | `os.lazy.echo.v1` |
| `msg::connect(name_edit.text)` | nothing: a name built at run time cannot be seen, and the call is refused with `permission denied (EACCES)` |
| `modplay::play(song)` | `os.lazy.audio.v1` |
| `file_write_text("notes.txt", t)` or any `file_*` or `dir_*` call | a `files` rule: read and write `$HOME/.apps/<system_name>` |
| every app | `os.lazy.display.v1`, `os.lazy.input.v1`, `os.lazy.display.cursor.v1` |

A string literal passed to `msg::connect`, `msg::on`, `msg::subscribe` or
`msg::publish` is declared; a computed one is not. Nothing a script does declares
network access. Play in the IDE runs the project under the label the installed
app would get, so a refusal in Play is the refusal users would see.

### 50. Make a `.lzp` package

Goal: turn a project into an app in the Start menu.

The project file for a small app, with a version the package manager accepts
(`MAJOR`, `MAJOR.MINOR` or `MAJOR.MINOR.PATCH`, each part below 65536):

```toml
name = "notes"
version = "1.0.0"
startup = "main_form"
assets = ["data/*.txt"]

[[items]]
kind = "form"
name = "main_form"
layout = "main_form.lfm"
code = "main_form.rhai"
```

Then, in the IDE, **File > Make LazyOS App**: it saves, checks, builds the package,
shows what it asks for, and hands it to the Package Installer. Or from a host with
the LazyOS repository, and then on LazyOS:

```bash
python tools/lazyrad/package.py --project lazyrad-os/samples/hello --out hello.lzp --system-name user.me.hello
pkgctl inspect ~/hello.lzp      # what it declares and asks for; changes nothing
pkgctl install ~/hello.lzp
```

Expectations: a version installs once, so raise `version` in the `.lrp` before you
build a changed app; the Start menu files the app under its default category; the
data folder of the installed app is `~/.apps/<system_name>/`.

### 51. A notes app that keeps its text

Goal: a complete small app that autosaves and restores its text. This is the
pattern for any app that remembers something.

Form (window 360 x 270):

```toml
[[node]]
kind = "MultilineEdit"
name = "notes_edit"
left = 12
top = 12
width = 336
height = 200
anchor = "fill"

[[node]]
kind = "Label"
name = "status_label"
left = 12
top = 224
width = 336
anchor = "bottom_left"

[[node]]
kind = "Timer"
name = "save_timer"
left = 12
top = 240
interval = 2000
enabled = true
```

Script:

```rhai
let dirty = false;

fn form_load() {
    try {
        if file_exists("notes.txt") {
            notes_edit.text = file_read_text("notes.txt");
        }
    } catch (error) {
        status_label.text = `Could not read notes.txt: ${error}`;   // do not let form_load fail
    }
}

fn notes_edit_change(text) {
    dirty = true;
    status_label.text = "Unsaved changes";
}

fn save_timer_tick() {
    save_notes();
}

fn form_close() {
    save_notes();                     // best effort: runs only when the user closes the window
}

fn save_notes() {
    if !dirty {
        return;
    }
    try {
        file_write_text("notes.txt", notes_edit.text);
        dirty = false;
        status_label.text = `Saved at ${now()}`;
    } catch (error) {
        status_label.text = `Not saved: ${error}`;
    }
}
```

A timer batches the writes instead of writing on every keystroke. The file lands in
`~/.apps/<system_name>/notes.txt` for the installed app. The `files` permission is
declared for you because the script calls `file_*` functions. An error escaping
`form_load` stops the app before it shows a window, which is why the read is in a
`try`.

### 52. Start up even when a service is refused

Needs `os.lazy.confd.v1`. Goal: an installed app whose live theme feature fails
softly if the permission is missing.

Form (window 340 x 100):

```toml
[[node]]
kind = "Label"
name = "theme_label"
left = 12
top = 12
width = 316

[[node]]
kind = "Label"
name = "status_label"
left = 12
top = 40
width = 316
```

Script:

```rhai
fn form_load() {
    theme_label.text = "theme: unknown";
    try {
        let value = sys::confd::get("sys/ui/theme");
        if value != () {
            theme_label.text = `theme: ${value.str_value}`;
        }
        sys::confd::on_changed("sys/ui/#", Fn("ui_changed"));
    } catch (error) {
        status_label.text = `running without live theme updates: ${error}`;
    }
}

fn ui_changed(event) {
    theme_label.text = `theme key ${event.payload.path} changed`;
}
```

A script runs with the app's own permissions and nothing more; a call the package
did not declare fails with an error ending in `permission denied (EACCES)`. An
error that escapes the startup form's `form_load` ends the app (exit code 2), and
`init` shows its "stopped unexpectedly" notice with the reason. A `try` around the
optional parts keeps the rest alive.

## Debugging

### 53. Log with print and debug

Goal: see what a handler does while it runs.

Form (window 320 x 100):

```toml
[[node]]
kind = "Button"
name = "step_button"
left = 16
top = 16
width = 120
height = 32
text = "Step"

[[node]]
kind = "Label"
name = "status_label"
left = 16
top = 60
width = 280
```

Script:

```rhai
fn form_load() {
    form.state.step = 0;
}

fn step_button_click() {
    form.state.step += 1;
    print(`step ${form.state.step}`);          // one line in the IDE's Output pane
    debug(form.state);                         // the value's text, without a position
    status_label.text = `step ${form.state.step}`;
}
```

`print` and `debug` write one line each to the player's standard output, which the
IDE's Output pane shows as it happens. An installed app run from the Start menu has
no console you can see; show the value in a label, or append it to a log file as
in recipe 30.

### 54. Show an error where the user can read it

Goal: catch a failure and report it in the window instead of a message box.

Form (window 340 x 130):

```toml
[[node]]
kind = "Edit"
name = "name_edit"
left = 16
top = 16
width = 200

[[node]]
kind = "Button"
name = "run_button"
left = 16
top = 52
width = 120
height = 32
text = "Run"

[[node]]
kind = "Label"
name = "status_label"
left = 16
top = 96
width = 300
```

Script:

```rhai
fn run_button_click() {
    try {
        check_name(name_edit.text);
        let ratio = 100 / name_edit.text.len();     // fails on an empty name: division by zero
        status_label.text = `OK, ${ratio}`;
    } catch (error) {
        status_label.text = `Problem: ${describe(error)}`;
        print(`run failed: ${describe(error)}`);
    }
}

fn check_name(text) {
    if text.len() > 20 {
        throw "That name is too long";
    }
}

// The readable text of a caught error: a string for `throw "text"` and for
// errors from a service or a file function, a map for a language error.
fn describe(error) {
    if type_of(error) == "map" && "message" in error { error.message } else { error.to_string() }
}
```

An uncaught error in a handler opens a message box with the file, line and column,
and the program keeps running. `throw "text"` raises your own error and `catch`
gets the text. A runtime error of the language itself (division by zero, a missing
variable) arrives as a map with a `message`, and `${error}` would print the whole
map, hence `describe`.

### 55. Breakpoints and the Immediate window

Goal: stop a handler on a line, look at its variables and try a fix.

Form (window 320 x 200):

```toml
[[node]]
kind = "ListView"
name = "cart_list"
left = 12
top = 12
width = 160
height = 100
items = ["3", "4", "five"]

[[node]]
kind = "Button"
name = "total_button"
left = 180
top = 12
width = 120
height = 28
text = "Total"

[[node]]
kind = "Label"
name = "total_label"
left = 12
top = 124
width = 288
```

Script:

```rhai
fn total_button_click() {
    let sum = 0;
    for item in cart_list.items {
        sum += parse_int(item);       // press F9 here to set a breakpoint
    }
    total_label.text = `Total: ${sum}`;
}
```

Click **Total** under the debugger and the third row fails. To find out why:

1. Click on the `sum +=` line and press **F9** (a dot appears in the gutter).
2. Press **Ctrl+F5** (Start with Debugging) and click the button.
3. The program stops on the line and the **Locals** tab shows `item` and `sum`.
   **F8** steps into a call, **Shift+F8** steps over, **F5** continues.
4. In **Immediate** type `parse_int(item)` and press Evaluate: it runs in the stopped
   frame. Typing `item` shows `"five"`, which is not a number.
5. Fix the data, or guard the call with a `try` as in recipe 11.

A script error also stops at the failing line when the program runs under the
debugger. Breakpoints belong to the IDE session; they are not saved.

### 56. A handler that never runs

Goal: find out why a click does nothing.

A handler is wired only if a function with exactly the right name exists. A
misspelling is not an error: the function is simply never called. Form (window
320 x 100):

```toml
[[node]]
kind = "Button"
name = "save_button"
left = 16
top = 16
width = 120
height = 32
text = "Save"

[[node]]
kind = "Label"
name = "status_label"
left = 16
top = 60
width = 280
```

Script:

```rhai
fn save_buton_click() { status_label.text = "never"; }      // control name misspelt
fn save_button_clicked() { status_label.text = "never"; }   // the event is `click`
fn Save_Button_Click() { status_label.text = "never"; }     // names are case-sensitive

fn save_button_click() { status_label.text = "saved"; }     // the only one that runs
```

Checklist: the name is the control's name from the `.lfm`, then `_`, then the event
in snake_case (`key_down`, `double_click`); the control exists in this form; the
event exists for that kind ([Controls](controls.md)). In the IDE, double-click the
control or use the procedure combo above the editor and it writes a correct stub.
Window events are `form_load` and `form_close`; `form_resize` and `form_activate`
are never called in this build.

### 57. Common errors and what they mean

Goal: read an error message and know what to change.

Form (window 320 x 120):

```toml
[[node]]
kind = "Button"
name = "broken_button"
left = 16
top = 16
width = 140
height = 32
text = "Try a mistake"

[[node]]
kind = "Label"
name = "status_label"
left = 16
top = 64
width = 280
```

Script:

```rhai
fn broken_button_click() {
    try {
        total = 5;                    // never declared with let
    } catch (error) {
        status_label.text = error.message;   // a language error is a map; `message` is its text
    }
}
```

The message above is `Variable not found: total`. The rest of the usual ones:

| Message | Meaning and fix |
|---|---|
| `Variable not found: ghost` | The name is not a variable, a top-level `let`, a control or `form`. Declare it with `let`, or fix the spelling of the control. |
| `Function not found: foo (i64)` | No function `foo` takes one integer. Functions are found by name and parameter count. |
| `unknown property 'Text' on name_edit (Edit); did you mean 'text'?` | Property names are snake_case and exactly the catalog's. |
| `expected text, found i64` | You wrote a number to a text property. Use `` `${n}` `` or `n.to_string()`. |
| `expected int, found f64` | An int property such as `selected` or `left` got a float. Use `.to_int()`. A number outside a property's range is refused with the same kind of error. |
| `expected a list of strings, found i64` | `items` takes an array of strings only; convert numbers with `n.to_string()`. |
| `cv.fill_rect takes 5 arguments (x, y, w, h, color), got 3` | A method argument count is wrong; the message gives the real signature. |
| `cannot set `mouse_x` on `cv`: the property is read-only here` | Read-only properties can be read, not written. |
| `Stack overflow` | A function you defined has the same name and parameter count as a control method it calls, such as `fn best_fit()`. Rename it; the IDE's lint warns about it. |
| `'go' is a reserved keyword` | A control named `go` (or `new`, `use`, `match`, `case`, `module`, `public`, `default`, `is`, `in`, `as`, `do`, `for`, `print`, ...) fails to compile where the script uses its name. Rename the control. |
| `operation budget exceeded` | One handler ran about a million operations. Split the work over a `Timer` or a `Canvas` frame loop. |
| `permission denied (EACCES)` | The installed app did not declare that interface or topic: see recipe 49. |
| ``access denied for "...": path is outside the sandbox`` | The script named a file outside its data folder, project and documents. |
| `msg::run()` refused in a form | Events arrive on their own; remove the loop. |
| `Function not found: ...` for `sys::` or `msg::` calls on a desktop build | Those modules exist only in LazyOS's player. |

Every one of these is an ordinary error: `try`/`catch` can handle it, and an
uncaught one in a handler becomes a message box that names the file, line and
column.

## See also

- [LazyRAD documentation](index.md): the hub.
- [Forms](forms.md) and [Controls](controls.md): the `.lfm` format and every control.
- [Scripting](scripting.md) and [the standard library](stdlib.md): how handlers run and what a script can call.
- [LazyOS extras](lazyos.md) and the [Messenger API reference](../rhai/api/index.md): `msg`, `sys::*` and `modplay`.
- [Packaging](packaging.md) and [Projects](projects.md): `.lrp` files, assets, permissions and `.lzp` packages.
- [The IDE](ide.md): the designer, Play and the debugger.
- [The Rhai cookbook](../rhai/cookbook.md): the same services from the `rhai` command.
