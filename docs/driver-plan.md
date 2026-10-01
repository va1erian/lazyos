# LazyOS driver architecture — plan

Status: **draft for review**. Scope: a small, generic driver model that fits
Messenger and the capability/ACL security core. First consumers: QEMU's
emulated **network card** and **sound card**. Out of scope: hot-loadable
driver modules, and any network stack (sockets, IP, TCP, DHCP, DNS — see
`platform-plan.md` S6).

Related: [platform-plan.md](platform-plan.md) §4.2 ("Driver model"),
[security-model.md](security-model.md), [messenger.md](messenger.md),
[architecture/block-devices.md](architecture/block-devices.md).

## 1. Where we are

- Drivers today are three **in-kernel singletons** (`kernel/src/block/`: ATA
  PIO, legacy virtio-blk, config-mechanism-1 PCI). No shared driver
  abstraction beyond `BlockDevice`; the block registry is a fixed array.
- PCI: enumerate + read BAR only. No BAR sizing/mapping, no command-register
  control, no capability list, no MSI. Modern (memory-BAR) virtio is detected
  but not driven (`1af4:1042`).
- Interrupts: 8259 PIC only; handlers exist for timer, IRQ1, IRQ12. All other
  drivers poll. No generic IRQ registration.
- DMA: one static 4 KiB bounce page; `virt_to_phys` by page-table walk. No
  contiguous allocator, no pinned user buffers.
- Messenger has `Endpoint/Channel/Object/Buffer` handles, shared buffers
  (page-aligned frames mapped into several spaces, fences), kernel-stamped
  credentials, a default-deny ACL with audit, per-uid quotas, and
  `teardown_task` cleanup. `CAP_SYS_ADMIN` is documented as "driver grants";
  `CAP_DEV_CLAIM` (issue #240) is the one device capability.
- `init` supervises userspace services from a static manifest with restart and
  health topics.

## 2. Design decisions

### D1. Two tiers, one contract

| Tier | Who | Used for |
|---|---|---|
| **Kernel drivers** | statically linked, implement a `Driver` trait | boot-critical: block (FS needs it before init), PS/2, serial, timer |
| **Userspace drivers** | ordinary ring-3 services, unprivileged uid, granted *device handles* | everything else: NIC, sound, later GPU/USB |

New drivers default to **userspace**. Reasons: a driver crash is a supervised
restart (init already does backoff + health), the driver is a normal Messenger
service so class interfaces, ACL, audit and quotas apply unchanged, and the
kernel stays small. Cost: syscall/IPC latency on the control path; the data
path uses shared-buffer rings, so this is acceptable for a NIC and audio.

The existing block drivers stay in-kernel and are ported onto the shared
**device core** (§3.1) and PCI/virtio transport code, not rewritten as
userspace services (D1 registers them; D7 moves block to modern virtio).

### D2. Kernel provides mechanism, not device knowledge

The kernel knows buses, resources, IRQs and DMA memory. It never knows what a
"NIC" is. Class semantics (net, audio, ...) live entirely in the driver's
Messenger interface, defined in IDL. That keeps the kernel surface identical
for every device class.

### D3. Authority is a handle, not an ambient capability

A driver gets a **`Device` handle** (new `HandleKind::Device`) for exactly the
device it claimed. Rights bits on the handle (`MMIO`, `PIO`, `IRQ`, `DMA`,
`CONFIG`) gate each resource family. No `CAP_DEV_*` bit is needed for
per-device access; the reserved `CAP_DEV_*` idea reduces to one coarse gate,
`CAP_DEV_CLAIM`, required to *call claim at all*, so an app can never even
attempt it.

**Grant rule.** `claim` derives the handle's rights as the intersection of
(a) the resources the device actually has (`MMIO`/`PIO` only if it has a
memory/I/O BAR, `IRQ` only if it has an interrupt line, `DMA` only if it is
bus-master capable, `CONFIG` always) and (b) what the class-specific policy
rule permits for this actor. If the intersection is empty, `claim` fails with
`EPERM` *before* any owner is recorded, so no handle is created and the device
stays unclaimed; the denial is audited. Rights are fixed at claim time and can
only be narrowed afterwards, never widened.

### D4. Interrupts arrive as Messenger messages

The kernel ISR masks the line, sets an atomic "raised" bit and sends EOI; it
takes no lock and allocates nothing, because on one CPU whatever it interrupted
may hold the heap or a channel lock. A task-context bottom half (run at every
syscall entry and from the mux loop) then posts a one-way message **from the
kernel identity** to the endpoint the driver registered. The driver receives it
in the same `Selector`/`recv` loop as its client requests and calls `irq_ack`
after servicing (which unmasks). At most one message is outstanding per
(claim, IRQ) (§3.3 covers shared lines), so an interrupt storm cannot fill a
queue or burn the driver's `QueueDepth` quota. This is the "interrupt → event
delivery through Messenger" line from the platform plan.

### D5. Be honest about DMA

Without an IOMMU a DMA-capable driver can write any physical memory, so a
**driver with a `DMA` right is trusted like the kernel**. The plan does not
pretend otherwise. Mitigations in order: (1) driver uid + ACL + audit limit
who can be that driver; (2) teardown always disables bus mastering and resets
the device before frames are freed, so a dead driver cannot scribble on
recycled memory; (3) IOMMU (VT-d; QEMU offers `intel-iommu`) is a named later
stage that turns the `DMA` right into a per-device IOVA domain without changing
the driver-facing API (§3.4 hands out *bus addresses*, which today equal
physical).

## 3. Architecture

```
   apps ──Messenger──▶ os.lazy.net.nic.v1 / os.lazy.audio.v1   (class IDL)
                          ▲ served by
                   ┌──────┴───────┐   unprivileged uid, supervised by init
                   │ driver task  │   e.g. netd-virtio, sndd-virtio
                   └──────┬───────┘
      Device handle: MMIO map · port IO · IRQ msgs · DMA buffers · config
   ═══════════ syscall 23: dev_* ═══════════════════════════════════════
   ┌──────────────────────── kernel device core ─────────────────────────┐
   │ bus enumeration (PCI) → Device table → claim/ACL/audit → resources │
   └─────────────────────────────────────────────────────────────────────┘
        devd (userspace, matches devices to drivers, publishes topics)
```

### 3.1 Kernel device core (`kernel/src/dev/`)

- **`DeviceId`**: stable small integer per discovered function.
  `DeviceInfo { id, bus: Pci, ids: (vendor, device, subsys), class, resources }`
  where resources are typed: `Bar { index, kind: Mem|Io, base, len }`,
  `Irq { line }`, later `Msi`.
- **Bus enumerators** produce `DeviceInfo`. Only PCI now, behind a `Bus` trait
  so ACPI/platform devices can be added without touching drivers.
- **Table**: fixed capacity (like the block registry), no per-device heap
  allocation on the hot path. Each entry has `owner: Option<TaskSlot>` and a
  generation counter so stale handles fail closed.
- **`Driver` trait** for in-kernel drivers: `matches(&DeviceInfo) -> bool`,
  `attach(&Device) -> Result`, `detach()`. Static table of `&'static dyn
  Driver`, probed at boot. (No dynamic loading, per scope.)
- **PCI upgrades** (`dev/pci.rs`, moved from `block/pci.rs`): BAR sizing
  (write-ones probe), 64-bit BARs, command-register helpers (memory space,
  I/O space, bus master), capability-list walk, interrupt-line read. MMCONFIG
  and MSI/MSI-X are deferred but the `Irq` resource type leaves room.

### 3.2 Userspace surface: syscall 23 `dev_*`

Syscall numbers 15-22 are the filesystem, `power` and `fsync` calls
(`process/fsops.rs`), so the device syscall is **23**. Same multiplexed style
as syscalls 5/12/14 (op in `rdi`, arguments in `rsi`, `rdx`, `r10`, `r8`;
result or `-errno` in `rax`). Implemented in `kernel/src/dev/syscall.rs`; the
user wrappers are `user/src/dev.rs`.

| Op | Effect | Checks |
|---|---|---|
| `list(buf, rows)` | copy device rows (ids, class, BAR sizes/kinds, `owned`, `generation`; never physical BAR bases) | `CAP_DEV_CLAIM` (devd), `os.kernel.dev` `list` |
| `claim(id, irq_endpoint, flags)` | returns a `Device` handle; sets owner; `flags` bit 0 opts in to a shared interrupt line | `CAP_DEV_CLAIM`; `authorize(actor, "os.kernel.dev.<class>", claim)` with the resolved device class (§3.5), rights per the grant rule (§2 D3), device unowned, `DeviceClaims` quota |
| `map_bar(dev, bar)` | maps a memory BAR uncached, no-exec, into the caller's private user range | handle right `MMIO`; BAR at least a page, at most 64 MiB, not overlapping RAM; charged to `UserMemory` by uid |
| `pio(dev, bar, off, width\|write\|val)` | port in/out inside the device's I/O BAR only | handle right `PIO`; offset+width < len; never below port 0x100 or in 0xCF8-0xCFF |
| `cfg_read/cfg_write(dev, off, width, val)` | PCI config; the only writable register is the command register, masked | right `CONFIG`; bus-master needs `DMA`; no BAR programming |
| `irq_enable(dev, 0)` / `irq_ack(dev, 0)` | arm / acknowledge (unmask) | right `IRQ`; `ENOSYS` when the line is not PIC-routable |
| `dma_alloc(dev, len, flags, out)` | physically contiguous frames as a **Buffer handle**; writes the bus address to `*out` | right `DMA`, quota `DmaMemory`; `flags` bit 0 `SHARE_ONLY`, bit 1 64-bit address OK; `len` 1..=4 MiB |
| `release(dev)` | quiesce + free | owner |
| `inventory(buf, rows)` (10) | read-only: each device's id, class, PCI ids, owner uid and claim rights | `os.kernel.dev` `inventory` (labelled apps refused) |
| `policy(buf, rows)` (11) | read-only: the driver class rules installed at boot (`ENOENT` before) | `os.kernel.dev` `policy` |
| `denials(buf, rows)` (12) | read-only: refused claims still in the audit ring, newest first | `CAP_AUDIT_READ`, `os.kernel.dev` `denials` |

Every op re-checks the handle against the device table and the claim table
(owner, generation), so a handle from another task, an earlier claim or a
released device fails with `EBADF`. Everything is bounds-checked against the
device's own resources; no op takes a raw physical or port address from
userspace (no ambient authority).

### 3.3 Interrupt path

- Replace fixed handlers with **vector stubs 32–47** that call
  `dev::irq::dispatch(line)`: if the line is not the kernel's own (timer,
  keyboard, cascade, mouse keep their handlers) and no kernel driver owns it
  → mask the line, set an atomic raised bit, EOI. A spurious IRQ 7/15 is
  recognised from the PIC in-service register and dropped. The **bottom half**
  (`dev::intx::service`, task context) turns the raised bit into the
  kernel→driver one-way message and runs the ack-deadline sweep; the message is
  still posted from the kernel identity, just not from the ISR itself.
- **Shared INTx contract.** A line may be shared only by claimants that
  opted in at `claim` time (each supplies its own `irq_endpoint`) and armed the
  line with `irq_enable`. On an interrupt the kernel masks the line once and
  posts **one message to every armed claimant**; each claim keeps its own
  pending bit, so the one-outstanding limit is enforced **per (claim, line)**,
  not per line. The line is unmasked only when every claimant that was sent a
  message has called `irq_ack` (a claimant that has not armed, has released, or
  died is not waited on). A claimant that does not ack within a bounded
  deadline (100 ticks) is dropped from that delivery round: the kernel unmasks
  the line, audits the laggard, and leaves its pending bit set (it stays "owed"
  one ack), so a hung driver cannot hold a shared line masked and starve its
  co-claimants. **Recovery:** while a claim is owed, later interrupts on the
  line are not posted to it; the kernel only records a `missed` bit, so its
  queue stays bounded. A late `irq_ack` clears the pending bit, makes the
  claim eligible for delivery again, and, if `missed` is set, immediately
  posts one fresh message (level-triggered devices simply re-assert). A late
  ack never unmasks the line for other claimants; that already happened when
  the round timed out. Devices that do not
  opt in get exclusive lines; a second claim on an occupied exclusive line
  fails with `EBUSY`. In-kernel drivers only poll and disable their function's
  INTx, so they never assert a line a userspace claimant shares.
- Kernel drivers register a plain `fn(line)` instead of a message.
- APIC/IOAPIC and MSI are out of scope; the `Irq` resource kind and the
  dispatch table are the seam. **Verified on QEMU** (see
  [`architecture/devices.md`](architecture/devices.md)): the firmware programs a
  PIC-routable Interrupt Line on both `pc` (i440fx) and `q35`, and a real
  virtio-net interrupt travels to a userspace claimant end to end on both. A
  function whose INTx pin is 0 carries no `Irq` resource, and a line that is
  reserved or out of range falls back to polling (`irq_enable` returns
  `ENOSYS`), which is also the CI-safe default.

### 3.4 DMA buffers

`dma_alloc` returns a `HandleKind::Buffer` (the existing shared-buffer object)
and writes the **bus address** so the driver can program descriptors. Frames
are allocated **physically contiguous** from a DMA pool reserved once at boot
(`mem::dma`): `min(16 MiB, usable/8)`, below 4 GiB (32-bit-capable devices),
above the low megabyte, and never handed out by the general frame allocator.
The pool's bitmap lives inside the frame allocator's lock, so the documented
order `REGISTRY -> FRAMES` covers every DMA path; a pool frame returns to the
pool when its last reference drops. Bytes are charged to a new
`Resource::DmaMemory` quota (default 8 MiB per uid); the per-process buffer
*count* cap still applies, but the shared-buffer 8 MiB byte cap does not.

Because it is a normal Buffer handle (with `TRANSFER`/`DUPLICATE` rights) it
can be **passed to a client in a Messenger message** for zero-copy
audio/packet payloads, and `SHARE_ONLY` lets a client hand a buffer to the
driver without mapping it. The buffer is never executable. On the last
reference drop the frames return to the pool and the `DmaMemory` charge is
released exactly once, against the uid that allocated it. `release` and task
teardown close the owner's reference by buffer object id, so a transferred
handle that a client still holds keeps the frames and the charge alive; bus
mastering is cleared before any of those frames can be reused. Drivers should
keep descriptor rings driver-owned and copy/validate client data into them; the
driver never trusts client lengths.

### 3.5 Security integration

- **ACL**: new interface `os.kernel.dev` with methods `list`, `claim`,
  `map`, `dma`. `claim` first resolves the device and its class, then calls
  `authorize` with a **class-specific `interface_id`** (`os.kernel.dev.<class>`,
  e.g. `os.kernel.dev.net` for PCI class 0x02), and only assigns ownership if
  that verdict allows. A generic "may claim" rule therefore cannot authorize
  claiming a class the policy did not name: "label `net-driver` may claim
  class net" says nothing about audio or storage. Default deny once policy is
  loaded; the bootstrap window is allow, as elsewhere (unchanged). `claim` is
  the gate and yields `CONFIG|IRQ`; the `map` and `dma` methods on the same
  class id are consulted (without an audit record) to add `MMIO|PIO` and
  `DMA`, so policy can withhold a family the device has.
- **Credentials**: drivers run as dedicated system uids (`_net`, `_snd`) with
  only `CAP_DEV_CLAIM` (+ the per-class ACL rule), launched by init via
  `spawn_as`. They can never `CAP_SETUID` or reach uid 0.
- **Audit**: every claim/release/denial is a record with device id, class and
  reason code, in the existing hash-chained ring.
- **Quotas**: `Resource::DeviceClaims` (default 8 per uid, landed in #240) and
  `Resource::DmaMemory` (issue #241) in `quota.rs`. BAR mappings are charged to
  `UserMemory` by uid, not through the per-address-space ledger.
- **Teardown**: `ipc::teardown_task` gains "release all claims": mask IRQs,
  clear PCI bus-master and memory/IO enable and set INTx-disable, unmap MMIO,
  free DMA frames (issue #241), clear `owner`, bump generation, drop the task
  from shared-line rounds. There is no function-level reset yet. The kernel
  cannot publish `system/events/device/<id>` (topics are a userspace service),
  so `list` rows carry `owned` and `generation` and `devd` polls or reacts to
  the driver's exit to respawn it.
- **Names/topics**: driver services register `os.lazy.<class>.<vendor>` names
  under the existing registry policy; clients discover by class through
  `devd`, not by knowing the driver.

### 3.6 `devd` (userspace)

Small service with `CAP_DEV_CLAIM` and list right only: reads the device list,
matches against a static **driver manifest** (PCI vendor/device/class →
driver program, uid, restart policy — same shape as `init`'s `MANIFEST`),
asks `init` to launch the driver, and publishes retained topics
`system/devices/<id>` (added/claimed/removed, class, state) and
`system/health/<driver>`. It never touches device memory itself. Hot-plug is
explicitly not built, but the topic shape supports it.

### 3.7 Configuration

Driver and device preferences (irq mode, buffer sizes, MAC override, default
audio format, enable/disable policy) live in the `confd` registry; the kernel
never reads it. See [driver-config-plan.md](driver-config-plan.md).

### 3.8 Class interfaces (Messenger IDL, `docs/idl/`)

Kept minimal and versioned; each is a *control* interface plus a shared-buffer
data plane.

**`os.lazy.net.nic.v1`** (link layer only — no IP):
`Info() → {mac, mtu, link, features}`, `SetRxMode(mode)`,
`AttachRing(rx_buf, tx_buf, notify_topic)` — two single-producer/
single-consumer frame rings in shared buffers with fences, `Stats()`. Link
change published on `system/net/<nic>/link`. A future stack service is just
another client of this interface (and can be the only holder of the ring
buffers).

**`os.lazy.audio.v1`** (`idl/audio.midl`, as built):
`Info() → {streams, formats, rates, channels}`,
`OpenStream(dir, format, rate, channels, period_bytes) → StreamGrant` (closest
supported parameters), `AttachRing(stream)` (the request carries the shared
ring), `Commit(stream, frames) → consumed`, `Start/Stop/Drain(stream)`,
`Position(stream)`, `CloseStream(stream)`; underrun/xrun on
`system/audio/<card>/event` (declared, not yet published). **The client owns the
ring and the driver copies out of it**: replies cannot carry buffers, and this
is also section 3.4's rule that driver rings stay driver-owned. Mixing and
per-app volume are a later `audiod` service, not the driver's job.

## 4. QEMU first candidates

| Class | Driver #1 (this plan) | Why | Driver #2 (validation of genericity) |
|---|---|---|---|
| NIC | **virtio-net-pci** | reuses one virtio transport with blk and snd; simplest rings | **e1000** (`-device e1000`): non-virtio, BAR-register + descriptor rings, proves the core is not virtio-shaped |
| Sound | **virtio-sound-pci** (present in QEMU 10.2 here) | same transport, tiny control/event/tx/rx queues | **intel-hda** (`-device intel-hda -device hda-duplex`) or AC97 |

Shared piece: a **modern virtio-PCI transport** library (capability parse,
common/notify/ISR/device configs in memory BARs, feature negotiation incl.
`VIRTIO_F_VERSION_1`, split virtqueues). It works from a userspace `Device`
handle and from the in-kernel `Driver` tier, and finally lets the block driver
move off legacy 0.9.5. Interim: QEMU `disable-modern=on` lets the current
legacy code path run virtio-net for a spike, but the deliverable is the modern
transport.

Headless verification uses QEMU only:
- NIC: `-netdev user,id=n0 -device virtio-net-pci,netdev=n0` plus
  `-object filter-dump,id=f,netdev=n0,file=net.pcap`; the test sends a raw
  broadcast/ARP frame and asserts it in the pcap, and injects an ARP reply
  through user-mode networking to check RX.
- Sound: `-audiodev wav,id=a0,path=out.wav -device virtio-sound-pci,audiodev=a0`;
  the test plays a known tone and a script asserts the WAV (length, non-silent,
  frequency via zero crossings). `-audiodev none` for CI smoke.
- Add `--net`/`--sound` flags to `run_demo.py` and the QMP launch helper, and
  matrix entries in `docs/architecture/`.

## 5. Staged delivery

Each stage is independently mergeable; **every kernel stage ships correctness
+ stress tests** under `kernel/src/tests/dev_suite.rs` per AGENTS.md, and
`python tools/test/run.py --accel none` must pass. Files stay under 500 lines.

**Stage D0 — Docs and IDL (no code).** Land this plan, add
`docs/architecture/devices.md`, draft `os.lazy.net.nic.v1`/`os.lazy.audio.v1`
IDL, list `CAP_DEV_CLAIM` in `security-model.md`, tick the driver-model line
in the platform plan.

**Stage D1 — Device core + PCI upgrade.** `kernel/src/dev/`, BAR sizing and
64-bit BARs, command-register control, capability walk, `Device` table with
owner/generation, `Driver` trait; move `block/pci.rs` under it and register
the existing block drivers as in-kernel drivers with no behavior change.
Tests: enumeration of QEMU q35 devices, BAR size probe vs known devices,
claim/unclaim/double-claim, generation invalidation, 1M claim/release cycles
with no leak. Boot line: `DEV:ENUM:PASS`.

**Stage D2 — Interrupts (done, #240).** Vector stubs and dispatch, mask/ack,
shared INTx, kernel→driver one-way message with a task-context bottom half
(one outstanding per claim). Verified q35 and i440fx interrupt-line routing.
Tests: raise/ack ordering, storm (100k IRQs, queue depth stays 1),
unclaimed-line safety, spurious IRQ 7/15, shared-line rounds and deadlines.
Boot line: `DEV:IRQ:PASS`.

**Stage D3 — Userspace access syscall (23) (done, #240).** `claim`,
`map_bar`, `pio`, `cfg_*`, `irq_*`, `release`, ACL interface `os.kernel.dev`
and per-class ids, audit records, `DeviceClaims` quota, teardown hook. Tests:
every op with hostile input (out-of-range BAR/offset, foreign handle, stale
generation, no right, no CAP), teardown-while-mapped, driver-crash-then-reclaim
soak (spawn/kill 10k times), audit chain still verifies. Boot line:
`DEV:SYSCALL:PASS`.

**Stage D4 — DMA (done, #241).** Boot-time contiguous DMA pool over the frame
refcount table, `dma_alloc` → Buffer handle + bus address, `DmaMemory` quota,
bus-master off before frame reuse at teardown, `DEV:DMA:PASS` boot line. Tests
(`dev_suite::DMA`, `dev_suite::DMA_STRESS`): `dev_dma_layout_and_zeroing`,
`dev_dma_alignment`, `dev_dma_hostile_input`, `dev_dma_fragmentation`,
`dev_dma_quota`, `dev_dma_lifetime_transfer`, `dev_dma_share_only`,
`dev_dma_teardown_releases_all`, `dev_dma_busmaster_ordering`,
`dev_stress_dma_pool_alloc_free_soak`, `dev_stress_dma_spawn_kill_soak`.

**Stage D5 — virtio transport + first NIC driver.** *Transport landed with D6
(`libs/virtio`); the NIC interface, frame ring and wire definitions landed as
networking stage N0 ([`architecture/networking.md`](architecture/networking.md));
the NIC driver (`netdrv`, stage N1, `user/src/bin/netdrv.rs`) and the `_net`
uid and `init` manifest row landed with N1; `devd` is still open.* Modern virtio-PCI library,
`virtio-net` userspace driver, `devd`, driver manifest, `_net` uid, init
manifest row, `os.lazy.net.nic.v1` served. Demo: `nicctl` tool prints MAC and
link; frame TX/RX loopback test against `filter-dump`. Boot evidence
`NET:NIC:PASS`, plus an ABI-bench-style QEMU check in CI.

**Stage D6 — Sound driver (done).** `virtio-snd` userspace driver on the same
transport (`user/src/bin/sndd.rs`), `os.lazy.audio.v1`, `_snd` uid (901, only
`CAP_DEV_CLAIM`, under `init`), the `beep` client, and a WAV-based assertion:
`python tools/sound/run.py` boots QEMU with `-audiodev wav` and requires both the
driver's own 440 Hz tone and `beep`'s 880 Hz tone, in order, in the recording.
Boot evidence `SND:PLAY:PASS` and `BEEP:PLAY:PASS`, plus `BEEP:PROBE:PASS` (28
hostile-input checks and an intruder task), `BEEP:SOAK:PASS` (40 stream
lifecycles), and `SND:IRQ:PASS` (the driver arms its INTx line and takes real
interrupts, with polling as the fallback). Details, the DMA-lifetime lesson and
what is not done:
[`architecture/audio.md`](architecture/audio.md).

**Stage D7 — Genericity proof and hardening.** e1000 and intel-hda (or AC97)
drivers built with *no* new syscall ops; if one is needed, the core is fixed
and the plan revised. Then: modern-virtio block on the transport, fuzz the
`dev_*` syscall, decide on IOMMU (VT-d) and MSI/IOAPIC follow-ups.

## 6. Testing summary

| Layer | How |
|---|---|
| Kernel unit | `dev_suite`: table, claim, resource bounds, IRQ dispatch, DMA allocator, teardown |
| Kernel stress | claim/release, IRQ storm, spawn/kill driver, DMA alloc/free generations |
| Security | ACL deny/allow, missing cap, cross-uid handle use, audit denial records, no uid-0 path |
| Integration | QEMU boot with net/sound flags; pcap and WAV assertions; driver crash restart via `init` |
| Visual | not applicable except a status line in the desktop later |

## 7. Risks and open questions

1. **INTx routing / interrupt-line value** — verified in D2 on `pc` and `q35`
   (`architecture/devices.md`); polling remains the fallback for unroutable lines.
2. **Contiguous DMA memory fragmentation** — reserve a DMA pool at boot
   (fixed size) rather than searching the general pool.
3. **Userspace latency for audio** — use large periods and ring positions
   kept in shared memory; measure before adding kernel help.
4. **Trusted DMA drivers** (D5) — accepted for now, documented, IOMMU stage.
5. **Function-level reset support** varies; fall back to command-register
   disable plus virtio status reset.
6. **Decided: block moves onto the device core.** Block drivers stay
   in-kernel (FS needs them before init) but are ported onto the shared
   `dev` core and the modern virtio transport (D1 and D7).
7. **Decided: `devd` stays separate from `init`** for least privilege; only
   `init` spawns drivers, `devd` only matches and publishes.

## 8. Explicit non-goals

Hot-load/unload modules, ACPI namespace/power management, USB (beyond HID:
[usb-hid-plan.md](usb-hid-plan.md) adds `usbd`, an xHCI driver for keyboards,
mice and tablets, under the `os.kernel.dev.usb` class; mass storage, hubs and
everything else on the bus stay out), GPU, SMP interrupt routing, MSI-X,
IOMMU implementation, any protocol above the NIC link layer, audio
mixing/resampling, and Linux ABI device nodes (`/dev/*`, ioctl) — the Linux
ABI can later front these class interfaces.
