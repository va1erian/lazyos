# LazyOS Configuration Registry — a Messenger-native ODM/registry plan

**One line:** a single, schema-typed, versioned configuration and object
database — `regd` — reachable only through Messenger, that plays the role the
Windows Registry plays for Win32 and the ODM plays for AIX: the one place
system services, drivers, and apps store structured settings and discover
each other's configuration, with transactional writes, history, and
capability-checked access instead of ambient files.

This is a focused plan alongside [`messenger.md`](messenger.md),
[`security-model.md`](security-model.md), and the relevant stage in
[`platform-plan.md`](platform-plan.md). It assumes Messenger's sync/async
fabric and the writable ext2 + VFS already described there.

---

## 1. Why LazyOS needs this (and what "comparable to ODM/Registry" means)

Both prior art points solve the same problem: **many independent components
need typed, structured, queryable configuration, with a stable API, instead of
each one inventing its own text file and parsing convention.**

| System | Storage model | Access | Versioning |
|---|---|---|---|
| Windows Registry | hierarchical hive of keys/values, typed (`REG_DWORD`, `REG_SZ`, `REG_BINARY`, ...) | Win32 `Reg*` API, ACLs per key | none built-in; apps roll their own |
| AIX ODM | flat-file "object classes" (schema'd C-struct-like records) queried like a tiny relational store | `odmget`/`odmadd`/liboodm C API | none built-in |
| **LazyOS `regd` (this plan)** | hierarchical **paths** of typed, schema-validated **records**, backed by an object store on ext2 | **Messenger interface only** — sync calls + pub/sub change topics | **built-in**: every write is a new generation, readable by generation or "current" |

What we take from each:

- From the **Registry**: hierarchical namespace (`HKLM`-style trees), typed
  values, per-key ACLs, "watch this key for changes" semantics.
- From **ODM**: schema'd records (not just scalar values) queried by
  predicate, and a clean separation between the *object classes* (schema) and
  the *objects* (data) so services can define their own schemas without
  kernel/daemon changes.
- New, because LazyOS starts from a capability system instead of decades of
  ambient-trust compatibility: **no ambient filesystem access to config at
  all** — everything is mediated by Messenger calls against handles, and every
  mutation is versioned so `regd` itself becomes the audit log.

---

## 2. Design stance

| Concern | Choice | Why |
|---|---|---|
| Mediation point | **Userspace service (`regd`) over Messenger, backed by ext2** | Keeps the kernel mechanism-only (per platform-plan §1.6); `regd` can evolve schemas/compaction without kernel changes. |
| Namespace | **Hierarchical paths**, e.g. `sys/net/if/eth0`, `user/<uid>/shell/prefs` | Familiar (Registry-like), sorts/prefixes naturally for ACLs and enumeration. |
| Data model | **Schema'd records** (named, typed fields), not raw blobs | ODM-style: self-describing, diffable, validated on write. |
| Versioning | **Append-only generations per key**, current pointer + history | Every write is atomic and reversible; matches "versioned" requirement directly. |
| Durability | **Write-ahead log + snapshot on ext2**, fsync on commit | Config loss on crash is unacceptable; small enough dataset that WAL is cheap. |
| Access | **Messenger interface only** (`os.lazy.regd.v1`), no direct file access | Matches "reachable using Messenger"; lets the kernel/`messengerd` stamp identity and enforce ACLs per path. |
| Change notification | **Pub/sub topics** mirroring the path tree | Services react to config changes (Registry has no native equivalent; this is closer to `gconf`/`dconf`/systemd `PropertiesChanged`, adapted to Messenger). |
| Security | **Per-path ACLs + kernel-stamped identity**, default-deny like the rest of Messenger | Consistent with `security-model.md`; a sandboxed app cannot read another app's or another user's subtree unless granted. |

---

## 3. Data model

### 3.1 Schemas ("object classes", ODM-style)

A schema declares a record type once; `regd` validates every write against it.

```
schema net.lazy.registry.schema.v1 "sys.net.iface" {
  1: string   name;          // "eth0"
  2: bool     dhcp;
  3: bytes4   static_addr;   // optional, present if !dhcp
  4: u32      mtu = 1500;    // default
  5: string   driver;
}
```

- Schemas are themselves records under `sys/regd/schema/<name>` — self-hosting,
  like ODM's `PdAt`/`PdDv` predefined classes, but introspectable through the
  same API used for data.
- Field IDs are stable and append-only (mirrors Messenger's method-id rule in
  `messenger.md` §3), so old readers can skip unknown fields and schemas can
  gain fields without breaking existing records.
- A schema can mark fields `required`, give a `default`, and reference an enum
  (backed by Messenger's existing TLV primitive types).

### 3.2 Keys, records, and generations

- A **key** is a path (`sys/net/if/eth0`) plus a schema id. A key holds a
  linked list of **generations**: `(gen: u64, writer: Credentials, ts, parent_gen,
  record_bytes, comment)`.
- **Current** always points at the latest committed generation; reads default
  to current but can pin an explicit `gen`.
- A generation is immutable once committed — "editing" a key creates
  `gen+1`; this gives versioning for free and makes `regd` double as the
  config audit trail required by `security-model.md`'s auditing goals.
- **Compaction**: a background policy (configurable per subtree, e.g.
  "keep last 32 generations or 30 days") folds old generations into a
  snapshot so history doesn't grow unbounded; compaction itself is a logged
  operation, never silent data loss of the *current* value.

### 3.3 Storage layout on ext2

```
/system/regd/
  wal/                 -- write-ahead log segments, replayed on boot
  objects/<hash2>/<key-hash>.rec   -- one file per key, log of generations (append-only)
  schema/<name>.schema             -- compiled schema blobs
  snapshot.idx                     -- path -> object file + current-gen index, rebuilt from WAL if stale
```

`regd` owns this directory exclusively; nothing else is granted a handle to
it, so the *only* way to reach configuration is the Messenger interface —
enforced the same way `xuid` is the only process with grants to the display
hardware.

---

## 4. Messenger interface

Interface `os.lazy.regd.v1`, exposed as one root object obtained from
`messengerd` by service name `os.lazy.regd`.

```
interface os.lazy.regd.v1 {
  // CRUD, schema-validated
  Get(path, gen: optional<u64>) -> (record, gen, writer_cred, ts);
  Put(path, schema_id, record, expected_gen: optional<u64>, comment) -> (new_gen);
  Delete(path, expected_gen: optional<u64>) -> (tombstone_gen);

  // Enumeration / query (ODM-style predicate query)
  List(path_prefix, recursive: bool) -> (stream of path);
  Query(path_prefix, schema_id, predicate) -> (stream of (path, record));

  // History
  History(path, limit) -> (stream of (gen, writer_cred, ts, comment));
  Revert(path, to_gen) -> (new_gen);           // creates a new gen copying an old one

  // Schema management
  RegisterSchema(schema_def) -> (schema_id);
  GetSchema(schema_id | name) -> (schema_def);

  // ACL management (delegated, not ambient)
  SetAcl(path, acl) -> ();     // caller must hold MANAGE right on path
  GetAcl(path) -> (acl);
}

// Pub/sub, mirrors the path tree as Messenger topics:
//   topic "regd/changed/<path>"            -- retained, latest generation summary
//   topic "regd/changed/<subtree>/#"       -- wildcard subscribe to a whole subtree
// payload: (path, old_gen, new_gen, writer_cred, ts)
```

- `expected_gen` gives **optimistic concurrency** (compare-and-swap), so two
  services racing to update `sys/net/if/eth0` don't silently clobber each
  other — `Put` fails with `REGD_CONFLICT` and the caller re-reads.
- `Query` reuses Messenger's existing parcel/TLV machinery for the predicate
  (field == value, range, prefix match on string fields) — no new query
  language, just structured filters over typed fields, matching ODM's
  `SQL`-lite `odmget -q` without inventing SQL.
- Everything is a normal Messenger transaction, so it inherits deadlines,
  cancellation, and the existing introspection (`ListServices`,
  `GetInterface`) for free — a `regctl` CLI is just another Messenger client.

---

## 5. Security model integration

Following `security-model.md`'s default-deny stance:

- Every path carries an **ACL**: `{owner, group, world} x {READ, WRITE,
  MANAGE, WATCH}`, checked against the kernel-stamped `uid/gid/label` on each
  call — the same credential struct Messenger already stamps, no new identity
  mechanism.
- Well-known subtrees get fixed default ACLs at first boot (seeded by
  `regd`'s own schema, so the policy is inspectable, not hardcoded logic):
  - `sys/**` — root-writable, world-readable (device/driver/network config).
  - `user/<uid>/**` — that user read/write, root read (per-user prefs, like
    `HKCU`).
  - `secrets/**` — **not stored in `regd` at all**; delegate to `keyd`
    (per `messenger.md` §1's "secrets never in the kernel [or general
    config store]" stance) and store only a reference/handle in `regd`.
- `regd` itself runs as an unprivileged service holding only the ext2
  subtree grant it needs — it has no more ambient power than any other
  service; its authority is entirely "the process that `messengerd` resolves
  `os.lazy.regd` to."
- All mutations are attributed (`writer_cred` on every generation) and
  streamed to `auditd` via the normal `regd/changed/#` topic — configuration
  changes become part of the system audit log automatically, not a
  bolt-on.

---

## 6. Failure modes and guarantees

- **Crash during write:** WAL replay on `regd` startup either completes or
  discards the in-flight write; `Get` never observes a torn record — same
  durability bar as the ext2 journal work in `platform-plan.md`.
- **Two writers race:** resolved via `expected_gen` CAS; no lost updates.
- **Schema evolution:** additive-only at the wire level (new optional fields);
  a breaking change ships as a new schema name/version (`sys.net.iface.v2`)
  and `regd` can host both while services migrate — mirrors how Messenger
  interfaces are versioned (`messenger.md` §3).
- **regd itself crashes/restarts:** stateless from the client's point of view
  — handles are Messenger handles, not regd-internal state; a client just
  reconnects and re-resolves `os.lazy.regd` via `messengerd`, exactly like
  any other service restart.

---

## 7. Staged rollout

Fits after Messenger core + writable ext2/VFS land (see `platform-plan.md`
§5 stages); proposed as its own stage, call it **S-regd**, sequenced once a
sync Messenger call and a writable filesystem both exist:

1. **Bootstrap:** `regd` skeleton service, in-memory only, `Get`/`Put`/`List`
   over Messenger, no persistence, no ACLs (dev-mode). Proves the interface
   shape.
2. **Persistence:** WAL + object files on ext2, generations, `History`/
   `Revert`. Kernel test suite gains a `regd`-adjacent correctness + soak
   suite per AGENTS.md's testing requirement (many rapid `Put`s/generations,
   crash-and-replay simulation, concurrent CAS races).
3. **Schemas:** `RegisterSchema`/`GetSchema`, validation on `Put`, `Query`
   predicates.
4. **ACLs + audit:** per-path ACLs, kernel-credential enforcement, `auditd`
   topic wiring, secrets delegation to `keyd`.
5. **Migration:** move existing ad hoc config (network config, xuid/display
   settings, accounts prefs) from whatever flat files they use today onto
   `regd`, seeding default schemas/ACLs; add `regctl` CLI for interactive
   inspection (`regctl get`, `regctl history`, `regctl watch`).
6. **Compaction + quotas:** background generation pruning policy, per-user
   storage quotas, `regd` health/metrics exposed the same way `healthd`
   expects (per `platform-plan.md` §4's observability pillar).

Steps 2–4 each need both correctness and stress tests before being called
done, per AGENTS.md's kernel-component testing requirement, even though
`regd` is a userspace service — the same soak-testing discipline (many
generations, concurrent writers, WAL replay under induced crashes) applies
because config corruption is a whole-system failure.

---

## 8. Open questions

- Should `List`/`Query` support cross-subtree joins (ODM's multi-class
  queries), or is a single-subtree predicate scan enough for LazyOS's scale?
  Leaning toward **no joins** initially — keep `regd` simple and let callers
  compose.
- Do per-user hives get a separate object store file (for cheap per-user
  backup/restore, closer to how Windows hives are separate files) or share
  one global object store keyed by path? Leaning toward **separate store
  per top-level subtree** (`sys`, `user/<uid>`, ...) so a user's hive can be
  backed up/restored independently.
- Should schema definitions live in `regd` itself (self-hosting, as drafted
  above) or be compiled into services at build time like Messenger
  interfaces are? Self-hosting wins for runtime introspection (`regctl
  schema sys.net.iface`) but needs care that `regd` can bootstrap its own
  schema-of-schemas before any service registers one.
