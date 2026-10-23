# `os.lazy.webd.browser.v1`

Interface id: `0x2c352157f6ab1380`

The scope of a web browser: everything `os.lazy.webd.net.v1` allows, plus
the shared `browser` profile kept on disk, downloads into the person's
Downloads folder, content blocking and `Handoff.Browser`.

## Methods

| Method | Id | Kind | Signature |
|---|---|---|---|
| CreateView | 1 | sync | `(config: ViewConfig, width: U32, height: U32, events: Channel<os.lazy.webd.view.v1>) -> (view: U64)` |

## Objects

Kernel objects a request carries, in the order of the parcel's
object list (the index each field must hold).

| Method | Field | Type | Object |
|---|---|---|---|
| CreateView | `events` | `Channel<os.lazy.webd.view.v1>` | `objects[0]`, a channel the receiver sends `os.lazy.webd.view.v1` on |

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
