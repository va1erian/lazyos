# `regd` v1 — implementation report (issue #260)

**Branch:** `feat/regd-service-v1` · **Base:** `main` (stacked on #259/#267's
`libs/regd`, which is currently unmerged; the first two commits are that PR).

This is the implementation report for
[`docs/config-registry-plan.md`](config-registry-plan.md) v1 (§1–§5 only; v2 is
untouched). It records what was built, exactly what was run, and every place the
result deviates from the issue or could not be verified.

## What shipped

| Area | Files |
|---|---|
| Store/service core | `libs/regd/src/service.rs` (new), `libs/regd/src/lib.rs` |
| IDL + wire stubs | `idl/regd.midl` (new), `libs/generated/src/lib.rs`, `idl/manifest.json`, `docs/idl/os.lazy.regd.v1.md` |
| Client helper | `user/src/messenger/regd.rs` (new), `user/src/messenger/mod.rs`, `user/src/messenger/types.rs` |
| Service binary | `user/src/bin/regd.rs` (new) |
| CLI | `user/src/bin/regctl.rs` (new) |
| Kernel | `kernel/src/fs/{vfs,ext2,mod}.rs`, `kernel/src/process/{fsops,mod}.rs` |
| Wiring | `build.rs`, `user/Cargo.toml`, `kernel/Cargo.toml`, `user/src/bin/init/state.rs` |
| Tests | `libs/regd/tests/service.rs`, `libs/generated/tests/regd.rs`, `kernel/src/tests/regd_suite.rs`, `kernel/src/tests/ext2_suite/format_and_roundtrip.rs` |
| CI | `.github/workflows/{ci,clippy,midlc}.yml` |
| QEMU evidence | `tools/screenshot/examples/regd_demo.json` |

### Design

- **Store core** (`libs/regd/src/service.rs`): `Regd<F: StoreFs, S: ChangeSink>`
  holds the committed `Store`, its backing `StoreFs`, and a change sink. A
  mutation is applied to a **clone**, persisted, swapped in only on success,
  and then announced. A persist failure returns `REGD_IO` and leaves the live
  store and the published state unchanged. The core is Messenger- and
  syscall-free, so the host tests, the kernel suite, and the ring-3 binary all
  drive the same commit logic.
- **IDL**: `os.lazy.regd.v1` with `Get/Set/Delete/List` and a `Value` struct
  (`kind` plus `Option` payloads); `List` replies with an `Array<String>`.
- **Service** (`user/src/bin/regd.rs`): binds `StoreFs` to the VFS
  (`write store.tmp` → `fsync` → `rename store.tmp store`), takes the caller uid
  **only** from `sys::cred_get(Some(message.sender))`, and publishes committed
  `sys/` changes on `system/regd/changed/<path>` with payload `(path, deleted)`
  and never the value.
- **Storage**: `/system/regd` when it can be created and a probe write
  succeeds (`persistent=true`, health `ok`); otherwise `/tmp/regd` (ramfs) with
  a warning and health `degraded`.
- **Kernel `fsync`**: native syscall 22 → `Vfs::flush` → a new
  `Filesystem::flush` default (no-op for ramfs/overlay) overridden by ext2.
- **`regctl`**: `get/set/delete/list/watch` plus a `demo` self-test that
  `regd` spawns at boot (`demo=1`) to exercise the whole path over real
  Messenger.

## Commands run and results

| Command | Result |
|---|---|
| `cargo test -p regd` | 49 pass (clauses, codec, persist, soak, service) |
| `cargo test -p messenger-generated` | 12 pass (echo + regd round-trips) |
| `python tools/midlc/test_midlc.py` | 14 pass |
| `python tools/midlc/midlc.py --check --out libs/generated/src/lib.rs idl/echo.midl idl/regd.midl` | up to date |
| `python tools/test/run.py --accel none` | **TEST:SUMMARY:PASS=250 FAIL=0** (includes 9 `regd_*` and `fs_ext2_rename_over_existing_replaces`) |
| `cargo fmt --all -- --check` | clean |
| `cargo clippy -p kernel -p user --target x86_64-unknown-none -Zbuild-std=core,alloc -- -D warnings` | clean |
| `cargo clippy -p libmessenger -p messenger-generated -p lazyos-crypto -p font-atlas -p regd --all-targets -- -D warnings` | clean |
| `cargo build` | clean |
| `python tools/screenshot/qemu_session.py --image target/lazyos.img --out shots/regd --script tools/screenshot/examples/regd_demo.json --fail-on PANIC` | `REGCTL:SELFTEST:PASS`, `ok: true`, `REGD:CTL:EXIT` |

### Tests fail for the right reason

Broken deliberately, observed the expected failure, then reverted:

- Ignoring the `persist` error in `Regd::commit` → `regd_io_leaves_store_unchanged` failed.
- Making `announceable` always true → `regd_user_changes_are_silent` failed.
- Allowing uid 1000 to write `sys/` in `path.rs::can_write` → `access_rules_are_enforced` failed.

## Deviations, decisions, and unverified items

1. **`midlc` had a real bug, and it was fixed.** The generated `encode_*` for
   `Option`/`Array` wrote the element into `target` instead of the declared
   `nested` encoder, producing parcels the decoder could not read; `Option`
   also mapped to the nonexistent `alloc::option::Option`. Any IDL using these
   shapes (the `Value`/`List` surface here) was affected. Fixed in
   `tools/midlc/midlc.py`, with `test_midlc.py` regressions and a
   `libs/generated/tests/regd.rs` round-trip test; `midlc` now also emits
   `INTERFACE_ID`. `libs/regd`'s store logic was used as-is and not rewritten.
2. **Cross-user subscribe denial is enforced by not announcing user changes.**
   The kernel `authorize_topic` hook is `(actor, publish|subscribe, fnv1a32(segment))`
   with no per-path ownership rule, and `messengerd`'s broker has no equivalent,
   so `user/<uid>` owner-only subscriptions cannot be expressed. Per the
   issue's fallback, `service::announceable` marks only `sys/` paths; a `user/`
   write commits but publishes nothing. There is accordingly no user/ topic for
   a cross-user subscribe test to target; `regd_user_changes_are_silent` pins
   the fallback.
3. **Topic namespace.** The issue body says `regd/changed/<path>`; the
   Follow-ups say `system/regd/changed/<path>`. `system/regd/changed/` was used
   (newer guidance, and `regd` runs as root so the broker's `system/` gate
   permits it). Paths deeper than five segments exceed the broker's
   `MAX_SEGMENTS = 8`; publishing is best-effort and such changes are silently
   not announced.
4. **The soak test is single-threaded.** Kernel tests cannot execute the
   ring-3 service, so `regd_soak_sets_and_restarts` drives the shared core with
   several logical callers (`sys`, `user/1000`, `user/1001`) and periodic
   reloads, not concurrent tasks.
5. **`fsync` flushes the whole mount**, not one file (the trait carries no
   handle). No kernel test calls the `fsync` syscall directly; the ext2 test
   covers `Vfs::flush` and rename-over-existing. Linux ABI `fsync` (74) is
   unchanged (`ENOSYS`); the ABI bench was not run.
6. **ext2 rename atomicity.** The normal replacement (new contents, temp gone)
   and device flush are verified. ext2 is not journaled, so a power loss
   *during* the rename is only guaranteed to leave the old or the new
   directory entry, not a torn file; the ramfs fallback replaces under one
   lock. This is documented in the test and `regd`'s module docs.
7. **Near-limit stores.** `libs/regd` bounds logical `path + value` at 1 MiB,
   but encoded framing can exceed the VFS's 1 MiB write cap, so a maximal store
   would fail to persist with `REGD_IO` (a safe failure; the store is
   unchanged). The store limits were not changed.
8. **CI edits** (`regd` added to the host clippy lists; midlc `--check` now
   globs `idl/*.midl`) are required by the new crate and IDL.
9. **Interactive `regctl` session.** A services boot has no shell (the kernel
   spawns only `init`), and a shell-started daemon blocks `exec`, so the QEMU
   evidence uses `regd`'s `demo=1` self-test (the `clipboardd`/`sysmond`
   convention) rather than input-injected typing. The real Messenger transport,
   VFS `StoreFs`, and change publisher are exercised end to end.
