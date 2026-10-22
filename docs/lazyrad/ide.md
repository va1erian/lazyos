# The LazyRAD IDE

LazyRAD is a Visual Basic 6 style environment: you draw a form, double-click a
button to get its handler, write the handler in Rhai, press F5 to run it,
debug it when it misbehaves and package it when it works. This page is a tour
of the IDE (`lazyrad` on LazyOS) and ends with a first app you can build in a
few minutes.

Where LazyOS differs from the desktop build of LazyRAD, the text says so, and
the last section collects the differences.

## Starting the IDE

* **On LazyOS** the IDE is the core package `os.lazy.lazyrad` (category
  development, so it is in the Start menu's development group). Images are
  built with `python tools/run_demo.py --lazyrad`.
* **From the Terminal**, give it a project folder or an `.lrp` file; the IDE
  opens the folder:

  ```bash
  rhai -e 'msg::connect("os.lazy.init.v1").launch("os.lazy.lazyrad", "/home/user/projects/hello", 0)'
  ```

* **On a desktop host** build LazyRAD and run `cargo run -p lazyrad-ide`.

With no project open the window shows the **Start Page**, the first tab: the
logo, one line of welcome text and the built-in **Getting started** tutorial
(handlers, properties, `form.state`, hot reload, the standard library, the
Canvas and PictureBox, modules and a minute of Rhai). Scroll it with the wheel.
Its code samples are checked against the runtime by LazyRAD's own tests, so
they are safe to copy.

## The window

```text
+-----------------------------------------------------------+
| menu bar                                                  |
| toolbar                                                   |
+---------+---------------------------------+---------------+
| Toolbox | document tabs                   | Project       |
|         |  Start Page | main_form | ...   |---------------|
|         |                                 | Properties    |
|         +---------------------------------+---------------+
|         | Output | Locals | Call Stack | Immediate        |
+---------+---------------------------------------------------+
| status bar                                                |
+-----------------------------------------------------------+
```

| Pane | What it does |
|---|---|
| Toolbox | The Pointer and one tile per control kind: Button, Edit, Label, CheckBox, RadioGroup, GroupBox, ListView, ComboBox, Canvas, PictureBox, Timer. |
| Document tabs | The Start Page, then one tab per open document. A form has two kinds: the **designer** (tab named `main_form`) and its **code** (`main_form.rhai`). A module has code only. A `*` after the title means unsaved changes. |
| Project | The Project Explorer: the project row, a **Forms** group and a **Modules** group. |
| Properties | The property grid for what is selected in the front designer. |
| Output | The log line (the most recent messages: commands, `print` output from your program, run and debug notes) with the **Error List** below it. |
| Locals, Call Stack, Immediate | The debugger's panes (see Debugging below). |
| Status bar | `Design` while nothing runs, `Run` while a program runs, `Break` while it is stopped in the debugger. |

The window title is `LazyRAD - <project>`, with `*` when anything is unsaved
and ` [run]` or ` [break]` while a program runs or is stopped. Drag the
dividers between panes to resize them; the sizes are saved when you exit.

The toolbar is icons only (hover for the name and shortcut) except **Run** and
**End**. Its groups are: New Project, Open Project, Save, Save All; Undo,
Redo; Cut, Copy, Paste; Run, End; Start with Debugging, Break, Step Over.

## Menus and keyboard shortcuts

Every menu item, toolbar button and shortcut raises the same command, so they
always agree. Items that need an open project are greyed out until you have
one, and Start, End, Continue, Break and the step commands follow the run
state. The menus have access letters (underlined in their names); Alt chords
are left to the menu itself and are not IDE shortcuts.

**File**

| Item | Shortcut | Notes |
|---|---|---|
| New Project | Ctrl+N | Asks for a name, then where to put the project (see the first app below). |
| Open Project... | Ctrl+O | Pick an `.lrp` file. |
| Recent | | The last 10 projects, newest first. |
| Save | Ctrl+S | Writes the project, every form and every code file that changed. |
| Save As... | | Saves under another `.lrp` name or folder; the project is renamed to the file's stem. Saving into another folder refuses to overwrite files already there. |
| Save All | Ctrl+Shift+S | Same as Save: one project is one save. |
| Make `<Project>`.exe... | | A standalone executable (see [Packaging](packaging.md)). |
| Make LazyOS App... | | LazyOS only (see [Packaging](packaging.md)). |
| Close Project, Exit | | Unsaved work gets a Save / Discard / Cancel prompt first. |

**Edit** (the first group acts on the front document; the keys go to the
focused editor, designer or text box themselves)

| Item | Shortcut |
|---|---|
| Undo | Ctrl+Z |
| Redo | Ctrl+Y or Ctrl+Shift+Z |
| Cut, Copy, Paste | Ctrl+X, Ctrl+C, Ctrl+V |
| Delete | Del |
| Select All | Ctrl+A |
| Find... | Ctrl+F |
| Find Next | F3 |
| Replace... | Ctrl+H |
| Go To Line... | Ctrl+G |

Find asks for the text; wrap it in `/.../` to use a regular expression.
Matching ignores case and F3 searches forward. Replace asks for the text to
find, then the replacement, and replaces every match. Go To Line takes a
one-based line number. All four work in the code tab in front.

**View**

| Item | Shortcut | Notes |
|---|---|---|
| Code | F7 | Open the selected form's or module's code. |
| Object | Shift+F7 | Open the selected form's designer. |
| Project, Properties, Toolbox, Output | | Listed, but in this build they only write a line to the Output pane: the panes are always shown. |
| Theme: Light, Dark, System | | Switches at once and is remembered. System follows the desktop (on LazyOS, the desktop theme). |

**Project**

| Item | Notes |
|---|---|
| Add Form | A new form named `form1`, `form2`, ... (the first unused) with a `form_load` stub. Opens its designer. |
| Add Module | A new module named `module1`, ... Opens its code. |
| Remove | Takes the selected item out of the project. Its files stay on disk. If it was the startup item, the first remaining item becomes the startup. |
| Properties | Listed, but only writes a line to the Output pane in this build. Edit the `.lrp` by hand for now (see [Projects](projects.md)). |

**Run**

| Item | Shortcut | Available |
|---|---|---|
| Start | F5 | Idle, or stopped (then it continues) |
| Start with Debugging | Ctrl+F5 | Idle |
| Continue | F5 | Stopped |
| Break | Ctrl+Shift+F5 | Running under the debugger |
| End | Shift+F5 | Running |
| Step Into | F8 | Stopped |
| Step Over | Shift+F8 | Stopped |
| Step Out | Ctrl+Shift+F8 | Stopped |
| Toggle Breakpoint | F9 | Always (needs a code tab in front) |

While a program runs, the commands that edit the project (Add Form, Add Module,
Remove, Replace, and the Edit menu's Undo, Redo, Cut, Paste and Delete) are
refused with a note in Output: press End first. Typing in a code tab stays
possible, but the running program is the copy that was saved at Start.

Window-level chords are ignored while a prompt or file dialog is open, so a
Ctrl+S meant for a text box never saves the project underneath it.

## The Project Explorer

Click a row to select it. A second click on the same row within half a second
(a double-click) opens it: a form opens in its designer, a module in the code
editor. Right-click an item for its menu:

| Entry | Does |
|---|---|
| View Code | Same as F7. |
| View Object | Same as Shift+F7. |
| Rename... | Renames the item and its files. Item names are identifiers: letters, digits and `_`, not starting with a digit. The old files are deleted by the next save, after the new ones are written. |
| Remove | Same as Project > Remove. |
| Set as Startup | Makes the item the one the project runs first. |

## Designing a form

Open a form's designer (double-click it in the explorer, or select it and
press Shift+F7). The form is drawn with the real widgets, so what you see is what
runs, on a dot grid (8 design units).

* **Add a control.** Click a toolbox tile, then drag a rectangle on the form;
  a plain click drops the control at its default size. Double-clicking a tile
  (or pressing Return or Space on it) drops a control in the centre of the
  form instead. A control drawn inside a container such as a GroupBox becomes
  that container's child. The tool stays armed until you click the Pointer
  tile, which is how you go back to selecting.
* **Names.** A new control gets its kind in lower case plus a number:
  `button1`, `label1`, `edit1`, `check_box1`. A Button, Label, CheckBox or
  GroupBox starts with its name as its caption. The name is how scripts refer
  to the control, so rename it (the `(Name)` row in the grid) before you write
  handlers: renaming rewrites the form's `.rhai` handlers to match.
* **Select.** Click a control to select it; Ctrl+click adds it to or removes it
  from the selection; dragging on empty space draws a marquee. Clicking empty
  space selects the form itself, whose handles resize the client area.
* **Move and resize.** Drag a control, or drag one of its eight handles. Both
  snap to the grid. The arrow keys nudge the selection by one grid step, and
  Ctrl+arrow by one unit.
* **Keys in the designer.** Del or Backspace delete, Esc selects the form,
  Ctrl+C, Ctrl+X, Ctrl+V copy, cut and paste, Ctrl+D duplicates, Ctrl+Z and
  Ctrl+Y (or Ctrl+Shift+Z) undo and redo, Ctrl+A selects every control. Every
  change is one undo step.
* **The property grid.** Two columns, name and value, with the object combo at
  the top to pick a control and Alphabetic and Categorized tabs. Click a value
  to edit it: text and numbers open an editor (Return or leaving the cell
  commits, Esc reverts), booleans and enumerations open a check box or a
  dropdown. From the keyboard, Up, Down, Home, End, Page Up, Page Down and Tab
  move the current row, and Return or F2 edits it. Properties that only exist
  at run time are hidden and read-only ones cannot be edited. The properties of
  every control are in [Controls](controls.md).
* **Double-click a control** (or the form) to open its code at the default
  handler, creating the function if it is not there yet. For a Button that is
  `fn button1_click() { }`; for the form it is `form_load`. The new function is
  appended to the end of the file and the caret lands inside its braces.

The file the designer edits is the `.lfm`; its format is described in
[Forms](forms.md).

## Writing code

A form's code tab has two combo boxes above the editor. The **object** combo
lists the form and each control that has events; the **procedure** combo lists
that object's events (`click`, `change`, `load`, ...). Choosing an event that has
no handler yet appends one with the right name and arguments, and the form's
list also offers `reload`, the hot-reload hook `form_reload(old_state)`.
Handler naming and the full event model are in [Scripting](scripting.md).

The editor is a monospace grid with Rhai syntax colouring, bracket matching and
line markers in the gutter (breakpoints, errors, warnings).

| Key | Action |
|---|---|
| Ctrl+Space | Open code completion. It also opens by itself after the second letter of a word and after `.` or `::`. |
| Up, Down, Page Up, Page Down | Move in the completion list. |
| Enter or Tab | Accept the completion (one undo step). |
| Esc | Close the completion list. |
| Tab, Shift+Tab | Indent and outdent. |
| Ctrl+Z, Ctrl+Y, Ctrl+X, Ctrl+C, Ctrl+V, Ctrl+A | The usual editing keys. |

Completion is context aware. After `control.` it offers that control's
properties and methods, after `form.` and `app.` their members, after
`module::` the functions your project module defines, anywhere else the Rhai
keywords, the standard library, your controls, the modules and the functions
and variables the script declares. Nothing is offered inside a comment or a
string.

**Errors show as you type.** About 300 ms after the last keystroke the IDE
compiles the code in the background and underlines parse errors in the editor
and lists them in the **Error List** under Output, as `name (line:column)
message`. Double-click a row (or press Return on it) to jump there. When you
press Start, the whole-project check also adds **lint warnings** (warning icon)
for scripts that compile but are likely to fail at run time:

* a top-level `const` used inside a function of a *module* (Rhai functions
  cannot see it; write `global::NAME`);
* a function whose name matches a method it calls on a value, which makes it
  call itself;
* a `module::function` call to a module that neither the project nor the player
  defines.

Warnings never stop a run. Errors do: Start refuses to launch while the
project has any.

## Running a program

**Start (F5)** saves every unsaved document, checks the whole project (the
same check the player runs), and starts the player as a separate process, so
a crash or an endless loop in your program never freezes the IDE. If the check
finds problems they go to the Error List and nothing starts. Output from
`print` appears in the Output pane. A runtime error that the player reports
opens the failing file with the caret on the line.

**End (Shift+F5)**, closing the program's window, or closing the project, stops
the program.

### Hot reload

When the IDE starts the player with `--watch`, saving a `.lfm` or `.rhai`
file (or the `.lrp`) checks the project and rebuilds the changed form in the
running window, running `form_load` again. A save that does not check keeps the
running form and shows the problems in a banner instead. To carry values over,
define `fn form_reload(old_state) { form.state = old_state; }`: it runs after
`form_load` with the `form.state` the old form left behind. The player polls
the files every 500 ms.

With no breakpoint set, F5 starts the player with `--watch` on a desktop host.
A debug run (below) reloads too. **On LazyOS the IDE does not pass `--watch`
for a plain run**, so a normal F5 run does not reload: save, End and press F5
again, or set a breakpoint to run under the debugger.

## Debugging

Click in a code tab and press **F9** to set a breakpoint on the caret's line (a
dot in the gutter). Press F5: with a breakpoint set, Start runs the program
under the debugger; without one it runs normally. **Start with Debugging
(Ctrl+F5)** always uses the debugger, so Break and stop-on-error work even with
no breakpoint.

When the program stops, the stop line is tinted (warning colour for a
breakpoint, step or Break; error colour for an error), the status bar says
`Break`, and the Locals tab comes to the front.

| Tab | Shows |
|---|---|
| Locals | The variables of the innermost frame: name, type and value. A note says when more were cut to fit. |
| Call Stack | The frames, innermost first (`#0 form_load  main_form.rhai:3`). Double-click a row (or press Return) to open that file and line. |
| Immediate | Type an expression in the box and press **Evaluate**. It runs in the stopped frame, may assign, and the result is listed as `= value (type)` or `! error`. When the program is not stopped it tells you so. |

Commands while stopped: **F5** continues, **F8** steps into a call, **Shift+F8**
steps over, **Ctrl+Shift+F8** steps out, **Shift+F5** ends the program.
**Ctrl+Shift+F5** (Break) asks a running program to stop at the next line it
executes; a program idle between events stops at the first line of its next
handler.

What the debugger can and cannot do:

* A script **error** stops at the failing line with its message and locals;
  continuing lets the error take its normal route (a message box).
* Only the innermost frame has locals and can be evaluated in; the Call Stack
  lists outer frames, but their variables are not available.
* Stepping past the end of a handler ends the stop: there is no "stop in the
  next handler".
* The program's window freezes while it is stopped, as in VB6. The IDE is
  another process and stays responsive.
* Breakpoints belong to the IDE session, not to the project: they follow
  renames and edits but are not saved, and they are forgotten when you close
  the project.
* Line numbers refer to the saved files. Starting a run saves first; if you
  toggle a breakpoint in an unsaved file during a run, the IDE reminds you to
  save so both sides count lines the same way.
* There are no watches and no Breakpoints list yet, and no edit-and-continue.

### The debug protocol

The IDE and the player talk in **JSON lines**: one JSON object per line, every
message carrying `"v":1`, commands on the player's stdin and events on its
stdout (interleaved with the program's own `print` output; a line that is not
a protocol message is plain output). Lines are one-based and files are
project-relative with `/`. The protocol is defined by the crate
`lazyrad-debug-proto`.

| Commands (IDE to player) | Meaning |
|---|---|
| `set_breakpoints` | Replace the breakpoints of one file with a list of lines. |
| `continue`, `step_into`, `step_over`, `step_out` | Resume; only valid while stopped. |
| `pause` | Stop at the next line executed. |
| `stop` | End the program. |
| `evaluate` | Evaluate `expr` in frame 0, echoing an `id`. |

| Events (player to IDE) | Meaning |
|---|---|
| `ready` | The debugger is attached. |
| `stopped` | Reason (`breakpoint`, `step`, `pause`, `exception`), file, line, message, call stack and locals. |
| `resumed` | The program runs again. |
| `evaluated` | The answer to an `evaluate`: `ok`, value and type. |
| `output`, `error` | Text for the Output pane; a problem the debugger reports. |
| `exited` | The program ended, with its exit code. |

A line longer than 256 KiB is dropped, and a `stopped` event is trimmed to
40 KiB (flagged `truncated`). If the IDE goes away while the program is
stopped, the player detaches: it resumes with the debugger off rather than
hang.

## Settings

Settings live in `settings.toml`, read when the IDE starts and written when you
change the theme, open a project and exit. Edit it with the IDE closed.

| Key | Meaning | Default |
|---|---|---|
| `theme` | `"light"`, `"dark"` or `"system"` (also View > Theme). | `"system"` |
| `editor_font_size` | The code editor's size in points. | `12.0` |
| `editor_font_family` | A **monospace** family for the editor. | the platform's (on LazyOS, the bundled JetBrains Mono) |
| `recent_projects` | Project folders, newest first, at most 10. | empty |
| `player_path` | A player executable to use instead of the one next to the IDE (development override). | unset |
| `[panes]` | `toolbox`, `right`, `output` and `project` sizes, in design units. | 140, 240, 160, 220 |

An unreadable file is reported on the error stream and defaults are used; a
missing or partial file is fine.

## A first app: Hello

You will make a window with a button and a label; clicking the button changes
the label.

**1. New project.** Press Ctrl+N. Type a project name (the dialog offers
`MyApp`; a name is letters, digits and `_`, starting with a letter or `_`) and
accept. Then choose where the project goes: on a desktop host a folder picker
asks for the folder; on LazyOS an in-window dialog opens in `~/projects` and you
**type the name of the new folder**. The IDE creates the folder with
`<name>.lrp`, `main_form.lfm` and `main_form.rhai`, and opens the form's
designer. It refuses a folder that already holds a project.

**2. Add a Button.** Click the **Button** tile in the toolbox, then drag a
rectangle on the form. It is named `button1`.

**3. Add a Label.** Click the **Label** tile and drag another rectangle. It is
named `label1`. Click the **Pointer** tile to stop drawing.

**4. Set the captions.** Select the button and, in the Properties grid, set
`text` to `Say hello`. Select the label and clear its `text`. Click empty form
space and set the form's `title` to `Hello`.

**5. Create the handler.** Double-click the button. The code tab
`main_form.rhai` opens with a new function and the caret inside it. Type:

```rhai
fn button1_click() {
    label1.text = "Hello from LazyRAD!";
}
```

Handlers are named `<control>_<event>`; the control is a variable named after
it, and properties are lower case with underscores, exactly as the grid shows
them.

**6. Run it.** Press F5. The project is saved and checked, and the program
opens in its own window. Click **Say hello**.

**7. Change it while it runs** (desktop host, where F5 passes `--watch`): edit
the text in the handler and press Ctrl+S; the window rebuilds. On LazyOS press
Shift+F5 and F5 again.

**8. Stop on a line.** In the code tab put the caret on the `label1.text` line,
press F9 and F5. When the program stops, look at Locals, type `label1.text`
into the Immediate box and press Evaluate, then press F5 to continue.

To read a name typed by the user, add an Edit (`edit1`) and change the handler
to:

```rhai
fn button1_click() {
    label1.text = `Hello, ${edit1.text}!`;
}
```

When it works, [Packaging](packaging.md) turns it into an installable app.

## On LazyOS

| Topic | LazyOS |
|---|---|
| Where projects live | `~/projects` (created by the IDE; the file dialog starts there, then the home folder, then `/transient`). |
| Where settings live | `~/.apps/os.lazy.lazyrad/config/settings.toml`. A project you run from the IDE keeps its own files in `~/.apps/os.lazy.lazyrad/data/`. |
| The samples | Read-only copies are in `/system/share/lazyrad/` (see [Projects](projects.md)). Copy one into `~/projects` to edit it: `cp -r /system/share/lazyrad/hello ~/projects/`. |
| New Project, Open, Save As | Use xui's in-window file dialog, not a native one. New Project has no folder picker: you name the new folder. |
| Run | The player is `lrplay`, installed beside the IDE. The IDE passes `--client` always and `--debug` for a debug run; a plain run has no hot reload (see Hot reload). |
| Run permissions | The packaged IDE runs your project under a development label with the permissions the installed app would get. The first Play (and any Play that needs more than you already allowed) opens the **Package Installer** to approve them; the approval lasts until you log out. See [Packaging](packaging.md). |
| Make LazyOS App | Builds a `.lzp` and hands it to the Package Installer ([Packaging](packaging.md)). |
| Messenger | Scripts can call system services with `sys::*` and `msg::*`; see [LazyOS modules](lazyos.md) and [msg](../rhai/msg.md). |
| Responsiveness | Under QEMU emulation the IDE can be slow to react in a large window; give it a moment after a keystroke. |
| Evidence | The IDE prints serial markers such as `LRIDE:UP:PASS`, `LRIDE:OPEN:PASS:<project>`, `LRIDE:RUN:PASS:<project>` and, in the debugger, `LRIDE:BREAK:PASS:<item>:<line>`. |

The player's window freezes while a debug run is stopped, and it takes the
keyboard focus when it opens: click the IDE before using its shortcuts.

## See also

* [LazyRAD](index.md): the hub for these pages.
* [Projects](projects.md) and [Packaging](packaging.md): the other two thirds
  of this tour.
* [Forms](forms.md), [Controls](controls.md), [Scripting](scripting.md) and
  [The standard library](stdlib.md): what you draw and what you write.
* [Cookbook](cookbook.md): worked examples.
* [LazyRAD on LazyOS](../lazyrad-plan.md) and
  [LazyRAD as a package](../lazyrad-package-plan.md): design and status.
