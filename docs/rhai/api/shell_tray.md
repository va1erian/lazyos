# `sys::shell_tray`

The Rhai module for the Messenger interface `os.lazy.shell.tray.v1`. Wire-level reference: [os.lazy.shell.tray.v1](../../idl/os.lazy.shell.tray.v1.md).

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

## Functions

### `update(icon, tooltip, status, badge, menu)`

`Update(icon: Option<Icon>, tooltip: Option<String>, status: Option<U32>, badge: Option<String>, menu: Option<Menu>) -> ()`

Replace the given parts of the app's item; absent fields are kept
(an empty `badge` removes the badge). `ENOENT` before `Set`.

Arguments:

| Name | IDL type | Script value |
|---|---|---|
| `icon` | `Option<Icon>` | map (`Icon`) or () |
| `tooltip` | `Option<String>` | string or () |
| `status` | `Option<U32>` | int or () |
| `badge` | `Option<String>` | string or () |
| `menu` | `Option<Menu>` | map (`Menu`) or () |

Returns `()`.

```rhai
let result = sys::shell_tray::update(icon, tooltip, status, badge, menu);
```

### `clear()`

`Clear() -> ()`

Drop the app's custom item: a running resident app goes back to its
default item, any other app leaves the tray. Not an error without one.

Returns `()`.

```rhai
let result = sys::shell_tray::clear();
```

Not callable from a script (the request carries a kernel object): `Set`.

## Types

### `Item`

An app's item.

| Name | IDL type | Script value |
|---|---|---|
| `icon` | `Icon` | map (`Icon`) |
| `tooltip` | `String` | string |
| `status` | `U32` | int |
| `badge` | `Option<String>` | string or () |
| `menu` | `Array<MenuItem>` | array of map (`MenuItem`) |
| `activate` | `U32` | int |

```rhai
let value = sys::shell_tray::new_item();
```

### `Icon`

The picture; an unusable one falls back (see `Icon`).
Text under the verified app name in the tooltip; may be empty.
A `Status` value: `Passive` items go to the overflow first,
`Attention` pulses.
A short count or mark drawn over the icon (at most 3 characters).
The menu rows, in order; the shell adds the Quit row after them.
An `Activation` value: what a primary click does.
The item's picture: exactly one source. An empty `Icon`, one with
several sources, a Lucide name the named-icon library (`lazyicons`,
`docs/icons.md`) does not know, an image larger than 64 x 64 or whose
`data` is not `width * height * 4` bytes, or a `file` name with a
directory part falls back to the app's
package icon (`icons/app-16.png`, `app-32.png` at 2x), else to the
Lucide `app-window` outline, so an item never lacks a picture.

| Name | IDL type | Script value |
|---|---|---|
| `lucide` | `Option<String>` | string or () |
| `mask` | `Option<Image>` | map (`Image`) or () |
| `pixels` | `Array<Image>` | array of map (`Image`) |
| `file` | `Option<String>` | string or () |

```rhai
let value = sys::shell_tray::new_icon();
```

### `Image`

A Lucide outline by its kebab-case name (`volume-2`), tinted with
the bar's ink: the easy choice for an app without art.
An alpha-only image (`data` holds RGBA; only alpha is used),
tinted like a Lucide outline.
Full-colour RGBA at 1x and optionally 2x (at most two images),
drawn as given; the shell picks the one closest to the cell size.
Empty: no pixels.
A PNG under the app's own install directory's `icons/` (a bare
file name, no `/`).
Straight RGBA8 pixels, rows tightly packed.

| Name | IDL type | Script value |
|---|---|---|
| `width` | `U32` | int |
| `height` | `U32` | int |
| `data` | `Bytes` | blob |

```rhai
let value = sys::shell_tray::new_image();
```

### `Menu`

A whole menu, as `Update` replaces it (a wrapper so an empty menu is
told apart from an absent one).

| Name | IDL type | Script value |
|---|---|---|
| `rows` | `Array<MenuItem>` | array of map (`MenuItem`) |

```rhai
let value = sys::shell_tray::new_menu();
```

### `MenuItem`

One menu row. `id` is what `MenuItem` reports back; `parent` is 0 for
a top-level row or the id of an earlier `Submenu` row.

| Name | IDL type | Script value |
|---|---|---|
| `id` | `U32` | int |
| `parent` | `U32` | int |
| `label` | `String` | string |
| `kind` | `U32` | int |
| `enabled` | `Bool` | bool |
| `checked` | `Bool` | bool |
| `is_default` | `Bool` | bool |

```rhai
let value = sys::shell_tray::new_menu_item();
```

### `Generation`

A `MenuKind` value.
The row a `DefaultItem` activation runs (at most one). Named so
because `default` is reserved in Rhai.
The generation of the shell's tray: the payload of
`session/<session>/shell/tray`. A restarted shell starts empty and
publishes a new generation once it serves this interface; a client
that sees the generation change calls `Set` again.

| Name | IDL type | Script value |
|---|---|---|
| `generation` | `U64` | int |

```rhai
let value = sys::shell_tray::new_generation();
```

### `Status`

A string; the constants of this module are:

- `STATUS_ACTIVE` = `"Active"`
- `STATUS_PASSIVE` = `"Passive"`
- `STATUS_ATTENTION` = `"Attention"`

`STATUS` is the array of all of them.

### `MenuKind`

A string; the constants of this module are:

- `MENU_KIND_NORMAL` = `"Normal"`
- `MENU_KIND_CHECK` = `"Check"`
- `MENU_KIND_RADIO` = `"Radio"`
- `MENU_KIND_SEPARATOR` = `"Separator"`
- `MENU_KIND_SUBMENU` = `"Submenu"`

`MENU_KIND` is the array of all of them.

### `Activation`

A string; the constants of this module are:

- `ACTIVATION_EVENT` = `"Event"`
- `ACTIVATION_MENU` = `"Menu"`
- `ACTIVATION_DEFAULT_ITEM` = `"DefaultItem"`

`ACTIVATION` is the array of all of them.

## Topics

### `session/{session}/shell/tray`

Published (retained) by the shell of `session` when it starts serving.

Payload `Generation`, QoS `latest`, retained.

Helpers: `SESSION_PATTERN`, `session_topic(session)`, `on_session(session, handler)`, `subscribe_session(session)`, `publish_session(session, payload)`.

```rhai
sys::shell_tray::on_session(session, |event| print(event.payload));
```

## See also

- [Messenger APIs for Rhai](index.md)
- [msg: Messenger from Rhai](../msg.md)
- [os.lazy.shell.tray.v1 on the wire](../../idl/os.lazy.shell.tray.v1.md)
