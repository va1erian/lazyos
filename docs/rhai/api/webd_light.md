# `sys::webd_light`

The Rhai module for the Messenger interface `os.lazy.webd.light.v1`. Wire-level reference: [os.lazy.webd.light.v1](../../idl/os.lazy.webd.light.v1.md).

webd's light views (docs/webd-plan.md sections 3.6 and 4, phase P2): an app
that only needs WebKit's HTML/CSS/layout/font engine asks `webd` for a
view, hands it the page's HTML, and gets the page's pixels back in shared
memory slots plus a few page events. A light view has **no network** (no
scheme but `data:`, `blob:` and `about:blank` ever loads, and the engine
refuses the rest too), no cookies, cache or storage, no hand-off to other
programs, and a **locked navigation**: a link click, a form submit or a
`<meta http-equiv=refresh>` becomes `LinkClicked` and nothing loads. Scripts
are off unless `LightConfig.scripts` asks, and even then have no network
and no windows. Served by `webd` as `app.os.lazy.webd.light`; a package
that lists only this interface cannot resolve the full service
(`os.lazy.webd.view.v1`, a later interface).

Like `os.lazy.display.v1`, one interface carries both directions. **Calls**
(app to webd) are ordinary request/reply methods, input and slot returns
are `oneway`. **Events** (webd to app) are `oneway` methods sent on the
endpoint the app moved in with `CreateView` (`events`): `Frame`, `State`,
`Scroll`, `LinkClicked`, `ClipboardWrite`, `ViewLost` and `Ping`. Method
ids are append-only from the first release; the full interface reuses
these ids and wire shapes (its `CreateView` also carries a `scale` as
field 4, which this one does not have yet), so client code is shared.

**Identity comes from the kernel, never from the call.** A view belongs
to the (user, label) of the task that created it and is unusable by any
other caller: every call on someone else's view fails like a call on a view
that does not exist (`ENOENT`). Nothing in a request names the owner. A
caller in another login session than webd's gets `EACCES`.

**Pixels.** The app owns the memory: it attaches up to three `Buffer` slots
of the view's size (`AttachSlot`, RGBA8, `width * 4` bytes per row, at
least `width * height * 4` bytes). webd copies a whole frame into a slot the
app has released and sends `Frame(view, slot, seq, width, height)`; the slot
is the app's until it calls `ReleaseSlot`. When every slot is busy a frame
is dropped and webd asks the engine for a fresh whole frame as soon as one
is released, so the app always ends on the latest picture and webd never
queues frames or blocks on a slow app. `Resize` detaches every slot (their
size is no longer right): attach new ones before the next frame can come.

Limits (`xui-app/crates/webd/src/limits.rs`): at most 8 live views per
client (`ENOSPC`); width and height 1..=8192 and at most 16 Mpixel
(`EINVAL`); at most 3 slots per view, and at most 512 MiB of slot memory per
client (`EDQUOT`); `html` at most 8 MiB (`E2BIG`); `user_style` at most 64
KiB (`EINVAL`). A call on a view the engine lost (`ViewLost`) fails with
`ENODEV` until the app calls `DestroyView`. Failures reply with the
standard error field (id 15, `docs/midl.md`).

## Functions

### `destroy_view(view)`

`DestroyView(view: U64) -> ()`

End the view, its page and its event endpoint. Slots the app still
holds stay its own memory. Not an error for a view that is already gone.

Arguments:

| Name | IDL type | Script value |
|---|---|---|
| `view` | `U64` | int |

Returns `()`.

```rhai
let result = sys::webd_light::destroy_view(view);
```

### `release_slot(view, slot)`

`ReleaseSlot(view: U64, slot: U32) -> () oneway`

The app is done reading `slot`; webd may paint into it again. A slot
that is not busy, or a view that is not the caller's, is ignored.

Arguments:

| Name | IDL type | Script value |
|---|---|---|
| `view` | `U64` | int |
| `slot` | `U32` | int |

One-way: the call returns `()` once the message is queued.

```rhai
let result = sys::webd_light::release_slot(view, slot);
```

### `resize(view, width, height)`

`Resize(view: U64, width: U32, height: U32) -> ()`

Change the view's size. Every slot is detached (attach new ones of the
new size); the next frame has the new size.

Arguments:

| Name | IDL type | Script value |
|---|---|---|
| `view` | `U64` | int |
| `width` | `U32` | int |
| `height` | `U32` | int |

Returns `()`.

```rhai
let result = sys::webd_light::resize(view, width, height);
```

### `set_visible(view, visible)`

`SetVisible(view: U64, visible: Bool) -> () oneway`

A view that is not visible gets no paints (its page still runs).

Arguments:

| Name | IDL type | Script value |
|---|---|---|
| `view` | `U64` | int |
| `visible` | `Bool` | bool |

One-way: the call returns `()` once the message is queued.

```rhai
let result = sys::webd_light::set_visible(view, visible);
```

### `refresh(view)`

`Refresh(view: U64) -> () oneway`

Ask for a whole frame (the app lost or discarded its picture).

Arguments:

| Name | IDL type | Script value |
|---|---|---|
| `view` | `U64` | int |

One-way: the call returns `()` once the message is queued.

```rhai
let result = sys::webd_light::refresh(view);
```

### `mouse(view, kind, button, x, y, mods)`

`Mouse(view: U64, kind: U32, button: I32, x: I32, y: I32, mods: U32) -> () oneway`

Input, as the display protocol delivers it, in view pixels. `kind` is a
`MouseKind`, `button` 0 left, 1 middle, 2 right (-1 none: a move with no
button held); `mods` bit 0 shift, 1 control, 2 alt, 3 meta.

Arguments:

| Name | IDL type | Script value |
|---|---|---|
| `view` | `U64` | int |
| `kind` | `U32` | int |
| `button` | `I32` | int |
| `x` | `I32` | int |
| `y` | `I32` | int |
| `mods` | `U32` | int |

One-way: the call returns `()` once the message is queued.

```rhai
let result = sys::webd_light::mouse(view, kind, button, x, y, mods);
```

### `wheel(view, x, y, ticks_x, ticks_y, mods)`

`Wheel(view: U64, x: I32, y: I32, ticks_x: I32, ticks_y: I32, mods: U32) -> () oneway`

A wheel turn of `ticks_x` / `ticks_y` notches at `(x, y)`.

Arguments:

| Name | IDL type | Script value |
|---|---|---|
| `view` | `U64` | int |
| `x` | `I32` | int |
| `y` | `I32` | int |
| `ticks_x` | `I32` | int |
| `ticks_y` | `I32` | int |
| `mods` | `U32` | int |

One-way: the call returns `()` once the message is queued.

```rhai
let result = sys::webd_light::wheel(view, x, y, ticks_x, ticks_y, mods);
```

### `key(view, press, keysym, text, mods)`

`Key(view: U64, press: Bool, keysym: U32, text: String, mods: U32) -> () oneway`

A key edge: `keysym` is the X11 keysym the display protocol carries,
`text` the character it types (empty for none).

Arguments:

| Name | IDL type | Script value |
|---|---|---|
| `view` | `U64` | int |
| `press` | `Bool` | bool |
| `keysym` | `U32` | int |
| `text` | `String` | string |
| `mods` | `U32` | int |

One-way: the call returns `()` once the message is queued.

```rhai
let result = sys::webd_light::key(view, press, keysym, text, mods);
```

### `focus(view, focused)`

`Focus(view: U64, focused: Bool) -> () oneway`

The view gained or lost keyboard focus.

Arguments:

| Name | IDL type | Script value |
|---|---|---|
| `view` | `U64` | int |
| `focused` | `Bool` | bool |

One-way: the call returns `()` once the message is queued.

```rhai
let result = sys::webd_light::focus(view, focused);
```

### `frame(view, slot, seq, width, height)`

`Frame(view: U64, slot: U32, seq: U64, width: U32, height: U32) -> () oneway`

Event: slot `slot` of `view` holds frame number `seq` (counting from 1)
of `width` x `height` pixels. The slot is the app's until `ReleaseSlot`.

Arguments:

| Name | IDL type | Script value |
|---|---|---|
| `view` | `U64` | int |
| `slot` | `U32` | int |
| `seq` | `U64` | int |
| `width` | `U32` | int |
| `height` | `U32` | int |

One-way: the call returns `()` once the message is queued.

```rhai
let result = sys::webd_light::frame(view, slot, seq, width, height);
```

### `state(view, title, loading, finished, error)`

`State(view: U64, title: String, loading: Bool, finished: U32, error: String) -> () oneway`

Event: what the page says about itself. `loading` while a load is in
progress; `finished` counts the loads that have finished (so the first
paint of a `LoadHtml` can be told from the old page's); `error` is
empty or a short reason a load failed. Texts are cut at 48 KiB.

Arguments:

| Name | IDL type | Script value |
|---|---|---|
| `view` | `U64` | int |
| `title` | `String` | string |
| `loading` | `Bool` | bool |
| `finished` | `U32` | int |
| `error` | `String` | string |

One-way: the call returns `()` once the message is queued.

```rhai
let result = sys::webd_light::state(view, title, loading, finished, error);
```

### `scroll(view, x, y, at_bottom)`

`Scroll(view: U64, x: I32, y: I32, at_bottom: Bool) -> () oneway`

Event: the page scrolled to `(x, y)` in document pixels; `at_bottom`
when it cannot scroll further.

Arguments:

| Name | IDL type | Script value |
|---|---|---|
| `view` | `U64` | int |
| `x` | `I32` | int |
| `y` | `I32` | int |
| `at_bottom` | `Bool` | bool |

One-way: the call returns `()` once the message is queued.

```rhai
let result = sys::webd_light::scroll(view, x, y, at_bottom);
```

### `link_clicked(view, url)`

`LinkClicked(view: U64, url: String) -> () oneway`

Event: a link was clicked, a form submitted or a meta refresh fired,
and nothing loaded. `url` is absolute.

Arguments:

| Name | IDL type | Script value |
|---|---|---|
| `view` | `U64` | int |
| `url` | `String` | string |

One-way: the call returns `()` once the message is queued.

```rhai
let result = sys::webd_light::link_clicked(view, url);
```

### `clipboard_write(view, text)`

`ClipboardWrite(view: U64, text: String) -> () oneway`

Event: the user copied `text` out of the page.

Arguments:

| Name | IDL type | Script value |
|---|---|---|
| `view` | `U64` | int |
| `text` | `String` | string |

One-way: the call returns `()` once the message is queued.

```rhai
let result = sys::webd_light::clipboard_write(view, text);
```

### `view_lost(view, reason)`

`ViewLost(view: U64, reason: U32) -> () oneway`

Event: the view cannot go on (`reason` is a `LostReason`). The id stays
the app's to `DestroyView`; the app makes a new view and loads again.

Arguments:

| Name | IDL type | Script value |
|---|---|---|
| `view` | `U64` | int |
| `reason` | `U32` | int |

One-way: the call returns `()` once the message is queued.

```rhai
let result = sys::webd_light::view_lost(view, reason);
```

### `ping()`

`Ping() -> () oneway`

Event: liveness probe, about once a second; nothing to answer. `EPIPE`
on the endpoint tells webd the app is gone and ends its views.

One-way: the call returns `()` once the message is queued.

```rhai
let result = sys::webd_light::ping();
```

Not callable from a script (the request carries a kernel object): `CreateView`, `AttachSlot`, `LoadHtml`.

## Types

### `LightConfig`

How a light view is configured: these four fields and nothing else. No
origins, profile, scheme policy or storage exists to ask for.

| Name | IDL type | Script value |
|---|---|---|
| `scripts` | `Bool` | bool |
| `color_scheme` | `U32` | int |
| `default_font_size` | `U32` | int |
| `user_style` | `String` | string |

```rhai
let value = sys::webd_light::new_light_config();
```

### `ColorScheme`

A string; the constants of this module are:

- `COLOR_SCHEME_AUTO` = `"Auto"`
- `COLOR_SCHEME_DARK` = `"Dark"`

`COLOR_SCHEME` is the array of all of them.

### `MouseKind`

A string; the constants of this module are:

- `MOUSE_KIND_MOVE` = `"Move"`
- `MOUSE_KIND_PRESS` = `"Press"`
- `MOUSE_KIND_RELEASE` = `"Release"`

`MOUSE_KIND` is the array of all of them.

### `LostReason`

A string; the constants of this module are:

- `LOST_REASON_UNKNOWN` = `"Unknown"`
- `LOST_REASON_REFUSED` = `"Refused"`
- `LOST_REASON_CRASHED` = `"Crashed"`
- `LOST_REASON_OUT_OF_MEMORY` = `"OutOfMemory"`
- `LOST_REASON_OTHER` = `"Other"`

`LOST_REASON` is the array of all of them.

## See also

- [Messenger APIs for Rhai](index.md)
- [msg: Messenger from Rhai](../msg.md)
- [os.lazy.webd.light.v1 on the wire](../../idl/os.lazy.webd.light.v1.md)
