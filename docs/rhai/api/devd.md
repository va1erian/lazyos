# `sys::devd`

The Rhai module for the Messenger interface `os.lazy.devd.v1`. Wire-level reference: [os.lazy.devd.v1](../../idl/os.lazy.devd.v1.md).

The device manager (issue #497, docs/driver-plan.md section 3.6): matches
the PCI functions the kernel enumerated against a static driver manifest
(`libs/devmatch`), asks `init` to start each matched driver
(`os.lazy.init.v1.StartDriver`), and reports every device's state.

`devd` has no device authority of its own: it reads the kernel's read-only
inventory, never claims, maps or touches a device, and names to `init` only
a driver row and the device it matched; `init` decides the program, its
credentials and its arguments. Failures are a structured error field
(errno-style code, friendly text), not a typed reply.

## Functions

### `devices()`

`Devices() -> (devices: Array<DeviceState>)`

Every device the kernel enumerated, with its match and state.

Returns `devices`: `Array<DeviceState>` (array of map (`DeviceState`)).

```rhai
let result = sys::devd::devices();
```

## Types

### `DeviceState`

One device as `devd` sees it.

| Name | IDL type | Script value |
|---|---|---|
| `id` | `U64` | int |
| `vendor` | `U32` | int |
| `device` | `U32` | int |
| `class` | `String` | string |
| `driver` | `String` | string |
| `model` | `String` | string |
| `state` | `String` | string |
| `owner` | `U32` | int |
| `pid` | `U64` | int |
| `ifname` | `String` | string |

```rhai
let value = sys::devd::new_device_state();
```

## Topics

### `system/devices/{id}`

The kernel's device id (the `dev=<id>` a driver is started with).
PCI vendor and device ids.
The device class name (`net`, `audio`, `storage`, ...).
The manifest's driver row for it, empty when none matched.
What the match was (`virtio-net`, `Intel 8254x`, ...), empty when
none matched.
`unmatched` (no manifest entry: a device the kernel drives, or one
nobody does), `starting` (asked of `init`, not claimed yet),
`claimed`, `released` (its driver let it go), `busy` (its driver
row already drives another device), `nodriver` (this image does
not ship the driver the manifest names) or `failed` (`init`
refused).
The uid holding the claim, or 4294967295 when nobody does.
The driver's task, 0 before `init` started it.
The interface name `devd` gave a network card (`eth0`, `eth1`, ...
in enumeration order), the name its driver serves as
`os.lazy.net.nic/<ifname>`; empty for every other device.
One retained topic per device, published whenever its state changes, so
a subscriber that starts late learns every device at once.

Payload `DeviceState`, QoS `latest`, retained.

Helpers: `DEVICES_PATTERN`, `devices_topic(id)`, `on_devices(id, handler)`, `subscribe_devices(id)`, `publish_devices(id, payload)`.

```rhai
sys::devd::on_devices(id, |event| print(event.payload));
```

## See also

- [Messenger APIs for Rhai](index.md)
- [msg: Messenger from Rhai](../msg.md)
- [os.lazy.devd.v1 on the wire](../../idl/os.lazy.devd.v1.md)
