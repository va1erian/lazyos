# Core audit: kernel, system services, Messenger (2026-10-18)

Scope: the kernel (`kernel/src`, tests excluded), Messenger (kernel IPC,
`libs/messenger`, `midlc`'s generated decoders), the userspace runtime
(`user/src`, `libs/lazyos-sys`) and the system services in `user/src/bin`.
Focus: correctness, code quality, good practice. The audit is read-only:
nothing was built or run, so every finding comes from reading the code at
`0c90f6a8a`.

Ten reviewers each took one area. Each finding gives `file:line`, the defect
and a concrete failure. **V** marks a High finding that a second pass
re-checked against the code. Findings marked PLAUSIBLE need a test to
confirm.

## Status (2026-10-19)

Every High finding is fixed and merged. The fixes went through a second
review round, and the combined tree passed the full kernel suite
(1607/0) before merging.

| Findings | PR |
|---|---|
| H1, M-K2 | #907 |
| H3, H4, M-M3 | #909 |
| H2, H8, H9, M-K1, M-R3 | #910 |
| H5, H13, M-T1 | #911 |
| H7 | #912 |
| H6 | #913 |
| H10, H11, H12, M-R4 | #914 |

Everything else is still open and tracked as a checklist in **#916**
(section 9 lists it):

- the rest of patterns P1 and P2;
- the Medium and Low findings below;
- the follow-ups from reviewing the fix PRs;
- the SMP (S8) items.

Sections 2–8 are the audit as written at `0c90f6a8a`. Their `file:line`
references are as of that commit, and the High table describes the code
before the fixes.

## 1. Summary

The code is careful in the places that were designed with an attacker in
mind:

- the ELF loader;
- `rt_sigreturn` and `sysret` hardening;
- `ParcelView::parse` and the check that a message carries exactly its
  declared objects;
- the `lazypkg` archive checks;
- the elevd, accountsd, keyd and confd authorization model (all keyed on
  the kernel-stamped caller; no uid-0 shortcut);
- user-pointer validation;
- FUSE and partition-table parsing.

Most of the defects sit **between** those defended pieces. They fall into
five recurring patterns (section 2), and fixing each pattern once closes
several findings:

1. **A task slot is used as an identity.** Slots are reused at once
   (lowest free slot), and several authorization checks rest on them.
2. **Small global tables, with leak paths.** 64 channels, 64 pipes,
   64 registry names, 64 subscriptions, 16 clipboard sessions, 8 mounts.
   Each has a path where an unprivileged task fills it permanently.
3. **Teardown paths that skip the safe primitive.** `kill_process` is the
   only exit path that respects threads parked inside the kernel.
4. **Userspace lengths walked with interrupts off.** `munmap` and
   `mprotect` walk a 2^52-page range; unchecked `ptr + 8` panics the kernel.
5. **Receive loops that treat a per-message error as fatal.** One 17 KiB
   parcel kills `accountsd`, `messengerd` (restarted once only) and `init`.

## 2. Cross-cutting patterns

### P1. Task slot as identity

The kernel's own `docs/architecture/ipc-security.md` says `Message::sender`
is "an address, not an identity". The code still authorizes on it, or on
other raw slot numbers, in these places:

| Where | What a slot reuser gains |
|---|---|
| `kernel/src/ipc/syscalls.rs:279` `op_reply` (no check at all, see H1) | answers any pending call |
| `kernel/src/ipc/inet/pump.rs:67` `is_netd`, never cleared at teardown | reads and injects every app's socket data |
| `user/src/bin/xuid` surface owner = `message.sender` (`request.rs:249`, `present.rs:98`) | a dead app's window and its keyboard focus |
| `user/src/bin/inputd/hub.rs:109` compositor = `shell.sender` | if xuid crashes: registering surfaces and taking focus (greeter keystrokes) |
| `user/src/bin/init/{sessions.rs:149,drivers.rs:88,notice.rs:136,lifecycle.rs:151}` | forged logind/devd events in a narrow window |
| `kernel/src/ipc/syscalls/regops.rs:13` proxy ops by client slot | register/resolve into the wrong task's handle table |
| `kernel/src/ipc/shared.rs:261` per-process buffer accounting | another task's `shared_buffer_max` credit |
| `user/src/bin/audiod/server.rs:89`, `user/src/messenger/registry.rs:272` | the same |

**Fix once:** give tasks a generation (pid = slot plus generation, never
reused within a boot). Stamp it in the kernel's caller stamp. Make every
"is this the same peer" check compare `(slot, gen)`. Clear per-slot kernel
roles (`netd`, doorbell owner) in `ipc::teardown_task`.

### P2. Global tables that an unprivileged task can fill permanently

| Table | Size | Leak path | Ref |
|---|---|---|---|
| Messenger channels | 64 | a channel end moved into its own inbox is never reclaimed | H8 |
| Pipes / socket pairs | 64 | an `SCM_RIGHTS` cycle through epoll or a listener | H7 |
| Registry names | 64 | `router::Bus::subscribe` failure leaves a name behind | M-R4 |
| messengerd subscriptions | 64 | a dead owner's subscriptions are never collected | M-S1 |
| init handle table | 256 | router broker re-resolves on each publish to a stalled sink | H12 |
| clipboardd sessions | 16 | sessions are never forgotten, so the 17th login breaks copy and paste | M-S4 |
| mountd mounts | 8 (global) | failed entries kept, no per-uid cap | L |
| MSI vectors | 32 | re-arm after a disarm routes a fresh vector | L |
| Kernel heap | n/a | terminal transactions not awaited keep up to 1 MiB each | H9 |

**Fix once:** every global table needs (a) a per-uid or per-owner share,
(b) collection of a dead owner's rows, and (c) a kernel stress test that
loops the leak path more times than the table holds. AGENTS.md already asks
for the stress half; none of these paths has one.

### P3. One safe group-exit primitive, three unsafe ones

`kill_process` (`kernel/src/task/signal/control.rs:308`) documents why a
thread parked in the kernel must not be marked Done. `exit_thread_group`,
`terminate_process` and `kill_group` do it anyway (H5). Collapse them into
one.

### P4. Unbounded user lengths and raw pointer arithmetic

There is no field-access helper in `user_ptr`, so handlers write
`ptr + 8`, which overflows (H3). There is no `MMAP_LIMIT` bound on
`munmap`/`mprotect`/`madvise`/`mremap` (H4, M-M3). Add
`user_ptr::try_read_field(base, off)` / `try_write_field` using
`checked_add`, and add a single "valid user range" constructor that every
memory syscall goes through.

### P5. Receive errors

`op_recv` dequeues the message and installs its objects, then returns
`E2BIG`. Every server loop treats that as fatal. The fix needs both halves:

- **Kernel:** check `buf_cap` and `buf_ptr` before dequeuing. Roll back, or
  fail the transaction with `PeerDied`, on any delivery or copy-out failure.
- **Runtime:** a `recv` result that separates per-message failures from a
  dead endpoint, used by `Server::serve` and `Mailbox::run`.
  `wifi/server.rs:64` already does this by hand.

## 3. High

| # | Finding | Where | Status |
|---|---|---|---|
| H1 | **Any task can forge the reply to any pending Messenger call.** `op_reply` passes the user's txn id straight to `reply_owned`, with no check that the replier received the request and no `authorize`. Ids are `(global seq << 6) \| slot`, so they are guessable: learn the sequence from one call of your own, then spray. The await side was already hardened against guessable ids (`txn.rs:44`). Targets: the greeter↔accountsd password check, elevd, confd `Get`. **Fix:** record the delivering side or task in `Transaction` at `take_locked`; accept a reply only from it; add a kernel test. | `kernel/src/ipc/syscalls.rs:279`, `channels/call.rs:229`, `channels/registry.rs:29` | CONFIRMED **V** (two reviewers independently) |
| H2 | **One oversized parcel kills core services and leaks the objects it carried.** The kernel dequeues and installs objects, then returns `E2BIG` (16 KiB `DEFAULT_BUFFER` against a 1 MiB `MAX_PARCEL_BYTES`). `accountsd`, `elevd`, `keyd`, `confd`, `messengerd`, `logind` and `init` all `return Err` on it. Effects: `messengerd` is `Restart::Once`, so pub/sub is gone for the boot; `accountsd` restarts with an empty guessing limiter; PID 1 exits. **Fix:** P5. | `kernel/src/ipc/syscalls.rs:302`; `user/src/bin/accountsd.rs:135`, `messengerd.rs:156`, `init.rs:311`, `elevd.rs:150`; `user/src/messenger/endpoint.rs:218` | CONFIRMED **V** (three reviewers) |
| H3 | **Any Linux task, including the confined WebProcess, can panic (halt) the kernel** with an out-of-range pointer: `clock_gettime(CLOCK_MONOTONIC, (void*)-1)` evaluates `out + 8` with overflow checks on (dev profile, the default build). Other sites: `gettimeofday`, `nanosleep`, `pipe2`, `socketpair`, futex timeout, `pselect`/`ppoll`, `epoll_ctl`, `setitimer`, `prlimit64`, `set_robust_list` (panics later, at thread exit), `display/present.rs:148`. **Fix:** P4. | `kernel/src/process/linux/time.rs:78`, `pipes.rs:45,88`, `futex.rs:106`, `select.rs:29,245`, `epoll.rs:104`, `timers.rs:81`, `resources.rs:77`, `procattr.rs:139` | CONFIRMED **V** |
| H4 | **A huge `munmap`/`mprotect` length freezes the machine.** No `end` bound; `unmap_range` and `protect_range` step 4 KiB at a time without skipping absent upper-level tables, with IF=0 (`protect_range` has no poll point at all). A `PROT_NONE` `MAP_FIXED` reservation is free, so `mmap(..., 0x6000_0000_0000, PROT_NONE, MAP_FIXED, ...)` then `munmap` walks about 25 G pages. Past 2^48 the index math wraps and `VirtAddr::new` panics. **Fix:** bound by `MMAP_LIMIT`; walk with a skip-absent iterator (`resident.rs::count_below` already does). | `kernel/src/process/linux/mem.rs:136,211`; `kernel/src/mem/uspace.rs:333`; `kernel/src/mem/protect.rs:130` | CONFIRMED **V** |
| H5 | **`exit_group`, user faults, OOM kills and the hardening SIGSEGV end sibling threads parked inside the kernel.** A sibling holding the ext2 `YieldMutex` across `block::iowait` is marked Done with the lock held, and every later filesystem access by anyone parks forever. Their wait-queue entries also outlive them, so a reused slot absorbs a `notify_one` (lost wakeup). Reachable by any user: a multithreaded program whose main thread exits or segfaults while a worker reads. **Fix:** P3. | `kernel/src/task/lifecycle.rs:37`, `task/signal/control.rs:390`; callers `arch/fault.rs:88`, `mem/oom.rs:57`, `signal/harden.rs:161` | CONFIRMED **V** |
| H6 | **`/tmp` and `/transient` are one ramfs mounted twice, but the VFS caches and the open-file registry treat them as different.** The dentry and inode caches are keyed by mount index, and ramfs's `cache_deadline` is `None` (cached forever). A `chmod 600 /tmp/x` leaves `/transient/x` cached as 0644, so another user can open it. The open-file registry is keyed by path string, and ramfs `OpenFile`s do root-path I/O by name, so an fd can follow a recycled name into another user's file. **Fix:** mount the ramfs once (or canonicalize `/tmp` to `/transient` before resolution); give ramfs nodes (`open_node`). | `kernel/src/fs/mounts.rs:245-254`, `fs/vfs/cache.rs:66-140`, `fs/openfile.rs:203-289` | CONFIRMED **V** (mount and cache keys); exploitation PLAUSIBLE |
| H7 | **An `SCM_RIGHTS` cycle through an epoll or a listener leaks socket pairs forever.** `would_cycle` only looks at `Fd::Socket`, but an epoll holds `Fd` clones and a listener holds pending server ends. Example: `socketpair(p0,p1)`; `epoll_ctl(E, ADD, p0)`; `sendmsg(p1, [E])`; close all. Each round costs 2 of the 64 global pipes, so after 32 rounds no one can `pipe()` or `socketpair()` until reboot. **Fix:** treat epoll and listener as containers in `would_cycle`, or refuse sending them. | `kernel/src/ipc/pipe/rights.rs:114`, `process/linux/scm.rs:17` | CONFIRMED **V** |
| H8 | **A channel end moved into its own inbox is never reclaimed**, and there are only 64 channel slots. Create a pair, send a1 over a0 (any method declaring one `Channel<I>`), close a0. Side 1 has no handle and is never closed. About 64 rounds and every `create`/`connect`/IRQ channel fails with `RegistryFull`. Two-channel cycles work too. **Fix:** an orphan/cycle pass on the surviving side at close, or refuse moving an end into its own channel; a per-uid channel quota. | `kernel/src/ipc/channels/close.rs:85-167`, `channels.rs:116` | CONFIRMED (reviewer) |
| H9 | **Uncollected terminal transactions grow the kernel heap without bound or charge.** `MAX_OUTSTANDING` and `MAX_PENDING_PER_SENDER` count only `Pending`. `Replied` entries keep their reply (up to 1 MiB) until awaited. Loop: call_begin, recv, 1 MiB reply, never await. The runtime does this itself: dropping an in-flight `messenger_async::Call` only untracks it (`user/src/messenger_async/future.rs:130`). **Fix:** count terminal-uncollected transactions in the per-sender cap; `Drop` cancels and consumes. | `kernel/src/ipc/channels/call.rs:117`, `channels/txn.rs:21` | CONFIRMED (reviewer) |
| H10 | **A package one user installs autostarts in every user's session, including the admin's.** `take_opened` queues every installed `autostart` app for every session, while the module docs (lines 9-12) promise "runs as that user". There is no installer uid in the `Installed` row. **Fix:** record the installer uid; autostart only in that uid's sessions (or require `elevd` for `autostart`). | `user/src/bin/init/autostart.rs:160`, `user/src/bin/pkgd/install.rs:100` | CONFIRMED **V** |
| H11 | **`mimed` `Register` has no authorization, while `Unregister` requires a system caller.** Any session can rebind any `(mime, verb)` globally, so admin opening a PDF launches the attacker's chosen app as admin. Registrations are also unbounded (heap DoS). **Fix:** `caller_is_system` on `Register`; scope rows to the installer uid. | `user/src/bin/mimed/handlers.rs:66` | CONFIRMED **V** |
| H12 | **The interim topic router in `init` leaks one or two handles per publish to a stalled subscriber.** `Endpoint` is `Copy` with no `Drop`; `deliver` overwrites or discards the cached and re-resolved endpoint without `release`. About 200 events after a sink stops draining, PID 1 cannot resolve names or launch apps. **Fix:** release stale and failed endpoints; on `EAGAIN`, count a drop instead of re-resolving; cap subscribers. | `user/src/messenger/router.rs:279-297`, `user/src/messenger/endpoint.rs:24` | CONFIRMED **V** |
| H13 | **Signal frames are written to user memory from the scheduler ISR with `TASKS` held.** The write can demand-page a file mapping (`sigaltstack` in an untouched `MAP_SHARED` file), which needs the ext2 `YieldMutex`. Under `TASKS`, `relax` spins, and the holder can never run: an interrupts-off hang. **Fix:** deliver on the return-to-user path in task context, never in the ISR; at minimum, require a resident frame and defer otherwise. | `kernel/src/task/schedule.rs:111`, `signal/sweep.rs:153,225`, `signal/frames.rs:183` | PLAUSIBLE |

## 4. Medium

### Messenger (kernel)

- **M-K1.** A failed delivery (receiver out of handles, quota or slots; or
  `E2BIG`/`EFAULT` in `op_recv`) discards the request but leaves its
  transaction `Pending`. A caller with no deadline blocks forever, and the
  installed objects leak with numbers the receiver never learns. The comment
  at `syscalls.rs:263` claims the opposite. `channels/recv.rs:38-52`,
  `syscalls.rs:279-283`.
- **M-K2.** A request received by a worker thread that then dies stays
  `Pending` while other handles keep the side open. Nothing records who is
  serving it. Fix: the same `served_by` field as H1; end its transactions in
  `forget_task`. `ipc/mod.rs:56`, `channels/close.rs:70`.
- **M-K3.** Session users can claim free `os.lazy.*` names, for example
  `os.lazy.accounts` during a `service.restart`. No uid ACL is ever loaded,
  so `uid_verdict` is always `BOOTSTRAP_ALLOW`. This contradicts
  `docs/security-model.md:35`. Fix: refuse `os.lazy.*` to unlabelled tasks
  with `session != 0`. `kernel/src/ipc/policy.rs:181`.
- **M-K4.** The shared-buffer VA cursor is global, with no upper bound.
  Past `1<<47`, `VirtAddr::new` panics. `ipc/shared_va.rs:42`. PLAUSIBLE.

### Memory

- **M-M1.** `mprotect(PROT_NONE)` only clears W and sets NX, so resident
  pages stay readable (guard pages, GC decommit), and the kernel's validator
  accepts them too. `kernel/src/mem/protect.rs:63` (`apply_prot`). Verified.
- **M-M2.** A `brk` shrink releases quota for the whole range whatever VMAs
  remain. `munmap` the heap, then shrink `brk`, and the quota is released
  twice, so it can be bypassed indefinitely. `process/linux/mem.rs:283`,
  `process/mod.rs:305`.
- **M-M3.** `unmap_range` frees MMIO leaves (`free_frame(device phys)`,
  which panics through `debug_assert!` in dev). The comments in `pte.rs:311`
  and `mmio.rs:3` say it skips them. Reachable by a driver through a long
  `munmap`, or through an `mremap` grow that is not bounded by `MMAP_LIMIT`
  (`mremap.rs:99`).
- **M-M4.** `teardown::release_exited` takes FRAMES, QUOTAS and the buffer
  registry with IF=1 from the mux, so a tick inside FRAMES hangs the next
  `alloc_frame` (the #382 shape). `dev/intx/service.rs:40`.

### Tasks and signals

- **M-T1.** `stop_process` overwrites every thread's wait with
  `Blocked{Signal, None}`. A deadline-only park (iowait, lock contender)
  keeps its lock until `SIGCONT`: Ctrl-Z on a `cat` mid-read freezes the
  filesystem. Separately, `wake_task_with` resumes a stopped thread on any
  wake. `task/signal/control.rs:427`, `waiting.rs:79`.
- **M-T2.** A signal frame that cannot be written (read-only or unmapped
  stack) is silently dropped and the handler runs anyway. Linux delivers
  `SIGSEGV`. `task/signal/frames.rs:81-104`.
- **M-T3.** The per-uid fd quota drifts in `CLONE_FILES` groups: only the
  opener is charged, and its exit refunds everything. `task/fdtable.rs:83`,
  `fdshare.rs:56`.
- **M-T4.** `close_exited_fds`, documented "must run in task context", runs
  from the tick ISR through `finish_sweep`. `task/signal/sweep.rs:266`.

### Processes

- **M-P1.** `execve` silently truncates argv/envp (256 entries, 4096 bytes,
  with no NUL), and bad pointers read as empty strings. `xargs` loses
  arguments while reporting success. Return `E2BIG`/`EFAULT`.
  `process/linux/procctl.rs:159,176`.

### Filesystem

- **M-F1.** `rmdir` moves parked (unlinked but open) files to
  `<mount root>/.unlinked-N` as root *before* the permission check. A file
  from a 0700 home is then readable at `/` (0755), and anyone can trigger the
  moves on someone else's directory. `fs/openfile/rmdir.rs:24`.
- **M-F2.** The native `vfs_unlink`/`vfs_rename` bypass the open-file
  registry, and ramfs's `None == None` node ids make any stale entry "the
  same file". `fs/mod.rs:272`, `fs/openfile.rs:282`.
- **M-F3.** An `OpenFile` whose name was cleared has path `""`, which parses
  to `/`. `fchmod`/`fchown` on such an fd act on the root directory, and
  `flock` keys collide. Use `Option<String>` and act on the node.
  `fs/openfile.rs:286`, `process/linux/attr.rs:174`.

### Other IPC and devices

- **M-I1.** `close()` in a forked child removes the parent's epoll interest,
  because the epoll is shared. Exec's CLOEXEC closes do the same, which kills
  a server's listener interest. `task/fdops.rs:65`.
- **M-I2.** Socket-pair edge-trigger generation is
  `read_events ^ write_events`, which cancels in lockstep request/response
  traffic, so an edge-triggered waiter can hang.
  `ipc/pipe/socketpair.rs:244`.

### Services and runtime

- **M-S1.** messengerd's subscription table (64) is global, and a dead
  owner's subscriptions are never collected: SUBSCRIBE 64 times and exit
  denies pub/sub to everyone. `user/src/bin/messengerd/broker.rs:252`.
- **M-S2.** Task-slot identity in xuid, inputd and audiod (P1).
- **M-S3.** `DragStart` accepts any held button anywhere, so a background
  app can turn the user's click in another window into a drop of its own
  token. `user/src/bin/xuid/request_shell.rs:35`.
- **M-S4.** clipboardd never forgets a session (cap 16), so copy and paste
  stop after 16 logins in one boot. `user/src/bin/clipboardd/state.rs:107`.
- **M-R1.** Client wrappers (`confd::Client`, `central::Bus`, `router::Bus`,
  `topics_client`, `registry::Client`) never release their resolved handle.
  `elevd/perform.rs:192` leaks one per elevated settings operation, and
  `audio_events.rs` leaks one each time a publish times out.
- **M-R2.** The async executor busy-spins after the first completed call:
  `WAIT_CALLS` stays ready because it never drains the completion queue.
  `user/src/messenger_async/executor.rs:63`.
- **M-R3.** Dropping an in-flight `Call` leaks the kernel transaction (see
  H9).
- **M-R4.** A failed `router::Bus::subscribe` leaves a global registry name
  and two endpoints behind, and `logins.rs` retries every 10 s.
  `user/src/messenger/router.rs:332`.

## 5. Low (abridged)

**Kernel**

- `/home` late mount can stay half done if the ABI side fails
  (`fs/late.rs:94`).
- The console tty never calls `Ldisc::admits`, so the canonical queue is
  unbounded (`task/consoletty.rs:76`).
- The `.unlinked-*` reserved name is creatable through `x/.` paths
  (`fs/hidden.rs:26`).
- A FUSE daemon that answers just under 10 s stalls every path op
  indefinitely (`fs/fuse/channel.rs:146`).
- `recv`/`wait_any` on a side that another holder closed park forever
  (`channels/recv.rs:104`).
- `WAIT_CALLS` uses a stale deadline (`recv/waitset.rs`).
- Depth caps on a shared side have no per-client share
  (`channels/call.rs:45`).
- A child label's file and `AF_INET` gate ignores the parent's narrowed
  rules after a failed reload (`ipc/confine.rs:133`).
- `check_name` traces an unvalidated name, which can forge `LABEL:DENY`
  lines (`ipc/policy.rs:231`).
- `CAP_SETUID` can restamp any live task, `init` included
  (`ipc/credentials.rs:394`).
- `power()` is gated on `CAP_SYS_ADMIN`, which every root service keeps
  (`process/power.rs:94`).
- `PT_INTERP` bypasses `noexec` (`linux/libpath.rs:50`).
- Signal numbers are truncated with `as u8`, so `kill(pid, 265)` is
  `SIGKILL` (`linux/sig.rs:224`).
- `spawnv` stdio is checked and then used across sleeps
  (`spawnv.rs:372`).
- A native exec never enters a child label (`procctl.rs:234`).
- Image memory is charged to the spawner's uid (`task/linux_spawn.rs:82`).
- `execve` forgets `SIG_IGN`, which breaks `nohup` (`task/memstate.rs:92`).
- A failed `mirror_fd` desynchronizes shared fd tables
  (`fdshare.rs:78`).
- An MSI vector leaks on re-arm (`dev/intx.rs:282`).
- A hostile PCI capability offset overflows `u8` and panics
  (`dev/msi_hw.rs:88`, `dev/bus.rs:118`).
- NMI has no IST stack (it can land on the user RSP inside the `syscall`
  window), and there are no #DB or #MC handlers (`arch/idt.rs:22`).
- The `int 0x80` gate returns kernel scratch values in rcx and r11
  (`process/gate.rs:35`).
- The eventfd counter can reach `u64::MAX` (`ipc/eventfd.rs:97`).
- Unaligned `MAP_FIXED`/`munmap`/`mprotect` addresses are rounded instead
  of refused.
- Page 0 is mappable and SMEP/SMAP are off (`loader.rs:114`,
  `linux/mem.rs:71`).
- Page tables are not reclaimed after `munmap`.

**Messenger wire and generator**

- Integer TLV fields accept over-long payloads (`libs/messenger/src/tlv.rs:256`).
- Generated decoders never apply `MAX_DEPTH` and do not refuse recursive
  value structs; none exist today (`tools/midlc/midlc_rust.py:138`).
- Decoders do not check the field kind or element id that `docs/midl.md`
  requires; extra `Option` elements are ignored.
- A repeated object field passes the `next == count` check (latent: no
  interface has two objects of the same kind).

**Services and runtime**

- messengerd's retained-topic table is global and first-come
  (`broker.rs:173`).
- logind logs the attempted user name mid-line without validation
  (`logind.rs:275`).
- logd's `Tail` is open to everyone while `TailFile` is restricted, and
  `require_root` checks uid 0 (`logd/serve.rs:30,87`).
- mountd's 8-mount global cap; `List` shows every user's servers
  (`libs/mounttable/src/table.rs:96`).
- xuid's `CreateSurface` keeps an unsanitized, unbounded title
  (`xuid/request.rs:100`).
- The FTP password goes in argv while SMB's goes in the environment
  (`libs/mounttable/src/lib.rs:236`).
- The executor's stale-id probe passes deadline 0, which means "forever"
  (`executor.rs:69`).
- The FUSE `Reply` `data_len` is not tied to `data.len()`
  (`sys/fuse.rs:252`).
- `netstd` slices with an unclamped length from netd (`netstd.rs:90`).
- `files.rs` accepts interior NULs in paths (`files.rs:55`).

## 6. Code quality and practice

- **Comments that contradict the code.**
  - `reply_owned` documents its missing check as future work (H1).
  - `pte.rs`/`mmio.rs` say `unmap_range` skips MMIO (M-M3).
  - `syscalls.rs:263` says a bad receive block never costs the message
    (M-K1).
  - `autostart.rs` says an app runs as its installer's user (H10).
  - `user_ptr.rs:163` says syscalls run to completion; they park.
  - `frames.rs:81` says a SAFETY condition holds that nothing checks (M-T2).
  - `cow.rs:49` describes restoring leaves that are not restored.
  - Stale references: `process.rs` (`MAX_TASKS` "is 16", "every task runs
    as uid 0"), `process/mod.rs:13-46` (retired spawn ops), `fs/mod.rs`
    (FAT root), the `channels.rs` "duplicate before sending" op that doesn't
    exist, and `messenger_async` "leaf futures always park".

  When a comment carries the safety argument, a wrong comment is a bug.
  Review should treat them that way.
- **SAFETY comments that name the wrong invariant.** About a dozen
  `unsafe { user_ptr::read/write }` sites say "the syscall ABI's contract".
  The real invariant is validation inside `user_ptr`. The infallible
  `user_ptr::read/write` also hide `EFAULT`: retire them in favour of the
  `try_*` forms.
- **Duplicated low-level code.**
  - About eight hand-written page-table walkers, and PTE bit constants
    redefined in five files. One walker with a skip-absent iterator would
    also fix H4.
  - Four group-termination primitives (P3).
  - User-runtime `MsgArgs` built by hand in about ten modules instead of
    the safe wrappers `lazyos_sys::msg` already has; three `encode` copies
    and two `error_field` copies.
  - `os.lazy.*` name reservations split across three branches of
    `policy.rs`.
  - Two child-label start gates (`children::may_start`,
    `confine::may_spawn`).
- **Error mapping.**
  - `E::Io` maps to `EINVAL`, which hides failing disks.
  - `read_parcel` returns `E2BIG` for an empty parcel.
  - `shared::retain` returns `NotFound` for a refcount ceiling.
  - fork reports a full task table as `ENOMEM` (Linux: `EAGAIN`).
  - `F_DUPFD` past the limit returns `EMFILE` (Linux: `EINVAL`).
- **Interrupt-masking discipline is implicit.** `heap.rs` masks around its
  lock; FRAMES, `vma::SPACES`, QUOTAS and SLAB assume the caller did. A
  `debug_assert!(!interrupts_enabled())` in those lock helpers would have
  caught M-M4.
- **`debug_assert!(false)` on attacker-reachable accounting paths**
  (`frames.rs:205,210,242`, slab) turns a bookkeeping error into a dev-build
  panic. Count and report instead.
- **Trace rule violations.**
  - `xuid/reap.rs:63` prints a marker with `sys::write_str(format!(...))`,
    which the ratchet's literal scan misses.
  - `logind` puts free text mid-line.
  - `policy.rs` traces unvalidated names.
- **Mojibake** in `user/src/messenger/endpoint.rs:103` and
  `messenger/mod.rs`.
- **Good:**
  - Every file in scope is under 500 lines (`registry.rs` 487 and
    `syscalls.rs` 475 are close).
  - Every service protocol looked at is MIDL-generated.
  - No `unwrap` on untrusted input in the services.
  - `fhs` paths are used throughout.

## 7. Test gaps

Each High finding has a natural kernel test, and none exists:

- `reply` from a task that is not the receiver must fail.
- A loop of the H7/H8 leak paths, run more times than the table holds,
  must leave the table empty.
- A thread group exits while a sibling sits in `iowait`, and then the
  filesystem must still work.
- `munmap(addr, 2^63)` must return quickly.
- `clock_gettime(.., -1)` must return `EFAULT`.
- A parcel larger than the server's buffer must not kill the server.
- `PROT_NONE` on a resident page must fault on read.

The "stress test for every kernel component" rule in AGENTS.md would have
caught most of section 2.

## 8. Suggested order

1. **H1** (reply forgery). It is small, local, and breaks the Messenger
   trust model.
2. **H3 + H4** (user-triggered kernel panic or hang). Both are mechanical:
   a field helper and a range bound.
3. **H2 / P5** (receive errors). A kernel and runtime pair of changes
   fixes every service at once.
4. **H5 + M-T1** (one safe group-exit and stop primitive).
5. **P1** (task generations). One kernel change, then mechanical service
   updates.
6. **H6, H7, H8, H9, H12** and the rest of P2, each with a stress test.
7. **H10, H11** (cross-user policy in init and mimed).
8. The Mediums, then the Lows.

## 9. Remaining items

Tracked in #916, one checkbox per item; check an item off there when its fix
merges. In summary:

- **P1 (task slot as identity):** everything except the reply check (#907).
  - the kernel's `netd` role, never cleared at teardown;
  - xuid, inputd and audiod ownership;
  - init's trusted-sender checks;
  - registry proxy ops;
  - shared-buffer accounting.
- **P2 (fillable global tables):** messengerd subscriptions and the
  retained-topic table, clipboardd sessions, mountd mounts, MSI vectors.
- **Medium:**
  - M-K3, M-K4;
  - M-M1, M-M2, M-M4;
  - M-T2, M-T3, M-T4;
  - M-P1;
  - M-F1, M-F2, M-F3;
  - M-I1, M-I2;
  - M-S3, M-R1, M-R2.
- **Low:** everything in section 5, plus a cross-mount rename that returns
  `ENOSYS` instead of `EXDEV`.
- **Follow-ups from the fix reviews:**
  - #907: `execve` keeps received-but-unanswered calls;
  - #911: no test through the real `exit_group` path;
  - #915: EINTR after a stop and continue;
  - #914:
    - `ListApps` reports the manifest's `autostart` flag;
    - packages that the old `Keep` path promoted to core still autostart for
      everyone;
    - the `mime_register` QEMU scenario has not been run.
- **SMP (S8):** waits that park after dropping their lock (Messenger
  `await_reply`/`recv`, pty), borrowed user slices in FUSE and provider
  replies, the `carries_rights` lock order, and the per-CPU globals in
  `arch/linux.rs`.
- **Code quality:** section 6.
