# How a LazyRAD script works

Every LazyRAD form has a Rhai script beside it: the form (`.lfm`) says what is
on the window, the script (`.rhai`) says what happens. This page is the model
behind it: which files run, how a click finds its function, how `name_edit.text`
resolves, what the `form` object is, where errors go and what the runtime
refuses. The functions a script can call (`msg_box`, `file_read_text`,
`random`, ...) are in [the standard library](stdlib.md); what only LazyOS adds
is in [LazyOS extras](lazyos.md).

Everything here describes the LazyRAD revision LazyOS pins (`lazyrad-os/Cargo.toml`)
and was checked against its source.

## Files and modules

A project (`.lrp`, see [Projects](projects.md)) lists items. Each item is a
script file:

| Item kind | Files | What the script is |
|---|---|---|
| `form` | `name.lfm` and `name.rhai` | the code behind one window |
| `module` | `name.rhai` | a library of functions other scripts call |

The `startup` item is the form that opens first. Closing it ends the program;
closing any other form only closes that window.

```toml
name = "hello"
version = "0.1.0"
startup = "main_form"

[[items]]
kind = "form"
name = "main_form"
layout = "main_form.lfm"
code = "main_form.rhai"

[[items]]
kind = "module"
name = "util"
code = "util.rhai"
```

How a module reaches a form:

* Every form gets its **own** Rhai engine, and every module is compiled and run
  again in each of them, before the form's own script. Nothing is shared between
  forms: not module variables, not `form.state`, not controls.
* A module's functions are available **two ways**: qualified (`util::greeting("Ada")`)
  and bare (`greeting("Ada")`), because they are also put in the global
  namespace. Two modules that define the same name with the same number of
  parameters therefore clash when called bare; qualify the call.
* `import "util" as u;` at the top of a form script also works (`u::greeting(...)`).
* A module's top-level `const` is **not visible inside its own functions**
  (a Rhai rule: functions cannot see the script around them). Write
  `global::NAME` inside the function, or keep the value in a function. To hand a
  value to other scripts, declare it with `export const NAME = ...;` and read it
  as `util::NAME`. (A form script is different: its top-level variables are
  shared with its functions, see below.)

```rhai
// util.rhai
const WORDS = ["Hello", "Hi", "Hey"];
export const COUNT = 3;

fn greeting(name) {
    `${global::WORDS[0]}, ${name}!`
}
```

A module function is called from a handler on the form's engine, so it also
resolves control names (`result_label.text = ...`) the way a handler does. That
couples the module to the forms that use it; passing values in as arguments is
cleaner.

## Top-level code and shared variables

A form script may have statements outside any function. They run **once**, when
the form is built, after the modules and before `form_load`. At that moment the
controls already exist, so `status_label.text = "starting"` works at top level.

Every top-level `let` and `const` is kept afterwards and shared with every
function of the script:

```rhai
let clicks = 0;
const GREETING = "Hello";

fn form_load() {
    result_label.text = GREETING;
}

fn hello_button_click() {
    clicks += 1;                       // one variable, seen by every handler
    result_label.text = `${GREETING}, ${name_edit.text}! (${clicks})`;
}
```

* A `const` can be read but not assigned (`GREETING = "x"` is a runtime error).
* Arrays and maps are one shared value: `items.push(1)` in one handler is seen
  by the next.
* A function **parameter** or a local `let` with the same name hides the shared
  variable inside that function.
* Only variables declared at the top level of the script count; one declared
  inside an `if` or a loop block is gone when the block ends.
* While the top-level code itself runs, the functions it calls cannot yet see the
  top-level variables (they are collected when it finishes). Initialise from
  `form_load` when a function needs them.
* Shared variables are lost on a [hot reload](#hot-reload); keep what must
  survive in `form.state`.
* Assigning to a name that was never declared (`total = 5` with no `let total`)
  is `Variable not found: total`.

## Handlers: `fn <control>_<event>(args)`

A handler is a function named after the control and the event, joined by `_`:
the control name exactly as in the `.lfm`, then the event name in snake_case
(`Click` becomes `click`, `MouseDown` becomes `mouse_down`, `DoubleClick`
becomes `double_click`). Use snake_case control names too (`hello_button`,
`name_edit`): the control part is used verbatim.

```rhai
fn hello_button_click() { result_label.text = "Hello"; }
fn name_edit_change(text) { count_label.text = `${text.len()} characters`; }
fn agree_check_toggle(checked) { ok_button.enabled = checked; }
fn items_list_activate(index) { status_label.text = `row ${index}`; }
```

An event is wired only if the function exists. A control with no handler does
nothing when clicked; a misspelt handler name is not an error, it is just never
called. The IDE can create the stub for you.

### Arguments

Each event passes values from the widget; the handler declares as many
parameters as it wants **by name and count**, and Rhai matches a function by
name *and* number of parameters:

* fewer parameters than the event supplies: the extra values are dropped, so
  `fn name_edit_change()` is fine when you do not need the text;
* more parameters than the event supplies: the extra ones are `()`.

Define one function per handler name. (Two functions with the same name and
different parameter counts are legal Rhai, but the runtime records a single
parameter count per name, so only one of them is ever called for the event.)

Argument types are plain Rhai values: whole numbers are `int`, fractional ones
`float`, text `string`, `checked` a `bool`, a list property an `array`. The
events of the controls in the catalog:

| Control | Event (handler suffix) | Arguments |
|---|---|---|
| Button, Hyperlink | `Click` (`_click`) | none |
| CheckBox, ToggleButton | `Toggle` | `checked` (bool) |
| RadioGroup, ComboBox | `Select` | `index` (int) |
| ListView | `Select`, `Activate` | `index` (int) |
| Edit, MultilineEdit | `Change` | `text` (string) |
| NumberField, Slider | `Change`, `Commit` | `value` (float) |
| Timer | `Tick` | none |
| Canvas | `Frame` | `dt` (float, seconds since the last frame) |
| Canvas | `KeyDown`, `KeyUp` | `key` (string) |
| Canvas | `MouseDown`, `MouseUp` | `x`, `y` (float), `button` (string) |
| Canvas | `MouseMove` | `x`, `y` (float) |
| PictureBox | `KeyDown` | `key` (string) |
| PictureBox | `Wheel` | `delta` (float), `ctrl` (bool) |
| PictureBox | `Click`, `DoubleClick` | `x`, `y` (float) |

[Controls](controls.md) lists every control with its properties, events and
methods; that page is authoritative for events not in this table.

You may also call a handler yourself like any function, with exactly the
parameters it declares: `hello_button_click();`.

## The form's lifecycle

Only three window-level functions exist. The others the designer lists for a
window (`Resize`, `Activate`) are not delivered by the runtime in this revision;
nothing calls `fn form_resize` or `fn form_activate`.

| Function | When it runs |
|---|---|
| `fn form_load()` | once, after the widgets are built and the top-level code has run |
| `fn form_close()` | when the window is asked to close (the user closes it); it cannot cancel the close |
| `fn form_reload(old_state)` | after `form_load` on a [hot reload](#hot-reload), with the previous `form.state` |

`app.quit()` ends the event loop directly and `other_form.unload()` closes a
window directly; neither goes through the close request, so do not rely on
`form_close` for cleanup that must always happen (save when the user acts).

An error in the startup form's top-level code or `form_load` stops the program
before a window is shown: the player prints the error as JSON on stderr and
exits with code 2 (on LazyOS `init` then shows its "stopped unexpectedly"
notice with the reason). An error in `form_close` is only printed to stderr.

Closing a secondary form and opening it again builds it from scratch: fresh
engine, fresh variables, `form_load` again.

## How bare names resolve

A handler writes `name_edit.text`, not `form.controls("name_edit").text`.
Rhai functions cannot see anything outside themselves, so the runtime installs a
resolver that is asked for every name the function does not already have. It
answers in this order, and the first match wins:

1. a variable already in the function (a parameter, a `let`);
2. a **top-level `let`/`const`** of the script;
3. a **control** of the form, by its name in the `.lfm`;
4. `form` (the form object, below);
5. a **global** the host registered: `app`, then the names of the project's
   forms (for `other_form.show()`).

The value found is pushed into the function's scope as an ordinary variable, which
is why `label.text = "x"` can assign (a value returned straight from the resolver
would be read-only). What this means for naming:

* A control called `count` is hidden by a top-level `let count` or a parameter
  `count`. Give controls a suffix (`count_label`, `name_edit`) and variables
  plain names.
* A control called `app`, `form` or the name of another form hides that global.
* A name no step knows is `Variable not found: ghost`.
* Because the resolver runs per call, functions *do* see top-level variables and
  controls even though plain Rhai functions would not. They still cannot see the
  **local variables of their caller**; pass them as arguments.

### Reading and writing controls

* **Properties** are read and written with dot syntax (`name_edit.text`,
  `volume_slider.value = 40`) or with a string index, which helps when the name
  is computed: `name_edit["text"]`. Property names are exactly the catalog's,
  all snake_case (`tab_index`); there are no Visual Basic aliases (`Caption`,
  `ListIndex`).
* **Types are checked on write.** An `int` property (`left`, `selected`) takes an
  `int`, not a float: write `box.left = (box.left + 2.5).to_int()`. A `float`
  property takes an int or a float. A text property takes a string
  (`label.text = n.to_string()`). A colour property takes `"#rrggbb"` or an int
  `0xRRGGBB` (what `rgb(r, g, b)` returns) and reads back as `"#rrggbb"`. A list
  property (`items`) takes an array of strings.
* **Methods** take their arguments positionally (`canvas1.fill_rect(10, 10, 50, 20, 0xff0000)`).
  For a method argument that is an `int`, a float is truncated; for a `float`
  argument, an int is widened.
* Reading a collection property gives you a **copy**: `list.items.push("x")` and
  `list.items[0] = "y"` do update the control (Rhai writes the change back), but
  `let a = list.items; a.push("x");` does not.

Mistakes name the control, its kind and the right spelling:

```text
unknown property 'Text' on name_edit (Edit); did you mean 'text'?
unknown property 'nope' on result_label (Label); properties: <the kind's list>
unknown method 'fill_rect' on label1 (Label); methods: <the kind's list>
canvas1.fill_rect takes 5 arguments (x, y, w, h, color), got 3
```

## The `form` object

`form` is one object per form, always available:

| Member | Meaning |
|---|---|
| `form.state` | a map (`#{}`) that lives as long as the form: data that must outlive one event, and the only thing a hot reload carries over |
| `form.title` | a string the script owns; starts as `""`. It does **not** change the window's title bar in this revision (the title bar comes from the `.lfm`) |
| `form.show()`, `form.hide()` | show or hide every control of the form (the window itself stays) |

```rhai
fn form_load() {
    form.state.count = 0;
    form.state.history = [];
}

fn add_button_click() {
    form.state.count += 1;
    form.state.history.push(form.state.count);
    count_label.text = `${form.state.count}`;
}
```

`form.state = 5` is an error (`form.state must be a map`). `form.state` hands
out a copy when read into a variable (`let s = form.state; s.count = 9;` changes
only `s`), but a path such as `form.state.count += 1` or
`form.state.history.push(x)` updates it.

`form.Title` or `form.nope` fail with the list of members (`title, state`).

### Several forms

Each form name in the project is a global that stands for that form's window:

```rhai
// main_form.rhai
fn settings_button_click() {
    settings_form.show();      // opens the window if it is not already open
}

// settings_form.rhai: a Close button inside the settings form itself
fn close_button_click() {
    settings_form.unload();    // closes this window
}
```

* `show()` opens a **non-modal** window; calling it on an open form does nothing.
  There is no modal display yet.
* The two forms do not see each other's controls, variables or `form.state`.
  Pass data through a file in the app's data folder ([`file_write_text`](stdlib.md)),
  or through Messenger topics ([LazyOS extras](lazyos.md)).
* `msg_box` and file dialogs always open in the window of the form whose script
  asked for them.
* A form's own name is also a global, so a secondary form closes itself with
  `<its own name>.unload()` (as `settings_form` does above). `unload()` only
  closes a window that `show()` opened. The startup form's window was not opened
  that way, so `main_form.unload()` does nothing there: the startup form ends
  when its script calls `app.quit()` or the user closes the window.
* If a secondary form's `form_load` (or its top-level code) fails, `show()` does
  not stop the program: the error is printed to stderr and the window opens
  empty, with none of the form's controls. Only the startup form's failure ends
  the program.

## Asynchronous calls and callbacks

The UI never blocks on a script. Calls that need the user or the window system
return at once and report later through a function you pass:

* `msg_box(text, title, buttons, callback)`: the callback gets `"ok"`, `"cancel"`,
  `"yes"` or `"no"`;
* `open_file_dialog(title, filter, callback)`: the callback gets the path or `()`;
* Messenger handlers on LazyOS ([LazyOS extras](lazyos.md)) and `modplay` deck
  handlers run on the window between events.

A callback is a closure or a named function pointer:

```rhai
fn quit_button_click() {
    msg_box("Quit without saving?", "Quit", "yes_no", |answer| {
        if answer == "yes" { app.quit(); }
    });
}

fn later() {
    msg_box("Done", "Notice", "ok", Fn("after_notice"));
}

fn after_notice(answer) {
    status_label.text = `closed with ${answer}`;
}
```

A callback runs in the form that created it; if the form is hot-reloaded while
the box is open, the answer is dropped rather than run against the new script.

A long loop in a handler blocks the window. There is no `sleep` or `wait`; for
anything that happens over time use a `Timer` control (`enabled` starts
`false`; set `timer1.enabled = true` and handle `fn timer1_tick()`) or a
`Canvas` `fps` and `fn canvas1_frame(dt)`.

```rhai
fn start_button_click() {
    form.state.seconds = 0;
    clock_timer.interval = 1000;
    clock_timer.enabled = true;
}

fn clock_timer_tick() {
    form.state.seconds += 1;
    time_label.text = `${form.state.seconds} s`;
}
```

## Errors: what the user sees

A script error is located and shown with the file, line and column:

```text
main_form.rhai:12:5: Variable not found: total (in hello_button_click)
```

When an error passes through several functions the chain is appended, outermost
first, joined by arrows, and a run of the same function is collapsed to its name
followed by a multiplication sign and the count: a function `mute` that called
itself 200 times appears as `mute`, a space, the multiplication sign and `200`
(mute, repeated 200 times). Rhai's own messages repeat the position at their end.

| Where it happens | What happens |
|---|---|
| A parse error in any `.rhai` | the project does not start (exit code 1); every problem is listed at once |
| Top-level code or `form_load` of the startup form | the program does not start (exit code 2) |
| An event handler or a callback | a message box titled `LazyRAD` shows the located error; the program keeps running |
| A `Canvas` `Frame` handler | the same box, and the canvas's frame loop stops (its `fps` becomes 0); set `fps` again to restart it |
| A `Timer` `Tick` handler | the same box, and that timer is turned off (`enabled = false`) |
| A Messenger or `modplay` handler | the same box; a handler that fails on every event is shown once |

`try`/`catch` is ordinary Rhai and works everywhere:

```rhai
fn save_button_click() {
    try {
        file_write_text("notes.txt", notes_edit.text);
        status_label.text = "saved";
    } catch (error) {
        status_label.text = `could not save: ${error}`;
    }
}
```

### Limits and stopping

* Each handler call, callback and the top-level run may execute about **one
  million** Rhai operations; past that it stops with `operation budget exceeded`.
  Rhai's own hard cap is ten million operations.
* Calls nest at most **128** deep (`Stack overflow` beyond it); an expression
  nested deeper than 64 levels (32 inside a function) does not compile.
* The host can set a stop flag; the running script ends at its next progress
  check with `script stopped` (this is what the IDE's stop uses).

## Lint warnings

Beside parse errors the project check runs a static pass that warns about
scripts which compile but are likely to fail at run time. Warnings never stop a
project from running. The IDE lists them with a line and column; the player
prints them (JSON `kind` `warning`) only together with real errors.

| Warning | Why |
|---|---|
| `fn X calls .X(...) on a value, which Rhai resolves back to this function and recurses` | a function you define whose **name and parameter count** equal a control method hijacks every call of that method |
| `` `NAME` is a module-level constant, which a function cannot see `` | a module function used a top-level `const` of its module; use `global::NAME` |
| `` `m::f` names a module the project and its extensions do not define `` | a `module::function` call whose module does not exist (typo, or `msg`/`sys` on a host build) |

The first is the one that bites. Rhai gives a script function priority over a
built-in with the same name and parameter count when written as a method call,
so this recurses until `Stack overflow`:

```rhai
// Same name and arity as PictureBox's best_fit(): picture1.best_fit() calls
// this function, which calls picture1.best_fit(), which calls this function.
fn best_fit() { picture1.best_fit(); }

// Fine: a different name.
fn view_best_fit() { picture1.best_fit(); }
```

## Hot reload

A running program can pick up edits without restarting. The player watches the
`.lrp` and every `.lfm`/`.rhai` it references (every 500 ms). On a change it
checks and builds the new version **before** touching the running one:

* if the project no longer checks or a form does not build, the running forms
  stay and a banner at the top of the window shows `reload failed: <first
  problem>`;
* otherwise every open window is rebuilt: timers and Messenger subscriptions of
  the old form are released, open message boxes are dropped, top-level code runs
  again (shared variables start over), then `form_load`, then
  `fn form_reload(old_state)` if you define it;
* a runtime error in the new `form_load` shows in the banner, but the reload
  stands and `form_reload` still runs.

```rhai
fn form_load() {
    form.state.score = 0;
}

// Keep the score across edits of the UI.
fn form_reload(old_state) {
    if "score" in old_state {
        form.state.score = old_state.score;
    }
}
```

A script without `form_reload` starts with fresh state. On LazyOS the IDE runs
a project with hot reload in a Debug run (`--debug`); Play passes no `--watch`
and `lrplay` has no such flag.

## Debugging hooks

Under the IDE's debugger (`lrplay --debug`) the player checks every statement,
so breakpoints, stepping, locals and "break on error" work; a stop blocks the
player's window until the IDE resumes it. Output from `print(...)` and
`debug(...)` goes to the player's standard output, which the IDE's output pane
shows. [The IDE](ide.md) page covers the debugger itself.

## A Rhai refresher for LazyRAD

The rest of the language is in [the Rhai tour](../rhai/language.md). These are
the points that trip people up in forms.

| Topic | Remember |
|---|---|
| Numbers | `7 / 2` is `3` (integer division); `7.0 / 2` is `3.5`. Convert with `.to_int()`, `.to_float()`, `parse_int("12")`, `parse_float("1.5")`. Round with `.round()` (a float method). |
| Text | `` `${a} and ${b}` `` builds a string from any values; `"" + 5` also works. Compare with `==`. |
| Nothing | `()` is "no value": what a missing handler argument, a cancelled dialog or an absent map key gives. Test with `x == ()` or `type_of(x) == "string"`. |
| Statements | end each with `;`; a function returns its last expression if it has no `;`. `if` and blocks are expressions: `let w = if a > b { a } else { b };` |
| Names | functions are looked up by **name and parameter count**; variables from the enclosing function are not visible to a function you call (pass them in). |
| Values | arrays and maps are copied when passed to a function; return the new value or keep the data in `form.state` or a top-level variable. |
| Ranges | `for i in 0..n` excludes `n`; `0..=n` includes it. |
| Errors | `throw "text"` and `try { ... } catch (e) { ... }`. |
| Maps | `#{ a: 1, "b c": 2 }`; `"a" in map` tests a key. |

Closures are written `|x| x + 1`; they capture the variables around them by
value, and `Fn("name")` makes a pointer to a script function. Both are accepted
wherever a callback is expected.

## See also

* [The standard library](stdlib.md): every function a script can call
* [LazyOS extras](lazyos.md): Messenger, `sys::*`, `modplay`, folders, markers
* [Forms and `.lfm` files](forms.md)
* [Controls](controls.md)
* [Projects](projects.md)
* [The IDE and debugger](ide.md)
* [Packaging apps](packaging.md)
* [The Rhai language tour](../rhai/language.md)
