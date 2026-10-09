# Block devices: ATA, PCI, virtio, NVMe, AHCI

**What it is.** The storage abstraction filesystems sit on: a `BlockDevice`
trait, a fixed registry with a selected boot device, and four drivers.

**Key files**

| Path | Role |
|---|---|
| `kernel/src/block/mod.rs` | `BlockDevice`, registry, boot device, probe order, `virt_to_phys` |
| `kernel/src/block/ata.rs` | ATA PIO primary-master driver (read path) |
| `kernel/src/block/mem.rs` | `MemDisk` over a memory region; the bootloader ramdisk registers as `ram0` (#5) |
| `kernel/src/dev/pci.rs` | PCI config-space access (0xCF8/0xCFC); moved here from `block/pci.rs` by the device core (#239) |
| `kernel/src/block/virtio.rs` (+ `virtio/plan.rs`, `ring.rs`, `queue.rs`, `io.rs`, `modern.rs`, `regs.rs`) | virtio-blk driver, read/write; one instance per PCI function, through the modern (1.x) transport when the function has one, the legacy (0.9.5) I/O window otherwise (issue #497, [drivers.md](drivers.md)) |
| `kernel/src/block/ahci.rs` (+ `libs/ahci`) | AHCI (SATA) driver (docs/ahci-plan.md A2): one block device per port with an ATA disk, polled READ/WRITE DMA EXT with up to 8 slots in flight, flush and standby at power-off |
| `kernel/src/block/nvme.rs` (+ `libs/nvme`) | NVMe driver (docs/nvme-install-plan.md N1): one polled I/O queue pair per controller, namespace 1, read/write/flush, shutdown notification |
| `kernel/src/block/iowait.rs` | How a request waits: park on deadlines (`Wait::MaySleep`) or spin (`Wait::Spin`); `breathe` for long CPU stretches |

**`BlockDevice` trait** (`mod.rs:86`)

| Method | Contract |
|---|---|
| `name` | registry name (`"ata0"`, `"virtio0"`) |
| `sector_size` / `sector_count` | geometry (512 bytes everywhere today) |
| `read_sectors` / `write_sectors` | whole sectors into/from a caller buffer |
| `read_sectors_vectored` / `write_sectors_vectored` | one consecutive sector range as a list of buffers (the ext2 cache's pages); default: one call per buffer, virtio-blk: requests of up to 256 KiB, several in flight, partitions translate |
| `read_sectors_vectored_with` / `write_sectors_vectored_with` | the same, saying how the caller may wait (`Wait::Spin`, `Wait::MaySleep`); the default ignores it, virtio-blk honours it, partitions forward it |
| `stats` | request counters ([`stats.rs`](../../kernel/src/block/stats.rs)), virtio-blk only; partitions answer `None` |
| `flush` | durability point; devices without a cache complete immediately |
| `is_writable` | whether `write_sectors` can succeed (default: false) |
| `check_range` | shared bounds validation via `check_range(...)` |

- Registry: fixed array of `&'static dyn BlockDevice`, `MAX_DEVICES = 24`, no
  heap. Drivers are `'static` singletons (`ata::probe`, `virtio::probe`).
- `init()` is a thin wrapper over `dev::init` (device core, #239). The one-shot
  guards are `dev::INITED` (enumeration) and `dev::driver::PROBED` (driver
  attach), not a block-layer flag. The in-kernel driver table attaches ATA first
  (`install_ata`; stays the fallback boot device), then virtio (`install_virtio`,
  once per matching function), whose *first* device takes the boot slot; later
  ones (a data disk) are registered without displacing it. Filesystems read through
  the `BlockDevice` handle they were opened with, not the global boot device;
  `virt_to_phys` walks the active page table for DMA descriptors.

**Drivers**

| Driver | Transport | Read | Write | Notes |
|---|---|---|---|---|
| `ata` | PIO, ports 0x1F0-0x1F7 | yes | no (default `ReadOnly`) | 28-bit LBA, polled, `IDENTIFY DEVICE` for geometry; `IO` mutex serializes |
| `virtio` | modern PCI (capabilities, memory BAR, `libs/virtio`) or legacy BAR0 I/O window | yes | yes | per function (up to 4, `virtio0`..`virtio3`): own queue 0 split virtqueue in static memory, up to 8 requests in flight (up to 256 KiB each, DMA straight to and from the caller's buffers), polled, `is_writable` = attached |
| `ahci` | PCIe, class 01:06 prog-if 01 (any vendor), ABAR (BAR 5) mapped uncached | yes | yes | up to 4 ports across all controllers (`ahci0`..`ahci3`, port order): command list, received-FIS area and one command table per slot in static memory, polled (no interrupts), up to 8 commands of 256 KiB in flight, PRDT entries straight to the caller's buffers (a bounce page for odd addresses, and for pages above 4 GiB on an HBA without `CAP.S64A`), `FLUSH CACHE EXT` after the power path's sync, `STANDBY IMMEDIATE` at power-off; only 512-byte logical sectors with LBA48 and `FLUSH CACHE EXT` are served; ATAPI and RAID-mode controllers are skipped |
| `nvme` | PCIe, class 01:08:02, BAR0 mapped uncached (`mem::mmio::map_kernel`) | yes | yes | up to 2 controllers (`nvme0`, `nvme1`): admin and one I/O queue pair in static memory, polled (no interrupts), up to 8 commands of 64 KiB in flight (fewer when `MDTS` says so), PRP entries straight to the caller's buffers (a bounce page for buffers that are not dword aligned), Flush when the controller has a volatile write cache, `CC.SHN` after the power path's sync; only 512-byte LBA formats are served |
| `pci` | config mechanism 1 (`kernel/src/dev/pci.rs`) | - | - | enumerate bus/device/function, match vendor/device, decode + size BARs (32/64-bit), command register, capability walk, interrupt line; no MMCONFIG/MSI |

- ATA is read-only because the write path was not needed for the FAT boot image;
  ext2 write traffic requires virtio-blk (`-drive if=virtio`). An ext2 volume on
  ATA still mounts, read-only, and logs that its mount (`/` or the legacy `/data`)
  is read-only.
- Several virtio-blk functions are independent devices: each `Slot` owns its
  queue and its request slots' control blocks, so a boot disk and a data disk
  never share ring state. The registry names them in PCI enumeration order.
- Virtio negotiates no feature bits and detects but does not drive modern-only
  devices (`1af4:1042`), which need BAR mapping in the kernel page table (next
  step, `virtio.rs` module docs).
- DMA (docs/performance-plan.md P5): the device reads and writes the
  caller's own buffers. The kernel heap maps scattered frames and a stack
  slice can straddle pages, so `virtio/plan.rs` cuts a transfer into requests
  of up to 64 pieces that never cross a page (each translated with
  `virt_to_phys` as it is planned) and 256 KiB, ending on a sector boundary;
  a segment may straddle two requests. One request is a descriptor chain of
  its slot's header, the pieces, and its slot's status byte. Up to
  `MAX_INFLIGHT` (8) requests from any callers are in the queue at once
  (`virtio/ring.rs`): the device lock is held only to submit and to reap,
  whoever holds it reaps every completion (freeing its descriptors and
  marking its request done), and the owner takes its result. A transfer
  never returns while the device may still touch its buffers: it waits for
  every request it submitted, and a request still outstanding after 10 s
  resets the device (after which it touches no memory), failing every request
  then in flight and setting the queue up again. A queue too small for the
  66-descriptor chain is refused at attach. ATA PIO has no such constraint.
  The driver is split into `virtio.rs` (transfers and the device),
  `virtio/plan.rs`, `virtio/ring.rs`, `virtio/io.rs` (ports, attach, reset)
  and `virtio/queue.rs` (ring memory and descriptors).
- Waiting (`block/iowait.rs`): syscalls run with interrupts off, and the
  driver used to busy-wait inside them, stopping the machine for every request
  (a 1.6 s stretch in one `write`). A caller passing `Wait::MaySleep` parks
  instead when the scheduler runs and `task::relax::can_block` holds: first
  at about 3/4 of the device's usual answer time (a running average per
  device and direction), then in slices of 20 to 250 µs on the P2 one-shot
  deadline timer. The device's INTx line is shared with the network card's
  user-space driver (line 11 on QEMU's machine), which a kernel handler
  cannot share with a claimant, so the driver polls the used ring and asks for
  no interrupts (`VRING_AVAIL_F_NO_INTERRUPT`). A killed waiter keeps waiting
  in naps until the device is done with its buffers. Every other caller
  (FAT, partition scans, boot-time mounts, the test suite's own task) passes
  `Wait::Spin` and busy-waits, draining the i8042. A spin's deadline is
  read from the TSC (`monotonic_ns` cannot pass one tick while interrupts
  are off) and every 1024 spins is an `irq_window` poll point, so a device
  that never answers costs its timeout, not minutes with the timer shut out
  (issue #449, `block_sleep_spin_ends_by_deadline`).
- Who may sleep: the ext2 adapter, while it holds its volume gate
  (`fs/ext2/volio.rs`). A gate holder holds the mount table (FS or ABI_FS) and
  the gate, all `task::relax::YieldMutex`es, and the library's lock and block
  cache lock, plain spin locks only ever reached through the gate (the
  flusher try-locks it): a contender meets a yielding lock first, exactly as
  under the USB block provider, which already parks there. FAT does not hold
  its FAT-sector cache lock across a device read for the same reason.
  `iowait::breathe` lets interrupts in during long CPU stretches under the
  same rule (at most every 50 µs): between ext2 pieces, at the library's pause
  points (`ext2fs::Ext2::set_pause`), between loader chunks, between staged
  user copies and between serial-mirror chunks.

**Vectored requests and counters.** The ext2 block cache
([`block-cache.md`](block-cache.md)) writes back runs of consecutive blocks
whose pages are scattered frames, and reads ahead the same way; virtio-blk
points the device at those pages directly, so a 256 KiB run is one request. Every virtio request is
counted (`IoStats`: reads, writes, bytes); once a disk has been idle for 3 s
after activity the kernel task prints
`block: virtio0 reads N (K KiB) writes M (K KiB) flushes F`, which is how the
harnesses read the cost of a run. There is no cache in the block layer itself:
the ext2 driver caches above it (the block-cache doc says why), FAT `/boot`
reads go straight to the device.

**Partitions (`block/partition.rs`, plan F1).** After the drivers attach,
`block::init` reads the MBR of every whole disk (not partitions; `ram0` scans
itself when it registers, below) and
registers each entry of type `0x83` or FAT (`01 04 06 0B 0C`) as `<disk>p<n>`,
1-based like the MBR slot (`virtio0p3`). The table is hostile input: an entry
must have `sectors > 0`, `lba >= 1` and `lba + sectors <= disk` (checked add);
overlapping entries are both dropped, extended (`05`, `0F`) and protective
(`EE`) entries are logged and skipped, and nothing is clamped. `Partition`
implements `BlockDevice` by delegation: `check_range` against the partition,
then the offset is added with `checked_add`. Slots are a static pool
(`MAX_PARTITIONS = 16`, names in static storage), so the registry still holds
`&'static dyn BlockDevice` with no heap; a full pool or registry is logged.
`Ext2::open` of a whole disk with a partition table fails on the superblock
magic and must keep doing so. A sector that is a FAT volume boot record (a
jump, then `FAT` at byte 54 or `FAT32` at 82) is not read as a table, so a bare
FAT image registers no partitions. `partition::scan_disk(disk) -> usize` is the
entry point for a disk that appears after boot (the USB stick through `usbd`):
it registers `<disk>p<n>` once per disk and refuses partition devices. Tests:
`partition_*` in `tests/partition_suite.rs`.

**Ramdisk (#5, the USB stick's RAM root).** The bootloader hands over a
ramdisk: the USB stick image's whole-disk image (MBR, FAT `lazyos.cfg`, ext2 OS
volume; [`../usb-stick.md`](../usb-stick.md)) or a bare FAT image from
`LAZYOS_RAMDISK=<fat image>`. `kernel_main` registers `BootInfo.ramdisk_*` as
`ram0` (writable, so a root on it is a RAM root) after probing ATA/virtio, and
`register_ramdisk` scans its MBR (`ram0p1`, `ram0p2`). **When a ramdisk is
present and its boot volume carries `lazyos.cfg`, it wins:**
`fs::mounts::build` moves `ram0` and its partitions ahead of every other
device, so the boot volume, `lazyos.cfg` and the root all come
from it, and a disk carrying a volume with the same UUID (a QEMU dev run that
also attaches `target/lazyos.img`) can never take `/`; the choice does not
depend on probe order. A ramdisk without `lazyos.cfg` (a bare
`LAZYOS_RAMDISK` FAT image) keeps the registration order. The chosen root is logged as `FS:ROOT:<device>`
(`FS:ROOT:none` when nothing mounted). Disks are still searched for the home
volume. `Fat16::open` accepts a bare (MBR-less) image whose sector 0 is the
BPB. Tests: `block_memdisk_*`, `block_ramdisk_*` in `tests/ramdisk_suite.rs`,
`partition_fat_vbr_not_a_table`, and `mount_ramdisk_*` in
`tests/mount_suite/ramdisk_root.rs`.

**Boot device and mount interaction**

- `fs::init` (`fs/mounts.rs`) first looks for a FAT volume carrying
  `lazyos.cfg`: when it names a root, that ext2 volume (found by UUID, on any
  device or partition) is `/`, the FAT volume is `/boot` (read-only), and an
  optional home volume is `/home`; nothing is probed for `/data`. The rest of
  this list is the **legacy layout**, used without a `lazyos.cfg` or when its
  root is missing.
- The legacy layout iterates `block::devices()`, tries FAT then ext2 on each
  device it is handed, and mounts the first success at `/`. The volume keeps that
  device handle, so a second volume never reads the wrong disk (#244).
- It then probes the other devices for ext2 and mounts the first at `/data`.
  Which bus each disk is on does not matter (the boot disk may be IDE with the
  data disk the only virtio device, or both virtio); only the ext2 magic does.
- Bench/test doubles register through the same `register()` API (tests use a
  fake device name pattern).

**Drivers as device-core drivers (#239).** ATA and legacy virtio-blk are also
registered in the kernel device core's static `Driver` table
([`devices.md`](devices.md)): `dev::init` seeds the ISA ATA controller as a
platform device, enumerates PCI, then runs the table, which calls the same
`ata::probe`/`virtio::probe` and registers them through the block registry
exactly as before. `block::init` is now a thin idempotent wrapper over
`dev::init`, so all probe paths (boot, `fs::init`, kernel tests) behave the
same.

**NVMe (`block/nvme.rs`, docs/nvme-install-plan.md N1).** The protocol is
`libs/nvme`, a pure `no_std` crate host-tested against a model controller
(bring-up, every PRP shape, media errors, stray completions, a hung command,
a controller that never comes ready) with seeded fuzz of the Identify pages,
completions, the PRP planner and a fully hostile controller. The kernel
supplies its `Platform`: BAR0 in the kernel MMIO window (firmware may place a
64-bit BAR above the physical-memory map), queue, Identify and PRP-list pages
in static memory, and a TSC clock (bring-up runs with interrupts off). The
driver entry comes after virtio in the device-core table and takes the boot
slot only when no other disk did; the root is still chosen by UUID from
`lazyos.cfg`, so the dev image boots from NVMe alone (`FS:ROOT:nvme0p3`).
Every wait is bounded: a timeout or `CSTS.CFS` disables the controller and the
device then answers `Io`. The device lock is a `YieldMutex` held for a whole
transfer, so a `Wait::MaySleep` caller parks in `iowait` while commands run.
Tests: `nvme_suite` against a scratch disk (`tools/test/run.py --nvme`), and
`tools/boot/run.py --media nvme` boots an image from QEMU's `-device nvme`.

**AHCI (`block/ahci.rs`, docs/ahci-plan.md A2/A3).** The protocol is
`libs/ahci`, a pure `no_std` crate host-tested against a model HBA (handoff,
empty/ATAPI/unknown ports, IDENTIFY variants, every PRDT shape, task file
errors with commands in flight, COMRESET, a port that will not stop, short
`PRDBC`, hung commands) with seeded fuzz of IDENTIFY, the PRDT planner and a
fully hostile HBA. Errors are read from `PxIS` only (`PxTFD.ERR` keeps the
last error until another command overwrites it); a port is detached after two
failed recoveries in a row, not after media errors. The kernel supplies the
`Platform` as for NVMe and shares its DMA page and bounce helpers
(`block/dma.rs`). Tests: `ahci_suite` against a scratch disk
(`tools/test/run.py --ahci`), `tools/boot/run.py --media ahci`, and
`tools/run_demo.py --disk ahci` (also on the launcher) to try it by hand.

**User-space block providers (`block/provider.rs`).** A ring-3 driver A ring-3 driver
holding `CAP_BLOCK_PROVIDER` (only `usbd`, for a USB stick) registers a
`UserDisk` through syscall 33; it joins the registry as `usb<n>` and is driven
like any other device, one request at a time through a kernel bounce buffer,
with a per-request timeout and the disk failing fast once its provider dies.
Its partitions are scanned when the late home mount runs (`fs::late`), not
at `block::init`. Details: [usb-storage.md](usb-storage.md).

**Status.** Working: ATA reads (default QEMU image), virtio-blk reads/writes on
several functions with sleeping waits and several requests in flight, PCI
enumeration. Open: modern virtio (memory BAR), an interrupt-driven virtio-blk
(needs a PIC line a kernel handler can share with a user-space claimant),
AHCI/NVMe, ATA writes. Tests: `virtio_suite`, `block_sleep_suite` (planner
rules and soak, kernel threads parked in the driver beside a spinning caller,
a killed waiter, four threads on one cached ext2 volume).
