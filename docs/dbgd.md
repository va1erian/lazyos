# Using dbgd: inspect a LazyOS box over the network

`dbgd` lets you (or an AI agent) read a running LazyOS machine without a serial
port or a camera: the live boot log, what each service printed, tasks, memory,
PCI devices, their configuration space and (allowlisted) registers, driver
state, a USB controller snapshot and Messenger's registry.
This is the how-to; the design, protocol and threat model are in
[`dbgd-plan.md`](dbgd-plan.md). Inspection is read-only; an image built with
`LAZYOS_DBGD_CONTROL=1` can also restart and hot-reload services (section 5).

## 1. Put it in an image

`dbgd` is in no image unless asked for, and needs the network stack:

```bash
python tools/run_demo.py --dbgd            # QEMU: networking + dbgd, host port 9701 forwarded
```

For a real PC, build the stick with the switch (the NIC driver for the box
comes with `LAZYOS_NETD=1`):

```bash
LAZYOS_DESKTOP=1 LAZYOS_USB_IMAGE=1 LAZYOS_NETD=1 LAZYOS_DBGD=1 cargo build
python tools/boot/write_stick.py --device \\.\PhysicalDriveN --yes
```

The first build with the switch makes a random key and saves it in
`target/dbgd.key` (a build message says so). Set your own with
`LAZYOS_DBGD_KEY=<32..128 hex chars>`; `LAZYOS_DBGD_PORT` and
`LAZYOS_DBGD_PEER=<ip>` change the port and restrict who may connect.

Boot the box. Once DHCP gives it an address, the console shows
`DBGD:READY port=9701 peer=any`. Anyone who can read the stick can read the
key (it is in `lazyos.cfg`), so use this on development machines only.

## 2. Look at it

```bash
python tools/dbg/dbgctl.py --host 192.168.1.50 log --follow     # kernel log, live
python tools/dbg/dbgctl.py --host 192.168.1.50 log --source programs --lines 200
python tools/dbg/dbgctl.py --host 192.168.1.50 tasks
python tools/dbg/dbgctl.py --host 192.168.1.50 usb              # xHCI regs, ports, slots
```

`--host` defaults to 127.0.0.1 (QEMU) and the key to `target/dbgd.key`;
`--key HEX` overrides it. `--json` prints the raw result.

| Command | Shows |
|---|---|
| `log [--follow] [--lines N] [--source kernel\|programs\|<logd journal>]` | the boot log; what services, apps and the kernel's traces (`DEV:`, `BOOT:PHASE`, `LABEL:DENY`, `TEST:`) printed |
| `tasks`, `sysinfo`, `mem`, `fabric` | scheduler, kernel statistics, memory, Messenger counters |
| `devices`, `drivers` | PCI inventory with owner and rights; `devd`'s match and driver state |
| `usb` | `USBD:DUMP:HC` / `PORT` / `DEV`: controller registers, port status, slot and endpoint 0 state |
| `pci-config BDF [OFFSET [LEN]]` | read-only PCI configuration space of a function (`devices` shows the BDFs): the 64-byte header, all 256 bytes on an image built with `LAZYOS_DBGD_MMIO=1`; no claim, works while a driver owns the device |
| `mmio BDF BAR OFFSET [LEN]` | read-only 32-bit register reads of allowlisted BAR ranges; needs an image built with `LAZYOS_DBGD_MMIO=1` (below) |
| `hw` | the `HW:*` verdicts of the boot log |
| `msg-registry`, `msg-services`, `msg-topics`, `topic NAME` | Messenger: names, supervised services, topics, one retained value |
| `cat PATH` | a file under `/transient`, `/tmp`, `/logs`, `/system/etc`, `/system/share`, `/docs` |
| `methods`, `call METHOD '{"k":v}'` | the method table; any method by name |

Every log line is a record: `tag` (`USBD:DIAG`), `fields` (`port=1`,
`ep0dq=0x1f000`), `text` and `pos` (byte offset). Filter on tags instead of
scraping text. The line grammar is `libs/tracefmt`'s (the emitters build
lines with it, `dbgd` parses with it), and [`trace/channels.md`](trace/channels.md)
lists every tag prefix, which tree emits it and which harness judges it.

A trace can be compiled out (`LAZYOS_TRACE=0`) or masked (`trace.mask=` in
`lazyos.cfg`, `LAZYOS_TRACE_MASK`); `sysinfo` shows the channels the kernel
prints as `trace_channels`, so a masked channel can be told from a silent one
([`tracing-plan.md`](tracing-plan.md)).

### Reading hardware (`pci-config`, `mmio`)

```bash
python tools/dbg/dbgctl.py --host 192.168.1.50 devices                 # BDFs: 00:02.0 8086:5916 ...
python tools/dbg/dbgctl.py --host 192.168.1.50 pci-config 00:02.0 0 256
python tools/dbg/dbgctl.py --host 192.168.1.50 mmio 00:02.0 0 0x70008 16   # BDF BAR OFFSET LEN
```

Config space needs `diag.dbg=1` for the 64-byte header; bytes 0x40..0x100 and MMIO need the box built with
`LAZYOS_DBGD_MMIO=1` (`run_demo.py --dbgd-mmio`, or the launcher's *dbgd
register reads* checkbox) because a register read can wedge hardware, and it
only answers inside ranges listed for that exact `vendor:device` and BAR:
the built-in table in `libs/pciprobe/src/builtin.rs`, plus
`LAZYOS_DBGD_MMIO_ALLOW=8086:5916:0:0x45400-0x45410` (comma separated, also
`--dbgd-mmio-allow`) which becomes `diag.dbg.mmio.allow=` lines. A refusal
names the allowed ranges. A device a driver holds is refused too, unless
`LAZYOS_DBGD_MMIO_CLAIMED=1` (`--dbgd-mmio-claimed`). Reads are rate limited
to about 256 KiB/s; the design and what is deliberately not reachable (RAM,
I2C, writes) are in [`dbgd-plan.md`](dbgd-plan.md#hardware-reads-pciconfig-and-mmioread).

## 3. Let an agent use it (MCP)

```bash
pip install mcp
claude mcp add lazyos-dbg -- python tools/mcp/debug_bridge.py --connect 192.168.1.50:9701
```

Start a new Claude Code session so the tools load: `log_tail`, `list_tasks`,
`fabric_stats`, `devices`, `drivers`, `usb_dump`, `hwreport`, `pci_config`, `mmio_read`,
`messenger_registry`, `messenger_services`, `messenger_topics`, `fs_read` and
`dbgd_call`. The `lazyos-dbg` project skill (`.claude/skills/lazyos-dbg`)
tells an agent how to use them for bring-up and driver debugging.

### A window or the taskbar freezes: `UI:STALL`

`dbgd` cannot see what a task waits on, so every `xui-app` program times the
work that can hold its UI thread (`xui-app/src/stall.rs`) and prints a line
for anything over 100 ms (a loop pass or a present, 150 ms):

```
UI:STALL kind=call ms=1840 at=123456 app=lazyshell thread=main what=iface=0x... method=3 deadline=... result=-110
```

`kind` is `call` (one Messenger call: interface id and method, so the
service), `connect` (a service name that would not resolve), `chore` (one
step of the shell's heartbeat, by name), `input`/`update`/`present` (the
backend's parts of a pass), `pass` (the whole pass) or `scan` (the shell's
desktop-folder scan on its worker thread, with the time per filesystem call:
`open_dir`, `next`, `stat`, `meta`, `open`, `read`, `apps` as sum/worst in
microseconds, plus the entry count). `at` is the PIT tick
the work ended at; compare it with `sysinfo`'s `ticks` (100 Hz). Read them
with `log --source programs --lines 2000 | grep UI:STALL`. At most 40 lines
per 10 s per thread print, then `UI:STALL:SUPPRESSED n=<count>`. Map an
interface id to its service with `idl/manifest.json`.

The line is an `xui_app::trace` line on channel `ui` (docs/tracing-plan.md,
T3): one whole write to the console device, so it reaches the program ring
and the serial port even from an app started in the Terminal (whose stdout
is a pty), and it is absent from an image built with `LAZYOS_TRACE=0`
(`sysinfo`'s `trace_channels` says which channels print).

## 4. Troubleshooting

| Symptom | Likely cause |
|---|---|
| No `DBGD:READY` | image built without `LAZYOS_DBGD=1`; `DBGD:OFF <why>` names a bad `diag.dbg.*` line; no DHCP address yet |
| Connection refused / times out | wrong IP or port; `diag.dbg.peer` set to another address; firewall; QEMU without the `9701:9701` forward |
| `-32001` right after connecting | wrong key (the build's, in `target/dbgd.key`), or no `auth` within 10 s |
| `-32004` locked out | failed attempts back off from 1 s to a minute: wait |
| `-32003 ... LAZYOS_DBGD=1?` | the kernel was built without the switch: rebuild kernel and image together |
| `usb.dump` unavailable | the image has no `usbd` (`LAZYOS_USB=1`), or no xHCI controller |
| Second client hangs | one client at a time: close the first (the MCP bridge holds a connection) |
| `-32002 control is off` | the image was built without `LAZYOS_DBGD_CONTROL=1` |
| `mmio.read` `-32002 ... diag.dbg.mmio=1` | the image was built without `LAZYOS_DBGD_MMIO=1` (`DBGD:READY ... mmio=0`) |
| `mmio.read` `-32002 ... allowed for BAR0: ...` | the span is not inside a listed range: choose one of the named ranges, or add one with `LAZYOS_DBGD_MMIO_ALLOW` after reading what the register does |
| `mmio.read` `-32002 ... claimed` | a driver holds the device: `LAZYOS_DBGD_MMIO_CLAIMED=1` allows it, at the risk of racing the driver |
| `-32003 ... BAR is not readable memory` | an I/O or unassigned BAR, memory decode off (a quiesced claim), or a BAR over RAM |
| `-32005` | hardware reads are rate limited: wait the milliseconds the message names |
| `reload` says `rolled-back` | the new binary exited or did not spawn in its trial: read `log --source programs` (`INIT:RELOAD:ROLLBACK reason=`) |

## 5. Hot-reload a service (control images only)

Build the box with the control tier, once:

```bash
LAZYOS_DESKTOP=1 LAZYOS_USB_IMAGE=1 LAZYOS_NETD=1 LAZYOS_DBGD=1 LAZYOS_DBGD_CONTROL=1 cargo build
python tools/run_demo.py --dbgd-control      # the same in QEMU
```

The control tier makes the pre-shared key worth the whole box: a `reload`
replaces a service with any binary the key holder uploads, under that
service's identity, so a reloaded driver has its DMA (all of memory) and a
reloaded `wlanmd` gets every Wi-Fi PMK from `keyd`. Keep `target/dbgd.key`
private, set `LAZYOS_DBGD_PEER`, and use a control image only while you
develop on the box (the NUC Wi-Fi loop in
[wifi-rtw88-plan.md](wifi-rtw88-plan.md) section 5 recommends one).

Then, after each change to a service or driver, rebuild with the **same
switches** (so the binary matches the box) and push it:

```bash
python tools/dbg/dbgctl.py --host 192.168.1.50 reload usbd
```

`dbgctl` takes `/system/bin/usbd` from `target/lazyos.img` (or the ELF you
name), uploads it, and `init` on the box restarts `usbd` from it. If the new
`usbd` exits or will not start within the trial (10 s, `--trial-ms`), `init`
puts the stick's `usbd` back by itself and `dbgctl` says `rolled-back` with
the reason; otherwise `committed`. Reloading `netdrv` drops the connection
for a moment: `dbgctl` reconnects to read the verdict.

| Command | Does |
|---|---|
| `reload NAME [FILE] [--trial-ms N]` | upload and run a new binary for a service |
| `restart NAME` | restart a service as it is |
| `revert NAME` | back to the stick's binary |
| `reloads` | what was reloaded since boot, and how it ended |
| `app-install FILE.lzp [--version V] [--no-relaunch]` | install a package (core apps too) and relaunch its windows; `--version` rewrites the manifest's version first |
| `relaunch APP` | close and reopen every window of an app |

A reload lasts until `revert` or a reboot: the stick is never written.

Apps (Calculator, LazyWriter, an installed game) are packages, so they are
swapped by installing a new `.lzp`, which `init` then relaunches in place:

```bash
python tools/dbg/dbgctl.py --host 192.168.1.50 app-install target/pkg/core/os.lazy.writer-0.1.0.lzp
python tools/dbg/dbgctl.py --host 192.168.1.50 relaunch os.lazy.writer
```

Unlike a service reload this is a real install on the OS volume (it
survives a reboot), it may replace a core app, and there is no automatic
rollback: install the previous package to go back. `--no-relaunch` leaves
running windows alone. `pkgd` refuses a package at the version already
installed, so while iterating on an app install each rebuild with a new one:
`app-install FILE.lzp --version 0.1.1` (then `0.1.2`, ...) rewrites only the
manifest's `version` line in memory; the file on disk is not touched.
`messengerd` and `dbgd` cannot be reloaded. This is remote code execution:
use it only on development machines on a network you trust.

## 6. Tests

```bash
python tools/dbg/run.py [--usb]        # builds, boots QEMU, judges every method, refusal and reload
python tools/dbg/run.py --no-control   # an image without the control tier refuses it
python tools/dbg/run.py --no-mmio      # an image without MMIO reads: config space only
python tools/dbg/test_judge.py         # the hardware-read judge's self-test
cargo test -p dbgwire                  # protocol, config, allowlist, control rules, seeded fuzz
```
