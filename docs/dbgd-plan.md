# dbgd: remote inspection of a LazyOS box (issue #701)

A PC with no serial port was debugged by filming its screen. `dbgd` answers
"what is the state of xHCI slot 2?" and "what did `usbd` print in the last
minute?" over the network card instead: one TCP port, newline-delimited
JSON-RPC 2.0, behind a pre-shared key. **v1 is read-only.**
How-to and troubleshooting: [`dbgd.md`](dbgd.md).

```bash
python tools/run_demo.py --dbgd                       # QEMU: networking + dbgd, 9701 forwarded
python tools/dbg/dbgctl.py log --follow               # the kernel boot log, live
python tools/dbg/dbgctl.py log --source programs      # what usbd, netdrv, ... printed
python tools/dbg/dbgctl.py tasks | usb | devices | drivers | hw | msg-services
python tools/dbg/run.py [--usb]                       # build, boot, judge every method
python tools/mcp/debug_bridge.py --connect HOST       # the same data as MCP tools
```

A real box: build the stick with `LAZYOS_DBGD=1` (plus `LAZYOS_NETD=1` and the
NIC driver the box needs), read `target/dbgd.key`, and
`dbgctl --host <box ip> --key <hex> ...`. The key is also in the stick's
`lazyos.cfg`.

## Switches (compiled in or out)

| Switch | What it does |
|---|---|
| `LAZYOS_DBGD=1` | The kernel's two log ops (syscall 14 ops 2 and 3, `cfg(lazyos_dbgd)`), the program-output ring, the `dbgd` binary in `/system/bin`, its row in `init`'s manifest (uid 911, no capability), its `diag.dbg.*` lines. Needs `LAZYOS_NETD=1`. |
| `LAZYOS_DBGD_KEY=<hex>` | The key (16 to 64 bytes of hex). Unset: one is made once and kept in `target/dbgd.key`. |
| `LAZYOS_DBGD_PORT`, `LAZYOS_DBGD_PEER` | TCP port (9701); the only IPv4 address that may connect. |

Without `LAZYOS_DBGD=1` none of it exists: a kernel built without it answers
syscall 14 op 2 with `-EINVAL` (the kernel suite checks both ways, and the
program-output ring is not even allocated), no `dbgd` binary or manifest row
is in the image, and `lazyos.cfg` has no `diag.dbg` line. With it, `dbgd`
still runs only when `lazyos.cfg` says `diag.dbg=1` with a valid key: no key,
no service (`DBGD:OFF <why>` on the console).

## Protocol

One JSON object per line, at most 16 KiB. On connect the server sends

```json
{"jsonrpc":"2.0","method":"hello","params":{"server":"dbgd","proto":"lazyos-dbg/1","nonce":"<32 hex>","auth":"hmac-sha256","uptime_ms":123}}
```

and the client has 10 s to answer
`{"jsonrpc":"2.0","id":1,"method":"auth","params":{"mac":"<hex>"}}` with
`mac = HMAC-SHA256(key, "lazyos-dbg/1 client" || nonce)`. The result carries
`server_mac = HMAC-SHA256(key, "lazyos-dbg/1 server" || nonce)`, which the
client must check: the box proves it holds the key too. One failed attempt
closes the connection and delays the next by 1 s, doubling to a minute. No
encryption in v1 (TLS comes when `nettls` is usable on a listener); the
handshake keeps the key off the wire and a recording useless.

Errors: `-32700` parse, `-32600` invalid request, `-32601` unknown method,
`-32602` bad params, `-32001` not authenticated, `-32002` denied by policy,
`-32003` the data source is not there, `-32004` locked out.

## Methods (`libs/dbgwire/src/methods.rs`)

| Method | Returns |
|---|---|
| `auth`, `ping`, `methods` | handshake, uptime, this table |
| `log.tail {lines, source}` | `kernel` (the boot log), `programs` (what services printed: `USBD:`, `NETDRV:`...), or a `logd` journal; every line split into `tag`, `fields`, `text`, `pos` (byte offset) |
| `log.sources`, `log.follow {lines, source}`, `log.unfollow` | journals; a stream of `log` notifications with `cursor` and `dropped` (bytes the ring overwrote before they were read) |
| `tasks.list`, `sysinfo`, `mem.stats`, `fabric.stats` | the scheduler table, the kernel statistics snapshot, its memory counters, the Messenger fabric counters |
| `devices.list`, `drivers.list` | the PCI inventory with owner and rights (`devctl devices`); `devd`'s match, driver and state (`devctl drivers`) |
| `usb.dump` | `usbd`'s last snapshot: `USBD:DUMP:HC` (USBCMD, USBSTS, CRCR, DCBAAP, CONFIG, command and event ring pointers), `USBD:DUMP:PORT` (PORTSC fields), `USBD:DUMP:DEV` (slot state and address, endpoint 0 state and dequeue pointer, ids) |
| `msg.registry`, `msg.services`, `msg.topics`, `msg.topic {topic}` | Messenger: registered names with owners and interfaces; `init`'s supervised services; the broker's topics; one exact topic's retained value |
| `fs.read {path, offset, len}` | a file under `/transient`, `/tmp`, `/logs`, `/system/etc`, `/system/share`, `/docs` (never the shadow file, `/conf`, `/accounts` or `lazyos.cfg`); at most 32 KiB; a directory lists its names |
| `hwreport` | the `HW:*` verdict lines of the boot log, as fields |

Not in v1 (the issue's list): `pci.config` and `mmio.read` need a driver's
claim on the device, which `dbgd` deliberately does not hold; `usb.dump`
carries the registers `usbd` itself owns instead. Both are v2 candidates
(config-space reads for unclaimed functions through the kernel).

## Where the data comes from, and why a program-output ring

The boot log (`klog`) holds only what the *kernel* prints. Programs'
output (`sys::write_str`) is mirrored to the serial port and the console
panes but was never kept: the `USBD:DIAG` line of the bring-up existed only
on a screen. A `LAZYOS_DBGD=1` kernel keeps it in a second 64 KiB ring
(`kernel/src/klog.rs`, written by `serial::mirror`, read by op 3), kept
apart so a chatty program cannot push the boot out of the boot log.

`usbd` rewrites `/transient/usbd.dump` (at most every 2 s, when the bus
changed); `dbgd` serves the file. Everything else is read through the
interfaces `top`, `devctl` and `messengerctl` already use, as `dbgd`'s
own uid with no capability.

## Security and threat model

This is remote inspection, and v2 is remote code execution by design, so v1
is built to be refused by default:

* **Absent from release images**: the build switch above; nothing of it is
  compiled in without it.
* **Off unless configured**, and **no key, no service**.
* **Authenticated**: HMAC challenge, fresh nonce per connection, constant-time
  compare, mutual proof, lockout with growing delay (shared across connections).
* **Peer-restricted** (`diag.dbg.peer`) when set; checked before the nonce.
* **Least authority**: uid 911, no capability, read-only calls; `fs.read` is
  an allowlist of roots minus a list of secrets, on top of the VFS's own
  permissions.
* **Bounded input**: line, JSON depth, element count and string length caps;
  the parser and method table are `libs/dbgwire`, host-tested and fuzzed
  (`fuzz/fuzz_targets/dbgwire.rs`).
* **Audited**: every request is a `DBGD:AUDIT peer= method= outcome=` line and
  every refusal a `DBGD:SECURITY peer= reason=` line, written to the console
  and kept in the program-output ring (so `log.tail source=programs` shows
  who asked what). Values from the network cannot forge a field.

What it does **not** protect against: anyone who can read the stick (the key
is in `lazyos.cfg` on the FAT volume); anyone on the path between the box and
the client, who can read the log in the clear and take over an authenticated
session (no TLS yet). Treat a `LAZYOS_DBGD=1` image as a development machine.

Known gaps against the issue text: the audit trail is the console/ring lines,
not yet a `system/events/security/dbgd` record journalled by `logd`; that
needs a MIDL topic declaration (`idl/`, generated code, `messengerd`'s
publish filter) and is the next change. Per-service `logd` journals are
readable only for the sources `logd` lets a non-root uid see.

## Tests

```bash
cargo test -p dbgwire                              # protocol, config, allowlist, seeded fuzz
cargo test -p build-support-tests dbgd             # the build's lines parse back
python fuzz/gen_corpus.py --check                  # the checked-in seeds are current
LAZYOS_DBGD=1 LAZYOS_NETD=1 LAZYOS_TEST_FILTER=sysinfo python tools/test/run.py --accel none   # kernel ops
LAZYOS_TEST_FILTER=sysinfo python tools/test/run.py --accel none                                # compiled out
python tools/dbg/run.py [--usb]                    # QEMU: every method, the refusals, the live stream, the MCP bridge
cd tools/lazygui && python -m unittest test_catalog_dbgd
```

## v2 (not started): control and hot reload

As the issue describes (`diag.control=1`, per-session confirmation, service
upload with `healthd` rollback, then whole-OS reload). The method table's
`Access` enum is where a `Control` tier goes, and `libs/dbgwire` has no write
method for a test to confuse with one.
