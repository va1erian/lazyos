# `sys::net_stack`

The Rhai module for the Messenger interface `os.lazy.net.stack.v1`. Wire-level reference: [os.lazy.net.stack.v1](../../idl/os.lazy.net.stack.v1.md).

The network stack service `netd` (docs/networking-plan.md N2): address
configuration, routes, statistics and ping, over the interfaces the stack
drives (one per `os.lazy.net.nic.v1` card, however many are present). `netd` runs as the `_netd` user with no
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
caller may have up to 4 pings outstanding and all callers together 8; past
either limit the call fails with `EAGAIN`. The stack's clock is the 100 Hz tick, so round trips read in
multiples of 10 ms. `Resolve` (N3) parks the same way: the answer comes
when the resolver replies, `ETIMEDOUT` when `timeout_ms` passes.

## Functions

### `interfaces()`

`Interfaces() -> (list: Array<InterfaceInfo>)`

The interfaces the stack drives, one per attached card.

Returns `list`: `Array<InterfaceInfo>` (array of map (`InterfaceInfo`)).

```rhai
let result = sys::net_stack::interfaces();
```

### `addresses()`

`Addresses() -> (list: Array<AddressInfo>)`

The addresses configured on every interface.

Returns `list`: `Array<AddressInfo>` (array of map (`AddressInfo`)).

```rhai
let result = sys::net_stack::addresses();
```

### `routes()`

`Routes() -> (list: Array<RouteInfo>)`

The routing table: every interface's on-link route and default route,
the default routes ordered by metric (the lowest wins).

Returns `list`: `Array<RouteInfo>` (array of map (`RouteInfo`)).

```rhai
let result = sys::net_stack::routes();
```

### `stats()`

`Stats() -> (stats: StackStats)`

Stack counters since it started.

Returns `stats`: `StackStats` (map (`StackStats`)).

```rhai
let result = sys::net_stack::stats();
```

### `ping(dst, payload_len, timeout_ms)`

`Ping(dst: Bytes, payload_len: U32, timeout_ms: U32) -> (result: EchoResult)`

Send one ICMP echo request to `dst` (four octets) with `payload_len`
bytes of payload (0 to 1400) and wait for its reply. `timeout_ms`
(10 to 60000) bounds the wait. Fails with `EINVAL` for a bad argument,
`ENETUNREACH` when there is no address or route yet, `EAGAIN` when this
caller has 4 pings outstanding or all callers together have 8, and `ETIMEDOUT` when no reply came.

Arguments:

| Name | IDL type | Script value |
|---|---|---|
| `dst` | `Bytes` | blob |
| `payload_len` | `U32` | int |
| `timeout_ms` | `U32` | int |

Returns `result`: `EchoResult` (map (`EchoResult`)).

```rhai
let result = sys::net_stack::ping(dst, payload_len, timeout_ms);
```

### `resolve(name, timeout_ms)`

`Resolve(name: String, timeout_ms: U32) -> (addrs: Array<Bytes>)`

Look `name` up (A records) at the resolvers DHCP or the static
configuration gave. `name` is a host name of at most 253 bytes made of
dot-separated labels of 1 to 63 letters, digits and hyphens (`EINVAL`
otherwise); `timeout_ms` is 10 to 60000. Fails with `ENETUNREACH` when
there is no address or no resolver, `EAGAIN` when this caller has 4
lookups outstanding or all callers together have 8, `ENOENT` when the
resolver says the name has no address, and `ETIMEDOUT` when no answer
came. The reply lists the addresses found, four octets each. A dotted
quad needs no query and is answered at once.

Arguments:

| Name | IDL type | Script value |
|---|---|---|
| `name` | `String` | string |
| `timeout_ms` | `U32` | int |

Returns `addrs`: `Array<Bytes>` (array of blob).

```rhai
let result = sys::net_stack::resolve(name, timeout_ms);
```

### `renew()`

`Renew() -> ()`

Ask for a fresh DHCP lease (drops the current one). Gated by the ACL.

Returns `()`.

```rhai
let result = sys::net_stack::renew();
```

### `reattach()`

`Reattach() -> ()`

Drop the attachment to the NIC driver and establish it again, as after
a driver restart: the stack keeps its lease and timers. A recovery and
test call; gated by the ACL.

Returns `()`.

```rhai
let result = sys::net_stack::reattach();
```

## Types

### `InterfaceInfo`

One network interface.

| Name | IDL type | Script value |
|---|---|---|
| `name` | `String` | string |
| `mac` | `Bytes` | blob |
| `mtu` | `U32` | int |
| `link` | `Bool` | bool |
| `mode` | `U32` | int |
| `dhcp` | `U32` | int |
| `kind` | `U32` | int |
| `metric` | `U32` | int |
| `primary` | `Bool` | bool |

```rhai
let value = sys::net_stack::new_interface_info();
```

### `InterfaceList`

The interface name (`eth0`).
Six octets, network order.
The driver reports the link up.
A `ConfigMode` ordinal: how the interface is configured.
A `DhcpState` ordinal (`Off` for a static interface).
A `NicKind` ordinal of `os.lazy.net.nic.v1`.
The default-route metric (wired 100, wireless 600; the lowest
usable one carries new traffic).
This interface holds the best default route right now: new
connections and the resolvers come from it.
Every interface at once, retained so a late subscriber sees the set.

| Name | IDL type | Script value |
|---|---|---|
| `list` | `Array<InterfaceInfo>` | array of map (`InterfaceInfo`) |

```rhai
let value = sys::net_stack::new_interface_list();
```

### `AddressInfo`

One configured IPv4 address.

| Name | IDL type | Script value |
|---|---|---|
| `interface` | `String` | string |
| `addr` | `Bytes` | blob |
| `prefix_len` | `U32` | int |
| `source` | `U32` | int |
| `lease_secs` | `U32` | int |

```rhai
let value = sys::net_stack::new_address_info();
```

### `RouteInfo`

Four octets, network order.
An `AddrSource` ordinal.
Seconds of lease left when asked (0 when static or unknown).
One IPv4 route.

| Name | IDL type | Script value |
|---|---|---|
| `interface` | `String` | string |
| `dest` | `Bytes` | blob |
| `prefix_len` | `U32` | int |
| `gateway` | `Bytes` | blob |
| `metric` | `U32` | int |

```rhai
let value = sys::net_stack::new_route_info();
```

### `StackStats`

Four octets; all zero with `prefix_len` 0 is the default route.
Four octets; all zero for an on-link route.
The route's metric: an on-link route is 0, a default route has its
interface's metric.
Stack counters since it started.

| Name | IDL type | Script value |
|---|---|---|
| `rx_frames` | `U64` | int |
| `tx_frames` | `U64` | int |
| `rx_bytes` | `U64` | int |
| `tx_bytes` | `U64` | int |
| `tx_dropped` | `U64` | int |
| `rx_bad_length` | `U64` | int |
| `nic_resets` | `U64` | int |
| `leases` | `U64` | int |
| `lease_losses` | `U64` | int |
| `pings_sent` | `U64` | int |
| `pings_answered` | `U64` | int |
| `pings_timed_out` | `U64` | int |
| `lookups_sent` | `U64` | int |
| `lookups_answered` | `U64` | int |
| `lookups_failed` | `U64` | int |

```rhai
let value = sys::net_stack::new_stack_stats();
```

### `EchoResult`

Frames read from and written to the NIC rings.
Frames the stack wanted to send but the transmit ring had no room for.
Frames the driver's ring delivered with an impossible length.
Times the NIC ring or the driver went away and was re-attached.
DHCP leases acquired and lost.
Pings started, answered, and timed out.
Name lookups started, answered with at least one address, and
failed or timed out.
The answer to one echo request.

| Name | IDL type | Script value |
|---|---|---|
| `rtt_ms` | `U32` | int |
| `source` | `Bytes` | blob |
| `bytes` | `U32` | int |

```rhai
let value = sys::net_stack::new_echo_result();
```

### `AddressEvent`

An interface's address state.

| Name | IDL type | Script value |
|---|---|---|
| `interface` | `String` | string |
| `addr` | `Bytes` | blob |
| `prefix_len` | `U32` | int |
| `gateway` | `Bytes` | blob |

```rhai
let value = sys::net_stack::new_address_event();
```

### `ConfigMode`

A string; the constants of this module are:

- `CONFIG_MODE_DHCP` = `"Dhcp"`
- `CONFIG_MODE_STATIC` = `"Static"`

`CONFIG_MODE` is the array of all of them.

### `AddrSource`

A string; the constants of this module are:

- `ADDR_SOURCE_DHCP` = `"Dhcp"`
- `ADDR_SOURCE_STATIC` = `"Static"`

`ADDR_SOURCE` is the array of all of them.

### `DhcpState`

A string; the constants of this module are:

- `DHCP_STATE_OFF` = `"Off"`
- `DHCP_STATE_DISCOVERING` = `"Discovering"`
- `DHCP_STATE_BOUND` = `"Bound"`

`DHCP_STATE` is the array of all of them.

## Topics

### `system/net/interfaces`

The interfaces the stack drives, republished whenever one appears,
vanishes, changes link or changes which one is primary.

Payload `InterfaceList`, QoS `latest`, retained.

Helpers: `INTERFACES_PATTERN`, `on_interfaces(handler)`, `subscribe_interfaces()`, `publish_interfaces(payload)`.

```rhai
sys::net_stack::on_interfaces(|event| print(event.payload));
```

### `system/net/{ifname}/addr`

Round trip in milliseconds (10 ms resolution).
Four octets: who answered.
Bytes of echo payload returned.
The address `netd` currently holds on an interface, retained so a late
subscriber sees it. `{ifname}` is the interface name.

Payload `AddressEvent`, QoS `latest`, retained.

Helpers: `ADDR_PATTERN`, `addr_topic(ifname)`, `on_addr(ifname, handler)`, `subscribe_addr(ifname)`, `publish_addr(ifname, payload)`.

```rhai
sys::net_stack::on_addr(ifname, |event| print(event.payload));
```

### `system/events/network/up`

Published once when the stack first has an address (and again after a
lease is lost and regained).

Payload `AddressEvent`, QoS `latest`.

Helpers: `NETWORK_UP_PATTERN`, `on_network_up(handler)`, `subscribe_network_up()`, `publish_network_up(payload)`.

```rhai
sys::net_stack::on_network_up(|event| print(event.payload));
```

## See also

- [Messenger APIs for Rhai](index.md)
- [msg: Messenger from Rhai](../msg.md)
- [os.lazy.net.stack.v1 on the wire](../../idl/os.lazy.net.stack.v1.md)
