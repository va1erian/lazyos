# `sys::pkgd`

The Rhai module for the Messenger interface `os.lazy.pkgd.v1`. Wire-level reference: [os.lazy.pkgd.v1](../../idl/os.lazy.pkgd.v1.md).

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

## Functions

### `inspect(path)`

`Inspect(path: String) -> (info: PackageInfo)`

Open and validate the `.lzp` at `path` without changing anything. Root
may name any absolute path; anyone else a file under `/transient` or
their own home folder (the path is normalised first). `problems` is empty for a package that
could be installed; otherwise it lists every reason it cannot be.

Arguments:

| Name | IDL type | Script value |
|---|---|---|
| `path` | `String` | string |

Returns `info`: `PackageInfo` (map (`PackageInfo`)).

```rhai
let result = sys::pkgd::inspect(path);
```

### `install(path)`

`Install(path: String) -> (app: Installed)`

Install the package at `path` (the same source rule as `Inspect`):
extract it to its install directory under `/apps`, copy its
`docs/*.md` to `/docs/apps/<system_name>/`, record it, register its
MIME verbs, load its policy, then publish
`system/events/pkg/install`. Fails if the same `system_name` is already
installed at this version and digest. Needs the caller to be the
session owner or root; `pkgd` audits who asked.

Arguments:

| Name | IDL type | Script value |
|---|---|---|
| `path` | `String` | string |

Returns `app`: `Installed` (map (`Installed`)).

```rhai
let result = sys::pkgd::install(path);
```

### `remove(system_name)`

`Remove(system_name: String) -> ()`

Remove `system_name`: stop its running instances, unregister its MIME
verbs, revoke its policy, delete its install directory and its
documentation, then publish `system/events/pkg/remove`. User data
under `/home` is kept.

Arguments:

| Name | IDL type | Script value |
|---|---|---|
| `system_name` | `String` | string |

Returns `()`.

```rhai
let result = sys::pkgd::remove(system_name);
```

### `list()`

`List() -> (apps: Array<Installed>)`

Every installed app, in install order.

Returns `apps`: `Array<Installed>` (array of map (`Installed`)).

```rhai
let result = sys::pkgd::list();
```

### `installed(system_name)`

`Installed(system_name: String) -> (app: Option<Installed>)`

One installed app by `system_name`, if present.

Arguments:

| Name | IDL type | Script value |
|---|---|---|
| `system_name` | `String` | string |

Returns `app`: `Option<Installed>` (map (`Installed`) or ()).

```rhai
let result = sys::pkgd::installed(system_name);
```

### `label_name(label_id)`

`LabelName(label_id: U32) -> (label: String)`

The name of the policy label with the kernel id `label_id`
(`app:<system_name>`, `tool:<system_name>`, ...). Reading a label's
name takes `CAP_SETUID`, which `elevd` deliberately lacks, so it asks
here to tell a developer tool's shared label from an app's before it
lets an administrator's approval stand (`libs/elevpolicy`,
`approvals`). Answered to `elevd` alone; `ENOENT` for an id the
kernel does not know.

Arguments:

| Name | IDL type | Script value |
|---|---|---|
| `label_id` | `U32` | int |

Returns `label`: `String` (string).

```rhai
let result = sys::pkgd::label_name(label_id);
```

### `provisioned()`

`Provisioned() -> (state: ProvisionState)`

Whether this start's core package provisioning is finished, and what
it did (`docs/packages.md`, core packages). `pkgd` answers it while it
provisions, so a client such as `init`'s autostart can wait for it
without blocking; `Install` and `Remove` are refused with `EAGAIN`
until `done`.

Returns `state`: `ProvisionState` (map (`ProvisionState`)).

```rhai
let result = sys::pkgd::provisioned();
```

### `develop(path, confirm)`

`Develop(path: String, confirm: Bool) -> (label: String, approved: Bool)`

Approve a development run of the package at `path` (the same source
rule as `Inspect`; issue #529): validate it, compile its permissions and
load them for the label `dev:<system_name>`, which an IDE whose
manifest says `develop = true` may then give the program it runs. Only
the session owner or root, unlabelled (the Installer), may call it.
With `confirm` false nothing is asked: the rules are loaded and
`approved` is true only if this session already approved the same or a
wider rule set for that label; otherwise `approved` is false and nothing
changes, so the Installer shows the consent screen and calls again with
`confirm` true once the user accepts. Approvals live in `pkgd`'s memory
only, are dropped (and the label's rules revoked) when the session that
gave them logs out, and every one is audited as
`system/events/pkg/develop`.

Arguments:

| Name | IDL type | Script value |
|---|---|---|
| `path` | `String` | string |
| `confirm` | `Bool` | bool |

Returns a map:

| Name | IDL type | Script value |
|---|---|---|
| `label` | `String` | string |
| `approved` | `Bool` | bool |

```rhai
let result = sys::pkgd::develop(path, confirm);
```

### `develop_declined(path)`

`DevelopDeclined(path: String) -> ()`

The user declined the development consent for the package at `path`
(same caller and source rules as `Develop`). Nothing is loaded; the
refusal is audited and published as `system/events/pkg/denied` under
the package's `system_name`, so the IDE waiting for the approval stops.

Arguments:

| Name | IDL type | Script value |
|---|---|---|
| `path` | `String` | string |

Returns `()`.

```rhai
let result = sys::pkgd::develop_declined(path);
```

### `install_approved(path, digest, core, devtool)`

`InstallApproved(path: String, digest: String, core: Bool, devtool: Bool) -> (app: Installed)`

`elevd`'s install of a package an administrator approved on the
trusted prompt (docs/accounts-plan.md U2, review of #659); refused
with `EPERM` from anyone else. `digest` is the SHA-256 `Inspect`
reported for the file the prompt described: `pkgd` installs the
bytes it reads only when they hash to it, so the file cannot be
swapped between the approval and the install. `core` says what was
approved: `false` (`pkg.install`) refuses a package that would
replace a core app, `true` (`pkg.update-core`) refuses one that would
not. Plain `Install` never replaces a core app for `elevd`. `devtool`
says the administrator saw the developer-tool warning
(`pkg.install-devtool`, `docs/devtool-packages-plan.md` section 4): a
developer-tool package is installed only when it is `true` and any
other package only when it is `false`, so a tool is never installed
from a prompt that did not warn about it.

Arguments:

| Name | IDL type | Script value |
|---|---|---|
| `path` | `String` | string |
| `digest` | `String` | string |
| `core` | `Bool` | bool |
| `devtool` | `Bool` | bool |

Returns `app`: `Installed` (map (`Installed`)).

```rhai
let result = sys::pkgd::install_approved(path, digest, core, devtool);
```

### `install_debug(path, digest)`

`InstallDebug(path: String, digest: String) -> (app: Installed)`

`dbgd`'s install of a package a developer uploaded to a box built for
remote control (docs/dbgd-plan.md, v2 app swapping): accepted from
`dbgd` alone and only when `/boot/lazyos.cfg` says
`diag.dbg.control=1`, refused with `EPERM` otherwise. `path` must be
`dbgd`'s staging file (`dbgwire::control::staged_package_path`) and
the bytes must hash to `digest` (SHA-256, hex), as for
`InstallApproved`. Unlike every other install it may replace a core
app, which is what a developer iterating on one needs.

Arguments:

| Name | IDL type | Script value |
|---|---|---|
| `path` | `String` | string |
| `digest` | `String` | string |

Returns `app`: `Installed` (map (`Installed`)).

```rhai
let result = sys::pkgd::install_debug(path, digest);
```

## Types

### `ProvisionState`

Core package provisioning progress.

| Name | IDL type | Script value |
|---|---|---|
| `done` | `Bool` | bool |
| `ready` | `Bool` | bool |
| `installed` | `U64` | int |
| `upgraded` | `U64` | int |
| `kept` | `U64` | int |
| `failed` | `U64` | int |

```rhai
let value = sys::pkgd::new_provision_state();
```

### `PackageInfo`

The pass of this start is over (successfully or not).
The packages that open at login (`entry.autostart`) are provisioned
(they go first), so a session can start before `done`.
Newer versions the user installed over a core app, left in place.
What a package declares, as the consent screen shows it.

| Name | IDL type | Script value |
|---|---|---|
| `name` | `String` | string |
| `system_name` | `String` | string |
| `author` | `String` | string |
| `version` | `String` | string |
| `description` | `String` | string |
| `digest` | `String` | string |
| `install_dir` | `String` | string |
| `mime` | `Array<MimeHandler>` | array of map (`MimeHandler`) |
| `permissions` | `Array<Permission>` | array of map (`Permission`) |
| `problems` | `Array<String>` | array of string |
| `category` | `String` | string |
| `autostart` | `Bool` | bool |

```rhai
let value = sys::pkgd::new_package_info();
```

### `MimeHandler`

Lowercase hex SHA-256 of the archive.
Where it would be installed, relative to `/apps`.
Empty when the package can be installed.
The menu group (`lazypkg::Category`), `accessories` by default.
Whether the app asks to start when the user logs in.
One handled file type.

| Name | IDL type | Script value |
|---|---|---|
| `mime_type` | `String` | string |
| `verbs` | `Array<String>` | array of string |
| `has_icon` | `Bool` | bool |

```rhai
let value = sys::pkgd::new_mime_handler();
```

### `Permission`

Whether the package ships icons for this type.
One requested permission with the friendly explanation the installer
shows. `kind` is `interface`, `topic`, `file`, `network`, `develop` or
`resident` (`entry.resident`, value `true`); `risk` is
`low`, `medium` or `high`. `explanation` comes from `pkgd`'s table
keyed by MIDL interface name, so every client shows the same words.

| Name | IDL type | Script value |
|---|---|---|
| `kind` | `String` | string |
| `value` | `String` | string |
| `risk` | `String` | string |
| `explanation` | `String` | string |

```rhai
let value = sys::pkgd::new_permission();
```

### `Installed`

An installed app, as `confd` records it under `sys/apps/<system_name>`.

| Name | IDL type | Script value |
|---|---|---|
| `system_name` | `String` | string |
| `name` | `String` | string |
| `version` | `String` | string |
| `install_dir` | `String` | string |
| `digest` | `String` | string |
| `binary` | `String` | string |
| `installed_at` | `U64` | int |
| `abi` | `String` | string |
| `args` | `Array<String>` | array of string |
| `origin` | `U32` | int |
| `category` | `String` | string |
| `autostart` | `Bool` | bool |
| `verbs` | `Array<String>` | array of string |
| `resident` | `Bool` | bool |
| `installer` | `U32` | int |
| `machine_wide` | `Bool` | bool |
| `kind` | `String` | string |
| `shell` | `Bool` | bool |

```rhai
let value = sys::pkgd::new_installed();
```

### `PkgEvent`

Install directory relative to `/apps`.
Entry binary, relative to the install directory (`bin/<name>.elf`).
Kernel ticks at install time.
The program's ABI, `native` or `linux`: `init` needs it to pick the
spawn personality, and an ELF header cannot tell them apart.
The manifest's fixed `entry.args`, passed before any launch path.
An `Origin` value, cached from the shipped set at provisioning and
install time so readers need not open `/system/packages`.
The menu group (`lazypkg::Category`).
Whether the app starts when a session opens (`entry.autostart`).
The manifest's `[[mime]]` verbs, de-duplicated, in manifest order.
Whether the app is resident (`entry.resident`): it may run with no
window, once per session, with an icon in the taskbar.
The uid that asked for the install, as the kernel stamped the
request. Only that user's sessions open the app at login when it
sets `autostart` (and it is neither core nor `machine_wide`): an
app one user installed must not start inside everyone else's
session, an administrator's included. A row recorded before this
field reads as 0, which no login session has.
Installed by a system caller (`elevd` after an administrator
approved it, `dbgd`, provisioning): a machine-wide choice, so
`autostart` opens it in every user's session.
The package class the manifest named when it was installed:
`developer-tool` (`[app] kind`, `docs/devtool-packages-plan.md`),
whose app runs under the label `tool:<system_name>`, or empty for
an application (`app:<system_name>`). A tool's row is used only
when it is `machine_wide` or core, which is how `pkgd` and `init`
know an administrator approved it.
The tool is the user's shell (`[app] shell = true`,
`docs/devtool-packages-plan.md` "T-later: Terminal"): a program
run under its label acts as the session's owner towards `pkgd`
and `init`, as an unlabelled program would. Only a core package
(`origin` core) is ever recorded with it; `false` for every
application and every other tool. Readers go through
`pkgstore::shellrole::row_is_shell`, never this field alone.
One audit record: the payload of `system/events/pkg/<op>`, where `op`
is `install`, `remove`, `denied`, `provision` (a core package
installed, upgraded or re-marked at startup, or the end of a pass),
`develop` (a development run approved: `ok` true, `detail` the label)
or `undevelop` (a session's development approvals dropped at logout). The same record, hex-encoded with
a chained SHA-256, is appended to `/logs/pkg.log`.

| Name | IDL type | Script value |
|---|---|---|
| `op` | `String` | string |
| `system_name` | `String` | string |
| `version` | `String` | string |
| `install_dir` | `String` | string |
| `digest` | `String` | string |
| `actor_uid` | `U64` | int |
| `ok` | `Bool` | bool |
| `detail` | `String` | string |

```rhai
let value = sys::pkgd::new_pkg_event();
```

### `Origin`

A string; the constants of this module are:

- `ORIGIN_USER` = `"User"`
- `ORIGIN_CORE` = `"Core"`

`ORIGIN` is the array of all of them.

## Topics

### `system/events/pkg/{op}`

uid of the task that asked.
Friendly text: the error for a failure, empty on success.
Published on every install, removal, refused request and provisioning
step. Not retained:
`List` is the state, the events are the trail.

Payload `PkgEvent`, QoS `latest`.

Helpers: `PKG_PATTERN`, `pkg_topic(op)`, `on_pkg(op, handler)`, `subscribe_pkg(op)`, `publish_pkg(op, payload)`.

```rhai
sys::pkgd::on_pkg(op, |event| print(event.payload));
```

## See also

- [Messenger APIs for Rhai](index.md)
- [msg: Messenger from Rhai](../msg.md)
- [os.lazy.pkgd.v1 on the wire](../../idl/os.lazy.pkgd.v1.md)
