# `sys::webd_view`

The Rhai module for the Messenger interface `os.lazy.webd.view.v1`. Wire-level reference: [os.lazy.webd.view.v1](../../idl/os.lazy.webd.view.v1.md).

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
plus: at most 4 web processes per user for full views (a view of a new
app or profile past that is `EDQUOT`), at most 64 origins of at most 256
bytes, at most 64 blocking exceptions, a URL at most 8 KiB, at most 256
downloads per profile in a session, a rule list at most 32 MiB. Failures
reply with the standard error field (id 15).

## Functions

### `destroy_view(view)`

`DestroyView(view: U64) -> ()`

End the view; not an error for a view that is gone.

Arguments:

| Name | IDL type | Script value |
|---|---|---|
| `view` | `U64` | int |

Returns `()`.

```rhai
let result = sys::webd_view::destroy_view(view);
```

### `release_slot(view, slot)`

`ReleaseSlot(view: U64, slot: U32) -> () oneway`

The app is done reading `slot`.

Arguments:

| Name | IDL type | Script value |
|---|---|---|
| `view` | `U64` | int |
| `slot` | `U32` | int |

One-way: the call returns `()` once the message is queued.

```rhai
let result = sys::webd_view::release_slot(view, slot);
```

### `resize(view, width, height)`

`Resize(view: U64, width: U32, height: U32) -> ()`

Change the view's size; every slot is detached.

Arguments:

| Name | IDL type | Script value |
|---|---|---|
| `view` | `U64` | int |
| `width` | `U32` | int |
| `height` | `U32` | int |

Returns `()`.

```rhai
let result = sys::webd_view::resize(view, width, height);
```

### `set_visible(view, visible)`

`SetVisible(view: U64, visible: Bool) -> () oneway`

A view that is not visible gets no paints.

Arguments:

| Name | IDL type | Script value |
|---|---|---|
| `view` | `U64` | int |
| `visible` | `Bool` | bool |

One-way: the call returns `()` once the message is queued.

```rhai
let result = sys::webd_view::set_visible(view, visible);
```

### `refresh(view)`

`Refresh(view: U64) -> () oneway`

Ask for a whole frame.

Arguments:

| Name | IDL type | Script value |
|---|---|---|
| `view` | `U64` | int |

One-way: the call returns `()` once the message is queued.

```rhai
let result = sys::webd_view::refresh(view);
```

### `load(view, url)`

`Load(view: U64, url: String) -> ()`

Load `url` in the view: it must pass the view's scheme policy (`EACCES`
otherwise; `file:` never does), at most 8 KiB.

Arguments:

| Name | IDL type | Script value |
|---|---|---|
| `view` | `U64` | int |
| `url` | `String` | string |

Returns `()`.

```rhai
let result = sys::webd_view::load(view, url);
```

### `reload(view, bypass_blocking)`

`Reload(view: U64, bypass_blocking: Bool) -> ()`

Load the page again; `bypass_blocking` loads it once without the
content blocker.

Arguments:

| Name | IDL type | Script value |
|---|---|---|
| `view` | `U64` | int |
| `bypass_blocking` | `Bool` | bool |

Returns `()`.

```rhai
let result = sys::webd_view::reload(view, bypass_blocking);
```

### `stop(view)`

`Stop(view: U64) -> ()`

Arguments:

| Name | IDL type | Script value |
|---|---|---|
| `view` | `U64` | int |

Returns `()`.

```rhai
let result = sys::webd_view::stop(view);
```

### `back(view)`

`Back(view: U64) -> ()`

Arguments:

| Name | IDL type | Script value |
|---|---|---|
| `view` | `U64` | int |

Returns `()`.

```rhai
let result = sys::webd_view::back(view);
```

### `forward(view)`

`Forward(view: U64) -> ()`

Arguments:

| Name | IDL type | Script value |
|---|---|---|
| `view` | `U64` | int |

Returns `()`.

```rhai
let result = sys::webd_view::forward(view);
```

### `download(view, url)`

`Download(view: U64, url: String) -> ()`

Save `url` as a download (Save Page As): a view with `downloads` only.

Arguments:

| Name | IDL type | Script value |
|---|---|---|
| `view` | `U64` | int |
| `url` | `String` | string |

Returns `()`.

```rhai
let result = sys::webd_view::download(view, url);
```

### `mouse(view, kind, button, x, y, mods)`

`Mouse(view: U64, kind: U32, button: I32, x: I32, y: I32, mods: U32) -> () oneway`

Input, as on the light interface.

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
let result = sys::webd_view::mouse(view, kind, button, x, y, mods);
```

### `wheel(view, x, y, ticks_x, ticks_y, mods)`

`Wheel(view: U64, x: I32, y: I32, ticks_x: I32, ticks_y: I32, mods: U32) -> () oneway`

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
let result = sys::webd_view::wheel(view, x, y, ticks_x, ticks_y, mods);
```

### `key(view, press, keysym, text, mods)`

`Key(view: U64, press: Bool, keysym: U32, text: String, mods: U32) -> () oneway`

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
let result = sys::webd_view::key(view, press, keysym, text, mods);
```

### `focus(view, focused)`

`Focus(view: U64, focused: Bool) -> () oneway`

Arguments:

| Name | IDL type | Script value |
|---|---|---|
| `view` | `U64` | int |
| `focused` | `Bool` | bool |

One-way: the call returns `()` once the message is queued.

```rhai
let result = sys::webd_view::focus(view, focused);
```

### `dialog_reply(view, dialog, accepted, value)`

`DialogReply(view: U64, dialog: U64, accepted: Bool, value: String) -> ()`

The answer to `Dialog` `dialog`: `accepted` is OK, `value` a prompt's text.

Arguments:

| Name | IDL type | Script value |
|---|---|---|
| `view` | `U64` | int |
| `dialog` | `U64` | int |
| `accepted` | `Bool` | bool |
| `value` | `String` | string |

Returns `()`.

```rhai
let result = sys::webd_view::dialog_reply(view, dialog, accepted, value);
```

### `navigation_answer(view, request, allow)`

`NavigationAnswer(view: U64, request: U64, allow: Bool) -> ()`

The answer to `NavigationRequested` `request`: `allow` loads it.

Arguments:

| Name | IDL type | Script value |
|---|---|---|
| `view` | `U64` | int |
| `request` | `U64` | int |
| `allow` | `Bool` | bool |

Returns `()`.

```rhai
let result = sys::webd_view::navigation_answer(view, request, allow);
```

### `notify_permission(view, origin, answer)`

`NotifyPermission(view: U64, origin: String, answer: U32) -> ()`

The answer to `NotifyPermissionRequest` for `origin` (a `PermissionAnswer`):
kept for the view's profile while webd runs.

Arguments:

| Name | IDL type | Script value |
|---|---|---|
| `view` | `U64` | int |
| `origin` | `String` | string |
| `answer` | `U32` | int |

Returns `()`.

```rhai
let result = sys::webd_view::notify_permission(view, origin, answer);
```

### `download_cancel(view, download)`

`DownloadCancel(view: U64, download: U64) -> ()`

Stop download `download`; its `.part` file is removed.

Arguments:

| Name | IDL type | Script value |
|---|---|---|
| `view` | `U64` | int |
| `download` | `U64` | int |

Returns `()`.

```rhai
let result = sys::webd_view::download_cancel(view, download);
```

### `set_blocking(view, enabled, exceptions)`

`SetBlocking(view: U64, enabled: Bool, exceptions: Array<String>) -> ()`

Content blocking on or off for the view's profile, and the hosts it is
off for. A view made with `blocking` only.

Arguments:

| Name | IDL type | Script value |
|---|---|---|
| `view` | `U64` | int |
| `enabled` | `Bool` | bool |
| `exceptions` | `Array<String>` | array of string |

Returns `()`.

```rhai
let result = sys::webd_view::set_blocking(view, enabled, exceptions);
```

### `blocking_load(view, list)`

`BlockingLoad(view: U64, list: String) -> ()`

Attach list `list` from the profile's store (compiled earlier).

Arguments:

| Name | IDL type | Script value |
|---|---|---|
| `view` | `U64` | int |
| `list` | `String` | string |

Returns `()`.

```rhai
let result = sys::webd_view::blocking_load(view, list);
```

### `blocking_forget(view, list)`

`BlockingForget(view: U64, list: String) -> ()`

Detach list `list` and remove it from the store.

Arguments:

| Name | IDL type | Script value |
|---|---|---|
| `view` | `U64` | int |
| `list` | `String` | string |

Returns `()`.

```rhai
let result = sys::webd_view::blocking_forget(view, list);
```

### `frame(view, slot, seq, width, height, whole, shift, rects, offset_x, offset_y, at_bottom)`

`Frame(view: U64, slot: U32, seq: U64, width: U32, height: U32, whole: Bool, shift: Option<Shift>, rects: Array<Rect>, offset_x: I32, offset_y: I32, at_bottom: Bool) -> () oneway`

Event: slot `slot` holds frame `seq`. `whole`: every pixel is new.
Otherwise the app moves its picture by `shift` (when present) and then
copies `rects` from the slot. `offset_x`/`offset_y` is the page's scroll
offset these pixels were painted at; `at_bottom` when the page cannot
scroll further down from there.

Arguments:

| Name | IDL type | Script value |
|---|---|---|
| `view` | `U64` | int |
| `slot` | `U32` | int |
| `seq` | `U64` | int |
| `width` | `U32` | int |
| `height` | `U32` | int |
| `whole` | `Bool` | bool |
| `shift` | `Option<Shift>` | map (`Shift`) or () |
| `rects` | `Array<Rect>` | array of map (`Rect`) |
| `offset_x` | `I32` | int |
| `offset_y` | `I32` | int |
| `at_bottom` | `Bool` | bool |

One-way: the call returns `()` once the message is queued.

```rhai
let result = sys::webd_view::frame(view, slot, seq, width, height, whole, shift, rects, offset_x, offset_y, at_bottom);
```

### `state(view, title, loading, finished, error, url, committed_url, can_back, can_forward, progress, secure)`

`State(view: U64, title: String, loading: Bool, finished: U32, error: String, url: String, committed_url: String, can_back: Bool, can_forward: Bool, progress: U32, secure: Bool) -> () oneway`

Event: what the page says about itself. `url` is the address on show,
`committed_url` the page on screen; `progress` in thousandths;
`secure` when the committed page came over verified HTTPS. Texts are
cut at 48 KiB.

Arguments:

| Name | IDL type | Script value |
|---|---|---|
| `view` | `U64` | int |
| `title` | `String` | string |
| `loading` | `Bool` | bool |
| `finished` | `U32` | int |
| `error` | `String` | string |
| `url` | `String` | string |
| `committed_url` | `String` | string |
| `can_back` | `Bool` | bool |
| `can_forward` | `Bool` | bool |
| `progress` | `U32` | int |
| `secure` | `Bool` | bool |

One-way: the call returns `()` once the message is queued.

```rhai
let result = sys::webd_view::state(view, title, loading, finished, error, url, committed_url, can_back, can_forward, progress, secure);
```

### `scroll(view, x, y, at_bottom)`

`Scroll(view: U64, x: I32, y: I32, at_bottom: Bool) -> () oneway`

Event: the page scrolled (as on the light interface).

Arguments:

| Name | IDL type | Script value |
|---|---|---|
| `view` | `U64` | int |
| `x` | `I32` | int |
| `y` | `I32` | int |
| `at_bottom` | `Bool` | bool |

One-way: the call returns `()` once the message is queued.

```rhai
let result = sys::webd_view::scroll(view, x, y, at_bottom);
```

### `link_clicked(view, url)`

`LinkClicked(view: U64, url: String) -> () oneway`

Event: as on the light interface (a full view's navigation is not
locked, so webd does not send it today).

Arguments:

| Name | IDL type | Script value |
|---|---|---|
| `view` | `U64` | int |
| `url` | `String` | string |

One-way: the call returns `()` once the message is queued.

```rhai
let result = sys::webd_view::link_clicked(view, url);
```

### `clipboard_write(view, text)`

`ClipboardWrite(view: U64, text: String) -> () oneway`

Arguments:

| Name | IDL type | Script value |
|---|---|---|
| `view` | `U64` | int |
| `text` | `String` | string |

One-way: the call returns `()` once the message is queued.

```rhai
let result = sys::webd_view::clipboard_write(view, text);
```

### `view_lost(view, reason)`

`ViewLost(view: U64, reason: U32) -> () oneway`

Arguments:

| Name | IDL type | Script value |
|---|---|---|
| `view` | `U64` | int |
| `reason` | `U32` | int |

One-way: the call returns `()` once the message is queued.

```rhai
let result = sys::webd_view::view_lost(view, reason);
```

### `ping()`

`Ping() -> () oneway`

One-way: the call returns `()` once the message is queued.

```rhai
let result = sys::webd_view::ping();
```

### `band(view, slot, doc_x, doc_y, width, height)`

`Band(view: U64, slot: U32, doc_x: I32, doc_y: I32, width: U32, height: U32) -> () oneway`

Event: band slot `slot` holds `height` rows of the page from document
row `doc_y`, painted at horizontal offset `doc_x`, `width` wide.

Arguments:

| Name | IDL type | Script value |
|---|---|---|
| `view` | `U64` | int |
| `slot` | `U32` | int |
| `doc_x` | `I32` | int |
| `doc_y` | `I32` | int |
| `width` | `U32` | int |
| `height` | `U32` | int |

One-way: the call returns `()` once the message is queued.

```rhai
let result = sys::webd_view::band(view, slot, doc_x, doc_y, width, height);
```

### `hover(view, url)`

`Hover(view: U64, url: String) -> () oneway`

Event: the pointer is over a link (its address) or no longer ("").

Arguments:

| Name | IDL type | Script value |
|---|---|---|
| `view` | `U64` | int |
| `url` | `String` | string |

One-way: the call returns `()` once the message is queued.

```rhai
let result = sys::webd_view::hover(view, url);
```

### `cursor(view, shape)`

`Cursor(view: U64, shape: U32) -> () oneway`

Event: the page wants this pointer shape (a `CursorShape`).

Arguments:

| Name | IDL type | Script value |
|---|---|---|
| `view` | `U64` | int |
| `shape` | `U32` | int |

One-way: the call returns `()` once the message is queued.

```rhai
let result = sys::webd_view::cursor(view, shape);
```

### `new_window_requested(view, url, gesture)`

`NewWindowRequested(view: U64, url: String, gesture: Bool) -> () oneway`

Event: the page asked for a new window (`target=_blank`,
`window.open`); nothing opened. `gesture`: a click or key started it.

Arguments:

| Name | IDL type | Script value |
|---|---|---|
| `view` | `U64` | int |
| `url` | `String` | string |
| `gesture` | `Bool` | bool |

One-way: the call returns `()` once the message is queued.

```rhai
let result = sys::webd_view::new_window_requested(view, url, gesture);
```

### `navigation_requested(view, request, url, kind, gesture)`

`NavigationRequested(view: U64, request: U64, url: String, kind: U32, gesture: Bool) -> () oneway`

Event: a navigation outside the view's policy is held (`Handoff.Ask`);
answer with `NavigationAnswer` within 10 seconds or it is refused.
`kind` is a `NavigationKind`.

Arguments:

| Name | IDL type | Script value |
|---|---|---|
| `view` | `U64` | int |
| `request` | `U64` | int |
| `url` | `String` | string |
| `kind` | `U32` | int |
| `gesture` | `Bool` | bool |

One-way: the call returns `()` once the message is queued.

```rhai
let result = sys::webd_view::navigation_requested(view, request, url, kind, gesture);
```

### `dialog(view, dialog, kind, message, default_value)`

`Dialog(view: U64, dialog: U64, kind: U32, message: String, default_value: String) -> () oneway`

Event: the page opened a JavaScript dialog (a `DialogKind`) and waits
for `DialogReply`.

Arguments:

| Name | IDL type | Script value |
|---|---|---|
| `view` | `U64` | int |
| `dialog` | `U64` | int |
| `kind` | `U32` | int |
| `message` | `String` | string |
| `default_value` | `String` | string |

One-way: the call returns `()` once the message is queued.

```rhai
let result = sys::webd_view::dialog(view, dialog, kind, message, default_value);
```

### `download_started(view, download, url, name, total)`

`DownloadStarted(view: U64, download: U64, url: String, name: String, total: I64) -> () oneway`

Event: a download began, into the person's Downloads folder under a
`.part` name. `total` is the announced size or -1.

Arguments:

| Name | IDL type | Script value |
|---|---|---|
| `view` | `U64` | int |
| `download` | `U64` | int |
| `url` | `String` | string |
| `name` | `String` | string |
| `total` | `I64` | int |

One-way: the call returns `()` once the message is queued.

```rhai
let result = sys::webd_view::download_started(view, download, url, name, total);
```

### `download_progress(view, download, received, total)`

`DownloadProgress(view: U64, download: U64, received: I64, total: I64) -> () oneway`

Arguments:

| Name | IDL type | Script value |
|---|---|---|
| `view` | `U64` | int |
| `download` | `U64` | int |
| `received` | `I64` | int |
| `total` | `I64` | int |

One-way: the call returns `()` once the message is queued.

```rhai
let result = sys::webd_view::download_progress(view, download, received, total);
```

### `download_done(view, download, name)`

`DownloadDone(view: U64, download: U64, name: String) -> () oneway`

Event: every byte is in `name` (a file of the Downloads folder).

Arguments:

| Name | IDL type | Script value |
|---|---|---|
| `view` | `U64` | int |
| `download` | `U64` | int |
| `name` | `String` | string |

One-way: the call returns `()` once the message is queued.

```rhai
let result = sys::webd_view::download_done(view, download, name);
```

### `download_failed(view, download, reason)`

`DownloadFailed(view: U64, download: U64, reason: String) -> () oneway`

Arguments:

| Name | IDL type | Script value |
|---|---|---|
| `view` | `U64` | int |
| `download` | `U64` | int |
| `reason` | `String` | string |

One-way: the call returns `()` once the message is queued.

```rhai
let result = sys::webd_view::download_failed(view, download, reason);
```

### `notify_permission_request(view, origin)`

`NotifyPermissionRequest(view: U64, origin: String) -> () oneway`

Event: a page asked to show notifications for `origin`.

Arguments:

| Name | IDL type | Script value |
|---|---|---|
| `view` | `U64` | int |
| `origin` | `String` | string |

One-way: the call returns `()` once the message is queued.

```rhai
let result = sys::webd_view::notify_permission_request(view, origin);
```

### `notify_clicked(view, notification)`

`NotifyClicked(view: U64, notification: U64) -> () oneway`

Event: the person clicked notification `notification` (webd posted it
as a toast titled with the site); the page's click handler ran.

Arguments:

| Name | IDL type | Script value |
|---|---|---|
| `view` | `U64` | int |
| `notification` | `U64` | int |

One-way: the call returns `()` once the message is queued.

```rhai
let result = sys::webd_view::notify_clicked(view, notification);
```

### `blocking_list(view, list, status)`

`BlockingList(view: U64, list: String, status: U32) -> () oneway`

Event: what became of rule list `list` (a `BlockingStatus`).

Arguments:

| Name | IDL type | Script value |
|---|---|---|
| `view` | `U64` | int |
| `list` | `String` | string |
| `status` | `U32` | int |

One-way: the call returns `()` once the message is queued.

```rhai
let result = sys::webd_view::blocking_list(view, list, status);
```

### `blocked(view, count)`

`Blocked(view: U64, count: U32) -> () oneway`

Event: the number of loads the lists blocked on the page on show.

Arguments:

| Name | IDL type | Script value |
|---|---|---|
| `view` | `U64` | int |
| `count` | `U32` | int |

One-way: the call returns `()` once the message is queued.

```rhai
let result = sys::webd_view::blocked(view, count);
```

### `responsive(view, responsive)`

`Responsive(view: U64, responsive: Bool) -> () oneway`

Event: the page's process stopped answering, or answers again.

Arguments:

| Name | IDL type | Script value |
|---|---|---|
| `view` | `U64` | int |
| `responsive` | `Bool` | bool |

One-way: the call returns `()` once the message is queued.

```rhai
let result = sys::webd_view::responsive(view, responsive);
```

### `handed_off(view, url)`

`HandedOff(view: U64, url: String) -> () oneway`

Event: webd gave `url` to the program the person chose for it
(`Handoff.Browser`).

Arguments:

| Name | IDL type | Script value |
|---|---|---|
| `view` | `U64` | int |
| `url` | `String` | string |

One-way: the call returns `()` once the message is queued.

```rhai
let result = sys::webd_view::handed_off(view, url);
```

Not callable from a script (the request carries a kernel object): `CreateView`, `AttachSlot`, `LoadHtml`, `BlockingCompile`.

## Types

### `ViewConfig`

How a full view is made; fixed for its life.

| Name | IDL type | Script value |
|---|---|---|
| `profile` | `String` | string |
| `ephemeral` | `Bool` | bool |
| `scheme_policy` | `U32` | int |
| `origins` | `Array<String>` | array of string |
| `scripts` | `Bool` | bool |
| `handoff` | `U32` | int |
| `downloads` | `Bool` | bool |
| `popups` | `Bool` | bool |
| `blocking` | `Bool` | bool |
| `notifications` | `Bool` | bool |
| `user_agent_suffix` | `String` | string |
| `deltas` | `Bool` | bool |
| `dialogs` | `Bool` | bool |

```rhai
let value = sys::webd_view::new_view_config();
```

### `Rect`

`""`: the app's own profile (in memory); `browser`: the shared
browsing profile (`os.lazy.webd.browser.v1`).
The profile's data stays in memory (a private window).
A `SchemePolicy`.
With `Origins`: the `scheme://host[:port]` the page may load.
JavaScript runs in the page.
A `Handoff`: what happens to a navigation the policy does not cover.
Responses the page cannot show become downloads.
`NewWindowRequested` events are sent.
Content blocking (the profile's rule lists).
The Notifications API.
Appended to the engine's user agent; at most 64 bytes.
Frames carry damage, scroll shifts and bands.
JavaScript dialogs reach the app (else they are dismissed).
A rectangle of the view, in pixels.

| Name | IDL type | Script value |
|---|---|---|
| `x` | `I32` | int |
| `y` | `I32` | int |
| `width` | `I32` | int |
| `height` | `I32` | int |

```rhai
let value = sys::webd_view::new_rect();
```

### `Shift`

The part of the picture that moved: the area and the offset.

| Name | IDL type | Script value |
|---|---|---|
| `x` | `I32` | int |
| `y` | `I32` | int |
| `width` | `I32` | int |
| `height` | `I32` | int |
| `dx` | `I32` | int |
| `dy` | `I32` | int |

```rhai
let value = sys::webd_view::new_shift();
```

### `SchemePolicy`

A string; the constants of this module are:

- `SCHEME_POLICY_WEB` = `"Web"`
- `SCHEME_POLICY_DATA_ONLY` = `"DataOnly"`
- `SCHEME_POLICY_ORIGINS` = `"Origins"`

`SCHEME_POLICY` is the array of all of them.

### `Handoff`

A string; the constants of this module are:

- `HANDOFF_DENY` = `"Deny"`
- `HANDOFF_ASK` = `"Ask"`
- `HANDOFF_BROWSER` = `"Browser"`

`HANDOFF` is the array of all of them.

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

### `CursorShape`

A string; the constants of this module are:

- `CURSOR_SHAPE_DEFAULT` = `"Default"`
- `CURSOR_SHAPE_HAND` = `"Hand"`
- `CURSOR_SHAPE_TEXT` = `"Text"`
- `CURSOR_SHAPE_WAIT` = `"Wait"`
- `CURSOR_SHAPE_HIDDEN` = `"Hidden"`
- `CURSOR_SHAPE_RESIZE_H` = `"ResizeH"`
- `CURSOR_SHAPE_RESIZE_V` = `"ResizeV"`
- `CURSOR_SHAPE_RESIZE_NWSE` = `"ResizeNwse"`
- `CURSOR_SHAPE_RESIZE_NESW` = `"ResizeNesw"`

`CURSOR_SHAPE` is the array of all of them.

### `DialogKind`

A string; the constants of this module are:

- `DIALOG_KIND_ALERT` = `"Alert"`
- `DIALOG_KIND_CONFIRM` = `"Confirm"`
- `DIALOG_KIND_PROMPT` = `"Prompt"`

`DIALOG_KIND` is the array of all of them.

### `NavigationKind`

A string; the constants of this module are:

- `NAVIGATION_KIND_LINK_CLICKED` = `"LinkClicked"`
- `NAVIGATION_KIND_FORM_SUBMITTED` = `"FormSubmitted"`
- `NAVIGATION_KIND_BACK_FORWARD` = `"BackForward"`
- `NAVIGATION_KIND_RELOAD` = `"Reload"`
- `NAVIGATION_KIND_FORM_RESUBMITTED` = `"FormResubmitted"`
- `NAVIGATION_KIND_OTHER` = `"Other"`

`NAVIGATION_KIND` is the array of all of them.

### `BlockingStatus`

A string; the constants of this module are:

- `BLOCKING_STATUS_READY` = `"Ready"`
- `BLOCKING_STATUS_NOT_STORED` = `"NotStored"`
- `BLOCKING_STATUS_STALE` = `"Stale"`
- `BLOCKING_STATUS_COMPILE_FAILED` = `"CompileFailed"`
- `BLOCKING_STATUS_UNAVAILABLE` = `"Unavailable"`

`BLOCKING_STATUS` is the array of all of them.

### `PermissionAnswer`

A string; the constants of this module are:

- `PERMISSION_ANSWER_BLOCK` = `"Block"`
- `PERMISSION_ANSWER_ALLOW` = `"Allow"`
- `PERMISSION_ANSWER_DISMISS` = `"Dismiss"`

`PERMISSION_ANSWER` is the array of all of them.

## See also

- [Messenger APIs for Rhai](index.md)
- [msg: Messenger from Rhai](../msg.md)
- [os.lazy.webd.view.v1 on the wire](../../idl/os.lazy.webd.view.v1.md)
