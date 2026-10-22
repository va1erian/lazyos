# `sys::init`

The Rhai module for the Messenger interface `os.lazy.init.v1`. Wire-level reference: [os.lazy.init.v1](../../idl/os.lazy.init.v1.md).

The userspace service supervisor (issues #93, #158): the supervision table,
the built-in app registry and the app-launch path.

`init` serves a `router` topic broker on the same endpoint; those topic
payloads are not part of this interface. Failures are returned as a
structured error field (errno-style code, friendly text), not as a typed
reply, so the error field is hand-written next to these stubs.

## Functions

### `services()`

`Services() -> (services: Array<ServiceStatus>)`

Snapshot the supervision table.

Returns `services`: `Array<ServiceStatus>` (array of map (`ServiceStatus`)).

```rhai
let result = sys::init::services();
```

### `launch(app, args, session)`

`Launch(app: String, args: String, session: U64) -> (app: String, pid: U64, session: U64, existing: Bool)`

Launch an app as a session child. `session` 0 means the caller's own
session; only the session's owner (or root) may launch into it. `args`
is empty or one absolute path (at most 1024 bytes, no control
character or `"`), appended to the app's fixed arguments as a single
`argv` item; any other value is refused with `EINVAL`. A resident app
(`AppInfo.resident`) already running in that session starts nothing:
`init` sends its instance `Reopen(args)` (`os.lazy.init.app.events.v1`)
and answers `existing` true with that instance's pid.

Arguments:

| Name | IDL type | Script value |
|---|---|---|
| `app` | `String` | string |
| `args` | `String` | string |
| `session` | `U64` | int |

Returns a map:

| Name | IDL type | Script value |
|---|---|---|
| `app` | `String` | string |
| `pid` | `U64` | int |
| `session` | `U64` | int |
| `existing` | `Bool` | bool |

```rhai
let result = sys::init::launch(app, args, session);
```

### `list_apps()`

`ListApps() -> (apps: Array<AppInfo>)`

Enumerate the app registry: the built-in apps first, then the apps the
package manager installed (`AppInfo.installed`; the core packages
first, then the others by `system_name`), read afresh from the
configuration registry on every call. `hidden` is the caller's own.

Returns `apps`: `Array<AppInfo>` (array of map (`AppInfo`)).

```rhai
let result = sys::init::list_apps();
```

### `stop(app)`

`Stop(app: String) -> (stopped: U64)`

Stop every running instance of the app `app` (the app id, as `Launch`
takes it), retiring its supervision row without a restart: an
instance that watches its lifecycle (`os.lazy.init.app.v1`), or a
resident app that may still come to watch, is sent `Quit` and killed
if it still runs 3 s after this call (docs/tray-plan.md section 5);
any other is killed at once. The reply comes once every instance has
exited. `stopped` is how many were running. Only root, a holder of
`CAP_SETUID` (the package manager) or the session owner may stop; an
owner reaches only instances in their own session. Unknown or idle apps
are not an error, `stopped` is just 0.

Arguments:

| Name | IDL type | Script value |
|---|---|---|
| `app` | `String` | string |

Returns `stopped`: `U64` (int).

```rhai
let result = sys::init::stop(app);
```

### `shutdown(mode, reason, force)`

`Shutdown(mode: U32, reason: String, force: Bool) -> (accepted: Bool, phase: String)`

Begin an orderly shutdown (docs/shutdown.md): `mode` is a `PowerMode`
value. The reply is immediate; `init` then stops the session apps, the
services in reverse dependency order, syncs and calls the kernel's
`power`. `reason` is logged (at most 128 bytes, no control character).
Root or any caller in a login session may ask; a labelled (installed)
app is refused with `EPERM`, and an unknown mode is `EINVAL`. A request
while a shutdown is already running is not an error: `accepted` is true
and `phase` is the current phase. `force` skips the graceful stop: every
remaining child is killed at once and the machine stops.

Arguments:

| Name | IDL type | Script value |
|---|---|---|
| `mode` | `U32` | int |
| `reason` | `String` | string |
| `force` | `Bool` | bool |

Returns a map:

| Name | IDL type | Script value |
|---|---|---|
| `accepted` | `Bool` | bool |
| `phase` | `String` | string |

```rhai
let result = sys::init::shutdown(mode, reason, force);
```

### `start_driver(driver, device, ifname)`

`StartDriver(driver: String, device: U64, ifname: String) -> (started: Bool, pid: U64)`

Start the driver row `driver` for device `device` (issue #497,
docs/driver-plan.md section 3.6). Only the running task of the `devd`
row may ask: `init` keeps each driver's program, credentials and
arguments, and `devd` names only the row and the device it matched,
which the driver receives as `dev=<device>`. `ifname` is the name `devd`
gave the card (`eth0`) for a driver that serves one named card per
instance (`netdrv`): the driver receives it as `ifname=<ifname>` and
the row may run once per card. Empty for a driver whose Messenger name
is unique (`sndd`): a row already running for that device is not an
error (`started` is false) and one running for another device is
`EBUSY` (one card per driver). A non-empty `ifname` that is not a
lower-case letter run followed by digits (at most 15 bytes), or that
names an interface another card holds, is `EINVAL`. An unknown row is
`ENOENT`, any other caller `EPERM`, and a request during a shutdown
`EBUSY`.

Arguments:

| Name | IDL type | Script value |
|---|---|---|
| `driver` | `String` | string |
| `device` | `U64` | int |
| `ifname` | `String` | string |

Returns a map:

| Name | IDL type | Script value |
|---|---|---|
| `started` | `Bool` | bool |
| `pid` | `U64` | int |

```rhai
let result = sys::init::start_driver(driver, device, ifname);
```

### `ready()`

`Ready() -> () oneway`

A supervised service tells `init` it is serving (docs/performance-plan.md
P7.3): its name is registered and requests are answered. Rows that
depend on it start only then, and the desktop's apps only once every
boot service is ready. Only the running task of a manifest row counts;
anything else is ignored.

One-way: the call returns `()` once the message is queued.

```rhai
let result = sys::init::ready();
```

### `report_failure(reason)`

`ReportFailure(reason: String) -> () oneway`

A launched app says why it is about to fail (issue #549), so the
desktop's "stopped unexpectedly" notice can show more than an exit
code. Only the running task of a launched row counts; anything else
is ignored. `reason` is shown, never parsed: control characters become
spaces and it is cut to 512 bytes. A later report replaces an earlier
one, and every new run starts with none.

Arguments:

| Name | IDL type | Script value |
|---|---|---|
| `reason` | `String` | string |

One-way: the call returns `()` once the message is queued.

```rhai
let result = sys::init::report_failure(reason);
```

### `home(op, name, uid, gid)`

`Home(op: String, name: String, uid: U32, gid: U32) -> ()`

Make or retire an account's home (docs/accounts-plan.md U1). Accepted
from the accounts service alone (the `_accounts` system uid,
unlabelled): `init` runs the change as root. `op` is `create` (a 0700
`/home/<name>` owned by `uid`/`gid`, filled from the skeleton; an
existing home is kept), `archive` (moved to
`/home/.archived/<name>-<uid>`, root-only) or `remove`. `uid` must be a
human account's (1000 or more) and `name` a login name. The reply
comes once the change is done; a failed change is `EIO`.

Arguments:

| Name | IDL type | Script value |
|---|---|---|
| `op` | `String` | string |
| `name` | `String` | string |
| `uid` | `U32` | int |
| `gid` | `U32` | int |

Returns `()`.

```rhai
let result = sys::init::home(op, name, uid, gid);
```

### `restart_service(name)`

`RestartService(name: String) -> (pid: U64)`

Restart the system service `name` (docs/accounts-plan.md U2): its
running task is killed and the supervisor starts it again as after a
crash. Accepted from `elevd` alone, after an administrator approved
it, and only for the services `elevpolicy::RESTARTABLE` lists (never
`elevd`, `logind`, `accountsd`, `keyd`, `confd`, `logd` or `init`'s
own fabric): any other name is `EPERM`, an app or an unknown row
`ENOENT`, a row not running `EAGAIN`.

Arguments:

| Name | IDL type | Script value |
|---|---|---|
| `name` | `String` | string |

Returns `pid`: `U64` (int).

```rhai
let result = sys::init::restart_service(name);
```

### `reload_service(name, binary, sha256, trial_ms)`

`ReloadService(name: String, binary: String, sha256: String, trial_ms: U32) -> (pid: U64)`

Hot-reload the system service `name` (docs/dbgd-plan.md, v2): accepted
from `dbgd` alone, and only when `/boot/lazyos.cfg` says
`diag.dbg.control=1`; never `messengerd` or `dbgd`
(`dbgwire::control::reloadable`). With `binary` empty the row is
restarted as it is. Otherwise `binary` must be `dbgd`'s staging file
for `name`: `init` copies it to a root-owned file, checks its SHA-256
against `sha256` (hex), kills the running task and starts the copy.
If the new run exits, or cannot be spawned, before `trial_ms` has
passed, `init` rolls back to the image's binary at once; otherwise it
commits. A reload lasts until `RevertService` or the next boot.
`EPERM` for another caller or a refused name, `ENOENT` for no such
manifest row, `EAGAIN` for a row still waiting for its dependencies or
a shutdown, `EINVAL` for a bad file or a digest that does not match.

Arguments:

| Name | IDL type | Script value |
|---|---|---|
| `name` | `String` | string |
| `binary` | `String` | string |
| `sha256` | `String` | string |
| `trial_ms` | `U32` | int |

Returns `pid`: `U64` (int).

```rhai
let result = sys::init::reload_service(name, binary, sha256, trial_ms);
```

### `revert_service(name)`

`RevertService(name: String) -> (pid: U64)`

Put the image's binary back for a hot-reloaded service and restart
it. Same callers and refusals as `ReloadService`; a service that was
never reloaded is just restarted.

Arguments:

| Name | IDL type | Script value |
|---|---|---|
| `name` | `String` | string |

Returns `pid`: `U64` (int).

```rhai
let result = sys::init::revert_service(name);
```

### `relaunch_app(app)`

`RelaunchApp(app: String) -> (stopped: U64, started: U64)`

Restart a running app after `pkgd.InstallDebug` replaced it
(docs/dbgd-plan.md, v2 app swapping): every running instance of the
app `app` (its id, as `Launch` takes it) is killed and launched again
in its own session with its own argument, from the app's current
install. Same callers as `ReloadService`. `stopped` counts the
instances killed, `started` the ones launched again; an app that is
not running is not an error (both 0).

Arguments:

| Name | IDL type | Script value |
|---|---|---|
| `app` | `String` | string |

Returns a map:

| Name | IDL type | Script value |
|---|---|---|
| `stopped` | `U64` | int |
| `started` | `U64` | int |

```rhai
let result = sys::init::relaunch_app(app);
```

### `reloads()`

`Reloads() -> (reloads: Array<ReloadState>)`

The hot reloads since boot, one row per service that had one.
Read-only: any caller.

Returns `reloads`: `Array<ReloadState>` (array of map (`ReloadState`)).

```rhai
let result = sys::init::reloads();
```

## Types

### `ReloadState`

One service's hot-reload state (`Reloads`).

| Name | IDL type | Script value |
|---|---|---|
| `name` | `String` | string |
| `state` | `String` | string |
| `sha256` | `String` | string |
| `pid` | `U64` | int |
| `detail` | `String` | string |

```rhai
let value = sys::init::new_reload_state();
```

### `PowerState`

The shutdown's progress: the payload of `system/power/state`.

| Name | IDL type | Script value |
|---|---|---|
| `phase` | `String` | string |
| `mode` | `U32` | int |
| `reason` | `String` | string |
| `deadline` | `U64` | int |

```rhai
let value = sys::init::new_power_state();
```

### `ServiceStatus`

One row of `init`'s supervision table.

| Name | IDL type | Script value |
|---|---|---|
| `name` | `String` | string |
| `state` | `String` | string |
| `pid` | `U64` | int |
| `restarts` | `U64` | int |
| `deps` | `String` | string |
| `health` | `String` | string |
| `label` | `U32` | int |

```rhai
let value = sys::init::new_service_status();
```

### `AppInfo`

Service name.
Service phase (`pending`/`running`/`restarting`/`stopped`/`failed`).
Task slot the supervisor started, or 0.
Restart count.
Comma-separated dependency names.
Last known health string for the service.
The kernel id of the policy label the task runs under, as the
kernel stamped it (`0`: unlabelled, and for a row that is not
running). Every process an installed app starts keeps it, so a
caller whose stamped label equals a launched app's is that app's
own code: how LazyShell names the sender of a notification whose
task is not the one `init` launched (`os.lazy.notify.v1`).
One row of `init`'s built-in app registry (issue #158): the S5 start
menu's enumeration unit and the resolution table `Launch` uses.

| Name | IDL type | Script value |
|---|---|---|
| `id` | `String` | string |
| `name` | `String` | string |
| `path` | `String` | string |
| `restart` | `String` | string |
| `verbs` | `Array<String>` | array of string |
| `installed` | `Bool` | bool |
| `origin` | `String` | string |
| `category` | `String` | string |
| `hidden` | `Bool` | bool |
| `autostart` | `Bool` | bool |
| `icon` | `String` | string |
| `resident` | `Bool` | bool |

```rhai
let value = sys::init::new_app_info();
```

### `ServiceEvent`

App id: the lowercase program stem (`top` -> `/system/bin/top`).
Display name for menus.
On-disk ELF path.
Default restart policy (`always`/`on-failure`/`once`).
MIME verbs the app handles, in registration order.
Whether the package manager installed the app (`id` is then its
`system_name`) rather than the image shipping it.
`core` (a package the image ships, not removable), `user` (a
package someone installed) or `system` (a built-in program such as
the desktop shell or the installer).
The menu group (`lazypkg::Category`): the package's, `system` (or
`development` for LazyRAD) for a built-in desktop program, empty for
a console one, which the start menu leaves out.
Whether the start menu leaves the app out for the caller: their
`user/<uid>/menu/hidden/<id>`, else the machine's
`sys/menu/hidden/<id>`. A hidden app still launches and opens files.
Whether the app opens when a session starts.
An installed app's 32-pixel icon, `icons/app-32.png` in its
install directory (every package ships one); empty for a
built-in, which the shell draws from its own icon set.
Whether the package declared `resident` (docs/tray-plan.md
section 5): it may run with no window, runs once per session and
always has a tray icon.
One service lifecycle event (issue #307): the payload of
`system/events/service/<name>`. The topic carries the service name, so
it is not repeated here; `health` is the service's retained health
topic, for display by a consumer that only logs the event.

| Name | IDL type | Script value |
|---|---|---|
| `state` | `String` | string |
| `pid` | `U64` | int |
| `restarts` | `U64` | int |
| `status` | `U64` | int |
| `health` | `String` | string |
| `detail` | `String` | string |

```rhai
let value = sys::init::new_service_event();
```

### `AppFailure`

A launched app stopped and `init` will not start it again (issue
#549): it failed while starting, kept crashing, or failed with no
restart policy. The payload of `system/events/app/<id>`. The desktop
shell of `session` tells the user, so a broken app is one clear
message rather than a window that flickers open and shut.

| Name | IDL type | Script value |
|---|---|---|
| `name` | `String` | string |
| `status` | `U64` | int |
| `summary` | `String` | string |
| `reason` | `String` | string |
| `session` | `U64` | int |
| `startup` | `Bool` | bool |
| `at` | `U64` | int |

```rhai
let value = sys::init::new_app_failure();
```

### `ResidentApp`

One running resident app of a session.

| Name | IDL type | Script value |
|---|---|---|
| `app` | `String` | string |
| `pid` | `U64` | int |

```rhai
let value = sys::init::new_resident_app();
```

### `ResidentApps`

The app id `Launch` took.
The instance's task.
The payload of `session/<session>/apps/resident`.

| Name | IDL type | Script value |
|---|---|---|
| `apps` | `Array<ResidentApp>` | array of map (`ResidentApp`) |

```rhai
let value = sys::init::new_resident_apps();
```

### `PowerMode`

A string; the constants of this module are:

- `POWER_MODE_POWER_OFF` = `"PowerOff"`
- `POWER_MODE_REBOOT` = `"Reboot"`

`POWER_MODE` is the array of all of them.

## Topics

### `system/power/state`

Phase name: `stopping` (requested), `apps`, `services`,
`quiesced`, then `power` just before the kernel call.
A `PowerMode` value.
The requester's reason text.
Absolute tick (100 Hz) by which `init` forces the stop.
The retained shutdown progress topic: absent until a shutdown starts,
then one value per phase, so the compositor can paint the
shutting-down screen and a test can follow the phases.

Payload `PowerState`, QoS `latest`, retained.

Helpers: `POWER_STATE_PATTERN`, `on_power_state(handler)`, `subscribe_power_state()`, `publish_power_state(payload)`.

```rhai
sys::init::on_power_state(|event| print(event.payload));
```

### `system/events/service/{name}`

Supervision phase (`pending`/`running`/`restarting`/`stopped`/`failed`).
Task slot of the running child, or 0.
Restart count.
Exit status of the last run (0 while running).
The service's retained health topic (`system/health/<name>`).
Human-readable detail, empty when there is none.
The retained service lifecycle topic (issue #307): `init` publishes one
on `system/events/service/<name>` for every supervision transition, so
`healthd` (which derives its rows from it) and `logd` observe the state
without polling.

Payload `ServiceEvent`, QoS `latest`, retained.

Helpers: `SERVICE_PATTERN`, `service_topic(name)`, `on_service(name, handler)`, `subscribe_service(name)`, `publish_service(name, payload)`.

```rhai
sys::init::on_service(name, |event| print(event.payload));
```

### `system/events/app/{app}`

The app's display name (its id when it has none).
Exit status of the last run (`128 + signal` for a signal).
The status as a person reads it (`exit code 2`, `signal 11
(segmentation fault)`).
The app's own `ReportFailure` text; empty when it sent none.
The login session the app ran in.
Whether it failed while starting (rather than after running).
The kernel tick (100 Hz) the failure happened at.
App failures, published by `init` on the central broker
(`messengerd`, unlike the service events above, which `init`'s own
broker carries) so any session program can subscribe. Retained per
app, so a shell that comes up after an app failed at boot still hears
of it; a subscriber judges a retained value by `at` (the shell ignores
one older than a minute). `{app}` is the app id `Launch` took.

Payload `AppFailure`, QoS `buffered`, retained.

Helpers: `APP_PATTERN`, `app_topic(app)`, `on_app(app, handler)`, `subscribe_app(app)`, `publish_app(app, payload)`.

```rhai
sys::init::on_app(app, |event| print(event.payload));
```

### `session/{session}/apps/resident`

The session's running resident apps (docs/tray-plan.md section 5),
published by `init` on the central broker on every start and exit of
one. Retained, so a (restarted) shell puts a default tray item on the
bar for each at once.

Payload `ResidentApps`, QoS `latest`, retained.

Helpers: `SESSION_APPS_RESIDENT_PATTERN`, `session_apps_resident_topic(session)`, `on_session_apps_resident(session, handler)`, `subscribe_session_apps_resident(session)`, `publish_session_apps_resident(session, payload)`.

```rhai
sys::init::on_session_apps_resident(session, |event| print(event.payload));
```

## See also

- [Messenger APIs for Rhai](index.md)
- [msg: Messenger from Rhai](../msg.md)
- [os.lazy.init.v1 on the wire](../../idl/os.lazy.init.v1.md)
