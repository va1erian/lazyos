# `os.lazy.net.stack.v1`

Interface id: `0xb80ce5d5fc59627d`

The network stack service `netd` (docs/networking-plan.md N2): address
configuration, routes, statistics and ping, over the one interface the
stack drives (`os.lazy.net.nic.v1`). `netd` runs as the `_netd` user with no
capabilities, is the only client of the NIC driver, and parses every frame
the network sends it: nothing here is authority over a device.

**Nothing names a caller.** Who is asking is the kernel-stamped sender of the
call; `Renew` and other changes are gated by the ACL (`libs/netpolicy`), not
by a field in a request. Failures are returned as the shared structured error
field (`services::error_field`) instead of the declared reply fields.

**Blocking without threads.** `Ping` is a synchronous call whose reply is
*parked*: `netd` answers it when the echo reply arrives, or with `ETIMEDOUT`
when `timeout_ms` passes, and the caller sleeps in the kernel meanwhile with
its own deadline (`msg_cancel` and the caller's death clean up). One
caller may have up to 8 pings outstanding; past that the call fails with
`EAGAIN`. The stack's clock is the 100 Hz tick, so round trips read in
multiples of 10 ms.

## Methods

| Method | Id | Kind | Signature |
|---|---|---|---|
| Interfaces | 1779791769 | sync | `() -> (list: Array<InterfaceInfo>)` |
| Addresses | 912810883 | sync | `() -> (list: Array<AddressInfo>)` |
| Routes | 321835703 | sync | `() -> (list: Array<RouteInfo>)` |
| Stats | 267161228 | sync | `() -> (stats: StackStats)` |
| Ping | 2142761129 | sync | `(dst: Bytes, payload_len: U32, timeout_ms: U32) -> (result: EchoResult)` |
| Renew | 438534286 | sync | `() -> ()` |
| Reattach | 60999493 | sync | `() -> ()` |

## Topics

| Topic | Payload | QoS | Retained | Permissions |
|---|---|---|---|---|
| `system/net/+/addr` | `AddressEvent` | latest | yes | `publish:system/net/+/addr`, `subscribe:system/net/+/addr` |
| `system/events/network/up` | `AddressEvent` | latest | no | `publish:system/events/network/up`, `subscribe:system/events/network/up` |

## struct `InterfaceInfo`

- `name: String`
- `mac: Bytes`
- `mtu: U32`
- `link: Bool`
- `mode: U32`
- `dhcp: U32`

## struct `AddressInfo`

- `interface: String`
- `addr: Bytes`
- `prefix_len: U32`
- `source: U32`
- `lease_secs: U32`

## struct `RouteInfo`

- `interface: String`
- `dest: Bytes`
- `prefix_len: U32`
- `gateway: Bytes`

## struct `StackStats`

- `rx_frames: U64`
- `tx_frames: U64`
- `rx_bytes: U64`
- `tx_bytes: U64`
- `tx_dropped: U64`
- `rx_bad_length: U64`
- `nic_resets: U64`
- `leases: U64`
- `lease_losses: U64`
- `pings_sent: U64`
- `pings_answered: U64`
- `pings_timed_out: U64`

## struct `EchoResult`

- `rtt_ms: U32`
- `source: Bytes`
- `bytes: U32`

## struct `AddressEvent`

- `interface: String`
- `addr: Bytes`
- `prefix_len: U32`
- `gateway: Bytes`

## enum `ConfigMode`

- Dhcp, Static

## enum `AddrSource`

- Dhcp, Static

## enum `DhcpState`

- Off, Discovering, Bound
