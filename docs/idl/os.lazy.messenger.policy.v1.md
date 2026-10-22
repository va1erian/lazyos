# `os.lazy.messenger.policy.v1`

Interface id: `0xe625b4ee97525d37`

The kernel's label-keyed Messenger policy (application package system,
phase 1; `docs/architecture/ipc-security.md`).

A process stamped with a label is judged by the rules loaded here, never by
its uid. There are five label forms: `app:<reverse.dns.name>` (an installed
app), `app:<reverse.dns.name>/<name>` (a helper program of one),
`system:<name>` (a platform service), `dev:<reverse.dns.name>` (an app run
from an IDE) and `tool:<reverse.dns.name>` (a developer tool). Apart from a
tool, a labelled process is default-deny: it may only use what its own
namespace grants (`app.<id>.<name>` services, `app/<id>/...` topics) plus the
allow rules loaded here. A tool owns the same namespaces and may use any
further name or topic that is not reserved; the rules a package manager
loads for it are default allow after a short list of explicit denies. `LoadLabel` is the one way rules get in: it is the
request body of the privileged `acl_load` messenger op, which needs
`CAP_IPC_CONTROL`, and it is never a parcel any service receives.

## Methods

| Method | Id | Kind | Signature |
|---|---|---|---|
| LoadLabel | 888159937 | sync | `(label: String, rules: Array<LabelRule>, program: Option<String>) -> ()` |

## struct `LabelRule`

- `interface_id: U64`
- `method: U32`
- `allow: Bool`
