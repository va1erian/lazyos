# `sys::usb_bt`

The Rhai module for the Messenger interface `os.lazy.usb.bt.v1`. Wire-level reference: [os.lazy.usb.bt.v1](../../idl/os.lazy.usb.bt.v1.md).

A USB Bluetooth controller, as `usbd` moves its packets for `btd`
(docs/bluetooth-plan.md, phase B2).

`usbd` owns every xHCI controller, so only it can reach the radio of a USB
Bluetooth controller (HCI over USB, class `E0/01/01`: commands go out as
class requests on endpoint 0, events come in on the interrupt endpoint,
ACL data moves on the bulk endpoints). It forwards the packets and holds no
Bluetooth knowledge beyond what delimits a packet (an event's length byte,
an ACL header's length field): the host stack that parses what a stranger
in radio range sends is `btd`, a separate process that holds one
capability and no DMA. `usbd` serves this interface as `os.lazy.usb.bt`
while a controller is bound (one at a time) and withdraws it, closing the
endpoint, when the controller is unplugged.

**Packets travel in rings, never in a message body** (the NIC rule, see
`os.lazy.net.nic.v1`). `btd` creates one shared buffer holding both rings
and hands it over with `AttachRing`: the `Commands` ring (frames from
`btd`, `usbd` consumes) at byte 0 and the `Events` ring (frames from the
controller, `usbd` produces) after it, `framering::ring_bytes(slots)` bytes
each. A ring frame is one byte telling the packet kind (the H4 indicator:
1 command, 2 ACL data, 4 event) and the packet. `Commands` carries
commands and outgoing ACL data in the order `btd` queued them; `Events`
carries events and incoming ACL data in the order they arrived. SCO and
ISO data are not carried.

**Wake-up without polling.** Each direction has one one-way message and the
ring's shared `armed` flag: the consumer arms the ring before it sleeps
(after a last look at it) and the producer sends the doorbell only when it
was armed, so a burst costs one message and an idle `usbd` or `btd` sleeps
until there is work. `btd` sends `Kick` after queueing commands; `usbd`
sends `Notify` into the endpoint `btd` handed over after it produced
events.

**Both sides treat the other as hostile.** Every index and length read
from shared memory is read once and validated; a frame is copied before it
is looked at; a ring whose peer breaks the protocol is poisoned and the
attachment is dropped (`NotifyBit.Gone`). A command whose length field
does not match its frame, an ACL packet likewise, or a frame of any other
kind, is dropped and counted (`Stats.refused`), never sent to the device.
Events a full `Events` ring has no room for wait in a bounded queue in
`usbd` and are dropped past it (`Stats.dropped`).

**Only `_bt` (uid 916) and root may call it**: anyone else gets `EACCES`.
The interface carries authority over the machine's input devices, so it is
denied to developer tools. No controller bound (it was unplugged) is
`ENODEV`; a second attach while one is held is `EBUSY`; a buffer of the
wrong size or a slot count that is not a power of two from 16 to 1024 is
`EINVAL`. Failures are returned as the shared structured error field.

## Functions

### `describe()`

`Describe() -> (device: Device)`

Events and ACL packets received from the controller.
Commands and ACL packets sent to the controller.
Packets from the controller dropped because the `Events` ring and
the queue behind it were full, or no ring was attached.
Frames from `btd` refused (wrong kind, length not matching).
Packets from the controller longer than `usbd` carries.
Transfers the controller failed (a stalled endpoint, a command it
refused).
Rings poisoned by a peer that broke the protocol.
Describe the bound controller.

Returns `device`: `Device` (map (`Device`)).

```rhai
let result = sys::usb_bt::describe();
```

### `detach_ring(ring)`

`DetachRing(ring: U32) -> ()`

Release the rings; `usbd` stops reading and writing them.

Arguments:

| Name | IDL type | Script value |
|---|---|---|
| `ring` | `U32` | int |

Returns `()`.

```rhai
let result = sys::usb_bt::detach_ring(ring);
```

### `stats()`

`Stats() -> (stats: Stats)`

Packet and error counters.

Returns `stats`: `Stats` (map (`Stats`)).

```rhai
let result = sys::usb_bt::stats();
```

### `kick(ring)`

`Kick(ring: U32) -> () oneway`

Event, client to `usbd`: the client queued frames in the `Commands`
ring of `ring`. Ignored from anyone but the owner.

Arguments:

| Name | IDL type | Script value |
|---|---|---|
| `ring` | `U32` | int |

One-way: the call returns `()` once the message is queued.

```rhai
let result = sys::usb_bt::kick(ring);
```

### `notify(ring, events)`

`Notify(ring: U32, events: U32) -> () oneway`

Event, `usbd` to the client: something needs the client's attention.
`events` is a bitmap: bit *n* is set for the `NotifyBit` with ordinal
*n*. Reserved bits are zero; a client ignores them.

Arguments:

| Name | IDL type | Script value |
|---|---|---|
| `ring` | `U32` | int |
| `events` | `U32` | int |

One-way: the call returns `()` once the message is queued.

```rhai
let result = sys::usb_bt::notify(ring, events);
```

Not callable from a script (the request carries a kernel object): `AttachRing`.

## Types

### `Device`

What the bound controller is.

| Name | IDL type | Script value |
|---|---|---|
| `vendor` | `U32` | int |
| `product` | `U32` | int |
| `version` | `U32` | int |
| `name` | `String` | string |
| `port` | `String` | string |
| `high_speed` | `Bool` | bool |

```rhai
let value = sys::usb_bt::new_device();
```

### `Stats`

The device release number (bcdDevice).
The product string the device reports, empty when it has none.
The `usbd` port name (`<hc>-<port>[.<hub port>...]`).
The controller runs at high speed (microframes) or above.
Counters since the controller was bound.

| Name | IDL type | Script value |
|---|---|---|
| `events` | `U64` | int |
| `acl_in` | `U64` | int |
| `commands` | `U64` | int |
| `acl_out` | `U64` | int |
| `dropped` | `U64` | int |
| `refused` | `U64` | int |
| `oversize` | `U64` | int |
| `errors` | `U64` | int |
| `ring_errors` | `U64` | int |

```rhai
let value = sys::usb_bt::new_stats();
```

### `NotifyBit`

A string; the constants of this module are:

- `NOTIFY_BIT_PACKETS` = `"Packets"`
- `NOTIFY_BIT_SPACE` = `"Space"`
- `NOTIFY_BIT_GONE` = `"Gone"`

`NOTIFY_BIT` is the array of all of them.

## See also

- [Messenger APIs for Rhai](index.md)
- [msg: Messenger from Rhai](../msg.md)
- [os.lazy.usb.bt.v1 on the wire](../../idl/os.lazy.usb.bt.v1.md)
