# Networking: NIC interface, frame rings and virtio-net

**What it is.** The link-layer foundation of LazyOS networking, built in stages
from [`docs/networking-plan.md`](../networking-plan.md) (the plan holds the
rationale and the roadmap; this page describes what exists). The kernel knows
nothing about networking: a userspace NIC driver serves `os.lazy.net.nic.v1`
over Messenger, and a userspace stack service is its only client, exactly as
`sndd` and `os.lazy.audio.v1` split audio ([`audio.md`](audio.md)).

**Status: stage N0.** The interface, the frame ring and the virtio-net wire
definitions are built and host tested; the driver (N1) and `netd` (N2) are not.

**Key files**

| Path | Role |
|---|---|
| `idl/net.midl` | `os.lazy.net.nic.v1`, compiled by `midlc` into `libs/generated` and `docs/idl/os.lazy.net.nic.v1.md` |
| `libs/framering/` | The single-producer/single-consumer frame ring in shared memory. Pure `no_std`; `fuzz.rs` is the model-checked entry point |
| `libs/virtio-net/` | virtio-net feature bits, device config, the 12-byte packet header, the frame-length policy and receive-completion parsing, and the clamped driver settings. Pure `no_std` |
| `libs/virtio/` | The modern virtio-PCI transport (shared with `sndd`); queue cap raised to 256 in N0 |
| `libs/fuzzkit/` | A seeded PRNG and a driver that prints the failing seed; host only |
| `fuzz/` | The cargo-fuzz crate (outside the OS workspace) and its checked-in seed corpus |
| `tools/net/` | Tooling notes; the pcap analyzer and end-to-end harness arrive with N1 |
| `.github/workflows/net.yml` | Host tests, lints, corpus drift check and bounded fuzz runs |

## The interface

`os.lazy.net.nic.v1` is link layer only: `Info`, `SetRxMode`, `AttachRing`,
`DetachRing`, `Stats`, and two one-way messages, `Kick` and `Notify`. Its topic
`system/net/{nic}/link` is retained and carries a `LinkEvent`.

**The client owns the rings** (the audio rule): replies cannot carry buffers or
handles, and a device must never read memory a client can rewrite. So
`AttachRing` carries the client's two shared buffers and a notify endpoint in the
request's parcel vectors (`buffers[0]` receive, `buffers[1]` transmit,
`handles[0]` the endpoint) and only the slot count in the body. The driver copies
frames between those rings and its own DMA slots in both directions.

**Wake-up.** The draft used a topic name; a topic puts a broker round trip on the
receive path. Instead each direction has one one-way message and one shared flag:

```
driver -> client   Notify(ring, events)   posted when the client armed the rx ring
client -> driver   Kick(ring)             sent when the driver armed the tx ring
```

A consumer arms its ring (`Consumer::arm`), looks at the ring once more, and
only then sleeps; a producer calls `take_notify` after a burst, which clears the
flag with one atomic exchange, and sends a message only if it returns true. A
burst therefore costs one message and a frame that lands between "look" and
"sleep" is never missed. A consumer that never arms just polls.

## The frame ring (`libs/framering`)

```
0x000  header page: magic version slots slot_bytes | head @0x40 | tail @0x80 | armed @0xC0
0x1000 slot 0 .. slot N-1, 2048 bytes each: u16 length, then the frame
```

Slots are fixed size (16 to 1024 of them, a power of two), so a frame is at most
2046 bytes, a full-MTU frame (1514) fits with room, and validation is trivial;
the cost is memory (256 slots is 512 KiB per direction). Indices are free-running
`u32`s.

**The peer is hostile.** Each endpoint keeps its own index privately and only
*writes* it to the header. The peer's index is read once per call and checked
(`head - tail` may not exceed the slot count, in wrapping arithmetic); the slot
length is read once, and a frame is copied out of shared memory before anyone
parses it. What a peer can do:

| Peer misbehaviour | Result |
|---|---|
| Index claims more frames than the ring holds | the endpoint is **poisoned**: every later call returns `Corrupt`; the driver detaches the client and counts a ring error |
| Length above 2046 in a slot | that slot is consumed and reported (`BadLength`), never truncated; the next frame is unaffected |
| Rewrites a slot after the consumer copied it | nothing: the consumer parses its private copy |
| Scribbles on the flag | a spurious or missing wake-up, nothing else |
| Wrong header at attach (magic, version, slots, non-zero indices) | `attach` fails with `BadHeader` |

Nothing a peer writes can make this side read or write outside the region, panic
or loop. The producer refuses empty and over-long frames itself (`Empty`,
`TooLong`) and returns `Full` with nothing written.

## virtio-net wire definitions (`libs/virtio-net`)

* **Features.** `WANTED = MAC | STATUS`; nothing else is taken (no checksum or
  segmentation offload, no merged buffers, no control queue). A test pins that no
  layout-changing bit is ever accepted.
* **Device config.** Read by feature: MAC (6 bytes), STATUS (link), MQ, MTU. A
  short or absent window reads as zeros. A multicast or all-zero MAC is unusable
  and the driver will refuse to start on it.
* **Packet header.** 12 bytes (`VERSION_1`), little endian, `NetHdr::PLAIN`
  outgoing; on receive any offload flag or segmentation type is a device bug and
  the frame is dropped and counted.
* **Frame policy.** A frame shorter than 14 bytes (a runt) or longer than
  MTU + 14 (1514 at the default) is dropped and counted in either direction,
  never truncated or padded. `rx_frame` also rejects a device-reported length
  beyond the buffer instead of slicing with it.
* **Settings.** The `net/virtio-net/*` keys of `driver-config-plan.md` §2,
  clamped in pure code: ring entries round down to a power of two in 16..=256,
  `mtu` 576..=1500, `poll_interval_ms` 1..=1000, `irq_mode` `auto` or `poll`, a
  MAC override only if unicast and locally administered. A restart-class key
  restarts the driver only when its *effective* value changes, so garbage cannot
  start a crash loop.

## Testing

| Layer | What | Run |
|---|---|---|
| Host unit | `framering` (23): empty, full, wrap-around, exactly max frame and one over, the `u32` index wrap, coalesced wake-ups, every kind of hostile index and length, attach header validation, a two-thread order check. `virtio-net` (30): layouts, config by feature, length boundaries at 13/14/1514/1515, settings clamps with hostile values, MAC rules. `virtio` queue: a full 256-entry ring over five wraps and a 255-descriptor chain. Generated stubs (7) | `cargo test -p framering -p virtio-net -p virtio -p messenger-generated` |
| Seeded fuzz | Long random scripts against a reference model, with a hostile peer scribbling on indices, lengths and payloads between steps, guard pages either side of the ring, and random device configs, completions and settings; a failure prints its seed | `cargo test -p framering -p virtio-net fuzz::` |
| Coverage-guided fuzz | libFuzzer on the same entry points, with a checked-in seed corpus; CI runs each target for a bounded time | `cargo fuzz run framering --fuzz-dir fuzz fuzz/corpus/framering fuzz/seeds/framering` (Linux) |

See [`tools/net/README.md`](../../tools/net/README.md) for the design of the
fuzzing (the same `fuzz::run` serves the seeded tests and libFuzzer, and a saved
crash replays under plain `cargo test`).

## Not done

The driver (`netdrv`), `_net` uid and `init` row, `nicctl`, the pcap analyzer
and QEMU harness (N1); `libs/netstack`, `netd`, `netctl`, `ping` (N2); sockets,
`nc`, `ftp`, the Linux `AF_INET` shim (N3 to N5). The class ACL rules for
`os.kernel.dev.net` land with the driver.
