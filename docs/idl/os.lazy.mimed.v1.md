# `os.lazy.mimed.v1`

Interface id: `0x69d01278f9971fe6`

The MIME database and open-with registry (issues #116, #158).

`mimed` guesses a MIME type from a path, resolves which app handles a type
and verb, and turns "open this file" into a launch request for `init` plus
a `system/events/open/<app>` event on the central broker. Failures are
returned as a structured error field (errno-style code, friendly text), not
as a typed reply.

## Methods

| Method | Id | Kind | Signature |
|---|---|---|---|
| Guess | 1763202418 | sync | `(path: String) -> (mime: String)` |
| Lookup | 1772818603 | sync | `(mime: String, verb: String) -> (app: Option<String>)` |
| Verbs | 1649509833 | sync | `(mime: String) -> (verbs: Array<String>)` |
| Open | 1401622761 | sync | `(path: String, verb: String) -> (app: String, mime: String, topic: String, published: Bool, launched: Bool)` |
| Register | 658098656 | sync | `(mime: String, app: String, verb: String) -> ()` |

## Topics

| Topic | Payload | QoS | Retained | Permissions |
|---|---|---|---|---|
| `system/events/open/+` | `OpenEvent` | latest | no | `publish:system/events/open/+`, `subscribe:system/events/open/+` |

## struct `OpenEvent`

- `path: String`
- `mime: String`
- `verb: String`
