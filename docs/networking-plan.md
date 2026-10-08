# LazyOS networking — exploration and plan

Status: **N0 and N1 built** (2026-09-30): the NIC interface is real MIDL,
`libs/framering`, `libs/virtio-net` and `libs/nicdrv` exist with host tests and
fuzzing, the queue cap is resolved, and `netdrv` brings up a virtio-net card in
QEMU with a packet-capture-judged harness. N2 and later are not built; see
§10.1 for the state of each stage and §13 for what the plan got wrong. What is built is described in
[architecture/networking.md](architecture/networking.md).
Scope: the shortest credible path from "no network" to basic tools working
(`ping`, a netcat equivalent, an FTP client, name lookups), and the three
decisions that path depends on: which TCP/IP stack to reuse, how the NIC driver
is built, and what the userland API looks like. This is the LAN half of
platform stage **S6** ([platform-plan.md](platform-plan.md) §4.7); it starts
where the driver plan stops ([driver-plan.md](driver-plan.md) §8 excludes "any
protocol above the NIC link layer").

Related: [driver-plan.md](driver-plan.md) (D5, the NIC driver),
[driver-config-plan.md](driver-config-plan.md) (`net/<drv>/*` keys),
[architecture/devices.md](architecture/devices.md),
[architecture/audio.md](architecture/audio.md) (the reference userspace driver),
[messenger.md](messenger.md), [security-model.md](security-model.md) §4.2 and §5,
[linux-abi-plan.md](linux-abi-plan.md), [rust-std.md](rust-std.md).

## 1. Summary of recommendations

| Question | Recommendation |
|---|---|
| Stack | Reuse **smoltcp** (0.14, 0BSD, `no_std`). Do not write one; do not port Netstack3 or lwIP |
| Where the stack runs | A userspace service, **`netd`**, separate from the NIC driver. The driver holds DMA authority and parses nothing; `netd` parses hostile packets and holds no device authority |
| Driver | **`virtio-net`** userspace driver on the existing `libs/virtio` transport, shaped like `sndd`; serves `os.lazy.net.nic.v1` (link layer only). This is the open half of driver stage D5 (issue #241) |
| Native API | Messenger interfaces in `idl/net.midl`: `os.lazy.net.socket.v1` (sockets, blocking through deferred replies), `os.lazy.net.stack.v1` (admin), plus the NIC interface |
| Linux ABI | Later stage: `AF_INET` in the kernel shim, fronting `netd`. This makes BusyBox `nc`/`ping`/`ftpget`/`wget` and Rust `std::net` work unmodified, but it is the one piece that needs new kernel code |
| First tools | Native `netctl`, `ping`, `nc`, `nslookup`, `ftp` against the Messenger API, so the goal is reachable **with no kernel change** |
| Verification | A `tools/net/run.py` harness modelled on `tools/sound/run.py`: the verdict is the packet capture and what the host peer received, never serial markers alone |

## 2. Where we are

| Piece | State today | Evidence |
|---|---|---|
| Network stack, `AF_INET` | none; `socket()` returns `EAFNOSUPPORT` for anything but `AF_UNIX` | `kernel/src/process/linux/socket.rs` |
| Device core, `dev_*` syscall 23, INTx to userspace, DMA pool | done (D1–D4) | [architecture/devices.md](architecture/devices.md) |
| Modern virtio-PCI transport, split virtqueues | done, host tested, used by `sndd` | `libs/virtio` |
| A real virtio-net interrupt reaching a userspace claimant | proven on `pc` and `q35` (test only, legacy device, raw `pio`) | `dev_irq_real_device_end_to_end`, `tools/test/run.py --nic` |
| NIC driver, `devd`, `_net` uid, driver manifest | done (`devd` and the e1000 with issue #497) | issues #241, #497 |
| `os.lazy.net.nic.v1` | **prose only**: `docs/idl/os.lazy.net.nic.v1.md` exists from D0, but there is no `idl/*.midl` for it and no entry in `idl/manifest.json` | `idl/` |
| Driver config keys | specified (`net/<drv>/mtu`, `rx_ring_entries`, `mac_override`, ...) | [driver-config-plan.md](driver-config-plan.md) §2 |
| Linux fd layer | `Fd` enum with pipes, `AF_UNIX` stream/seqpacket pairs, listeners, `poll`, `epoll` with edge generations | `kernel/src/task/fdtypes.rs`, `kernel/src/ipc/epoll.rs` |
| BusyBox | `defconfig` static build already runs on the shim; its network applets are compiled in and simply fail at `socket()` | `tools/abi/busybox.py` |
| Messenger features the design leans on | deferred replies with real deadlines, `begin_call`/`await_reply`, shared buffers, 1 MiB parcels, kernel-stamped credentials, `PeerDied` | [messenger.md](messenger.md) §2, §6, §7 |
| Known Messenger gaps that matter here | replies cannot carry handles or buffers; per-connection channels exist (`Connect`, #483) but `netd` does not use them yet | [architecture/ipc-core.md](architecture/ipc-core.md) |
| Time | 100 Hz PIT, 10 ms resolution everywhere | `kernel/src/process/linux/time.rs` |
| Entropy | kernel ChaCha20 pool behind Linux `getrandom`; no native wrapper found in `user/src/sys.rs` | `kernel/src/entropy.rs` |

## 3. Reusable Rust network stacks

### 3.1 Survey

| Candidate | What it is | Fit for LazyOS | Verdict |
|---|---|---|---|
| **[smoltcp](https://github.com/smoltcp-rs/smoltcp)** 0.14.0 (Aug 2026) | Standalone event-driven TCP/IP stack, `no_std`, heap optional, 0BSD, stable Rust 1.91+, about 43k lines. Ethernet, ARP, IPv4 (fragmentation and reassembly), IPv6, ICMP, UDP, TCP (window scaling, out-of-order reassembly, keep-alive, Nagle, delayed ACK, Reno/CUBIC), and DHCPv4, DNS, raw and ICMP sockets | Builds for `x86_64-unknown-none` with `alloc`, no floats, one explicit `poll` loop that maps directly onto a Messenger service loop. 0BSD is compatible with our GPL-3.0-or-later | **Use this** |
| [embassy-net](https://crates.io/crates/embassy-net) 0.9 | Async wrapper around smoltcp for the Embassy executor | Adds an executor dependency we do not have; the value is all smoltcp's | No; take smoltcp directly |
| Netstack3 (Fuchsia) | Production, POSIX-oriented Rust stack, about 135k lines, `no_std` core | Not published as a crate; must be vendored out of the Fuchsia tree with its build glue, and the core supplies no sockets/bindings layer. Asterinas evaluated the switch and closed it as not planned ([asterinas#1821](https://github.com/asterinas/asterinas/issues/1821)) | No; revisit only if smoltcp's TCP proves limiting |
| lwIP (C) through bindings | Mature embedded C stack | Needs a C toolchain in the OS workspace and an `unsafe` FFI boundary around the most exposed parser in the system; contradicts the code standards | No |
| Write our own | | TCP alone is months of interop debugging; nothing about LazyOS needs a bespoke stack | No |
| [virtio-drivers](https://crates.io/crates/virtio-drivers) 0.13 (`VirtIONet`) | rCore's virtio guest drivers | We already have `libs/virtio`, built for the `dev_*` handle model and host tested; a second transport would be duplication | No; add a small `libs/virtio-net` next to `libs/virtio-snd` |

Precedent: smoltcp is the stack behind Redox (`smolnetd`), Hermit, Asterinas,
ArceOS and Theseus, so "hobby or research OS in Rust + smoltcp" is the
well-trodden path, including the userspace-daemon shape we want (Redox).

### 3.2 What smoltcp does not give us

These are the parts `netd` must own; none is a blocker for the goal.

- **No BSD socket layer.** No blocking calls, no fd, no `accept` backlog: a
  listening smoltcp TCP socket turns into the one connection it accepts.
  `netd` emulates a backlog by keeping a small pool of listening sockets per
  bound port and re-arming on accept.
- **No ephemeral port allocation, no loopback routing, one device per
  `Interface`.** `netd` allocates ports, and serves `127.0.0.1` with a second
  `Interface` over smoltcp's `phy::Loopback` (or short-circuits in the socket
  layer).
- **TCP without SACK and timestamps**, IPv4 options ignored. Fine on a LAN and
  through QEMU user networking; lossy WAN throughput will be modest.
- **Caller supplies time and randomness.** Time comes from the 100 Hz tick
  (10 ms granularity: acceptable for TCP timers, poor for `ping` RTT, see §10).
  Randomness (initial sequence numbers, ports, DHCP/DNS ids) needs a native
  entropy call.
- **Compile-time limits** (address count, route count, reassembly buffer,
  neighbor cache) are set by cargo features / env; pick them once in
  `libs/netstack`.

Proposed dependency line (exact feature names to be confirmed when pinning):

```toml
smoltcp = { version = "0.14", default-features = false, features = [
    "alloc", "medium-ethernet", "proto-ipv4",
    "socket-tcp", "socket-udp", "socket-icmp", "socket-raw",
    "socket-dhcpv4", "socket-dns",
] }
```

IPv6 (`proto-ipv6`) is a feature flag away and deliberately off for the first cut.

### 3.3 Later, above the stack

| Need | Option |
|---|---|
| DHCP, DNS | smoltcp's own `dhcpv4` and `dns` sockets |
| TLS (S6 stage 3, keys in `keyd`) | [embedded-tls](https://crates.io/crates/embedded-tls) (TLS 1.3 client, `no_std`, no allocator) or rustls (`no_std` + `alloc` with a custom crypto provider over `libs/crypto`) |
| FTP for `std` programs | [suppaftp](https://crates.io/crates/suppaftp) once `std::net` works through the Linux shim; the native `ftp` tool is small enough to hand-write (§8) |

## 4. Architecture

```
  ping  nc  ftp  nslookup  netctl          BusyBox nc/wget/ftpget, std::net
   │ native client lib (generated)               │ Linux syscalls (N5)
   │                                     ┌───────┴────────┐
   │                                     │ kernel AF_INET │ thin fd shim,
   │                                     │ socket shim    │ no protocol code
   │                                     └───────┬────────┘
   └──────────────┬──── Messenger ───────────────┘
                  ▼   os.lazy.net.socket.v1 / os.lazy.net.stack.v1
        ┌───────────────────┐  uid _netd, no CAP_DEV_CLAIM, no DMA
        │ netd              │  smoltcp + socket table + DHCP/DNS + policy
        └─────────┬─────────┘
                  │ os.lazy.net.nic.v1: control calls + two shared frame rings
        ┌─────────▼─────────┐  uid _net, CAP_DEV_CLAIM only
        │ virtio-net driver │  never parses a payload
        └─────────┬─────────┘
   ═══════ syscall 23 dev_* : claim · map_bar · irq · dma_alloc ═══════
                  kernel device core (unchanged)
```

### 4.1 Where should the stack live?

| Option | For | Against | Verdict |
|---|---|---|---|
| A. smoltcp and the NIC driver in the kernel | Fewest hops; Linux `AF_INET` is a direct call | Breaks driver-plan D1/D2 (drivers in userspace, kernel knows no device class); a hostile-packet parser in ring 0; kernel code needs the full correctness + soak suite | Rejected |
| B. Stack inside the driver process | One copy fewer per frame, one task fewer | The packet parser would hold `DMA` rights, which are kernel-equivalent until an IOMMU exists (driver-plan D5); one stack per NIC; e1000 would duplicate it | Rejected |
| **C. Separate `netd` over `os.lazy.net.nic.v1`** | Matches the target architecture box (`netd (socket API, DNS, TCP)`), the driver plan ("a future stack service is just another client") and the threat model ("network daemon sandboxed, minimal parser surface"). A `netd` crash is a supervised restart that never touches the device | One extra frame copy and one wake hop per batch | **Recommended** |

The platform plan's "in-kernel socket core" survives as the thin `AF_INET` fd
shim of §7.2: descriptors, readiness and policy checks in the kernel, protocols
in `netd`.

## 5. The NIC driver (`virtio-net`)

This completes driver stage D5. `sndd` is the template
([architecture/audio.md](architecture/audio.md)); the structure carries over
almost file for file.

| Path (proposed) | Role |
|---|---|
| `idl/net.midl` | `os.lazy.net.nic.v1` as real MIDL (today it is prose only), generated into `libs/generated` |
| `libs/virtio-net/` | Wire definitions, pure `no_std` with host tests: feature bits, `virtio_net_config` (MAC, status), the 12-byte `virtio_net_hdr` |
| `libs/framering/` | The shared SPSC frame ring used between driver and stack, host tested including a hostile peer |
| `user/src/bin/netdrv.rs`, `netdrv/` | The driver: `device.rs` (claim, BARs), `queues.rs` (rx/tx virtqueues, DMA slots), `rings.rs` (client rings), `service.rs` (dispatch) |
| `user/src/bin/nicctl.rs` | Prints MAC, link, stats (the D5 demo) |

**Bring-up sequence**

1. `dev::list`, claim the function. QEMU's default `virtio-net-pci` is
   *transitional* (`1af4:1000`, legacy and modern both present); with
   `disable-legacy=on` it is `1af4:1041`. Match both and always drive the
   modern interface through the capability list.
2. `Transport::negotiate`: require `VERSION_1`; want `MAC` (bit 5) and `STATUS`
   (bit 16). Take nothing else at first: no `MRG_RXBUF`, no checksum or
   segmentation offload, no control queue. `NicInfo.features` stays 0.
3. One `dma_alloc` for both virtqueues and all packet slots, held for the
   driver's lifetime. **Never free DMA while the device runs** (the audio
   lesson: the kernel treats that as a device stop).
4. Queue 0 is receive, queue 1 transmit. Pre-post every receive slot
   (12-byte header + 1514-byte frame, one descriptor each). `DRIVER_OK`.
5. Claim with an interrupt endpoint, `irq_enable`, fall back to polling on
   `ENOSYS` (`net/virtio-net/irq_mode`, `poll_interval_ms`). Line 11 is shared
   with virtio-blk on both QEMU machines, so claim with `FLAG_SHARED_IRQ`.
6. Serve `Info`, `SetRxMode`, `AttachRing`, `DetachRing`, `Stats`; publish
   `system/net/<nic>/link`.

**Data path.** The driver copies between its own DMA slots and the client's
shared rings, in both directions, and trusts neither side:

- Device → driver: used-ring ids and lengths are bounds-checked by
  `libs/virtio`; a length above the slot size is a dropped frame and a stat.
- Client → driver: ring indices are reduced modulo the capacity. A frame
  shorter than the 14-byte Ethernet header or longer than the complete-frame
  bound (MTU + 14, so 1514 bytes at the default 1500-byte MTU) is dropped and
  counted, never truncated; a valid frame is copied into DMA at its full
  length, read once. The device never reads memory a client can rewrite
  (driver-plan §3.4).
- Driver → client: `netd` must likewise copy a frame out of the shared ring
  before parsing it, because the producer could rewrite it mid-parse.

**Frame ring (proposal for `libs/framering`).** Fixed 2048-byte slots (a `u16`
length then the frame), power-of-two slot count, producer and consumer indices
in a header page. Fixed slots cost memory (256 slots = 512 KiB per direction)
but make validation trivial and rule out the wrap-around bugs of a variable
length byte ring. Revisit with a byte ring only if memory matters.

**Points to settle before coding (the IDL is still a draft):**

- *Wake-up. **Decided (N0).*** The draft's `notify: String` topic is gone. The
  client passes a notify endpoint in `AttachRing` (`handles[0]`, with both
  rings in one shared buffer, `buffers[0]`: receive ring at byte 0, transmit ring
  after it, and the slot count in the body). Wake-ups are two one-way messages, `Notify` (driver to client) and
  `Kick` (client to driver), coalesced by a shared `armed` flag in each ring's
  header instead of by the kernel: a consumer arms the ring, looks once more,
  then sleeps; a producer clears the flag with one atomic exchange when it sends.
  `netd` therefore waits on a single endpoint for client calls, NIC notices and
  timeouts. No kernel fence is needed (the kernel has none, issue #677). See `idl/net.midl` and `libs/framering`.
- *Queue size. **Decided (N0).*** `libs/virtio` now caps a queue at
  `MAX_QUEUE = 256` (it was 64), which is the config plan's default
  `rx_ring_entries` and what QEMU's virtio-net offers; the in-struct free list
  costs 768 bytes at the cap. The `rx_ring_entries`/`tx_ring_entries` keys clamp
  to a power of two in 16..=256 rather than the plan's 16..=4096, and `mtu` to
  576..=1500 rather than 9000 (a 2048-byte slot cannot carry a jumbo frame).
  A device that offers a smaller queue is driven at its own size. 256 slots of
  2 KiB is 512 KiB per queue, two queues about 1 MiB of DMA, well inside the
  8 MiB `DmaMemory` quota.
- *`devd`.* D5 bundles `devd` and a driver manifest. Neither is needed to get
  packets flowing: start the driver from an `init` manifest row with a `_net`
  credential exactly as `sndd` is (`SND_CRED`), behind a `LAZYOS_NET=1` image
  flag, and land `devd` separately.
- *One client.* The ring has one consumer, so the driver accepts one attached
  client (`EBUSY` otherwise), owner = the kernel-stamped sender, released on
  `DetachRing` or when the owner's notify endpoint reports the peer gone.

**Second driver.** e1000 (`-device e1000`) stays the D7 genericity proof; it
serves the same interface, so `netd` does not change.

## 6. `netd`, the stack service

| Path (proposed) | Role |
|---|---|
| `libs/netstack/` | Pure `no_std` + `alloc`, host tested: the smoltcp `Device` over a frame-ring pair, the socket table, backlog emulation, port allocation, address and option validation |
| `user/src/bin/netd.rs`, `netd/` | Service glue: NIC attach, event loop, request dispatch, parked transactions, config, topics |

**Event loop.** One wait: `recv(deadline)` on `netd`'s endpoint, with the
deadline from smoltcp's `poll_delay`, rounded up to a tick. On wake: drain the
receive ring into private buffers, `iface.poll`, move data between smoltcp
sockets and parked client transactions, reply to whatever became ready, flush
the transmit ring and notify the driver once.

**Blocking calls without threads.** A `Recv`, `Accept` or `Connect` that
cannot finish is *parked*: `netd` keeps the transaction id and replies later,
exactly as the topics broker parks `next_event`. The caller sleeps in the
kernel with a real deadline; `msg_cancel` and `PeerDied` clean up.

**Configuration and state**

- DHCP by default; static settings from `confd` under `sys/net/<if>/`
  (`mode`, `address`, `gateway`, `dns`), read-only for `netd`, with the
  clamp-and-default rules of [driver-config-plan.md](driver-config-plan.md) §4.
  `confd` is a soft dependency.
- State goes on retained topics, not into `confd`: `system/net/<if>/addr`,
  `system/net/<if>/link`, and `system/events/network/up` (already named in
  [topics-catalog.md](topics-catalog.md)).

**Limits.** A fixed socket table (say 64), a per-uid socket quota, fixed
per-socket buffers (for example 16 KiB each way for TCP, 8 datagrams for UDP).
Nothing is allocated from a client-supplied size.

**Identity.** A dedicated `_netd` uid with no capabilities. It is the only
permitted client of the NIC driver and the only holder of the ring buffers.

## 7. Userland API

### 7.1 Native: Messenger interfaces (`idl/net.midl`)

Per AGENTS.md every interface is MIDL; nothing below is hand-encoded.

| Interface | Served by | Purpose |
|---|---|---|
| `os.lazy.net.nic.v1` | driver | link layer (§5) |
| `os.lazy.net.stack.v1` | `netd` | `Interfaces`, `Addresses`, `Routes`, `Stats`, `Resolve(name)`; what `netctl` and `nslookup` call |
| `os.lazy.net.socket.v1` | `netd` | sockets |

Sketch of the socket interface (shape only; names and types to be fixed in
review):

```idl
interface os.lazy.net.socket.v1 {
    method Open(kind: U32, protocol: U32) -> (sock: U32);      // Stream, Datagram, IcmpEcho, Raw
    method Bind(sock: U32, addr: SockAddr) -> ();
    method Connect(sock: U32, addr: SockAddr) -> ();            // parks until established
    method Listen(sock: U32, backlog: U32) -> ();
    method Accept(sock: U32) -> (conn: U32, peer: SockAddr);    // parks
    method Send(sock: U32, data: Bytes) -> (sent: U32);         // parks when the window is full
    method Recv(sock: U32, max: U32) -> (data: Bytes);          // parks; max = 0 is EINVAL; empty = end of stream
    method SendTo(sock: U32, addr: SockAddr, data: Bytes) -> (sent: U32);
    method RecvFrom(sock: U32, max: U32) -> (data: Bytes, from: SockAddr);
    method Poll(sock: U32, interest: U32) -> (ready: U32);      // parks until any bit is ready
    method Shutdown(sock: U32, how: U32) -> ();
    method SetOption(sock: U32, option: U32, value: U64) -> ();
    method LocalAddr(sock: U32) -> (addr: SockAddr);
    method PeerAddr(sock: U32) -> (addr: SockAddr);
    method Close(sock: U32) -> ();

    struct SockAddr { family: U32, addr: Bytes, port: U32 }     // 4 or 16 address octets
}
```

Design notes:

- **Ownership.** A socket belongs to the kernel-stamped sender of `Open`; every
  other caller gets `EACCES` (the `sndd` stream rule). Nothing in a request body
  names a caller.
- **Data plane, first cut: bytes in parcels.** Parcels carry up to 1 MiB, so a
  16 KiB `Send`/`Recv` chunk is one copy each way. This is enough for every
  tool in §8. A per-socket shared ring (client-supplied buffer in the request,
  as audio does) is a later optimisation, not a prerequisite.
- **End of stream is unambiguous.** `Recv` and `RecvFrom` reject `max = 0`
  with `EINVAL`, so an empty `data` on a stream socket always means the peer
  closed. Readiness without reading is `Poll`'s job, not a zero-length read.
- **Multiplexing.** `nc` needs "socket or stdin, whichever first": it issues
  `Recv` with `begin_call`, polls stdin, and collects the reply with
  `await_reply` (or uses `messenger_async`). `Poll` covers many sockets.
- **Lifetime. *Decided (N3).*** A socket is an id on the one shared endpoint, and
  `netd` reclaims a dead client's sockets by watching the scheduler's task list
  (native syscall 13, no kernel change): the owner is the sender's task slot (the kernel's
  pid is the slot, so a task landing in a dead owner's slot before the sweep is
  not told apart yet), and a sweep every 20 ticks closes what a dead owner left. The
  per-socket-channel design (the kernel closing a channel when its client dies)
  stays the better answer; the kernel now has per-connection channels
  (`Connect`, [architecture/ipc-core.md](architecture/ipc-core.md)), and moving
  `netd` onto them is what remains. Nothing in the interface depends on which
  one is behind it.
- **Client library.** `user/src/messenger/net.rs`: the generated client plus
  thin `TcpStream`, `TcpListener`, `UdpSocket` wrappers named after `std::net`,
  so tools read conventionally and a future native `std` port (rust-std.md
  Route A) has an obvious mapping.

### 7.2 Linux ABI: `AF_INET` in the kernel shim

Static musl binaries issue raw `socket`/`connect`/`sendto` syscalls, so there
is no libc layer to retarget (unlike Fuchsia's fdio or Redox's relibc); the
kernel shim has to front `netd`. Two ways:

| Option | How | For | Against |
|---|---|---|---|
| L1. Proxy every call (Redox scheme style) | `read`/`write`/`connect` on an `Fd::Inet` become Messenger calls to `netd`, made by the kernel on the caller's behalf | Least kernel state | The kernel today can only *post* one-way messages (`post_from_kernel`); it needs a kernel-originated synchronous call stamped with the caller's credentials. `poll`/`epoll` scan `Fd::poll()` synchronously and cannot round-trip to `netd` |
| **L2. Kernel socket object, `netd` behind it (Fuchsia `zx_socket` style)** | `Fd::Inet` wraps a kernel object with two byte or datagram queues and a state word, like today's `SocketPair`. The app's `read`/`write`/`poll`/`epoll` run entirely in the kernel on that object; control calls (`connect`, `bind`, `listen`, `accept`, options) go to `netd` over Messenger; `netd` pumps the far side | Reuses the existing stream, readiness and edge-generation code; no `netd` round trip per `read` | `netd`, a native task, needs a way to hold the far side (a new handle kind or a small `sock_*` native syscall); still needs the kernel-originated control call |

Recommendation: **L2**, decided and built in stage N5 (the spike confirmed it; see `docs/architecture/networking.md`). Either way this is new
kernel surface, so it ships with correctness and soak tests in
`kernel/src/tests/` (hostile `sockaddr` lengths, fd exhaustion, close during a
parked `connect`, thousands of connect/close cycles, `netd` death with sockets
open) and `python tools/test/run.py --accel none` must pass.

What falls out once it works:

- **BusyBox applets**, already in the image: `nc`, `wget`, `ftpget`/`ftpput`,
  `telnet`, `tftp`, `nslookup`, and servers (`ftpd`, `httpd`, `telnetd`).
  `ping` needs a raw or datagram ICMP socket mapped onto `netd`'s `IcmpEcho`
  kind.
- **Rust `std::net`** in any musl program, including XUI apps.
- **DNS for free**: musl resolves names itself over UDP, so it only needs
  `/etc/resolv.conf` (written by `netd` from DHCP) and `/etc/hosts` in the
  overlay root.
- Not covered: netlink and the `SIOCGIF*` ioctls, so BusyBox `ifconfig`/`route`
  will not work; `netctl` is the supported tool.

Also note `FD_COUNT` is 16 per task: enough for the tools here, tight for any
real server.

### 7.3 Authority and policy

- The default-deny Messenger ACL already gates `os.lazy.net.socket.v1` per
  method, so "this label may `Connect` but not `Listen`" is a policy rule, not
  new mechanism. Apps get no network unless granted
  ([security-model.md](security-model.md) §5 item 4).
- `netd` enforces the two reserved capabilities from the kernel-stamped
  credentials: `CAP_NET_BIND` for ports below 1024, `CAP_NET_RAW` for the `Raw`
  kind. The `IcmpEcho` kind is a datagram echo socket and needs neither, so
  `ping` runs unprivileged.
- The Linux shim must pass the *caller's* stamped identity on control calls.
  `netd` trusts it only on the kernel-originated path, never from a request
  body.
- Every refusal is audited. Egress rules, per-sandbox firewalling and consent
  prompts are S7 and hook in at `netd`'s `Connect`/`Bind`/`SendTo`.

## 8. The tools

Native programs in `user/src/bin/`, added to the native-program table in
`kernel/src/process/linux/native.rs` so the shell and the desktop Terminal can
run them (as `beep` was).

| Tool | Does | Needs |
|---|---|---|
| `nicctl` | MAC, link, frame counters | NIC driver only |
| `netctl` | `netctl addr`, `route`, `stats`, `dhcp renew` | `stack.v1` |
| `ping <host> [count]` | ICMP echo with sequence, loss and RTT; the host is a name or a dotted quad | `Ping`, `Resolve` |
| `nslookup <name>` | A-record lookup | `Resolve` |
| `nc <host> <port>`, `nc -l <port>`, `-u` | stdin/stdout to a TCP or UDP socket, client or listener | `Stream`/`Datagram` sockets, stdin multiplexing |
| `ftp <host>` | Passive mode only (`PASV`), binary transfers: `ls`, `cd`, `pwd`, `get`, `put`, `quit`; files through the VFS (`/tmp`, `/data`) | two TCP sockets; a small host-tested reply parser in `libs/` |

FTP is a line protocol with a second data connection; a passive-only client is
a few hundred lines and a good end-to-end exercise for connect, stream I/O and
close ordering. After stage N5 the BusyBox equivalents work as well, which
gives a useful cross-check: two independent clients over the same stack.

## 9. Verification

Same principle as audio: serial markers say *when* to look; the evidence is
what crossed the wire.

**QEMU setup.** `-netdev user,id=n0 -device virtio-net-pci,netdev=n0
-object filter-dump,id=f0,netdev=n0,file=net.pcap`. User networking gives the
guest 10.0.2.15 by DHCP, a gateway at 10.0.2.2 that reaches the host and
answers echo requests itself, and DNS at 10.0.2.3. `hostfwd=tcp::PORT-:7`
exposes a guest listener to the host. Echo to addresses beyond the gateway
depends on the host's ICMP support, so tests ping the gateway only.

| Layer | What | Run |
|---|---|---|
| Host unit | `libs/virtio-net` (layouts, feature sets), `libs/framering` (wrap, full/empty, hostile indices and lengths, long soak), `libs/netstack` (smoltcp over a scripted device: ARP, DHCP, echo, TCP open/transfer/close, backlog, port exhaustion, malformed and truncated frames) | `cargo test -p virtio-net -p framering -p netstack` |
| Harness unit | The pcap checker must fail when it should (missing reply, wrong payload, truncated capture) | `python tools/net/test_analyze_pcap.py` |
| End to end | `python tools/net/run.py`: DHCP completes; `ping 10.0.2.2` shows request and reply in the pcap; guest `nc` to a host echo server returns the exact bytes; host connects through `hostfwd` to guest `nc -l`; `ftp` get and put against a harness-run FTP server compare byte for byte; hostile-input probe and a connect/close soak, as `beep probe=1`/`soak=40` do | new |
| Variants | `--services` (supervised, `_net`/`_netd` uids), `--machine q35 --virtio-disk`, `--no-device` (driver and `netd` exit or idle cleanly), `--poll` (no interrupts) | new |
| Kernel | Only if kernel code is added (stage N5): correctness + soak in `kernel/src/tests/` | `python tools/test/run.py --accel none` |
| ABI bench | Stage N5: `tcpecho` and `udpecho` fixtures using plain `std::net` | `python tools/abi/run.py` |
| Visual | Terminal session typing `ping`, captured with `qemu_session.py` | optional |

A fully deterministic alternative to user networking is a host-side peer on a
raw-frame netdev (`-netdev socket`/`dgram`), where the harness script answers
ARP and echo itself. Keep it in reserve for frame-level tests (the D5 "inject
an ARP reply" check) and for CI hosts where user-mode ICMP is unreliable.

## 10. Staged delivery

Each stage is independently mergeable and ends with evidence.

| Stage | Deliverable | Kernel change | Evidence |
|---|---|---|---|
| **N0** | This plan reviewed; `idl/net.midl` with the NIC interface (wake-up mechanism settled), frame-ring layout, `libs/framering` + `libs/virtio-net` with host tests | none | `cargo test`; `midlc` output replaces the hand-written IDL page |
| **N1** | `virtio-net` driver, `_net` uid, `init` row, `LAZYOS_NET=1`, `nicctl`, `tools/net/run.py` skeleton, `--net` in `run_demo.py`; class ACL rules for `_net` on `os.kernel.dev.net` (claim, map, DMA). Closes the D5 half of #241 | none expected | `NET:NIC:PASS`; a transmitted ARP request and its reply in the pcap; interrupt delivery count |
| **N2** | `libs/netstack` + `netd`: DHCP, ARP, echo responder, `stack.v1`, `netctl`, and `ping`; ACL rules for `nic.v1` (`_netd` and root, for the diagnostic tools, are its only permitted callers of the control methods; anyone may read `Info` and `Stats`; `_net` sends its own `Notify`) and for `stack.v1` | a native entropy call if none exists | `ping 10.0.2.2` replies visible in the pcap: **first user-visible milestone** |
| **N3** | `socket.v1` (TCP, UDP, parked calls, ownership, reclaim), client library, `nc`, `nslookup`; ACL rules for the `socket.v1` methods | none (or peer-closed notification, §7.1) | bytes round-trip with a host echo server both ways; probe and soak modes |
| **N4** | `ftp` client | none | byte-exact get/put against the harness server: **stated goal reached** |
| **N5** | Linux `AF_INET` shim (L1/L2 spike, then build), `/etc/resolv.conf`, ABI fixtures | **yes**, with full test coverage | BusyBox `nc`/`wget`/`ftpget` and `std::net` fixtures pass |
| **N6** | Hardening: per-profile tightening of the socket rules and denial tests, quotas, loopback, e1000, shared-ring data plane, finer clock, fuzzing the frame and request parsers | some | policy denial tests, throughput numbers |

ACL grants land with the stage that introduces the actor, not at the end. The
fabric is still in its bootstrap-allow window today (as for `sndd`, see
[architecture/audio.md](architecture/audio.md) "Not done"), so nothing is
refused without them yet, but each stage must keep working the moment a policy
is loaded.

After N6 the rest of S6 (TLS through `keyd`, IPv6, daemons, Messenger over the
network) builds on the same interfaces.

### 10.1 Status

| Stage | State |
|---|---|
| N0 | **Built.** `idl/net.midl` (`os.lazy.net.nic.v1`), `libs/framering`, `libs/virtio-net`, `libs/fuzzkit`, the 256-entry queue cap, the `fuzz/` cargo-fuzz crate, `.github/workflows/net.yml`, `tools/net/README.md` |
| N1 | **Built.** `netdrv` (`user/src/bin/netdrv.rs`), `libs/nicdrv` (its host-tested core), `nicctl`, `user/src/messenger/net.rs`, the `_net` uid (902) and `init` row, `LAZYOS_NET=1`, `libs/netpolicy` (class rules, loaded by a kernel test), `tools/net/run.py` + `analyze_pcap.py` + their tests, `--net` in `run_demo.py`. Evidence: a 42-exchange ARP capture, frame-policy and probe frames checked on the wire, in five harness variants |
| N2 | **Built.** `libs/netstack` (smoltcp 0.14), `netd` (`user/src/bin/netd.rs`), `os.lazy.net.stack.v1` (`idl/net.midl`), `netctl`, native `ping`, the `_netd` uid (903, no capabilities) and `init` row, `LAZYOS_NETD=1`, `libs/netpolicy` call rules (loaded by a kernel test), native syscall 26 and `CLOSE_RELEASE`. Evidence: a capture with 6 DHCP exchanges and 46 echo pairs (checksums valid) in the default, `--services`, q35 and `--poll` runs; `--no-device` is an idle-state check only (no capture is analysed: `netd` and the driver must idle cleanly). Kernel: `python tools/test/run.py --accel none`, 589/589, which includes the syscall 26 tests (bounds, bad pointers, distinct output, soak), the `CLOSE_RELEASE` tests (channel and syscall level, 20 000-round soak) and the `nic.v1`/`stack.v1` call-rule test |
| N3 | **Built.** `os.lazy.net.socket.v1` (`idl/net.midl`) and `Resolve` on `stack.v1`; the socket layer in `libs/netstack` (table, TCP, UDP, DNS; 60 host tests, a 300-cycle connect/close soak, a seeded random-call fuzz); `netd` serving them with parked calls, per-owner and total limits, ownership by task slot (a spawn counter to tell slot reuse apart is future kernel work) and reclaim of dead owners; the client `user/src/messenger/netsock.rs` and `netstd.rs` (`TcpStream`, `TcpListener`, `UdpSocket`); `nc`, `nslookup`, `ping` with names; `netctl sockets`, `sockprobe=1` and `socksoak=<n>`; the `socket.v1` and `Resolve` rules in `libs/netpolicy` (loaded by the kernel test). Evidence (`tools/net/run.py --netd`): 42 TCP flows to the host echo server (215 618 bytes) whose streams reassemble from the capture to exactly what the server received, 41 UDP echo pairs, a DNS query for `localhost` that was answered, and a 150 000-byte stream the harness sent into the guest's `nc -l` through a port forward and got back unchanged; the checker has its own tests (`tools/net/test_sockets_pcap.py`) |
| N4 | **Built.** `libs/ftpwire` (reply parser for multi-line replies in any chunking, strict `PASV`/`EPSV` parsing, injection-proof command builder, CRC-32, the `nc -g` pattern; 26 tests incl. a seeded random-stream fuzz), the `ftp` tool (`user/src/bin/ftp.rs`, `ftp/session.rs`): passive mode only, binary, `pwd cd ls size get put quit`, script-driven (`cmd ; cmd`) or a prompt. Native programs have no file-write syscall, so `get` writes to standard output (`get x - > /tmp/x`) or discards after checksumming (`get x !`), and `put` reads a file or generates a stream (`put -g N name`). The data connection always goes to the control peer, taking only the port from the `227` reply. Evidence (`tools/net/run.py --netd`): against a host FTP server (`hostpeers.FtpServer`) the capture shows one control connection whose commands equal the server's record, and five data connections (listing, two downloads, an upload of 150 000 generated bytes, a re-download of it) whose bytes equal the files byte for byte, each closed by a FIN from both sides, plus the client's printed CRC-32s; `tools/net/test_sockets_pcap.py` covers the checker |
| N5 | **Built.** The kernel's `AF_INET` socket object (`kernel/src/ipc/inet/`, option **L2** of §7.2) over small-ring socket pairs; the Linux calls `socket`, `bind`, `connect`, `listen`, `accept`/`accept4`, `shutdown`, `getsockname`/`getpeername`, `sendto`/`recvfrom`, `setsockopt`/`getsockopt` plus `FIONBIO`/`FIOCLEX` on them; native syscall 27 as the pump `netd` serves (`netd/inet.rs`, `libs/lazyos-sys/src/inet.rs`); the `netfix` Linux fixture (`std::net` only). Evidence: 24 in-kernel tests (`LAZYOS_TEST_FILTER=inet`, with a fake `netd`: correctness, hostile input, bounds, and soaks of 3 000 connections, 5 000 datagrams, a megabyte each way, random call sequences) and `tools/net/run.py --netd`, where `netfix` echoes 200 000 bytes, completes a timed non-blocking connect, fails a connect to a closed port, exchanges 22 datagrams and accepts a connection the harness opens, all judged from the capture against the host servers' records. Gaps are listed in `docs/architecture/networking.md` |
| N6 | Not built (out of scope for the current work) |

## 11. Risks and open questions

1. **Peer-death signalling for sockets** (§7.1). Decides whether a socket is a
   channel or an id. Needs an answer in N0, because it shapes the IDL.
2. **Kernel-originated synchronous calls** (§7.2). Required by the Linux shim
   under both options; does not exist today. Isolated to N5 so it cannot delay
   the native tools.
3. **10 ms clock.** `ping` will report RTTs of 0 or 10 ms, and a `netd` that
   only wakes on ticks adds latency. Interrupt-driven wake-ups solve the
   second; the first wants a TSC-backed monotonic clock exposed to native
   programs. Worth doing in N6, not before.
4. **Latency of three context switches per packet** (driver → `netd` → app) on
   one CPU. Batch per wake, coalesce notifications, measure before optimising.
   Interactive tools will not notice; bulk transfer numbers go in N6.
5. **Shared interrupt line 11** with the polled in-kernel virtio-blk. Covered
   by the shared-INTx contract, but the ack deadline (100 ticks) means a busy
   `netdrv` must ack promptly; polling remains the fallback.
6. **NIC IDL is unimplemented prose.** *Resolved in N0:* it is MIDL
   (`idl/net.midl`), the wake-up is `Kick`/`Notify` plus an `armed` flag, and the
   rings travel in the request (§5).
7. **`MAX_QUEUE = 64`** in `libs/virtio` against a documented default of 256
   ring entries. *Resolved in N0:* the cap is 256 (§5).
8. **smoltcp TCP limits** (no SACK/timestamps, one connection per listening
   socket). Acceptable for the goal; the fallback if it ever is not would be
   vendoring Netstack3's core, which is a large project.
9. **Task and fd budgets.** Two more always-on services (driver, `netd`) and
   16 fds per task. Fine now (`MAX_TASKS` is 256); revisit for servers.
10. **Trusted DMA driver.** Unchanged from driver-plan D5; the split in §4.1
    keeps the parser out of that trust domain.

## 12. Non-goals for this plan

IPv6, TLS, Wi-Fi, routing/forwarding/NAT, multiple NICs, a firewall language,
netlink or `ifconfig` compatibility, zero-copy receive, remote Messenger
transport, and hot-plug. Each has a seam above; none is needed for `ping`,
`nc` and `ftp`. Wi-Fi (and the multi-NIC `netd` it needs) is explored in
[wifi-plan.md](wifi-plan.md); TLS (HTTPS and Gmail IMAP as the targets) in
[tls-plan.md](tls-plan.md).

## 13. Decisions taken and corrections to this plan

Kept current as stages land; the reasoning for each is where it is used.

**N0**

- *Wake-up mechanism* (§5): one-way `Kick` and `Notify` to endpoints, coalesced by
  an `armed` flag in the ring header. The plan preferred the endpoint over a
  topic but did not say how the coalescing to "one outstanding notice" would be
  done without kernel help; the shared flag is that mechanism.
- *Ring ownership* (§5): the rings, and the notify endpoint, are in the
  `AttachRing` request. The plan said "the client supplies the rings"; the
  interface also needs the slot count in the body so the driver can check both
  buffer lengths exactly.
- *Frame ring layout* (§5): the slot is 2048 bytes, so the largest frame is
  **2046**, not 2048; the plan's "2048-byte slots, a `u16` length" leaves the
  length inside the slot. Slot counts are 16..=1024 (a ring is at most 2 MiB and
  one `dma_alloc` is at most 4 MiB).
- *Queue cap and settings* (§5, risk 7): cap raised to 256; `rx_ring_entries`
  clamps to 16..=256 and `mtu` to 576..=1500. Both differ from
  [driver-config-plan.md](driver-config-plan.md) §2, which should be read with
  this note until the two are merged.
- *Poisoned rings.* An index that claims more frames than the ring holds
  poisons that endpoint permanently; the plan only said "validated". The driver
  detaches such a client and counts it.
- *Extra interface members.* `NicInfo.max_frame`, a wider `NicStats` (bytes,
  runts, oversize, ring errors, interrupts), a `LinkEvent` payload for the link
  topic, and `NotifyBit`/`RxMode` enums, none of which the draft had.
- *One buffer, not two.* The first N0 draft sent the rings as `buffers[0]` and
  `buffers[1]`. The kernel reports only the first transferred buffer to a
  receiver (`recv` gives a first handle and a first buffer, and the parcel bytes
  still carry the sender's handle numbers), so the second could never be used.
  Both rings now share one buffer of `2 * ring_bytes(slots)`. Found while reading
  `sndd`'s default-arm cleanup for N1 (gone since `messenger-core-plan.md`
  M3: an unclaimed object closes with the `Message`).

**N1**

- *Interrupts share the service endpoint.* The claim names the driver's own
  service endpoint (the unpublished side of its channel pair, which the kernel
  accepts because it is held by exactly one handle), so the kernel's interrupt
  messages and client calls land in one inbox and one `recv` with a deadline
  serves both. The plan's "single-wait event loop" holds for the driver as well
  as for `netd`, and no second endpoint or selector is needed. The line is
  claimed `FLAG_SHARED_IRQ` (it is shared with the polled virtio-blk on both QEMU
  machine types); `irq_mode=poll`, an unroutable line or a refused endpoint fall
  back to polling.
- *The driver core is a library.* Everything whose bug could hurt a neighbour
  (the virtqueue slot pools, the attached client, the frame policy, the
  receive filter, the statistics) is `libs/nicdrv`, host tested against a fake
  virtio-net device and a hostile client, with a model-checked fuzz entry point.
  The plan's file list (`device.rs`, `queues.rs`, `rings.rs`, `service.rs`) maps
  onto `netdrv/` for the parts that touch the machine and `nicdrv` for the rest.
- *`SetRxMode` is software.* Without the control queue the device filters
  nothing, so `Filtered` (the default) is a destination-MAC check in the driver
  (ours, broadcast or multicast), `Promiscuous` passes everything, `Off` drops
  everything; every mode is "applied", so `ok` is always true. The draft allowed
  `Filtered` to degrade to promiscuous and answer false.
- *Idle, not exit, without a device.* `sndd` exits 0 on a machine with no card;
  `netdrv` parks, because its `init` row restarts it on any exit (a restart-class
  setting change exits 0 by design) and a restart loop on a missing device would
  end in `Failed`.
- *Class ACL rules are data the kernel installs at boot.* `libs/netpolicy`
  holds the rules, and since issue #481 `dev::policy` installs them (with
  `_usb`'s and `_snd`'s) before `init` starts any driver, default deny for
  every other non-root uid (`dev_sys_boot_policy_confines_each_driver_to_its_class`). The rules (`_net`: claim, map and DMA on
  `os.kernel.dev.net`), host tests pin their spelling to the `midlc` ids, and
  `dev_sys_net_driver_policy_is_exactly_the_class_rules` loads them into the real
  ACL and checks the decisions, including that root is refused the class. The
  rules for `nic.v1` itself (`_netd` the only client) land with `netd` in N2.
- *Two kernel facts the plan did not know.* `recv` reports only the first
  transferred buffer and handle (see N0), and a driver's per-call allocation
  lands in a recycling heap only below 64 KiB, so every hot path in the driver
  and `nicctl` reuses its buffers.
- *Queue sizes follow the device.* `Transport::queue_max` reads the device's
  limit per queue; the driver uses the smaller of the setting and that limit
  (QEMU offers 256), rounded down to a power of two.
- *Evidence had to wait for the interrupt message.* The self-test can find its
  ARP reply by polling before the kernel's interrupt message has been read, so
  it gives the message a few ticks before judging `NET:IRQ`. `delivered=1` after
  one exchange is normal: the kernel keeps one message outstanding per claim.
- *Not done in N1:* `devd` (out of scope by the brief), MSI, offloads, jumbo
  frames, a second driver, and a shared module for the PCI bring-up code that
  `sndd` and `netdrv` now both carry (the two copies of `device.rs`/`dma.rs`
  differ only in the error type and the device id).

**N2**

- *Entropy.* No native call existed (`keyd` seeds from `RDRAND` and the tick).
  Added syscall 26, `random(buf, len)`: at most 256 bytes per call from the
  kernel CSPRNG that backs Linux `getrandom`, open to every task with no
  capability (it grants no authority, and `netd` has none). Tests: bounds, bad
  pointers, distinct output, a soak.
- *`Ping` is on `stack.v1`.* The plan listed ping with the tools; it is a method
  of the stack service (`Ping(dst, payload_len, timeout_ms) -> EchoResult`), a
  parked call, so `ping` and `netctl` are thin clients and the echo socket stays
  inside `netd`.
- *`CLOSE_RELEASE`.* `netd` passes its own endpoint to the driver as the notify
  endpoint so one `recv` serves both. The driver's close of a received endpoint
  then closed the service for everyone. Releasing (close only if no other
  handle) is now a flag of `close_endpoint`; the driver and `netd` release what
  they did not create. The second kernel change of the stage, with tests.
- *Head-of-line blocking in smoltcp's ICMP socket.* An undeliverable queued
  packet stays at the head, so one ping to a dead address blocked all later
  ones. The probe found it; `Stack::reset_icmp` rebuilds the socket when a ping
  times out with its packet queued. Cost: a reply to a ping that already timed
  out is discarded.
- *Leases are validated.* The plan assumed DHCP could be trusted. A lease with
  an unusable address or a prefix outside /1 to /30 is rejected: any address
  held is dropped and the DHCP client restarts discovery (resetting smoltcp's
  socket, which otherwise believes it is configured until the lease expires);
  unusable routers and resolvers are dropped and at most three resolvers are
  kept. A server that keeps offering a bad lease keeps the client discovering,
  at the network's round-trip pace; there is no backoff yet.
- *`Notify` needs a rule.* The driver's wake-up is judged as a call from `_net`
  on `nic.v1`, so `NIC_CLIENT_RULES` allows `_net` exactly that method (and no
  other uid may send it).
- *10 ms clock accepted* as the plan said: RTTs are 0 or 10 ms; the first ping
  (ARP first) takes about 90 ms.
- *Not done in N2:* DNS queries (resolvers are kept, not used), non-owner call
  enforcement (no policy loader, so `Renew` and `Reattach` are open to anyone
  until one exists), releasing a parked `Ping` whose caller cancelled or died
  (the slot is held until the ping's own timeout, at most 60 s), the shared PCI
  bring-up module, hosted CI (the workflow is written, not run on GitHub).
  `sndd` leaving extra received handles open was closed by
  `messenger-core-plan.md` M3 (an unclaimed object closes with the
  `Message`).
