# `sys::bt`

The Rhai module for the Messenger interface `os.lazy.bt.v1`. Wire-level reference: [os.lazy.bt.v1](../../idl/os.lazy.bt.v1.md).

Bluetooth devices: list them, find new ones, pair and forget
(docs/bluetooth-plan.md, phase B5). `btd` serves it as `os.lazy.bt`; its
clients are the Settings pane and `btctl`.

**Who may call.** The kernel stamps the caller's uid, label and session
(a request body cannot name one). `List` is open to any user of the login
session; every other method (`Status`, `Subscribe`, `StartDiscovery`,
`StopDiscovery`, `Pair`, `ConfirmNumber`, `CancelPairing`, `Forget`) is
for the session user or `elevd` (`btpolicy::may_use_api`), because a
passkey is for the person at the machine and a pair or forget changes
which devices may type into every session. Anybody else, including root,
gets `EACCES`. The interface is in `devtool::DENY`: a developer tool's
script cannot call it.

**No silent pairing.** A device is paired only after `Pair` named it, in
a window of at most a minute. Outside the window the host is not
discoverable and admits bonded devices only. A keyboard must pair with an
authenticated key: the host shows a passkey (`Phase.ShowPasskey`) for the
user to type on the keyboard, or a number to compare
(`Phase.ConfirmNumber`, answered with `ConfirmNumber`). A mouse pairs
without either (Just Works). One pairing runs at a time (`EBUSY`).

**No method ever returns a key.** Link keys, long term keys and identity
resolving keys stay in `btd` and its 0600 bond file. `Device` carries what
a list needs: the address, the name, the kind and the state.

**Events.** `Subscribe` hands `btd` an event channel; `DeviceEvent` and
`PairingEvent` are sent on it. A client that is slow loses events (the
channel is bounded), so on any doubt it calls `List` and `Status`, which
are always the truth. A pairing event for a passkey is sent to the
subscribers only.

Failures of calls are returned as the shared structured error field:
`EACCES` (not allowed), `ENODEV` (no controller), `EBUSY` (a pairing is
running, or too many subscribers), `ENOENT` (no such device), `EINVAL`
(not an address, or not the device being paired).

## Functions

### `list()`

`List() -> (devices: Array<Device>, controller: Bool, discovering: Bool)`

A scan found it.
Its name, state or signal changed.
It was forgotten (or a scan result expired without a bond).
Known devices: the bonded ones and what the last scan found.

Returns a map:

| Name | IDL type | Script value |
|---|---|---|
| `devices` | `Array<Device>` | array of map (`Device`) |
| `controller` | `Bool` | bool |
| `discovering` | `Bool` | bool |

```rhai
let result = sys::bt::list();
```

### `status()`

`Status() -> (pairing: Pairing)`

The pairing in progress, or the outcome of the last one. For clients
that cannot receive events (`btctl`).

Returns `pairing`: `Pairing` (map (`Pairing`)).

```rhai
let result = sys::bt::status();
```

### `start_discovery(seconds)`

`StartDiscovery(seconds: U32) -> ()`

Scan for `seconds` (1 to 60): Inquiry on BR/EDR and a scan for LE
devices that advertise as a keyboard or mouse. Results arrive as
`DeviceEvent`s. A scan already running is extended. The host is not
discoverable by it.

Arguments:

| Name | IDL type | Script value |
|---|---|---|
| `seconds` | `U32` | int |

Returns `()`.

```rhai
let result = sys::bt::start_discovery(seconds);
```

### `stop_discovery()`

`StopDiscovery() -> ()`

Returns `()`.

```rhai
let result = sys::bt::stop_discovery();
```

### `pair(address)`

`Pair(address: String) -> ()`

Pair with a device a scan found: opens the pairing window (60
seconds) for that address and no other. `PairingEvent`s follow.
`EBUSY` while another pairing runs, `ENOENT` for an address no scan
has seen.

Arguments:

| Name | IDL type | Script value |
|---|---|---|
| `address` | `String` | string |

Returns `()`.

```rhai
let result = sys::bt::pair(address);
```

### `confirm_number(address, accept)`

`ConfirmNumber(address: String, accept: Bool) -> ()`

Answer `Phase.ConfirmNumber`: `accept` when the number matches the one
the device shows. `EINVAL` when `address` is not the device being
paired or no number waits.

Arguments:

| Name | IDL type | Script value |
|---|---|---|
| `address` | `String` | string |
| `accept` | `Bool` | bool |

Returns `()`.

```rhai
let result = sys::bt::confirm_number(address, accept);
```

### `cancel_pairing()`

`CancelPairing() -> ()`

Give up the pairing in progress and close the window. Not an error
when none runs.

Returns `()`.

```rhai
let result = sys::bt::cancel_pairing();
```

### `forget(address)`

`Forget(address: String) -> ()`

Remove the bond, drop the link and close the device's input source.
`ENOENT` for an unknown address.

Arguments:

| Name | IDL type | Script value |
|---|---|---|
| `address` | `String` | string |

Returns `()`.

```rhai
let result = sys::bt::forget(address);
```

### `device_event(kind, device)`

`DeviceEvent(kind: DeviceEventKind, device: Device) -> () oneway`

Event, `btd` to a subscriber.

Arguments:

| Name | IDL type | Script value |
|---|---|---|
| `kind` | `DeviceEventKind` | string (`DeviceEventKind` variant) |
| `device` | `Device` | map (`Device`) |

One-way: the call returns `()` once the message is queued.

```rhai
let result = sys::bt::device_event(kind, device);
```

### `pairing_event(pairing)`

`PairingEvent(pairing: Pairing) -> () oneway`

Event, `btd` to a subscriber: the pairing moved on.

Arguments:

| Name | IDL type | Script value |
|---|---|---|
| `pairing` | `Pairing` | map (`Pairing`) |

One-way: the call returns `()` once the message is queued.

```rhai
let result = sys::bt::pairing_event(pairing);
```

Not callable from a script (the request carries a kernel object): `Subscribe`.

## Types

### `Device`

One device: found by a scan, or bonded.

| Name | IDL type | Script value |
|---|---|---|
| `address` | `String` | string |
| `name` | `String` | string |
| `transport` | `Transport` | string (`Transport` variant) |
| `input` | `InputClass` | string (`InputClass` variant) |
| `rssi` | `Option<I32>` | int or () |
| `bonded` | `Bool` | bool |
| `connected` | `Bool` | bool |

```rhai
let value = sys::bt::new_device();
```

### `Pairing`

The device did not answer in time.
`CancelPairing` (or the window closed).
The device or the user said no, or the numbers differed.
The device failed to authenticate.
The device cannot be paired safely: for example a keyboard that
could not prove the passkey.
The link dropped during the pairing.
The pairing in progress, or the last one's outcome.

| Name | IDL type | Script value |
|---|---|---|
| `phase` | `Phase` | string (`Phase` variant) |
| `address` | `String` | string |
| `passkey` | `U32` | int |
| `number` | `U32` | int |
| `reason` | `FailReason` | string (`FailReason` variant) |

```rhai
let value = sys::bt::new_pairing();
```

### `Transport`

A string; the constants of this module are:

- `TRANSPORT_BR_EDR` = `"BrEdr"`
- `TRANSPORT_LE` = `"Le"`

`TRANSPORT` is the array of all of them.

### `InputClass`

A string; the constants of this module are:

- `INPUT_CLASS_UNKNOWN` = `"Unknown"`
- `INPUT_CLASS_KEYBOARD` = `"Keyboard"`
- `INPUT_CLASS_POINTER` = `"Pointer"`

`INPUT_CLASS` is the array of all of them.

### `Phase`

A string; the constants of this module are:

- `PHASE_IDLE` = `"Idle"`
- `PHASE_CONNECTING` = `"Connecting"`
- `PHASE_SHOW_PASSKEY` = `"ShowPasskey"`
- `PHASE_CONFIRM_NUMBER` = `"ConfirmNumber"`
- `PHASE_PAIRED` = `"Paired"`
- `PHASE_FAILED` = `"Failed"`

`PHASE` is the array of all of them.

### `FailReason`

A string; the constants of this module are:

- `FAIL_REASON_NONE` = `"None"`
- `FAIL_REASON_TIMEOUT` = `"Timeout"`
- `FAIL_REASON_CANCELLED` = `"Cancelled"`
- `FAIL_REASON_REJECTED` = `"Rejected"`
- `FAIL_REASON_AUTH_FAILED` = `"AuthFailed"`
- `FAIL_REASON_UNSUPPORTED` = `"Unsupported"`
- `FAIL_REASON_LINK_LOST` = `"LinkLost"`

`FAIL_REASON` is the array of all of them.

### `DeviceEventKind`

A string; the constants of this module are:

- `DEVICE_EVENT_KIND_FOUND` = `"Found"`
- `DEVICE_EVENT_KIND_CHANGED` = `"Changed"`
- `DEVICE_EVENT_KIND_REMOVED` = `"Removed"`

`DEVICE_EVENT_KIND` is the array of all of them.

## See also

- [Messenger APIs for Rhai](index.md)
- [msg: Messenger from Rhai](../msg.md)
- [os.lazy.bt.v1 on the wire](../../idl/os.lazy.bt.v1.md)
