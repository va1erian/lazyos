# LazyOS Configuration Registry — `confd` (v1: simple, v2: deferred features)

**One line:** `confd` is a small userspace service that stores typed
key/value configuration in a hierarchical path tree, reachable only through
Messenger, and tells subscribers when a value changes. It is the Windows
Registry / ODM idea reduced to what LazyOS needs today.

Companion to [`messenger.md`](messenger.md),
[`security-model.md`](security-model.md), and
[`platform-plan.md`](platform-plan.md). It assumes Messenger sync calls and
the writable ext2 + VFS already exist.

**Scope split.** §1–§5 describe **v1**, which is deliberately minimal. Every
feature we considered and cut is collected in **§6 (v2)** so nothing is lost;
v1 must not grow into it.

---

## 1. v1 goals and non-goals

Goals:

- One place for services and apps to keep structured settings instead of each
  inventing a text file and parser.
- Reachable only via Messenger (no direct file access to the store).
- Survives reboots and crashes without corrupting the store.
- Services can react to changes (pub/sub).

Non-goals for v1 (all moved to §6): schemas, versioning/history, CAS,
per-path ACLs, audit trail, compaction, queries, quotas, `keyd` delegation.

---

## 2. Design

| Concern | v1 choice |
|---|---|
| Mediation | Userspace service `confd` over Messenger; kernel stays mechanism-only. |
| Namespace | Hierarchical paths: `sys/net/eth0/mtu`, `user/1000/shell/theme`. |
| Data model | One **value** per path: `bool`, `i64`, `u64`, `string`, or `bytes`. No records, no schemas. Structure comes from the path tree (`.../eth0/dhcp`, `.../eth0/mtu`). |
| Persistence | The whole tree in memory; on every write, serialize to `<dir>/store.tmp` (`/data/confd` preferred, see §5), fsync, rename over `<dir>/store`. Rename is atomic, so a crash leaves the old or the new store, never a torn one. Config is small; this is fast enough. |
| Access | Messenger interface `os.lazy.confd.v1` only. `confd` alone holds a handle to its store directory. |
| Notification | One Messenger topic per changed path. |
| Access control | Two fixed rules using the kernel-stamped `uid` (see §4). |

---

## 3. Messenger interface

`os.lazy.confd.v1`, root object resolved from `messengerd` as `os.lazy.confd`.

```
interface os.lazy.confd.v1 {
  Get(path)              -> (value);          // NOT_FOUND if absent
  Set(path, value)       -> ();               // creates or overwrites
  Delete(path)           -> ();               // deleting an absent path is OK
  List(path_prefix)      -> (stream of path); // all paths under the prefix
}

// Pub/sub
//   topic "confd/changed/<path>"   payload: (path, new_value | deleted)
//   topic "confd/changed/<subtree>/#"   wildcard for a whole subtree
```

- `Set` and `Delete` are applied and persisted before the call returns; the
  change topic is published after the persist succeeds.
- `List` returns only paths the caller is allowed to read (§4).
- Limits, to keep `confd` bounded: path ≤ 256 bytes, value ≤ 4 KiB, total
  store ≤ 1 MiB. Exceeding a limit returns `CONFD_TOO_LARGE`.
- Path validation: segments are `[a-z0-9_.-]+`, separated by `/`, no empty
  segments, no `..`. Anything else returns `CONFD_BAD_PATH`.
- Writes are last-writer-wins. Callers that need read-modify-write must
  tolerate that in v1 (see §6 for CAS).

A `confctl` CLI (`get`, `set`, `delete`, `list`, `watch`) is just another
Messenger client.

---

## 4. Access control (v1)

Just two rules, checked against the `uid` Messenger already stamps on each
call:

- `sys/**` — anyone can read, only uid 0 can write.
- `user/<uid>/**` — only that user (and uid 0) can read or write.
- Any other top-level path is rejected (`CONFD_BAD_PATH`).

`confd` runs unprivileged with only the `/system/confd` grant. Secrets do not go
in `confd` in v1; use `keyd` directly.

---

## 5. Failure modes, rollout, testing

**Failure modes**

- *Crash during write:* atomic rename (§2) means the store is always either
  the previous or the new complete version. A leftover `store.tmp` is deleted
  on startup.
- *confd restarts:* clients reconnect and re-resolve `os.lazy.confd` like any
  other service; there is no per-client state. Subscribers should re-`Get`
  after reconnecting, since change topics are best-effort.
- *Corrupt store file:* `confd` starts empty, logs to `logd`, and keeps the bad
  file as `store.corrupt` for inspection.
- *Where the store lives:* on the shipped image `/system` is the read-only FAT
  boot volume (`mkdir` fails with `EROFS`) and `/tmp` is volatile ramfs, so the
  only persistent, writable location is the ext2 data volume. Preference order
  is `/data/confd`, `/system/confd` (for a future writable system volume),
  then `/tmp/confd` (reported *degraded*). The kernel mounts `/data` before
  userspace starts (there is no mount syscall), so normally `confd` finds it
  ready; there is no retry in `init` because the mount is not asynchronous.
- *Data volume arrives late, or an earlier run used a lower location:* settings
  must not be silently lost. At startup `confd` merges any store left in a
  lower-ranked directory into the chosen one (`Confd::absorb`); while running
  on a lower-ranked directory it re-probes `/data/confd` every ~200 ticks and
  on success moves onto it (`Confd::rebind`). Both merges only add paths the
  destination lacks, so existing `/data` values always win; persist happens
  before the switch, so a failure leaves the running store unchanged and is
  retried. A fully merged source store is renamed `store.migrated`, so a value
  deleted afterwards is never resurrected. Entries that do not fit the limits
  are skipped (and the source kept) rather than dropped. Changed `sys/` paths
  are announced; `CONFD:SEED`/`CONFD:MIGRATED` serial lines record it.

**Rollout**

1. `confd` in-memory with `Get`/`Set`/`Delete`/`List` and change topics.
2. Persistence (atomic-rename store) and the uid rules from §4.
3. `confctl` CLI, then migrate existing ad hoc config (network, xuid/display,
   accounts prefs) onto `confd`.

**Testing** (per AGENTS.md, even though `confd` is userspace): correctness
tests for path validation, limits, permission rules, and persist/reload;
a soak test with many rapid `Set`s and induced crashes between write and
rename to confirm the store never tears.

---

## 6. v2 and later (deferred from v1)

Everything below was in the earlier draft of this plan and is intentionally
**not** in v1. Ordering is roughly by expected value; each item is
independent unless noted.

### 6.1 Schemas (ODM-style object classes)

- Typed, named-field records validated on write, instead of scalar values.
  Field IDs are stable and append-only (like Messenger method IDs) so schemas
  can gain fields without breaking old records; fields can be `required`,
  have defaults, or reference enums.
- Schemas are themselves records under `sys/confd/schema/<name>`
  (self-hosting, introspectable via the same API); `RegisterSchema` /
  `GetSchema` calls. Open question: bootstrap of the schema-of-schemas vs.
  compiling schemas into services at build time.
- Each generation (6.2) would carry its own `schema_id`; a key is identified
  by path alone, and a breaking schema change that must coexist with the old
  shape uses a different path (or versioned segment like `eth0@v2`).

### 6.2 Versioning and history

- Append-only **generations** per key: `(gen, schema_id, writer, ts,
  parent_gen, record, comment)`; current pointer plus history; reads can pin a
  `gen`. Generations are immutable.
- `History(path, limit)` and `Revert(path, to_gen)` (revert creates a new
  generation copying an old one).
- Storage: write-ahead log plus per-key object files and an index snapshot,
  replacing v1's single-file atomic rename.
- **Compaction:** per-subtree retention policy (e.g. last 32 generations or
  30 days); compaction is itself logged and never drops the current value.

### 6.3 Optimistic concurrency (CAS)

- `expected_gen` on `Put`/`Delete`/`Revert`: fails with `CONFD_CONFLICT` if it
  doesn't match the current generation. Omitted means blind write.
- `expected_gen = 0` means create-only-if-absent (the first real generation
  is 1), so racing creators can't both win.
- Requires 6.2.

### 6.4 Per-path ACLs

- ACL per path: `{owner, group, world} x {READ, WRITE, MANAGE, WATCH}`,
  checked against the kernel-stamped `uid/gid/label`; `SetAcl`/`GetAcl` (needs
  MANAGE).
- Default ACLs for well-known subtrees seeded from `confd`'s own data so
  policy is inspectable: `sys/**` root-writable/world-readable, `user/<uid>/**`
  owner rw + root r.
- `List`/`Query` filter results per path; READ on a parent does not reveal
  private descendants.

### 6.5 Audit trail

- Every generation records `writer_cred`; changes are streamed to `auditd`
  via `confd/changed/#`, but that topic is only a best-effort notification.
- The durable record is `History`. Generations are not compactable until
  `auditd` acknowledges them, tracked as a **per-subtree monotonic sequence
  number** assigned to every committed generation.
- `AuditReplay(subtree, after_seq, limit)` returns the next generations in
  order; `auditd` advances its cursor to the highest `seq` returned, repeats
  until a page comes back short, then acknowledges. Gives one resumable
  cursor per subtree with nothing skipped or double-counted, including after
  an `auditd` restart. Needs MANAGE + WATCH.
- Requires 6.2 and 6.4.

### 6.6 Secrets delegation to `keyd`

- Store in `confd` only an opaque, **non-authorizing** `keyd` key name. Because
  `sys/**` is world-readable, the identifier must grant nothing by itself:
  `keyd` re-checks the caller's identity/ACL on every operation, so reading
  the name out of `confd` gives no more access than knowing a secret's name.

### 6.7 Query

- `Query(path_prefix, schema_id, predicate)`: structured filters over typed
  fields (equality, range, string prefix) reusing Messenger's parcel/TLV
  machinery; no SQL. No cross-subtree joins (open question; leaning no).
- Requires 6.1.

### 6.8 Quotas, health, and storage layout

- Per-user storage quotas; `confd` health/metrics exposed the way `healthd`
  expects.
- Open question: a separate store file per top-level subtree (`sys`,
  `user/<uid>`) for independent backup/restore (leaning yes) vs. one global
  store.

### 6.9 v2 testing note

Once versioning, WAL, and CAS land, add the heavier soak suite: many rapid
`Put`s/generations, crash-and-replay simulation, and concurrent CAS races,
because config corruption is a whole-system failure.
