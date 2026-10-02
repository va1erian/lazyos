# `os.lazy.pkgd.v1`

Interface id: `0x2e65545739956542`

The application package manager (`docs/packages.md`, phase 3 of the
package system).

`pkgd` is the only task that writes `/apps` and `/docs/apps`, records
installed apps in `confd`, registers their MIME verbs with `mimed` and loads
their Messenger
policy into the kernel (`acl_load`, `CAP_IPC_CONTROL`). A GUI installer is
an unprivileged client: it calls `Inspect`, shows the user what the package
asks for, and forwards the user's yes as `Install`. Failures are returned
as a structured error field (errno-style code, friendly text), not as a
typed reply; a package that fails validation reports every problem in
`PackageInfo.problems` instead of an error so the installer can list them.

## Methods

| Method | Id | Kind | Signature |
|---|---|---|---|
| Inspect | 1027767735 | sync | `(path: String) -> (info: PackageInfo)` |
| Install | 890027328 | sync | `(path: String) -> (app: Installed)` |
| Remove | 564498461 | sync | `(system_name: String) -> ()` |
| List | 220805025 | sync | `() -> (apps: Array<Installed>)` |
| Installed | 1755800129 | sync | `(system_name: String) -> (app: Option<Installed>)` |
| Provisioned | 1076218465 | sync | `() -> (state: ProvisionState)` |

## Topics

| Topic | Payload | QoS | Retained | Permissions |
|---|---|---|---|---|
| `system/events/pkg/+` | `PkgEvent` | latest | no | `publish:system/events/pkg/+`, `subscribe:system/events/pkg/+` |

## struct `ProvisionState`

- `done: Bool`
- `ready: Bool`
- `installed: U64`
- `upgraded: U64`
- `kept: U64`
- `failed: U64`

## struct `PackageInfo`

- `name: String`
- `system_name: String`
- `author: String`
- `version: String`
- `description: String`
- `digest: String`
- `install_dir: String`
- `mime: Array<MimeHandler>`
- `permissions: Array<Permission>`
- `problems: Array<String>`
- `category: String`
- `autostart: Bool`

## struct `MimeHandler`

- `mime_type: String`
- `verbs: Array<String>`
- `has_icon: Bool`

## struct `Permission`

- `kind: String`
- `value: String`
- `risk: String`
- `explanation: String`

## struct `Installed`

- `system_name: String`
- `name: String`
- `version: String`
- `install_dir: String`
- `digest: String`
- `binary: String`
- `installed_at: U64`
- `abi: String`
- `args: Array<String>`
- `origin: U32`
- `category: String`
- `autostart: Bool`
- `verbs: Array<String>`

## struct `PkgEvent`

- `op: String`
- `system_name: String`
- `version: String`
- `install_dir: String`
- `digest: String`
- `actor_uid: U64`
- `ok: Bool`
- `detail: String`

## enum `Origin`

- User, Core
