# `os.lazy.bt.v1`

Interface id: `0x35c643664b9f4cae`

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

## Methods

| Method | Id | Kind | Signature |
|---|---|---|---|
| List | 1 | sync | `() -> (devices: Array<Device>, controller: Bool, discovering: Bool)` |
| Status | 2 | sync | `() -> (pairing: Pairing)` |
| StartDiscovery | 3 | sync | `(seconds: U32) -> ()` |
| StopDiscovery | 4 | sync | `() -> ()` |
| Pair | 5 | sync | `(address: String) -> ()` |
| ConfirmNumber | 6 | sync | `(address: String, accept: Bool) -> ()` |
| CancelPairing | 7 | sync | `() -> ()` |
| Forget | 8 | sync | `(address: String) -> ()` |
| Subscribe | 9 | sync | `(events: Channel<os.lazy.bt.v1>) -> ()` |
| DeviceEvent | 10 | oneway | `(kind: DeviceEventKind, device: Device) -> ()` |
| PairingEvent | 11 | oneway | `(pairing: Pairing) -> ()` |

## Objects

Kernel objects a request carries, in the order of the parcel's
object list (the index each field must hold).

| Method | Field | Type | Object |
|---|---|---|---|
| Subscribe | `events` | `Channel<os.lazy.bt.v1>` | `objects[0]`, a channel the receiver sends `os.lazy.bt.v1` on |

## struct `Device`

- `address: String`
- `name: String`
- `transport: Transport`
- `input: InputClass`
- `rssi: Option<I32>`
- `bonded: Bool`
- `connected: Bool`

## struct `Pairing`

- `phase: Phase`
- `address: String`
- `passkey: U32`
- `number: U32`
- `reason: FailReason`

## enum `Transport`

- BrEdr, Le

## enum `InputClass`

- Unknown, Keyboard, Pointer

## enum `Phase`

- Idle, Connecting, ShowPasskey, ConfirmNumber, Paired, Failed

## enum `FailReason`

- None, Timeout, Cancelled, Rejected, AuthFailed, Unsupported, LinkLost

## enum `DeviceEventKind`

- Found, Changed, Removed
