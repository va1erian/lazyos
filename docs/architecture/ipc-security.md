# Messenger security: credentials, ACL, audit, quotas

**What it is.** The kernel security core around every Messenger call and every
metered resource: kernel-stamped identity, a default-deny ACL, the hash-chained
audit ring, and per-uid quotas. Spec: [security-model.md](../security-model.md).

**Key files**

| Path | Role |
|---|---|
| `kernel/src/ipc/credentials.rs` | `Cred`, capability bits, audited transition gate (issue #68/#101) |
| `kernel/src/ipc/acl.rs` | Ordered default-deny rule list, verdicts, reason codes |
| `kernel/src/ipc/audit.rs` | 128-entry ring with an FNV-1a hash chain |
| `kernel/src/quota.rs` | Per-uid resource limits and usage (issue #103) |
| `kernel/src/ipc/mod.rs` | `authorize()`: the single policy choke point |

**Credentials** (`credentials.rs`)

| Capability | Meaning |
|---|---|
| `CAP_NET_BIND`, `CAP_NET_RAW` | privileged ports / raw sockets |
| `CAP_SYS_ADMIN`, `CAP_SYS_TIME`, `CAP_AUDIT_READ` | mounts/driver grants, clock, audit stream |
| `CAP_IPC_CONTROL` | manage other services' endpoints; registry proxy |
| `CAP_SETUID` | use the credential transition gate |
| `CAP_KILL` | signal tasks of another uid (`kill`/`tkill`/`tgkill`); otherwise only same-uid targets (and `SIGCONT` within a session) |
| `CAP_DEV_CLAIM` | list and claim devices through syscall 23 (the coarse gate; the class ACL rule `os.kernel.dev.<class>` and the `Device` handle rights bound what a claim can do) |
| `CAP_INPUT_RAW` | drain the raw input event bus through syscall 25 (`kernel/src/input/rawsys.rs`; every keystroke passes through it, so `init` stamps it onto `inputd` alone; see [../input-plan.md](../input-plan.md)) |

- `Cred { uid, gid, caps, label_id, session }`; a program the kernel starts is
  `Cred::ROOT` (uid 0, all caps), and every task another task creates (`spawn`,
  `fork`, `clone`) starts with a copy of its creator's credentials
  (`credentials::inherit`), so a child is never more privileged than its
  parent. State is keyed by task slot; userspace has no direct write.
- `transition(actor, target, cred)` is the one audited path: the actor needs
  `CAP_SETUID`, the request may never widen privilege (uid 0 only by root, caps
  only downward), the target must be the actor or a live task, and the kernel
  task may only restamp itself. `check` validates a spawn before the task exists;
  `read` lets a task read its own identity, or another's with `CAP_SETUID`.
  Transition records use `AUDIT_INTERFACE = "os.cred."` / `AUDIT_METHOD_SET` with
  `reason` codes for allowed, not-privileged, widening and bad target.

**ACL** (`acl.rs`)

- `Rule { actor, interface_id, method, allow }` with `ANY_*` wildcards; first
  match wins, so exact denies can precede broad allows. Empty policy = the
  bootstrap window (allow, `BOOTSTRAP_ALLOW`); once loaded, no match = deny
  (`DEFAULT_DENY`). `load`, `is_loaded`, `rule_count`, `evaluate`,
  `evaluate_verdict` (with machine-readable reason).
- `authorize(actor_slot, interface_id, method, txn_id)`
  (`kernel/src/ipc/mod.rs:43`) reads kernel-stamped credentials, evaluates the
  policy, and records an audit event on denial (and on allows while tracing).

**Audit ring** (`audit.rs`)

- `AUDIT_CAPACITY = 128`; the oldest event is overwritten, so a denial flood
  cannot allocate. `AuditEvent` carries ticks, actor slot/uid/label,
  interface/method, allow flag, reason code and `txn_id`.
- Every record extends `chain(prev, event)` (FNV-1a 64 from `GENESIS_HASH`), so
  `auditd` can detect removed, reordered or edited records. `recent`, `count`,
  `total`, `denials`, `allows`, `last_hash` are the read surface;
  `set_trace(true)` also records allows (off by default). The ring is never
  cleared on a running system; `reset` is test-only.

**Quotas** (`quota.rs`, issue #103)

| Resource | Default limit | Enforced at |
|---|---|---|
| `KernelMemory` | 32 MiB | shared buffers (frames) |
| `UserMemory` | 256 MiB | `sbrk`/`mmap` VMA growth |
| `Handles` | 1024 | handle open/duplicate |
| `Fds` | 256 | API only until the fd table is charged |
| `QueueBytes` / `QueueDepth` | 4 MiB / 1024 messages | channel enqueue, charged to sender uid |
| `CpuTicks` | 2^32 | API only until the scheduler meters uids |

- `DEFAULT_LIMITS` applies to regular uids; uid 0 gets `ROOT_LIMITS`. Limits are
  kernel policy (`set_limit`); syscall 11 (`process::sys_quota`) is read-only and
  copies `usage/limit` pairs in `Resource::ALL` order.
- `charge`/`charge_many` are atomic (all-or-nothing); `release` is saturating and
  counts accounting errors. Entries are created on first charge and remember peak
  usage. `QUOTAS` is a leaf lock (credentials -> quotas order).
- `UserMemory` is also ledgered per address space (`SpaceCharge`): a release only
  returns what that space charged, and `forget_address_space` gives the rest
  back when the space is freed, so an exiting task cannot strand its uid's
  quota.

**Task teardown** (`ipc::teardown_task`)

- `task::reap_child` and `task::reclaim_pending` call it for every slot they free,
  *before* the address space is released: names owned by the slot are dropped,
  each channel endpoint is closed (a side is only marked dead when no other
  handle still names it, so one client exiting does not fail its siblings'
  calls), each buffer handle is closed and its mapping unmapped from the still
  live page table, transactions the slot started are forgotten, and the handle
  table is emptied (returning the per-uid handle charge). The next task to reuse
  the slot starts with nothing.

**Status.** Working: stamped identity, audited transitions, default-deny policy,
hash-chained audit, quotas on handles/buffers/queues/user memory. Open: policy
compiler/hot reload, fd/CPU quota call sites, withholding uid 0 by default.

**Device claims** (`kernel/src/dev/`, issue #240). `dev_*` authorizes `claim`
against the class-specific id `os.kernel.dev.<class>` (`dev/class.rs`), so a
rule for one class never covers another. Every claim, release, denial,
DMA-enable attempt and interrupt-ack timeout is one audit record whose
`interface_id` is the class id, `method` says what happened, `txn_id` is
`1 << 40 | device id` (`dev/report.rs`), and `reason_code` uses codes from 0x10
up (a granted claim carries the rights in bits 8 and up). `Device` handles are
never duplicable or transferable, and `ipc::teardown_task` releases every claim
the dying task holds before it touches the handle table.
