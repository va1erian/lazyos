# Dyon on LazyOS — feasibility assessment

_Investigation date: 2026. Target: [dyon 0.51.3](https://crates.io/crates/dyon)._

## Question

How feasible is it to run a scripting-language interpreter based on
[Dyon](https://github.com/PistonDevelopers/dyon) on LazyOS?

## What Dyon is

A dynamically typed scripting language written in Rust. Numbers are `f64`;
values include arrays, objects, booleans, options/results, 4D vectors and
matrices; it supports functions, closures, loops, a lifetime/mutability checker,
`go`/`in` coroutines, and dynamic module loading.

### Facts (verified)

- **Crate:** `dyon` 0.51.3, MIT/Apache-2.0.
- **Runtime dependencies:** `lazy_static`, `piston_meta` (its parser), `range`,
  `read_color`, `read_token`, `vecmath`, `advancedresearch-tree_mem_sort`;
  optional `rand`, `tokio`/`reqwest`.
- **Feature flags (default on):** `debug_lookup`, `dynload`, `file`, `http`
  (pulls `reqwest`), `rand`, `stdio`, `threading`. Optional: `async`/`tokio`.
- **No `no_std` feature is advertised**, and the dependency set is
  std-oriented (`std::collections`, threads, time, I/O).

## LazyOS constraints (as of the assessment)

> The constraints below are the pre-#25 runtime this assessment was written
> against. Since then ring-3 programs have a heap (`sbrk`), a Messenger client
> library, and `std` programs run through the Linux ABI shim; option C below
> was taken (see the update at the end).

Our ring-3 user programs at the time were deliberately minimal:

- `#![no_std]`, `#![no_main]`, **no `std`, no allocator, no libc**.
- Fixed memory: code at `0x400000`, a small user stack; no heap growth.
- Syscalls: `exit`, `write`, `read_char`, `read_file` (`int 0x80`).
- No threads, no clock, no dynamic loading.

This is the crux: Dyon is a **std** crate, and we have no `std` and not even an
allocator in user space.

## Options

### A. Link `dyon` into a ring-3 program as-is — ❌ not feasible today

Dyon needs `std` (collections, I/O, time, threads). Our user programs cannot
link `std`; Rust's `std` requires an OS/target layer we do not provide. Booting
`dyon` would also pull a large dependency tree and, with default features,
`reqwest`/`http`.

### B. Port Dyon (and its deps) to `no_std` + `alloc` — ⚠️ large, upstream-dependent

Technically possible in principle (Dyon is pure Rust), but it means:

- forking/patching `dyon`, `piston_meta`, `lazy_static`, `vecmath`, …;
- replacing `std::collections::HashMap` with a core/alloc map, removing
  std I/O, threads, `time`, and float formatting;
- providing `f64` math via `libm` in `no_std`.

The upstream project does not target `no_std`; keeping a fork in sync is a
maintenance burden. Rough effort: **weeks**, mostly in dependencies, for a
language runtime we would then have to wire to syscalls.

### C. Grow our own Dyon-*inspired* interpreter in `no_std` — ✅ pragmatic

Keep the interpreter inside our no_std user runtime and adopt the parts of
Dyon's surface we want. This is an incremental continuation of `user/src/bin/sh.rs`.

**Prerequisites (ordered):**

1. **A user-space heap.** Add an allocator to user programs so we can use
   `Vec`/`String`/maps as a scripting runtime needs. This requires a
   `mem`/`sbrk`-style syscall to give the program more pages on demand, plus a
   simple bump/linked-list allocator in the program.
2. **A richer syscall surface**: `open`/`read`/`write`/`close` (so scripts can
   load `.dyon`-like files), and optionally `time`/`threads`.
3. **Float support**: `f64` numbers and math via `libm` (already a dependency of
   the kernel); Dyon numbers are `f64`, so this matters for fidelity.
4. **Collections**: arrays (`Vec`), objects/records (a small `BTreeMap`).

With (1)–(4) we can implement a Dyon-like subset: `f64` numbers, arrays,
objects, `fn`, `for`, `if`, `let`, closures, and `print`.

Rough effort for a useful subset: **days**, not weeks, because we control the
runtime and can drop the features we don't need (coroutines, namespaces,
lifetime checking, HTML colors, 4D vectors).

### D. Run Dyon on the host, not in the guest — ✅ easy but off-goal

Run `dyon` on the host (or a small service) and drive LazyOS over syscalls/QMP.
This gives real Dyon but does not run the interpreter *on* LazyOS, so it does
not meet the goal of "an interpreter based on Dyon" running in the OS.

## Recommendation

- **Short term:** extend `SH.ELF` toward a Dyon-like subset (option C), starting
  with a user-space heap + `sbrk`-style memory syscall. This is the enabling
  step for any real language.
- **Medium term:** if Dyon semantics are wanted specifically, consider vendoring
  the `piston_meta` parser grammar ideas rather than the whole runtime.
- **Not recommended now:** porting full Dyon (option B) or linking `std`; the
  cost is high and orthogonal to the OS work.

## Prerequisite of record

The single most valuable next step for *any* interpreter is **giving ring-3
programs a heap**: a memory-growth syscall plus a tiny allocator. Everything
else (Dyon-like or otherwise) builds on that.

> **Update:** implemented. Syscall 4 (`sbrk`) grows the user heap, and
> `user/src/heap.rs` is a bump allocator on top of it, so `SH.ELF` can use
> `Vec`/`String`. The interpreter in `user/src/lang/` already covers `f64`
> numbers, booleans, strings, arrays, `let`, `print`, `if`/`else`, arithmetic,
> comparisons and indexing — a Dyon-inspired subset.
