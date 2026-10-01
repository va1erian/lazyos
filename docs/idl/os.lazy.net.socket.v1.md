# `os.lazy.net.socket.v1`

Interface id: `0x5cbc5b5a07e2bb16`

Sockets (docs/networking-plan.md N3), served by `netd` next to
`os.lazy.net.stack.v1`: TCP streams and UDP datagrams over IPv4.

**Ownership.** A socket belongs to the kernel-stamped sender of the `Open`
that made it (or of the `Accept` that returned it); every call on it by
anyone else fails with `EACCES` (and is audited). Nothing in a request
names a caller. The owner is the sender's task slot *and* the pid the
scheduler's task list shows in it, so a task that reuses a dead owner's
slot owns none of its sockets. A socket whose owner exits is reclaimed by
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
without a reply and the caller's own deadline ends the wait.

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

## Methods

| Method | Id | Kind | Signature |
|---|---|---|---|
| Open | 1401622761 | sync | `(kind: U32) -> (sock: U32)` |
| Bind | 816668494 | sync | `(sock: U32, addr: SockAddr) -> ()` |
| Connect | 1535748249 | sync | `(sock: U32, addr: SockAddr, timeout_ms: U32) -> ()` |
| Listen | 1745080006 | sync | `(sock: U32, backlog: U32) -> ()` |
| Accept | 1353867593 | sync | `(sock: U32, timeout_ms: U32) -> (conn: U32, peer: SockAddr)` |
| Send | 1921914063 | sync | `(sock: U32, data: Bytes, timeout_ms: U32) -> (sent: U32)` |
| Recv | 1829805133 | sync | `(sock: U32, max: U32, timeout_ms: U32) -> (data: Bytes)` |
| SendTo | 1246690602 | sync | `(sock: U32, addr: SockAddr, data: Bytes) -> (sent: U32)` |
| RecvFrom | 81212541 | sync | `(sock: U32, max: U32, timeout_ms: U32) -> (data: Bytes, from: SockAddr)` |
| Poll | 1454776152 | sync | `(sock: U32, interest: U32, timeout_ms: U32) -> (ready: U32)` |
| Shutdown | 1911669355 | sync | `(sock: U32, how: U32) -> ()` |
| LocalAddr | 444792203 | sync | `(sock: U32) -> (addr: SockAddr)` |
| PeerAddr | 838739718 | sync | `(sock: U32) -> (addr: SockAddr)` |
| Close | 1300671683 | sync | `(sock: U32) -> ()` |
| Stats | 267161228 | sync | `() -> (stats: SocketStats)` |

## struct `SockAddr`

- `addr: Bytes`
- `port: U32`

## struct `SocketStats`

- `open: U32`
- `opened: U64`
- `closed: U64`
- `reclaimed: U64`
- `connected: U64`
- `accepted: U64`
- `refused: U64`
- `resets: U64`
- `tx_bytes: U64`
- `rx_bytes: U64`
- `tx_datagrams: U64`
- `rx_datagrams: U64`
- `parked: U64`
- `park_timeouts: U64`
- `not_owner: U64`

## enum `SockKind`

- Stream, Datagram

## enum `Ready`

- Readable, Writable, Acceptable, Closed, Error

## enum `Shutdown`

- Read, Write, Both
