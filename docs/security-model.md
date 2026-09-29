# LazyOS security model

LazyOS aims to be **friendly, transparent, and secure**. This document defines
how identity, permissions, sandboxing, secrets, and audit work. It complements
the [platform plan](platform-plan.md) and the [Messenger spec](messenger.md).

The one-sentence model: **no ambient authority; every privileged action is a
capability or a policy-checked Messenger call, performed by an authenticated
identity, and explained if denied.**

## 0. Implementation status

This document is the target model. As of 2026-09-28 the kernel and services
implement the following; everything else below is specification
([`architecture/ipc-security.md`](architecture/ipc-security.md) has the detail):

| Implemented | Specified only |
|---|---|
| Kernel-stamped credentials (`uid/gid/caps/label/session`) on every task and message; children inherit, only kernel-started programs are root; audited `CAP_SETUID` transitions that can never widen privilege (section 2) | Service accounts: every system service still runs as uid 0 (section 4.1 "system services do not run as root" is the goal, not the state) |
| Console login through `logind` + `accountsd`, Argon2id verification inside `keyd`, hashes in `SHARE_ONLY` buffers; failed logins audited (section 3) | Rate limiting, key/2FA, per-user sealing of secrets, TLS in `keyd`; `keyd` key ids are not yet scoped to their owner (issue #187) |
| VFS `rwx`/`umask`/sticky checks against kernel credentials, root bypass (4.1) | POSIX ACLs, mount namespaces / filesystem jails (5.3) |
| Capability bits `CAP_NET_*`, `CAP_SYS_ADMIN`, `CAP_SYS_TIME`, `CAP_AUDIT_READ`, `CAP_IPC_CONTROL`, `CAP_SETUID`, `CAP_KILL` (cross-uid signals); `CAP_SYS_ADMIN` gates the display grant (4.2) | `CAP_DEV_CLAIM` (the device-claim gate; D0/D1 documented in 4.2), dropping capabilities on `execve` |
| Handles with rights as the primary Messenger right; default-deny ordered ACL at the kernel call boundary; per-segment topic policy (4.3) | The policy language/compiler, profiles from manifests, hot reload, revocation of live handles (5.2, 6) |
| Per-uid quotas on kernel memory, user memory, handles, queue bytes/depth (5.5); friendly `ERR_QUOTA` | Syscall allowlists (5.1), network policy (5.4), fd/CPU quota enforcement |
| Validated user pointers on every native and Linux syscall, NX on user pages, length-checked parcels fuzzed in CI, every `unsafe` documented and gated by clippy (7) | W^X enforcement, SMEP/SMAP, stack canaries, signed kernel, crash dumps, watchdog |
| 128-entry hash-chained kernel audit ring, denials always recorded (9) | `auditd`, on-disk log, `CAP_AUDIT_READ` query interface, "why was this denied" UI |
| | Elevation service (10), signed bundles and updates (11), consent UX (12), the red-team CI suite (13) |

---

## 1. Goals and threat model

**In scope**

- Malicious or buggy *apps* run by a user (the common case).
- A compromised *service* (daemon) must not gain more than its profile allows.
- Cross-user isolation on a multiuser machine.
- Network-facing daemons (later).
- Physical/local attackers (console).

**Out of scope (initially)**

- Nation-state hardware attacks; DMA attacks (mitigated later by IOMMU).
- Side channels (Spectre-class); we still apply standard mitigations.
- Supply chain of upstream toolchains (we sign our own artifacts).

**Adversary capabilities we assume:** the attacker can run arbitrary code as an
unprivileged user, craft arbitrary syscalls and Messenger parcels, exhaust
memory/fds/handles within their quota, and race multi-threaded code.

---

## 2. Identity

| Principal | Representation |
|---|---|
| User | `uid` (u32), unique; `0` reserved for the system |
| Group | `gid`; a user has a primary group + supplementary groups |
| Service account | a user with no login, owning service state |
| Session | `session_id`, created by `logind`; scopes topic/service namespaces and default ACLs |
| Process label | short string/profile id used in policy (e.g. `app:com.example.editor`) |
| Kernel | the only component that stamps credentials; userspace can never set them |

Credentials travel with every Messenger message and syscall and are copied into
audit records. The kernel keeps `uid/gid/caps/label/session` in the task struct;
`setuid`-like transitions are only allowed toward *less* privilege or through the
elevation service.

---

## 3. Authentication and login

- **Accounts** are managed by the accounts service (users, groups, home dirs,
  password verifiers). Password hashes are **Argon2id**, and only `keyd` can
  verify them; the hash never leaves `keyd`'s `SHARE_ONLY` memory.
- **Login** (`logind`) runs a small PAM-like pipeline: identify → authenticate
  (console password, later key/2FA) → create session → grant the session its
  default capabilities (own compositor, own clipboard, session topics, home dir).
- **Console sessions** start the compositor + desktop shell as the user.
- **Service accounts** never log in; they receive their profile at supervision
  time.
- Failed logins are rate-limited and audited (source, user, attempt).

---

## 4. Authorization

Three layers, all checked, all auditable:

### 4.1 Filesystem (UNIX-style)

- `rwx` bits for owner/group/other, `umask`, sticky-bit on shared dirs, plus
  optional POSIX ACLs later.
- Enforcement in the **VFS core** against kernel credentials; no path bypass.
- Root (`uid 0`) bypass exists only inside the *kernel-init* profile; system
  services do not run as root.

### 4.2 Capabilities

Fine-grained kernel privileges, granted per profile, never ambient:

| Capability | Allows |
|---|---|
| `CAP_NET_BIND` | bind ports < 1024 |
| `CAP_NET_RAW` | raw sockets |
| `CAP_SYS_ADMIN` | mounts, driver grants |
| `CAP_SYS_TIME` | set the clock |
| `CAP_AUDIT_READ` | read the audit stream |
| `CAP_IPC_CONTROL` | manage other services' endpoints |
| `CAP_DEV_CLAIM` | attempt to claim a discovered device at all |

`CAP_DEV_CLAIM` is deliberately **coarse**: it is the single gate that lets an
actor call the device core's `claim` in the first place. It is *not* a
per-device or per-class grant — authority over a specific device is the
`Device` handle returned by `claim`, whose rights (`MMIO`, `PIO`, `IRQ`, `DMA`,
`CONFIG`) are fixed by that device's actual resources and by a class-specific
ACL rule (`os.kernel.dev.<class>`), and can only be narrowed afterwards.
`claim` fails with `EPERM` before any owner is recorded when either the
capability or the class rule is absent, so an app without `CAP_DEV_CLAIM`
cannot even attempt to probe the device table. Drivers run as their own system
uids (`_net`, `_snd`) holding `CAP_DEV_CLAIM` plus their one class rule, never
uid 0. The gate is specified here; the kernel enforcement lands with the
`dev_*` syscall (driver-plan stage D3).

Capabilities are in the task struct, shown in `ps`, and dropped on `execve`
unless the profile is privileged.

### 4.3 Messenger (capability + policy)

- Holding a **handle** is the primary right: you can call an object only if you
  hold a handle for it with `CALL`.
- The **kernel ACL hook** additionally evaluates policy per call:
  `(sender label/uid/session, target owner, interface.method, rights)`.
- Names and topics are policy-controlled too: an app can `Resolve("os.lazy.fs")`
  only if its profile permits it, and can publish/subscribe only to granted topic
  segments (wildcards included).
- **Default deny**: a new app can do nothing until its manifest permissions are
  approved.

---

## 5. Sandbox model

Every process runs under a compiled **profile** combining:

1. **Syscall allowlist.** The native syscall gate checks a per-process bitmap.
   Linux-ABI processes get the same treatment for their syscall table. Denials
   return the friendly error (never a bare `EPERM`) and are audited.
2. **Messenger policy.** Interface/method allowlists, topic filters, handle-right
   restrictions, and permitted peers. Enforced at the kernel call boundary.
3. **Filesystem view (jail).** A mount namespace exposes only the app's home,
   declared data scopes, and read-only system paths. Path traversal cannot escape
   because the VFS resolves against the namespace.
4. **Network policy.** Local/LAN/internet egress rules, bind permissions, DNS
   restrictions; enforced by `netd` + kernel socket checks.
5. **Resource quotas.** Memory, CPU share, fds, handles, IPC queue depth,
   outstanding transactions, buffer allocations. Exceeding a quota is a friendly
   `ERR_QUOTA` with the current usage and limit.
6. **Device grants.** Framebuffer, input devices, audio later — only via handles
   issued by the session/device services.

The kernel side of item 5 is `kernel::quota` (issue #103): a per-uid table of
limits and live usage for kernel memory (shared-buffer frames), user memory
(`mmap`/`brk` growth), handles, fds, Messenger queue bytes/depth, and CPU ticks.
Charges and releases happen at the choke points (`handles::open`, channel
enqueue/dequeue, `shared::create`, `mmap`/`brk`), keyed by the task's stamped
uid, so two processes of one user share one limit. A refusal is a friendly
`ERR_QUOTA` carrying the resource name, current usage and limit. Defaults live
in `quota::DEFAULT_LIMITS` (uid 0 stays uncapped until login stamps a real
uid); policy sets limits with `quota::set_limit`, and native syscall 11 reads
the caller's usage/limits read-only.

Profiles are **compiled by `messengerd`/`init` from manifests + admin policy** and
hot-loaded into the kernel. Human-readable source of truth is checked into the
app bundle; the compiled form is hashed and audited.

---

## 6. Apps, manifests, and consent

An app bundle declares:

```toml
[app]
id = "com.example.editor"
version = "1.2.0"
publisher = "Example Inc"
[permissions]
files = ["read:/home/*/docs", "write:~/.config/editor"]
interfaces = ["os.lazy.fs.reader.v1:call", "os.lazy.clipboard.v1:read"]
topics = ["publish:session/*/editor", "subscribe:system/events/*"]
network = []
capabilities = []
```

- **Install consent** shows a human-readable diff of the requested permissions
  with plain-language explanations, grouped by risk, plus the publisher
  signature.
- **First-use prompts** for sensitive resources (camera, network, clipboard
  paste) can be enabled by policy; prompts are themselves Messenger calls to the
  consent service and always logged.
- **Revocation** is immediate: policy is hot-reloaded and existing handles that
  violate the new policy are marked for revocation.

---

## 7. Kernel hardening

- Rust `no_std` with no `unsafe` outside audited modules; all `unsafe` blocks
  documented with invariants.
- **W^X** for user pages; NX by default; SMEP/SMAP; stack canaries.
- Strict syscall validation: every pointer/length checked against the caller's
  VMAs; no kernel dereference of user addresses without a validated copy; all
  user copies go through checked helpers (the COW/MM rework supports this).
- Parcel parsing is length-checked and fuzzed; no recursion without a depth cap.
- Handle and buffer allocations are metered; no unbounded kernel allocation from
  userspace.
- Panic policy: kernel panics are logged with a backtrace and, by default, halt;
  a watchdog can reboot into recovery. Crash dumps are opt-in and symbolised.
- **Kernel integrity:** signed kernel image, measured boot chain (UEFI secure
  boot later), and a read-only kernel text section on x86_64 huge pages.
- **Least privilege in the kernel:** policy decisions use compact tables; no
  user-controlled code (no eBPF-like JIT) initially — filters are interpreted
  with hard limits.

---

## 8. Secrets and cryptography

- **`keyd`** owns all long-term secrets: password hashes, TLS private keys, disk
  encryption keys, signing keys.
- Clients never receive raw keys. They ask for operations: `Sign(key, digest)`,
  `Decrypt(handle, ciphertext)`, `Wrap/Unwrap`, `TLS server session`. Key material
  lives in `SHARE_ONLY` buffers that are not mapped into any client address space.
- **Unlock model:** secrets are sealed per user with a key derived at login
  (Argon2id from the password + machine secret); a locked account's keys are
  unavailable even to the kernel.
- **RNG:** a kernel entropy source (RDRAND/RDSEED + timing jitter, pooled) feeds
  a CSPRNG; userspace gets bytes only via `keyd`/`getrandom`.
- **Crypto policy:** algorithm allowlists, key sizes, no home-grown primitives
  (use vetted crates), constant-time where relevant.

---

## 9. Audit and forensics

- A kernel **audit ring** records: logins/logouts, elevation, service
  start/stop/crash, policy load/change, **all denials**, sensitive grants
  (clipboard, filesystem writes outside home, network listens), and optionally
  traces for tagged transactions.
- `auditd` drains the ring into a hash-chained log on disk (tamper-evident),
  enforces retention, and exposes a read-only query interface
  (`CAP_AUDIT_READ`).
- Every record has: timestamp (monotonic + realtime), actor (pid/uid/label/
  session), action, target, decision, and a correlation id (`txn_id` when
  applicable).
- The **Task Manager / `messengerctl`** can show "why was this denied" by
  following the correlation id.

---

## 10. Privilege elevation

- `sudo`-like elevation is a service call, not a setuid bit: the elevation
  service authenticates the user (password/policy), then grants a **time-boxed
  capability set** to a spawned child (never to the caller in place).
- Elevation always prompts (GUI or console), always audits, and can be policy-
  limited per command (`sudo lazyosctl service restart netd`).
- `CAP_SYS_ADMIN` is never granted to ordinary sessions; system administration
  happens through scoped control interfaces (`os.lazy.system.admin.v1`) rather
  than a superuser shell, with every call logged.

---

## 11. Updates and trust

- Apps and services are distributed as **signed bundles**; the installer verifies
  the publisher signature against system trust roots and records the hash.
- The manifest's permission diff is shown at install *and* update time
  (permission creep is visible).
- `init`/`messengerd` verify service signatures when required by policy (system
  services must be signed).
- Rollback: previous versions are retained; a failed update can be reverted with
  `lazyosctl rollback`.
- Revocation lists and trust-root updates are themselves signed and audited.

---

## 12. Friendly security (UX)

Security must not be a maze:

- **Explain every denial.** `ERR_DENIED` carries: what was attempted, which app,
  which permission was missing, how to grant it, and a docs link.
- **Diff-based consent.** "This app wants to read /home/alice/docs (new)" rather
  than a wall of scopes.
- **Least-surprise defaults.** New apps get no network and no filesystem access
  beyond their own data dir until approved.
- **Visible state.** Settings > Privacy shows active grants; Task Manager shows
  per-process capability and IPC activity; tray shows a shield when a sensitive
  grant is active (camera/mic later).
- **Reversible.** Revoke a grant and it takes effect immediately, with a clear
  list of what will break.

---

## 13. Verification plan

| Check | Where |
|---|---|
| Policy unit tests (allow/deny matrices) | CI |
| Syscall-gate deny tests per profile | CI |
| Messenger parcel/handle-transfer fuzzing | CI (nightly) |
| Red-team samples (read other user, escape jail, call ungranted interface, exfiltrate via clipboard, open socket) — all must be denied + audited | CI |
| Audit log integrity (hash chain verifies) | CI |
| Crash-dump symbolisation and panic-policy test | CI |
| Key isolation test (`keyd` buffers unmappable to clients) | CI |
| Boot integrity / signature verification | release |

---

## 14. Threat matrix (summary)

| Threat | Mitigation |
|---|---|
| Malicious app reads another user's files | VFS permission checks against kernel creds; fs jail; audit |
| App calls an ungranted service | Default-deny Messenger policy; handles required; audit |
| App escapes syscall allowlist | Per-process syscall bitmap; friendly deny; audit |
| App exhausts kernel memory | Metered handles/buffers/queues; quotas; OOM policy |
| Forgery of sender identity | Kernel-stamped credentials; userspace cannot write them |
| Clipboard/data exfiltration | Clipboard policy + consent; transfer caps; audit; a paste of sensitive data can be marked |
| Compromised daemon | Least-privilege service account; signed bundle; sandbox profile; no root |
| Network daemon exploit | Sandboxed; TLS keys in `keyd`; per-profile firewall; minimal parser surface |
| Kernel bug via syscalls/parcels | Rust safety, checked copies, fuzzing, W^X, SMEP/SMAP, KASLR-lite |
| Secret theft | `keyd` isolation; `SHARE_ONLY` buffers; per-login sealing |
| Repudiation | Hash-chained audit log; correlation ids for calls |
| Privilege creep | Capabilities dropped on exec; timed elevation; diff-based consent |

---

*See also:* [Platform plan](platform-plan.md) · [Messenger spec](messenger.md)
