# Messenger security: credentials, ACL, audit, quotas

**What it is.** The kernel security core around every Messenger call and every
metered resource: kernel-stamped identity, a default-deny ACL, the hash-chained
audit ring, and per-uid quotas. Spec: [security-model.md](../security-model.md).

**Key files**

| Path | Role |
|---|---|
| `kernel/src/ipc/credentials.rs` | `Cred`, capability bits, audited transition gate (issue #68/#101) |
| `kernel/src/ipc/acl.rs` | Ordered default-deny rule list, verdicts, reason codes; label-keyed rule sets |
| `kernel/src/ipc/labels.rs` | Interned label table (`app:<id>`, `system:<name>`, `dev:<id>`) |
| `kernel/src/ipc/devspawn.rs` | A labelled task's spawn into a `dev:` label (issue #529) |
| `kernel/src/ipc/policy.rs` | Namespace rules for labelled tasks (`os.lazy.*`, `app.<id>.*`, `app/<id>/`) |
| `kernel/src/ipc/syscalls/aclop.rs` | The `acl_load` messenger op (`OP_ACL_LOAD = 18`) |
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
  A service that only needs to know *who called* reads the identity stamped on
  the message instead (`RECV_SENDER_ID`, [ipc-fabric.md](ipc-fabric.md)): no
  capability, no capability bits disclosed.
  Transition records use `AUDIT_INTERFACE = "os.cred."` / `AUDIT_METHOD_SET` with
  `reason` codes for allowed, not-privileged, widening, bad target and
  label-locked.
- **Labels.** `label_id` names an interned string (`labels.rs`): at most 160
  bytes of `[a-z0-9.:-]`, one of `app:<reverse.dns.name>` (an installed app),
  `system:<name>` (a platform service) or `dev:<reverse.dns.name>` (an app run
  from an IDE under its own permissions), 256 labels, append-only (`0` =
  unlabelled; a full table refuses new labels rather than evicting). The label
  is write-once: `LabelStamp::Assign` (the labelled spawn, `spawnv` with
  `AsLabelled`) sets it on a new child for an unlabelled `CAP_SETUID` creator;
  every other stamp is `LabelStamp::Keep` and fails with
  `TransitionError::LabelLocked` if it would change it. Children inherit their
  creator's label, so an app's helpers stay in its sandbox.
- **Spawning into `dev:`** (`devspawn.rs`, `LabelStamp::Develop`, issue #529).
  The one assignment without `CAP_SETUID`: a *labelled* caller's `spawnv`
  `AsLabelled` naming a `dev:` label is allowed when (1) the caller's label
  rules allow `os.lazy.process.label.spawn.v1` (`idl/policy.midl`) with
  `fnv1a32(<target label>)` as the method (a manifest's `develop = true`
  compiles to the wildcard method), (2) the target label already exists and
  holds rules (`pkgd` loads an approved set, always ending with a catch-all
  deny, and revokes by loading none; this path only looks labels up, never
  interns), and (3) the child keeps the caller's uid, gid and session with a
  subset of its capabilities. A refusal is `-EACCES`, audited on the scope's
  interface id with the hashed label as the method (and printed as a
  `LABEL:DENY ... spawn=<label>` line under `LAZYOS_LABEL_TRACE=1`); an
  `app:`/`system:` target from a labelled caller keeps the gate's `-EPERM`.
  Unlabelled callers are unchanged. The stamp is re-checked when applied.
  `spawnv`'s `personality::STDIO` flag hands the child three of the caller's
  descriptors as 0, 1 and 2 (nothing else), so the IDE keeps the run's pipes.

**ACL** (`acl.rs`)

- `Rule { actor, interface_id, method, allow }` with `ANY_*` wildcards; first
  match wins, so exact denies can precede broad allows. Empty policy = the
  bootstrap window (allow, `BOOTSTRAP_ALLOW`); once loaded, no match = deny
  (`DEFAULT_DENY`). `load`, `is_loaded`, `rule_count`, `evaluate`,
  `evaluate_verdict` (with machine-readable reason).
- `authorize(actor_slot, interface_id, method, txn_id)`
  (`kernel/src/ipc/mod.rs:43`) reads kernel-stamped credentials, evaluates the
  policy, and records an audit event on denial (and on allows while tracing).
  A task with a label is judged by its label's rules (`policy::evaluate_labelled`),
  never its uid; unlabelled tasks keep the uid rules and the bootstrap window.
- **Label rules** (`acl::load_label`): a second, separate rule set whose actor is
  a label id. A labelled task is default-deny from its first call (no bootstrap
  window); rules are first-match, at most 256 per label and 4096 in total.
  `load_label` replaces every rule of one label, so revoking an app is loading
  an empty list. The only implicit grants are the registry's
  register/unregister/resolve calls (checked per name, below) and the app's
  own namespaces.
- **Namespaces** (`policy.rs`, issue #308): `register` of `os.lazy.*` needs a
  `system:*` label, or no label plus uid 0, `CAP_IPC_CONTROL` or `CAP_DEV_CLAIM`
  (a provisioned driver); any other unlabelled task falls back to the uid rules
  (bootstrap-allow until a uid policy is loaded), so `init`'s capability-less
  services such as `netd` still register. No labelled app can claim `os.lazy.*`.
  `app.<id>.<name>` needs label `app:<id>` (`<name>` is
  one dot-free segment so a name names exactly one id); a labelled task may
  register nothing else. A topic at or under `app/<id>/` (publish or subscribe)
  is allowed for `app:<id>`. `dev:<id>` owns exactly the same names and topics
  as `app:<id>`, so a development run behaves like the installed app. A
  labelled app's `Register` must also name every interface it advertises
  (`interface_names`, one per id; `fnv1a64(name)` must equal the id) and every
  name must lie in its domain, `<id>.<name>.v<N>`
  (`policy::check_interfaces`, `policy::interface_in_domain`); `system:*` and
  unlabelled tasks are not limited, but names they send must match. Refusals
  are audited as `UNNAMED_INTERFACE` (10) or `FOREIGN_INTERFACE` (11) with the
  offending interface id as `txn_id` (issue #495). Resolving any other name (checked as
  `os.lazy.messenger.names.resolve.v1` with `fnv1a32(name)` as the method, so a
  rule grants one exact name), calling any interface and every other topic
  segment need an allow rule for the label. Checks run against the *client's*
  slot when `messengerd` proxies, and before the registry lookup, so a refusal
  reveals nothing about which names exist. Every refusal is audited with the
  label id and a reason (`LABEL_DEFAULT_DENY`, `RESERVED_NAMESPACE`,
  `OUTSIDE_NAMESPACE`, ...); `policy::explain(label, reason)` renders the
  friendly sentence naming the namespace the app may use.
- **Loading policy** (`OP_ACL_LOAD`, `idl/policy.midl`): the request parcel's
  body is the generated `LoadLabelArgs { label, rules: Array<LabelRule> }`;
  the caller needs `CAP_IPC_CONTROL` (`-EPERM`, audited) and must itself pass
  `authorize` (a labelled task is refused). Replace-all per label; a malformed
  label or more than 256 rules is `-EINVAL`, a full table or rule budget
  `-ENOMEM`, and a failed load changes nothing. Userspace: `user::messenger::policy`.

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
| `UserMemory` | 256 MiB | `sbrk`/`mmap` VMA growth, and a program's ELF segments at load (charged to the uid that runs it, refunded when the space is freed; #265) |
| `Handles` | 1024 | handle open/duplicate |
| `Fds` | `4 * fd_max` | every open descriptor slot (open, dup, dup2, fork, spawn copies); `EMFILE` at the cap (#483) |
| `QueueBytes` / `QueueDepth` | 4 MiB / 1024 messages | channel enqueue, charged to sender uid |
| `CpuTicks` | 2^32 | every timer tick, booked to the running task's uid; past the limit its tasks pay 8x the stride (#483) |
| `DeviceClaims` | 8 | `dev::claim` |
| `DmaMemory` | 8 MiB | `dev::dma_alloc` contiguous pool bytes |

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
