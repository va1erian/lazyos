# `os.lazy.shell.v1`

Interface id: `0x591939ff6e05f1c8`

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

## Methods

| Method | Id | Kind | Signature |
|---|---|---|---|
| Status | 1 | sync | `() -> (windows: Array<TaskbarEntry>, focused: Option<U64>, menu_open: Bool, menu: Array<Launcher>, desktop: Array<Launcher>)` |
| ShowStartMenu | 2 | sync | `(open: Bool) -> ()` |
| Launch | 3 | sync | `(app: String) -> (pid: U64)` |
| Refresh | 4 | sync | `() -> (menu: U32, desktop: U32)` |
| Activate | 5 | sync | `(surface: U64) -> ()` |

## struct `TaskbarEntry`

- `surface: U64`
- `title: String`
- `minimized: Bool`

## struct `Launcher`

- `app: String`
- `label: String`
