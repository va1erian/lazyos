# Block devices: ATA, PCI, virtio

**What it is.** The storage abstraction filesystems sit on: a `BlockDevice`
trait, a fixed registry with a selected boot device, and three drivers.

**Key files**

| Path | Role |
|---|---|
| `kernel/src/block/mod.rs` | `BlockDevice`, registry, boot device, probe order, `virt_to_phys` |
| `kernel/src/block/ata.rs` | ATA PIO primary-master driver (read path) |
| `kernel/src/block/mem.rs` | `MemDisk` over a memory region; the bootloader ramdisk registers as `ram0` (#5) |
| `kernel/src/block/pci.rs` | Legacy PCI config-space access (0xCF8/0xCFC) |
| `kernel/src/block/virtio.rs` | Legacy virtio-blk (0.9.5) driver, read/write |

**`BlockDevice` trait** (`mod.rs:86`)

| Method | Contract |
|---|---|
| `name` | registry name (`"ata0"`, `"virtio0"`) |
| `sector_size` / `sector_count` | geometry (512 bytes everywhere today) |
| `read_sectors` / `write_sectors` | whole sectors into/from a caller buffer |
| `flush` | durability point; devices without a cache complete immediately |
| `is_writable` | whether `write_sectors` can succeed (default: false) |
| `check_range` | shared bounds validation via `check_range(...)` |

- Registry: fixed array of `&'static dyn BlockDevice`, `MAX_DEVICES = 8`, no
  heap. Drivers are `'static` singletons (`ata::probe`, `virtio::probe`).
- `init()` probes once (`PROBED`): ATA first (stays the fallback boot device),
  then virtio which takes the boot slot when present. Filesystems read through
  the `BlockDevice` handle they were opened with, not the global boot device;
  `virt_to_phys` walks the active page table for DMA descriptors.

**Drivers**

| Driver | Transport | Read | Write | Notes |
|---|---|---|---|---|
| `ata` | PIO, ports 0x1F0-0x1F7 | yes | no (default `ReadOnly`) | 28-bit LBA, polled, `IDENTIFY DEVICE` for geometry; `IO` mutex serializes |
| `virtio` | legacy PCI, BAR0 I/O window | yes | yes | queue 0 split virtqueue in static memory, one request at a time, 4 KiB bounce page, `is_writable` = device present |
| `pci` | config mechanism 1 | - | - | enumerate bus/device/function, match vendor/device, read BARs; no MMCONFIG/MSI |

- ATA is read-only because the write path was not needed for the FAT boot image;
  ext2 write traffic requires virtio-blk (`-drive if=virtio`).
- Virtio negotiates no feature bits and detects but does not drive modern-only
  devices (`1af4:1042`), which need BAR mapping in the kernel page table (next
  step, `virtio.rs` module docs).
- DMA buffers must be physically contiguous: virtio copies through a `'static`
  4 KiB bounce page because the kernel heap maps scattered frames; ATA PIO has
  no such constraint.

**Ramdisk fallback (#5).** With `LAZYOS_RAMDISK=<fat image>` the build hands
the image to the bootloader; `kernel_main` registers `BootInfo.ramdisk_*` as
`ram0` *after* probing ATA/virtio, so a real disk keeps priority and `fs::init`
falls through to `ram0` when no disk has a volume. `Fat16::open` accepts a bare
(MBR-less) image whose sector 0 is the BPB. Tests: `block_memdisk_*`,
`block_ramdisk_*` in `tests/ramdisk_suite.rs`.

**Boot device and mount interaction**

- `fs::init` iterates `block::devices()`, tries FAT then ext2 on each device
  it is handed, and mounts the first success at `/`. The volume keeps that
  device handle, so a second volume never reads the wrong disk (#244).
- Bench/test doubles register through the same `register()` API (tests use a
  fake device name pattern).

**Status.** Working: ATA reads (default QEMU image), virtio-blk reads/writes,
PCI enumeration. Open: modern virtio (memory BAR), AHCI/NVMe, ATA writes, DMA
rings for drivers beyond the bounce-buffer path.
