# Devices: the kernel device core

**What it is.** The shared kernel-side foundation under every driver: typed
device resources, a bus-enumeration seam, a fixed-capacity device table with
ownership and a generation counter, a static in-kernel `Driver` table, and
(issue #240) the interrupt path and the `dev_*` syscall that let an unprivileged
userspace driver claim a device. It knows buses, resources and IRQs — never what
a "NIC" or "sound card" is. Class semantics live in each driver's Messenger
interface (see [`docs/driver-plan.md`](../driver-plan.md)).

**Key files**

| Path | Role |
|---|---|
| `kernel/src/dev/mod.rs` | core types (`DeviceId`, `DeviceInfo`, `BusId`), boot enumeration, `DEV:ENUM` line |
| `kernel/src/dev/resources.rs` | typed resources: `Bar { index, kind, base, len, is_64, prefetchable }`, `Irq { line }` |
| `kernel/src/dev/pci.rs` | PCI config mechanism 1 (0xCF8/0xCFC): enumerate, BAR decode + write-ones sizing, command register, capability walk, interrupt line |
| `kernel/src/dev/bus.rs` | `Bus` trait + `PciBus`, builds `DeviceInfo` rows from bus enumeration |
| `kernel/src/dev/table.rs` | fixed `MAX_DEVICES = 32` table; `owner: Option<TaskSlot>` + `generation` |
| `kernel/src/dev/driver.rs` | `Driver` trait (`matches` / `attach` / `detach`) and the static `DRIVERS` table |
| `kernel/src/dev/irq.rs` | ISR-side `dispatch(line)` (lock-free), kernel `fn(line)` handlers, which lines are routable |
| `kernel/src/dev/intx.rs` | Task-context bottom half and the shared-INTx contract: rounds, ack deadline, `missed` recovery |
| `kernel/src/dev/claims.rs` | Userspace claims: rights, `Device` handle, interrupt binding and state, BAR mappings |
| `kernel/src/dev/grant.rs`, `class.rs` | The grant rule; PCI class to `os.kernel.dev.<class>` ids and method ids |
| `kernel/src/dev/syscall.rs`, `ops.rs`, `dma.rs` | Syscall 23: `list`/`claim`/`release`/`irq_*`; `map_bar`/`pio`/`cfg_*`; `dma_alloc` |
| `kernel/src/dev/policy.rs`, `inspect.rs` | The driver class rules installed at boot (#481); the read-only `inventory`/`policy`/`denials` ops |
| `kernel/src/dev/teardown.rs`, `report.rs`, `selfcheck.rs` | Release on exit; audit records; the `DEV:IRQ`/`DEV:SYSCALL`/`DEV:DMA` boot lines and routing log |
| `kernel/src/mem/dma.rs` | Boot-time contiguous DMA pool: bitmap, first-fit with alignment, stats |
| `kernel/src/ipc/shared/dma.rs` | `create_from_frames`: a Buffer over an existing contiguous run, `DmaOwner` |
| `kernel/src/arch/irq_stubs.rs`, `arch/pic.rs` | IDT stubs for the PIC lines; mask/EOI/in-service/IRR helpers |
| `kernel/src/mem/mmio.rs`, `mem/cow.rs` | Uncached MMIO mappings tagged with a software PTE bit; fork split out of `mem/mod.rs` |
| `kernel/src/ipc/channels_kernel.rs` | `post_from_kernel`: one-way messages from the kernel identity |
| `user/src/dev.rs` | Userspace wrappers for syscall 23 |

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
window. A BAR that reads 0 is *unassigned*, not absent: it is sized as a
32-bit memory BAR and reported with base 0 and its length (a BAR whose sizing
finds no window is skipped). The `Irq`/`Msi` seam is what later interrupt work (D2) and MMCONFIG hang
off.

**In-kernel drivers.** `DRIVERS` is a static `&[&'static dyn Driver]`; nothing
is hot-loaded. At boot `dev::init` seeds the platform (ISA) devices, enumerates
PCI into the table, and runs the table: the first driver whose `matches` accepts
a device gets to `attach` it (claims it for the kernel owner) or is rolled back.
The table lock is never held across `attach`, so a driver may call back into the
core and a failed attach (for example the seeded ATA controller on a machine
with no IDE disk) releases its claim without deadlocking. Each legacy
virtio-blk function is its own block device (up to four, `virtio0`..`virtio3`;
see [block-devices.md](block-devices.md)).
The legacy block drivers (ATA PIO, legacy virtio-blk) are registered this way
with no behavior change; their `attach` still calls the same probe code, so the
block registry, boot-device selection and logs are identical.

**Interrupts (D2).** PIC lines 3-11 and 13-15 (and the cascade) have IDT stubs
(`arch/irq_stubs.rs`); the timer, keyboard and mouse keep their own handlers and
can never be claimed. `irq::dispatch(line)` runs in interrupt context on one CPU,
so it takes no lock and allocates nothing: it recognises a spurious IRQ 7/15
(in-service register), runs a kernel driver's `fn(line)` if one is registered,
and otherwise masks the line, sets an atomic raised bit and sends a *specific*
EOI (an unclaimed line is masked and counted, so it cannot storm). The bottom
half, `intx::service`, runs from every native syscall entry and each mux frame:
it posts one one-way message per armed claimant from the kernel identity
(`ipc::channels::post_from_kernel`, sender slot 0) and expires ack deadlines.
The message is interface `os.kernel.dev`, method `irq`, body `u32` device id,
`u32` irq index (0), `u32` claim generation. The shared-line contract (opt-in
sharing, one outstanding message per (claim, line), unmask after the last ack,
100-tick deadline, `missed`-bit recovery, exclusive lines fail `EBUSY`) is
`docs/driver-plan.md` section 3.3, implemented in `intx.rs`.

**The `dev_*` syscall (D3).** Syscall 23 (see the table in
`docs/driver-plan.md` 3.2 and the header of `dev/syscall.rs`). `claim` checks
`CAP_DEV_CLAIM`, resolves the device, authorizes `os.kernel.dev.<class>`,
derives rights = device resources AND class policy (`EPERM` before any owner is
recorded when empty), then takes ownership, the `DeviceClaims` charge and the
handle, undoing each on failure, and finally quiesces the PCI function (decode
and bus-master off, INTx disabled). Every later op resolves the handle against
the device table (owner, generation) and the claim table. MMIO maps into
`0x30_0000_0000..0x38_0000_0000`, inside PML4 entry 0, so a mapping exists only
in the claimant's own page tables (the shared-buffer range is one page-table
subtree shared by every address space); its leaves carry software PTE bit 10
(`pte::MMIO`), which `unmap_range`, `free_user_table` and `fork` honour, so a
device frame is never freed, shared or inherited. `ipc::teardown_task`
releases every claim first: interrupts masked and the claimant dropped from
rounds, decode/bus-master cleared with INTx disabled, MMIO unmapped and
uncharged, generation bumped, one audit record. A task that has *exited* but
is not yet reaped is a zombie whose address space (and MMIO mappings) lives on
until the parent reaps it, so the claim is released then; but the dangerous
part is stopped at exit: `process::finish` marks the slot and
`dev::silence_exited` (run from the interrupt bottom half and from `finish`)
takes its claims out of interrupt delivery, masks a line nobody else listens
on, and clears the function's decode/bus-master enables.

**Driver class rules (#481).** Each driver's device-class rules are data in
its own crate (`libs/netpolicy`, `libs/usbpolicy`, `libs/sndpolicy`: claim,
map and DMA on its one class). `dev::policy::install_boot_policy` compiles
them in and installs them right after enumeration (`DEV:POLICY rules=9`),
before `init` can start a driver. The set is separate from the Messenger uid
policy, which is still in its bootstrap window: loading the driver rules there
would default-deny every other Messenger call. `claim` must pass both; once
installed, a non-root uid may claim, map or DMA a class only if a rule allows
it (first match wins), and a refusal is audited as `CLASS_DENIED` (`0x22`).
Root keeps its ambient authority (the harness images boot drivers as root, and
root can become any driver uid anyway); the in-kernel drivers and the PS/2 and
display paths never call `claim`. The kernel suite installs the same policy
(`dev_sys_boot_policy_*`), and each driver proves it on a real boot under its
harness flag: `user::dev::inspect::cross_class_probe` tries every device of
another class and prints `DEV:CROSSCLAIM:<snd|net|usb>:PASS` when all were
refused with `EACCES`; the sound, net and USB judges require it in their
`--services` runs.

**Seeing it (#481).** Three read-only ops (10-12, `dev/inspect.rs`, layouts
in `libs/devinspect`) show the inventory with each owner's uid and rights,
the installed rules, and the refused claims (`CAP_AUDIT_READ`). Nothing can
edit the rules at run time, on purpose. `devctl [devices|rules|denials]` prints
them from a shell; the **Devices** desktop app (`xui-app/src/bin/devices.rs`,
Start menu, or `python tools/run_demo.py --devices` to open it at boot) shows
the same and refreshes every two seconds
(`tools/screenshot/examples/devices_desktop.json` drives both).

**The interrupt endpoint (partly hardened, #283).** Kernel-stamped
`os.kernel.dev` messages are posted into the inbox of the channel side named at
`claim`, and whoever holds that side reads them. `claim` requires the side to be
held by exactly that one handle in the task tables (a name resolve gives every
client a handle to the same side, so a resolved service endpoint is refused with
`EBADF`) and drops its `DUPLICATE`/`TRANSFER` rights. **Known gaps:** a handle
duplicated earlier and currently *in flight* in a queued message, and a side
published in the name registry, are not counted; closing them needs a
kernel-owned IRQ channel kind (tracked in #283 item 1).

**IRQ routing on QEMU (observed).** The boot log prints one
`dev: irq route ...` line per PCI function (pin, Interrupt Line, verdict). A
function with INTx pin 0 has no `Irq` resource. Firmware (SeaBIOS) programmed a
PIC-routable line on both machine types:

| Machine | Function | Pin | Line |
|---|---|---|---|
| `pc` (i440fx) | PIIX4 power management `8086:7113` | 1 | 9 |
| `pc` | e1000 `8086:100e` / virtio-net `1af4:1000` (00:03.0), virtio-blk `1af4:1001` | 1 | 11 |
| `pc` | host bridge, PIIX3 ISA and IDE, std VGA | 0 | none |
| `q35` | virtio-blk `1af4:1001` (00:02.0), virtio-net `1af4:1000` (00:03.0) | 1 | 11 |
| `q35` | ICH9 AHCI (00:1f.2), SMBus (00:1f.3) | 1 | 10 |
| `q35` | host bridge, VGA, ICH9 LPC | 0 | none |

The four PIRQs are shared, so virtio-blk (polled by the kernel) and any userspace
NIC share line 11; in-kernel drivers therefore disable their function's INTx at
attach. The test `dev_irq_real_device_end_to_end` proves the wiring end to end
whenever the VM has a legacy virtio-net: a userspace "driver" claims it, kicks
a TX descriptor with `pio` only, and checks that the PIC latches line 11, the
real ISR runs, one message reaches its endpoint from the kernel, and `irq_ack`
unmasks the line. It passed on both machines (the CI image has no such
function and only logs the routing). Reproduce with
`qemu-system-x86_64 ... -netdev user,id=n0 -device virtio-net-pci,netdev=n0,disable-modern=on`
(add `-machine q35` and a virtio-blk disk, since q35 has no IDE); CI runs the
suite three ways (`default`, `--nic`, and `--machine q35 --virtio-disk --nic`
through `tools/test/run.py`), and the ATA-specific tests skip with an `INFO`
line when the machine has no ATA disk. A line that
is reserved or out of range (or a function with no pin) is not routable: the
claim succeeds and `irq_enable` returns `ENOSYS`, the polling fallback; the
suite covers it with the reserved mouse line and a `0xFF` line.

**Boot line.** On a successful enumeration the kernel prints
`DEV:ENUM:PASS:<n> devices (<pci> PCI, <drivers> attached)`; an enumeration that
found no PCI function prints `DEV:ENUM:FAIL:no PCI devices enumerated`, and one
that found more functions than the 32-entry table holds prints
`DEV:ENUM:FAIL:device table full, <n> PCI function(s) dropped`.

Three more boot lines come from `dev::selfcheck` after the IDT is loaded:
`DEV:IRQ:PASS:16 vectors installed, <r>/<w> INTx-wired PCI functions on routable
lines` (each of the 16 PIC vectors has a present gate),
`DEV:SYSCALL:PASS:syscall 23 gates refuse, 0 claims` (unknown op, kernel-task
claim and a bad handle all refuse cleanly), and
`DEV:DMA:PASS:<pages> pages, largest run <n>` for the boot-time DMA pool
(`DEV:DMA:INFO:no DMA pool reserved` when RAM is too small).

**DMA (D4, #241).** A driver with the `DMA` right calls
`dma_alloc(dev, len, flags, out)`: the kernel takes a contiguous run from the
boot-time DMA pool, wraps it in the ordinary shared-buffer object
(`ipc::shared::create_from_frames`), and writes the run's physical address —
the bus address until an IOMMU exists — to `*out`. The run is page aligned
(larger alignments are honoured by the allocator), zeroed, below 4 GiB, and
charged to `Resource::DmaMemory` (default half the pool, at least 8 MiB per
uid); `len` is 1 byte up to the whole pool and the flags are `SHARE_ONLY` and "64-bit address OK". The returned `Buffer`
handle is transferable and can be handed to a client zero-copy. The pool is
reserved once at `mem::init` (`limits::dma_pool_bytes`: 1/32 of RAM, 16..64
MiB, never more than 1/8 of RAM; docs/architecture/limits.md), above the low
megabyte,
clear of the refcount table); its frames stay in the frame refcount table but
are marked `RESERVED` while free, so the general allocator never hands them
out, and a pool frame returns to the pool — not the general free list — when
its last reference drops. Pool frames are outside `FrameStats::total`/`free`,
so DMA traffic never perturbs a frame `live()` delta. The claim records each
live DMA buffer by object id (bounded to 16 per claim); `release` and task
teardown quiesce the device first (bus mastering off) and then close the
owner's reference, so a client that still holds a transferred buffer keeps its
frames and charge until the last reference goes. **Without an IOMMU a driver
with `DMA` is trusted like the kernel**: it can program its device to write any
physical address, and only the ACL, its dedicated uid and audit limit who can
be that driver (driver-plan D5).

**Status.** Working: platform + PCI enumeration, BAR sizing (32/64-bit),
command-register helpers (status bits are never written back), capability walk,
claim/release with generations, ATA/virtio-blk as in-kernel drivers (D1);
interrupt dispatch and the shared-INTx contract (D2); the `dev_*` syscall, MMIO
mappings, grant rule, class ACL, audit, quota and teardown (D3); the DMA pool
and `dma_alloc` (D4, #241). Not done: function-level reset (teardown clears the
command register enables instead), MMCONFIG, MSI/MSI-X, IOAPIC,
ACPI/platform enumeration beyond the single ATA seed, and an IOMMU.

**DMA and the device's lifetime.** Bus mastering is always off before a DMA
run can be reused. `release`, task exit and task reap quiesce the device. While
the claim is live: (a) the *driver* closing its own last reference is an
explicit free and quiesces the device first (`dev::dma_buffer_freed`: bus
mastering and decode off, INTx disabled), so it re-enables what it needs
afterwards; (b) anyone else dropping the last reference (a client closing a
transferred buffer, a discarded in-flight message) does not stop the device:
the run is *quarantined* (`dev::dma_quarantine`), keeping its pool pages and
`DmaMemory` charge until `release_claim` has quiesced the device and frees
them; (c) a failed `dma_alloc` never showed the device the address, so it
frees without quiescing. DMA buffer leaves carry software PTE bit 11
(`pte::DMA`) and `fork` gives the child no mapping for them (like MMIO), so a
forked driver cannot end up with a private copy the device never sees.
