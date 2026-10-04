# Networking: NIC interface, frame rings, the virtio-net driver, the stack service

**What it is.** LazyOS networking, built in stages from
[`docs/networking-plan.md`](../networking-plan.md) (the plan holds the rationale
and the roadmap; this page describes what exists). The kernel knows nothing about
networking: a userspace NIC driver serves `os.lazy.net.nic.v1` over Messenger,
and a userspace stack service is its only client, exactly as `sndd` and
`os.lazy.audio.v1` split audio ([`audio.md`](audio.md)).

**Status: stages N0 to N5.** The interface, the frame ring, the virtio-net
wire definitions, the `netdrv` driver, `nicctl`, the packet-capture harness, the
stack library `netstack`, the stack service `netd`, `netctl` and `ping` are built
and verified: `ping 10.0.2.2` is answered and the replies are in the capture. N3
adds TCP and UDP sockets, name lookups, `nc` and `nslookup`: bytes round-trip
with a host echo server both ways, and the capture agrees with the server
byte for byte. N4 adds the `ftp` client (byte-exact against a host server) and N5 the Linux `AF_INET` shim, so a
static musl program using only `std::net` talks through the same stack.

**Key files**

| Path | Role |
|---|---|
| `idl/net.midl` | `os.lazy.net.nic.v1`, compiled by `midlc` into `libs/generated` and `docs/idl/os.lazy.net.nic.v1.md` |
| `libs/framering/` | The single-producer/single-consumer frame ring in shared memory. Pure `no_std`; `fuzz.rs` is the model-checked entry point |
| `libs/virtio-net/` | virtio-net feature bits, device config, the 12-byte packet header, the frame-length policy, receive-completion parsing and the clamped driver settings. Pure `no_std` |
| `libs/nicdrv/` | The driver's logic, host tested against a fake device and a hostile client: `queues.rs` (virtqueues over one DMA block, a slot per descriptor), `engine.rs` (the attached client, frame policy, receive filter, statistics), `arp.rs` (the self-test's one ARP exchange), `testdev.rs` (the fake device), `fuzz.rs` |
| `libs/virtio/` | The modern virtio-PCI transport (shared with `sndd`); queue cap raised to 256 in N0, `Transport::queue_max` added in N1 |
| `libs/netpolicy/` | The access rules of the stack (`_net`'s claim/map/dma on the net class), as data with host tests; the kernel suite loads them into the real ACL |
| `libs/fuzzkit/` | A seeded PRNG and a driver that prints the failing seed; host only |
| `user/src/bin/netdrv.rs`, `netdrv/` | The driver: `device.rs` claim + BAR mapping, `card.rs` bring-up, doorbell, link and interrupts, `dma.rs`, `config.rs` (`confd` keys), `service.rs` request dispatch, `selftest.rs` |
| `user/src/messenger/net.rs` | Blocking client of the interface and the client's side of the rings |
| `user/src/bin/nicctl.rs`, `nicctl/` | The control tool and evidence client: show, `arp`, `probe=1` (+ `role=intruder`), `soak=<n>` |
| `libs/netstack/` | The stack over a frame ring, host tested: `device.rs` (smoltcp `Device` over the two rings), `stack.rs` (interface, DHCP, the echo socket and the ping table), `config.rs` (static configuration, address validation), `testnet.rs` (a scripted gateway and LAN), `tests.rs`, `fuzz.rs` |
| `idl/net.midl` (second interface) | `os.lazy.net.stack.v1`, compiled into `docs/idl/os.lazy.net.stack.v1.md` |
| `user/src/bin/netd.rs`, `netd/` | The stack service: `nic.rs` (finds the driver, attaches rings, re-attaches), `config.rs` (`confd` keys), `service.rs` (`stack.v1`, parked `Ping` calls, address topics) |
| `user/src/messenger/netstack.rs` | Blocking client of `stack.v1` |
| `user/src/bin/netctl.rs`, `netctl/`, `ping.rs` | `netctl` (show, `sockets`, `renew`, `probe=1`, `soak=<n>`, `sockprobe=1`, `socksoak=<n>`) and the native `ping` (names resolve) |
| `libs/netstack/src/stack/` (N3) | `sockets.rs`, `tcp.rs`, `udp.rs`, `dns.rs`, `observe.rs`: the socket table and its operations, name lookups; `testpair.rs` (two stacks back to back) and `testdns.rs` for the tests |
| `user/src/bin/netd/` (N3) | `sock.rs` (`socket.v1` dispatch), `parked.rs` (parked calls), `resolve.rs` (`Resolve`), `owners.rs` (callers by slot) |
| `user/src/messenger/netsock.rs`, `netstd.rs` | Blocking client of `socket.v1`; `TcpStream`, `TcpListener`, `UdpSocket` over it |
| `user/src/bin/nc.rs`, `nslookup.rs` | The tools |
| `kernel/src/process/randsys.rs` | Native syscall 26, random bytes for services (with `CLOSE_RELEASE`, N2's only new kernel surface) |
| `kernel/src/process/linux/native.rs` | `nicctl`, `netctl`, `ping`, `nc` and `nslookup` in the table of native programs a shell may run |
| `fuzz/` | The cargo-fuzz crate (outside the OS workspace) and its checked-in seeds |
| `xui-app/src/net/`, `xui-app/src/bin/network.rs`, `nettools.rs` | The desktop apps: Network (status; DHCP or a static setup checked with `netstack::config` and written to `confd`) and Net Tools (ping, lookups, an HTTP fetch and a web server over `std::net` on worker threads). Shipped by `LAZYOS_NETD=1` desktops; see [`../networking-host-access.md`](../networking-host-access.md) |
| `tools/net/qemu_net.py` | The QEMU arguments of an interactive or scripted networked boot (`run_demo.py --net`, the launcher, the screenshot tools): card, user network, port forwards, isolation, capture |
| `tools/net/` | `run.py` the harness, `analyze_pcap.py` + `pcap.py` the capture judge and `test_analyze_pcap.py` its tests; N3: `hostpeers.py` (the host's echo servers and inbound client), `sockets_pcap.py` (the TCP/UDP/DNS judge) and `test_sockets_pcap.py` |
| `.github/workflows/net.yml` | Host tests, lints, seeds drift check, bounded fuzz runs, the harness in ten variants (five for the driver, five with `netd`) |

## The interface

`os.lazy.net.nic.v1` is link layer only: `Info`, `SetRxMode`, `AttachRing`,
`DetachRing`, `Stats`, and two one-way messages, `Kick` and `Notify`. Its topic
`system/net/{nic}/link` is retained and carries a `LinkEvent` (published by the
driver at start and on every link change when the broker is reachable).

**The client owns the rings** (the audio rule): replies cannot carry buffers or
handles, and a device must never read memory a client can rewrite. So
`AttachRing` carries the client's shared buffer and a notify endpoint in the
request's parcel vectors (`buffers[0]` holds both rings, receive at byte 0 and
transmit at `ring_bytes(slots)`; `handles[0]` is the endpoint) and only the slot
count in the body. It is one buffer because the kernel surfaces only the first
transferred buffer of a request to its receiver (`recv` reports a first handle
and a first buffer; the parcel bytes keep the sender's numbers). The driver
copies frames between those rings and its own DMA slots in both directions.

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
"sleep" is never missed. A consumer that never arms just polls. `Notify` also
carries `TxSpace` (the transmit ring had been full and drained) and `LinkChange`.

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

## The driver (`netdrv`)

`netdrv` is an ordinary ring-3 program. It claims the virtio-net function (QEMU's
default `virtio-net-pci` is the transitional `1af4:1000`; `disable-legacy=on`
makes it `1af4:1041`; either way only the modern interface is used), and serves
the interface for the life of the machine.

**Bring-up.** `device.rs` enables memory decode and bus mastering, parses the
virtio capabilities, maps the BARs they point into (every structure
bounds-checked against the BAR the kernel reported) and wraps them in a
`Transport`. `card.rs` negotiates features (`VERSION_1` required; `MAC` and
`STATUS` wanted; nothing else, so no offloads, no merged buffers, no control
queue), reads the device config (a missing, multicast or all-zero MAC stops the
driver with a message), picks queue sizes from the settings but never above what
the device offers (`Transport::queue_max`), allocates **one** DMA block for both
queues and every slot (a receive slot and a transmit slot per descriptor), posts
every receive buffer, and sets `DRIVER_OK`. The block is held until the driver
exits: the kernel treats a driver freeing its own DMA buffer as a device stop
([`audio.md`](audio.md), "never free a DMA buffer while the device runs").

**One loop, one endpoint.** The claim names the driver's own service endpoint as
the interrupt endpoint and opts in to a shared line (line 11 is shared with the
polled virtio-blk on both QEMU machine types), so the kernel's interrupt messages
and client calls arrive in the same `recv`, and a single wait with a deadline
serves both. A line that is not routable, or `irq_mode=poll` (or `irq=poll` in
the service arguments), falls back to polling: the loop then wakes every
`poll_interval_ms`. With the line armed nothing needs a timer (P4.5): frames and
finished transmits interrupt, the client's frames come with a kick, and a full
client receive ring drops rather than holds; the loop parks for 20 ticks only to
pace the link poll, the keep-alive and the configuration refresh. Each wake: read the interrupt message if that is what came
(only slot 0 may send one; the ISR status is read to deassert the level, then
`irq_ack`), answer the request if it was one, then pump.

**Pumping** is `nicdrv::Engine::pump`: reap finished transmits, move completed
receive buffers to the client's receive ring (each copied out of its DMA slot
first, then checked, filtered and pushed), move frames from the client's
transmit ring into transmit slots, kick the device once per queue that gained
buffers, and report which wake-ups the client is owed. The **frame policy** holds
in both directions: a frame shorter than the 14-byte Ethernet header or longer
than MTU + 14 is dropped and counted (`runts`, `oversize`), never truncated, so
it never reaches the device or the wire. The receive filter (`SetRxMode`) drops
frames not for this MAC (broadcast and multicast pass) unless promiscuous, and
everything when `Off`.

**Hostile device.** A used-ring entry the driver cannot have produced (an id
that is not in flight, a used index that ran ahead) makes the driver exit with
`NET:NIC:FAIL` so `init` restarts it; a length beyond the slot, a short buffer or
an offload header is one dropped frame and a counted ring error.

**Security.** Under `init` the driver runs as system uid 902 (`_net`) with only
`CAP_DEV_CLAIM` (`NETDRV:CRED uid=902 caps=0x100` in the boot log). The owner of
the attachment is the kernel-stamped sender of `AttachRing`; nothing in a request
body names a caller. Another task gets `EACCES` on `DetachRing` and `SetRxMode`,
`EBUSY` on `AttachRing`, and its `Kick` is ignored. `Info` and `Stats` are open.
A client that exits without detaching is found by an empty `Notify` sent every
second to its endpoint (a dead peer makes the send fail) and released. Buffers
and endpoints a refused request carried are closed (the kernel surfaces only the
first of each kind, so a request carrying several leaves the extras open until
the client's own quotas stop it; the same gap `sndd` documents).

**Access rules.** `libs/netpolicy` names what `_net` may do to a device: claim,
map and DMA on the `os.kernel.dev.net` class, nothing else. The kernel
installs these class rules at boot with every other driver's (`dev::policy`,
issue #481): from then on a non-root uid claims, maps or DMAs a device class
only if a rule gives it that class (`dev_sys_boot_policy_confines_each_driver_to_its_class`). The kernel suite also
loads this table into the Messenger ACL and checks that `_net` gets a NIC with every right, is refused other
device classes, and that no other uid (root included) is granted the net class
(`dev_sys_net_driver_policy_is_exactly_the_class_rules`).

**Configuration.** `sys/dev/net/virtio-net/{irq_mode,poll_interval_ms,
rx_ring_entries,tx_ring_entries,mtu,mac_override}` from `confd`, a soft
dependency: tried for about a second at start, defaults otherwise, re-read every
5 s (which also picks up a `confd` that started late). Every value is clamped by
`virtio_net::settings`; `mtu` applies live, and a restart-class key makes the
driver exit 0 (and `init` respawn it) only when its *effective* value changes.
`mac_override` changes the MAC the driver reports and filters on; without the
control queue it cannot reprogram the device.

## The stack service (N2)

```
app / ping / netctl --stack.v1--> netd --nic.v1 + two rings--> netdrv --virtio--> QEMU
```

`netd` runs as system uid 903 (`_netd`) with **no capabilities**
(`NETD:CRED uid=903 caps=0x0`). It is the only client of the NIC driver and the
only place frames are parsed. `libs/netstack` is smoltcp 0.14 (no default
features: Ethernet, IPv4, DHCPv4, ICMP, UDP and TCP sockets; the stack answers
echo requests itself) behind a `Stack` type; `netd` is the loop around it.

**One loop, one endpoint, again.** `netd` resolves its own published endpoint and
hands that handle to the driver as the notify endpoint of `AttachRing`, so one
wait with a deadline serves client calls and the driver's `Notify` messages (a
`wait_any` that also holds the kernel's `AF_INET` doorbell, see "Linux sockets").
The deadline is `Stack::poll_delay_ms` (smoltcp's next timer). The rings have 256
slots each way, more than a 256 KiB TCP window in full-size segments. The attachment is
re-made when the driver goes away and comes back (`Reattach` does it on demand).

**Device.** `RingDevice` is smoltcp's `Device` over the client side of the two
rings. `receive` checks that the transmit ring has room before it pops a frame,
so a reply is never made and then dropped. A peer that poisons a ring detaches
the device (`is_poisoned`); the stack keeps its state and waits for a new
attachment.

**Configuration.** DHCP by default. A lease is validated before it is applied
(`accept_lease`): the address must be a usable unicast address and the prefix
between /1 and /30, otherwise the lease is rejected: a held address is dropped
and discovery restarts. A router that is not a usable unicast address, or is the address
itself, is dropped; unusable resolvers are dropped and at most three are kept.
Static configuration (`netd` arguments or `confd` `sys/net/*`) goes through the
same checks. Randomness (initial sequence numbers, DHCP transaction ids) is drawn
from native syscall 26 once at start.

**`os.lazy.net.stack.v1`** (`idl/net.midl`): `Interfaces`, `Addresses`, `Routes`,
`Stats`, `Ping(dst, payload_len, timeout_ms) -> EchoResult`, `Renew`, `Reattach`.
`Ping` is a **parked call**: `netd` keeps the caller's transaction and replies
when the echo reply arrives or the timeout expires, so a slow ping never blocks
the loop. At most 8 are pending (`MAX_PINGS`) and at most 4 per caller
(`PER_CALLER_PINGS`, `EAGAIN` past either), payloads are at most 1400 bytes,
and a ping to an unusable address is refused up front. The retained topic
`system/net/{ifname}/addr` carries the address, and `system/events/network/up`
fires when the first address is bound.

**Access rules** (`libs/netpolicy`, loaded into the real ACL by the kernel test
`net_call_rules_decide_who_may_call_what`): `_netd` and root may call everything
on `nic.v1`, everyone else only `Info` and `Stats`, plus `_net` (the driver) may
send its own `Notify` wake-up; on `stack.v1` everyone may
read and `Ping`, only `_netd` and root may `Renew` or `Reattach`. Unlike the N1
class rules these call rules refuse nothing today (no Messenger policy
loader), so the driver's own owner
check is what stops a second client.

**Clock.** The kernel tick is 100 Hz, so the stack's clock is 10 ms and a ping
reports 0 or 10 ms (the first, which waits for ARP, about 90 ms). The plan
accepted this; a finer clock is N6.

### Kernel surface added in N2

* **Syscall 26, `random(buf, len)`**: up to 256 bytes from the kernel CSPRNG
  (`kernel/src/entropy.rs`, the source behind Linux `getrandom`). Open to every
  task, no capability (randomness grants no authority); a bad destination is
  `-EFAULT` with nothing written; a count above 256 is a short read. The plan
  said to check for an existing native call: there was none (`keyd` seeds from
  `RDRAND` and the tick). Tests: `kernel/src/tests/hardening_suite/native_random.rs`.
* **`CLOSE_RELEASE`** flag of `close_endpoint`: close the side only if no other
  handle names it. An explicit close ends a channel side for every holder, which
  is right for an owner and wrong for a receiver that was handed somebody's
  endpoint: the driver, closing the notify endpoint it was given, was closing
  `netd`'s service endpoint. The driver and `netd` now release what they did not
  create. Tests: `kernel/src/tests/ipc_channel_suite/release.rs` (including a
  20 000-round soak) and `messenger_suite/release_flag.rs`.

### What testing found

* A head-of-line bug: smoltcp keeps an undeliverable queued ICMP packet at the
  head of the socket's queue, so one ping to an unreachable address blocked
  every later one. The in-guest probe found it. `Stack::reset_icmp` rebuilds the
  socket when a ping times out with its packet still queued; a host test fails
  without the fix. The trade-off: a late echo reply to a ping that already timed
  out is discarded, which is what a timeout means anyway.
* smoltcp does not queue an automatic echo reply behind ARP resolution, so a
  host test primes the neighbour first; on the wire the requester has just
  resolved us, so it does not matter.
* smoltcp cannot report a lease length without a buffer to keep the DHCP reply
  in; `Stack` owns a 1 KiB buffer for the socket (as a raw pointer freed in
  `Drop` after the socket is removed, so no `Box` is retagged under the
  socket's borrow). An earlier draft leaked it, and libFuzzer's leak detector
  caught that because the target builds a stack per input.

## Verification

The proof is the packet capture, not the log. `python tools/net/run.py` builds
with `LAZYOS_NET=1`, boots headless QEMU with `-netdev user,id=n0 -device
virtio-net-pci,netdev=n0 -object filter-dump,id=f0,netdev=n0,file=net.pcap`, and
`analyze_pcap.py` judges the file:

| Evidence | Where it comes from |
|---|---|
| `NET:NIC:PASS`, the driver's broadcast ARP request for 10.0.2.2 and the gateway's reply | the driver's self-test through its own engine, then in the capture |
| 42 complete ARP exchanges, each request answered *after* it by a reply addressed to the guest with consistent sender fields | self-test (1) + `nicctl arp` (1) + `nicctl soak=40` (40 attach/exchange/detach cycles) |
| no captured frame shorter than 14 or longer than 1514 bytes | the driver's frame policy, seen from outside |
| the probe's frames on the wire: exactly 14 and 1514 bytes, payload intact; none of 13 or 1515 bytes | `nicctl probe=1` pushes all four through its own ring |
| `NET:IRQ:PASS delivered=N` (or `NETDRV:IRQ:POLLING`) | the driver's own counter; the kernel's interrupt messages are not on the wire |
| `NICCTL:PROBE:PASS checks=40`, `NICCTL:INTRUDER:PASS checks=5`, `NICCTL:SOAK:PASS iterations=40` | markers, which only say the guest is done |

The probe sends malformed and out-of-contract requests (a foreign interface id,
an unknown method, every wrong slot count, buffer sizes one byte off, rings
nobody initialised or of another size, a missing buffer or endpoint, a second
attach, an unknown rx mode, a wrong ring id), checks the frame-length policy
through the counters, runs an intruder task that must be refused on the owner's
ring, corrupts its own transmit ring (the driver must drop the client, count it
and accept a new one), and finishes by asking the driver a question. The soak
compares the fabric snapshot's handle, endpoint, shared-buffer and mapping totals
before and after 40 cycles: any growth fails it, as does a driver ring error or
a transmit slot that did not come back.

| Layer | What | Run |
|---|---|---|
| Host unit | `framering` (23), `virtio-net` (30), `nicdrv` (31: a fake device and a hostile client, frame boundaries 13/14/1514/1515, receive filter, backpressure, wake-ups, attach validation, ownership, poisoned rings, hostile used entries, tens of thousands of frames each way with slot conservation), `virtio` (23), `netpolicy` (3), generated stubs (7) | `cargo test -p framering -p virtio-net -p nicdrv -p netpolicy -p virtio -p messenger-generated` |
| Seeded fuzz | Long random scripts against a reference model. For `nicdrv` the model covers which frames reach the client and the wire, every counter, wake-ups and slot conservation while nobody misbehaves; once the client scribbles or the device lies, safety and guard pages only | `cargo test -p framering -p virtio-net -p nicdrv fuzz::` |
| Coverage-guided fuzz | libFuzzer on the same entry points (`framering`, `framering_header`, `virtio_net`, `nicdrv`), checked-in seeds | `cargo fuzz run nicdrv --fuzz-dir fuzz fuzz/corpus/nicdrv fuzz/seeds/nicdrv` (Linux) |
| Harness unit | The pcap judge must fail on a missing reply, a wrong payload, the wrong order, a truncated capture, an empty file, frames outside the policy | `python tools/net/test_analyze_pcap.py` |
| End to end | The capture-judged run above | `python tools/net/run.py` |
| Variants | `--services` (supervised as `_net`, uid 902 checked), `--machine q35 --virtio-disk`, `--poll` (interrupts off), `--no-device` (`-nic none`: the driver prints `NETDRV:NODEV` and idles) | see `tools/net/README.md` |
| Kernel | N1: the class rules loaded into the real ACL. N2: syscall 26 (bounds, bad pointers, distinct output, soak), `CLOSE_RELEASE` (channel and syscall level, soak), the call rules for `nic.v1` and `stack.v1` | `python tools/test/run.py --accel none` (589 tests at the last N2 run) |

### N2 evidence

`python tools/net/run.py --netd` builds with `LAZYOS_NETD=1` (which adds
`/system/bin/netd`, `/system/bin/netctl` and `/system/bin/ping` to the N1 image) and boots `netd` with
`demo=1`: it waits for DHCP, then runs `netctl` (info), a real `ping 10.0.2.2 4`,
`netctl probe=1` and `netctl soak=40`. The capture must show at least 6 complete
DHCP exchanges (the start, the probe's renewal, and a renewal every tenth soak
round) and at least 46 echo request/reply pairs with the gateway (4 from `ping`,
2 from the probe, 40 from the soak), each reply after its request with matching
identifier, sequence and payload, with valid IPv4 and ICMP checksums. The soak
compares what `netd` and `netdrv` hold (handles, shared buffers, buffer bytes)
per task before and after, and checks the stack's counters (every ping
answered, nothing dropped, every renewal and reattachment counted).

| Layer | What | Run |
|---|---|---|
| Host unit | `netstack` (32 tests: DHCP, lease validation, static mode, ARP, echo both ways, the ping table and its cap, cancellation, timeouts, head-of-line, ring poison, backpressure, detach/attach) | `cargo test -p netstack` |
| Seeded fuzz | Hostile frames, a gateway that lies about leases, truncated and bit-flipped answers (checksums repaired half the time), a jumping clock; invariants checked every step | `cargo test -p netstack fuzz::` |
| Coverage-guided fuzz | `netstack` target, 9 checked-in seeds | `mkdir -p fuzz/corpus/netstack`, `cargo fuzz run netstack --fuzz-dir fuzz fuzz/corpus/netstack fuzz/seeds/netstack -- -max_total_time=60` (Linux) |
| End to end | The capture-judged run | `python tools/net/run.py --netd` |
| Variants | `--netd --services` (`_netd` uid 903, no caps), `--netd --machine q35 --virtio-disk`, `--netd --poll`, `--netd --no-device` (`netd` prints `NETD:NIC:WAIT` and idles) | see `tools/net/README.md` |

## Sockets and names (N3)

```
nc / nslookup / ping --socket.v1, stack.v1--> netd (smoltcp sockets, DNS) --nic.v1--> netdrv
```

**Where the code is.** `libs/netstack/src/stack/` holds the socket layer, all host
tested: `sockets.rs` (the table, ids, quotas, ports, closing), `tcp.rs` (connect,
listen, accept, send, receive, shutdown, readiness), `udp.rs` (bind, connect, `SendTo`,
`RecvFrom`), `dns.rs` (lookups), `observe.rs` (what the wire did to each stream, once
per poll). `user/src/bin/netd/` adds the service side: `sock.rs` (dispatch and parked
calls), `resolve.rs` (parked lookups), `owners.rs` (who is calling). The client side is
`user/src/messenger/netsock.rs` (one method per call) and `netstd.rs` (`TcpStream`,
`TcpListener`, `UdpSocket`, named after `std::net`). The interface is `idl/net.midl`
(`os.lazy.net.socket.v1`; docs in `docs/idl/`).

**One endpoint, two interfaces.** The socket interface is served on the stack
service's endpoint (`os.lazy.net.stack`, registered with both interface ids); `netd`
tells them apart by id. The receive buffer is 20 KiB so a full 16 KiB `Send` and its
framing fit; a request too large for it (no legal one is) is consumed by the kernel
but unreadable, so it is dropped without a reply and counted
(`NETD:OVERSIZE`), and the sender's own deadline ends the wait. The probe found this:
`netd` used to exit on it.

**Ownership is the sender's task slot.** Messenger stamps a request with the sender's
slot. The owner id is `pid << 16 | slot`, with the pid read from the scheduler's task
list (syscall 13, refreshed at most once per tick, decoded without allocating:
`TaskSnapshot::live_pid`). **Known gap:** the kernel's pid is the slot number, so the
pid adds nothing yet and a task that lands in a dead owner's slot before the sweep has
reclaimed its sockets is taken for that owner. The remedy is a per-slot spawn counter
in the task snapshot (a kernel ABI change with its own tests); the owner id already
has the field for it. Every call on a socket goes through the table's `entry`,
which refuses anyone but the owner (`EACCES`, counted in `not_owner` and logged as
`NETD:DENY` for the first sixteen). A sweep every 20 ticks reclaims the sockets of
owners no longer alive (`NETD:RECLAIM`), so a crashed client leaks nothing; if the
task list cannot be read, nobody is reclaimed.

**Parked calls.** `Connect`, `Accept`, `Send` (buffer full), `Recv`, `RecvFrom` and
`Poll` try the operation first; if it cannot finish, the transaction is kept (at most
4 per owner, 32 in all, `EAGAIN` past either) and retried after every poll of the
stack. A parked call ends with its result, with `ETIMEDOUT` at its deadline (10 ms to
60 s, 0 meaning 60 s), or with `EBADF` if its socket is closed under it. The event
loop's wait is the minimum of smoltcp's next timer, the nearest parked deadline and
the next sweep. A `Connect` that timed out leaves the handshake running; calling again
waits for the same attempt.

**Limits.** 64 sockets, 8 per owner, 256 KiB of buffer each way per stream (the window
is scaled from it; Reno congestion control; a 10 ms delayed ACK; P4.3) (8
datagrams of 1472 bytes for UDP), a listener's backlog is at most 8 smoltcp sockets
(one connection each), at most 32 closed streams finishing in the background (the
oldest is aborted past that, and any stream lingers at most 30 s). Nothing is sized
by a client. Port numbers below 1024 are refused to everyone (`netd` cannot read a
caller's `CAP_NET_BIND` yet); ephemeral ports start at a seeded offset.

**Names.** `Resolve(name, timeout_ms)` is a parked call on `stack.v1`: the name is
validated (`valid_host_name`), a dotted quad is answered at once, anything else goes to
the first resolver DHCP gave through smoltcp's DNS socket, with a deadline of its own.
At most 8 lookups run (4 per caller). `ENOENT` is the resolver saying the name has no
address, `ENETUNREACH` is no address or no resolver. `ping` resolves names through it,
and `nslookup` is a thin client.

**Tools.** `nc [-u] [-l] [-i] [-n] [-x] [-w secs] [-g bytes] <host> <port> [text...]`:
connects (or listens for one connection), sends the text and a newline, prints what
comes back until the peer closes or `-w` idle seconds pass. `-g` sends a deterministic
byte stream and `-x` checks the echo against it (`-l -x` is an echo server). Native
programs have only a blocking `read_char`, no end-of-input, so `-i` relays one typed
line at a time (an empty line ends it) and there is no streaming both ways.

**Access rules.** `libs/netpolicy` now names the `socket.v1` methods and `Resolve`:
every method is open to every caller for now (N6 narrows it per profile); the kernel
test `net_call_rules_decide_who_may_call_what` loads the table into the real ACL.
Ownership and quotas are `netd`'s own and hold whatever the table says.

### N3 evidence

`python tools/net/run.py --netd` goes on after the N2 clients with `nslookup
localhost`, three `nc -x` clients against the harness's echo servers on the gateway
(TCP 47771: a line and 200 000 generated bytes; UDP 47772: a datagram),
`netctl sockprobe=1`, `netctl socksoak=40`, and an `nc -l -x` listener on 47773 that the
harness reaches through a `hostfwd` rule and feeds 150 000 bytes. The verdict is the
capture (`sockets_pcap.py`) next to what the host servers recorded:

* every TCP flow to the echo port has a complete handshake, valid checksums, streams
  that reassemble without a gap, the same bytes echoed back, and a FIN from both sides;
  the set of (length, SHA-256) of the guest's streams equals the server's;
* connections to the closed port (the probe's and the soak's every eighth) are never
  established; resets are reported, not required (QEMU stays silent on some hosts);
* every UDP datagram is echoed with the same payload and the server saw exactly those;
* a well-formed A query for `localhost` left for the resolver (its answer is reported
  when the host network gave one);
* the harness's inbound connection: handshake, its bytes in, the same bytes out.

`python tools/net/test_sockets_pcap.py` feeds the checker broken captures (a flipped
byte, a short echo, a lost segment, a missing FIN, no handshake, a bad checksum, an
unanswered datagram, a query for the wrong name, a server that saw other bytes).

| Layer | What | Run |
|---|---|---|
| Host unit | `netstack` (60 tests): TCP transfer both ways, 100 kB in order, full buffers and recovery, close and shutdown, refusal, ownership, hostile arguments, port reuse, quotas, listener, UDP both ways, truncation, connected UDP, reclaim, 300 connect/close cycles with no leak, 2 000 datagrams, name lookups (answer, NXDOMAIN, silent resolver, literals, hostile names, limits, 200 in a row) | `cargo test -p netstack` |
| Seeded fuzz | Random socket calls (valid and invalid ids, every call in every state) between two stacks, bounds checked every step | `cargo test -p netstack random_socket` (`FUZZ_CASES=N` for longer) |
| Probe | `netctl sockprobe=1`: foreign interface and method, garbage and empty bodies, ids that name nothing, every bad argument, quotas, the parked-call cap and honest timeouts, `Close` under a waiter, a second task refused on every call, an oversized request, a refused connection | in the demo |
| Soak | `netctl socksoak=40`: connect, echo, close, UDP echo, listener open/close and a refused connection per round; sockets open back to the start, opened = closed, byte counters, and `netd`'s handles and buffers unchanged | in the demo |
| Harness unit | The TCP/UDP/DNS judge must fail when it should | `python tools/net/test_sockets_pcap.py` |
| End to end | The capture-judged run with the host servers | `python tools/net/run.py --netd` |

### What testing found (N3)

* The first part of N3 had landed `sockets.rs` and `tcp.rs` without wiring them into
  the stack (no `mod`, no table field, no UDP half), so nothing had compiled them.
  The back-to-back host tests (`testpair.rs`) are what made them real.
* A closing stream was only reaped once smoltcp said `Closed`; the side that closes
  first sits in TIME-WAIT, so every socket the soak closed stayed in the closing list
  until the 32-entry cap aborted the oldest. `reap_closing` now treats TIME-WAIT as
  done (`retire` already did).
* The probe's 48 KiB `Send` made `netd` exit: a request larger than the receive buffer
  fails the `recv` with `E2BIG` after the kernel has consumed it, and the loop treated
  every receive error as fatal. It now counts the request, logs `NETD:OVERSIZE` and carries on.
* Slots are reused, so a socket keyed by the sender's slot can pass to the next task
  there. Review caught that the pid does not prevent this (it is the slot); the
  owner id is shaped for a spawn counter but the kernel does not supply one yet.
* On this Windows host QEMU's user networking never answers a SYN to a closed host
  port, where Linux hosts reset it. A connection that must fail therefore ends in
  `ECONNREFUSED` or `ETIMEDOUT`, and the capture check requires only that none was established.
* An empty `Open` body is a valid stream `Open` (all fields default); the probe had
  wrongly expected it to be refused.

## The FTP client (N4)

`ftp <host>[:port] [user=NAME] [pass=SECRET] [cmd ; cmd ...]` (`user/src/bin/ftp.rs`,
`ftp/session.rs`) is a passive-mode, binary-only client over `TcpStream`. Commands:
`pwd`, `cd`, `ls`, `size`, `get <remote> [- | !]`, `put <local> [remote]`,
`put -g <bytes> <remote>`, `quit`; with no commands a prompt reads lines (a failed
command does not end the session). `-q` silences the dialogue.

**Everything a server controls goes through `libs/ftpwire`.** `ReplyParser` takes
bytes in any chunking and returns whole replies, multi-line ones included; a line over
1024 bytes, a reply over 64 lines, a malformed code, a different code inside a
multi-line reply or a bare CR is an error and the parser stays failed (a control
connection that lost sync is not trusted again). Reply text is reduced to printable
ASCII before it reaches the console. `parse_pasv` accepts only a well-formed
`(h1,h2,h3,h4,p1,p2)`, and the client **ignores the address in it**: the data
connection goes to the control peer, so a server cannot send the client to a third
host. `command()` refuses an argument holding CR, LF or NUL, so a file name cannot
carry a second command.

**Files.** Native programs have no file-write syscall. `get x -` writes the bytes to
standard output (redirect it), `get x !` counts and checksums them, and `put local`
reads a file with `read_file` (up to 4 MiB) while `put -g N name` sends the `nc -g`
stream. End of a download is the server closing the data connection; end of an upload
is the client's FIN, after which the final `226` is read. Markers: `FTP:LOGIN`,
`FTP:GET name bytes=N crc=...`, `FTP:PUT ...`, `FTP:LS bytes=N`, `FTP:PASS` or `FTP:FAIL`.

### N4 evidence

`netd demo=1` runs the client against `hostpeers.FtpServer` on the gateway (port
47780, a small passive server that records every command and every transfer):
`pwd ; cd pub ; cd / ; ls ; get hello.txt ! ; get big.bin ! ; put -g 150000 up.bin ;
size up.bin ; get up.bin ! ; quit`. `sockets_pcap.check_ftp` requires: one control
connection, closed from both sides, whose client commands equal the server's record
(and the harness's expected list); one data connection per recorded transfer, to its
passive port, whose bytes in the transfer's direction equal the file (and nothing
flows the other way), closed by FINs from both sides; no connection to any port that is
not part of the session. The CRC-32s the client printed must match the bytes the
harness knows. Tests: `cargo test -p ftpwire`, `python tools/net/test_sockets_pcap.py`
(the checker fails on a flipped byte, a short upload, an injected command, a missing
FIN, a transfer with no connection, an unrelated connection, bytes the wrong way).

## Linux sockets (N5)

```
musl / std::net / BusyBox --socket(AF_INET)--> kernel InetSock <-- syscall 27 --> netd --> netstack --> netdrv
                                        (side B of a ring pair)    (side A; the pump's doorbell wakes netd)
```

Static musl programs issue raw `socket`/`connect`/`sendto` syscalls, so the kernel has
to front `netd` (docs/networking-plan.md §7.2). This is the plan's option **L2**: the
kernel holds a socket object whose data path is the existing `SocketPair`, and `netd`
serves the other side. Per-call proxying (L1) was rejected for four reasons that the
tree confirmed: `poll`/`epoll` rescan `Fd::poll()` synchronously and could not round-trip
to a service; `netd` keys sockets by the calling task, so a forked child (which `nc -l -e`,
`httpd`, `telnetd` and `ftpd` all use) would get `EACCES`; `Fd::drop` cannot block, so a
`Close` could not be a call; and a blocking call inside a syscall cannot be tested in the
in-kernel suite, which has no scheduler.

**Where the code is.**

| Path | Role |
|---|---|
| `kernel/src/ipc/inet/mod.rs` | The table of up to 64 sockets (id = slot plus generation), the request queue, `reset` |
| `kernel/src/ipc/inet/sock.rs` | `InetSock`: states, `begin_*`/`finish` for the control calls, accept queue, `poll_gen`, close on drop |
| `kernel/src/ipc/inet/pump.rs` | What `netd` calls: `next_request`, `complete`, `accepted`, `net_read`, `net_write`, `net_eof`, `net_error`, `close_ack`, and the wire form of a request |
| `kernel/src/process/inetsys.rs` | Native syscall 27, the userspace face of the pump |
| `kernel/src/process/linux/inet.rs` | `socket`, `bind`, `connect`, `listen`, `accept`/`accept4`, `shutdown`, `getsockname`/`getpeername`, `sendto`/`recvfrom`, `read`/`write` for `AF_INET` |
| `kernel/src/process/linux/sockopt.rs`, `kernel/src/ipc/inet/timeout.rs` | `setsockopt`/`getsockopt`; `SO_RCVTIMEO`/`SO_SNDTIMEO` stored in ticks |
| `kernel/src/ipc/inet/bell.rs` | The pump's doorbell: what an application does that `netd` must act on wakes it (P4.1) |
| `kernel/src/ipc/pipe/small.rs` | 256 KiB rings, a TCP window each (a socket's pair is 512 KiB, the cap is 128 rings, 32 MiB of a heap that grows on demand); which end rings the doorbell |
| `user/src/bin/netd/inet.rs`, `inet/flow.rs`, `user/src/sys/inetpump.rs` | The pump in `netd` and its syscall wrapper |
| `tools/abi/fixtures/src/netfix.rs` | The Linux `std::net` program the shim is judged by |

**Control calls are requests.** `bind`, `connect` and `listen` queue a request and park on
the socket's wait queue; `netd` fetches the request (`NEXT`), runs it against the stack, and
answers (`COMPLETE`) with the status and the addresses the stack chose. A connection's data
path (a `SOCK_STREAM` pair, or `SOCK_SEQPACKET` for a datagram socket) appears with the
answer. A non-blocking `connect` returns `EINPROGRESS` and completes in the background,
reported through `poll` and `SO_ERROR`; an interrupted or timed-out call leaves the request
running (`EALREADY` on a retry). Connections `netd` accepts are handed in (`ACCEPTED`) and
wait in the listener's queue (16 at most).

**Bytes.** The application reads and writes side B with the ordinary socket code, so `read`,
`write`, `poll`, `epoll` (edges included), `dup`, `fork` and `shutdown` need nothing from
`netd`. **No timer** (docs/performance-plan.md P4.1): the kernel rings the pump's doorbell
(`ipc/inet/bell.rs`) when a request is queued, when the application writes into an empty send
ring, reads from a receive ring with under 2 KiB free (the only case in which `netd` can be
holding bytes for it) or drops either ring; `netd`'s own reads and writes never ring.
`netd` parks in `wait_any` on its endpoint and the bell (`WAIT_INET`, only the attached task
may arm it). **Draining in place** (P4.2): `netd` reads side A straight into the stack's free
send space and writes the stack's queued bytes straight into side A
(`Stack::socket_send_with`/`socket_recv_with`), so nothing is staged or allocated and nothing
leaves one side that the other cannot hold; each direction loops until a side is exhausted or
the socket's 256 KiB budget for the pass is spent, and a pass that moved bytes is followed by
another at once (what is left rings no bell and may bring no frame). One syscall-27 `READ`
fills `netd`'s buffer in place and may move 4 MiB; one Linux `read`/`write` moves up to a whole
ring (P4.4). A datagram socket's messages carry the peer's address in front (6 bytes),
so `sendto`/`recvfrom` prepend and strip it. Closing the last descriptor queues a close; `netd`
first sends the bytes the application wrote, then closes the stack socket and acknowledges, and
the kernel frees the slot only then.

**Authority.** Syscall 27 is for the stack alone: `ATTACH` needs uid 0 or `_netd` (903),
and every other op needs the attached task. Attaching again (a restarted `netd`) discards every
socket of the old one: their applications read end of stream and get `EPIPE`.

**Timeouts.** `SO_RCVTIMEO` and `SO_SNDTIMEO` behave as on Linux (docs/tls-plan.md §5.4): a
`struct timeval` whose `tv_usec` is outside `0..1_000_000` is `EDOM`, a short `optlen` `EINVAL`,
`{0, 0}` means none, a negative `tv_sec` gives up at once, and the value is kept rounded up to
whole 10 ms ticks, which `getsockopt` reports. A blocking `read`/`recv`/`recvfrom` (UDP too) or
`accept` that waits longer returns `EAGAIN`, a blocking `write`/`send`/`sendto` on a full ring
returns `EAGAIN` (or the bytes it did write), and a blocking `connect` returns `EINPROGRESS`
while the connection goes on. The deadline is fixed when the call starts; a signal still ends
the wait with `EINTR`; non-blocking sockets are unaffected.

**Limits and gaps.** Per-call `MSG_DONTWAIT` and `MSG_PEEK` are ignored (the descriptor's
`O_NONBLOCK` is honoured); `sendmsg`/`recvmsg`, `select` and `ppoll` are not implemented (musl's
resolver and `std` do not need them here); `setsockopt` accepts the usual options and ignores
them, except `SO_RCVTIMEO` and `SO_SNDTIMEO`; `AF_INET6`, raw sockets and netlink are still `EAFNOSUPPORT`; port numbers below 1024 are
refused by `netd` as for Messenger clients; a `netd` that dies leaves open sockets to see end
of stream only when the new one attaches. **Speed** (`python tools/net/bulk.py`,
[`docs/perf/network.md`](../perf/network.md); WHPX, dev profile, user networking): a `connect` to
the gateway takes 0.5-1 ms (the first after a program starts 1-10 ms), and bulk TCP runs at about
70 MB/s guest to host and 130-190 MB/s host to guest (before P4: 10-13 ms per `connect`, and
about 50 MB/s and 60 MB/s on a quiet host). Delayed ACKs and retransmission timers still round
to `netd`'s 10 ms clock until P2 gives it a finer one.

### N5 evidence

* **In-kernel (`LAZYOS_TEST_FILTER=inet`)**: 24 tests under `kernel/src/tests/linux_suite/inet_*.rs`
  with a fake `netd` standing where a sleeping caller would wait. Correctness: request and state
  machine, the wire form, the pump's authority and hostile input (stale ids, calls out of order,
  bad pointers), every syscall's argument checks, a TCP client and a refused connection, a
  non-blocking connect (success and failure), listen/accept/accept4, the bounded accept queue,
  datagrams (addresses, truncation, connected UDP), `poll` and edge-triggered `epoll`, socket
  options, `dup` and `close`. Stress: 3 000 connect/exchange/close rounds, 5 000 datagrams of every
  length, a megabyte each way through the rings byte for byte, a close right after a write,
  a `netd` restart with sockets open, and six seeds of 2 000 random calls with bounds checked at
  every step; each test ends by checking that no descriptor, socket slot, queued request or ring
  was left.
* **End to end (`python tools/net/run.py --netd`)**: `netd demo=1` runs `/system/bin/netfix` under the Linux personality, a static
  musl Rust program using only `std::net`: a 200 000-byte echo through a duplicated socket and a
  half-close, a timed (non-blocking) connect, a connect that must fail, address queries, 22 UDP
  echoes including the 1472-byte limit, and a server that accepts a connection the harness opens
  through a port forward and echoes 100 000 bytes. The capture is judged with the same checks as
  the native tools: the extra TCP flows and datagrams are in the server's record byte for byte, and
  the inbound connection carries the harness's bytes both ways.

## Not done

BusyBox `nc`/`wget`/`ftpget` as clients of the shim (no BusyBox in the local build; the CI bench has one), `/etc/resolv.conf` and `/etc/hosts` for musl's resolver, sockets used from a second thread (the shim gives a thread a descriptor table of its own: `thread::spawn` after `socket` cannot share it), `sendmsg`/`recvmsg`/`select`/`ppoll`, `MSG_DONTWAIT`/`MSG_PEEK`; per-profile tightening of the
socket rules and a policy loader (the ACL refuses nothing today, so `Renew` and
`Reattach` are callable by anyone); `CAP_NET_BIND` (nobody binds a port below
1024) and `CAP_NET_RAW`; loopback (a socket cannot connect to this machine's own
address); IPv6; a shared-ring data plane (bytes travel in parcels, one copy each
way); `Poll` over many sockets in one call; a request-size limit that answers
instead of dropping (a request over about 20 KiB is consumed by the kernel and
cannot be read, so the sender waits for its own deadline); a parked `Ping` or
`Resolve` is not released when its caller cancels or dies (`netd` learns of the end only when the stack
reports a result, so an abandoned one holds a slot until its timeout, at most
60 s, which two callers can use to make others see `EAGAIN` for that long; parked
*socket* calls are tidied when their owner is reclaimed); the host-dependence of
the refused-connection evidence (QEMU's user networking answers a connection to a
closed host port with a reset on Linux and with silence on Windows, so the
harness requires only that no such connection was established); a shared module
for the PCI bring-up that `sndd` and `netdrv` both carry, and `sndd`'s
`discard_transfers` leaving extra transferred handles open; `devd` (the driver is
started by `init`'s manifest or the kernel directly); MSI/MSI-X (INTx only);
checksum/segmentation offload and jumbo frames; a second NIC driver (e1000); a
fully tickless serve loop (with the line armed the loop wakes every 20 ticks only
for the link poll, the client's keep-alive and the configuration refresh; the
2-tick wake while a client was attached went in P4.5); `EVENT_IDX` and merged
receive buffers (P4.6: about one interrupt per 30 received frames already under
bulk load, so not yet worth it).
