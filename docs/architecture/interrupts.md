# Interrupt routing: the I/O APIC and MSI

**What it is.** How a device interrupt reaches the CPU since issue #616
(docs/n150-driver-plan.md K1): the legacy lines on the I/O APIC instead of
the 8259, and message-signalled interrupts (MSI, MSI-X) on vectors of their
own for userspace drivers. The driver contract did not change: the same
`irq_enable`, the same one-way message from the kernel, the same `irq_ack`
([devices.md](devices.md), [driver-plan.md](../driver-plan.md) 3.3).

**Key files**

| Path | Role |
|---|---|
| `kernel/src/arch/irqchip.rs` | The legacy lines 0-15 on whichever controller delivers them: mask, EOI, spurious check, request register; the switch to the I/O APIC (`HW:IRQCHIP:` line) |
| `kernel/src/arch/ioapic.rs` | The I/O APIC at GSI 0: registers, redirection entries, mask |
| `kernel/src/arch/lapic.rs` | The local APIC: now always up when present (`init` is idempotent), LINT0 masked once the I/O APIC delivers, the IRR read the suite uses |
| `kernel/src/arch/msi_stubs.rs` | IDT stubs for the 32 MSI vectors (`0x40..0x60`) |
| `kernel/src/dev/msi.rs` | Vector pool, the lock-free vector handler, per-vector masking and latching, `route`/`unroute`, `DEV:MSI:PASS` |
| `kernel/src/dev/msi_hw.rs` | Programming the MSI and MSI-X capabilities; the kernel's MSI-X table mapping |
| `kernel/src/dev/bus.rs` | Capability discovery: `Resource::Msi`, `Resource::MsiX` (a table outside the function's BARs is dropped) |
| `libs/acpi/src/madt.rs` | `inti` (trigger/polarity from an override), `Signal::{ISA, PCI}`, `ioapic_for` |
| `tools/irqpath.py` | The harnesses' `--irq-path msi|pic` switch and judge |

## The legacy lines

Boot starts on the 8259, as before: the PIT probe reads its request register.
Once `timer::init` has chosen the tick, `irqchip::init` moves every line to
the I/O APIC that starts at GSI 0 when the MADT names one and the local APIC
is up with an ID below 256, then masks both 8259s and the local APIC's LINT0.
Each ISA line keeps its vector (`32 + line`), so every handler is unchanged;
only masking (a redirection-entry bit) and EOI (the local APIC's, which a
level-triggered entry broadcasts to the I/O APIC) differ.

A line goes to the GSI, trigger and polarity of its MADT interrupt source
override (IRQ 0 is GSI 2 on every QEMU machine). A line a PCI function names
in its Interrupt Line register is level-triggered, active low (PCI INTx) when
no override says otherwise; QEMU's `pc` and `q35` override lines 5, 9, 10 and
11 to level, active high (checked against the golden tables,
`libs/acpi/src/tests/mod.rs`). The cascade (2) is never routed.

That register is the firmware's 8259 routing. On QEMU the same wire reaches
the I/O APIC input of that number, so INTx works on both controllers. A real
chipset in APIC mode wires PCI INTx to inputs 16-23 instead, through the
DSDT's `_PRT`, which needs an AML interpreter LazyOS does not have: on real
hardware, PCI devices are expected to use MSI, and an INTx-only device may
stay silent (its driver polls). `LAZYOS_IRQCHIP=pic` keeps the 8259.

Boot log:

```
HW:IRQCHIP:ioapic pins=24 dest=0 pci_lines=0x0a00
irqchip: line 0 -> gsi 2 entry 0x20
irqchip: line 9 -> gsi 9 entry 0x18029
```

The PIT runs in mode 2 (rate generator): in mode 3 QEMU's I/O APIC edge
input raised IRQ 0 twice per period, a 200 Hz tick the 8259 never showed. A
PIT tick the local APIC already accepted when line 0 is masked is ignored
(`timer::stale_tick`), since masking an I/O APIC entry does not withdraw it.

On the I/O APIC lines 7 and 15 are ordinary: the 8259's spurious IRQs cannot
occur, and the I/O APIC's own spurious interrupts use the local APIC's
spurious vector (`0xFF`, no EOI).

## Message-signalled interrupts

A function with an MSI or MSI-X capability gets a vector of its own at
`irq_enable`. The kernel allocates it from 32 vectors (`0x40..0x60`: above the
APIC timer's `0x30`/`0x31`, below the syscall gate) and programs the
capability itself, address `0xFEE0_0000 | APIC ID << 12`, data = the vector
(fixed delivery, edge), one vector per function (Multiple Message Enable 0;
MSI-X table entry 0, every other entry masked). INTx-disable stays set. MSI is
preferred when a function has both (no table to map). `irq_enable` returns
the mode: 0 INTx, 1 MSI, 2 MSI-X (`user::dev::IrqMode`); with no vector free
the claim falls back to its INTx line, and a function with neither answers
`ENOSYS` (poll). `LAZYOS_MSI=0` keeps every claim on INTx.

A driver never writes an interrupt address: `cfg_write` still refuses all of
config space but the command register, and `map_bar` leaves the pages of an
MSI-X table out of the driver's mapping (a BAR that is nothing but the table is
refused, `EPERM`). The table is written through a kernel mapping made once per
device (`mmio::map_kernel`), with memory decode switched on around the write
if the claim has it off.

Delivery reuses `dev::intx`'s rounds. Vector `i` is delivery *source*
`16 + i`, next to the sixteen lines; `Claim::source()` is the vector once
routed, else the line. The vector handler (`msi::dispatch`) is lock-free like
`irq::dispatch`: it masks the vector in software, sets the source's raised bit
and sends the local APIC EOI. A message that arrives while its vector is masked
is *latched* (an MSI is an edge: nothing re-asserts it), and unmasking turns a
latched message into a raise, which is what a still-asserted level line does.
Where the hardware can (MSI-X always, MSI with mask bits) the vector is also
masked at the function while a round is open, so a storming device stops at
the source. Everything else, one outstanding message per claim, the ack
deadline, `missed` recovery, is the INTx code.

Teardown: `release` and task reap quiesce the function (bus mastering off, so
no message is in flight), then `unroute` switches its MSI/MSI-X off, forgets
any raise or latch and frees the vector; a dying task's vectors are freed at
exit (`silence_exited`). A message that still lands on a freed vector is
counted spurious. A fresh claim starts with both capabilities disabled.

`DEV:MSI:PASS:<bdf> dev <id> <Msi|MsiX> vector <v>` is printed the first time
each device is sent an interrupt on a vector: the proof of the whole path.

### Drivers

| Driver | Device (QEMU) | Mode on the msi path |
|---|---|---|
| `netdrv` | virtio-net | MSI-X: `Transport::use_msix(0)` points the config vector and every queue at entry 0 (`NETDRV:IRQ:MsiX`) |
| `netdrv` | e1000 | INTx (the 82540EM has no MSI) |
| `sndd` | virtio-sound | MSI-X, as virtio-net (`SNDD:IRQ:MsiX`) |
| `sndd` | intel-hda | MSI (`SNDD:IRQ:Msi`) |
| `usbd` | qemu-xhci | MSI-X (`USBD:IRQ hc=0 armed MsiX`); the table's page of BAR0 is a hole in `usbd`'s mapping |

A virtio function with MSI-X on never interrupts for a queue left at
`NO_VECTOR`, so a virtio driver must call `use_msix` when `irq_enable`
answers MSI-X (`libs/virtio`, host-tested).

## Verification

```bash
cargo test -p acpi -p virtio
python tools/test_irqpath.py
LAZYOS_TEST_FILTER=dev_ python tools/test/run.py --accel none        # dev_ioapic_*, dev_msi_*, every INTx case on the I/O APIC
LAZYOS_TEST_FILTER=dev_ python tools/test/run.py --accel none --extra-arg=-netdev --extra-arg=user,id=n0 --extra-arg=-device --extra-arg=virtio-net-pci,netdev=n0   # dev_irq_real_device_end_to_end on MSI-X
python tools/net/run.py --machine q35 --virtio-disk          # virtio-net on MSI-X, DEV:MSI:PASS
python tools/net/run.py --irq-path pic                       # the 8259 and INTx
python tools/sound/run.py --machine q35 --virtio-disk        # virtio-sound on MSI-X
python tools/sound/run.py --card hda --irq-path pic
python tools/usb/run.py                                      # qemu-xhci on MSI-X
```

The suite's cases: `dev_msi_raise_ack_ordering`, `dev_msi_storm_queue_depth_one`
(10 000 messages, one queued), `dev_msi_teardown_vector_in_flight` (release and
crash), `dev_msi_spurious_vectors`, `dev_msi_falls_back_to_intx` (every vector
taken), `dev_msi_soak` (2000 claim/interrupt/teardown cycles), and
`dev_ioapic_routes_every_line`, `dev_ioapic_entry_encoding`,
`dev_ioapic_mask_toggle_soak`. On an image built with `LAZYOS_IRQCHIP=pic
LAZYOS_MSI=0` the same suite runs the 8259 path (the I/O APIC and MSI cases
log `INFO` and check the fallback).

**Not done.** More than one vector per function (MSI-X per queue), the I/O
APIC's inputs 16-23 (needs `_PRT`), interrupt remapping (VT-d, with the IOMMU,
#615), and SMP destinations (S8: every message goes to the boot CPU).
