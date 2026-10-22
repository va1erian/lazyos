# LazyRAD projects

A LazyRAD project is a folder of plain-text files: one `.lrp` project file that
names the pieces, a `.lfm` layout and a `.rhai` script for each form, a `.rhai`
file for each shared module, and any extra files you ship with it. There is no
binary format and no hidden state, so projects work with ordinary tools: you
can edit them in the Editor, copy them with `cp -r`, and keep them in version
control. This page describes the `.lrp` file, how the pieces fit together,
the checks a project must pass and the samples that ship with LazyRAD.

## Folder layout

```text
hello/
  hello.lrp          the project file; its name must be <name>.lrp
  main_form.lfm      a form's layout (TOML)
  main_form.rhai     the form's script: its event handlers and helpers
  util.rhai          a module: functions shared by the forms
  songs/intro.mod    an asset, if the .lrp lists it in `assets`
```

* Exactly one `.lrp` file may sit in the folder.
* Forms and modules are **flat**: the `.lfm` and `.rhai` files of the items are
  in the project folder itself, never in a subfolder. Only assets may live in
  subfolders.
* Files the `.lrp` does not list are ignored: they are not loaded, not
  exported and not packaged. (The `devplay` sample keeps a launcher script
  next to its project this way.)
* Files are plain UTF-8 text. Project files must be regular files; a symbolic
  link is refused.

## The `.lrp` file

The project file is TOML. This is the Hello sample's:

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

| Key | Required | Meaning |
|---|---|---|
| `name` | yes | The project name. The file must be called `<name>.lrp`. |
| `version` | yes | A free-form string, `"0.1.0"` for a new project. When you package the project it must be a number, `MAJOR`, `MAJOR.MINOR` or `MAJOR.MINOR.PATCH`, each part below 65536; see [Packaging](packaging.md). |
| `startup` | yes | The `name` of the item that runs first. A new project uses its first form, `main_form`. |
| `items` | yes | The forms and modules, one `[[items]]` table each (below). |
| `icon` | no | A `.ico` file, as a plain file name in the project folder. Used as the icon of an exported Windows executable. A LazyOS package does not use it: its icons are generated from the project name. |
| `assets` | no | A list of glob patterns for extra files to ship (see "Assets and resource files" below). |

Unknown keys are ignored (and are not written back when the IDE saves).

An `[[items]]` table has a `kind` and a `name`, and file names relative to the
project folder:

| `kind` | Keys | What it is |
|---|---|---|
| `"form"` | `name`, `layout`, `code` | A window: a `.lfm` layout and a `.rhai` script. The `name` is the form's name; the IDE keeps `name` under `[window]` in the `.lfm` equal to it. |
| `"module"` | `name`, `code` | A script with no window: functions shared by every form. |

Rules for the file names (`layout`, `code`, `icon`): each must be a **plain file
name**, with no folder, no `..`, no drive letter and no leading `/`. This is
what stops a crafted project from reading or overwriting files elsewhere.

Item names, as the IDE accepts them, are identifiers: a letter or `_`, then
letters, digits and `_`. The IDE names new items `form1`, `module1`, ... and
the starter form `main_form`; renaming an item renames its files to
`<name>.lfm` and `<name>.rhai`.

## Forms, scripts and modules

**A form** is a `.lfm` plus a `.rhai`. The `.lfm` is the layout (see
[Forms](forms.md)); the `.rhai` holds its event handlers (see
[Scripting](scripting.md)). A handler is a function named
`<control>_<event>`, plus `form_load` and `form_close` for the window itself.

**A module** is a `.rhai` with no layout. Every module in the project is
compiled and registered when the project loads, and the functions in it are
available to every form's script in three ways:

```rhai
// util.rhai
fn greeting(name) {
    greeting("Hello", name)
}

fn greeting(word, name) {
    `${word}, ${name}!`
}

// main_form.rhai
fn button1_click() {
    label1.text = greeting("Ada");          // directly
    label1.text = util::greeting("Ada");    // qualified by the module name
}
```

and, in a script that prefers it, `import "util" as u;` then `u::greeting("Ada")`.
The module name is the item's `name`. Rhai picks between functions of the same
name by their number of arguments, which is how `greeting` can take one or two.

A module's top-level statements run once, when the module is registered. Rhai
functions cannot see variables declared outside them, so a module function
that needs one of the module's top-level `const` values writes `global::NAME`,
as the IDE's lint warning reminds you.

Forms and modules are listed in the `.lrp`; nothing is globbed. To add a
form or module use **Project > Add Form** or **Add Module** in the IDE
([IDE tour](ide.md)), or add an `[[items]]` table and the files by hand.

## Assets and resource files

`assets` lists extra files to ship with the project: sounds, pictures, data.

```toml
assets = ["songs/*.mod", "icons/logo.png"]
```

| Pattern | Matches |
|---|---|
| `*` | any run of characters except `/` |
| `?` | exactly one character except `/` |
| `**` | any run of characters including `/`, so `songs/**` reaches subfolders |
| anything else | itself |

Patterns use `/` as the separator on every system, and must be relative: not
empty, not starting with `/`, with no `..` component and no `\`. A pattern that
matches nothing is a warning, not an error. Matching never follows symbolic
links and the `.lrp` itself is never an asset.

A script reads an asset with the file functions, by its project-relative path:

```rhai
fn form_load() {
    let intro = file_read_bytes("songs/intro.mod");   // a blob
    let notes = file_read_text("notes.txt");
}
```

The same path works from a project folder, from an exported executable and
from an installed LazyOS app, because the runtime serves the files the project
ships (its items and its assets) itself. Assets are read-only. A script writes
files to the folder the player allows it, not into the project: on LazyOS that
is the app's private data folder ([Packaging](packaging.md));
see [The standard library](stdlib.md) for the file functions.

## Checks and error messages

Before anything runs, the IDE and the player check the whole project, and
every problem is reported at once. Each message names the file and, when it
can, the line (`main_form.lfm:7: ...`). In the IDE they go to the Output
pane and the **Error List**; the player prints them as one JSON object per
line on its error stream (`kind`, `file`, `line`, `col`, `message`) and a
readable line too when it runs on a terminal. On a desktop host,
`lazyrad-player --check <folder or .lrp>` prints them without opening a window.

**The project file**

| Message | What it means |
|---|---|
| `no lrp project file in this directory` | The folder has no `.lrp`. |
| `expected one lrp file, found N` | More than one `.lrp` in the folder. |
| ``project file is named `a.lrp` but its `name` is `b`; rename it to `b.lrp` `` | The file name and `name` disagree. |
| ``item path `x` must be a plain file name in the project folder`` | An item (or the icon) names a folder, `..` or an absolute path. |
| ``item file `x` is a symbolic link; project files must be regular files in the project folder`` | A listed file is a link. |
| ``asset pattern `x` must be a relative path inside the project folder`` | An unsafe `assets` entry. |
| ``startup item `x` is not in the project`` | `startup` names no item. Set a startup item (right-click an item in the Project Explorer, **Set as Startup**). |
| ``referenced layout file `x` does not exist`` (also `code`, `icon`) | The `.lrp` lists a file that is not in the folder. |
| a TOML error with a line number | The `.lrp` or an `.lfm` is not valid TOML, or a key has the wrong type. |

**Forms** (messages start with the control and property, `button1.text: ...`)

| Message | What it means |
|---|---|
| ``unknown widget kind `X` `` | A `kind` that is not in the control catalog. |
| `` `p` is not a property of Button `` | A property the control does not have (property names are lower case with underscores). |
| `` `p` expects number, found text `` | A value of the wrong type. |
| ``node name `x` is used more than once`` | Two controls with one name. |
| `` `x` is not a valid node name `` (also `not a valid form name`) | A name that is not an identifier. |
| ``parent `x` does not exist``, ``GroupBox `x` does not accept a Y child``, ``Button `x` cannot contain children``, `` `x` is part of a parent cycle `` | A bad `parent` link. |
| ``tab_index 2 is also used by `other` `` | A warning: two sibling controls share a tab position. |

**Scripts**

| Report | What it means |
|---|---|
| A parse error, `file:line:col message` | The `.rhai` does not compile. Nothing starts. |
| A lint warning | The script compiles but is likely to fail when it runs (a module `const` used in a module function, a function that calls itself by accident, a call into an unknown module). The project still runs. |

The player exits with code 0 after a normal run, **1** when the project does
not load, validate or compile, and **2** after a fatal runtime error such as
`form_load` throwing. An error inside an event handler is not fatal: the
runtime shows it in a message box and the program keeps running. On LazyOS the
exit status and reason become the "stopped unexpectedly" notice (see
[Packaging](packaging.md)).

## Starting a project and saving it

**File > New Project** writes `<name>.lrp` (version `0.1.0`, startup
`main_form`), `main_form.lfm` (an empty form) and `main_form.rhai` (a
`form_load` stub). It refuses a folder that already holds a project or any of
those files. **Save** rewrites only the files whose contents changed. A
project renamed with **Save As** takes the new `.lrp`'s name; if the folder changes, every item file is copied there and the
old folder is left alone.

**Remove** takes an item out of the `.lrp` but never deletes its files.

## Shipped samples

The sources are in `lazyrad-os/samples/` in the LazyOS repository. On a
LazyOS image built with `python tools/run_demo.py --lazyrad` the first six are
installed as read-only folders in `/system/share/lazyrad/`; copy one to your
projects folder to change it, for example
`cp -r /system/share/lazyrad/hello ~/projects/`. `--lazyrad-samples` (and the
`LAZYRAD_SAMPLES` variable) add more folders to that list.

| Sample | Items | What it shows |
|---|---|---|
| `hello` | form, module `util` | The smallest complete project: a Label, an Edit, a ComboBox and a Button; the handler calls the overloaded `greeting` function of a standard module. |
| `calculator` | form, module `calc` | Many buttons sharing one module: seventeen controls whose handlers are one-liners over helpers in `calc.rhai`. |
| `todo` | form | A ListView whose rows a script adds and removes: read `items`, change the copy, assign it back; `selected` as a row index. |
| `brickbreaker` | form | An Arkanoid-style game on a **Canvas**: a 60 fps `game_canvas_frame(dt)`, keyboard and mouse input, drawing with the canvas methods. The state lives in `form.state` (LazyRAD's own copy of the sample keeps it in top-level variables). |
| `messenger` | form | LazyOS only. Messenger from a form: read a `confd` setting with `sys::confd::get`, react to changes with `sys::confd::on_changed`, serve a method with `msg::serve`. See [LazyOS modules](lazyos.md) and [msg](../rhai/msg.md). |
| `modplayer` | form, modules `cells`, `demo_song` | LazyOS only. A ProTracker player over the player's `modplay` module: playlist, transport, meters, a pattern view. Also an installable package. See [the MOD player](../lazyrad-modplay.md). |
| `pictures` | form, module `paths` | LazyOS only. The Picture Viewer: a **PictureBox**, a Timer for the slide show, `app.documents` for the file it was asked to open. The core app `os.lazy.pictures`. See [the Picture Viewer](../lazyrad-pictures.md). |

Three more projects are test fixtures rather than examples, and are not in
the default image: `perf2000` (a 2000-line module for measuring typing
latency in the IDE), `devplay` (a form that reads a `confd` key, prints a
marker and quits, used to test [development runs](packaging.md))
and `crashload` (a `form_load` that always throws, used to test the
[failure notice](packaging.md)).

LazyRAD's own repository ships four examples in `examples/`: `hello`,
`calculator`, `todo` and `brickbreaker`. LazyOS carries copies of them. The
`.lrp` and `.lfm` files and the first three scripts are identical to the
originals; the `brickbreaker` script differs in how it keeps its state.

## See also

* [LazyRAD](index.md): the hub for these pages.
* [The IDE tour](ide.md): creating, editing and running a project.
* [Packaging](packaging.md): turning a project into an app.
* [Forms](forms.md) and [Controls](controls.md): the `.lfm` format and the
  controls it can hold.
* [Scripting](scripting.md): handlers, events and the lifecycle.
* [Cookbook](cookbook.md): worked examples.
