# `os.lazy.shell.tray.v1`

Interface id: `0xe125dc0e9d908624`

The taskbar tray (docs/tray-plan.md), served by LazyShell on Messenger
under the name `os.lazy.shell.tray`.

The shell owns the tray: rendering, order, overflow, menus and policy.
There is **one item per app**, keyed by the caller's kernel-stamped label
(the `init` app id for an unlabelled built-in), so there is nothing to
name or count: `Set` creates or replaces the app's item and `Clear`
returns it to the default (a running resident app keeps a default item,
any other app leaves the tray). The tooltip header, the menu header and
the shell-added Quit row show the registry name `init.ListApps` gives that
label, never app text.

Callers must be in the shell's login session (kernel-stamped
`cred.session`) and be uid 0 or the shell's uid; a labelled app also needs
this interface in its manifest (implied by `resident`). Anyone else gets
`EACCES`. Every field is validated: a tooltip of at most 256 characters,
a badge of at most 3, known `Status`/`Activation`/`MenuKind` values, at
most 64 menu rows of at most 128 characters with unique non-zero ids,
`parent` naming an earlier top-level `Submenu` row (depth at most 2), at
most one `is_default` row (required by `DefaultItem`); anything else is
`EINVAL`. An unusable icon is not an error: it falls back (see `Icon`).
A session holds at most 64 items (`ENOSPC`). Failures reply with the
standard error field (id 15, `docs/midl.md`).

## Methods

| Method | Id | Kind | Signature |
|---|---|---|---|
| Set | 1 | sync | `(item: Item, events: Channel<os.lazy.shell.tray.events.v1>) -> ()` |
| Update | 2 | sync | `(icon: Option<Icon>, tooltip: Option<String>, status: Option<U32>, badge: Option<String>, menu: Option<Menu>) -> ()` |
| Clear | 3 | sync | `() -> ()` |

## Objects

Kernel objects a request carries, in the order of the parcel's
object list (the index each field must hold).

| Method | Field | Type | Object |
|---|---|---|---|
| Set | `events` | `Channel<os.lazy.shell.tray.events.v1>` | `objects[0]`, a channel the receiver sends `os.lazy.shell.tray.events.v1` on |

## Topics

| Topic | Payload | QoS | Retained | Permissions |
|---|---|---|---|---|
| `session/+/shell/tray` | `Generation` | latest | yes | `publish:session/+/shell/tray`, `subscribe:session/+/shell/tray` |

## struct `Item`

- `icon: Icon`
- `tooltip: String`
- `status: U32`
- `badge: Option<String>`
- `menu: Array<MenuItem>`
- `activate: U32`

## struct `Icon`

- `lucide: Option<String>`
- `mask: Option<Image>`
- `pixels: Array<Image>`
- `file: Option<String>`

## struct `Image`

- `width: U32`
- `height: U32`
- `data: Bytes`

## struct `Menu`

- `rows: Array<MenuItem>`

## struct `MenuItem`

- `id: U32`
- `parent: U32`
- `label: String`
- `kind: U32`
- `enabled: Bool`
- `checked: Bool`
- `is_default: Bool`

## struct `Generation`

- `generation: U64`

## enum `Status`

- Active, Passive, Attention

## enum `MenuKind`

- Normal, Check, Radio, Separator, Submenu

## enum `Activation`

- Event, Menu, DefaultItem
