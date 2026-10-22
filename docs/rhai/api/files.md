# `sys::files`

The Rhai module for the Messenger interface `os.lazy.files.v1`. Wire-level reference: [os.lazy.files.v1](../../idl/os.lazy.files.v1.md).

Files, the desktop file explorer (`os.lazy.files`, issue #488).

Files serves no methods: what it shares with the rest of the session is
the selection of its focused folder window, so another app (the Editor, a
script) can act on "what the user picked" without asking Files. Copy and
paste go through `os.lazy.clipboard.v1` as `text/uri-list`, and the
`reveal` verb reaches Files through `mimed.Open` and `init.Launch` with
the item's path as the argument.

The topic is per session: an installed app may publish or subscribe only
under its own kernel-stamped session, never another's.

## Types

### `Selection`

One folder window's selection.

| Name | IDL type | Script value |
|---|---|---|
| `folder` | `String` | string |
| `paths` | `Array<String>` | array of string |

```rhai
let value = sys::files::new_selection();
```

## Topics

### `session/{session}/selection`

The folder the window shows, as an absolute path.
The selected items, as absolute paths in view order; empty when
nothing is selected.
Published whenever the selection of a Files window changes (and when
a window opens or gains a new listing), retained so a late subscriber
learns the current selection at once.

Payload `Selection`, QoS `latest`, retained.

Helpers: `SESSION_SELECTION_PATTERN`, `session_selection_topic(session)`, `on_session_selection(session, handler)`, `subscribe_session_selection(session)`, `publish_session_selection(session, payload)`.

```rhai
sys::files::on_session_selection(session, |event| print(event.payload));
```

## See also

- [Messenger APIs for Rhai](index.md)
- [msg: Messenger from Rhai](../msg.md)
- [os.lazy.files.v1 on the wire](../../idl/os.lazy.files.v1.md)
