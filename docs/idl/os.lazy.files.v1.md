# `os.lazy.files.v1`

Interface id: `0x95bb1421ccc6b3e7`

Files, the desktop file explorer (`os.lazy.files`, issue #488).

Files serves no methods: what it shares with the rest of the session is
the selection of its focused folder window, so another app (the Editor, a
script) can act on "what the user picked" without asking Files. Copy and
paste go through `os.lazy.clipboard.v1` as `text/uri-list`, and the
`reveal` verb reaches Files through `mimed.Open` and `init.Launch` with
the item's path as the argument.

The topic is per session: an installed app may publish or subscribe only
under its own kernel-stamped session, never another's.

## Methods

| Method | Id | Kind | Signature |
|---|---|---|---|

## Topics

| Topic | Payload | QoS | Retained | Permissions |
|---|---|---|---|---|
| `session/+/selection` | `Selection` | latest | yes | `publish:session/+/selection`, `subscribe:session/+/selection` |

## struct `Selection`

- `folder: String`
- `paths: Array<String>`
