# `os.lazy.devd.v1`

Interface id: `0xdfcb893f6178130f`

The device manager (issue #497, docs/driver-plan.md section 3.6): matches
the PCI functions the kernel enumerated against a static driver manifest
(`libs/devmatch`), asks `init` to start each matched driver
(`os.lazy.init.v1.StartDriver`), and reports every device's state.

`devd` has no device authority of its own: it reads the kernel's read-only
inventory, never claims, maps or touches a device, and names to `init` only
a driver row and the device it matched; `init` decides the program, its
credentials and its arguments. Failures are a structured error field
(errno-style code, friendly text), not a typed reply.

## Methods

| Method | Id | Kind | Signature |
|---|---|---|---|
| Devices | 665645856 | sync | `() -> (devices: Array<DeviceState>)` |

## Topics

| Topic | Payload | QoS | Retained | Permissions |
|---|---|---|---|---|
| `system/devices/+` | `DeviceState` | latest | yes | `publish:system/devices/+`, `subscribe:system/devices/+` |

## struct `DeviceState`

- `id: U64`
- `vendor: U32`
- `device: U32`
- `class: String`
- `driver: String`
- `model: String`
- `state: String`
- `owner: U32`
- `pid: U64`
