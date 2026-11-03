# `os.lazy.usb.bt.v1`

Interface id: `0x8e8962f04a3497f6`

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

## Methods

| Method | Id | Kind | Signature |
|---|---|---|---|
| Describe | 582948360 | sync | `() -> (device: Device)` |
| AttachRing | 62355614 | sync | `(slots: U32, rings: Ring<Commands, Events>, notify: Channel<os.lazy.usb.bt.v1>) -> (ring: U32)` |
| DetachRing | 162562056 | sync | `(ring: U32) -> ()` |
| Stats | 267161228 | sync | `() -> (stats: Stats)` |
| Kick | 754690623 | oneway | `(ring: U32) -> ()` |
| Notify | 314575196 | oneway | `(ring: U32, events: U32) -> ()` |

## Objects

Kernel objects a request carries, in the order of the parcel's
object list (the index each field must hold).

| Method | Field | Type | Object |
|---|---|---|---|
| AttachRing | `rings` | `Ring<Commands, Events>` | `objects[0]`, a shared buffer holding the rings `Commands`, `Events` back to back |
| AttachRing | `notify` | `Channel<os.lazy.usb.bt.v1>` | `objects[1]`, a channel the receiver sends `os.lazy.usb.bt.v1` on |

## Rings

| Ring | Layout | Producer | Doorbell / advance | |
|---|---|---|---|---|
| `Commands` | frames | client | doorbell `Kick` | Frames from `btd`: commands and outgoing ACL data. |
| `Events` | frames | server | doorbell `Notify` | Frames from the controller: events and incoming ACL data. |

## struct `Device`

- `vendor: U32`
- `product: U32`
- `version: U32`
- `name: String`
- `port: String`
- `high_speed: Bool`

## struct `Stats`

- `events: U64`
- `acl_in: U64`
- `commands: U64`
- `acl_out: U64`
- `dropped: U64`
- `refused: U64`
- `oversize: U64`
- `errors: U64`
- `ring_errors: U64`

## enum `NotifyBit`

- Packets, Space, Gone
