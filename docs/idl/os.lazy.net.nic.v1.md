# `os.lazy.net.nic.v1`

Interface id: `0x6748c83c2024715b`

A network interface card, **link layer only** (docs/driver-plan.md §3.8,
docs/networking-plan.md §5). No IP, ARP or DHCP: those belong to the stack
service `netd`, which is just another client of this interface and, by
policy, the only one. A NIC driver (`netdrv`, virtio-net first) serves it and
never parses a payload.

**The client owns the rings.** Replies cannot carry buffers or channels (the
kernel refuses objects in a reply), and the driver must not let its device
read memory a client can rewrite, so the client creates two shared buffers
and the driver copies frames between them and its own DMA slots, in both
directions (the audio rule, see `os.lazy.audio.v1`). A ring is the
single-producer/single-consumer frame ring of `libs/framering`: fixed
2048-byte slots, a `u16` length then the frame, a power-of-two slot count,
free-running `u32` indices in a header page. Nothing about the wire lives in
a request body except the slot count.

`AttachRing` carries two objects (its `Ring<Rx, Tx>` and `Channel`
parameters; the ring declarations say the layout):

* `rings`: one shared buffer holding **both rings**, back to back: the
**receive ring** (driver produces, client consumes) at byte 0 and the
**transmit ring** (client produces, driver consumes) at byte
`framering::ring_bytes(slots)`.
* `notify`: the **notify endpoint**, an endpoint the client holds the
receiving side of, to which the driver posts `Notify`.

The buffer must be exactly `2 * framering::ring_bytes(slots)` bytes (two
header pages plus `2 * slots` slots) or the call fails with `EINVAL`;
`slots` must be a power of two from 16 to 1024 or it fails with `EINVAL`. Only one client may
be attached (`EBUSY` otherwise); its owner is the kernel-stamped sender of
`AttachRing`, and calls on the ring by anyone else fail with `EACCES`.

**Wake-up.** This interface replaces the draft's `notify: String` topic: a
topic would put a broker round trip on the receive path. Instead each
direction has one one-way message and one shared flag, so a waiter needs a
single wait on a single endpoint and a burst costs one message, not one per
frame (the rule the kernel uses for interrupt messages):

* driver to client, `Notify`: posted to the notify endpoint when frames
arrive in the receive ring, when the transmit ring gains space after
being full, or when the link changes;
* client to driver, `Kick`: sent to the driver's endpoint after the client
queues frames in the transmit ring.

A message is sent only when the consumer of that ring has *armed* it: the
consumer sets the ring's `armed` flag before it sleeps (after a final look at
the ring, so a frame that lands in between is never missed) and the producer
clears it with one atomic exchange when it sends. That coalesces a burst to
at most one outstanding message per ring, with no kernel help. A consumer
that never arms simply polls; it can only harm itself.

Both sides treat the other as hostile: every index and length read from
shared memory is validated and read exactly once, and a frame is copied out
before it is looked at. A ring whose peer breaks the protocol (an index
that claims more frames than the ring holds, an oversized length) is
poisoned: the driver counts it in `NicStats.ring_errors` and detaches the
client. A frame shorter than the 14-byte Ethernet header or longer than
`NicInfo.max_frame` is dropped and counted, never truncated.

**One name per card.** A driver serves one card and registers it as
`os.lazy.net.nic/<ifname>` (`os.lazy.net.nic/eth0`): `devd` picks the
interface name and `init` hands it to the driver as `ifname=<name>`. The
stack lists the registry for that prefix to find the cards, so cards may
appear and vanish while it runs. There is no bare `os.lazy.net.nic`.

Failures of calls are returned as the shared structured error field
(`services::error_field`) instead of the declared reply fields.

## Methods

| Method | Id | Kind | Signature |
|---|---|---|---|
| Info | 266462757 | sync | `() -> (info: NicInfo)` |
| SetRxMode | 506115710 | sync | `(mode: U32) -> (ok: Bool)` |
| AttachRing | 62355614 | sync | `(slots: U32, rings: Ring<Rx, Tx>, notify: Channel<os.lazy.net.nic.v1>) -> (ring: U32)` |
| DetachRing | 162562056 | sync | `(ring: U32) -> ()` |
| Stats | 267161228 | sync | `() -> (stats: NicStats)` |
| Kick | 754690623 | oneway | `(ring: U32) -> ()` |
| Notify | 314575196 | oneway | `(ring: U32, events: U32) -> ()` |

## Objects

Kernel objects a request carries, in the order of the parcel's
object list (the index each field must hold).

| Method | Field | Type | Object |
|---|---|---|---|
| AttachRing | `rings` | `Ring<Rx, Tx>` | `objects[0]`, a shared buffer holding the rings `Rx`, `Tx` back to back |
| AttachRing | `notify` | `Channel<os.lazy.net.nic.v1>` | `objects[1]`, a channel the receiver sends `os.lazy.net.nic.v1` on |

## Topics

| Topic | Payload | QoS | Retained | Permissions |
|---|---|---|---|---|
| `system/net/+/link` | `LinkEvent` | latest | yes | `publish:system/net/+/link`, `subscribe:system/net/+/link` |

## Rings

| Ring | Layout | Producer | Doorbell / advance | |
|---|---|---|---|---|
| `Rx` | frames | server | doorbell `Notify` | The receive ring: frames the card received, driver to client. |
| `Tx` | frames | client | doorbell `Kick` | The transmit ring: frames to send, client to driver. |

## struct `NicInfo`

- `mac: Bytes`
- `mtu: U32`
- `max_frame: U32`
- `link: Bool`
- `features: U32`
- `kind: U32`

## struct `NicStats`

- `rx_frames: U64`
- `tx_frames: U64`
- `rx_bytes: U64`
- `tx_bytes: U64`
- `rx_dropped: U64`
- `tx_dropped: U64`
- `runts: U64`
- `oversize: U64`
- `ring_errors: U64`
- `interrupts: U64`
- `link_changes: U32`

## struct `LinkEvent`

- `up: Bool`
- `changes: U32`

## enum `NotifyBit`

- RxReady, TxSpace, LinkChange

## enum `RxMode`

- Off, Filtered, Promiscuous

## enum `NicKind`

- Wired, Wireless
