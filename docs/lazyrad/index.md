# LazyRAD

LazyRAD is the way to write a LazyOS app without writing Rust. You draw a form,
double-click a button, write a Rhai handler, press F5, debug it, and package it
as an installable app. The IDE is `lazyrad`, the program that runs a project is
the player `lrplay`, and both ship as the core package `os.lazy.lazyrad`.

An app is two small files you edit: a **form** (`.lfm`, the window and its
controls) and a **script** (`.rhai`, what happens when you click, type or a
timer fires), tied together by a **project** (`.lrp`).

```rhai
// main_form.rhai: runs when hello_button is clicked
fn hello_button_click() {
    result_label.text = "Hello, " + name_edit.text + "!";
}
```

## Where to go

| I want to | Read |
|---|---|
| draw my first form and run it | [The IDE](ide.md) |
| understand the files in a project | [Projects](projects.md) |
| know what a control can do | [Forms](forms.md) and the [Controls reference](controls.md): [input](controls-input.md), [choices](controls-choices.md), [display](controls-display.md), [Canvas](controls-canvas.md), [PictureBox](controls-picturebox.md) |
| know how a script runs and what it can call | [Scripting](scripting.md), [Standard library](stdlib.md) |
| use LazyOS services, sound or Messenger from a form | [LazyOS-only modules](lazyos.md), [Messenger API reference](../rhai/api/index.md) |
| install my app or share it | [Packaging](packaging.md) |
| copy a working recipe | [Cookbook](cookbook.md) |
| learn the language | [The Rhai language](../rhai/language.md) |

## How the pieces fit

| Piece | What it is |
|---|---|
| Form (`.lfm`) | A TOML document: the window and a flat list of controls with their properties and anchors |
| Script (`.rhai`) | Rhai code; a function named `<control>_<event>` is that control's handler |
| Project (`.lrp`) | Lists the forms, scripts and resources and names the start form |
| Player (`lrplay`) | Builds the form, binds the script, runs the window; also the stub inside every `.lzp` |
| IDE (`lazyrad`) | Designer, property grid, code editor, debugger, Make LazyOS App |
| Package (`.lzp`) | The installable result: player, project and manifest, with the permissions the script needs |

## See also

- [Rhai on LazyOS](../rhai/index.md)
- [msg: Messenger from Rhai](../rhai/msg.md)
- [Packages](../packages.md)
- [LazyRAD plan and architecture](../lazyrad-plan.md)
