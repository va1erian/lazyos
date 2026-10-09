# Using dbgd: inspect a LazyOS box over the network

`dbgd` lets you (or an AI agent) read a running LazyOS machine without a serial
port or a camera: the live boot log, what each service printed, tasks, memory,
PCI devices, driver state, a USB controller snapshot and Messenger's registry.
This is the how-to; the design, protocol and threat model are in
[`dbgd-plan.md`](dbgd-plan.md). v1 is read-only.

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
| `log [--follow] [--lines N] [--source kernel\|programs\|<logd journal>]` | the boot log; what services (`usbd`, `netdrv`, ...) printed |
| `tasks`, `sysinfo`, `mem`, `fabric` | scheduler, kernel statistics, memory, Messenger counters |
| `devices`, `drivers` | PCI inventory with owner and rights; `devd`'s match and driver state |
| `usb` | `USBD:DUMP:HC` / `PORT` / `DEV`: controller registers, port status, slot and endpoint 0 state |
| `hw` | the `HW:*` verdicts of the boot log |
| `msg-registry`, `msg-services`, `msg-topics`, `topic NAME` | Messenger: names, supervised services, topics, one retained value |
| `cat PATH` | a file under `/transient`, `/tmp`, `/logs`, `/system/etc`, `/system/share`, `/docs` |
| `methods`, `call METHOD '{"k":v}'` | the method table; any method by name |

Every log line is a record: `tag` (`USBD:DIAG`), `fields` (`port=1`,
`ep0dq=0x1f000`), `text` and `pos` (byte offset). Filter on tags instead of
scraping text.

## 3. Let an agent use it (MCP)

```bash
pip install mcp
claude mcp add lazyos-dbg -- python tools/mcp/debug_bridge.py --connect 192.168.1.50:9701
```

Start a new Claude Code session so the tools load: `log_tail`, `list_tasks`,
`fabric_stats`, `devices`, `drivers`, `usb_dump`, `hwreport`,
`messenger_registry`, `messenger_services`, `messenger_topics`, `fs_read` and
`dbgd_call`. The `lazyos-dbg` project skill (`.claude/skills/lazyos-dbg`)
tells an agent how to use them for bring-up and driver debugging.

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

## 5. Tests

```bash
python tools/dbg/run.py [--usb]    # builds, boots QEMU, judges every method and refusal
cargo test -p dbgwire              # protocol, config, allowlist, seeded fuzz
```
