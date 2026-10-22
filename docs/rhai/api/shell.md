# `sys::shell`

The Rhai module for the Messenger interface `os.lazy.shell.v1`. Wire-level reference: [os.lazy.shell.v1](../../idl/os.lazy.shell.v1.md).

LazyShell, the desktop shell (issue #157), published on Messenger under the
name `os.lazy.shell`.

LazyShell is a UI client of the compositor (`os.lazy.display.v1`), `init`
(app registry, `Launch`) and `confd` (`sys/ui/*`); it decides nothing about
authority. This interface lets scripts, tests and other session programs
drive the desktop the way a user would (open the start menu, launch an app
as if its menu row or desktop icon was activated, reload the menu and the
desktop icons) and read what the shell currently shows.

Callers must be uid 0 or share the shell's uid (kernel-stamped
credentials); anyone else gets `EACCES`. Failures are returned as a
structured error field (id 15, errno-style code plus friendly text) instead
of the declared reply fields.

## Functions

### `status()`

`Status() -> (windows: Array<TaskbarEntry>, focused: Option<U64>, menu_open: Bool, menu: Array<Launcher>, desktop: Array<Launcher>, tray: Array<TrayEntry>)`

What the shell shows right now: the taskbar's window entries (in
taskbar order), the focused window, whether the start menu is open, the
start-menu rows, the desktop icons and the tray items (in bar order,
docs/tray-plan.md).

Returns a map:

| Name | IDL type | Script value |
|---|---|---|
| `windows` | `Array<TaskbarEntry>` | array of map (`TaskbarEntry`) |
| `focused` | `Option<U64>` | int or () |
| `menu_open` | `Bool` | bool |
| `menu` | `Array<Launcher>` | array of map (`Launcher`) |
| `desktop` | `Array<Launcher>` | array of map (`Launcher`) |
| `tray` | `Array<TrayEntry>` | array of map (`TrayEntry`) |

```rhai
let result = sys::shell::status();
```

### `show_start_menu(open)`

`ShowStartMenu(open: Bool) -> ()`

Open (`open` true) or close the start menu.

Arguments:

| Name | IDL type | Script value |
|---|---|---|
| `open` | `Bool` | bool |

Returns `()`.

```rhai
let result = sys::shell::show_start_menu(open);
```

### `launch(app)`

`Launch(app: String) -> (pid: U64)`

Launch the registry app `app` through `init.Launch` in the shell's
session, exactly as clicking its start-menu row would (including the
launch-origin zoom). `ENOENT` when the app is unknown or not shipped.

Arguments:

| Name | IDL type | Script value |
|---|---|---|
| `app` | `String` | string |

Returns `pid`: `U64` (int).

```rhai
let result = sys::shell::launch(app);
```

### `refresh()`

`Refresh() -> (menu: U32, desktop: U32)`

Re-read the start menu (`sys/ui/menu` plus installed apps) and the
desktop icons (the desktop folder, `$HOME/Desktop`, or `sys/ui/desktop`
without one); returns how many rows each has.

Returns a map:

| Name | IDL type | Script value |
|---|---|---|
| `menu` | `U32` | int |
| `desktop` | `U32` | int |

```rhai
let result = sys::shell::refresh();
```

### `activate(surface)`

`Activate(surface: U64) -> ()`

Activate (restore, raise, focus) the window `surface` as a click on its
taskbar entry would. `ENOENT` when the taskbar has no such entry.

Arguments:

| Name | IDL type | Script value |
|---|---|---|
| `surface` | `U64` | int |

Returns `()`.

```rhai
let result = sys::shell::activate(surface);
```

## Types

### `TaskbarEntry`

One taskbar entry.

| Name | IDL type | Script value |
|---|---|---|
| `surface` | `U64` | int |
| `title` | `String` | string |
| `minimized` | `Bool` | bool |

```rhai
let value = sys::shell::new_taskbar_entry();
```

### `Launcher`

The compositor's surface id.
The window title.
Whether the window is minimized.
One start-menu row or desktop icon: the `init` registry app it
launches (for a desktop icon that is not an app shortcut, its absolute
path in the desktop folder) and the label it shows.

| Name | IDL type | Script value |
|---|---|---|
| `app` | `String` | string |
| `label` | `String` | string |

```rhai
let value = sys::shell::new_launcher();
```

### `TrayEntry`

One tray item.

| Name | IDL type | Script value |
|---|---|---|
| `app` | `String` | string |
| `tooltip` | `String` | string |
| `status` | `U32` | int |
| `custom` | `Bool` | bool |
| `visible` | `Bool` | bool |

```rhai
let value = sys::shell::new_tray_entry();
```

## See also

- [Messenger APIs for Rhai](index.md)
- [msg: Messenger from Rhai](../msg.md)
- [os.lazy.shell.v1 on the wire](../../idl/os.lazy.shell.v1.md)
