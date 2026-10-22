# Packaging and sharing LazyRAD apps

When a project works you can hand it to someone else in one of three ways: as
a **LazyOS app** (an installable `.lzp` package, which shows up in the Start
menu like any other app), as a **standalone executable** on a desktop host, or
as the project folder itself. This page covers the first two, how an app is
installed and what it is allowed to do, the `lrplay` command line, where an
app's files live and what happens when an app fails.

## Which one do you want?

| You want | Use | Where |
|---|---|---|
| An app on LazyOS, in the Start menu, with its own permissions | **File > Make LazyOS App** | The IDE on LazyOS |
| The same, from a script or a build | `tools/lazyrad/package.py`, the `lzpack` example, or LazyRAD's `lazyrad-pack` | A host with the LazyOS repository (`lazyrad-pack`: the LazyRAD repository) |
| One executable file for a desktop OS | **File > Make `<Project>`.exe** | The IDE on a desktop host |
| Just run it, no install | `lrplay --client <project folder>` | LazyOS Terminal |
| Hand over the source | Copy the project folder ([Projects](projects.md)) | Anywhere |

## Make LazyOS App

**File > Make LazyOS App...** appears only when the IDE can install packages,
which means on LazyOS. Nothing is installed until you agree.

1. **Save and check.** Every unsaved document is saved, then the whole project
   is checked exactly as a run would check it. Errors stop the build and go to
   the Error List; lint warnings do not.
2. **Build the package.** The IDE copies the player (`lrplay`, found beside the
   IDE) and your project into a `.lzp`, with a generated manifest whose
   permissions come from what your scripts use (see "Permissions" below).
3. **Pre-check.** The package is opened and judged by the same code the
   installer uses. Any problem it finds is written to the Output pane as
   "The app cannot be installed: ..." and the build stops.
4. **First consent.** A dialog titled `Install <Name> <version>?` shows who made
   it (`By <author>. System name: <id>.`) and every permission as
   `[risk] explanation (kind: value)`, or "It asks for no special
   permissions." Choose **Install** or **Cancel**.
5. **The Package Installer.** The IDE stages the package as
   `/transient/lazyrad-<system_name>-<version>.lzp` and asks `mimed` to open it
   with the `install` verb. The Package Installer opens in your session, shows
   the same permissions grouped by risk (high first) and installs when you agree
   there (Review, then Permissions, then Install). It is a second, independent
   consent step: the IDE never installs anything itself, because the package
   manager refuses every request from an app that carries a label, and the
   Installer, which runs unlabelled, does the install.
6. **Run it?** Once the Installer reports success the IDE asks "Run it now?"
   and starts the new app through `init`. The staged file is deleted.

Things to know:

* The IDE's window does not repaint while it waits for the Installer. Finish
  (or cancel) in the Installer window. A cancel publishes nothing, so the IDE
  reports "was not installed: the Package Installer reported nothing" after five
  minutes.
* A given version installs once. To install a changed app, raise `version` in
  the `.lrp` first; "This version of the app is already installed." means you
  did not.
* The author is your user name (`$USER`, or `lazyos`). It is shown at install
  time and is **not verified**.
* The Start menu files a LazyRAD app under its default category (Accessories),
  because the manifest sets none.

On a desktop host, **Make LazyOS App** is not offered. Use `lazyrad-pack` or
the LazyOS repo's scripts below.

## What is in a package

A package is a standard LazyOS application package ([Application
packages](../packages.md)); LazyRAD invents no format of its own.

```text
manifest.toml
bin/lrplay.elf                   the player: a copy of the one beside the IDE
icons/app-16.png                 generated: a coloured tile whose hue comes
icons/app-32.png                 from a hash of the project name, so apps
icons/app-128.png                look different in the Start menu
resources/project/<name>.lrp     the project, exactly as it is on disk
resources/project/<items>        every .lfm and .rhai the .lrp lists
resources/project/<assets>       the files the `assets` globs match, under
                                 their project-relative paths
```

Only what the `.lrp` lists goes in: an extra `.rhai` or `.txt` lying in the
project folder is left out.

The generated manifest looks like this (here for a project `Todo` made by
`ada`, whose scripts use no files and no services):

```toml
[app]
name = "Todo"
system_name = "user.ada.todo"
author = "ada"
version = "1.0.0"

[entry]
binary = "bin/lrplay.elf"
abi = "linux"
args = ["--project", "resources/project"]

[permissions]
interfaces = ["os.lazy.display.v1", "os.lazy.display.cursor.v1", "os.lazy.input.v1"]
topics = []
files = []
network = []
```

| Field | Where it comes from |
|---|---|
| `name` | The project name; 1 to 64 characters. |
| `system_name` | `user.<author>.<project>`, each part lower-cased to ASCII letters and digits with other runs turned into one `-` and cut at 48 bytes. It names the install directory, the app's data folder and its security label. The IDE cannot change it; the command-line tools take `--system-name`. If the author or project name has no ASCII letter or digit, the build fails and says so. |
| `author` | `$USER` in the IDE, `LazyOS` in `lzpack`, or `--author`. 1 to 128 characters. |
| `version` | The `.lrp` `version`, which must be `MAJOR`, `MAJOR.MINOR` or `MAJOR.MINOR.PATCH` with each part below 65536; it is written as three parts (`1` becomes `1.0.0`). Anything else is reported ("project version ... is not MAJOR.MINOR.PATCH"). |
| `description` | Optional, up to 1024 characters; set by the command-line tools only. |
| `entry.args` | Always `["--project", "resources/project"]`; the player resolves the path against its own install directory (see `lrplay`). |
| `entry.abi` | Always `linux`: the player is a static musl program, which `init` must start in the Linux personality. |

All problems with a manifest are collected and reported together, so you fix
them in one pass. An icon in the `.lrp` is not used for a package.

## Permissions

An installed app runs under kernel rules compiled from its manifest, so the
package must declare every system service its scripts call. You do not write the
list: Make LazyOS App reads your scripts and declares what it finds, and the
consent screens show the result with the system's own explanations.

| What a script does | What the manifest declares |
|---|---|
| Always (the player itself) | `os.lazy.display.v1` (its window), `os.lazy.input.v1` (the keyboard) and `os.lazy.display.cursor.v1` (the pointer shape) |
| Calls a `file_*` or `dir_*` function (also by name, as in `Fn("file_read_text")`) | `files`: `read:$HOME/.apps/<system_name>` and `write:$HOME/.apps/<system_name>` (private storage; see "Where data lives" below) |
| Calls `sys::<alias>::...` | The alias's interface: `sys::confd::get(...)` declares `os.lazy.confd.v1` |
| Calls a topic helper such as `sys::confd::on_changed("sys/ui/#", ...)` | A topic rule, narrowed by the literal arguments: `subscribe:system/confd/changed/sys/ui/#` |
| Passes a **string literal** to `msg::connect`, `msg::on`, `msg::subscribe` or `msg::publish` | That interface or topic |
| Calls `modplay::...` (the sound module) | `os.lazy.audio.v1` |
| Anything about the network | Nothing: `network` is always empty |

The scan reads the token text of every `.rhai` file in the package. It cannot
see a name built at run time (`msg::connect("os.lazy." + name)`): such a call is
refused when the app runs, with a `permission denied (EACCES)` error. A topic
helper whose arguments are built at run time keeps its bare pattern, which the
kernel's per-segment check refuses past the wildcard.

An installed app owns only the names that start with `app.<system_name>.`, so
serving any other name with `msg::serve` (the `messenger` sample serves
`demo.lazyrad`) is refused: wrap it in `try`/`catch`, as the sample does. The
[Messenger from Rhai](../rhai/msg.md) page describes `sys::*` and `msg::*`, and
[LazyOS modules](lazyos.md) lists what a form script can reach.

The `files` rule is shown to you at install time. What actually holds a script
to its folder is the player's own sandbox (see below); the kernel does not
enforce the rule yet.

## The `lrplay` command line

`lrplay` is the player: it runs one project as a window of the desktop. It is
also the stub inside every package. Its command line is parsed strictly, and
every path is treated as untrusted input.

```text
lrplay [--client] [--debug] [--project <path> | <path>...] [attempt=N]
```

| Argument | Meaning |
|---|---|
| `--client` | Run as a client of the desktop compositor. `init` passes it to every desktop app; use it from the Terminal too. |
| `--debug` | Run under the IDE's debugger: speak the [debug protocol](ide.md) on standard input and output. The IDE passes it; you would not. |
| `--project <path>` or `--project=<path>` | The project folder or `.lrp`. |
| `<path>` | A positional path that is an `.lrp` file or a directory is the project (given once, either way). Any other path is a **document**: a file the app was asked to open, handed to scripts as `app.documents` (up to 64). |
| `attempt=N` | `init`'s retry counter (and its background-start flag): accepted and ignored. |

Any other flag is refused (`unknown option`); there is no `--watch` and no
`--check` on LazyOS.

Where the project is:

* an **absolute** path is used as given;
* a **relative** `--project` is joined to the player's **install directory**
  (the parent of the `bin/` folder holding `lrplay.elf`) and may not contain
  `..`; for a package, `resources/project` therefore resolves to
  `/apps/<system_name>/<version>-<hash>/resources/project`;
* with no project at all, `<install>/resources/project` is used when it exists.

The player finds its install directory from `argv[0]` (on LazyOS
`current_exe()` answers `/busybox`), which is why `init` starts an installed
app by its absolute path. A project that does not exist stops the player with
`LRPLAY:ARGS:FAIL:no project at <path>`.

From the Terminal, with the player of the LazyRAD package:

```bash
P=$(echo /apps/os.lazy.lazyrad/*/bin/lrplay.elf)
$P --client /system/share/lazyrad/hello &
$P --client ~/projects/hello &
```

(`$P` is `lrplay.elf` in the IDE's install directory; the directory name holds
the version and a hash, so use the glob.) The player writes its evidence to the
console as `LRPLAY:<stage>:PASS|FAIL`: `PROJECT`, `DATA` (the folder scripts may
write), `DOCUMENT`, `MSG`, `UP` (the first frame reached the compositor),
`EVENT` (the first handler ran), `DEBUG`, `EXIT`, and `FAIL` for `ARGS`, `BIND`
or `RUN`.

## Installing and removing from the command line

On LazyOS an `.lzp` can be installed without the IDE:

```bash
cp /system/share/samples/modplayer.lzp ~/    # present in images built with --modplayer
pkgctl inspect ~/modplayer.lzp        # what it declares and asks for; changes nothing
pkgctl install ~/modplayer.lzp
pkgctl list
pkgctl remove org.lazy.modplayer
```

Opening a `.lzp` in Files or the desktop starts the Package Installer on its
Review step. The installer lists the apps you installed, each with a **Remove**
button. See [Application packages](../packages.md) for the format and the
installer.

## Building a package from a script

On a host with the LazyOS repository:

```bash
python tools/lazyrad/package.py                                 # the MOD player -> target/pkg/modplayer.lzp
python tools/lazyrad/package.py --project lazyrad-os/samples/hello --out hello.lzp \
    --system-name user.me.hello
python tools/lazyrad/package.py --app modplayer --no-build      # reuse target/lazyrad/lrplay.elf
```

`package.py` builds the player (`tools/lazyrad/build.py`) unless `--no-build`,
then runs the `lzpack` example, which checks the project, derives the
permissions the way the IDE does (so a song-playing app gets
`os.lazy.audio.v1`) and writes the package. `--require` turns a missing musl
toolchain into an error. `--project` needs `--out`; `--system-name` and
`--description` set what the IDE cannot. `lzpack` itself also takes `--author`.

LazyRAD's own `lazyrad-pack` does the same without asking the LazyOS platform,
so it declares only the display interface and the private-folder rule, not the
Messenger services your scripts call:

```bash
lazyrad-pack <project dir | .lrp> --player lrplay.elf [--out dist] \
    [--author NAME] [--system-name ID] [--description TEXT] [--no-check]
```

It writes `<out>/<system_name>-<version>.lzp`, compiles the project first
unless `--no-check`, and exits 0 on success, 1 when the build failed, 2 on a
usage error.

## Standalone executables

**File > Make `<Project>`.exe...** exports a self-contained program on a desktop
host: a copy of the player with the project appended as a payload and a footer.
The program finds the payload in its own file at start-up and runs the project
from memory; nothing is extracted. The IDE saves, checks, asks where to write
the file (suggesting `<name>` plus the platform's executable suffix) and writes
it atomically.

* On Windows the copy is patched to the GUI subsystem (no console window), gets
  the `.lrp`'s `icon` (an `.ico`) and a version resource made from `name` and
  `version`. On Unix the file is marked executable.
* The payload holds the `.lrp`, every `.lfm` and `.rhai` and the assets. It is
  checked on load (version, sizes, a checksum, plain file names), so a damaged
  file is reported instead of crashing. The checksum catches damage, not
  tampering: the payload is the program.
* Appending a payload invalidates any code signature the player had. Code
  signing is out of scope.
* The IDE finds the player stub through the `player_path` setting, else the
  `lazyrad-player` beside the IDE.

On LazyOS the menu item exists but this route has not been exercised there,
and the exported program looks for its payload through `current_exe()`, which
LazyOS answers wrongly (see the `lrplay` section). Use Make LazyOS App on LazyOS.

## Where data lives

Everything LazyRAD writes lives in the home of the user running it (`$HOME`,
which `init` gives every session app). Installed apps never write inside
`/apps`, which belongs to the package manager.

| What | Where |
|---|---|
| Projects (the file dialog's first stop) | `~/projects/`, created by the IDE |
| IDE settings | `~/.apps/os.lazy.lazyrad/config/settings.toml` |
| Files of a project run from the IDE, or from a shell with `lrplay` | `~/.apps/os.lazy.lazyrad/data/` |
| Files of an **installed** app | `~/.apps/<system_name>/` (the `files` permission) |
| The installed program, its project and assets | `/apps/<system_name>/<version>-<hash>/` (read-only) |
| A package waiting for the Installer | `/transient/lazyrad-<system_name>-<version>.lzp`, deleted afterwards |

A script's file functions are confined by the player's sandbox. A relative path
names a file in the data folder, except that a read of a file or asset the
project ships is served from the project first. Besides its data folder a
script may read its own project (read-only) and each document the player was
started to open, together with the files directly beside that document (not
its subfolders); a file the user picks in `open_file_dialog` is granted while
the program runs. Everything else is refused with an ordinary script error you
can catch.

If `$HOME` is not set (a program started outside a session), LazyRAD falls back
to `/transient/lazyrad`, a RAM file system, and says so with
`LRPLAY:HOME:WARN` or `LRIDE:HOME:WARN`: nothing is kept across a reboot.

The first time the IDE starts after an upgrade it moves an old
`~/.apps/lazyrad` folder to `~/.apps/os.lazy.lazyrad`.

## Development runs

When you press F5 in the packaged IDE, the project does not run with the IDE's
own permissions. It runs as a child labelled `dev:<system_name>`, which gets the
**same names, topics and rules the installed app would get**, so what works in
Play works when installed, and what would be refused is refused now (including
`msg::serve` of a name outside the app's own). This needs the IDE's manifest to
say `develop = true`, which it does.

1. The IDE builds the package the way Make LazyOS App does and stages it in
   `/transient`.
2. It asks `mimed` to open it with the `develop` verb. The Package Installer
   shows "Run `<app>` from your development environment?" with every permission
   grouped by risk. **Allow** approves; **Cancel** (or Esc, or closing it)
   refuses and the IDE reports "The run was refused".
3. Once approved, the IDE starts the player under the label, keeping its output
   pipes, and the Output pane shows "Running under dev:<system_name>".

The approval is kept in memory for your session and is dropped when you log
out. A run that needs the same or fewer permissions than one you already
approved opens no window; one that needs more asks again. The IDE shows
"Waiting for approval..." while it waits (up to five minutes). A project that
claims the name of a built-in app is refused.

An IDE that was started without a package label (from the Terminal, in an image
without the LazyRAD package) forks the player directly and the project runs with
the IDE's own permissions instead; the Output pane says so when a Messenger
call is refused. Developers can run one development run with no window:
`lazyrad --play-dev <project>` prints `LRIDE:PLAY:OUT:<line>` for each line the
program printed. The same mechanism is described in [Application
packages](../packages.md).

## When an app fails

The player reports a problem it cannot continue from as one line on its error
stream, then exits: code **1** if the project does not load, validate or compile
(a missing file, a bad `.lfm`, a script that does not parse) and code **2** after
a fatal runtime error, such as `form_load` throwing. A runtime error inside an
event handler is not fatal: the player shows it in a message box and the program
keeps running.

For an installed app that fails while starting, `init` does **not** keep
restarting it, and the desktop shows one notice instead: an Error toast saying
the app stopped unexpectedly, with the app's name, its exit status and the
reason, and **Restart** and **Close** buttons. It stays until you dismiss it.
The reason is the last problem the player reported on its error stream, which
it hands to `init` just before it exits (`LRPLAY:REASON:PASS:<text>` on the
console). An app that keeps crashing is treated the same way.

The `crashload` sample is a project whose `form_load` always throws; it exists
to test this notice. To reproduce it, package it, then install the result with
`pkgctl install` and start it:

```bash
python tools/lazyrad/package.py --app crashload
```

## Limits

| Limit | Value |
|---|---|
| Size of the package Make LazyOS App will hand over | 8 MiB (the player is several MiB, about 2 MiB once compressed) |
| One project file or asset | 8 MiB |
| All files of a package together, uncompressed (the player included) | 64 MiB |
| Entries in a package | 1024 |
| Longest file name inside a package or payload | 255 bytes |
| The player copied into a package | 16 MiB |

## See also

* [LazyRAD](index.md): the hub for these pages.
* [The IDE tour](ide.md) and [Projects](projects.md).
* [LazyOS modules](lazyos.md) and [Messenger from Rhai](../rhai/msg.md): what
  your scripts can call, and so what the permissions list will contain.
* [Application packages](../packages.md): the `.lzp` format, the installer and
  development runs.
* [LazyRAD as a package](../lazyrad-package-plan.md) and
  [LazyRAD on LazyOS](../lazyrad-plan.md): design and status.
