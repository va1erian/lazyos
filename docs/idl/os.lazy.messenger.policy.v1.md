# `os.lazy.messenger.policy.v1`

Interface id: `0xe625b4ee97525d37`

The kernel's label-keyed Messenger policy (application package system,
phase 1; `docs/architecture/ipc-security.md`).

A process stamped with a label (`app:<reverse.dns.name>` or
`system:<name>`) is default-deny: it may only use what its own namespace
grants (`app.<id>.<name>` services, `app/<id>/...` topics) plus the allow
rules loaded here. `LoadLabel` is the one way rules get in: it is the
request body of the privileged `acl_load` messenger op, which needs
`CAP_IPC_CONTROL`, and it is never a parcel any service receives.

## Methods

| Method | Id | Kind | Signature |
|---|---|---|---|
| LoadLabel | 888159937 | sync | `(label: String, rules: Array<LabelRule>) -> ()` |

## struct `LabelRule`

- `interface_id: U64`
- `method: U32`
- `allow: Bool`
