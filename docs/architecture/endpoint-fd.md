# Pollable endpoints: a Linux descriptor for a Messenger handle

Design note for issue #667. Code: `kernel/src/ipc/endpointfd.rs` (the
descriptor), `kernel/src/ipc/channels/pollstate.rs` (what it reads and the
rings), `kernel/src/task/fdtypes.rs` (`Fd::Endpoint`), the op in
`kernel/src/ipc/syscalls.rs`; the client is `lazyos_sys::msg::endpoint_fd`
and, under `std`, `lazyos_sys::msg::Pollable` (`AsRawFd`/`AsFd`). Tests:
`kernel/src/tests/endpoint_fd_suite/` and the `msgpoll` ABI fixture
(`tools/abi/fixtures/src/msgpoll.rs`).

## The problem

`wait_any` ([wait-any.md](wait-any.md)) parks on up to eight endpoints and
**one** Linux descriptor (`WAIT_FD`). The other direction did not exist:
`poll`, `select` and `epoll` could not watch an endpoint, so a `std` program
could not put Messenger into an ordinary event loop (`mio`, tokio, calloop,
a hand-written `epoll` loop), and an app with sockets *and* Messenger work
(LazyWeb, Mail, Net Tools, `netd` clients) had to pick one primitive and poll
the other.

## The decision: an fd for an endpoint

Two shapes were on the table: (1) a native op that returns a descriptor for
an endpoint handle, readable when a message is queued; (2) an
`eventfd`-style doorbell attached to an endpoint and signalled on
empty-to-non-empty. **(1) is implemented.** It needs no second object whose
counter can drift from the queue (an eventfd counts signals, not messages:
after a partial drain it would read zero while messages wait, or the client
would have to re-arm it by hand), it is level-triggered like a socket, so
`epoll` edge and level modes both fall out of the existing `epoll` code, and
it is exactly what `AsRawFd` and `mio::unix::SourceFd` expect.

Native Messenger op `ENDPOINT_FD` (21): `MsgArgs::handle` is a channel
handle in the caller's table, `flags` is 0 or `ENDPOINT_FD_CLOEXEC`; the
result's `value` is the new descriptor. Refusals: `ENOENT` (no such
handle), `EINVAL` (not a channel, or an unknown flag), `EACCES` (no `CALL`,
the right `recv` needs), `EMFILE` (table full).

## Readiness rules

Level-triggered, recomputed on every scan (`EndpointWatch::poll_gen`):

| Report | When |
|---|---|
| `POLLIN` | a message is queued on this side, or the peer closed (a `recv` would not block) |
| `POLLOUT` | the peer is open and its inbox has room (a `send` would not get `QueueFull`) |
| `POLLHUP` | the peer closed; with `POLLIN` while messages remain and after |
| `POLLHUP \| POLLERR` | the endpoint is gone: the handle the descriptor was made from is closed or released, this side was closed by a holder, or the channel no longer exists |

Edges: the freshness counter `epoll` uses for `EPOLLET` is the side's
arrival count (`Endpoint::arrivals`, bumped by every delivery) plus the peer
close, so each new message is a fresh edge even while the side stayed
readable, and a repeated wait with no news reports nothing.

Nothing is consumed: the program takes the message with the Messenger
`recv` (or `try_recv`) as before, so objects, quotas and the poll grace
keep their one code path. `read`, `write` and `lseek` on the descriptor
fail (`EINVAL`, `ESPIPE`); `fstat` shows an anonymous inode,
`/proc/self/fd` links read `anon_inode:[messenger]`.

## Wakeups

The descriptor has no wait queue of its own. Its keyed-wakeup key
(`task::pollwait`, P6.5) is the channel object id tagged with bit 62 (the
other keys are kernel heap addresses), so a `poll`/`select` scan that saw it
records it, and `epoll_wait` (which records no keys) is woken by everything
as before. The channel code rings `channels::ring(channel, side)` after
releasing the registry lock (queue-then-task lock order) wherever an
answer can change: a delivery (`send`, a `call`'s request, a kernel post, a
`Connected` notice), a receive (room appears for the other side), a close
(both sides) and a handle release that leaves the side open (the
descriptor made from that handle hangs up). A ring with no endpoint
descriptor alive anywhere is one atomic load, so programs that never ask
for one pay nothing. The same rings reach a `wait_any` parked on a
descriptor (`WAIT_FD`), so an endpoint descriptor works there too.

## Lifetime and security

The descriptor holds no handle and no reference into the channel registry:
it names `(owner slot, handle number, channel object id)` and checks on
every scan that the owner's table still maps that number to that object. So:

* **It never outlives the handle.** Closing or releasing the handle, or the
  owner's exit (teardown closes its handles), hangs the descriptor up; a new
  handle that happens to reuse the number names another object id and does
  not revive it. Closing the descriptor first leaves the handle untouched.
* **It never widens the handle.** It can only report readiness: no receive,
  send, transfer or close goes through it. Opening it needs `CALL`, the
  right that already lets the holder `recv` (and so learn the same facts).
* **Copies (`dup`, `fork`, `CLONE_FILES`, a descriptor passed over a
  socket) watch the same handle.** Messenger handle
  tables are per task slot (a thread has its own, see
  [ipc-core.md](ipc-core.md)), Linux descriptor tables follow Linux rules.
  A descriptor copied into a child or shared with a thread keeps watching
  the *owner's* handle and hangs up when it goes; the copy gives the other
  task readiness bits and nothing else. Open it `ENDPOINT_FD_CLOEXEC` (as
  `Pollable` does) so it does not survive an `execve`.

## Using it from `std`

```rust
use lazyos_sys::msg::{self, OwnedHandle, Pollable, EXPIRED_DEADLINE};
let endpoint = Pollable::new(OwnedHandle::from_raw(handle))?;
// register endpoint.as_raw_fd() with epoll / mio::unix::SourceFd beside a socket,
// and on readiness: msg::recv(endpoint.handle(), &mut buf, EXPIRED_DEADLINE)
```

A thread's handle table is its own, so a second thread that wants to send
to the endpoint resolves a registered name (or is handed a handle in a
parcel) rather than sharing the number; the `msgpoll` fixture does exactly
that while the main thread is blocked in `epoll_wait(-1)`.

## Tests

`kernel/src/tests/endpoint_fd_suite/`: the op through the native gate and
its refusals; the readiness table above; hang-up on close, on release while
another holder keeps the side, and on the channel vanishing; every
close order with and without an `epoll` interest; `EPOLLET` re-arming per
arrival beside a level interest; `read`/`write`/`lseek` refused; keyed
wakeups for delivery, receive, peer close and release; a kernel thread
blocked in `epoll_wait` woken by a send and by its own timeout. Soaks:
20 000 random sends, receives and descriptor reopenings over 16 endpoints in
one `epoll` set (every wait must report exactly the endpoints holding a
message), and 2 000 blocked-`epoll_wait` rounds; both end with no watch and
no endpoint registration left. The `msgpoll` ABI fixture runs the same
contract from a musl `std` program, beside a `UnixStream`; `netfix`'s
`msgpoll` check (`tools/abi/fixtures/src/netfix_msgpoll.rs`, judged by
`tools/net/run.py --netd`, which requires `NETFIX:msgpoll:PASS`) does it
beside a `TcpStream` to the host echo server: TCP alone, the endpoint
alone, both in one wait, and a blocked `epoll_wait(-1)` woken by TCP.
