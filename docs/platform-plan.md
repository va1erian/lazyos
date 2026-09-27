# LazyOS Platform Plan — a Messenger-native multiuser OS

**One line:** LazyOS is a small x86_64 kernel whose system fabric is
**Messenger** — a kernel-mediated, capability-based RPC and pub/sub layer — on
top of which multiuser services, a GPU-less windowed GUI (XUI on tiny-skia), a
classic desktop shell, and local-first networking are built, with
**friendliness, transparency, and security** as first-class properties.

This document is the master plan. The fabric is specified separately in
[`messenger.md`](messenger.md); the trust and permission rules in
[`security-model.md`](security-model.md). Existing focused plans remain valid:
[`linux-abi-plan.md`](linux-abi-plan.md), [`rust-std.md`](rust-std.md),
[`xui-plan.md`](xui-plan.md), [`dyon-feasibility.md`](dyon-feasibility.md).

---

## 1. Principles

1. **Capability-first.** No ambient authority. Code can only reach a resource if
   it holds a handle for it (a file, a service object, a topic, a device).
2. **Async-native, sync as sugar.** One execution model underneath (queues,
   transactions, topics); a blocking `call()` is a convenience wrapper.
3. **Observable by construction.** Every service exposes introspection, metrics,
   and health *through Messenger itself*; there is no "private" side channel.
4. **Secure by default.** Default-deny sandbox profiles, kernel-stamped identity,
   audited mediation, secrets never in the kernel.
5. **Friendly failures.** Errors carry a code, a human explanation, a hint, and a
   docs link. The OS explains *why* something was denied and *how* to proceed.
6. **Small kernel, rich services.** Keep kernel policy minimal (mechanism), put
   policy in userspace services that can be upgraded and introspected.
7. **Multiuser from the ground up.** Every object has an owner; every session is
   a user session; services can be per-user or system-wide.
8. **Server-grade reliability.** Supervision, restart, quotas, logging, crash
   dumps, and clean shutdown are product features, not afterthoughts.

---

## 2. Where we are today (honest baseline)

| Area | Today | Gap to target |
|---|---|---|
| CPU/arch | x86_64, single CPU, no SMP | SMP, per-CPU scheduling |
| Tasks | RR scheduler, 16 slots, kernel stacks, `clone`/`fork`/`execve`/`wait4` | priorities, real wait queues, signals, process groups/trees |
| Memory | bump frame allocator (no free), eager mapping, COW fork without refcounts | frame reclaim, refcounts, VMA list, demand paging, page cache, quotas |
| IPC | none beyond `futex` and `clone` | Messenger core (handles, parcels, sync + pub/sub) |
| FS | read-only FAT12/16, root-only, whole-file-at-open | writable FS (ext2), VFS, permissions, dirs/symlinks, page cache |
| Users | none (all tasks are root) | accounts, login, sessions, permissions, audit |
| GUI | kernel mux, 2 fixed windows, tiny-skia-like raster in kernel | userspace compositor, surfaces/shared buffers, XUI toolkit, multi-window |
| Shell | native `sh` + BusyBox on the Linux shim | graphical shell, apps, clipboard, drag&drop |
| Unix/ABI | Linux syscall shim (busybox `sh` works) | sockets/pipes/signals completion, keep as compatibility layer |
| Net | none | loopback, virtio-net, IPv4/UDP/TCP, DNS |
| Observability | serial logs, ABI matrix/coverage | introspection, metrics, tracing, health, event log |
| Security | none | capabilities, sandbox, audit, secrets service |

The Linux ABI work (matrix 6/6 + BusyBox `sh`) is the **compatibility bridge**:
prebuilt binaries run, while Messenger becomes the *native* architecture.

---

## 3. Target architecture

```
+----------------------------------------------------------------------+
|  Shell & apps:  LazyShell, Files, Editor, Terminal, Paint, Settings, |
|                 TaskMgr, Help, Package installer                     |
+----------------------------------------------------------------------+
|  Toolkits:  XUI (retained widgets)  |  tiny-skia (software canvas)   |
|             libmessenger (sync + async), lazyos-std helper crates    |
+----------------------------------------------------------------------+
|  System services (userspace, over Messenger):                        |
|   init/supervisor   messengerd (registry+pubsub+policy)   accounts   |
|   logind (sessions) auditd   healthd   keyd (secrets/crypto)         |
|   netd (socket API, DNS, TCP)   xuid (compositor/session)   fsd/VFS  |
+----------------------------------------------------------------------+
|  Kernel (small, mechanism only):                                     |
|   mm (VMA, refcounts, demand paging)   sched (priorities, SMP)       |
|   Messenger core (handles, channels, parcels, topics, creds, ACL     |
|     hooks, deadlines, shared buffers)                                |
|   VFS core (mounts, inode cache, perm checks)   net core (loopback)  |
|   drivers: ATA/AHCI, virtio (blk/net/gpu-ish), PS/2, serial, PIT/TSC |
|   audit ring, syscall gate + per-process policy table, credentials   |
+----------------------------------------------------------------------+
|  Hardware                                                            |
+----------------------------------------------------------------------+
```

Two ABI surfaces coexist:

- **Native ABI (preferred):** Messenger + a small syscall set (memory, threads,
  device grants, timers). The GUI, shell, and all new apps use this.
- **Compatibility ABI:** the Linux `syscall` shim, kept for running existing
  static binaries (`busybox`, Rust `std` programs). It is a *guest* environment:
  its syscalls can be policy-filtered like everything else.

---

## 4. Pillars

### 4.1 Messenger (the fabric)

Full spec in [`messenger.md`](messenger.md). Summary:

- **Objects & interfaces.** A service publishes typed objects; an interface is a
  versioned set of methods identified by a hash of its reverse-DNS name.
- **Handles.** Unforgeable, transferable references with rights (`CALL`,
  `DUPLICATE`, `TRANSFER`, `MONITOR`). Holding a handle is the capability.
- **Sync path.** `call(handle, method, parcel) -> reply` as a transaction with a
  deadline; nested calls and reentrancy defined; cancellation supported.
- **Async path.** One-way calls plus **topics**: hierarchical names, wildcards,
  QoS (latest / buffered N / drop-oldest / reliable-with-ack), retained values,
  filters. This is the pub/sub half.
- **Kernel stamps identity** (pid/tid/uid/gid/session/label) on every message and
  enforces ACLs; a userspace daemon (`messengerd`) owns naming, fanout, policy
  hot-reload, and introspection.
- **Observability built in:** `ListServices`, `GetInterface`, `ListSubscribers`,
  `Stats`, transaction tracing, health heartbeats.

### 4.2 Kernel foundations

- **Memory:** refcounted frames, free lists, per-process VMA list, demand-zero
  and file-backed paging, page cache, COW with refcounts, OOM policy, quotas.
- **Scheduler:** priorities + fair share (MLFQ or weighted round-robin), per-CPU
  run queues on SMP, wake-on-message (a blocked-on-Messenger task wakes without
  spinning), timer wheel.
- **Processes:** process tree, process groups, sessions, POSIX-ish signals with
  handlers (needed for Linux ABI and for Ctrl-C in the desktop), general wait
  queues.
- **Driver model:** device enumeration (ACPI/PCI), IPC to userspace drivers,
  DMA-safe shared buffers, interrupt → event delivery through Messenger.
- **SMP:** AP bring-up, spinlocks per subsystem, IPIs; designed now, enabled
  later.

### 4.3 Users, sessions, services

- **Accounts service:** users, groups, home directories, password verifiers
  (Argon2 via `keyd`), policies. Kernel only knows numeric UID/GID in credentials.
- **Sessions:** `logind` creates a session per login (console now, remote later),
  owns the session bus namespace and default service ACLs; the compositor and
  desktop shell run inside a session.
- **Supervision:** `init` starts system services, restarts crashes with backoff,
  tracks dependencies, exposes status/health; socket/service activation
  (start-on-first-call) via `messengerd`.
- **Service accounts:** least-privilege UIDs for daemons; no root by default.

### 4.4 Storage

- **VFS core:** mounts, inodes, dentry cache, permissions, symlinks, `statfs`.
- **First writable FS:** ext2 (well-specified, simple, proven) read/write, then a
  journal/ordered mode by wrapping writes; later evaluate a log-structured native
  FS with snapshots.
- **Persistent device:** virtio-blk and AHCI/NVMe drivers; the boot FAT stays as
  the recovery/EFI volume.
- **Home directories and service state** live on the writable volume; the FAT
  volume remains read-only for shipped artifacts.

### 4.5 GUI stack

- **Userspace compositor (`xuid`)** owns the framebuffer (granted by the kernel
  as a device capability), composites surfaces, decorates windows, routes input,
  and implements the display protocol over Messenger.
- **Surfaces & zero-copy:** apps render into shared buffers negotiated through
  Messenger; the compositor reads them without copying. Damage regions + fences
  keep software rendering fast at 1080p.
- **XUI userspace toolkit:** the existing xui plan, moved out of the kernel:
  `tiny-skia` painter core + widget tree + theming; a LazyOS backend presents via
  the compositor protocol (no winit/GL).
- **Inter-app interaction:** all implemented as Messenger services/protocols:
  - **Clipboard service:** selection ownership, MIME-typed data offers, lazy
    transfer (like X11/Wayland data sources), history policy, per-session scope.
  - **Drag & drop:** compositor-mediated drag protocol; source offers typed data,
    target accepts; permission prompts for cross-app transfers.
  - **Shell integration:** MIME database + "open with" registry; file manager and
    terminal publish typed selections; global actions ("Open in Editor") are
    registered topics/verbs, discoverable and observable.
- **Multi-session:** one compositor per session; local console sessions first;
  remote display later via the same protocol over the network transport.

### 4.6 Desktop shell & apps (classic experience)

- **LazyShell:** desktop, icons, taskbar (start button, task list, clock, tray),
  start menu, run dialog, window controls, Alt-Tab, context menus, keyboard
  navigation, theming with a **Win95 theme** as the flagship look.
- **Core apps:** File Manager, Text Editor, Terminal (native shell + BusyBox),
  Paint (tiny-skia), Calculator, Settings/Control Panel, Task Manager
  (processes, CPU/memory, Messenger traffic, service health), Help viewer,
  Package/Install manager for signed bundles.
- **Shell scripts:** `sh` continues to work (native and BusyBox); a `lazyosctl`
  CLI exposes the same introspection as the GUI.

### 4.7 Networking (local-first)

- **Stage 1 — local:** loopback device + an in-kernel socket core; local
  `AF_UNIX`-equivalent is *Messenger itself*, so local IPC never needs sockets.
- **Stage 2 — LAN:** virtio-net, Ethernet/ARP/IPv4/ICMP/UDP/TCP, DHCP client, DNS
  resolver service (`netd`), firewall hooks per sandbox profile.
- **Stage 3 — services:** HTTP/SSH-like daemons as supervised services; TLS
  terminated via `keyd` so private keys never leave the secrets service.
- **Stage 4 — Messenger over network:** authenticated framing of partitions
  (mTLS or keyd-issued tokens) to federate pub/sub across hosts; the same
  introspection and policy model applies remotely.

### 4.8 Security (summary; see [`security-model.md`](security-model.md))

Identity in credentials, UNIX permissions on VFS, capability rights on handles,
**default-deny sandbox profiles** per app (allowed syscalls, Messenger
interfaces, topics, filesystem scopes, network), per-call policy checks at the
kernel Messenger boundary, audit ring + `auditd`, secrets in `keyd`, signed
service bundles, and friendly denials that explain themselves.

### 4.9 Transparency & developer experience

- `lazyosctl`: system state, services, mounts, sessions, health.
- `messengerctl`: list services/topics/methods, live stats, trace a call, tail a
  topic, inspect policy decisions.
- **Event log** (`logd`): structured records (boot, services, denials, logins),
  queryable and hash-chained.
- **Task Manager** surfaces the same data graphically.
- `explain <error>` / `doctor`: diagnostics with fixes.
- Docs are generated from IDL, so the published API surface cannot drift.

---

## 5. Stages (roadmap)

Each stage is independently valuable and ends with a demo + benchmark recorded in
CI. Sizes are rough (S/M/L/XL).

### S0 — Kernel foundations (L)
**Goal:** a base a server can be built on.
**Deliverables:** refcounted frames + free; VMA list; demand-zero paging; COW
refcounts; generalized wait queues; process tree; signals (basic); priorities;
slab allocator; kernel unit-test harness.
**Acceptance:** a soak test allocates/frees millions of frames with bounded
memory; `fork`/COW across many generations does not leak; `kill` terminates a
process tree; scheduler priorities observable.
**Depends on:** nothing. **Risk:** MM rework touching everything — land behind
tests incrementally.

### S1 — Messenger core (XL)
**Goal:** the sync + duplicate-the-handle kernel fabric.
**Deliverables:** handle table + rights; channels; parcels; transactions with
deadlines; one-way; shared buffers; kernel credential stamping; ACL hook points;
audit events; `messenger` syscall family; bootstrap channel from `init`.
**Acceptance:** echo service ping-pong with <X us median latency; handle rights
enforced; forged-credential attempt fails; parcel parser fuzzed in CI.
**Depends on:** S0. **Risk:** kernel attack surface — see `messenger.md` section
on validation.

### S2 — System services, registry, pub/sub (L)
**Goal:** usable fabric.
**Deliverables:** `messengerd` (namespace, activation, policy reload, fanout,
retained topics); `init` supervisor; `logd`; `healthd`; IDL compiler (`midlc`)
with Rust codegen + docs; `libmessenger` (blocking + async/futures);
`messengerctl`.
**Acceptance:** services start on demand; pub/sub fanout with QoS; generated docs
match interfaces; `messengerctl trace` shows a live transaction.
**Depends on:** S1.

### S3 — Users, sessions, writable storage (XL)
**Goal:** "real server" baseline.
**Deliverables:** ext2 read/write + VFS + permissions; virtio-blk/AHCI; accounts
(users/groups/home); `logind`; `keyd` (Argon2, crypto primitives); elevation
service; per-user service namespaces; quotas (disk/CPU/mem/fds).
**Acceptance:** create users, log in on the console, own files with modes, run a
service as a service account, deny cross-user access, `df`/`du`/`quota` work.
**Depends on:** S0–S2.

### S4 — GUI stack (L)
**Goal:** windows and inter-app interaction.
**Deliverables:** device-granted framebuffer; `xuid` compositor + display
protocol; shared-buffer surfaces; XUI userspace toolkit on tiny-skia; input
routing; clipboard service; drag&drop; MIME/open-with registry.
**Acceptance:** two XUI apps in real windows; copy/paste between them;
drag a file from the shell onto the editor; 60 fps composite at 1080p for simple
scenes (software).
**Depends on:** S2 (IPC), S0 (shared memory).

### S5 — Desktop shell & apps (L)
**Goal:** the classic experience.
**Deliverables:** LazyShell (desktop/taskbar/start/menus/Win95 theme), Files,
Editor, Terminal, Paint, Settings, Task Manager, Help; install manager for
signed bundles; accessibility basics (keyboard nav, scaling).
**Acceptance:** a user logs in and, using only the GUI, edits a file, copies it
between apps, browses the filesystem, and inspects running services.
**Depends on:** S3–S4.

### S6 — Networking (XL)
**Goal:** local-first connectivity, then LAN.
**Deliverables:** socket core + loopback; virtio-net; IPv4/UDP/TCP; DHCP; DNS
service; firewall per sandbox; HTTP/SSH-like daemons; TLS via `keyd`; remote
Messenger transport.
**Acceptance:** `ping` loopback/LAN; a TCP echo service reachable; TLS handshake
with keys confined to `keyd`; pub/sub federated between two QEMU guests.
**Depends on:** S2–S3.

### S7 — Security hardening & sandboxing (L)
**Goal:** turn on the model and prove it.
**Deliverables:** policy language + compiler; app manifests + install consent;
syscall allowlists; Messenger interface/topic policy; filesystem jails; signed
bundles; audit viewer; red-team suite in CI.
**Acceptance:** a malicious sample cannot read another user's files, cannot call
ungranted interfaces, cannot open sockets; every denial is audited and explained.
**Depends on:** S1–S6.

### S8 — SMP & performance (XL)
**Goal:** scale and responsiveness.
**Deliverables:** AP bring-up; per-CPU scheduling; fine-grained locks; profiling
(TSC counters per subsystem); zero-copy everywhere; power management basics.
**Acceptance:** linear speedup on N vCPUs for a parallel benchmark; interactive
latency under load stays bounded.
**Depends on:** S0–S6. **Risk:** high; keep single-CPU path correct first.

### S9 — Release engineering & observability (M)
**Goal:** trustworthy releases.
**Deliverables:** crash dumps + symbol service; structured boot log; release
images (raw/ISO) with checksums; CI matrix (boot, desktop smoke, ABI bench,
security suite, benchmarks); docs portal generated from IDL + design docs.
**Acceptance:** one-command boot of a release image; CI publishes all artifacts
and the compatibility matrix; `doctor` explains common failures.

---

## 6. Testing & CI strategy

| Layer | Method |
|---|---|
| Kernel | unit tests (frame math, VMA, parcel codec), soak tests, fault injection |
| Messenger | protocol conformance suite; fuzz the parcel parser; deadline/cancel tests; a mock network transport |
| Syscalls | the existing Linux ABI bench (keep green); native ABI tests |
| GUI | headless screenshot tests (existing pipeline) + scripted input sessions; golden images |
| Security | policy unit tests + red-team samples; deny-path assertions in CI |
| Perf | micro-benchmarks (IPC latency/throughput, fork, composite fps) with regression gates |

All of it runs headless in QEMU today; the same `qemu_shot`/`qemu_session`
tooling is reused for desktop sessions.

---

## 7. Compatibility strategy

- **Native first:** new apps target Messenger + the native ABI.
- **Linux ABI preserved:** the shim stays; it is the porting path for existing
  binaries and the current `std` support. Sandbox policies apply to both ABIs.
- **No dynamic linking initially** (static binaries), revisited later.
- **Stable Messenger contract:** interfaces are versioned and append-only; the
  IDL hash and generated docs are the compatibility reference.

---

## 8. Non-goals (for now)

- Dynamic linking / `ld.so`; full POSIX conformance.
- GPU acceleration (software rendering first; a GPU service can come later).
- Distributed consensus; network federation is best-effort initially.
- Real-time guarantees (priority classes only).
- Portability beyond x86_64.
- Binary compatibility with Windows/macOS apps.

---

## 9. Top risks

| Risk | Impact | Mitigation |
|---|---|---|
| Scope explosion across 6 pillars | never ships | stage gates; each stage demoed in CI before the next |
| Kernel IPC becomes the attack surface | security | minimal kernel policy, fuzzing, capability rights, audit; keep policy in userspace |
| MM rework destabilizes the ABI work | regressions | keep the ABI bench as a hard CI gate throughout |
| Software compositing too slow | UX | damage regions, shared buffers, tight/simple themes, measure early |
| Multiuser semantics bolt-on | rework | credentials/ownership designed into S0/S1 data structures now |
| Dual ABI drift | confusion | IDL is the single source of truth; Linux shim is explicitly a guest |

---

## 10. Success metrics

**Server-usable:** a headless image boots, starts services, creates users, logs
in over the console, runs a long-lived daemon as a service account, serves HTTP
over the LAN, and survives a service crash with automatic restart — all
introspectable via `lazyosctl`/`messengerctl`.

**Desktop-usable:** a user logs in graphically, runs two XUI apps side by side,
copies between them, drags a file from the shell into the editor, and sees live
health/telemetry in Task Manager — on software rendering, at interactive latency.

**Fabric health:** every Messenger call is attributable (who called what, when,
allowed/denied), and the full interface surface is enumerable and documented.

---

## 11. Immediate next steps (first PRs)

1. **Plan adopted:** milestone **LazyOS Platform**, epic **#52**, stage **S0
   #53** (tasks #54–#62), stage **S1 #63** (tasks #64–#70).
2. **S1 spike (thin vertical slice):** `msg_call` echo between two user tasks —
   handles (#64), parcels (#65), transactions (#66), syscalls + bootstrap +
   `libmessenger` echo (#69). Proves the kernel surface on the existing ABI.
3. **S0 groundwork in parallel:** refcounted frames + free (**#54**) unblocks COW
   correctness and quotas. Soak/unit-test harness is **#62**.
4. **Keep the Linux ABI bench green** as the regression gate for every stage.

---

*See also:* [Messenger specification](messenger.md) ·
[Security model](security-model.md) · [Linux ABI plan](linux-abi-plan.md) ·
[Xui plan](xui-plan.md)
