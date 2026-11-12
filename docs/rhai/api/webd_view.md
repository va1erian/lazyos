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
plus: at most 16 web processes per user for full views, and a view that
would start one while the user's memory charge cannot hold it is `EDQUOT`
(a view of a new app, profile or `process_key` past that is `EDQUOT`); at
most 64 views for a client with the browser scope (8 otherwise); at most
32 `process_key`s at once per client; at most 64 origins of at most 256
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

### `recover(view, reload)`

`Recover(view: U64, reload: Bool) -> ()`

The page's process does not answer (the app got `Responsive(false)`) and
the person chose a way out (LazyWeb, P4): the view's web process is
ended. With `reload` the page loads again in a fresh process. Without it
the page stays stopped: the engine reports the page as ended by request,
which is `ViewLost` with reason `Other`, and the app makes a new view.
The engine refuses (and does nothing) while other views share the
process; the service does not know.

Arguments:

| Name | IDL type | Script value |
|---|---|---|
| `view` | `U64` | int |
| `reload` | `Bool` | bool |

Returns `()`.

```rhai
let result = sys::webd_view::recover(view, reload);
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

### `wheel(view, x, y, ticks_x, ticks_y, mods, seq)`

`Wheel(view: U64, x: I32, y: I32, ticks_x: I32, ticks_y: I32, mods: U32, seq: U64) -> () oneway`

A wheel turn; a non-zero `seq` (rising) is answered by `WheelDone`.

Arguments:

| Name | IDL type | Script value |
|---|---|---|
| `view` | `U64` | int |
| `x` | `I32` | int |
| `y` | `I32` | int |
| `ticks_x` | `I32` | int |
| `ticks_y` | `I32` | int |
| `mods` | `U32` | int |
| `seq` | `U64` | int |

One-way: the call returns `()` once the message is queued.

```rhai
let result = sys::webd_view::wheel(view, x, y, ticks_x, ticks_y, mods, seq);
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

### `notify_forget(view, origin)`

`NotifyForget(view: U64, origin: String) -> ()`

Forget the profile's decision about `origin` (LazyWeb, P4): the page's
`Notification.permission` is `default` again and its next request is
asked again (`NotifyPermissionRequest`). A view made with
`notifications` only; `origin` is checked as in `NotifyPermission`.

Arguments:

| Name | IDL type | Script value |
|---|---|---|
| `view` | `U64` | int |
| `origin` | `String` | string |

Returns `()`.

```rhai
let result = sys::webd_view::notify_forget(view, origin);
```

### `notify_state(view, notification, state)`

`NotifyState(view: U64, notification: U64, state: U32) -> ()`

What became of notification `notification` that `NotifyShow` gave a
view made with `notify_in_client` (a `NotifyStateKind`): the app's toast
is on screen, was clicked (the page's click handler runs) or went away.
`ESRCH` for a notification webd did not give this view or that is over;
`EACCES` for a view without `notify_in_client`.

Arguments:

| Name | IDL type | Script value |
|---|---|---|
| `view` | `U64` | int |
| `notification` | `U64` | int |
| `state` | `U32` | int |

Returns `()`.

```rhai
let result = sys::webd_view::notify_state(view, notification, state);
```

### `recover_group(view)`

`RecoverGroup(view: U64) -> ()`

Like `Recover` without a reload, for a view that shares its web process
with others (tabs of one site, `ViewConfig.process_key`): the process
of the view's group is ended and every view in it is reported as ended
by request (`ViewLost` with reason `Other`). Each app makes its views
again. Ids 40 to 43 are kept for the data and credential calls of
docs/lazyweb-plan.md section 8.

Arguments:

| Name | IDL type | Script value |
|---|---|---|
| `view` | `U64` | int |

Returns `()`.

```rhai
let result = sys::webd_view::recover_group(view);
```

### `detach_slots(view)`

`DetachSlots(view: U64) -> (seq: U64)`

Detach every slot of the view, as `Resize` does, without resizing it
and without costing the page a layout: a view that is not on screen
(a background tab) gives its slot memory back to the client's quota.
The app attaches slots again and asks for a whole frame (`Refresh`)
when the view comes back. Replies with the sequence number of the last
`Frame` webd sent for the view (0 for none): frames numbered up to it
were painted into slots that are gone, and may still be queued for the
app; it ignores them rather than copy from a slot attached later.

Arguments:

| Name | IDL type | Script value |
|---|---|---|
| `view` | `U64` | int |

Returns `seq`: `U64` (int).

```rhai
let result = sys::webd_view::detach_slots(view);
```

### `set_user_agent(view, user_agent)`

`SetUserAgent(view: U64, user_agent: String) -> ()`

Change the user agent the page's requests and `navigator.userAgent`
show, from the next load on (a tab asking for the desktop site). The
view must have been made through `os.lazy.webd.browser.v1` with a
`user_agent` or a `user_agent_suffix` (else `EACCES`: the engine's
site quirks, which put a fixed agent on every request, are off for such
a view only). `user_agent` is printable ASCII, at most 256 bytes; ""
restores the agent the view was made with.

Arguments:

| Name | IDL type | Script value |
|---|---|---|
| `view` | `U64` | int |
| `user_agent` | `String` | string |

Returns `()`.

```rhai
let result = sys::webd_view::set_user_agent(view, user_agent);
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
| `notify_in_client` | `Bool` | bool |
| `process_key` | `String` | string |
| `user_agent` | `String` | string |

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
With `notifications`: the app shows the page's notifications
(`NotifyShow`, answered with `NotifyState`) instead of webd, so a
click on one brings the app's window forward.
Places the view in a web process of its own within the caller's
group: views of one client with the same key share a process,
views with other keys never do (tabs of one site together, other
sites apart). Opaque to webd: at most 64 printable ASCII bytes, ""
is the client's default group. It can only split the caller's own
views; it never joins another client's. Browser scope only
(`EACCES` otherwise), and only for the `browser`
profile kept on disk (`EINVAL` otherwise: an in-memory store is
shared only through a live group mate, so keyed private windows
would share no cookies). At most 32 keys at once per client
(`EDQUOT` past that: reuse a key).
Replaces the engine's user agent entirely (`user_agent_suffix` only
appends to it): printable ASCII, at most 256 bytes, browser scope
only (`EACCES` otherwise), and not together with a suffix.
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

### `NotifyStateKind`

A string; the constants of this module are:

- `NOTIFY_STATE_KIND_SHOWN` = `"Shown"`
- `NOTIFY_STATE_KIND_CLICKED` = `"Clicked"`
- `NOTIFY_STATE_KIND_CLOSED` = `"Closed"`

`NOTIFY_STATE_KIND` is the array of all of them.

## See also

- [Messenger APIs for Rhai](index.md)
- [msg: Messenger from Rhai](../msg.md)
- [os.lazy.webd.view.v1 on the wire](../../idl/os.lazy.webd.view.v1.md)
