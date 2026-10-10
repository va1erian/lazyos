# `os.lazy.notify.v1`

Interface id: `0xedc6a2bd550db5ab`

Desktop notifications (docs/notify-plan.md, issue #747): a service or app
tells the user that something happened, and LazyShell shows it as a toast.
Served by LazyShell on Messenger under the name `os.lazy.notify`.

The shell owns the presenter: placement, stacking, animation, the
timeouts and the policy (rate limits, do-not-disturb, per-app mute). A
sender only says *what* and never *how*, so a hung app cannot stall a
toast. Nothing is kept: there is no history and no way to list what was
shown, so a toast that is missed is gone. (An app with a tray item sets
the tray's `Attention` state alongside a Warn or Error toast: that is the
durable signal, docs/tray-plan.md section 10.)

Identity comes from the kernel, never from the call: the toast's header
shows the registry name `init.ListApps` gives the sender's app, so an app
cannot post as another app or as the system. A caller that is not a launched
app but runs in the shell's session under the shell's user (a script run
from the Terminal) is shown as "Terminal". Callers must be in the shell's
login session and be uid 0 or the shell's uid; a labelled app also needs
this interface in its manifest. Anyone else gets `EACCES`.

Every field is validated: a title of at most 64 characters (not empty), a
body of at most 256, at most 3 actions of at most 24 characters with
unique non-zero ids, a Lucide name or an image of at most 64 x 64
pixels, `progress` at most 100, a `level` that is a `Level` value;
anything else is `EINVAL`. An unusable icon is not an error: it falls
back to the level's own. At most 16 notifications of one sender are
alive (shown or waiting) at once (`ENOSPC`); a sender posting faster than
the rate limit gets `EBUSY` (nothing is shown). Failures reply with the
standard error field (id 15, `docs/midl.md`).

## Methods

| Method | Id | Kind | Signature |
|---|---|---|---|
| Notify | 1 | sync | `(level: Level, title: String, body: String, icon: Option<Icon>, actions: Array<Action>, progress: Option<U32>, replaces_id: Option<U64>, timeout: Option<U32>) -> (id: U64)` |
| Close | 2 | sync | `(id: U64) -> ()` |
| Listen | 3 | sync | `(events: Channel<os.lazy.notify.events.v1>) -> ()` |

## Objects

Kernel objects a request carries, in the order of the parcel's
object list (the index each field must hold).

| Method | Field | Type | Object |
|---|---|---|---|
| Listen | `events` | `Channel<os.lazy.notify.events.v1>` | `objects[0]`, a channel the receiver sends `os.lazy.notify.events.v1` on |

## struct `Icon`

- `lucide: Option<String>`
- `image: Option<Image>`

## struct `Image`

- `width: U32`
- `height: U32`
- `data: Bytes`

## struct `Action`

- `id: U32`
- `label: String`

## enum `Level`

- Info, Success, Warn, Error

## enum `CloseReason`

- User, Timeout, Replaced, Sender, Restart, Muted
