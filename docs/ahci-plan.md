# AHCI (SATA) block driver — plan

> **Status: revision 2 (2026-10-09). A1 (`libs/ahci`: model HBA tests, fuzz,
> clippy) and the code of A2/A3 (kernel adapter, `--ahci`, `--media ahci`,
> `--disk ahci`, flush and standby in the power path, CI jobs) are written.
> The kernel suite and the CI jobs have not run yet (no QEMU where this was
> written), and A0 (checking offsets against the specification) and A4 (the
> box) are open: the *to confirm* marks stand.**
>
> Deviations from revision 1: `run_demo.py` already had `--disk
> {virtio,ata}`, so AHCI is a third value of it (`--disk ahci`), not a new
> `--disk-bus`; the AHCI test disk sits on its own `-device ahci` rather
> than q35's built-in controller, so it works on every machine type.
> The disk driver for the Kaby Lake box ([kabylake-box-plan.md](kabylake-box-plan.md):
> a 512 GB SATA SSD on a Sunrise Point-LP AHCI controller, `8086:9d03`, no
> NVMe), and for any PC whose SATA runs in AHCI mode. It follows NVMe N1
> ([nvme-install-plan.md](nvme-install-plan.md)) piece for piece: a
> host-tested `no_std` protocol crate, a thin kernel adapter, polled
> completions, the same block-layer face. Facts about the AHCI and ATA
> standards come from the public AHCI 1.3.1 specification and ATA/ATAPI-8
> (ACS); register offsets and bits are checked against them in A0 and marked
> *to confirm* until then.

Related: [architecture/block-devices.md](architecture/block-devices.md) (the
`BlockDevice` trait, registry, partitions, `iowait`),
[driver-plan.md](driver-plan.md) D1 (block drivers are kernel drivers: `/`
must be readable before `init`), [real-pc-boot-plan.md](real-pc-boot-plan.md)
H4 (AHCI was a named seam), [architecture/block-cache.md](architecture/block-cache.md).

## 1. Short answer

1. **In the kernel, like NVMe.** `libs/ahci` (`no_std`, no syscalls, a
   `Platform` seam for MMIO, DMA pages and a clock, host-tested against a
   model HBA that can lie) and `kernel/src/block/ahci.rs` (+ `ahci/hw.rs`,
   `ahci/bounce.rs`) as its machine side. Matched by class `01:06` prog-if
   `01` (AHCI 1.0) on any vendor, in the static `DRIVERS` table after virtio
   and NVMe.
2. **v1 scope:** every implemented port with an ATA disk attached becomes a
   device (`ahci0`, `ahci1`, … in port order across controllers); 48-bit LBA
   `READ DMA EXT`/`WRITE DMA EXT`, `FLUSH CACHE EXT`, `STANDBY IMMEDIATE` at
   power-off; up to 8 commands in flight per port through 8 command slots
   (non-NCQ commands still run one at a time on the wire, but queueing them
   in slots lets the driver overlap setup and completion); polled, no
   interrupts; 512-byte logical sectors only (a 4Kn disk is refused with a
   log line, as NVMe refuses a 4 KiB namespace).
3. **Out of v1:** NCQ (`READ/WRITE FPDMA QUEUED`), TRIM (`DATA SET
   MANAGEMENT`), interrupts and MSI, hot-plug, port multipliers, ATAPI
   (optical drives are skipped by signature), SMART, link power management,
   RAID mode (Intel RST presents class `01:04`, which does not match).
4. **Testing is easy here:** QEMU models AHCI (ICH9; built into `-machine
   q35`, `-device ahci` elsewhere), so the kernel suite, a soak and a
   boot-from-AHCI CI job run without hardware. The box only confirms.

## 2. Where it goes

| Path | Change |
|---|---|
| `libs/ahci/` (new) | `regs.rs` (HBA and port registers, bits), `fis.rs` (Register H2D FIS, the received-FIS area), `cmd.rs` (command header, command table, PRDT planning), `identify.rs` (IDENTIFY DEVICE: model, capacity, logical/physical sector size, LBA48, write cache, flush support), `port.rs` (the per-port state machine: stop, start, reset, issue, reap, error recovery), `hba.rs` (BIOS handoff, `GHC.AE`, port discovery), `fake.rs`, `tests/`, `fuzz.rs`; every file under 500 lines |
| `kernel/src/block/ahci.rs`, `ahci/hw.rs`, `ahci/bounce.rs` (new) | the `Platform` (ABAR mapped with `mem::mmio::map_kernel`, static DMA pages, the TSC clock), the `BlockDevice` per port, `YieldMutex` per port, `iowait` waits |
| `kernel/src/dev/driver.rs` | an `ahci` row after `nvme`; `crate::block::install_ahci` |
| `kernel/src/block/mod.rs` | `install_ahci`; AHCI takes the boot slot only when no earlier driver found a disk (the NVMe rule) |
| power path (where NVMe sets `CC.SHN`) | after the final sync: `FLUSH CACHE EXT`, then `STANDBY IMMEDIATE`, per port |
| `kernel/src/tests/ahci_suite.rs` (new) | §4 |
| `tools/test/run.py`, `.github/workflows/kernel-tests.yml` | `--ahci` (a blank scratch disk on the AHCI bus, like `--nvme`), a `kernel-tests (ahci)` matrix row, an `ahci-boot` job beside `nvme-boot` |
| `tools/run_demo.py`, `tools/lazygui/` (+ tests) | a new `--disk-bus ahci` (boot the image from QEMU's AHCI on q35) and the same choice on the launcher's Advanced tab, tested in `test_catalog.py`. Neither front end has a disk-bus option today (NVMe shipped without one); this is for trying the driver by hand, not a build switch, since the driver is in every kernel |
| `docs/architecture/block-devices.md` | the driver row and its notes |

## 3. Design

### 3.1 Controller bring-up (`hba.rs`)

1. Map ABAR (BAR 5, *to confirm* per spec; a memory BAR) whole through
   `map_kernel`, refusing one shorter than `0x100 + 0x80 × (highest port + 1)`.
2. **BIOS/OS handoff** when `CAP2.BOH` is set: set `BOHC.OOS`, wait (bounded,
   25 ms then up to 2 s while `BOHC.BB` is set) for `BOHC.BOS` to clear. The
   Sunrise Point controller may not implement it *to confirm*; the code path
   is tested on the fake either way.
3. Set `GHC.AE`; leave `GHC.IE` clear (polled). No HBA reset (`GHC.HR`) in v1:
   firmware has initialised the ports and a reset restarts every link for
   nothing; the per-port reset below is the recovery tool. *Decision for A0*:
   if a real controller turns out to need `GHC.HR`, it is added behind the
   same bounded wait.
4. Read `CAP` (`NP`, `NCS` command slots, `S64A`, `SSS`), `PI` (implemented
   ports; trusted only as far as `NP` allows) and `VS`.
5. For each implemented port, bring it up (3.2); a port that fails is logged
   and skipped, never fatal for the others.

Intel PCH controllers have a port-enable register in PCI config space
(`PCS`, 0x92 on older chipsets); Linux enables ports there on some boards.
Whether Sunrise Point needs it is *to confirm* in A0; the firmware has
enabled the box's one port, and the driver only reads it in v1.

### 3.2 Port bring-up (`port.rs`)

1. **Stop** the port: clear `PxCMD.ST`, wait (bounded, 500 ms) for `PxCMD.CR`
   to clear; clear `PxCMD.FRE`, wait for `PxCMD.FR`. A port that will not
   stop gets a COMRESET (3.4) and is skipped if it still will not.
2. Point `PxCLB`/`PxCLBU` at a 1 KiB command list (1 KiB aligned) and
   `PxFB`/`PxFBU` at a 256-byte received-FIS area (256-byte aligned), from
   the controller's static DMA pages; without `CAP.S64A` every DMA address
   must be below 4 GiB, which the static pages are (checked, not assumed).
3. Set `PxCMD.FRE`; clear `PxSERR` (write ones) and `PxIS`.
4. **Presence:** `PxSSTS.DET == 3` (device present, PHY up) and `IPM == 1`
   (active). Otherwise the port is empty: no device, no error.
5. Wait (bounded, 1 s) for `PxTFD` to show neither `BSY` nor `DRQ`; then
   `PxSIG`: `0x0000_0101` is an ATA disk, `0xEB14_0101` ATAPI (skipped,
   logged), anything else skipped and logged.
6. Set `PxCMD.ST`. Issue IDENTIFY DEVICE (3.3). Refuse a disk without LBA48,
   with a logical sector other than 512 bytes, or with a capacity of zero;
   log model, capacity, physical sector size and write cache.
7. Register the port's `BlockDevice`; the block layer scans its partitions
   as for every other disk.

### 3.3 Commands (`cmd.rs`, `fis.rs`)

A command is a slot: its 32-byte header in the command list (`CFL` = 5
dwords, `W` for writes, `PRDTL`), its command table (the H2D register FIS,
then the PRDT), the `PxCI` bit to issue it. Each slot owns a fixed command
table in static memory with room for 64 PRDT entries.

- **Data path:** the device reads and writes the caller's buffers directly,
  each page translated with `virt_to_phys` (the virtio and NVMe rule). A
  transfer is cut into commands of at most 64 entries and 256 KiB, no entry
  crossing a page. Without `CAP.S64A` the HBA cannot address memory above
  4 GiB, and caller buffers (heap pages) can lie there: the planner checks
  every entry it builds, not only the static DMA pages, and a buffer with
  any page at or above 4 GiB goes through the port's bounce page, which is
  itself checked to be below 4 GiB at attach (the port is refused if not).
  AHCI requires each entry's address to be word aligned
  and its byte count even (`DBC` holds count − 1 with bit 0 set, *to
  confirm*); a buffer that breaks that goes through the port's bounce page,
  as NVMe's non-dword-aligned buffers do.
- **Commands used:** `READ DMA EXT` (`0x25`), `WRITE DMA EXT` (`0x35`),
  `FLUSH CACHE EXT` (`0xEA`), `IDENTIFY DEVICE` (`0xEC`), `STANDBY IMMEDIATE`
  (`0xE0`). Sector counts up to 65536 per command (0 means 65536) are never
  needed: 256 KiB is 512 sectors.
- **Completion:** poll `PxCI` for the slot's bit to clear, and `PxIS` /
  `PxTFD` for errors, from the waiter in `iowait` (park or spin, as the
  caller allows). Up to 8 slots are in flight per port; the device lock is
  held to issue and to reap, as in virtio-blk. A transfer never returns
  while the HBA may still touch its buffers.
- **What the device reports is untrusted:** `PRDBC` (bytes transferred) must
  equal what was asked or the command failed; a `PxCI` bit clearing for a
  slot never issued, or `PxTFD.ERR` with a slot still set, is an error, never
  an index; IDENTIFY's words are bounds-checked (capacity against LBA48's
  range, sector size words only when their validity bits say so).

### 3.4 Errors and timeouts

- **Task file error** (`PxIS.TFES`, `PxTFD.ERR`): the failing request returns
  `BlockError::Io`; every other in-flight request is failed too (a non-NCQ
  error stops the port's command processing *to confirm*). Recovery: stop
  the port (3.2 step 1), clear `PxSERR` and `PxIS`, and if `PxTFD` still
  shows `BSY` or `DRQ`, a **COMRESET**: `PxSCTL.DET = 1`, wait at least 1 ms,
  `DET = 0`, wait (bounded, 1 s) for `PxSSTS.DET == 3`, then the presence and
  signature checks again. Then start the port. The device stays registered.
- **Timeout:** a command unanswered for 10 s (the NVMe and virtio bound) is
  the same recovery. A port that fails recovery twice in a row is detached:
  its device stays registered and answers `BlockError::Io` (the NVMe rule),
  and the HBA touches no more of its memory (the port is stopped, `FRE`
  clear, before anything is freed; nothing is ever freed while it runs).
  A port that will not stop at all (`PxCMD.CR` stays set after COMRESET) is
  detached at once and flagged DMA-unsafe: the HBA may still be using the
  caller's buffers, so the kernel clears the controller's PCI bus-master
  bit (every port on it) before the request returns.
- **Host bus errors** (`PxIS.HBFS`, `HBDS`, `IFS`): treated as a timeout.
- **Media errors** reach the filesystem as `Io`, which ext2 already handles
  (read-only remount on a write failure, as today).

### 3.5 Power-off and reboot

The power path already syncs every filesystem, then tells NVMe to shut down.
AHCI adds, per registered port: `FLUSH CACHE EXT` (bounded, 30 s: an SSD with
a large cache), then `STANDBY IMMEDIATE` so the drive parks and commits
before power is cut. Reboot does the flush only. Failures are logged, never
block the power-off.

### 3.6 Coexistence

- **`-machine q35` has an AHCI controller in every run** (`8086:2922` at
  00:1f.2), usually with no disk or with QEMU's CD-ROM. Every existing q35
  harness therefore runs the probe: empty ports and an ATAPI signature must
  cost nothing but a log line, and the probe's total wait on an empty
  controller stays under 10 ms. A q35 run that attaches an IDE drive
  (`if=ide`) puts it on this AHCI controller, where the legacy ATA PIO driver
  never saw it: such a disk now appears as `ahci0`. Root selection is by
  UUID (`lazyos.cfg`), so nothing changes which volume mounts.
- **Order:** virtio, then NVMe, then AHCI in `DRIVERS`; AHCI takes the boot
  slot only when nothing else did.
- **ATA PIO** stays as it is: it drives legacy IDE ports, which an AHCI-mode
  PCH does not have (`firmware_suite` already checks a floating bus).

## 4. Testing

Every row is a planned acceptance criterion; none has run.

| Layer | What | Where |
|---|---|---|
| Host unit | the fake HBA: handoff, port stop timing, empty port, ATAPI and unknown signatures, IDENTIFY variants (no LBA48, 4Kn, 512e, zero capacity, garbage words), PRDT planning (page crossings, odd lengths, unaligned buffers into the bounce page, 64-entry limit), slot reuse, task file error with other slots in flight, COMRESET, a port that never stops, a `PxCI` bit clearing for a slot never issued, a wrong `PRDBC`, timeout and detach | `cargo test -p ahci` |
| Host fuzz | `fuzz::run(&[u8])`: a fully hostile HBA (every register read and every DMA write-back from the input) and IDENTIFY data; no panic, no access outside the slot's memory, every wait bounded | `FUZZ_CASES=30000 cargo test -p ahci --release seeded`; a `fuzz/` target and seeds |
| Kernel correctness | `ahci_suite`: attach to a scratch disk on QEMU's AHCI, IDENTIFY matches the image, read/write/flush round trips at every alignment (sector, page, odd addresses through the bounce page), vectored transfers, the last sector, out-of-range refused, a read of a region another test wrote, partitions registered, an empty port and a CD-ROM ignored | `python tools/test/run.py --machine q35 --ahci --accel none` |
| Kernel soak | 20 000 random-size, random-offset writes and reads with verification across 8 in-flight slots, interleaved flushes, and a parked waiter killed mid-transfer (its buffers stay untouched afterwards); then the port's request counters are balanced and no frame leaked | same run, `LAZYOS_TEST_FILTER=ahci` |
| Boot | `ahci-boot`: the dev image on QEMU's AHCI alone (q35, no virtio disk), `FS:ROOT:ahci0p3`, desktop screenshot judged | CI job beside `nvme-boot` |
| Existing harnesses | every q35 run unchanged (the probe's log line only) | CI as is |
| Box | §5 | — |

## 5. On the box

1. Boot `lazyos-usb.img`: the log names `ahci0`, the SSD's model and size
   (476.9 GiB, "SSD 512GB"), 512-byte logical sectors, and the Linux
   partitions (GPT: shown as a protective entry until N2's GPT reader).
2. Read-only checks while Linux lives there: a full sequential read of the
   disk with a checksum per GiB, compared with the same checksums from Linux
   (`dd` and `sha256sum`); throughput logged against `hdparm -t` on Linux.
3. Power-off from the menu: the log shows the flush and standby per port; the
   next Linux boot shows no unclean-shutdown complaint from the drive.
4. Writes only after the install decision (kabylake plan §3.5), or to a
   scratch partition Linux makes for the purpose.

## 6. Stages

| Stage | Deliverable | Verified by |
|---|---|---|
| **A0** Facts | AHCI 1.3.1 and ACS register and command tables into `libs/ahci` docs with section references, every *to confirm* here settled; Sunrise Point's `PCS` and handoff behaviour from its datasheet or from Linux's view on the box (`lspci -vvv -s 00:17.0`, read only) | review |
| **A1** Library | `libs/ahci` with the fake, unit tests, fuzz, clippy in CI | host tests |
| **A2** Kernel read/write | the adapter, `DRIVERS` row, `ahci_suite` correctness and soak, `--ahci` in `tools/test/run.py`, `kernel-tests (ahci)` | `python tools/test/run.py --machine q35 --ahci --accel none` and the whole suite |
| **A3** Boot and power | the boot slot rule, flush and standby in the power path, `ahci-boot` CI job, `--disk-bus ahci` in `run_demo.py` and the launcher | the CI job; a shutdown run whose log shows the standby |
| **A4** The box | §5 | the compat row |

A5 and later, each optional: NCQ, TRIM (needs a discard path from ext2),
interrupts on MSI (the box's controller has MSI *to confirm*), link power
management, SMART health in `sysmon`.

## 7. Risks

1. **Real controllers are stricter than QEMU's.** Port stop and COMRESET
   timings, `PxSERR` bits left by the firmware, and the handoff are where
   real hardware differs; the fake models the slow and the stuck cases, and
   every wait is bounded so the worst case is a skipped port.
2. **Spin-up and slow SSDs.** A device that takes seconds to clear `BSY`
   after power-on would be skipped by a 1 s wait. The box's SSD is up long
   before the kernel runs; if not, the wait grows behind a log line, not by
   default.
3. **The only disk holds Linux.** A driver bug that writes could destroy the
   box's Linux; nothing writes to the SSD before A4 step 4, and the
   acceptance reads are checked against Linux's.
4. **GPT.** The box's disk is GPT (Linux's default under UEFI) *inferred*;
   until N2 lands only the whole disk registers, which is enough for A4's
   reads.
