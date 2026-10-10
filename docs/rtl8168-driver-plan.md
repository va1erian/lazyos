# Realtek RTL8111H / RTL8168H NIC driver — plan

> **Status: revision 2 (2026-10-09). R1 and R2 are built and host-tested
> (`libs/rtl8168`, the `netdrv` back end, the `devmatch` row); the diagnosis
> tools for R0 and the VFIO script for R3 exist. Nothing has run on the chip:
> R0, R3 and R4 need the box.** See "As built" at the end for what differs
> from the draft.
> The wired Ethernet for the Kaby Lake box ([kabylake-box-plan.md](kabylake-box-plan.md)):
> a fourth back end of `netdrv` serving `os.lazy.net.nic.v1` unchanged, so
> `netd`, sockets and every tool keep working. It copies the shape of the
> 8254x back end ([architecture/drivers.md](architecture/drivers.md)) and of
> the deferred [i226-driver-plan.md](i226-driver-plan.md). Facts read from the
> box's Linux are stated with their source; chip facts from memory of the BSD
> drivers are *to confirm* (settled in R0); guesses are *inferred*.

Related: [architecture/networking.md](architecture/networking.md),
[architecture/interrupts.md](architecture/interrupts.md),
[driver-plan.md](driver-plan.md) (D5: a DMA driver is trusted like the
kernel), [networking-plan.md](networking-plan.md) §5 (the device is hostile),
[driver-config-plan.md](driver-config-plan.md) (`sys/dev/net/*`).

## 1. Short answer

1. **Shape:** `libs/rtl8168` (host-tested `no_std`: registers, reset, MAC,
   PHY access, the descriptor rings as a `nicdrv::NicRings`), a
   `netdrv/rtl8168_card.rs`, a `devmatch` row for `10ec:8168`, and the
   settings prefix `sys/dev/net/rtl8168`. No new kernel op: the chip needs a
   memory BAR, DMA below or above 4 GiB, and MSI, all of which exist.
2. **One revision.** "RTL8168" is a family of dozens of chip revisions told
   apart by the XID field of `TxConfig`, each with its own PHY and MAC
   set-up. v1 supports exactly the box's, **XID `541`** (Linux:
   "RTL8168h/8111h"), and refuses any other XID by name with the
   "unsupported" exit (kabylake plan §3.3). More revisions are added only
   with a box to test them on.
3. **No QEMU model, so the box is the test bench.** Host tests against a
   register-level fake and a fuzzed hostile device carry the logic; the
   chip is proven first through VFIO on the box's Linux (fast loop, but the
   chip arrives already set up by Linux), then on bare metal from a cold
   power-on, which is the real acceptance.
4. **v1 is small:** one receive and one transmit ring, MSI, 1500-byte MTU, no
   offloads, no jumbo frames, EEE and ASPM left as the firmware set them
   (ASPM is off on this box), autonegotiation 10/100/1000.
5. **Sources and licences.** No datasheet is public. The references are the
   BSD drivers (FreeBSD and OpenBSD `re(4)`) read for understanding; Linux's
   `r8169` is GPL-2.0-only and is not used. The `re(4)` sources carry a
   4-clause BSD licence (the advertising clause) *to confirm per file*,
   which is not GPL-compatible, so nothing is copied from them, including
   the per-revision tables of magic register values; LazyOS writes what it
   understands and what the box shows it needs (§3.2, §6).

## 2. The chip on the box

From `lspci -vvv -s 02:00.0` and `dmesg` on the box (2026-10-09):

| Item | Value |
|---|---|
| Id | `10ec:8168` rev `15`, subsystem `10ec:0123` |
| XID | `541`, "RTL8168h/8111h" (Linux's `r8169`) |
| BARs | BAR 0 I/O, 256 bytes; **BAR 2 memory, 64-bit, 4 KiB: the registers**; BAR 4 memory, 64-bit, 16 KiB: the MSI-X table (offset 0) and PBA (`0x800`) |
| Interrupts | MSI, 1 vector, 64-bit address; MSI-X, 4 vectors in BAR 4; INTx pin A |
| PCIe | gen 1 x1, max payload 128, max read request 4096; ASPM **disabled** on the link; L1 PM substates enabled in `L1SubCtl1` |
| IOMMU group | 8, alone (VFIO works) |
| MAC | read by Linux from the chip, a valid unicast address |
| Jumbo | Linux reports frames up to 9194 bytes, transmit checksum offload not available with jumbo |
| PHY | Linux attached its "Generic FE-GE Realtek PHY" driver |
| Firmware | `dmesg` shows no `r8169` firmware message, but the link was down (no cable) when it was read, and Linux loads the PHY patch at link-up *to confirm*; the kabylake plan's question 3 settles whether this chip gets a patch (`rtl8168h-2` *to confirm*) |

The kernel picks MSI when a function has both, and keeps the MSI-X table in
BAR 4 out of the driver's mapping; BAR 2 is mapped whole.

## 3. Design

### 3.1 Registers (`libs/rtl8168::regs`, all *to confirm* in R0)

| Name | Offset | Use |
|---|---|---|
| `IDR0–5` | `0x00` | station address (loaded from the chip's EEPROM/eFuse at power-on) |
| `MAR0–7` | `0x08` | multicast hash filter |
| `TNPDS` | `0x20` | transmit normal-priority descriptor ring base (64-bit) |
| `ChipCmd` | `0x37` | `RST`, `RE` (receive enable), `TE` (transmit enable) |
| `TxPoll` | `0x38` | `NPQ`: the transmit doorbell |
| `IntrMask` / `IntrStatus` | `0x3C` / `0x3E` | 16-bit; status is write-one-to-clear |
| `TxConfig` | `0x40` | DMA burst, inter-frame gap; the XID in its top bits |
| `RxConfig` | `0x44` | accept broadcast / own address / multicast, DMA burst |
| `Cfg9346` | `0x50` | config-register write lock |
| `PHYAR` | `0x60` | PHY register access (MII over the MAC) |
| `PHYstatus` | `0x6C` | link, speed, duplex |
| `RxMaxSize` | `0xDA` | largest frame received |
| `CPlusCmd` | `0xE0` | C+ mode: descriptor mode, checksum and VLAN offloads (off) |
| `RDSAR` | `0xE4` | receive descriptor ring base (64-bit) |
| `MaxTxPacketSize` | `0xEC` | transmit size limit |

### 3.2 Bring-up (`rtl8168_card::open`)

1. Map BAR 2; refuse a BAR shorter than the highest register used.
2. Read `TxConfig`; all ones means the function is gone. Decode the XID; any
   value but `541` ends here with the "unsupported" exit and the XID in the
   reason.
3. Mask all interrupts, clear `IntrStatus`; soft reset (`ChipCmd.RST`), wait
   (bounded, napping a tick per poll) for it to clear.
4. Read `IDR0–5`; a usable unicast address or `mac_override` or
   `SetupError::NoMac` (the rule every back end follows).
5. Unlock `Cfg9346`; program `CPlusCmd` (no offloads), `RxMaxSize` (1518 plus
   VLAN slack, *to confirm*), `MaxTxPacketSize`, the ring bases
   (`RDSAR`, `TNPDS`), `TxConfig` (burst, gap) and `RxConfig` (broadcast,
   own address, multicast through `MAR`, no promiscuous); lock `Cfg9346`;
   enable `RE | TE`.
6. **PHY.** The minimum first: read the PHY's ID and status over `PHYAR`,
   restart autonegotiation advertising 10/100/1000, nothing else. Linux and
   the BSD drivers write per-revision PHY and "extended PHY" values for this
   chip on top; which of those this chip actually needs for a stable link is
   found on the box, cold (§5), not assumed. Anything added is written down
   with what it fixes, and kept in `phy_541.rs` alone.
7. Unmask receive OK, receive error, receive descriptor unavailable,
   transmit OK, transmit error, link change and system error (the bit names
   and numbers *to confirm*) after `device::arm` has the vector.

### 3.3 Rings (`libs/rtl8168::rings`)

The 8254x contract in Realtek's descriptor format: 16-byte descriptors,
`opts1` (`OWN`, `EOR` end of ring, `FS`/`LS` first and last segment, the
length or buffer size), `opts2` (VLAN, unused), a 64-bit buffer address;
rings 256-byte aligned, at most 1024 entries *to confirm*, 256 by default.

- **Receive:** every descriptor owns a fixed 2 KiB slot and is handed to the
  device with `OWN` set (and `EOR` on the last). A completion is a
  descriptor whose `OWN` the device cleared. Its length includes the 4-byte
  FCS: the chip does not strip it and Linux's receive path for this revision
  subtracts `ETH_FCS_LEN` unconditionally. **The driver trims the FCS before
  `deliver`**, because `NicRings::poll_frames` takes `max_frame` as MTU plus
  Ethernet header (1514): with the FCS a maximum-size frame is 1518 and would
  be refused as oversize, and shorter frames would carry four bytes of FCS to
  the client. A reported length below 4 is a runt, and a trimmed length below
  14 is a runt. Tests: frames of 14, 15, 60, 61, 1000, 1513 and 1514 bytes
  arrive exactly as sent with the descriptor reporting length + 4; 1515 is
  `Oversize`; a descriptor length equal to the limit is a 4-byte-shorter
  frame, not an oversize one. A length past the slot, a frame without both `FS` and
  `LS` (spread over descriptors: v1 never asks for that), or an error bit
  (`RES`, CRC, runt, the rest *to confirm*) is dropped and counted. The frame
  is copied out of its slot, then the descriptor goes back with `OWN`.
- **Transmit:** fill the descriptor at the driver's index (`FS | LS | OWN`,
  length, `EOR` on the last), then ring `TxPoll.NPQ`. Reap in order while
  `OWN` is clear. At most `entries − 1` frames in flight.
- **Hostile device:** an `OWN` cleared on a descriptor the driver never
  handed over, or completions out of order, is `Fatal::Hardware`; nothing
  the device writes is used as an index.

The engine's `NicRings` trait needs no change; the doorbell is written by
the rings themselves (`TxPoll`), as on the 8254x (`NoBell`).

### 3.4 Interrupts, link and failure

- **MSI** (what the kernel grants). Read `IntrStatus`, write the same bits
  back to clear them, then `irq_ack`; mask while handling *to confirm* (the
  BSD drivers mask `IntrMask` around the handler).
- **INTx** stays the fallback; polled at 100 Hz when no line is routable.
- **Link:** the link-change cause and the 50-tick poll read `PHYstatus`
  (link, 10/100/1000, duplex); a change publishes
  `system/net/rtl8168-0/link` through the existing path.
- **Receive descriptor unavailable** (the ring ran dry): counted, the
  descriptors are refilled as they are reaped; never fatal.
- **Transmit stuck:** a ring with frames queued and no completion for 5 s
  while the link is up is `Fatal::Hardware("tx timeout")`; `netdrv` exits and
  `init` restarts it, and the restart's soft reset is the recovery. (The
  Realtek family is known for transmit hangs that only a reset clears
  *inferred* from the BSD drivers' watchdogs.)
- **System error** (a PCI error the chip reports) and an all-ones register
  read: `Fatal::Hardware`, the same restart.
- **Shutdown:** on `netdrv` exit or the lifecycle stop, disable `RE | TE`,
  mask, soft reset, so no DMA runs after the task ends (the kernel releases
  the claim and turns bus mastering off).

### 3.5 Where it goes

| Path | Change |
|---|---|
| `libs/rtl8168/` (new) | `regs.rs`, `desc.rs`, `rings.rs`, `setup.rs` (reset, XID, MAC, enable), `phy.rs` (`PHYAR` access, autoneg, `PHYstatus`), `phy_541.rs` (only what the box shows this revision needs), `fake.rs`, `tests.rs`, `fuzz.rs`; each under 500 lines |
| `libs/devmatch` | an entry "Realtek RTL8168" for `10ec:8168`, `RTL8168_DEVICES` kept equal to `rtl8168::DEVICES` by test |
| `user/src/bin/netdrv/` | `device.rs`: `Kind::Rtl8168`; `rtl8168_card.rs`; `card.rs`: `Backend::Rtl8168`; `rings.rs`: `AnyRings::Rtl8168`; `config.rs`: `sys/dev/net/rtl8168` |
| `tools/net/vfio_box.sh` (new, Linux only) | bind `02:00.0` to `vfio-pci`, run the image under QEMU/KVM with it and no emulated NIC, bind it back to `r8169` on exit |
| `docs/architecture/drivers.md`, `docs/compat/hardware.md` | the back end; the box's row |

No build switch and no launcher option: `netdrv` is in every networked image
and `devd` picks the back end at run time; QEMU has no such card to offer
in `--nic`. The box's VFIO run uses `vfio_box.sh` directly.

## 4. Testing

Every row is a planned acceptance criterion; none has run.

| Layer | What | Where |
|---|---|---|
| Host unit | reset that never clears, every XID other than `541` refused, all-ones device, blank and group MAC, `PHYAR` timeouts, link up/down/speed from `PHYstatus`, interrupt causes cleared by writing them back, the tx watchdog | `cargo test -p rtl8168` against `fake.rs` |
| Host rings | the 8254x ring tests in this format: `EOR` wrap, full and empty, hostile completions (`OWN` cleared on a descriptor never handed over, out of order, length 0 or past the slot, `FS` without `LS`, error bits), receive-unavailable recovery | `cargo test -p rtl8168` |
| Host fuzz | `fuzz::run(&[u8])`: scripted hostile device and client over rings and setup; seeds in `fuzz/seeds/rtl8168` | `FUZZ_CASES=20000 cargo test -p rtl8168 --release seeded` |
| Manifest | `devmatch` row and `plan()` with the box's function list | `cargo test -p devmatch` |
| Regression | every existing NIC harness unchanged (`tools/net/run.py`, `--nic e1000`, `--netd`, `--services`) | CI as is |
| Box | §5 | — |

## 5. On the box

**VFIO first** (`tools/net/vfio_box.sh`, Linux keeps the Wi-Fi): `devd`
starts `netdrv` for `10ec:8168`, the log shows the XID, MAC and
`NETDRV:IRQ:Msi`; DHCP lease, `ping` the gateway, `curl https://…`; a
1 GiB transfer each way with `nc` and a matching checksum, zero drops in
`NicStats`. This tests the rings, interrupts and MAC; not the PHY's cold
start, since `r8169` has already set the chip up.

**Bare metal, cold:** power the box off at the wall, boot
`lazyos-usb.img`. The same checks, plus:

1. Link at 1000 full to a gigabit switch, at 100 to a 100 Mb port if one is
   at hand.
2. 50 cable pulls: each one a `LinkChange` and a renewed lease, no driver
   restart.
3. A 24-hour soak (pings plus a transfer every 10 minutes): no restart, no
   tx timeout, no growth in `netdrv`'s memory.
4. Power-off from the menu leaves the card quiet; a reboot finds it again.

If the cold link is unstable where the VFIO one was not, the difference is
in the PHY set-up Linux did: compare `ethtool -d enp2s0` register dumps from
Linux with LazyOS's own dump (a `NETDRV:REGS` line at debug level) and add
the smallest PHY step that closes the gap to `phy_541.rs`, with a comment
saying what it fixes. A needed firmware patch is a decision, not a quiet
addition: its licence and whether LazyOS may ship it are settled first.

## 6. Stages

| Stage | Deliverable | Verified by |
|---|---|---|
| **R0** Facts | Register and descriptor tables into `libs/rtl8168` docs, every *to confirm* settled from the BSD sources (read, not copied) and the box (`ethtool -d`, `ethtool -i` with a cable, `dmesg`); licence of each reference recorded | review; saved outputs in `docs/compat/kabylake/` |
| **R1** Library | `libs/rtl8168` rings, setup, PHY access, fake, fuzz, clippy in CI | host tests |
| **R2** `netdrv` back end | `Kind::Rtl8168`, settings, `devmatch` row, the "unsupported" exit for other XIDs | host tests; every existing NIC harness green |
| **R3** VFIO | `vfio_box.sh`; §5 VFIO checks | the run's log and checksums |
| **R4** Bare metal | §5 cold checks; `phy_541.rs` with whatever the box proved necessary; the compat row | the soak's log and the row |

R5 and later, each optional: receive checksum offload, jumbo frames, link
speed on Messenger (shared with the I226 plan's I5), hardware counters into
`NicStats`, more revisions (each with its own box).

## 7. Risks

1. **Undocumented PHY set-up.** The biggest risk. A cold chip may link
   poorly or not at all without the per-revision writes Linux does. The
   mitigation is the cold bare-metal test and register-dump comparison in
   §5; the worst case is a decision about a firmware patch or a specific
   PHY sequence, recorded with its source.
2. **VFIO hides it.** A pass under VFIO proves less than it seems (§5). R4
   is not optional.
3. **Licences.** The BSD `re(4)` code is not GPL-compatible to copy *to
   confirm*; reading it for how the hardware behaves is fine, copying its
   tables is not. If a table turns out unavoidable, R0 records where an
   equivalent compatible source is (or that there is none).
4. **Transmit hangs.** Family-wide reports of stalls *inferred*; the 5 s
   watchdog and restart turn a hang into a short outage, and the soak
   measures how often.
5. **Wake and power states.** The function supports D3 and PME; v1 never
   leaves D0 and never arms wake.

## 8. As built (revision 2)

What the code does that the draft above did not say, or does differently:

- **Receive filter.** The chip is programmed to accept every unicast, multicast
  and broadcast frame (`MAR` all ones, `AcceptAllPhys`) and the engine filters,
  as the 8254x back end does, so `SetRxMode` works without reprogramming the
  chip. The draft's "no promiscuous" would have made the engine's promiscuous
  mode a lie.
- **Register widths.** `Regs` has 8, 16 and 32-bit accessors: `IntrMask` and
  `IntrStatus` are neighbours and the status is write-one-to-clear, so a 32-bit
  write meant for the mask would acknowledge causes.
- **Interrupts are unmasked after `device::arm`**, as §3.2 step 7 says (the
  8254x back end unmasks before; this one does not).
- **Transmit padding.** Frames under 60 bytes are zero-padded by the driver, so
  no revision-specific hardware padding behaviour is relied on.
- **Hostile completions.** A completion behind a descriptor the chip still owns,
  seen on two polls running, is `Fatal::Hardware`. Receive "never handed over"
  cannot arise (every descriptor is re-posted at once); transmit completions on
  descriptors not in flight are ignored.
- **Transmit watchdog.** `libs/rtl8168::TxWatchdog`: queued frames, no
  completion for 500 ticks, link up: `Fatal::Hardware("tx timeout")`.
  A system error, an all-ones interrupt status or link register is fatal too.
- **Unsupported revision.** `setup::identify` refuses every XID but `541`;
  `netdrv` prints `NETDRV:UNSUPPORTED <reason>` and parks (as for "no device")
  instead of exiting, because the kabylake plan's `EXIT_UNSUPPORTED` (§3.3, B2)
  does not exist yet. When it lands the park becomes that exit.
- **Shutdown.** `Card`'s `Drop` disables `RE | TE`, masks and soft-resets before
  the DMA region is released.
- **Diagnosis.** `netdrv` prints `NETDRV:RTL8168 xid=… phy_id=…`, the link and
  `NETDRV:REGS <label> <offset>: <32 bytes>` (256 bytes) at bring-up and on
  every link change, always (the chip has no QEMU model, so the log is the
  instrument). `tools/net/rtl8168/rtl8168_probe.py` collects the Linux side
  (`survey`: `lspci`, `ethtool -i/-d/-S/--show-eee`, `dmesg`, BAR 2 and the
  PHY's MII registers), decodes any dump and diffs Linux against LazyOS; its
  `decode` also checks the plan's assumptions (XID, unicast MAC, the function
  answering). `tools/net/vfio_box.sh` is R3's script.
- **Not done:** the cargo-fuzz target and `fuzz/seeds/rtl8168` (the seeded
  hostile-chip test `seeded_traffic_with_a_hostile_chip` runs in `cargo test`);
  `docs/compat/hardware.md` (it does not exist yet); every *to confirm* in §3.1
  stays so until `survey` has run on the box. The descriptor layout, error bit
  positions and register offsets are the ones the family's open drivers agree
  on, written from an understanding of them, not copied.
