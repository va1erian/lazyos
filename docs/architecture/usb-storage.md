# USB storage: `/home` on the stick LazyOS boots from

**What it is.** A USB mass-storage device (a stick) is served to the kernel
as a block device by the user-space USB driver, `usbd`, and the configured
home volume on it is mounted at `/home` after boot. Block drivers live in the
kernel and USB lives in user space ([driver-plan.md](../driver-plan.md)
D1/D2), so the stick reaches the filesystem through a new seam: a
**user-space block provider** (syscall 33). The goal is the real-PC plan's
persistent home on the boot stick ([real-pc-boot-plan.md](../real-pc-boot-plan.md)).

**Key files**

| Path | Role |
|---|---|
| `libs/usbmsc/` | Bulk-Only Transport (CBW/CSW, reset recovery), SCSI commands and sense, its own interface detection (SuperSpeed burst; fuzzed, unused by `usbd`, which binds through `libs/usbhid`'s descriptors), `Disk` (bring-up, read, write, flush); host-tested and fuzzed (`mscdesc`, `mscreply`, `mscsession`) |
| `libs/xhci/` | Plus the bulk Normal TRB (`trb::bulk`, at most 64 KiB) |
| `user/src/bin/usbd/msc.rs` | The mass-storage class (`class.rs` binds it): bulk pipes, disk bring-up, registration, serving the kernel's requests, idle flush, removal |
| `user/src/bin/usbd/msc_link.rs` | The `usbmsc` transport (`Link`): bulk transfers through the controller's bulk window, control requests, controller-side endpoint recovery |
| `kernel/src/block/provider.rs`, `provider/` | `UserDisk` (a `BlockDevice` served from user space), the request slot, timeouts and death; `sys.rs` is syscall 33 |
| `kernel/src/task/relax.rs` | `YieldMutex`: locks that may be held across a park |
| `kernel/src/fs/late.rs` | The late `/home` mount (`SETTLE`) |
| `user/src/bin/init/home.rs` | `init`'s bounded wait for the home volume |
| `tools/storage/` | The end-to-end harness (two boots, `e2fsck`) |

## How a block reaches the stick

1. `init` starts `usbd` as `_usb` with `CAP_DEV_CLAIM | CAP_INPUT_SOURCE |
   CAP_BLOCK_PROVIDER`. Every device is enumerated the same way, on a root
   port or below a hub ([usb.md](usb.md)); `class.rs` hands each mass-storage
   interface (class 08) to `msc.rs`. A SCSI Bulk-Only one (subclass 06,
   protocol 50) with a bulk IN and a bulk OUT endpoint has both opened as
   pipes (`Device::open_pipe`: max packet 512 or 1024, `bMaxBurst` from the
   SuperSpeed companion); any other is reported (`USBD:MSC:SKIP`). It can
   share a composite device with HID interfaces.
2. After the device's one Configure Endpoint the stick is brought up: INQUIRY (a direct
   access device), TEST UNIT READY until ready (bounded: 100 tries 100 ms
   apart, UNIT ATTENTION retried, NOT READY waited for, no medium final),
   READ CAPACITY(10), and (16) above 2 TiB, MODE SENSE(6) for write
   protection. LUN 0 only. Only 512-byte blocks are served
   (`USBD:MSC:SKIP` otherwise).
3. `usbd` registers the disk (`REGISTER`: sectors, writable); the kernel adds
   it to the block registry as `usb<n>` (`USBD:MSC:DISK`). After its first
   scan of every controller `usbd` reports `SCANNED`.
4. A filesystem's `read_sectors` becomes a request in the disk's single slot
   (a write's data is copied into the kernel's 64 KiB bounce buffer first;
   bigger transfers are split). `usbd`'s loop (`Controller::serve_storage`;
   when idle it waits up to a tick on a stick's queue instead of napping)
   takes it with `NEXT` (a write's
   data is copied out to it), runs READ(10/16), WRITE(10/16) or SYNCHRONIZE
   CACHE through `usbmsc`, and answers with `COMPLETE`. A read's data is
   copied into the bounce buffer only when its length is exactly the
   request's, and into the requester's buffer only after the tag is checked.
5. Every transfer is one Normal TRB from a 64 KiB window that never crosses
   a 64 KiB boundary, in a 128 KiB DMA region each controller allocates once
   and its sticks share (they are served one transfer at a time; the claim
   allows 16 DMA buffers). It is waited for synchronously; other endpoints'
   events stay queued for the dispatcher. A stall or failure halts the
   endpoint; `usbd` recovers it in the controller (`Device::recover`: Reset
   or Stop Endpoint, Set TR Dequeue Pointer past the abandoned TRB, its
   events dropped) and `usbmsc` on the device (CLEAR_FEATURE(HALT), CSW
   retried once, then Bulk-Only reset recovery).

### The driver's timeouts

A flash stick does not answer at a steady pace: a write (and especially its
status) can sit for seconds while the controller inside the stick erases and
moves blocks. The first real-PC run (a SanDisk 0781:5591 on an Intel 9d2f,
issue #704) lost `/home` minutes after boot: with a 5 s wait per transfer,
a slow write was abandoned and reset, retried up to four times, and the
kernel's then 10 s request deadline fired twice. Now (`msc_link.rs`):

| What | Limit | Why |
|---|---|---|
| One bulk transfer while serving a request | 30 s (`SERVE_TRANSFER_TICKS`) | Linux's SCSI command timeout |
| One kernel request, retries and recovery included | 45 s (`REQUEST_BUDGET_TICKS`) | below the kernel's 60 s `TAKEN_TICKS`, so the driver answers (with an error if it must) before the kernel gives up |
| One bulk transfer during bring-up | 5 s | a stick that never answers INQUIRY must not stall the other devices |

| An idle flush (SYNCHRONIZE CACHE after a write and a quiet second), recovery included | 8 s (`IDLE_FLUSH_TICKS`) | a request queued meanwhile must be taken within the kernel's 10 s |

Once the budget is spent every further transfer of the request fails at
once, and so do the Bulk-Only reset recovery's control requests: the
request ends as an I/O error (not a dead stick) and the recovery runs before
the next request (`reset_pending`), where only a failure with time left
marks the stick dead. While `usbd` waits on a stick its
HID devices are not served, so a stalled write pauses the USB mouse too.

Evidence for the next real-hardware run: `USBD:MSC:XFER port=<p> in|out
len=<n> result=timeout|stall|budget|<completion code> waited_ms=<ms>
epstate=<xHCI endpoint state> epdq=<dequeue pointer> usbcmd=.. usbsts=..`
for every transfer that did not complete (read before recovery changes
anything), then `USBD:MSC:REQ id=usb<n> op=read|write|flush lba=<n>
bytes=<n> ms=<ms> result=<..> stalls=<n> resets=<n>` for every request that
failed or took over 2 s. `USBD:MSC:FAIL ... next errno 3` now says what it
means: the kernel declared the disk dead, and its own log says why.

```
  ext2 / VFS ──read_sectors──▶ UserDisk (kernel, usb<n>)
                                  │ request slot + 64 KiB bounce buffer
              park in 10-tick ◀───┤
              slices, liveness    │ NEXT ▲      │ COMPLETE
                                  ▼      │      ▼
                              usbd: msc ── usbmsc (BOT/SCSI) ── xHCI bulk
```

## Syscall 33

| Op | Who | Arguments | Result |
|---|---|---|---|
| 0 `REGISTER` | provider | `info -> [sectors, 512, flags (bit 0 writable), 0]` | disk id |
| 1 `NEXT` | provider | `id, req, data, cap \| deadline << 32` | `1` and `req = [tag, op, lba, bytes]`, or `0` after waiting until `deadline` (at most 1 s ahead) |
| 2 `COMPLETE` | provider | `id, tag, status (0 ok, 1 io, 2 read-only, 3 gone), data` | `0` |
| 3 `REMOVE` | provider | `id` | `0`; the disk dies |
| 4 `SETTLE` | `CAP_SYS_ADMIN` | - | `0` none pending, `1` mounted now, `2` waiting, `3` mounted earlier, `4` absent |
| 5 `SCANNED` | provider | - | `0` |

A provider is a task holding `CAP_BLOCK_PROVIDER` **and** running as a uid in
`usbpolicy::BLOCK_PROVIDER_UIDS` (only `_usb`); a disk answers only the task
that registered it. Errors: `EPERM`, `ESRCH` (not the owner, or dead),
`EINVAL`, `ESTALE` (no such request in flight), `EFAULT`, `EBUSY` (eight
providers per boot).

## Death, timeouts and removal

Nothing the provider does can hang or crash the kernel:

- The requester parks on the disk in **10-tick slices** and checks at each
  wake that the provider task is alive. The provider must **take** a queued
  request within **10 s** (`QUEUE_TICKS`: a provider that does not even look
  is stuck) and **finish** one it took within **60 s** (`TAKEN_TICKS`). The
  second deadline is long on purpose: a real stick can stall a write for
  seconds while its flash reorganises (Linux gives a SCSI command 30 s), and
  the first real-PC run lost `/home` to a 10 s limit (issue #704). `usbd`
  bounds its own work on a request below that (45 s, see "The driver's
  timeouts"). Each timeout prints `block: usb<n>: <op> lba <n> (<bytes>
  bytes) timed out untaken|in the provider after <s> s`; a timed-out
  request is abandoned (a late `COMPLETE` is `ESTALE`). **Two timeouts in a
  row**, the provider's death (`ipc::teardown_task`), a `GONE` status or a
  `REMOVE` mark the disk **dead**: every pending and future request fails at
  once with `Io`, and `is_writable` turns false.
- A mount on a dead disk stays mounted and fails its I/O: the session sees
  errors under `/home`, never a hang. Unplugging the stick makes `usbd`
  `REMOVE` the disk (`USBD:MSC:GONE`); a re-plugged stick is a new `usb<n>`
  and is not remounted automatically.
- The filesystem locks a requester holds while it waits (both mount tables,
  each ext2 volume) are `task::relax::YieldMutex`es: a contender parks for
  200 µs at a time (`relax::PARK_NS`), instead of spinning with interrupts
  off while the holder waits for `usbd`. It parks rather than yields because
  classes are strict: a contender that only yielded stayed the best pick
  whenever it outranked the holder, which then never ran to release the lock
  (the `xuid` boot hang, issue #609). The provider's own syscall path never
  takes them. A context that holds the task table cannot park and its request
  fails; so does one with less than 14 KiB of kernel stack left
  (`relax::can_block`), since parking puts the scheduler's frames on top of
  ext2's. Kernel stacks went from 32 to 48 KiB for this: a file created on
  the stick parks about 24 KiB deep, and the first end-to-end run overflowed.
- **A provider never waits for the VFS.** A requester holds its mount table
  (the native one, or the Linux ABI one for a BusyBox shell) while it waits
  for the provider, and a native mutation takes both tables
  (`fs::coherence`). So a native fs call from `usbd` while a `/home` request
  waits (its `usb.dump` for `dbgd`, written every 2 s while the bus is
  busy) blocked until the request's 10 s untaken deadline, twice, and the
  disk died. That was the real cause of issue #704: the box ran a `dbgd`
  build. Now a task that serves a live disk gets `EAGAIN` from the native
  fs syscalls while either table is held (`process::fsops`, checked with
  interrupts off on one core, so nothing takes a table between the check
  and the call), and `usbd` writes its dump on a later pass.
  `tools/boot/persist.py` reproduced it every time before the fix.
- Slots are never reused within a boot (a dead disk may still be mounted), so
  at most eight sticks are served per boot.

## The late `/home` mount

`lazyos.cfg` always names the home volume (`home=LABEL=lazyhome`). When no
kernel-driven disk carries it at boot, `fs::mounts` leaves `/home` a
directory on `/` and records the request in `fs::late`. `init`'s supervision
loop calls `SETTLE` every pass and holds back the rows that use `/home`
(`accountsd`, `logind`, the autostart apps) until the answer is final:

- Until `usbd` reports `SCANNED`, `SETTLE` answers `WAITING` and reads
  nothing: `usbd` is still bringing devices up, one at a time, and a read
  of the first stick while the second runs its TEST UNIT READY loop would
  time out (and two timeouts kill a disk). An end-to-end run lost `/home`
  that way before this rule.
- Then `SETTLE` scans the MBR of each new provider disk (`partition::scan_disk`,
  partitions `usb<n>p<k>`), then looks for the ext2 volume with the
  configured label or UUID on provider disks only, reclaims its orphans and
  mounts it at `/home` in the native and the ABI mount tables with the
  configured flags plus `nosuid` (`fs: mounted usb0p1 at /home (late, ...)`).
- Other sticks stay registered block devices, never mounted.
- `init` stops waiting when the volume is mounted, nothing is pending,
  `usbd` reported its first scan done without it (`ABSENT`), `usbd` is not
  running, or after **60 s** (`INIT:HOME <why>`). Images without `usbd`
  never wait.

## The read cache

The volume on a stick is opened *uncached* (`fs/mounts.rs::open_ext2`): the
write-back block cache flushes from the kernel task, which must never wait for
`usbd`, and a stick can be pulled. That left every read, metadata included, as
a round trip to the polled driver. Measured on a real PC with `/home` on a
stick, opening a directory or a file cost 0.27-0.33 s each (a desktop shell
that looked at its folder once a second kept the stick busy for ever).

`UserDisk` therefore keeps a **write-through read cache**
(`block/provider/readcache.rs`): clean 4 KiB pages of what was read, 512 per
disk, oldest out first.

- A read of 32 KiB or less is served from the pages when it is wholly there; a
  miss reads the whole pages around the request in one request and keeps them.
  Larger reads (file data) bypass it, neither using nor filling it.
- Writes are untouched in order and timing: each goes to the device before it
  returns. Pages it overlaps take the new bytes when it completes; a failed
  write drops them. So a page is never ahead of the device, and the cache
  holds nothing that needs writing back (nothing for the flusher, nothing to
  lose when the stick is pulled).
- A reader that missed stores its pages only if no write started or ended since
  it began (`ReadCache::epoch`), so a page read before a write landed is not
  kept.
- A dead disk answers `Io` before it looks at the cache, and a slot registered
  again starts empty.

## Durability

- ext2's sync paths flush the device; a provider flush is SYNCHRONIZE CACHE
  on the stick (a device that rejects it has no cache to flush).
- `usbd` also flushes a stick on its own one second after its last write, so
  a stick pulled without a shutdown keeps everything but its last second.
- **Shutdown** ([shutdown.md](../shutdown.md)): `init` does not stop `usbd`
  (`OUTLIVE` in `shutdown.rs`). The kernel's `power()` runs `sync_all`, which
  writes back and flushes `/home` through `usbd` and marks the volume clean;
  only then does the machine stop. The watchdog's forced stop kills `usbd`
  first, so the `/home` sync then fails fast and the volume stays dirty.

## Security

- **Capability and uid.** `CAP_BLOCK_PROVIDER` alone is not enough: the
  kernel also checks the uid, so a service that inherited the bit (no manifest
  row keeps it but `usbd`'s) cannot serve a disk.
- **Hostile device, hostile driver.** Every descriptor, CSW and SCSI reply
  is parsed from a copy by `libs/usbmsc` with checked lengths and bounded
  retries; the parsers are fuzzed. The kernel trusts nothing from `usbd`:
  lengths are the kernel's, tags must match the request in flight, data only
  ever lands in the bounce buffer, and a silent or dying driver costs a
  bounded wait. A provider can still return wrong bytes: the stick's content
  is untrusted like any removable medium's (ext2 parses it defensively,
  `nosuid` is forced).

## Testing

| Layer | What | Run |
|---|---|---|
| Host unit | `usbmsc` (41): golden descriptors (HS, SS with burst, composite), CBW/CSW, every recovery path against a fault-injecting model device, SCSI parsers. `xhci`: plus bulk TRBs, bulk contexts with burst, rings abandoned lap after lap | `cargo test -p usbmsc --features fuzz -p xhci` |
| Fuzz | `mscdesc`, `mscreply`, `mscsession` (seeded tests and cargo-fuzz) | `cargo test -p usbmsc --features fuzz` |
| Kernel | `provider_suite` (31): data path, splitting, flush, ext2 on a stick, the late mount, a 3000-request stress with transient errors, error statuses, forged and stale tags, silent and dying providers, the syscall gate and hostile lengths; the read cache: repeats stay off the stick, writes go through and update, a failed write forgets, a dead disk fails though cached, streams bypass, the epoch and eviction, a 3000-round random read/write soak against the device | `LAZYOS_TEST_FILTER=provider python tools/test/run.py --accel none` |
| End to end | QEMU with `qemu-xhci` and a `usb-storage` stick (MBR, ext2 `lazyhome`), the virtio boot disk and no home disk: log in on the console, write a file in `/home/user`, power off, boot again, read it back, power off; `e2fsck -fn` on the stick | `python tools/storage/run.py` |

## Not done

- UAS, LUNs other than 0, block sizes other than 512 bytes, more than one
  request in flight per stick. A stick behind a hub works like any device
  there, but QEMU's only hub is full speed, so that path is untested; its
  unplug is noticed by the hub's report, not by the transfer that fails.
- Re-mounting a stick that was unplugged and plugged back in: it comes back
  as a new disk, unmounted.
- Booting the kernel itself from the stick is the bootloader's business
  (real-pc-boot-plan.md); this only serves `/home`.
