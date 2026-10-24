# `os.lazy.webd.light.v1`

Interface id: `0xc685c12f0603531c`

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

## Methods

| Method | Id | Kind | Signature |
|---|---|---|---|
| CreateView | 1 | sync | `(config: LightConfig, width: U32, height: U32, events: Channel<os.lazy.webd.light.v1>) -> (view: U64)` |
| DestroyView | 2 | sync | `(view: U64) -> ()` |
| AttachSlot | 3 | sync | `(view: U64, slot: U32, pixels: Buffer) -> ()` |
| ReleaseSlot | 4 | oneway | `(view: U64, slot: U32) -> ()` |
| Resize | 5 | sync | `(view: U64, width: U32, height: U32) -> ()` |
| SetVisible | 6 | oneway | `(view: U64, visible: Bool) -> ()` |
| Refresh | 7 | oneway | `(view: U64) -> ()` |
| LoadHtml | 11 | sync | `(view: U64, html: Buffer, base_url: String) -> ()` |
| Mouse | 20 | oneway | `(view: U64, kind: U32, button: I32, x: I32, y: I32, mods: U32) -> ()` |
| Wheel | 21 | oneway | `(view: U64, x: I32, y: I32, ticks_x: I32, ticks_y: I32, mods: U32, seq: U64) -> ()` |
| Key | 22 | oneway | `(view: U64, press: Bool, keysym: U32, text: String, mods: U32) -> ()` |
| Focus | 23 | oneway | `(view: U64, focused: Bool) -> ()` |
| Frame | 100 | oneway | `(view: U64, slot: U32, seq: U64, width: U32, height: U32) -> ()` |
| State | 101 | oneway | `(view: U64, title: String, loading: Bool, finished: U32, error: String) -> ()` |
| Scroll | 102 | oneway | `(view: U64, x: I32, y: I32, at_bottom: Bool) -> ()` |
| LinkClicked | 103 | oneway | `(view: U64, url: String) -> ()` |
| ClipboardWrite | 104 | oneway | `(view: U64, text: String) -> ()` |
| ViewLost | 105 | oneway | `(view: U64, reason: U32) -> ()` |
| Ping | 106 | oneway | `() -> ()` |

## Objects

Kernel objects a request carries, in the order of the parcel's
object list (the index each field must hold).

| Method | Field | Type | Object |
|---|---|---|---|
| CreateView | `events` | `Channel<os.lazy.webd.light.v1>` | `objects[0]`, a channel the receiver sends `os.lazy.webd.light.v1` on |
| AttachSlot | `pixels` | `Buffer` | `objects[0]`, a shared buffer |
| LoadHtml | `html` | `Buffer` | `objects[0]`, a shared buffer |

## struct `LightConfig`

- `scripts: Bool`
- `color_scheme: U32`
- `default_font_size: U32`
- `user_style: String`

## enum `ColorScheme`

- Auto, Dark

## enum `MouseKind`

- Move, Press, Release

## enum `LostReason`

- Unknown, Refused, Crashed, OutOfMemory, Other
