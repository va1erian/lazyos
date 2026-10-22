# `sys::net_nic`

The Rhai module for the Messenger interface `os.lazy.net.nic.v1`. Wire-level reference: [os.lazy.net.nic.v1](../../idl/os.lazy.net.nic.v1.md).

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

## Functions

### `info()`

`Info() -> (info: NicInfo)`

Describe the card: MAC, MTU, link state and the negotiated feature set.

Returns `info`: `NicInfo` (map (`NicInfo`)).

```rhai
let result = sys::net_nic::info();
```

### `set_rx_mode(mode)`

`SetRxMode(mode: U32) -> (ok: Bool)`

Choose which received frames reach the client: an `RxMode` ordinal.
The reply says whether the driver applied it (a device without a
filter table treats `Filtered` as `Promiscuous` and answers `false`).
Only the attached client may change it.

Arguments:

| Name | IDL type | Script value |
|---|---|---|
| `mode` | `U32` | int |

Returns `ok`: `Bool` (bool).

```rhai
let result = sys::net_nic::set_rx_mode(mode);
```

### `detach_ring(ring)`

`DetachRing(ring: U32) -> ()`

Release the rings; the driver stops reading and writing them. Also
implied when the owner exits or its notify endpoint reports the peer
gone.

Arguments:

| Name | IDL type | Script value |
|---|---|---|
| `ring` | `U32` | int |

Returns `()`.

```rhai
let result = sys::net_nic::detach_ring(ring);
```

### `stats()`

`Stats() -> (stats: NicStats)`

Frame and error counters since the driver started.

Returns `stats`: `NicStats` (map (`NicStats`)).

```rhai
let result = sys::net_nic::stats();
```

### `kick(ring)`

`Kick(ring: U32) -> () oneway`

Event, client to driver: the client queued frames in the transmit ring
of `ring`. Ignored from anyone but the owner.

Arguments:

| Name | IDL type | Script value |
|---|---|---|
| `ring` | `U32` | int |

One-way: the call returns `()` once the message is queued.

```rhai
let result = sys::net_nic::kick(ring);
```

### `notify(ring, events)`

`Notify(ring: U32, events: U32) -> () oneway`

Event, driver to client: something needs the client's attention.
`events` is a bitmap: bit *n* is set for the `NotifyBit` with ordinal
*n*. Reserved bits are zero; a client ignores them.

Arguments:

| Name | IDL type | Script value |
|---|---|---|
| `ring` | `U32` | int |
| `events` | `U32` | int |

One-way: the call returns `()` once the message is queued.

```rhai
let result = sys::net_nic::notify(ring, events);
```

Not callable from a script (the request carries a kernel object): `AttachRing`.

## Types

### `NicInfo`

Card description.

| Name | IDL type | Script value |
|---|---|---|
| `mac` | `Bytes` | blob |
| `mtu` | `U32` | int |
| `max_frame` | `U32` | int |
| `link` | `Bool` | bool |
| `features` | `U32` | int |
| `kind` | `U32` | int |

```rhai
let value = sys::net_nic::new_nic_info();
```

### `NicStats`

Six octets, network order.
Payload bytes of the largest frame the card carries.
Largest complete frame (`mtu` plus the 14-byte Ethernet header),
the bound the driver enforces in both directions.
Capability bitmap, zero if none: bit 0 receive checksum offload,
bit 1 transmit checksum offload, bit 2 VLAN tag insert/strip. Other
bits are reserved: a driver sets them to zero and a client ignores
them.
A `NicKind` ordinal: a cable (`Wired`) or a radio (`Wireless`). The
stack derives the default-route metric from it (wired 100,
wireless 600), so a cable wins when both are up.
Counters since the driver started.

| Name | IDL type | Script value |
|---|---|---|
| `rx_frames` | `U64` | int |
| `tx_frames` | `U64` | int |
| `rx_bytes` | `U64` | int |
| `tx_bytes` | `U64` | int |
| `rx_dropped` | `U64` | int |
| `tx_dropped` | `U64` | int |
| `runts` | `U64` | int |
| `oversize` | `U64` | int |
| `ring_errors` | `U64` | int |
| `interrupts` | `U64` | int |
| `link_changes` | `U32` | int |

```rhai
let value = sys::net_nic::new_nic_stats();
```

### `LinkEvent`

Frames dropped on receive for any reason (no ring, ring full, and
the invalid frames below).
Frames dropped on transmit for any reason (queue full, and the
invalid frames below).
Frames shorter than the 14-byte Ethernet header, either direction.
Frames longer than `max_frame`, either direction.
Rings poisoned by a peer that broke the protocol, plus device
used-ring entries the driver rejected.
Interrupt messages the driver handled (0 when polling).
The payload of `system/net/{ifname}/link`.

| Name | IDL type | Script value |
|---|---|---|
| `up` | `Bool` | bool |
| `changes` | `U32` | int |

```rhai
let value = sys::net_nic::new_link_event();
```

### `NotifyBit`

A string; the constants of this module are:

- `NOTIFY_BIT_RX_READY` = `"RxReady"`
- `NOTIFY_BIT_TX_SPACE` = `"TxSpace"`
- `NOTIFY_BIT_LINK_CHANGE` = `"LinkChange"`

`NOTIFY_BIT` is the array of all of them.

### `RxMode`

A string; the constants of this module are:

- `RX_MODE_OFF` = `"Off"`
- `RX_MODE_FILTERED` = `"Filtered"`
- `RX_MODE_PROMISCUOUS` = `"Promiscuous"`

`RX_MODE` is the array of all of them.

### `NicKind`

A string; the constants of this module are:

- `NIC_KIND_WIRED` = `"Wired"`
- `NIC_KIND_WIRELESS` = `"Wireless"`

`NIC_KIND` is the array of all of them.

## Topics

### `system/net/{ifname}/link`

Link changes since the driver started, so a subscriber can tell a
flap from a repeat.
Published by the driver whenever the link changes, and once at start.
`{ifname}` is the interface name `devd` gave the card (`eth0`), the
same name as `system/net/{ifname}/addr`: two cards of one model must
not share a retained topic.

Payload `LinkEvent`, QoS `latest`, retained.

Helpers: `LINK_PATTERN`, `link_topic(ifname)`, `on_link(ifname, handler)`, `subscribe_link(ifname)`, `publish_link(ifname, payload)`.

```rhai
sys::net_nic::on_link(ifname, |event| print(event.payload));
```

## See also

- [Messenger APIs for Rhai](index.md)
- [msg: Messenger from Rhai](../msg.md)
- [os.lazy.net.nic.v1 on the wire](../../idl/os.lazy.net.nic.v1.md)
