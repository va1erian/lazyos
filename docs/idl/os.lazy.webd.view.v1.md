# `os.lazy.webd.view.v1`

Interface id: `0x4fe4e35a0bc7bd89`

webd's full views (docs/webd-plan.md sections 3.3 to 3.5 and 4, phase P2
slice 3): what a browser needs of the web engine. A superset of the light
interface (`idl/webd_light.midl`): every method the two share has the same
id and wire shape (`tools/webd/test_idl.py` holds them together), and the
full one adds navigation over the network, a profile (cookies and site
storage), JavaScript dialogs, downloads, notifications, content blocking, the
link under the pointer, the pointer's shape, new-window requests, a policy for
other schemes, and frames that carry only what changed. Served by `webd` as
`app.os.lazy.webd.view`, together with the two scope interfaces below.

**Scopes.** What a view may reach is decided by the interface its
`CreateView` came through, which the kernel checks against the caller's
manifest before webd sees the call (the way `os.lazy.display.cursor.v1` is
a scope of the display):

* `os.lazy.webd.view.v1` (this interface): no network. The scheme policy
must be `DataOnly`; the page is what the app gives it (`LoadHtml`), with
scripts, dialogs and a profile of the app's own if it asks.
* `os.lazy.webd.net.v1`: also `Web` and `Origins` scheme policies: the
page loads `http:` and `https:` content.
* `os.lazy.webd.browser.v1`: also the shared `browser` profile (cookies and
history of the person's web browsing, kept on disk), downloads into the
person's Downloads folder, content blocking, and `Handoff.Browser`
(webd gives `mailto:` and other schemes to the program the person chose).

Every other call goes through this interface, so a package lists it with
any scope it asks for. A `CreateView` the scope does not cover is `EACCES`.

**Identity comes from the kernel, never from the call**, as for light
views: a view belongs to the (user, label) of the task that made it, and a
call on another's view is `ENOENT`. The profile `""` is the app's own: a
data store per app that stays in memory (webd cannot name an app's label
across boots, so nothing of it is kept on disk); `browser` is the one
profile kept on disk.

**Frames.** As for light views, the app attaches up to three `Buffer`
slots of the view's size (slots 0 to 2) and webd paints into a released
one. With `ViewConfig.deltas`, a `Frame` may carry only what changed:
`whole` is false, the app first moves the part of its picture inside
`shift` by `(dx, dy)` (what leaves the area is gone, what it uncovers is
stale) and then copies each of `rects` from the slot, where they sit at
their place in a whole frame's layout (`width * 4` bytes per row). A view
with deltas may also attach band slots (3 and 4, `width * min(height, 1024) * 4` bytes
each): `Band` events carry rows of the page outside the view, painted ahead
of a scroll. A frame that cannot be delivered (no free slot, a full queue)
makes the next one a whole frame.

**Questions.** A navigation the view's policy does not cover with
`Handoff.Ask` is held: `NavigationRequested` asks the app, and no answer in
10 seconds is a no. A JavaScript dialog waits for `DialogReply` (at most 3
dialogs in 10 seconds per view: more are dismissed without asking). A
notification permission request waits for `NotifyPermission`.

Limits (`xui-app/crates/webd/src/limits.rs`) are the light interface's,
plus: at most 16 web processes per user for full views, and a view that
would start one while the user's memory charge cannot hold it is `EDQUOT`
(a view of a new app, profile or `process_key` past that is `EDQUOT`); at
most 64 views for a client with the browser scope (8 otherwise); at most
32 `process_key`s at once per client; at most 64 origins of at most 256
bytes, at most 64 blocking exceptions, a URL at most 8 KiB, at most 256
downloads per profile in a session, a rule list at most 32 MiB. Failures
reply with the standard error field (id 15).

## Methods

| Method | Id | Kind | Signature |
|---|---|---|---|
| CreateView | 1 | sync | `(config: ViewConfig, width: U32, height: U32, events: Channel<os.lazy.webd.view.v1>) -> (view: U64)` |
| DestroyView | 2 | sync | `(view: U64) -> ()` |
| AttachSlot | 3 | sync | `(view: U64, slot: U32, pixels: Buffer) -> ()` |
| ReleaseSlot | 4 | oneway | `(view: U64, slot: U32) -> ()` |
| Resize | 5 | sync | `(view: U64, width: U32, height: U32) -> ()` |
| SetVisible | 6 | oneway | `(view: U64, visible: Bool) -> ()` |
| Refresh | 7 | oneway | `(view: U64) -> ()` |
| Load | 10 | sync | `(view: U64, url: String) -> ()` |
| LoadHtml | 11 | sync | `(view: U64, html: Buffer, base_url: String) -> ()` |
| Reload | 12 | sync | `(view: U64, bypass_blocking: Bool) -> ()` |
| Stop | 13 | sync | `(view: U64) -> ()` |
| Back | 14 | sync | `(view: U64) -> ()` |
| Forward | 15 | sync | `(view: U64) -> ()` |
| Download | 16 | sync | `(view: U64, url: String) -> ()` |
| Recover | 17 | sync | `(view: U64, reload: Bool) -> ()` |
| Mouse | 20 | oneway | `(view: U64, kind: U32, button: I32, x: I32, y: I32, mods: U32) -> ()` |
| Wheel | 21 | oneway | `(view: U64, x: I32, y: I32, ticks_x: I32, ticks_y: I32, mods: U32, seq: U64) -> ()` |
| Key | 22 | oneway | `(view: U64, press: Bool, keysym: U32, text: String, mods: U32) -> ()` |
| Focus | 23 | oneway | `(view: U64, focused: Bool) -> ()` |
| DialogReply | 30 | sync | `(view: U64, dialog: U64, accepted: Bool, value: String) -> ()` |
| NavigationAnswer | 31 | sync | `(view: U64, request: U64, allow: Bool) -> ()` |
| NotifyPermission | 32 | sync | `(view: U64, origin: String, answer: U32) -> ()` |
| DownloadCancel | 33 | sync | `(view: U64, download: U64) -> ()` |
| SetBlocking | 34 | sync | `(view: U64, enabled: Bool, exceptions: Array<String>) -> ()` |
| BlockingCompile | 35 | sync | `(view: U64, list: String, rules: Buffer) -> ()` |
| BlockingLoad | 36 | sync | `(view: U64, list: String) -> ()` |
| BlockingForget | 37 | sync | `(view: U64, list: String) -> ()` |
| NotifyForget | 38 | sync | `(view: U64, origin: String) -> ()` |
| NotifyState | 39 | sync | `(view: U64, notification: U64, state: U32) -> ()` |
| RecoverGroup | 44 | sync | `(view: U64) -> ()` |
| DetachSlots | 45 | sync | `(view: U64) -> (seq: U64)` |
| SetUserAgent | 46 | sync | `(view: U64, user_agent: String) -> ()` |
| Frame | 100 | oneway | `(view: U64, slot: U32, seq: U64, width: U32, height: U32, whole: Bool, shift: Option<Shift>, rects: Array<Rect>, offset_x: I32, offset_y: I32, at_bottom: Bool) -> ()` |
| State | 101 | oneway | `(view: U64, title: String, loading: Bool, finished: U32, error: String, url: String, committed_url: String, can_back: Bool, can_forward: Bool, progress: U32, secure: Bool, commits: U32) -> ()` |
| Scroll | 102 | oneway | `(view: U64, x: I32, y: I32, at_bottom: Bool) -> ()` |
| LinkClicked | 103 | oneway | `(view: U64, url: String) -> ()` |
| ClipboardWrite | 104 | oneway | `(view: U64, text: String) -> ()` |
| ViewLost | 105 | oneway | `(view: U64, reason: U32) -> ()` |
| Ping | 106 | oneway | `() -> ()` |
| Band | 107 | oneway | `(view: U64, slot: U32, doc_x: I32, doc_y: I32, width: U32, height: U32) -> ()` |
| Hover | 108 | oneway | `(view: U64, url: String) -> ()` |
| Cursor | 109 | oneway | `(view: U64, shape: U32) -> ()` |
| NewWindowRequested | 110 | oneway | `(view: U64, url: String, gesture: Bool) -> ()` |
| NavigationRequested | 111 | oneway | `(view: U64, request: U64, url: String, kind: U32, gesture: Bool) -> ()` |
| Dialog | 112 | oneway | `(view: U64, dialog: U64, kind: U32, message: String, default_value: String) -> ()` |
| DownloadStarted | 113 | oneway | `(view: U64, download: U64, url: String, name: String, total: I64) -> ()` |
| DownloadProgress | 114 | oneway | `(view: U64, download: U64, received: I64, total: I64) -> ()` |
| DownloadDone | 115 | oneway | `(view: U64, download: U64, name: String) -> ()` |
| DownloadFailed | 116 | oneway | `(view: U64, download: U64, reason: String) -> ()` |
| NotifyPermissionRequest | 117 | oneway | `(view: U64, origin: String) -> ()` |
| NotifyClicked | 118 | oneway | `(view: U64, notification: U64) -> ()` |
| BlockingList | 119 | oneway | `(view: U64, list: String, status: U32) -> ()` |
| Blocked | 120 | oneway | `(view: U64, count: U32) -> ()` |
| Responsive | 121 | oneway | `(view: U64, responsive: Bool) -> ()` |
| HandedOff | 122 | oneway | `(view: U64, url: String) -> ()` |
| NotifyShow | 123 | oneway | `(view: U64, notification: U64, origin: String, page_url: String, title: String, body: String, tag: String) -> ()` |
| NotifyCancel | 124 | oneway | `(view: U64, notification: U64) -> ()` |
| NotifyGone | 125 | oneway | `(view: U64, notification: U64) -> ()` |
| WheelDone | 126 | oneway | `(view: U64, seq: U64, x: I32, y: I32) -> ()` |

## Objects

Kernel objects a request carries, in the order of the parcel's
object list (the index each field must hold).

| Method | Field | Type | Object |
|---|---|---|---|
| CreateView | `events` | `Channel<os.lazy.webd.view.v1>` | `objects[0]`, a channel the receiver sends `os.lazy.webd.view.v1` on |
| AttachSlot | `pixels` | `Buffer` | `objects[0]`, a shared buffer |
| LoadHtml | `html` | `Buffer` | `objects[0]`, a shared buffer |
| BlockingCompile | `rules` | `Buffer` | `objects[0]`, a shared buffer |

## struct `ViewConfig`

- `profile: String`
- `ephemeral: Bool`
- `scheme_policy: U32`
- `origins: Array<String>`
- `scripts: Bool`
- `handoff: U32`
- `downloads: Bool`
- `popups: Bool`
- `blocking: Bool`
- `notifications: Bool`
- `user_agent_suffix: String`
- `deltas: Bool`
- `dialogs: Bool`
- `notify_in_client: Bool`
- `process_key: String`
- `user_agent: String`

## struct `Rect`

- `x: I32`
- `y: I32`
- `width: I32`
- `height: I32`

## struct `Shift`

- `x: I32`
- `y: I32`
- `width: I32`
- `height: I32`
- `dx: I32`
- `dy: I32`

## enum `SchemePolicy`

- Web, DataOnly, Origins

## enum `Handoff`

- Deny, Ask, Browser

## enum `MouseKind`

- Move, Press, Release

## enum `LostReason`

- Unknown, Refused, Crashed, OutOfMemory, Other

## enum `CursorShape`

- Default, Hand, Text, Wait, Hidden, ResizeH, ResizeV, ResizeNwse, ResizeNesw

## enum `DialogKind`

- Alert, Confirm, Prompt

## enum `NavigationKind`

- LinkClicked, FormSubmitted, BackForward, Reload, FormResubmitted, Other

## enum `BlockingStatus`

- Ready, NotStored, Stale, CompileFailed, Unavailable

## enum `PermissionAnswer`

- Block, Allow, Dismiss

## enum `NotifyStateKind`

- Shown, Clicked, Closed
