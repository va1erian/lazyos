# `os.lazy.process.label.spawn.v1`

Interface id: `0x2b9f30ad1cbea35e`

Kernel ACL scope for spawning a child into a development label (issue #529,
`docs/lazyrad-package-plan.md` section 3). A labelled task (an IDE) may give
a child it spawns the label `dev:<system_name>` only when its own rules
allow this interface id with `fnv1a32(<the dev label>)` as the method, and
only while `pkgd` holds an approved rule set for that label. The child keeps
the caller's uid, gid and session, and never gains a capability. A manifest
asks for it with `develop = true`, which compiles to the wildcard method.
Only the interface id is contractual; nothing ever serves this interface.

## Methods

| Method | Id | Kind | Signature |
|---|---|---|---|
| Assign | 938075628 | sync | `(label: String) -> ()` |
