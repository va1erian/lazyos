# dbgd: remote inspection of a LazyOS box (issue #701)

A PC with no serial port was debugged by filming its screen. `dbgd` answers
"what is the state of xHCI slot 2?" and "what did `usbd` print in the last
minute?" over the network card instead: one TCP port, newline-delimited
JSON-RPC 2.0, behind a pre-shared key. **v1 is read-only**; the v2 control
tier (restart and service hot reload) exists only in images built with
`LAZYOS_DBGD_CONTROL=1`.
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
| `LAZYOS_DBGD_CONTROL=1` | `diag.dbg.control=1`: the control tier below. Off by default. |

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
apart so a chatty program cannot push the boot out of the boot log. Op 3 is
refused (`EPERM`) to every uid but root and `dbgd`'s (911): service output can
hold anything a service logs, unlike the boot log (open like `dmesg`).

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

## v2: control and service hot reload

Stages 1 (restart only) and 2 of the issue. On a real box the loop becomes:
change a driver, `cargo build` with the box's switches, then

```bash
python tools/dbg/dbgctl.py --host <box> reload usbd       # /system/bin/usbd of target/lazyos.img
python tools/dbg/dbgctl.py --host <box> reload usbd my.elf --trial-ms 20000
python tools/dbg/dbgctl.py --host <box> restart usbd | revert usbd | reloads
```

with no stick write. `reload` waits for the verdict and exits non-zero on a
rollback. The MCP bridge has the same as `service_reload`, `service_restart`,
`service_revert` and `service_reloads`.

### The switch and the gate

| Layer | What it takes |
|---|---|
| Build | `LAZYOS_DBGD_CONTROL=1` writes `diag.dbg.control=1`; without it the line is absent |
| `dbgd` | `Config::control`; a `Control` method is `-32002` unless the box allows it **and** the session called `control.begin {"confirm":"control"}` (per connection: a reconnect starts read-only; the opening is a `DBGD:SECURITY ... control opened` record) |
| `init` | `ReloadService`, `RevertService` (`idl/init.midl`) accepted only from `dbgd`'s kernel-stamped identity (uid 911, unlabelled, no session) **and** when `init` itself reads `diag.dbg.control=1` in `lazyos.cfg` |
| Both | `dbgwire::control::reloadable`: a plain name, never `messengerd` (it claims the bootstrap channel once per boot) or `dbgd` (the connection watching the reload) |

### Methods

| Method | Does |
|---|---|
| `control.begin {confirm}` | open control for this connection |
| `service.restart {name}` | restart a manifest service as it is (`init` kills it and starts it at once) |
| `service.upload {name, offset, total, data}` | one chunk (6 KiB, base64; its encoding is exactly the JSON reader's longest string) into `fhs::state::DBGD_STAGE/<name>.elf`; chunks in order, offset 0 starts over, 32 MiB at most |
| `service.reload {name, sha256, trial_ms}` | hand the finished upload to `init`; `trial_ms` 1000..120000, default 10000 |
| `service.revert {name}` | the image's binary again |
| `service.reloads` | per service: `trial`, `committed`, `rolled-back` or `reverted`, the sha256, pid, and a rollback's reason (read tier) |

### What `init` does (`user/src/bin/init/reload.rs`)

1. Checks the caller, the box's switch and the name; refuses a row still
   `pending`, a shutdown in progress, or a launched app (`EPERM`, `ENOENT`,
   `EAGAIN`).
2. Reads exactly `dbgd`'s staging file for that name, checks its SHA-256
   against the one the client computed and that it is an ELF, and copies it
   to `fhs::state::INIT_RELOAD/<name>` (root-owned, ramfs). `dbgd` cannot
   swap the file after the check: `init` runs its own copy.
3. Kills the running task; the exit is recognised as the reload's own and
   the row is started again at once from the copy, with its manifest
   arguments and credentials. A row that was `failed` or `stopped` (a driver
   that crashed out of its restarts) is simply started: reloading a fix
   into a dead driver is the main use.
4. **Trial**: if the new run exits, or the spawn fails, before `trial_ms`,
   `init` drops the copy and starts the image's binary at once
   (`INIT:RELOAD:ROLLBACK reason=...`); still running at the deadline, it
   commits (`INIT:RELOAD:COMMIT`). The rollback is `init`'s, so it happens
   even when the reloaded service carried `dbgd`'s connection (`netdrv`,
   `netd`); the client reconnects and reads `service.reloads`.

A reload lives on the ramfs: a reboot always comes back to the stick's
image, which is the fallback a bad driver cannot break. "Healthy" is "still
running at the deadline", not `healthd`'s verdict: a service that runs but
misbehaves is the developer's to see in the log and `revert`.

### Security

This is remote code execution by design: anyone with the key on a
control-enabled box can run any program with the credentials of any service
but the two excluded (the platform services run as root). It is a separate
build switch, off by default and absent from `--dbgd` images; the switch is
checked by `init` on the box, not taken from `dbgd`; each connection opens
control on purpose; every step is a `DBGD:AUDIT` line and an `INIT:RELOAD:*`
line in the program-output ring. There is still no TLS: the binary crosses
the network in the clear, and the HMAC handshake authenticates the
connection's start, not each request. Never build a control image for a
machine on a network you do not trust.

### Apps: install and relaunch

Apps are not `init` manifest rows: they are packages `pkgd` installs and
`init` launches into a session. Swapping one is therefore a package install
and a relaunch, not a binary reload:

```bash
python tools/dbg/dbgctl.py --host <box> app-install target/pkg/core/os.lazy.calc-0.1.0.lzp
python tools/dbg/dbgctl.py --host <box> relaunch os.lazy.calc
```

| Method | Does |
|---|---|
| `app.upload {offset, total, data}` | one chunk of an `.lzp` into `dbgwire::control::staged_package_path()` (256 MiB at most) |
| `app.install {sha256, relaunch}` | `pkgd.InstallDebug`, then (unless `relaunch` is false) `init.RelaunchApp` for the installed `system_name`; answers the name, version, install directory, `stopped`, `started` |
| `app.relaunch {app}` | `init.RelaunchApp` alone |

`pkgd.InstallDebug` (`idl/pkgd.midl`) takes the same gate as a reload
(`dbgd`'s identity, `diag.dbg.control=1` read by `pkgd` itself), only the
staging file, and only bytes that hash to the client's digest; then it is an
ordinary install (validation, policy, MIME, audit), except that it may
replace a core app, which is what iterating on one needs.
`init.RelaunchApp` (`user/src/bin/init/relaunch.rs`) kills every running
instance and launches each again through `Launch`, in its own session with
the document it was opened on, so it comes back from the new install with
its label and policy. There is no trial: `pkgd` refuses an invalid package,
and a broken build is replaced by installing the previous `.lzp`.

### Not done (stage 3 and the rest of stage 1)

`fs.write` to a scratch area, spawning a program with captured output,
register writes, and the whole-OS reload (a kexec-style handoff with the PCH
TCO watchdog): stage 3's feasibility questions (device quiescing, what state
survives, the bootloader's boot-info) are still open.
