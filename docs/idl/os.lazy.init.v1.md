# `os.lazy.init.v1`

Interface id: `0xa549dce4687b08e`

The userspace service supervisor (issues #93, #158): the supervision table,
the built-in app registry and the app-launch path.

`init` serves a `router` topic broker on the same endpoint; those topic
payloads are not part of this interface. Failures are returned as a
structured error field (errno-style code, friendly text), not as a typed
reply, so the error field is hand-written next to these stubs.

## Methods

| Method | Id | Kind | Signature |
|---|---|---|---|
| Services | 1672675413 | sync | `() -> (services: Array<ServiceStatus>)` |
| Launch | 936096390 | sync | `(app: String, args: String, session: U64) -> (app: String, pid: U64, session: U64)` |
| ListApps | 1009359625 | sync | `() -> (apps: Array<AppInfo>)` |
| Stop | 1266644741 | sync | `(app: String) -> (stopped: U64)` |
| Shutdown | 1911669355 | sync | `(mode: U32, reason: String, force: Bool) -> (accepted: Bool, phase: String)` |
| StartDriver | 1713728693 | sync | `(driver: String, device: U64) -> (started: Bool, pid: U64)` |
| Ready | 197800596 | oneway | `() -> ()` |

## Topics

| Topic | Payload | QoS | Retained | Permissions |
|---|---|---|---|---|
| `system/power/state` | `PowerState` | latest | yes | `publish:system/power/state`, `subscribe:system/power/state` |
| `system/events/service/+` | `ServiceEvent` | latest | yes | `publish:system/events/service/+`, `subscribe:system/events/service/+` |

## struct `PowerState`

- `phase: String`
- `mode: U32`
- `reason: String`
- `deadline: U64`

## struct `ServiceStatus`

- `name: String`
- `state: String`
- `pid: U64`
- `restarts: U64`
- `deps: String`
- `health: String`

## struct `AppInfo`

- `id: String`
- `name: String`
- `path: String`
- `restart: String`
- `verbs: Array<String>`
- `installed: Bool`
- `origin: String`
- `category: String`
- `hidden: Bool`
- `autostart: Bool`
- `icon: String`

## struct `ServiceEvent`

- `state: String`
- `pid: U64`
- `restarts: U64`
- `status: U64`
- `health: String`
- `detail: String`

## enum `PowerMode`

- PowerOff, Reboot
