# Block devices: ATA, PCI, virtio

**What it is.** The storage abstraction filesystems sit on: a `BlockDevice`
trait, a fixed registry with a selected boot device, and three drivers.

**Key files**

| Path | Role |
|---|---|
| `kernel/src/block/mod.rs` | `BlockDevice`, registry, boot device, probe order, `virt_to_phys` |
| `kernel/src/block/ata.rs` | ATA PIO primary-master driver (read path) |
| `kernel/src/block/mem.rs` | `MemDisk` over a memory region; the bootloader ramdisk registers as `ram0` (#5) |
| `kernel/src/dev/pci.rs` | PCI config-space access (0xCF8/0xCFC); moved here from `block/pci.rs` by the device core (#239) |
| `kernel/src/block/virtio.rs` | Legacy virtio-blk (0.9.5) driver, read/write; one instance per PCI function |

**`BlockDevice` trait** (`mod.rs:86`)

| Method | Contract |
|---|---|
| `name` | registry name (`"ata0"`, `"virtio0"`) |
| `sector_size` / `sector_count` | geometry (512 bytes everywhere today) |
| `read_sectors` / `write_sectors` | whole sectors into/from a caller buffer |
| `read_sectors_vectored` / `write_sectors_vectored` | one consecutive sector range as a list of buffers (the ext2 cache's pages); default: one call per buffer, virtio-blk: as few 64 KiB requests as the range allows, partitions translate |
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
| `virtio` | legacy PCI, BAR0 I/O window | yes | yes | per function (up to 4, `virtio0`..`virtio3`): own queue 0 split virtqueue in static memory, one request at a time (up to 64 KiB, a descriptor per 4 KiB page of the 16-page bounce region), `is_writable` = attached |
| `pci` | config mechanism 1 (`kernel/src/dev/pci.rs`) | - | - | enumerate bus/device/function, match vendor/device, decode + size BARs (32/64-bit), command register, capability walk, interrupt line; no MMCONFIG/MSI |

- ATA is read-only because the write path was not needed for the FAT boot image;
  ext2 write traffic requires virtio-blk (`-drive if=virtio`). An ext2 volume on
  ATA still mounts, read-only, and logs that its mount (`/` or the legacy `/data`)
  is read-only.
- Several virtio-blk functions are independent devices: each `Slot` owns its
  queue, request header and bounce page, so a boot disk and a data disk never
  share ring state. The registry names them in PCI enumeration order.
- Virtio negotiates no feature bits and detects but does not drive modern-only
  devices (`1af4:1042`), which need BAR mapping in the kernel page table (next
  step, `virtio.rs` module docs).
- DMA buffers need physical addresses: virtio copies through a `'static`
  bounce region because the kernel heap maps scattered frames, and the request
  path never assumes that region is physically contiguous. One request is a
  descriptor chain of the header, one descriptor per 4 KiB page (each address
  translated with `virt_to_phys` at attach), and the status byte; at most
  `MAX_REQUEST_BYTES` (64 KiB) per request, one request in flight, polled. A
  queue too small for the 18-descriptor chain is refused at attach. ATA PIO has
  no such constraint. The driver is split into `virtio.rs` (request path and
  device), `virtio/io.rs` (ports and attach) and `virtio/queue.rs` (ring
  memory and descriptors).

**Vectored requests and counters.** The ext2 block cache
([`block-cache.md`](block-cache.md)) writes back runs of consecutive blocks
whose pages are scattered frames, and reads ahead the same way; virtio-blk
gathers such a list into its bounce region and scatters reads back out of it
(`virtio/gather.rs`), so a 64 KiB run is one request. Every virtio request is
counted (`IoStats`: reads, writes, bytes); once a disk has been idle for 3 s
after activity the kernel task prints
`block: virtio0 reads N (K KiB) writes M (K KiB) flushes F`, which is how the
harnesses read the cost of a run. There is no cache in the block layer itself:
the ext2 driver caches above it (the block-cache doc says why), FAT `/boot`
reads go straight to the device.

**Partitions (`block/partition.rs`, plan F1).** After the drivers attach,
`block::init` reads the MBR of every whole disk (not `ram0`, not partitions) and
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
magic and must keep doing so. Tests: `partition_*` in `tests/partition_suite.rs`.

**Ramdisk fallback (#5).** With `LAZYOS_RAMDISK=<fat image>` the build hands
the image to the bootloader; `kernel_main` registers `BootInfo.ramdisk_*` as
`ram0` *after* probing ATA/virtio, so a real disk keeps priority and `fs::init`
falls through to `ram0` when no disk has a volume. `Fat16::open` accepts a bare
(MBR-less) image whose sector 0 is the BPB. Tests: `block_memdisk_*`,
`block_ramdisk_*` in `tests/ramdisk_suite.rs`.

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

**Status.** Working: ATA reads (default QEMU image), virtio-blk reads/writes on
several functions, PCI enumeration. Open: modern virtio (memory BAR), AHCI/NVMe, ATA writes, DMA
rings for drivers beyond the bounce-buffer path.
