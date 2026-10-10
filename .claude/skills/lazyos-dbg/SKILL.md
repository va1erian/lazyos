---
name: lazyos-dbg
description: Inspect a running LazyOS machine (QEMU or a real PC) through its dbgd service, over MCP tools or tools/dbg/dbgctl.py. Use when debugging boot, USB, NIC or driver behaviour on hardware with no serial port, when asked what a service printed, what a task or controller is doing, or to read Messenger/devd state of a live box.
---

# Inspecting a live LazyOS box with dbgd

`dbgd` (issue #701, `docs/dbgd.md`, `docs/dbgd-plan.md`) is a read-only,
key-authenticated JSON-RPC service that a `LAZYOS_DBGD=1` image runs on TCP
9701. Prefer it to screenshots and serial scraping: the data is structured and
live. Inspection cannot change the machine; a box built with
`LAZYOS_DBGD_CONTROL=1` also accepts restarts and service hot reloads (below).

## Before anything else

1. Is the MCP server connected? Look for tools named `log_tail`,
   `list_tasks`, `usb_dump`, `messenger_services`, `dbgd_call`. If they are
   absent, use the CLI: `python tools/dbg/dbgctl.py --host <ip> <command>`
   (key defaults to `target/dbgd.key`).
2. Not reachable? The box needs an image built with `LAZYOS_DBGD=1
   LAZYOS_NETD=1` and an address (`DBGD:READY` on its console). QEMU:
   `python tools/run_demo.py --dbgd`. Troubleshooting: `docs/dbgd.md` section 4.
   One client at a time: do not run the CLI while the MCP bridge is connected.

## Where the information is

| Question | Tool |
|---|---|
| What did a service print (`USBD:*`, `NETDRV:*`, `XUID:*`)? | `log_tail(source="programs", lines=N)`; filter records on `tag` and `fields` |
| What did the kernel say at boot (`HW:*` verdicts, panics)? | `log_tail(source="kernel")`, `hwreport` |
| Is a service up, restarting, unhealthy? | `messenger_services` (state, pid, restarts, health), `list_tasks` |
| Who is registered on Messenger, what topics exist? | `messenger_registry`, `messenger_topics`, `dbgd_call("msg.topic", {"topic": ...})` |
| Which PCI function has which driver, who owns it? | `devices` (inventory, owner, rights), `drivers` (devd match and state) |
| xHCI controller, port and slot state | `usb_dump`: `USBD:DUMP:HC` (usbcmd, usbsts, crcr, dcbaap, erdp), `PORT` (portsc, ccs, ped, pls), `DEV` (slot state, ep0state, ep0dq vs ring) |
| Memory, counters, timeouts | `dbgd_call("mem.stats")`, `dbgd_call("sysinfo")`, `fabric_stats` |
| A file on the box | `fs_read(path)` (allowlisted roots only) |

## Method

- Start broad (`list_tasks`, `messenger_services`, `log_tail` of both rings),
  then narrow to the subsystem. Quote the records you rely on (tag and fields).
- A log record's `pos` is a byte offset: use it to order lines across calls.
  `dropped` in a stream means the ring wrapped; take a fresh `log_tail`.
- `usb.dump` is rewritten by `usbd` at most every 2 s and only when the bus
  changed: compare `ep0dq` with `ring` to see whether the controller fetched
  TRBs; `usbsts` bits (HSE, HCE) say whether it died.
- The program-output ring is 64 KiB: a chatty boot can push early lines out.
  Say so rather than concluding a line never happened.
- Everything you do is audited on the box (`DBGD:AUDIT` lines): fine, but do
  not poll in a tight loop.

## Changing the box (control images only)

On a `LAZYOS_DBGD_CONTROL=1` box a fix to a service or driver needs no re-flash:
rebuild the image with the box's switches, then `service_reload(name)` (or
`dbgctl reload NAME`). `init` restarts the service from the new binary and
rolls back by itself if it exits within the trial; the result says
`committed` or `rolled-back` with the reason. `service_restart`,
`service_revert`, `service_reloads` do the rest. An app (a desktop
package) is swapped with `app_install(lzp_path)`: `pkgd` installs it, core
apps included, and its open windows are relaunched (`app_relaunch(app)`
alone restarts them); this one is a real install that survives a reboot. These change a real machine:
do them when the user asked for a fix to be tried, say which service you
reload, and read the programs log afterwards. Without the switch they answer
`-32002`: build a new image and re-flash (`python tools/boot/write_stick.py`).

## Limits

No file writes, no register poking, no kernel reload. `pci.config` and
`mmio.read` do not exist (drivers own those). No TLS: do not use across an
untrusted network.

## Extending it

New methods go in `libs/dbgwire/src/methods.rs` (table and parameter bounds),
a handler in `user/src/bin/dbgd/handlers.rs`, a check in `tools/dbg/run.py`,
and a row in `docs/dbgd-plan.md`. Run `cargo test -p dbgwire` and
`python tools/dbg/run.py --usb`.
