# `os.lazy.net.nic.v1`

Interface id: `0x6748c83c2024715b`

A network interface card, **link layer only** (no IP/ARP/DHCP — that is a
future stack service and just another client of this interface). A NIC driver
serves it; clients own the frame rings and the driver never parses payloads.
See [`docs/driver-plan.md`](../driver-plan.md) §3.7.

## Methods

| Method | Id | Kind | Signature |
|---|---|---|---|
| Info | 266462757 | sync | `() -> (info: NicInfo)` |
| SetRxMode | 506115710 | sync | `(mode: RxMode) -> (ok: Bool)` |
| AttachRing | 62355614 | sync | `(rx: Buffer, tx: Buffer, notify: String) -> (ring: U32)` |
| DetachRing | 162562056 | sync | `(ring: U32) -> ()` |
| Stats | 267161228 | sync | `() -> (stats: NicStats)` |

`AttachRing` takes two single-producer/single-consumer frame rings in shared
buffers (fences included) and a topic the driver posts transmit-completion or
received-frame notices on. A driver may hold at most one attached ring per
client. Link changes are published on `system/net/<nic>/link`.

## struct `NicInfo`

- `mac: Bytes` — six octets, network order
- `mtu: U32`
- `link: Bool`
- `features: U32` — capability bitmap, zero if none: bit 0 receive checksum
  offload, bit 1 transmit checksum offload, bit 2 VLAN tag insert/strip. Other
  bits are reserved: a driver sets them to zero and a client ignores them

## struct `NicStats`

- `rx_frames: U64`
- `tx_frames: U64`
- `rx_dropped: U64`
- `tx_dropped: U64`
- `link_changes: U32`

## enum `RxMode`

- Off, Filtered, Promiscuous
