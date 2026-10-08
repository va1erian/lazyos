# Intel I226-V (and I225) NIC driver — plan

> **Status: draft, revision 2 (2026-10-08): decisions recorded (§8). Nothing here is built.**
> This plan fills stage K2 of [n150-driver-plan.md](n150-driver-plan.md) for
> the Intel family: one more back end of `netdrv` serving
> `os.lazy.net.nic.v1` unchanged, so `netd`, sockets and every tool keep
> working. Facts checked against this repository are stated plainly; facts
> about the chip that come from memory of public sources and still need the
> datasheet are marked *to confirm*; guesses are marked *inferred*.

Related: [architecture/drivers.md](architecture/drivers.md) (`devd`, the
8254x back end this one copies), [architecture/networking.md](architecture/networking.md),
[architecture/interrupts.md](architecture/interrupts.md) (MSI, MSI-X),
[driver-plan.md](driver-plan.md) (D5: a DMA driver is trusted like the
kernel), [driver-config-plan.md](driver-config-plan.md) (`sys/dev/net/*`),
[networking-plan.md](networking-plan.md) §5 (the device is hostile).

## 1. Short answer

1. **Shape: a third `netdrv` back end, no new kernel op.** `libs/igc`
   (host-tested `no_std`: registers, reset, PHY, advanced descriptor rings as
   a `nicdrv::NicRings`) plus `user/src/bin/netdrv/igc_card.rs`, one
   `devmatch` row and one settings prefix. The kernel already has everything
   the chip needs: 64-bit memory BARs, `dma_alloc` with the 64-bit flag, MSI
   (preferred when a function has both, which the I225/I226 does *to confirm*)
   and MSI-X with the table kept out of the driver's mapping.
2. **The hard part is testing, not code.** QEMU has no I225/I226 model. It
   does model the **82576 (`-device igb`, QEMU 8.0+)**, whose *advanced*
   descriptor format and per-queue register block (`0xC000` receive,
   `0xE000` transmit, `SRRCTL`, `RXDCTL`/`TXDCTL` enable bits) the I225
   inherited *to confirm*. So the ring code is written once, chip-neutral,
   and is to be proven end to end in CI on QEMU's `igb`; only reset, NVM/MAC, PHY and
   link are I226-specific, and those are to be proven by a register-level fake on
   the host and then by the real box.
3. **v1 is deliberately small:** one receive and one transmit queue, MSI with
   the legacy-style `ICR`/`IMS` causes (as the 8254x back end already does),
   1500-byte MTU, no offloads, EEE off, autonegotiation 10/100/1000/2500.
   Multi-queue, RSS, TSO, checksum offload, jumbo frames, PTP/TSN and
   Wake-on-LAN are later or out (§6).
4. **Write it from Intel's datasheet and FreeBSD `igc(4)`**, never from
   Linux's `igc`, which is GPL-2.0-only (n150-driver-plan §3 P1, wifi-plan §4).
   FreeBSD's driver is BSD-licensed Intel shared code *to confirm per file
   header*; it is read for understanding, the Rust is written fresh in
   `libs/e1000`'s shape.

## 2. The chip

| Item | What it is | Notes |
|---|---|---|
| PCI ids | Intel `8086`; I226: `125B` LM, `125C` **V**, `125D` IT, `3102` K, `5503` LMvP, `125F` blank NVM; I225: `15F2` LM, `15F3` V, `15F8` I, `0D9F` IT, `3100` K, `5502` LMvP, `15FD` blank NVM | from FreeBSD `igc_hw.h` *to confirm*; the box's own id comes from K0 |
| BARs | BAR 0: registers, 64-bit memory, 128 KiB *to confirm*; BAR 3: MSI-X table, 16 KiB *to confirm* | BAR 0 is `map_bar`ped whole; BAR 3 is the kernel's ([interrupts.md](architecture/interrupts.md)) |
| Interrupts | MSI and MSI-X (up to 5 vectors *to confirm*), INTx | the kernel picks MSI; in MSI mode causes are read from `ICR` (read-to-clear) as on the 8254x *to confirm* |
| Descriptors | advanced only *to confirm* (16-byte read/write-back formats, `SRRCTL.DESCTYPE` one-buffer) | Linux and FreeBSD both use advanced only; the legacy format is not relied on |
| Queues | 4 receive, 4 transmit *to confirm* | v1 uses queue 0 of each |
| PHY | internal 2.5GBASE-T PHY *to confirm*, reached through `MDIC`, guarded by the software/firmware semaphore (`SWSM`, `SW_FW_SYNC`) *to confirm* | 2.5G advertisement through the PHY's multi-gig autoneg control *to confirm* (FreeBSD `igc_phy.c`) |
| NVM | external flash; the MAC is loaded into `RAL0`/`RAH0` by hardware after reset *to confirm* | a blank-NVM part (`125F`) has no MAC: `mac_override` or refuse |
| Reset | `CTRL.DEV_RST` (not the 8254x's `CTRL.RST`), then wait for the NVM auto-read to finish | *to confirm* bit numbers and the done flag |
| Link | `STATUS.LU`; speed from `STATUS` including a 2500 bit; `ICR.LSC` on change | *to confirm* bit numbers |

Known field problems *inferred from widely reported Linux bug threads*:
links that flap with Energy Efficient Ethernet on, and a "PCIe link lost"
state in which every register reads `0xFFFF_FFFF` (often blamed on ASPM).
v1 turns EEE off and treats an all-ones register read as a dead device
(§4.3). The I225-V's early steppings had 2.5G problems of their own; the I225
ids are matched but only an I226 is a v1 target.

## 3. Where it goes

```
 devmatch row "Intel I225/I226"  ──▶ devd ──StartDriver(netdrv, id)──▶ init
                                                                        │ spawn dev=<id>
 netdrv: device.rs (Kind::Igc(model)) ─▶ igc_card.rs ─▶ card.rs ─▶ Engine<AnyRings::Igc>
                                              │
                                   libs/igc: regs, reset, nvm, phy, link, adv rings
                                             (+ i82576 setup: QEMU igb, test vehicle)
```

| Path | Change |
|---|---|
| `libs/igc/` (new) | `regs.rs` (offsets, bits, the `Regs` accessor, re-used shape of `libs/e1000::regs`), `adv.rs` (advanced descriptors, volatile, no references into DMA), `rings.rs` (`NicRings` over one DMA block, a fixed 2 KiB slot per descriptor), `i225.rs` (reset, MAC, EEE off, causes), `phy.rs` (semaphore, `MDIC`, autoneg), `link.rs` (up/speed), `i82576.rs` (the minimum to run QEMU's `igb`), `fake.rs`, `tests.rs`, `fuzz.rs`; every file under 500 lines |
| `libs/nicdrv` | nothing expected; if the engine needs a doorbell for tail writes it takes the 8254x's `NoBell` path |
| `libs/devmatch` | entries "Intel I225/I226" and "Intel 82576 (QEMU igb)" with `IGC_DEVICES`/`IGB_DEVICES`, and the test that keeps them equal to `igc::DEVICES` (as `the_manifest_agrees_with_the_drivers` does for e1000) |
| `user/src/bin/netdrv/` | `device.rs`: `Kind::Igc(model)`, `Kind::Igb(model)`; `igc_card.rs` (bring-up, like `e1000_card.rs`); `card.rs`: `Backend::Igc`, causes and link; `rings.rs`: `AnyRings::Igc`; `config.rs`: `sys/dev/net/igc` |
| `tools/net/qemu_net.py`, `tools/net/run.py`, `tools/run_demo.py`, `tools/lazygui/drivers.py` (+ tests) | `--nic igb` (`-device igb`), its expected model and IRQ path for the judge |
| `idl/net.midl` (stage I5, optional) | `NicInfo.speed_mbps`, `LinkEvent.speed_mbps` as new optional fields, regenerated with `midlc` |
| `docs/architecture/drivers.md`, `docs/compat/hardware.md` | the back end; the box's row |

No new build switch: `netdrv` is already in every networked image, and the
card is chosen at run time by `devd`. The launcher and `run_demo.py` gain
only the QEMU card choice (`--nic igb`), which is what AGENTS.md's "both front
ends" rule asks of a driver with no app.

## 4. Design

### 4.1 Bring-up (`igc_card::open`)

1. Map BAR 0; refuse a BAR shorter than the highest register used.
2. Read `STATUS`; all ones means the function is gone: fail before writing.
3. Mask everything (`IMC` all ones), `CTRL.DEV_RST`, wait (napping a tick
   per poll, bounded) for reset and the NVM auto-read to finish, mask again,
   read `ICR` to clear.
4. Take the PHY semaphore, set the autoneg advertisement (10/100 half/full,
   1000 full, 2500 full; each a setting), turn EEE off, restart autoneg,
   release the semaphore. A semaphore that never frees (firmware holds it) is
   a bounded timeout and a `SetupError`, not a hang.
5. MAC: `RAL0`/`RAH0` when `RAH.AV` is set and the address is usable
   unicast, else `mac_override`, else `SetupError::NoMac` (the 8254x rule).
6. Clear the multicast table; program `RCTL` (broadcast accept, strip CRC,
   2 KiB buffers via `SRRCTL`) and `TCTL`; set up queue 0's rings
   (`RDBAL/H`, `RDLEN`, `SRRCTL`, `RXDCTL.ENABLE` polled until it reads
   back set, then `RDT`; likewise transmit) and only then enable the units.
7. Clear `PCIEERRSTS` and `LANPERRSTS` and drain `PEIND` (§4.3), then
   unmask `RXDMT0 | RXT0 | TXDW | LSC | FER` (the 8254x's wanted set plus the
   fatal error, *to confirm* for this chip) after `device::arm` has the
   vector.

### 4.2 Rings (`libs/igc::rings`)

The same contract as `libs/e1000::rings`, in the advanced format: a receive
descriptor is written as (packet address, header address 0) and read back as
(length, status `DD`/`EOP`, error bits); a transmit descriptor is the
advanced data type with `DCMD.EOP | IFCS | RS` and `PAYLEN`. Slot `i` belongs
to descriptor `i`, so the device only ever sees driver-owned memory;
write-back lengths, a frame over several descriptors, an error bit and a
completion on a descriptor that was never posted are dropped and counted or
`Fatal::Hardware`, never used to index; a frame is copied out before the
engine looks at it. Ring sizes are powers of two with the length a multiple
of 128 bytes; 256 entries each by default, as the 8254x.

The 82576 and the I225 share this file. Their differences live in the
`setup` modules, so a bug QEMU's `igb` finds in the ring code is a bug fixed
for the I226.

### 4.3 Interrupts, link and failure

- **MSI first** (what the kernel grants a function that has both). Causes
  come from `ICR`; reading it clears the cause and, under MSI, needs no
  deassert ordering, but the 8254x order (read, then `irq_ack`) is kept.
- **MSI-X** only if the box's MSI turns out unusable (I5): `GPIE.MSIX_MODE`,
  one vector, `IVAR` mapping receive 0, transmit 0 and "other" to entry 0,
  causes in `EICR`. The kernel already programs entry 0 and masks the rest.
- **INTx** stays a fallback (`settings.irq_mode`), polled at 100 Hz when no
  line is routable, as today.
- **Link**: `LSC` and the 50-tick poll re-read `STATUS`; a change publishes
  `system/net/igc-0/link` through the existing path. Autoneg at 2.5G takes a
  few seconds; `netd` already waits for `LinkChange`.
- **A dead device**: any register read of all ones on a path that cannot
  legitimately return it (`STATUS`, `CTRL`) is `Fatal::Hardware("device
  gone")`; `netdrv` exits and `init`'s restart policy, with its backoff,
  tries again. Never spin on a bit of a register that reads all ones.
- **A fatal internal error** (an uncorrectable parity/ECC error in one of
  the chip's memories) raises `ICR.FER`. The reference is FreeBSD commit
  `bbf93227` ("igc: Recover from fatal internal memory errors", 2026-08-12,
  tested on an I225-IT rev. 3); its register names and offsets are checked
  against the datasheet in I0. `PEIND` (`0x01084`, read-clear) names the
  region: LAN `0x1`, management `0x2`, PCIe `0x4`, DMA `0x8`; the
  per-region status registers are `PCIEERRSTS` (`0x05BA8`, RW1C, fatal mask
  `0x78`) and `LANPERRSTS` (`0x05F58`, RW1C). On `FER` the dying `netdrv`
  masks `FER` (`IMC`), reads `PEIND` and the status registers, logs
  `NETDRV:FATAL:<region>:<status>`, and stops using the rings. Then:
  - **PCIe region** (`PEIND & 0x4`, or fatal bits in `PCIEERRSTS`): an
    ordinary reset is not enough and the order differs from a normal
    bring-up, because the parity error can stop PCIe and DMA traffic. Still
    holding its claim, the driver asserts `CTRL.DEV_RST`, waits at least
    3 ms (one 10 ms nap) before touching a register, polls (bounded) for
    the NVM auto-read and `STATUS.RST_DONE`, *then* clears bus mastering
    (`cfg_write` of the command register, an op it already uses), and
    clears `PCIEERRSTS`'s fatal bits.
  - **LAN or DMA region:** no extra step; the restart's reset is the
    recovery.
  - **Management region:** left to the management firmware, as FreeBSD
    does; the driver still restarts.

  It then returns `Fatal::Hardware("internal error")`, `netdrv` exits and
  `init` restarts it. The new instance's ordinary bring-up (§4.1) is the
  port reinitialisation: it turns bus mastering back on, resets, and before
  it unmasks `FER` clears `PCIEERRSTS` and `LANPERRSTS` again and drains
  `PEIND` (a reset can latch it again). Doing that drain on *every*
  bring-up also covers an instance that died before it finished the PCIe
  steps. In-flight frames are lost and the client sees a link drop and a
  fresh attach, as after any driver restart. A failure that raises no `FER`
  shows as a stuck queue (no completions while `STATUS.LU` is up); v1 does
  not detect that (a transmit watchdog is an I5 item). Fake-device tests:
  one per region. The PCIe case checks the order of register and config
  writes (`DEV_RST`, the wait, then bus master off, then the `PCIEERRSTS`
  clear), and that the second bring-up drains `PEIND` before `IMS.FER`.

### 4.4 Settings

`sys/dev/net/igc/*`, the keys `netdrv` already reads (`irq_mode`, ring sizes,
`mtu`, `mac_override`), plus `advertise` (a list of speeds, default all) and
`eee` (default off). Validation as for the other prefixes: a bad value is
logged and the default used.

## 5. Testing

Everything in this section is a planned acceptance criterion: nothing below
has run yet, and a row counts as met only when its stage (§6) lands.

| Layer | What | Where |
|---|---|---|
| Host unit | reset timing, reset that never finishes, semaphore held forever, `MDIC` errors and timeouts, blank NVM, `RAH.AV` clear, all-ones device, `FER` for each `PEIND` region (the PCIe recovery order checked write by write) then a second bring-up, link up/down/speed, every `SetupError` | `cargo test -p igc` against `fake.rs`, a register-level model of the chip that can lie |
| Host rings | the e1000 ring tests ported to the advanced format: gap movement, full/empty, hostile write-backs (length 0, past the slot, no `EOP`, error bits, `DD` on a descriptor never posted, head running past tail) | `cargo test -p igc` |
| Host fuzz | `fuzz::run(&[u8])`: a scripted hostile device and client over the rings and setup, shared with a `fuzz/` cargo-fuzz target; seeds in `fuzz/seeds/igc`, `python fuzz/gen_corpus.py --check` | `FUZZ_CASES=20000 cargo test -p igc --release seeded` |
| Manifest | `devmatch` rows equal `igc::DEVICES`; `plan()` with an I226 and a virtio-net picks the first in enumeration order | `cargo test -p devmatch` |
| QEMU end to end | `-device igb`: DHCP, ping, nslookup, the socket probe and soak, judged from the pcap as for virtio and e1000; MSI and MSI-X paths through `tools/irqpath.py`; `devd` starts `netdrv` for it | `python tools/net/run.py --nic igb [--netd] [--services] [--irq-path msi\|pic]` |
| Real hardware | the N150 box: first its own I226 passed through to a LazyOS guest under QEMU/KVM on the box's Linux (§5.3), then LazyOS booted bare from the stick | §5.1 |

Kernel: no new kernel surface is planned, so no new kernel suite. If one
appears (a `dev_*` op, a new MSI-X mode), it ships with correctness and soak
tests under `kernel/src/tests/` per AGENTS.md and
`python tools/test/run.py --accel none` passes.

### 5.1 Hardware acceptance (recorded in `docs/compat/hardware.md`)

1. `devctl` shows `8086:125C` claimed by `netdrv`; serial shows
   `NETDRV:IRQ:Msi` and the MAC printed on the case label.
2. Link at 2500 to a 2.5G switch, at 1000 to a gigabit one (the speed read
   from `STATUS`, logged until I5 carries it on Messenger).
3. DHCP lease, `ping` the gateway, `curl https://…` with the Mozilla roots.
4. A 1 GiB transfer each way (`nc` to a host) with zero drops in `NicStats`
   and a matching checksum.
5. 50 cable pulls: every one a `LinkChange`, the lease renewed, no restart.
6. A 24-hour soak (`ping -i` plus periodic transfers): no driver restart, no
   growth in `netdrv`'s memory.
7. Shutdown from the menu leaves the card quiet (no DMA after `power`);
   reboot finds it again.

### 5.2 Day one on the box (Linux installed)

Reading what Linux reports is fine; copying its `igc` code is not (§1.4).
Save every output under `docs/compat/n150/` (text, no secrets: the MAC may be
kept or masked). `<if>` is the I226's interface (`ip -br link`), `<bdf>` its
PCI address (`lspci -nn | grep -i ethernet`).

| Command | Answers |
|---|---|
| `lspci -nn` | every function's id (I0, K0; also counts them against `MAX_DEVICES` = 32) |
| `sudo lspci -vvv -s <bdf>` | BAR sizes and widths, MSI/MSI-X capabilities and vector count, ASPM state on the link, the PCIe capability |
| `ethtool -i <if>`, `ethtool <if>` | NVM/firmware version; advertised and linked modes (does the NVM default advertise 2500?) |
| `sudo ethtool --show-eee <if>` | whether EEE is on by default |
| `sudo ethtool -d <if>` | a register dump after Linux's bring-up: a reference state for the fake |
| `sudo ethtool -e <if> raw on > nvm.bin` | the NVM image (MAC words, defaults); never written back |
| `dmesg \| grep -iE 'igc\|DMAR\|IOMMU'` | Linux's view of the chip, and whether VT-d is on |
| `find /sys/kernel/iommu_groups/ -type l` | whether the I226 is alone in its IOMMU group (§5.3) |
| `iperf3` to another machine | a throughput baseline to compare LazyOS against |

### 5.3 VFIO on the box

With VT-d enabled in firmware and `intel_iommu=on` on the kernel command
line, unbind the I226 from `igc`, bind it to `vfio-pci`, and boot
`target/lazyos.img` under QEMU/KVM with `-device vfio-pci,host=<bdf>` and no
emulated NIC. LazyOS's `devd` then finds the real `8086:125C` and the host
IOMMU keeps a driver bug's DMA inside the guest. The box's only wired port
goes to the guest, so the host is reached over Wi-Fi or the local console
while it runs; rebinding to `igc` gives Linux the port back. A small
`tools/net/vfio_box.sh` (bind, run, rebind) keeps the loop to one command.
Differences from bare metal: interrupts are remapped through KVM, and the
guest's firmware is SeaBIOS/OVMF, not the box's, so §5.1 still ends on bare
metal.

## 6. Stages

Each stage merges on its own and keeps every existing harness green.

| Stage | Deliverable | Verified by |
|---|---|---|
| **I0** Facts | Pull the I225 datasheet (and the I226's, if public *to confirm*); turn every *to confirm* in §2 into a register table with section references in `libs/igc/src/regs.rs` docs; the box's Linux answers §5.2 and K0's `devctl` shows what LazyOS enumerates | the saved outputs in `docs/compat/n150/` |
| **I1** Advanced rings on QEMU `igb` | `libs/igc` rings + `i82576` setup; `netdrv` `Kind::Igb`; `devmatch` row; `--nic igb` in `qemu_net.py`, `net/run.py`, `run_demo.py`, the launcher's Drivers group, with tests | host tests and fuzz; `tools/net/run.py --nic igb --netd --services` green; MSI-X and PIC paths judged |
| **I2** I225/I226 setup | `i225.rs`, `phy.rs`, `link.rs` against the fake; `Kind::Igc`; `sys/dev/net/igc/*`; the I225/I226 `devmatch` row | host tests and fuzz; CI cannot run the chip, so no claim beyond "host-tested" in the PR |
| **I3** First light | VFIO on the box (§5.3), then bare metal: fix what the real chip disagrees with, then fix the fake too | §5.1 steps 1–3 under VFIO, then bare |
| **I4** Hardening | §5.1 steps 4–7; the compat row | the soak's log and the row |
| **I5** Extras, each optional | link speed on Messenger (`net.midl`); MSI-X if needed; a transmit watchdog (a queue with work and no completions while the link is up ends the driver like a fatal error); hardware counters (`CRCERRS`, `MPC`, …) into `NicStats`; receive checksum offload; jumbo frames (`RCTL.LPE`, `SRRCTL` sizes, `mtu` up to 9000) | per item: host tests, `--nic igb` where QEMU models it |

**Out of v1:** multiple queues and RSS (needs an engine that serves more than
one ring pair), TSO, PTP and TSN, Wake-on-LAN configuration (firmware setting,
n150-driver-plan P2), VLAN filtering, EEE on, a second port on dual-NIC boxes
(one device per driver row today; `devd` reports the second `busy`).

## 7. Risks

1. **No chip in CI.** QEMU's `igb` proves the shared rings, not reset, PHY
   or link. The fake carries those, so it is written from the datasheet's
   state machine, not from what the driver happens to do; every real-chip
   surprise in I3 is added to it.
2. **PHY bring-up is under-documented.** If the datasheet does not cover the
   2.5G advertisement fully, the chip's NVM default (which advertises 2.5G on
   most boards *inferred*) is accepted and the setting is dropped to "leave
   as is" until it is understood.
3. **PCIe link loss / ASPM.** Firmware may enable ASPM L1 on the slot; LazyOS
   has no ECAM yet to turn it off (n150-driver-plan P2). The all-ones check
   keeps a lost device from hanging the driver; if it happens on the box,
   ECAM and an ASPM override move ahead of I5.
4. **Device table size.** `MAX_DEVICES` is 32; a PCH plus two NICs, Wi-Fi and
   NVMe may come close. K0 checks for `DEV:ENUM:FAIL:device table full`.
5. **Firmware sharing the PHY.** On vPro parts (LM) the management engine
   may hold the semaphore; the V has no ME on the NIC *inferred*. Bounded
   waits make it an error, not a hang.

## 8. Decisions (2026-10-08)

1. **The QEMU `igb` back end is to ship** as a supported driver matched on
   `8086:10C9`, labelled "QEMU-verified" in the compat notes once I1's acceptance runs pass (until then it is planned, not supported).
2. **The I225 ids are matched in v1**; the compat row says only the I226 is
   verified.
3. **Real hardware is the N150 box running Linux** (no other Linux machine).
   It serves twice: as the source of the I0 facts (§5.2), and as a VFIO host
   that passes its own I226 into a LazyOS guest (§5.3), so I3 can iterate
   without re-flashing a USB stick. The bare-metal boot from the stick stays
   the final acceptance (§5.1).
