# `sys::net_socket`

The Rhai module for the Messenger interface `os.lazy.net.socket.v1`. Wire-level reference: [os.lazy.net.socket.v1](../../idl/os.lazy.net.socket.v1.md).

Sockets (docs/networking-plan.md N3), served by `netd` next to
`os.lazy.net.stack.v1`: TCP streams and UDP datagrams over IPv4.

**Ownership.** A socket belongs to the kernel-stamped sender of the `Open`
that made it (or of the `Accept` that returned it); every call on it by
anyone else fails with `EACCES` (and is audited). Nothing in a request
names a caller. The owner is the sender's task slot (and the pid the
scheduler's task list shows in it, which today is the slot number). Until
the kernel stamps tasks with a spawn counter, a task that lands in a dead
owner's slot before `netd` has reclaimed its sockets is taken for it. A socket whose owner exits is reclaimed by
`netd` within a fraction of a second, so a client need not `Close` before
it dies, but a well-behaved one does. One owner may hold at most 8 sockets
and everybody together 64; past either limit `Open` and `Accept` fail with
`EMFILE` and `ENFILE`. The interface is served on the stack service's
endpoint (`os.lazy.net.stack`); `netd` tells the interfaces apart by id.

**Blocking without threads.** `Connect`, `Accept`, `Recv`, `RecvFrom`,
`Send` (when the send buffer is full) and `Poll` are *parked*: `netd` keeps
the transaction and answers it when the socket is ready, or with
`ETIMEDOUT` when `timeout_ms` (10 to 60000; 0 means the longest wait, 60 s)
passes. A client that wants to wait longer calls again. A caller may have
at most 4 parked calls at once and everyone together 32 (`EAGAIN` past
either). Because the caller sleeps in the kernel with its own deadline,
`msg_cancel` and its death end the wait on its side; `netd` finds out when
it tries to reply. `Close` answers a call parked on that socket with
`EBADF`.

**Bytes travel in parcels.** A `Send` carries at most 16384 bytes and
`Recv` returns at most `max` (1 to 16384) bytes; each socket has a 16 KiB
buffer each way (TCP) or room for 8 datagrams of up to 1472 bytes (UDP).
`Recv` refuses `max = 0` with `EINVAL`, so an empty reply on a stream
socket always means the peer closed its side. A request too large for
`netd`'s receive buffer (about 20 KiB, so only a malformed one) is dropped
by the kernel and the caller's call fails at once with `EPIPE`.

**Ports.** `Bind` with port 0, or no `Bind` at all before `Connect`,
`SendTo` or `Listen`, takes an ephemeral port (49152 to 65535). Ports
below 1024 are refused with `EACCES` to every caller: `netd` runs without
the capability that would let it read a caller's `CAP_NET_BIND`, so until
the policy loader lands nobody binds a privileged port. A `Bind` to a port
another socket of the same kind holds fails with `EADDRINUSE`.

**Errors** come back as the shared structured error field
(`services::error_field`): `EBADF` for a socket id that is not open,
`EINVAL` for an argument or a state the call does not fit (a `Listen` on a
connected socket, a `Recv` on a listener), `ENETUNREACH` when there is no
address or no route, `ECONNREFUSED` when the peer reset a connection
attempt, `ECONNRESET` when it reset an open one, `ENOTCONN` for stream I/O
before `Connect`, `EPIPE` for a `Send` after the stream was shut for
writing, `EMSGSIZE` for a datagram that does not fit.

## Functions

### `open(kind)`

`Open(kind: U32) -> (sock: U32)`

Make a socket of `kind` (a `SockKind` ordinal). The reply is the id
every other call names.

Arguments:

| Name | IDL type | Script value |
|---|---|---|
| `kind` | `U32` | int |

Returns `sock`: `U32` (int).

```rhai
let result = sys::net_socket::open(kind);
```

### `bind(sock, addr)`

`Bind(sock: U32, addr: SockAddr) -> ()`

Choose the local address and port. `addr.addr` must be all zeros (any)
or the interface's own address; port 0 picks an ephemeral one.

Arguments:

| Name | IDL type | Script value |
|---|---|---|
| `sock` | `U32` | int |
| `addr` | `SockAddr` | map (`SockAddr`) |

Returns `()`.

```rhai
let result = sys::net_socket::bind(sock, addr);
```

### `connect(sock, addr, timeout_ms)`

`Connect(sock: U32, addr: SockAddr, timeout_ms: U32) -> ()`

Stream: open a connection to `addr`; parks until it is established or
refused. Datagram: fix the peer `Send`/`Recv` use (never parks).

Arguments:

| Name | IDL type | Script value |
|---|---|---|
| `sock` | `U32` | int |
| `addr` | `SockAddr` | map (`SockAddr`) |
| `timeout_ms` | `U32` | int |

Returns `()`.

```rhai
let result = sys::net_socket::connect(sock, addr, timeout_ms);
```

### `listen(sock, backlog)`

`Listen(sock: U32, backlog: U32) -> ()`

Stream only: accept connections on the bound port. `backlog` (1 to 8)
is how many connections may complete before `Accept` collects them.

Arguments:

| Name | IDL type | Script value |
|---|---|---|
| `sock` | `U32` | int |
| `backlog` | `U32` | int |

Returns `()`.

```rhai
let result = sys::net_socket::listen(sock, backlog);
```

### `accept(sock, timeout_ms)`

`Accept(sock: U32, timeout_ms: U32) -> (conn: U32, peer: SockAddr)`

Stream only: take one established connection off a listener as a new
socket owned by the caller; parks until one is there.

Arguments:

| Name | IDL type | Script value |
|---|---|---|
| `sock` | `U32` | int |
| `timeout_ms` | `U32` | int |

Returns a map:

| Name | IDL type | Script value |
|---|---|---|
| `conn` | `U32` | int |
| `peer` | `SockAddr` | map (`SockAddr`) |

```rhai
let result = sys::net_socket::accept(sock, timeout_ms);
```

### `send(sock, data, timeout_ms)`

`Send(sock: U32, data: Bytes, timeout_ms: U32) -> (sent: U32)`

Queue `data` (at most 16384 bytes) on a connected socket. Returns how
many bytes were taken (possibly fewer than sent); parks while no byte
would fit.

Arguments:

| Name | IDL type | Script value |
|---|---|---|
| `sock` | `U32` | int |
| `data` | `Bytes` | blob |
| `timeout_ms` | `U32` | int |

Returns `sent`: `U32` (int).

```rhai
let result = sys::net_socket::send(sock, data, timeout_ms);
```

### `recv(sock, max, timeout_ms)`

`Recv(sock: U32, max: U32, timeout_ms: U32) -> (data: Bytes)`

Take up to `max` (1 to 16384) bytes from a connected socket; parks
until at least one is there. An empty reply is the end of the stream.

Arguments:

| Name | IDL type | Script value |
|---|---|---|
| `sock` | `U32` | int |
| `max` | `U32` | int |
| `timeout_ms` | `U32` | int |

Returns `data`: `Bytes` (blob).

```rhai
let result = sys::net_socket::recv(sock, max, timeout_ms);
```

### `send_to(sock, addr, data)`

`SendTo(sock: U32, addr: SockAddr, data: Bytes) -> (sent: U32)`

Datagram: send `data` (at most 1472 bytes) to `addr`. Never parks: a
full send queue is `EAGAIN`.

Arguments:

| Name | IDL type | Script value |
|---|---|---|
| `sock` | `U32` | int |
| `addr` | `SockAddr` | map (`SockAddr`) |
| `data` | `Bytes` | blob |

Returns `sent`: `U32` (int).

```rhai
let result = sys::net_socket::send_to(sock, addr, data);
```

### `recv_from(sock, max, timeout_ms)`

`RecvFrom(sock: U32, max: U32, timeout_ms: U32) -> (data: Bytes, from: SockAddr)`

Datagram: take the next datagram, cut to `max` bytes (1 to 16384), and
where it came from; parks until one is there.

Arguments:

| Name | IDL type | Script value |
|---|---|---|
| `sock` | `U32` | int |
| `max` | `U32` | int |
| `timeout_ms` | `U32` | int |

Returns a map:

| Name | IDL type | Script value |
|---|---|---|
| `data` | `Bytes` | blob |
| `from` | `SockAddr` | map (`SockAddr`) |

```rhai
let result = sys::net_socket::recv_from(sock, max, timeout_ms);
```

### `poll(sock, interest, timeout_ms)`

`Poll(sock: U32, interest: U32, timeout_ms: U32) -> (ready: U32)`

Wait until any bit of `interest` (a bitmap of `Ready` ordinals) is
ready; the reply is the bitmap of what is. `Closed` and `Error` are
always of interest. Parks until then.

Arguments:

| Name | IDL type | Script value |
|---|---|---|
| `sock` | `U32` | int |
| `interest` | `U32` | int |
| `timeout_ms` | `U32` | int |

Returns `ready`: `U32` (int).

```rhai
let result = sys::net_socket::poll(sock, interest, timeout_ms);
```

### `shutdown(sock, how)`

`Shutdown(sock: U32, how: U32) -> ()`

Stream: end one or both directions (a `Shutdown` ordinal). `Write`
sends FIN after the queued bytes; the socket stays open for reading.

Arguments:

| Name | IDL type | Script value |
|---|---|---|
| `sock` | `U32` | int |
| `how` | `U32` | int |

Returns `()`.

```rhai
let result = sys::net_socket::shutdown(sock, how);
```

### `local_addr(sock)`

`LocalAddr(sock: U32) -> (addr: SockAddr)`

The socket's own address and port (`EINVAL` before `Bind`/`Connect`).

Arguments:

| Name | IDL type | Script value |
|---|---|---|
| `sock` | `U32` | int |

Returns `addr`: `SockAddr` (map (`SockAddr`)).

```rhai
let result = sys::net_socket::local_addr(sock);
```

### `peer_addr(sock)`

`PeerAddr(sock: U32) -> (addr: SockAddr)`

The peer's address and port (`ENOTCONN` when there is none).

Arguments:

| Name | IDL type | Script value |
|---|---|---|
| `sock` | `U32` | int |

Returns `addr`: `SockAddr` (map (`SockAddr`)).

```rhai
let result = sys::net_socket::peer_addr(sock);
```

### `close(sock)`

`Close(sock: U32) -> ()`

Release the socket. A stream closes gracefully (FIN); a parked call on
it is answered with `EBADF`.

Arguments:

| Name | IDL type | Script value |
|---|---|---|
| `sock` | `U32` | int |

Returns `()`.

```rhai
let result = sys::net_socket::close(sock);
```

### `stats()`

`Stats() -> (stats: SocketStats)`

Counters since `netd` started; anyone may read them.

Returns `stats`: `SocketStats` (map (`SocketStats`)).

```rhai
let result = sys::net_socket::stats();
```

## Types

### `SockAddr`

An IPv4 address and port. `addr` is four octets, network order; all
zero is "any" where that is allowed.

| Name | IDL type | Script value |
|---|---|---|
| `addr` | `Bytes` | blob |
| `port` | `U32` | int |

```rhai
let value = sys::net_socket::new_sock_addr();
```

### `SocketStats`

Counters since `netd` started.

| Name | IDL type | Script value |
|---|---|---|
| `open` | `U32` | int |
| `opened` | `U64` | int |
| `closed` | `U64` | int |
| `reclaimed` | `U64` | int |
| `connected` | `U64` | int |
| `accepted` | `U64` | int |
| `refused` | `U64` | int |
| `resets` | `U64` | int |
| `tx_bytes` | `U64` | int |
| `rx_bytes` | `U64` | int |
| `tx_datagrams` | `U64` | int |
| `rx_datagrams` | `U64` | int |
| `parked` | `U64` | int |
| `park_timeouts` | `U64` | int |
| `not_owner` | `U64` | int |

```rhai
let value = sys::net_socket::new_socket_stats();
```

### `SockKind`

A string; the constants of this module are:

- `SOCK_KIND_STREAM` = `"Stream"`
- `SOCK_KIND_DATAGRAM` = `"Datagram"`

`SOCK_KIND` is the array of all of them.

### `Ready`

A string; the constants of this module are:

- `READY_READABLE` = `"Readable"`
- `READY_WRITABLE` = `"Writable"`
- `READY_ACCEPTABLE` = `"Acceptable"`
- `READY_CLOSED` = `"Closed"`
- `READY_ERROR` = `"Error"`

`READY` is the array of all of them.

### `Shutdown`

A string; the constants of this module are:

- `SHUTDOWN_READ` = `"Read"`
- `SHUTDOWN_WRITE` = `"Write"`
- `SHUTDOWN_BOTH` = `"Both"`

`SHUTDOWN` is the array of all of them.

## See also

- [Messenger APIs for Rhai](index.md)
- [msg: Messenger from Rhai](../msg.md)
- [os.lazy.net.socket.v1 on the wire](../../idl/os.lazy.net.socket.v1.md)
