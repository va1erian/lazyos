# LazyOS Configuration Registry — `regd` (v1: simple, v2: deferred features)

**One line:** `regd` is a small userspace service that stores typed
key/value configuration in a hierarchical path tree, reachable only through
Messenger, and tells subscribers when a value changes. It is the Windows
Registry / ODM idea reduced to what LazyOS needs today.

Companion to [`messenger.md`](messenger.md),
[`security-model.md`](security-model.md), and
[`platform-plan.md`](platform-plan.md). It assumes Messenger sync calls and
the writable ext2 + VFS already exist.

**Scope split.** §1–§6 describe **v1**, which is deliberately minimal. Every
feature we considered and cut is collected in **§7 (v2)** so nothing is lost;
v1 must not grow into it.

---

## 1. v1 goals and non-goals

Goals:

- One place for services and apps to keep structured settings instead of each
  inventing a text file and parser.
- Reachable only via Messenger (no direct file access to the store).
- Survives reboots and crashes without corrupting the store.
- Services can react to changes (pub/sub).

Non-goals for v1 (all moved to §7): schemas, versioning/history, CAS,
per-path ACLs, audit trail, compaction, queries, quotas, `keyd` delegation.

---

## 2. Design

| Concern | v1 choice |
|---|---|
| Mediation | Userspace service `regd` over Messenger; kernel stays mechanism-only. |
| Namespace | Hierarchical paths: `sys/net/eth0/mtu`, `user/1000/shell/theme`. |
| Data model | One **value** per path: `bool`, `i64`, `u64`, `string`, or `bytes`. No records, no schemas. Structure comes from the path tree (`.../eth0/dhcp`, `.../eth0/mtu`). |
| Persistence | The whole tree in memory; on every write, serialize to `/system/regd/store.tmp`, fsync, rename over `/system/regd/store` (logic in `libs/regd`). A crash leaves the old or the new store, never a torn one, provided the volume's `rename` is atomic (see §6). Config is small; this is fast enough. |
| Access | Messenger interface `os.lazy.regd.v1` only. `regd` alone holds a handle to `/system/regd`. |
| Notification | One Messenger topic per changed path, under `system/regd/changed/` (§4). |
| Access control | Two fixed rules using the kernel-stamped `uid` (see §5). |

---

## 3. Messenger interface

`os.lazy.regd.v1`, root object resolved from `messengerd` as `os.lazy.regd`.

```
interface os.lazy.regd.v1 {
  Get(path)              -> (value);          // NOT_FOUND if absent
  Set(path, value)       -> ();               // creates or overwrites
  Delete(path)           -> ();               // deleting an absent path is OK
  List(path_prefix)      -> (stream of path); // all paths under the prefix
}

// Pub/sub (details in section 4)
//   topic "system/regd/changed/<path>"       payload: (path, deleted: bool)
//   topic "system/regd/changed/<subtree>/#"  wildcard for a whole subtree
```

- `Set` and `Delete` are applied and persisted before the call returns; the
  change topic is published after the persist succeeds. If the persist
  fails, the in-memory store must be left as it was and the call returns
  `REGD_IO` (see section 6).
- `List` returns only paths the caller is allowed to read (§5).
- Limits, to keep `regd` bounded: path ≤ 256 bytes, value ≤ 4 KiB, total
  store ≤ 1 MiB. Exceeding a limit returns `REGD_TOO_LARGE`.
- Path validation: segments are `[a-z0-9_.-]+`, separated by `/`, no empty
  segments, no `..`. Anything else returns `REGD_BAD_PATH`.
- Writes are last-writer-wins. Callers that need read-modify-write must
  tolerate that in v1 (see §7 for CAS).

A `regctl` CLI (`get`, `set`, `delete`, `list`, `watch`) is just another
Messenger client.

---

## 4. Messenger integration (v1)

`regd` is an ordinary Messenger service; it needs no kernel changes and no
special status beyond what `messengerd` already gives every service
(`messenger.md` §8, §13).

- **Registration and discovery.** `regd` calls `Register("os.lazy.regd",
  endpoint, ["os.lazy.regd.v1"])` with `messengerd`. Clients use
  `Resolve("os.lazy.regd")`. Because it is in the registry, it shows up in
  `os.lazy.messenger.registry.v1` (`ListServices`, `GetInterface`, `WhoOwns`)
  and `messengerctl services` with no extra work. Its IDL lives in `idl/` and
  goes through `midlc` like the echo sample, so client stubs are generated.
- **Start order and activation.** `init` starts `regd` after `messengerd` and
  the VFS are up, and before the services that read config. `regd` is
  declared in `init`'s service manifest so `Resolve` can also start it on
  demand. Consumers must tolerate `Resolve` failing briefly at boot and retry
  with a short backoff instead of assuming `regd` is already there.
- **Caller identity.** The `uid` in every access check is the credential the
  kernel stamped on the Messenger transaction. `regd` never trusts a uid sent
  in a request body. Reaching `regd` at all is a `CALL` right on its handle,
  governed by the normal Messenger policy; the uid rules in §5 are checked on
  top of that.
- **Change topics.** Topic names live under `system/regd/changed/`, matching
  the existing `system/...` topics (`system/events/...`, `system/stats/...`)
  so they fall under the same topic policy hook (`authorize_topic`). Only
  `regd` may publish there. Delivery is best-effort and conflated (the system
  topic default), and nothing is retained: a subscriber that just started
  calls `Get`/`List` to read current state, then relies on the topic for
  changes.
- **Topics must not bypass the uid rules.** Anyone who can subscribe to a
  topic sees what is published on it, so the payload carries only
  `(path, deleted)`, never the value; the subscriber calls `Get`, which runs
  the normal uid check. Because path names under `user/<uid>/` can still leak,
  the topic policy allows subscribing to `system/regd/changed/user/<uid>/...`
  only for that uid and root, and `system/regd/changed/sys/...` for anyone.
  This policy rule is part of v1; if it cannot be expressed yet, `regd` does
  not publish the `user/` subtree topics at all.
- **Health and logs.** `regd` sends heartbeats to `healthd` and logs through
  `logd` like the other services. Dependencies it reports: `messengerd` and
  the store directory being writable. The degraded mode described in §6 shows
  up as unhealthy-but-serving.
- **Client helper.** A small wrapper (`Get`/`Set`/`Delete`/`List`/`watch`
  around the generated stubs, with the resolve-and-retry above) lives in the
  user library so services do not each re-implement it. `regctl` is built on
  the same helper.

Registry vs `regd`: `messengerd`'s registry stores *who provides which
service* (names to endpoints, live only). `regd` stores *configuration data*
(paths to values, persistent). They never share state, and `regd` never
stores endpoint or handle data; a config value that names a service is just a
string the client passes to `Resolve`.

---

## 5. Access control (v1)

Just two rules, checked against the `uid` Messenger already stamps on each
call:

- `sys/**` — anyone can read, only uid 0 can write.
- `user/<uid>/**` — only that user (and uid 0) can read or write.
- Any other top-level path is rejected (`REGD_BAD_PATH`).

`regd` runs unprivileged with only the `/system/regd` grant. Secrets do not go
in `regd` in v1; use `keyd` directly.

---

## 6. Failure modes, rollout, testing

**Failure modes**

- *No writable persistent volume.* Today the FAT boot volume is read-only and
  ext2 is only exercised by the kernel test suite, so userspace has no
  guaranteed persistent, writable place for `/system/regd`. Until an ext2
  volume (on virtio-blk) is part of the image, `regd` falls back to a ramfs
  directory: it works, reports itself degraded to `healthd`, logs a warning,
  and config does not survive a reboot. ext2 also has no journal, so before
  relying on it, verify that the VFS `rename` over an existing file is a
  single atomic operation in the ext2 driver.
- *Persist fails at runtime* (disk full, I/O error): `Set`/`Delete` return
  `REGD_IO`, the in-memory store is left unchanged (mutate a copy, swap it in
  only after `persist` succeeds), and no change topic is published.
- *Crash during write:* atomic rename (§2) means the store is always either
  the previous or the new complete version. A leftover `store.tmp` is deleted
  on startup.
- *regd restarts:* clients reconnect and re-resolve `os.lazy.regd` like any
  other service; there is no per-client state. Subscribers should re-`Get`
  after reconnecting, since change topics are best-effort.
- *Corrupt store file:* `regd` starts empty, logs to `logd`, and keeps the bad
  file as `store.corrupt` for inspection.

**Rollout**

0. `libs/regd` store library (issue #259): validation, limits, uid rules,
   codec, atomic persist over a `StoreFs` trait.
1. `regd` service on Messenger with `Get`/`Set`/`Delete`/`List` (issue #260):
   registry registration, IDL, `init` start order, change topics with the
   subscription policy from §4, ramfs fallback.
2. Persistence on a real writable volume once one is in the image, with the
   rename check from §6.
3. `regctl` CLI and the client helper, then migrate existing ad hoc config (network, xuid/display,
   accounts prefs) onto `regd`.

**Testing** (per AGENTS.md, even though `regd` is userspace): correctness
tests for path validation, limits, permission rules, persist/reload, a
failed persist leaving the store unchanged, and topic delivery plus the
cross-user subscription denial;
a soak test with many rapid `Set`s and induced crashes between write and
rename to confirm the store never tears.

---

## 7. v2 and later (deferred from v1)

Everything below was in the earlier draft of this plan and is intentionally
**not** in v1. Ordering is roughly by expected value; each item is
independent unless noted.

### 6.1 Schemas (ODM-style object classes)

- Typed, named-field records validated on write, instead of scalar values.
  Field IDs are stable and append-only (like Messenger method IDs) so schemas
  can gain fields without breaking old records; fields can be `required`,
  have defaults, or reference enums.
- Schemas are themselves records under `sys/regd/schema/<name>`
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

- `expected_gen` on `Put`/`Delete`/`Revert`: fails with `REGD_CONFLICT` if it
  doesn't match the current generation. Omitted means blind write.
- `expected_gen = 0` means create-only-if-absent (the first real generation
  is 1), so racing creators can't both win.
- Requires 6.2.

### 6.4 Per-path ACLs

- ACL per path: `{owner, group, world} x {READ, WRITE, MANAGE, WATCH}`,
  checked against the kernel-stamped `uid/gid/label`; `SetAcl`/`GetAcl` (needs
  MANAGE).
- Default ACLs for well-known subtrees seeded from `regd`'s own data so
  policy is inspectable: `sys/**` root-writable/world-readable, `user/<uid>/**`
  owner rw + root r.
- `List`/`Query` filter results per path; READ on a parent does not reveal
  private descendants.

### 6.5 Audit trail

- Every generation records `writer_cred`; changes are streamed to `auditd`
  via `regd/changed/#`, but that topic is only a best-effort notification.
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

- Store in `regd` only an opaque, **non-authorizing** `keyd` key name. Because
  `sys/**` is world-readable, the identifier must grant nothing by itself:
  `keyd` re-checks the caller's identity/ACL on every operation, so reading
  the name out of `regd` gives no more access than knowing a secret's name.

### 6.7 Query

- `Query(path_prefix, schema_id, predicate)`: structured filters over typed
  fields (equality, range, string prefix) reusing Messenger's parcel/TLV
  machinery; no SQL. No cross-subtree joins (open question; leaning no).
- Requires 6.1.

### 6.8 Quotas, health, and storage layout

- Per-user storage quotas; `regd` health/metrics exposed the way `healthd`
  expects.
- Open question: a separate store file per top-level subtree (`sys`,
  `user/<uid>`) for independent backup/restore (leaning yes) vs. one global
  store.

### 6.9 v2 testing note

Once versioning, WAL, and CAS land, add the heavier soak suite: many rapid
`Put`s/generations, crash-and-replay simulation, and concurrent CAS races,
because config corruption is a whole-system failure.
