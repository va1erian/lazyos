# The LazyRAD standard library

The functions, constants and objects every LazyRAD script can use, on every
platform: message boxes, file dialogs, files, random numbers, time, colours and
the `app` object. It is deliberately small. Rhai already provides strings,
arrays, maps and maths, and LazyRAD adds only what Rhai lacks. Platform-specific
extras (`msg`, `sys::*`, `modplay`) are on [LazyOS extras](lazyos.md); how
handlers and controls work is on [Scripting](scripting.md).

Everything is snake_case. Every function can be called from any handler, callback
or module of any form. A failure is an ordinary Rhai error: it carries the
file, line and column, `try`/`catch` can handle it, and an uncaught one in a
handler becomes a message box (see [Scripting](scripting.md)).

## At a glance

| Area | Names |
|---|---|
| Output | `print`, `debug` |
| Arrays | `join` |
| Colours and games | `rgb`, `clamp`, `rects_overlap` |
| Random numbers | `random`, `random_range`, `seed_random` |
| Time | `now`, `today` |
| Dialogs | `msg_box`, `open_file_dialog` |
| Application | `app.title`, `app.path`, `app.documents`, `app.quit()` |
| Files | `file_read_text`, `file_write_text`, `file_append_text`, `file_read_bytes`, `file_write_bytes`, `file_exists`, `file_delete`, `dir_list`, `dir_create` |
| Forms | `form` (the current form), `<form_name>.show()` and `.unload()` (see [Scripting](scripting.md)) |
| Timers | the `Timer` control and the `Canvas` frame loop (below) |

## Output

| Function | Returns | Fails when | Example |
|---|---|---|---|
| `print(value)` | `()` | never | `print("loaded " + file_name)` |
| `debug(value)` | `()` | never | `debug(form.state)` |

Both are Rhai's own, rerouted: each call writes one line to the player's
standard output and flushes it, so the IDE's output pane (which reads the
player's output) shows it as it happens. `debug` writes only the value's text,
without a source position.

## Strings, arrays, numbers

LazyRAD adds one function; the rest is Rhai's.

| Function | Returns | Fails when | Example |
|---|---|---|---|
| `array.join(separator: string)` | string: the items' string forms joined by `separator`; `[].join("-")` is `""` | never | `[1, "two", 3.5].join(", ")` gives `1, two, 3.5` |

Rhai built-ins you will use constantly (all checked against the Rhai version
LazyOS ships; see [the Rhai tour](../rhai/language.md) for the rest):

| Kind | Functions |
|---|---|
| Strings | `len()`, `sub_string(start, count)`, `sub_string(start)`, `index_of(text)`, `contains(text)`, `starts_with(text)`, `ends_with(text)`, `split(separator)`, `to_upper()`, `to_lower()`, `parse_int(text)`, `parse_float(text)` |
| Strings that change in place | `trim()`, `replace(from, to)`, `pad(length, char)`, `crop(start)` modify the string and return `()`; see the warning below |
| Arrays | `len()`, `push(x)`, `pop()`, `remove(index)`, `insert(index, x)`, `contains(x)`, `index_of(x)`, `sort()`, `reverse()`, `filter(f)`, `map(f)`, `reduce(f, start)` (`f` is a closure or `Fn("name")`) |
| Maps | `keys()`, `contains(key)`, `remove(key)`, `len()` |
| Numbers | `abs(x)`, `min(a, b)`, `max(a, b)`, `sqrt(x)`, `x.floor()`, `x.round()`, `x.to_int()`, `x.to_float()`, `x.to_string()`, `PI()`, `sin(x)`, `cos(x)` |
| Blobs | `blob(size)`, `len()` (what `file_read_bytes` returns) |

> **In-place string methods.** `let t = name_edit.text; t.trim();` trims `t`;
> the call itself returns `()`. And `name_edit.text.trim()` reads the control's
> text, trims the copy and **writes the result back into the control**, still
> returning `()`. Copy into a variable first and use that.

## Colours and game helpers

These take any mix of ints and floats, because game code computes with both.

| Function | Returns | Fails when | Example |
|---|---|---|---|
| `rgb(r, g, b)` | int `0xRRGGBB`; each channel clamped to `0..=255` and truncated (a NaN channel is 0) | an argument is not a number: `rgb expects numbers, got string` | `canvas1.clear(rgb(0, 0, 64))` |
| `clamp(value, low, high)` | `value` limited to `low..=high`: an int when all three are ints, otherwise a float | `low > high` (or NaN): `clamp needs low <= high, got 10 and 0` | `clamp(health, 0, 100)` |
| `rects_overlap(x1, y1, w1, h1, x2, y2, w2, h2)` | bool: whether the rectangles share an area; rectangles that only touch along an edge do not overlap | an argument is not a number | `rects_overlap(ball_x, ball_y, 8, 8, brick_x, brick_y, 40, 12)` |

`rgb` is what a canvas or a colour property accepts. A colour property takes
that int or a `"#rrggbb"` string, and reads back as `"#rrggbb"`.

## Random numbers

One small generator (xorshift64*) per form, seeded from the clock at start.

| Function | Returns | Fails when | Example |
|---|---|---|---|
| `random()` | float in `[0.0, 1.0)` | never | `if random() < 0.25 { spawn(); }` |
| `random_range(start: int, end: int)` | int in `start..end`, **`end` excluded** | `end <= start`: `random_range needs start < end, got 5..5`; floats are not accepted | `let die = random_range(1, 7);` |
| `seed_random(seed: int)` | `()`; the following numbers repeat for the same seed | never | `seed_random(42);` |

`seed_random` sets the low bit, so seeds `2n` and `2n + 1` give the same
sequence.

## Time

| Function | Returns | Fails when | Example |
|---|---|---|---|
| `now()` | string `YYYY-MM-DD HH:MM:SS` | never | `clock_label.text = now();` |
| `today()` | string `YYYY-MM-DD` | never | `date_label.text = today();` |

On the desktop LazyRAD builds they use the local time zone when it is known. On
LazyOS there is no zone database: both report **UTC**. For elapsed time between
frames use the `dt` a `Canvas` `Frame` handler receives.

## Dialogs

Dialogs never block the script. The call queues the dialog, **returns at once**,
and the answer arrives later through a callback you pass; code after the call
runs before the user answers. A callback must declare exactly **one parameter**
(`|answer|`, `Fn("name")` with `fn name(answer)`), and it runs in the form that
asked.

### `msg_box`

| Call | What it shows |
|---|---|
| `msg_box(text)` | one **OK** button, titled with the project name (`app.title`) |
| `msg_box(text, title)` | the same with your title |
| `msg_box(text, title, buttons, callback)` | `buttons` is `"ok"`, `"ok_cancel"` or `"yes_no"`; `callback` receives the button pressed |

All return `()`. The callback receives `"ok"`, `"cancel"`, `"yes"` or `"no"`;
Enter counts as the affirmative button, Escape as the other (an OK-only box
always answers `"ok"`). `msg_box("?", "t", "maybe", cb)` fails with
`msg_box buttons must be "ok", "ok_cancel" or "yes_no", got "maybe"`.

```rhai
fn delete_button_click() {
    msg_box("Delete this item?", "Confirm", "yes_no", |answer| {
        if answer == "yes" {
            items_list.items = [];
        }
    });
}

fn info_button_click() {
    msg_box("Saved.");
}
```

If the form is hot-reloaded while the box is open, the answer is dropped.

### `open_file_dialog`

| Call | Meaning |
|---|---|
| `open_file_dialog(title, filter, callback)` | ask the platform for a file to open |
| `open_file_dialog(title, filter, callback, "folder")` | the same, and also grant read access to the picked file's folder (its listing and the files directly in it, never subfolders), so a viewer can `dir_list` the neighbours |

`filter` is `"name|pattern;name|pattern"`: groups separated by `;`, each a
display name and its `,`-separated patterns after a `|`; a bare name matches
everything. `"Pictures|*.png,*.jpg;All files|*.*"` is two groups. `callback`
receives the picked path as a string, or `()` when the user cancels. A picked
file is granted read access in the file sandbox (see below). Any option other
than `"folder"` fails with ``open_file_dialog: unknown option `x` (expected "folder")``.

```rhai
fn open_button_click() {
    open_file_dialog("Open a picture", "Pictures|*.png,*.jpg;All files|*.*", |path| {
        if type_of(path) == "string" {
            status_label.text = path;
        }
    }, "folder");
}
```

> **On LazyOS** (from the player's source at the pinned LazyRAD revision; not
> confirmed on a running guest) the player installs no file-dialog backend, so
> `open_file_dialog` answers `()` immediately, as if the user had cancelled.
> Start a document from Files (it arrives in `app.documents`) or keep files in
> the app's data folder instead. Always test for the cancel case, as above.

## The `app` object

`app` is a global describing the running program.

| Member | Type | Meaning |
|---|---|---|
| `app.title` | string | the project's name (`name` in the `.lrp`) |
| `app.path` | string | the project directory the program was started from; for an installed LazyOS app, `<install dir>/resources/project` |
| `app.documents` | array of strings | the files the program was started to open (a picture double-clicked in Files); empty when none. On LazyOS at most 64, absolute paths, readable together with their folder |
| `app.quit()` | `()` | ends the program after the current handler returns |

```rhai
fn form_load() {
    title_label.text = app.title;
    if app.documents.len() > 0 {
        status_label.text = `opening ${app.documents[0]}`;
    }
}

fn exit_button_click() {
    app.quit();
}
```

`app` is resolved after controls and `form`, so a control named `app` hides it.
`app.quit()` ends the event loop directly: it does not run `form_close`.

## Files and folders

Nine functions, all taking **text paths** with `/` separators. Failures are
Rhai errors; the message for an operating-system failure is `` `path`: <reason> ``
(for example `` `notes.txt`: No such file or directory ``).

| Function | Returns | Fails when | Example |
|---|---|---|---|
| `file_read_text(path)` | string, the whole UTF-8 file | the file is missing, unreadable or not valid UTF-8; or the path is refused | `let t = file_read_text("notes.txt");` |
| `file_write_text(path, text)` | `()`; creates or replaces the file | the folder does not exist or is not writable; or the path is refused for writing | `file_write_text("notes.txt", notes_edit.text);` |
| `file_append_text(path, text)` | `()`; appends, creating the file when missing | as `file_write_text` | `file_append_text("log.txt", now() + "\n");` |
| `file_read_bytes(path)` | blob, the whole file | as `file_read_text`, minus the UTF-8 check | `picture1.load(file_read_bytes(path));` |
| `file_write_bytes(path, data)` | `()`; creates or replaces the file with the blob | as `file_write_text`; `data` is not a blob | `file_write_bytes("copy.bin", blob(16));` |
| `file_exists(path)` | bool: whether a file **or directory** exists | the path is refused (a refused path is an error, not `false`) | `if file_exists("save.txt") { load_game(); }` |
| `file_delete(path)` | `()`; deletes one file | the file is missing; the path is a directory; the path is refused for writing | `file_delete("save.txt");` |
| `dir_list(path)` | array of names (files and folders, names only), sorted | the folder is missing or unreadable; or refused | `let names = dir_list("music");` |
| `dir_create(path)` | `()`; creates the folder and every missing parent | cannot create; or refused for writing | `dir_create("saves/slot1");` |

`file_write_text` does **not** create folders; call `dir_create` first. Writes
replace the whole file at once; there is no streaming.

**Not available:** rename, copy, move, file size or modification time, "is it a
directory", deleting a directory, a temporary-file helper, the working
directory. Walk a folder with `dir_list` and `file_exists`.

```rhai
fn save_button_click() {
    try {
        dir_create("saves");
        file_write_text("saves/slot1.txt", `${form.state.score}`);
        status_label.text = "saved";
    } catch (error) {
        status_label.text = `${error}`;
    }
}

fn load_button_click() {
    if file_exists("saves/slot1.txt") {
        form.state.score = parse_int(file_read_text("saves/slot1.txt"));
    }
}
```

### The project's own files

A relative path is looked up in the **project first** for the read functions
(`file_read_text`, `file_read_bytes`, `file_exists`). A script can therefore
read what the project ships, the same way from a folder on disk and from an
installed or exported app:

* every file an item of the `.lrp` names (`.lfm` and `.rhai` files), and
* every file matched by the `.lrp`'s `assets` globs (images, data, songs),

never the `.lrp` itself. A path with `..`, a backslash, a drive prefix or an
absolute path is never a project file, and a link inside the project is never
followed. A relative path that is not a shipped file falls through to the file
sandbox below, where it means "inside the app's data folder". Writes always go
to disk, so a file the script wrote is read back from where it was written, not
from the project. `dir_list` has no project view: it lists real folders only.

### The file sandbox

Every path a script names goes through one policy before it touches the
filesystem. Two policies exist.

**Unrestricted** (the default of a desktop build): every path is allowed,
relative ones against the working directory. Only an empty path
(`invalid path "": path is empty`) and one containing a NUL byte are refused.

**Sandboxed** (LazyOS, and any host that asks for it): a script reaches only
its **private folder**, plus a few read-only extras. The rules, in order:

1. An empty path, one with a NUL byte, or one longer than 4096 bytes is refused
   (`invalid path "...": ...`).
2. A **relative** path is joined to the private folder. An **absolute** path is
   used as is, and then must land inside the private folder or an extra grant.
3. `.` and `..` are resolved on the text first, without following links, so
   `../x` cannot climb out: a request that would leave the folder is refused
   with ``access denied for "../x": path is outside the sandbox``.
4. The deepest folder that already exists is then resolved through every
   symbolic link, and the tail that does not exist yet is appended. A path is
   allowed only when that real location is inside the private folder or a
   grant that gives the access asked for. So `saves/slot9/new.txt` is allowed
   before `saves/slot9` exists, while a link inside the folder that points
   outside is refused.
5. A **write** whose own last component is an existing symbolic link is always
   refused (`refusing to write through a symbolic link`), even if the link
   points back inside. A dangling link is refused for any access.
6. The check is by whole path components: a grant for `/a/b` does not cover
   `/a/bc`.

Access has two levels: reading (`file_read_text`, `file_read_bytes`,
`file_exists`, `dir_list`) and writing (`file_write_text`, `file_write_bytes`,
`file_append_text`, `file_delete`, `dir_create`). A write grant also allows
reading; a read grant never allows writing. The refusals read:

| Message | Cause |
|---|---|
| `access denied for "<path>": path is outside the sandbox` | not under the private folder or a grant that gives this access |
| `access denied for "<path>": refusing to write through a symbolic link` | rule 5 |
| `access denied for "<path>": no existing ancestor to resolve` | no parent of the path exists, or a dangling link was met |
| `invalid path "<path>": path is empty` / `contains a NUL byte` / `is longer than 4096 bytes` | rule 1 |

One caveat the policy documents about itself: the check resolves the path, then
the function opens it, so another process that swaps a folder for a link
between the two steps could escape. The scripts of one app run as the logged-in
user and the kernel's own rules still apply underneath.

### What a script may touch on LazyOS

LazyOS builds the sandbox for each run of the player (`lrplay`):

| The program is | Read and write | Also readable (never writable) |
|---|---|---|
| an installed `.lzp` app (its player runs from `/apps/<id>/<version>-<hash>/bin/`) | `<home>/.apps/<id>/` | its project (`<install dir>/resources/project`); each document it was started to open and, for each, the document's folder listing and the files directly in it |
| a project run by Play, from a terminal, or by `lrplay <project>` | `<home>/.apps/os.lazy.lazyrad/data/` | the project path it was given; the same documents |
| the IDE's designer preview | `<home>/projects/` | nothing more |

* `<home>` is `$HOME` (for the logged-in user, `/home/<name>`). If `$HOME` is
  unset or unusable (it must be absolute, at most 1024 bytes, with no `.`, `..`,
  empty segment or control character), the home is the RAM disk
  `/transient/lazyrad`, so **nothing is kept after a reboot**, and the player
  prints a `LRPLAY:HOME:WARN` line.
* The player creates the private folder when it starts, best effort.
* `/apps` belongs to the package manager; an installed app never writes inside
  its install directory.
* A project started from a directory can read every file in it; started from an
  `.lrp` path, the sandbox grants only that file, but files the project ships
  are still served by relative path (previous section).
* A document directly in `/` brings no folder grant. Subfolders of a document's
  folder stay out of reach.
* `/home/<other user>`, `/system`, `/conf` and the rest are out of reach: the
  error is the usual `path is outside the sandbox`.
* Relative paths mean the private folder, **not** the working directory and not
  the project: `file_write_text("notes.txt", ...)` writes
  `<home>/.apps/<id>/notes.txt`.

```rhai
fn form_load() {
    // The data folder: a relative path.
    if file_exists("settings.txt") {
        name_edit.text = file_read_text("settings.txt");
    }
    // A document the app was started to open, and its neighbours.
    if app.documents.len() > 0 {
        let first = app.documents[0];
        let bytes = file_read_bytes(first);
        status_label.text = `${first}: ${bytes.len()} bytes`;
    }
}
```

## Timers and frames

There is no `sleep`. Work that happens over time uses a control that raises an
event.

**`Timer`** is a non-visual control that raises `Tick` every `interval`
milliseconds while `enabled` is true.

| Member | Meaning |
|---|---|
| `interval` | milliseconds between ticks, 1 to 86 400 000 (one day); default 100 |
| `enabled` | whether it runs; **starts false**, so a script or the `.lfm` turns it on |
| `fn <name>_tick()` | the handler; it takes no arguments |

The runtime brings timers in line with the form after every event, so
`timer1.enabled = true` starts it when the handler returns, and changing
`interval` restarts it at the new rate. A `Tick` handler that fails turns its
timer off and shows the error once.

**`Canvas`** raises `Frame(dt)` about `fps` times a second while `fps` is above
0 (maximum 240); `dt` is the number of seconds since the previous frame, as a
float. A failing `Frame` handler sets `fps` to 0; setting it again restarts the
loop. A frame still queued when the loop stops is dropped.

```rhai
fn start_button_click() {
    game_timer.interval = 500;
    game_timer.enabled = true;
}

fn game_timer_tick() {
    form.state.ticks += 1;
    ticks_label.text = `${form.state.ticks}`;
}

fn board_canvas_frame(dt) {
    form.state.x += 60.0 * dt;
    board_canvas.clear(rgb(0, 0, 32));
    board_canvas.fill_circle(form.state.x, 80, 6, rgb(255, 220, 0));
}

fn form_load() {
    form.state.ticks = 0;
    form.state.x = 0.0;
    board_canvas.fps = 60;
}
```

A canvas's drawing methods and properties are on [the Canvas page](controls.md).

## Functions that do not exist

LazyRAD's library has no `input_box`, no clipboard functions, no `sleep`, no
`exit` (use `app.quit()`), no environment or process access, no network access
(on LazyOS call a service through [`sys::*`](lazyos.md)), and no `os::*`
module: that belongs to the `rhai` command, not to forms. A call to a name the
engine does not know fails with `Function not found: name (types...)`.

## See also

* [Scripting](scripting.md): handlers, lifecycle, errors
* [LazyOS extras](lazyos.md): `msg`, `sys::*`, `modplay`, data folders, markers
* [Controls](controls.md): properties, events and methods of every control
* [Projects](projects.md): the `.lrp` file and its `assets` globs
* [Packaging](packaging.md): what an installed app is allowed to do
* [The Rhai tour](../rhai/language.md)
