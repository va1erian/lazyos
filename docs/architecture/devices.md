# Devices: the kernel device core

**What it is.** The shared kernel-side foundation under every driver: typed
device resources, a bus-enumeration seam, a fixed-capacity device table with
ownership and a generation counter, and a static in-kernel `Driver` table. It
knows buses, resources and IRQs — never what a "NIC" or "sound card" is. Class
semantics live in each driver's Messenger interface (see
[`docs/driver-plan.md`](../driver-plan.md)).

**Key files**

| Path | Role |
|---|---|
| `kernel/src/dev/mod.rs` | core types (`DeviceId`, `DeviceInfo`, `BusId`), boot enumeration, `DEV:ENUM` line |
| `kernel/src/dev/resources.rs` | typed resources: `Bar { index, kind, base, len, is_64, prefetchable }`, `Irq { line }` |
| `kernel/src/dev/pci.rs` | PCI config mechanism 1 (0xCF8/0xCFC): enumerate, BAR decode + write-ones sizing, command register, capability walk, interrupt line |
| `kernel/src/dev/bus.rs` | `Bus` trait + `PciBus`, builds `DeviceInfo` rows from bus enumeration |
| `kernel/src/dev/table.rs` | fixed `MAX_DEVICES = 32` table; `owner: Option<TaskSlot>` + `generation` |
| `kernel/src/dev/driver.rs` | `Driver` trait (`matches` / `attach` / `detach`) and the static `DRIVERS` table |

**Ownership and generations.** Each table slot carries an `owner` and a
`generation`. `claim` fails with `Busy` when a device is already owned; it
returns a `DeviceHandle { id, generation }`. `release` only succeeds for the
handle whose generation matches the slot, then clears the owner and bumps the
generation. A handle from an earlier claim therefore fails closed with `Stale`
instead of acting on a device that has since been released or reused.

**Typed resources.** A BAR is `Mem` or `Io` with a base, a length (from the
write-ones probe), a 64-bit flag and a prefetchable flag. An interrupt line is
an `Irq { line }`. BAR sizing writes all-ones over the BAR, reads the
mask back and restores the original value, with memory and I/O decode switched
off for the duration (PCI requires this) and the command register restored, so
enumeration leaves the device as it found it; an I/O BAR that implements only
16 address bits is sized correctly; the pair of registers that a 64-bit BAR occupies is sized as one
window. The `Irq`/`Msi` seam is what later interrupt work (D2) and MMCONFIG hang
off.

**In-kernel drivers.** `DRIVERS` is a static `&[&'static dyn Driver]`; nothing
is hot-loaded. At boot `dev::init` seeds the platform (ISA) devices, enumerates
PCI into the table, and runs the table: the first driver whose `matches` accepts
a device gets to `attach` it (claims it for the kernel owner) or is rolled back.
The table lock is never held across `attach`, so a driver may call back into the
core and a failed attach (for example the seeded ATA controller on a machine
with no IDE disk) releases its claim without deadlocking. Only one legacy
virtio-blk is driven, as before; a second matching function stays unattached.
The legacy block drivers (ATA PIO, legacy virtio-blk) are registered this way
with no behavior change; their `attach` still calls the same probe code, so the
block registry, boot-device selection and logs are identical.

**Boot line.** On a successful enumeration the kernel prints
`DEV:ENUM:PASS:<n> devices (<pci> PCI, <drivers> attached)`; an enumeration that
found no PCI function prints `DEV:ENUM:FAIL:no PCI devices enumerated`.

**Status.** Working (D1): platform + PCI enumeration, BAR sizing (32/64-bit),
command-register helpers, capability walk, interrupt-line read, claim/release
with generations, ATA/virtio-blk registered as in-kernel drivers. Open (later
stages): userspace `dev_*` syscall (D3), IRQ dispatch (D2), DMA (D4), MMCONFIG,
MSI/MSI-X, ACPI/platform enumeration beyond the single ATA seed.
