# `os.lazy.net.nic.v1`

Interface id: `0x6748c83c2024715b`

A network interface card, **link layer only** (docs/driver-plan.md §3.8,
docs/networking-plan.md §5). No IP, ARP or DHCP: those belong to the stack
service `netd`, which is just another client of this interface and, by
policy, the only one. A NIC driver (`netdrv`, virtio-net first) serves it and
never parses a payload.

**The client owns the rings.** Replies cannot carry buffers or handles (the
kernel refuses transfers in a reply), and the driver must not let its device
read memory a client can rewrite, so the client creates two shared buffers
and the driver copies frames between them and its own DMA slots, in both
directions (the audio rule, see `os.lazy.audio.v1`). A ring is the
single-producer/single-consumer frame ring of `libs/framering`: fixed
2048-byte slots, a `u16` length then the frame, a power-of-two slot count,
free-running `u32` indices in a header page. Nothing about the wire lives in
a request body except the slot count.

`AttachRing` carries, in the parcel's `handles` and `buffers` vectors (the
TLV body has no `Handle`/`Buffer` fields, as in `os.lazy.display.v1`):

* `buffers[0]`: the **receive ring** (driver produces, client consumes);
* `buffers[1]`: the **transmit ring** (client produces, driver consumes);
* `handles[0]`: the **notify endpoint**, an endpoint the client holds the
receiving side of, to which the driver posts `Notify`.

Each buffer must be exactly `framering::ring_bytes(slots)` bytes (the header
page plus `slots` slots) or the call fails with `EINVAL`; `slots` must be a
power of two from 16 to 1024 or it fails with `EINVAL`. Only one client may
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

Failures of calls are returned as the shared structured error field
(`services::error_field`) instead of the declared reply fields.

## Methods

| Method | Id | Kind | Signature |
|---|---|---|---|
| Info | 266462757 | sync | `() -> (info: NicInfo)` |
| SetRxMode | 506115710 | sync | `(mode: U32) -> (ok: Bool)` |
| AttachRing | 62355614 | sync | `(slots: U32) -> (ring: U32)` |
| DetachRing | 162562056 | sync | `(ring: U32) -> ()` |
| Stats | 267161228 | sync | `() -> (stats: NicStats)` |
| Kick | 754690623 | oneway | `(ring: U32) -> ()` |
| Notify | 314575196 | oneway | `(ring: U32, events: U32) -> ()` |

## Topics

| Topic | Payload | QoS | Retained | Permissions |
|---|---|---|---|---|
| `system/net/+/link` | `LinkEvent` | latest | yes | `publish:system/net/+/link`, `subscribe:system/net/+/link` |

## struct `NicInfo`

- `mac: Bytes`
- `mtu: U32`
- `max_frame: U32`
- `link: Bool`
- `features: U32`

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
