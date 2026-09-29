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
  `CAP_DEV_*` is reserved but not implemented.
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

The kernel ISR masks the line, sets a pending flag, and posts a one-way
message **from the kernel identity** to the endpoint the driver registered.
The driver receives it in the same `Selector`/`recv` loop as its client
requests and calls `irq_ack` after servicing (which unmasks). At most one
message is outstanding per IRQ, so an interrupt storm cannot fill a queue or
burn the driver's `QueueDepth` quota. This is the "interrupt → event delivery
through Messenger" line from the platform plan.

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
   ═══════════ syscall 15: dev_* ═══════════════════════════════════════
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

### 3.2 Userspace surface: syscall 15 `dev_*`

Same multiplexed style as syscalls 5/12/14 (op in `rdi`).

| Op | Effect | Checks |
|---|---|---|
| `list(buf)` | copy `DeviceInfo` rows the caller may see | `CAP_DEV_CLAIM` (devd) |
| `claim(id, irq_endpoint)` | returns a `Device` handle; sets owner | `authorize(actor, "os.kernel.dev", claim)`, device unowned, quota |
| `map_bar(dev, bar)` | maps MMIO uncached into caller | handle right `MMIO`, mapping charged to `UserMemory` |
| `pio(dev, bar, off, width, val?)` | port in/out inside the device's I/O BAR only | handle right `PIO`, offset < len |
| `cfg_read/cfg_write(dev, off)` | PCI config, write masked (no BAR/bus-master bits from userspace directly) | right `CONFIG` |
| `irq_enable(dev, n)` / `irq_ack(dev, n)` | arm / unmask | right `IRQ` |
| `dma_alloc(dev, len)` | physically contiguous frames as a **Buffer handle** + bus address | right `DMA`, quota `DmaMemory` |
| `release(dev)` | quiesce + free | owner |

Everything is bounds-checked against the device's own resources; no op takes a
raw physical or port address from userspace (no ambient authority).

### 3.3 Interrupt path

- Replace fixed handlers with **vector stubs 32–47** that call
  `dev::irq::dispatch(line)`: if a device claim owns the line → mask, mark
  pending, post the kernel→driver one-way message; else existing handlers
  (timer/keyboard/mouse) run unchanged.
- Level-triggered INTx may be shared: the dispatch masks the line until *all*
  owners have acked; sharing is allowed only among the claimants that opt in.
- Kernel drivers register a plain `fn(line)` instead of a message.
- APIC/IOAPIC and MSI are out of scope; the `Irq` resource kind and the
  dispatch table are the seam. **Risk**: on `q35` the PCI *Interrupt Line*
  register may not be pre-programmed to a PIC-routable IRQ. Step 2 verifies
  this; the fallback is a polling mode (`irq_enable` returns `ENOSYS`, driver
  uses a timer tick), which is also the CI-safe default.

### 3.4 DMA buffers

`dma_alloc` returns a `HandleKind::Buffer` (existing shared-buffer object) with
two additions: frames are allocated **physically contiguous** (needs a small
contiguous-run allocator over the frame pool; bounded, charged to a new
`DmaMemory` quota, default 8 MiB per uid) and the call returns the **bus
address** so the driver can program descriptors. Because it is a normal Buffer
handle it can be **passed to a client in a Messenger message** for zero-copy
audio/packet payloads, and `SHARE_ONLY` lets a client hand a buffer to the
driver without mapping it. Drivers should keep descriptor rings driver-owned
and copy/validate client data into them; the driver never trusts client
lengths.

### 3.5 Security integration

- **ACL**: new interface `os.kernel.dev` with methods `list`, `claim`,
  `map`, `dma`. `claim` first resolves the device and its class, then calls
  `authorize` with a **class-specific `interface_id`** (`os.kernel.dev.<class>`,
  e.g. `os.kernel.dev.net` for PCI class 0x02), and only assigns ownership if
  that verdict allows. A generic "may claim" rule therefore cannot authorize
  claiming a class the policy did not name: "label `net-driver` may claim
  class net" says nothing about audio or storage. Default deny once policy is
  loaded; the bootstrap window is allow, as elsewhere (unchanged).
- **Credentials**: drivers run as dedicated system uids (`_net`, `_snd`) with
  only `CAP_DEV_CLAIM` (+ the per-class ACL rule), launched by init via
  `spawn_as`. They can never `CAP_SETUID` or reach uid 0.
- **Audit**: every claim/release/denial is a record with device id, class and
  reason code, in the existing hash-chained ring.
- **Quotas**: add `Resource::DmaMemory` and `Resource::DeviceClaims` to
  `quota.rs`.
- **Teardown**: `ipc::teardown_task` gains "release all claims": mask IRQs,
  clear PCI bus-master and memory/IO enable, function-level-reset when
  supported, unmap MMIO, free DMA frames, clear `owner`, bump generation,
  publish `system/events/device/<id>` so `devd` can respawn a driver.
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

### 3.7 Class interfaces (Messenger IDL, `docs/idl/`)

Kept minimal and versioned; each is a *control* interface plus a shared-buffer
data plane.

**`os.lazy.net.nic.v1`** (link layer only — no IP):
`Info() → {mac, mtu, link, features}`, `SetRxMode(mode)`,
`AttachRing(rx_buf, tx_buf, notify_topic)` — two single-producer/
single-consumer frame rings in shared buffers with fences, `Stats()`. Link
change published on `system/net/<nic>/link`. A future stack service is just
another client of this interface (and can be the only holder of the ring
buffers).

**`os.lazy.audio.v1`**:
`Info() → {streams, formats, rates, channels}`,
`OpenStream(dir, format, rate, channels, period_bytes) → {stream, buffer}`,
`Start/Stop/Drain(stream)`, `Position(stream)`; playback/capture data in a
shared ring buffer with a position fence; underrun/xrun on
`system/audio/<card>/event`. Mixing and per-app volume are a later `audiod`
service, not the driver's job.

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

**Stage D2 — Interrupts.** Vector stubs and dispatch table, mask/ack, shared
INTx, kernel→driver one-way message with coalescing. Verify q35 interrupt-line
routing (or land the polling fallback). Tests: raise/ack ordering, storm
coalescing (100k IRQs, queue depth stays 1), unclaimed-line safety, spurious
IRQ 7/15 handling.

**Stage D3 — Userspace access syscall (15).** `claim`, `map_bar`, `pio`,
`cfg_*`, `irq_*`, `release`, ACL interface `os.kernel.dev`, audit records,
`DeviceClaims` quota, teardown hook. Tests: every op with hostile input
(out-of-range BAR/offset, foreign handle, stale generation, no right, no
CAP), teardown-while-mapped, driver-crash-then-reclaim soak (spawn/kill 10k
times), audit chain still verifies.

**Stage D4 — DMA.** Contiguous-run allocator, `dma_alloc` → Buffer handle +
bus address, `DmaMemory` quota, bus-master off at teardown. Tests:
alignment, fragmentation refusal, quota exhaustion, frames returned after
crash, 1M alloc/free generations.

**Stage D5 — virtio transport + first NIC driver.** Modern virtio-PCI library,
`virtio-net` userspace driver, `devd`, driver manifest, `_net` uid, init
manifest row, `os.lazy.net.nic.v1` served. Demo: `nicctl` tool prints MAC and
link; frame TX/RX loopback test against `filter-dump`. Boot evidence
`NET:NIC:PASS`, plus an ABI-bench-style QEMU check in CI.

**Stage D6 — Sound driver.** `virtio-snd` userspace driver on the same
transport, `os.lazy.audio.v1`, `_snd` uid, `beep`/`play` tool, WAV-based CI
assertion. Boot evidence `SND:PLAY:PASS`.

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

1. **q35 INTx routing / interrupt-line value** — verify in D2, else polling.
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

Hot-load/unload modules, ACPI namespace/power management, USB, GPU, SMP
interrupt routing, MSI-X, IOMMU implementation, any protocol above the NIC
link layer, audio mixing/resampling, and Linux ABI device nodes (`/dev/*`,
ioctl) — the Linux ABI can later front these class interfaces.
