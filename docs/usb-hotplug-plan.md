# Plan: USB hot-plug for HID devices and storage

> **Status: draft proposal, scope settled (2026-10-10).** Builds on
> [usb-hid-plan.md](usb-hid-plan.md) (`usbd`, HID, U3 hot-plug),
> [architecture/usb.md](architecture/usb.md) and
> [architecture/usb-storage.md](architecture/usb-storage.md) (syscall 33, the
> late `/home` mount). Nothing here is implemented yet.
>
> Decisions taken: read-write **FAT32 and exFAT are in scope** (a stock stick
> must work); `/home` moves into `storaged` and `fs/late.rs` is deleted;
> volumes **automount** with a notice from a **tray applet**; the
> real-hardware target is the **AMD NUC** ([compat/amd-nuc](compat/amd-nuc/README.md)).

## Goal

Plugging and unplugging a USB keyboard, mouse, tablet or stick, at any time,
on a root port or behind a hub, any number of times in one boot, always ends
in a defined state:

- **HID:** the device works within a second of being plugged in, nothing
  stays stuck when it leaves, and no other device stalls while it comes or
  goes.
- **Storage:** a stick that appears can be mounted and used, a stick that
  disappears (cleanly ejected or yanked) fails its I/O fast and gives every
  kernel resource back, and the same stick plugged in again works again,
  including the stick that holds `/home`.
- **A stock stick works:** FAT32 and exFAT volumes mount read-write, as well
  as LazyOS's own ext2.
- **The system knows:** the desktop can list what is attached, and a tray
  applet gives a notice when something arrives or leaves and offers Open and
  Eject.

## What exists, and what is actually missing

HID hot-plug is **done and tested** (usb-hid-plan U3: port-status-change
events, detach releases keys and buttons, Disable Slot, per-slot DMA reuse,
`tools/usb/run.py --hotplug 200`, `--hub`). Storage *unplug* is handled (the
stick's disk dies, its I/O fails, nothing hangs). Everything else a user would
call hot-plug for storage is not there:

| # | Gap | Evidence | Effect |
|---|---|---|---|
| S1 | **Disk slots are never reused.** 8 providers per *boot*; each stick ever plugged takes one for good. The 16 partition-view slots and the 32-entry block registry have no release either. | `block/provider.rs:55` (`MAX_PROVIDERS`), `block/partition.rs` (`POOL`), `block/mod.rs:48` (no `unregister`) | The ninth plug of a boot fails; so does a `usbd` restart with a stick in. |
| S2 | **No mount lifecycle.** The only USB mount is the one-shot `/home` late mount; any other stick is a registered device nobody mounts. A re-plugged home stick is a new disk and `/home` stays degraded. | `fs/late.rs` (`PENDING.take()`), usb-storage.md "Not done" | A stick is unusable except at boot, as `/home`. |
| S3 | **No safe removal.** No eject, no sync-and-unmount; the only durability is a flush 1 s after the last write. | usb-storage.md "Durability" | A pulled stick is always an unclean volume. |
| S4 | **`usbd` is blind while it waits on a stick.** A transfer parks in `Hc::wait_until`, which *stashes* every other event, port-status-change included, until the transfer completes or times out (30 s per transfer, 45 s per request). Bring-up is a synchronous TEST UNIT READY loop (up to 10 s). | `usbd/msc_link.rs:144`, `usbd/hc_events.rs:25`, `msc_link.rs:218` (`present()` only after the wait) | Unplugging a stick mid-write can freeze every USB keyboard and mouse until the timeout; plugging one in freezes them for its spin-up. |
| S5 | **No FAT32, no exFAT, no writes.** The only FAT driver is the kernel's read-only FAT12/16 for `/boot` (978 lines, every mutating call is `FsError::ReadOnly`). | `kernel/src/fs/fat/mod.rs:1,259` | The sticks people actually own cannot be written, or read when FAT32/exFAT. |
| H1 | **No inventory and no events.** `usbd` serves no Messenger interface; `devd` knows PCI functions only; `usbctl`/`idl/usb.midl` were optional and never built. | usb.md "Not done" | The desktop cannot show a device list, a "keyboard connected" notice or a removable drive. Only serial markers and `dbgd`'s dump say what is attached. |
| H2 | **Fixed 12 device slots per controller** (one 12 KiB DMA region each; the claim allows 16 buffers; DMA is never freed while `usbd` runs). | usb.md "Hot-plug and memory" | A hub with a keyboard, mouse, tablet and two sticks is near the ceiling. |
| H3 | **Paths QEMU cannot exercise** are proven by host tests only: high-speed hubs and transaction translators, SuperSpeed hubs, low-speed devices, cable bounce and debounce, over-current, a port the controller disabled. `--hotplug` covers root-port keyboard and mouse only, never a hub-attached device, a tablet or a stick being re-plugged. | usb.md "Not done"; `tools/usb/run.py` | Real-PC hot-plug is unproven. |
| H4 | Keyboard LEDs are not driven, so a re-plugged keyboard cannot have its state restored. | usb.md "Not done" | Cosmetic until `inputd` drives LEDs. |

S1 and S4 are the two that make today's behaviour wrong rather than merely
incomplete; they come first.

## Key decisions

1. **A disk has a lifetime, and everything that refers to it is checked
   against it.** A slot is recycled only when the disk is dead *and* nothing
   references it, and every request carries the generation its issuer opened
   the disk with. A holder from before an unplug can fail, never write: a
   stale ext2 volume must never be able to put its "clean" superblock on the
   stick plugged in next. This is the highest-severity risk of the plan.
2. **Mount policy lives in user space.** The kernel keeps the mechanism (a
   narrowly gated mount/unmount of provider disks); a new service decides
   what mounts where, for whom and with which options. `late.rs`'s kernel-side
   volume search is the model to retire, not extend.
3. **`usbd` stays one task and never blocks on a device that may be leaving.**
   Waiting on a transfer is interruptible by that device's disconnect; bring-up
   is stepped from the main loop. A second task for storage is rejected for
   now: it would split controller ownership for a problem two small changes
   solve.
4. **Interfaces are MIDL** (`idl/usb.midl`, `idl/storage.midl`), generated
   with `midlc`, rhai schema and API regenerated. Per the repo rules there is
   no hand-written protocol and no compatibility shim: the `usb.dump`
   side channel and the one-shot late mount are replaced, not kept beside.
5. **Removable media is untrusted.** Always `nosuid,nodev`, no execute
   by default, files owned by the session user regardless of the uids on the
   medium, parsed by fuzzed libraries only.
6. **Limits that block the feature go.** The 8-per-boot and fixed-slot caps
   are replaced by real resource accounting (live disks, live DMA), not by
   larger constants.
7. **FAT32 and exFAT run in a user-space FUSE daemon, not in the kernel.**
   A stock stick is the most hostile input the filesystem layer will see; a
   parser bug should cost an unmount, not a kernel panic. The mechanism
   already exists (`fused`, `ftpfuse`/`smbfuse`, `mountd`, `owner=`
   ownership override, forced `nosuid`). The price is a small raw
   block-access syscall for the daemon and a FUSE round trip per operation;
   the library is `no_std`, so a kernel adapter stays possible if the
   measured cost is too high (phase P6). The kernel's read-only FAT12/16
   `/boot` driver is not touched.
8. **A notice is the tray applet's attention state, for now.** The tray has
   icon, tooltip, status (`Active`/`Passive`/`Attention`), a 3-character badge
   and a menu; it has no toast (`os.lazy.notify` is its own plan, and flyouts
   are tray T4). The applet is written so that its one "something happened"
   function is the only place a toast would be added.

## Design

### A. `usbd`: interruptible waits, stepped bring-up

- `Hc::wait_until` gains an abort predicate. While a stick's transfer is in
  flight it is checked on every wake: the stick's root port lost `CCS`, a
  port-status-change event for it (or its hub port) is in the queue, or its
  slot was disabled. The transfer then returns `XferError::Gone` at once, no
  recovery commands are sent to the dead slot, and the stashed events stay
  queued for the dispatcher as today.
- Below a hub the stick's disconnect arrives as the hub's status-change
  report, an interrupt-IN event of *another* endpoint; the abort predicate
  looks for that endpoint's events too, then lets `service_hub` examine the
  port before the transfer is declared lost.
- `Msc::start` becomes a state machine advanced one step per main-loop pass
  (INQUIRY, TEST UNIT READY with `next_at`, READ CAPACITY, MODE SENSE,
  register), so HID reports are drained between steps. The bounded tries
  (100 x 100 ms) are unchanged.
- Open measurement (phase 0): whether QEMU and real controllers post a
  transfer completion with an error when the device disappears, or nothing at
  all. The abort predicate is needed either way; the answer decides whether
  the old 30 s timeout is ever reached.

### B. Kernel: a disk lifecycle

- `UserDisk` gets a **generation** and a **lease count**. Opening the disk for
  a filesystem or a partition view takes a lease (an RAII token held by the
  `BlockIo` adapter); requests carry the generation. Dead means `alive ==
  false`; reclaimable means dead and zero leases.
- Reclaiming a slot clears its registry entry (`block::unregister`, new),
  recycles its partition-view slots, empties its read cache (already done on
  register) and frees the name `usb<n>` for the lowest free `n`. A late
  `COMPLETE` for an old generation is `ESTALE` as today.
- The registry and partition pool stop being append-only. `MAX_PROVIDERS`
  becomes "live disks", not "disks this boot".
- `provider.rs` is at 499 lines: the lifecycle goes in
  `block/provider/lifecycle.rs` before anything is added (repo limit 500).
- Open design point, settled by a spike in phase 2: where the lease is taken
  so that a mounted filesystem, an open file kept alive past `Vfs::unmount`
  (whose open nodes "keep the filesystem alive"), and a partition view all
  hold one. The fallback if leases prove invasive is generation-only: slots
  recycle when dead and unmounted, and every holder checks the generation per
  request, so a stale holder fails closed.

### C. Mounting: a `storaged` service

- **Kernel surface** (syscall 33 family, or a sibling): `MOUNT(disk, point,
  options)` and `UNMOUNT(point)`, gated by a new capability bit (`CAP_FS_MOUNT`)
  plus a uid allowlist like `usbpolicy::BLOCK_PROVIDER_UIDS`. It can name only
  provider disks, only mount points under `/media` (a new `libs/fhs`
  constant; `tools/fhs/check_literals.py` stays green), always with
  `nosuid,nodev`, and takes an `owner=<uid>` that overrides inode ownership
  the way the FUSE `owner=` does. `CAP_SYS_ADMIN` is deliberately not reused:
  it also grants power and display.
- **`storaged`** (`idl/storage.midl`, `os.lazy.storage.v1`; init row, its own
  uid, only that capability): watches the disk and usb topics, scans the
  partition table, identifies the filesystem, mounts at `/media/<label or
  uuid>`, and publishes a retained `system/storage/volumes/{id}` topic
  (`state`: `present`, `mounting`, `mounted`, `unsupported`, `unclean`,
  `removed`; label, size, writable, mount point, owner). Methods: `Volumes`,
  `Mount`, `Eject`, `Unmount`. Authorization: a volume belongs to the
  session user who was logged in when it appeared; only that user (or an
  administrator through `elevd`) ejects it.
- **Two mount paths, one policy.** ext2 goes through the kernel op above.
  FAT32 and exFAT go through `mountd`, which already starts and supervises
  one FUSE daemon per mount: `idl/mount.midl` gains a block-volume request
  (disk name, kind `fat`/`exfat`, owner) that only `storaged`'s uid may make,
  and `libs/mounttable` gains the kinds. The mount appears at the same
  `/media/<label or uuid>` either way, with the session user as owner.
- **Automount** happens only while a session exists, for the volume kinds in
  an allowlist (ext2, FAT32, exFAT), and never for a volume that fails the
  library's quick validation. A volume left dirty (ext2 not cleanly
  unmounted, FAT/exFAT dirty flag) mounts read-only with a notice unless it
  can be repaired safely (`reclaim_orphans`, ext2's journal when built with
  `LAZYOS_JOURNAL`). Nothing is ever "fixed" on a stick that is not ours
  without the user asking.
- **`/home`:** `storaged` takes over the home volume. `init`'s `SETTLE` wait
  becomes a wait on `storaged`'s `home` state, and `fs/late.rs` is deleted
  (the home volume is always ext2, so it uses the kernel op). A
  returning stick with the configured label or UUID **replaces** the dead
  mount at `/home` (a `Vfs` replace-mount; descriptors open on the old mount
  stay failed and apps reopen). Transparently reviving the old filesystem
  object is rejected: its inode and bitmap caches may predate changes made
  to the stick elsewhere.

### D. Safe removal

`Eject(volume)`: `storaged` stops new opens, asks the kernel to unmount
(`Ext2` flush, mark clean, drop the lease), then asks `usbd`
(`os.lazy.usb.v1.Eject(disk)`) to run SYNCHRONIZE CACHE and START STOP UNIT.
The volume's topic goes to `removed` with `safe = true`. An unmount blocked by
an open file reports who holds it instead of hanging; a forced "Eject anyway"
is a lazy unmount (the mount is forgotten, open descriptors fail). A yank
publishes `removed` with `safe = false` and a notice.

### E. Inventory and events

`os.lazy.usb.v1`, served by `usbd` on the wait set it already uses for the
controller interrupt:

- `Devices() -> Array<UsbDevice>`: controller, port path (route string),
  vendor/product, speed, class(es), `state` (`configured`, `unsupported`,
  `failed`, `removed`), bound functions, and the disk id for a stick.
- Retained topic `system/usb/devices/{id}`, republished on every change, so
  a late subscriber learns the whole tree.
- `Eject(disk)` as above. `usbpolicy` grows the registry rows `_usb` needs.
- Each device carries `removable` from the xHCI `PORTSC.DR` (Device
  Removable) bit, or the hub descriptor's removable mask below a hub. A
  machine's internal Bluetooth or card reader is `unsupported` or bound but
  never `removable`, and never raises a notice.
- Consumers: `usbctl` (a rhai script first, per the repo's rhai-first rule),
  the tray applet of section G, Settings -> Devices, and `dbgd`, which stops
  writing its own `usb.dump` file.

### F. FAT32 and exFAT

- **`libs/fatfs`**: `no_std` + `alloc`, no dependencies, read-write FAT12/16/32
  (long names, FSInfo, dirty bit) and exFAT (allocation bitmap, up-case table,
  name hashes, the no-FAT-chain flag, files over 4 GiB, clusters up to 32 MiB,
  volume dirty flag), written from the published specifications. A formatter
  (for tests and, later, a Format action), an independent fsck-style checker,
  and a fuzz entry point shared with a cargo-fuzz target, exactly as
  `libs/ext2fs` does. A third-party crate is used only if it passes a
  GPLv2-compatibility check like `tools/nettls/licenses.py`.
- **Write ordering is the data-safety story.** FAT has no journal: the library
  documents and tests the order of FAT, bitmap and directory-entry updates so
  a cut after any single write leaves a volume the checker can reason about
  (the way `ext2fs` is tested with a power cut at every write), and it clears
  the dirty flag only after the final flush.
- **`fatfuse`**: a FUSE daemon over `fused`, started by `mountd` with the
  disk name and `owner=`. FAT has no unix owner or mode, so every file is the
  session user's, with a `umask`-style mask from the mount options and no
  execute bit unless configured. Case rules and name validity follow the
  format (exFAT is case-insensitive and preserving; names Linux-style
  filesystems allow but FAT does not are refused with a clear error).
- **Raw block access** (new, small): a syscall that lets a holder of a new
  capability read and write sectors of a provider disk or partition, with the
  generation check of section B, a bounce buffer like the provider path, and
  the same allowlist-by-uid gate as every other provider call. `mountd`
  passes it to its daemon the way it passes `CAP_FS_PROVIDER`. Nothing else
  gets it; it also gives a future user-space `mkfs`/`fsck` their path.
- A sector cache in the daemon is write-through, like the kernel's stick read
  cache: nothing the daemon holds needs writing back when the stick is
  pulled.

### G. The tray applet (`removable`, `os.lazy.removable`)

A resident applet in the model of Volume and Network Status
([tray-plan.md](tray-plan.md) T3): no window, always in the tray, opens with
every session, state read from the retained topics (`system/usb/devices/+`,
`system/storage/volumes/+`) so it needs no interface of its own. Written in
Rust on `xui_app::resident` and `trayclient`: the tray's Rhai client is T5.

- **Icon and status:** a Lucide `usb` icon; `Passive` with nothing removable
  attached, `Active` while a volume is mounted, `Attention` for a few seconds
  after an event. The badge is the number of mounted volumes (at most three
  characters).
- **The notice** is the tooltip text plus the `Attention` flip, from one
  function: "PHOTOS mounted at /media/PHOTOS", "PHOTOS removed without
  ejecting (data may be lost)", "USB keyboard connected". Only devices that
  arrived while the session ran and are `removable` count; what was present
  at login, and anything internal, is silent. Events coalesce, so a hub
  that brings up four devices is one notice.
- **Menu:** per volume an Open row (through `mimed`/`init.Launch`, opening
  Files at the mount point) and an Eject row; an Eject all row; and the
  failure text when an eject was refused (who has a file open).
- **Packaging, per AGENTS.md:** `xui-app/packages/removable/`, an entry in
  `tools/xui/core_packages.py` and in the applet list of
  `build_support/core_packages.rs`, shipped in images that ship `usbd`,
  permissions derived from a run under `LAZYOS_LABEL_TRACE=1`, a
  `core_apps.json` entry, serial markers `REMOVABLE:UP:PASS`,
  `REMOVABLE:STATE <tooltip>`, `REMOVABLE:QUIT:PASS`, and a session such as
  `tools/screenshot/examples/removable.json`.

### H. HID follow-ups (small)

- The inventory and the applet are the HID user-visible part.
- Raise the 12-slot ceiling to what the controller's `MaxSlots` and a real
  DMA budget allow: lift the kernel's 16-buffers-per-claim limit instead of
  adding slots by pre-allocating more regions (decision 6).
- Re-send state on attach when `inputd` gains keyboard LEDs (H4); nothing
  until then.

## Phases

Each phase is independently shippable and ends with the listed checks. Kernel
phases ship correctness **and** stress tests (AGENTS.md) and must pass
`python tools/test/run.py --accel none`.

| Phase | Deliverable | Verification |
|---|---|---|
| **P0** Reproduce | Harness scenarios that fail today: `tools/storage/run.py --replug N` (N plug/unplug cycles of one stick, expect the 9th to fail), `--yank-during-write`, `--hub-yank`; `tools/usb/run.py` scenario holding a key while a stick spins up; a log of what the controller posts on disconnect (QEMU and the NUC). `tools/usb/hotplug.py` over a QMP socket for manual use. | Each scenario fails for the documented reason; the judges' `test_judge.py` fail when they should. |
| **P1** `usbd` never waits blind (A) | Abort predicate in `wait_until`, hub-aware disconnect detection, stepped `Msc::start`. | Host tests with a fault-injecting model controller (disconnect at every point of a request and of bring-up); P0 `--yank-during-write` and the HID-during-spin-up scenario now pass; no HID report is held up more than one tick. |
| **P2** Disk lifecycle (B) | Generations, leases, `block::unregister`, partition pool recycling, `provider/lifecycle.rs`. | `provider_suite`: 3000 plug/unplug generations with a mounted volume and an open file across each; a stale holder's write and flush never reach the next disk (checked by content); a forged old tag is `ESTALE`; name reuse; heap and registry back to baseline. `--replug 200` passes. |
| **P3** Inventory (E) | `idl/usb.midl`, `usbd` server and topic with the `removable` flag, `usbctl`, `dbgd` migrated, `idl/manifest.json` and rhai API regenerated. | `cargo test -p messenger-generated`; a session lists devices before and after `device_del`; an internal device is never `removable`; `tools/dbg/run.py` still judges the USB methods. |
| **P4** `storaged` and ext2 (C) | Kernel mount/unmount op and capability, `libs/fhs` `/media`, `idl/storage.midl`, `storaged`, ownership override, automount policy, `/home` through `storaged`, `late.rs` deleted. | Kernel suite for the gate (wrong uid, non-provider disk, bad point, nested mount) and stress; `tools/storage/run.py` (two boots) still passes through `storaged`; a new `--second-stick` run mounts a data stick at `/media/<label>`, writes as `user`, re-plugs it, reads it back; `e2fsck -fn`. |
| **P5** `libs/fatfs` (F) | The FAT12/16/32 and exFAT library, formatter, checker, fuzz targets and seeds. **Host only: it can start on day one**, in parallel with P0-P4. | `cargo test -p fatfs` incl. a power cut at every write; images made by the library pass `fsck.fat`/`fsck.exfat` and images made by `mkfs.fat`/`mkfs.exfat` (and a real 64 GB stick image) read back byte for byte, on Linux/WSL; seeded fuzz soak; `fuzz/gen_corpus.py --check`. |
| **P6** `fatfuse` and raw block access (F) | The raw-block syscall and capability, `fatfuse`, the `mountd` block-volume request and `mounttable` kinds, `storaged` choosing the path by filesystem. | Kernel suite for the syscall gate (uid, capability, generation, bounds) and a stress run; a `tools/fuse/ui_run.py`-style judge: mount a FAT32 and an exFAT stick, copy a 1 GiB file both ways with a checksum, unplug mid-copy (daemon exits, mount goes, nothing hangs), re-plug; throughput recorded against the FUSE round-trip cost (the trigger for a kernel adapter, risk 8). |
| **P7** Eject and integrity (D) | `Eject`, forced eject, unclean-volume policy, journal replay for removable ext2, dirty-flag handling for FAT/exFAT. | Harness: eject then pull (volume clean, on all three filesystems), pull without eject (notice, recovers on next mount), eject refused with a file open, unclean FAT mounts read-only. |
| **P8** Tray applet and desktop (G, E) | The `removable` applet and its package, Files "Devices" section with an eject button, Settings -> Devices pane; `run_demo.py --usb-stick PATH` (a hot-pluggable stick plus a QMP socket) and the matching GUI launcher control in `tools/lazygui/catalog.py` with `test_catalog.py` cases. | `core_apps.json` shows no `LABEL:DENY`; `removable.json` session: plug, `Attention` and tooltip, Open in Files, eject, unsafe pull; screenshots read, not only judged. |
| **P9** The AMD NUC | See below. | The six scenarios below, each with its `dbgctl` log under `docs/compat/amd-nuc/usb/` and the `hardware.md` row filled in; failures filed against the phase that owns them. |

Order: P0 first. P1, P2, P3 and P5 are independent of each other. P4 needs
P2; P6 needs P2, P4 and P5; P7 needs P4 and P6; P8 needs P3 and P4 (its Eject
rows need P7). Plausible first PRs are P0 alone (it makes the problem visible
without touching a driver) and P5 (a self-contained library nothing else
waits on).

### P9: the AMD NUC

The machine is documented in [compat/amd-nuc](compat/amd-nuc/README.md): a
MAGICNUC AS1 (Ryzen 5 3501U, Raven2), booting the stick image to the desktop,
with both RTL8168 ports and `dbgd` working. It is the only AMD box in
[compat/hardware.md](compat/hardware.md), and that row records no USB result
yet ("Input: not yet recorded", "xHCI behaviour" under *Still to check*). Its
Linux survey (`hw-survey/05-*`) already tells us what to expect:

| Fact (from the survey) | Why it matters here |
|---|---|
| Two xHCI controllers, `04:00.3` (`1022:15e0`: 4 USB 2 + 4 USB 3 ports) and `04:00.4` (`1022:15e1`: 2 USB 2 + 1 USB 3), both xHCI 1.1 | The first real test of two controllers with a device on each: per-controller DMA budget, slot pool and event ring (QEMU only has `--controllers 2`). |
| `HCCPARAMS1` = `0x0270ffe5` / `0x0260ffe5`: **`CSZ` set, so 64-byte contexts** | The 64-byte context layout is proven only by host tests today (usb.md "Not done"). Every enumeration on this box runs it. A 64-byte bug shows up as P9's very first failure. |
| Linux applies quirk mask `0x0004000840000010` to both controllers | Decode it against the survey kernel's `xhci.h` before deciding which quirks `usbd` needs; nothing is assumed necessary until a scenario fails. |
| A Realtek Bluetooth radio, `0bda:c822`, full speed, directly on port 2 of controller 2 | A real internal device that must be `unsupported` in the inventory and silent in the applet; also the check that a root-port `PORTSC.DR` (Device Removable) is set or not by this firmware. If it is not, "arrived after login" alone keeps it quiet. |
| A DREVO BladeMaster TE 87K keyboard (`1a2c:b51f`): **two HID interfaces**, both keyboards to Linux, one also exposing a mouse | A composite device of exactly the kind usb-hid-plan risk 8 left open: which interface `usbd` binds and whether every key (NKRO, media keys) arrives. Unplug and replug it. |
| A Logitech receiver, `046d:c542`, full speed | A second HID device on the other controller; the pair of them is the multi-controller hot-plug scenario. |
| No hub in the survey | Hub paths (high-speed and SuperSpeed, transaction translators) need a hub supplied for the test. |

- **How the evidence comes off the box:** `dbgd` over the network
  (`run_demo.py --dbgd`, `--dbgd-control`). `dbgctl.py usb` dumps
  `USBD:DUMP:HC/PORT/DEV` (controller registers, `PORTSC`, slot and endpoint
  0 state), `log --follow` and `log --source programs` carry `USBD:*`, and
  `reload usbd` hot-loads a rebuilt driver for the session without writing
  the stick. That makes P1 iterable on the NUC without reflashing.
  Caveat until P2 and P4 land: the reloaded `usbd` is a new provider task, so
  a `/home` on the boot stick dies with the old one; use a session that
  does not need `/home`, or accept a reboot per reload.
- **Scenarios** (each a documented `dbgctl` command sequence and its log,
  written to `docs/compat/amd-nuc/usb/`):
  1. Boot with the keyboard and receiver in: `USBD:XHCI hc=0|1`, `csz64=1`,
     the descriptors, which interfaces bind, every key and button arrives.
  2. Hot-plug the keyboard and receiver on every external port, and swap
     them between the two controllers; key held across the unplug.
  3. The boot stick on the second controller and a second stick on the first:
     two disks, `--replug`-style churn, yank under write load (P1's
     measurement of what this controller posts on disconnect).
  4. A SanDisk 0781:5591 (#704) and an exFAT stick of 64 GB or more
     (P6, P7): copy with a checksum, yank mid-copy, return.
  5. Keyboard, mouse and stick behind a USB 2 hub and a USB 3 hub, once a hub
     is supplied.
  6. `/home` on the boot stick unplugged and returned (P4).

## Risks

1. **A recycled slot aliasing a stale holder** corrupts the wrong stick (B).
   Mitigation is decision 1 and P2's content-checked stress test; this is the
   phase that most needs adversarial review.
2. **Real controllers differ from QEMU.** Disconnect semantics, debounce and
   over-current are unproven until P9; the abort predicate is written not to
   depend on any single event.
3. **Hostile filesystems.** ext2 is parsed in the kernel as for `/home` today;
   FAT32 and exFAT (decision 7) are not. Automount widens exposure, hence the
   allowlist, forced `nosuid,nodev`, the checker's pre-validation and fuzz
   coverage kept current (`fuzz/gen_corpus.py --check`).
4. **Mount ownership.** A foreign stick's uids are meaningless here; the
   `owner=` override must cover creation as well as stat, and must not let a
   user create setuid-looking files that survive onto another machine.
5. **Unmount with open files, and the home volume.** Forced unmount is a lazy
   unmount with failing descriptors; `/home` loss under a live session
   degrades apps before it recovers. Policy and wording of the notice need
   the session/logind owners.
6. **File-length limit.** `provider.rs` (499), `bus.rs` (445) and
   `device.rs` (444) are near 500; split by responsibility before adding to
   them (`tools/check_file_length.py`).
7. **`usbd` restart** (it runs `Restart::OnFailure`) re-enumerates every
   device; with P2 and P4 that is a series of ordinary plug events, but the
   `/home` replace-mount makes a `usbd` crash visible to the session. Worth a
   dedicated restart scenario in P4.
8. **FUSE throughput.** One round trip per read or write may make large exFAT
   copies slow, and `usbd` serves one request per stick at a time, so the
   floor is the stick. P6 measures it; if the daemon is the bottleneck the
   library moves behind a kernel adapter and decision 7 is revisited with
   numbers.
9. **Writing a FAT/exFAT implementation is a data-loss risk on media the user
   also uses elsewhere.** A bug here corrupts a stick that works on every
   other machine. Hence the independent checker, differential tests against
   the reference tools, a power cut at every write, and read-only mounting of
   any volume that is dirty or fails validation. exFAT stays read-only until
   its P5 checks pass in CI.
10. **The notice is weak.** Attention, a badge and a tooltip are easy to
    miss (decision 8). Accepted for now; a toast is the notify plan's job.

## Open questions

1. **Notice strength.** The tray has no toast. If the Attention flip and
   tooltip are too quiet for "stick mounted" and especially "removed without
   ejecting", the options are to build `os.lazy.notify` first or to pull tray
   T4 (a flyout the applet opens itself on arrival). The plan proceeds
   without either.
2. **Hubs for P9.** The NUC survey has none. A USB 2 hub and a USB 3 hub
   (ideally a high-speed one with a multi-TT or a single-TT) are needed
   for scenario 5; until then hub behaviour is QEMU and host tests only.
3. **What firmware does with `DR`.** Whether the NUC's root ports set Device
   Removable for the internal Bluetooth radio is unknown until P3 reads
   `PORTSC` over `dbgd`. If they do not, the `removable` flag is only as good
   as "present at login", and an ACPI `_UPC`/`_PLD` lookup (`libs/acpi`)
   becomes a follow-up.
