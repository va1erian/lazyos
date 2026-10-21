# Sandboxed processes: what the kernel enforces for a confined label

A package's label judges Messenger calls (`docs/architecture/ipc-security.md`).
Until issue #832 nothing judged what else a process can do: open a file, open
a socket, start a program, signal another process. An installed app's `files`
and `network` entries were consent text, and the `AF_INET` path and the VFS
never looked at a label. That is fine for an app the user runs as themselves;
it is not for a process whose job is to run what a web page says. A
**confined label** is the kernel's answer: a label whose task holds nothing
the manifest did not list.

```
UI process (lazywebplus-app) app:os.lazy.lazywebplus     ordinary app label
 |- fork + execve /system/bin/webprocess      -> app:os.lazy.lazywebplus/webprocess      confined
 `- fork + execve /system/bin/networkprocess  -> app:os.lazy.lazywebplus/networkprocess  confined
```

The package side (the `[processes.<name>]` tables, `pkgd` loading them, the
kernel stamping the child label at `execve`) is in `docs/packages.md` and
`docs/security-model.md`. This page is the enforcement side.

## What makes a label confined

`ipc::confine::confined(label)` (`kernel/src/ipc/confine.rs`):

* the label's rules carry the marker rule `filepolicy::confine_id()`, which
  `pkgstore` compiles every sandboxed table's `files` to (one rule per file
  rule, one stat-only rule per ancestor, and the marker), or
* its name has a `/` (`app:<id>/<name>`): a process label is confined **even
  with no rules loaded**, so a table that failed to load yields a process that
  can do nothing, never one that can do everything.

Once a label has held the marker it stays confined for the boot: `pkgd`
revokes an app by loading an empty rule list, and a task still running under
the label must not become free by it (`EVER_CONFINED`).

Unconfined labels, unlabelled tasks (every service) and the kernel pay one
cached lookup (a per-label snapshot, rebuilt when `acl::label_generation()`
changes) and are otherwise untouched.

## The gates

| What | Where | Answer |
|---|---|---|
| Any path: stat, open, read, write, create, unlink, rename, truncate, chmod/chown/utimes, listing, `chdir`, program load | `Vfs::check_path` and `Vfs::setattr*` (`kernel/src/fs/vfs.rs`), through `Id::label`: both mount tables, every Linux and native filesystem call | `EPERM`, audited, `LABEL:DENY ... file=<path> access=<stat|read|write>` |
| The device nodes the Linux layer fabricates before the VFS is asked (`/dev/tty`, `/dev/ptmx`, `/dev/pts/*`, data devices) | `open_path` (`process/linux/path.rs`) | the same call, same answer |
| `/proc`, `/etc/passwd`, `/etc/resolv.conf`, ... (fabricated) | named by path, so judged by the VFS gate first (`resolve` and `check_path` ask the VFS before they look at a fabricated entry) | same |
| `socket(AF_INET, ...)` | `inet::sys_socket` | `EPERM` unless the label holds the `os.lazy.net.socket.v1` rule (`network = ["outbound"]`) |
| `bind` to a port other than `0`, `listen` on `AF_INET` | `inet::sys_bind`, `sys_listen` | `EPERM`: `outbound` is a client (port `0` is the ephemeral port a resolver binds) |
| `bind`/`connect` of a named `AF_UNIX` socket | `socket.rs` | needs a `write` rule for the path; abstract names refused |
| `fork`, `vfork`, `clone` without `CLONE_THREAD`, `posix_spawn`'s `CLONE_VM` child, native `spawnv`, a native program `execve` starts as a child | `procctl.rs`, `native.rs`, `spawnv.rs` | `EPERM` |
| `execve` | `sys_execve` | `EPERM`: a confined image runs the one program it started with |
| `kill`, `tkill`, `tgkill`, native syscall 29, broadcast and group forms | `task::signal::send::may_signal` | only its own process (its threads); everything else `EPERM` |
| Descriptors at exec | `procctl::sys_execve` -> `confine::scrub_inherited_descriptors` | everything above fd 2 that is not a connected socket is closed, and standard input becomes `/dev/null` (a terminal's keystrokes are not the sandbox's) |
| Messenger calls, name resolution, topics | the label ACL (`ipc::policy`, unchanged) | default deny |

**Descriptors.** Audit of the browser's UI process: nothing in `webkit/port/app` opens a
file, a socket or a pipe itself; the window library's `eventfd` and `epoll` are created
`*_CLOEXEC` (`xui-app/src/backend/wakeup.rs`); an endpoint descriptor is only a pollable view
(`ipc::endpointfd`), whatever crosses; WebKit's IPC socketpair marks the *server* end
close-on-exec and leaves the client end open for the child (`ProcessLauncherLazyOS.cpp`), which
is the one descriptor meant to cross. The kernel does not rely on any of that: an image that
starts confined keeps only descriptors 0 to 2 and connected sockets.

**Baseline.** A confined task may always read the data devices (`/dev/null`,
`/dev/zero`, `/dev/full`, `/dev/random`, `/dev/urandom`; write to the first
three) and the fabricated `/proc` (machine and own-process descriptions), and
stat `/`. The list is `filepolicy::baseline`. Everything else needs a rule.

**Visibility.** A task may stat the ancestors of what it was granted (a
`realpath` walk, `access(F_OK)`), not read or list them, and sees no sibling:
`stat /home/user/.ssh` is `EPERM` while `stat /home/user/.apps` is fine under a
`$HOME/.apps/...` grant.

A walk that makes every component of a path (`mkdir -p`, WebKit's
`makeAllDirectories`, Rust's `create_dir_all`) goes through those ancestors:
`mkdir` of a name that exists is `EEXIST` for whoever may look at it, before
the question whether they may write the directory above (`Vfs::mkdir`, as on
Linux; the confined NetworkProcess could not open its own cache otherwise),
and `EPERM` for whoever may not.

**`$HOME`.** `$HOME/...` in a rule stands for `/home/<any name>/...`. That is
sound because the gate only narrows: the mode bits still apply, and a user's
home is `0700`.

## How the file gate is wired

* `vfs::Id` carries the caller's label (`Id::current()` reads it from the
  task's kernel-stamped credentials, like the uid). `Id::new(uid, gid)` is
  label `0`: the kernel acting for itself (the `/etc` backing files are read as
  `Id::ROOT` after the *Linux path* was approved).
* `Vfs::check_path` calls `confine_gate` before it looks at any node, so a
  refusal never reveals whether a path exists. `setattr`/`setattr_open` ask for
  `Write` explicitly (changing a mode needs a write grant, whatever the node's
  mode says).
* Rules are `LabelRule`s: `interface_id` is the 64-bit FNV-1a of
  `lazyos.file.<kind>:<subject>`, method any. There is no second loader and no
  second table: revoking a label drops its files with its Messenger rules.
  `libs/filepolicy` is the one place that derives them and judges a path; the
  kernel and `pkgstore` both link it.

## The audit of the Linux ABI (what a labelled task could do before)

Method: every entry of `linux_dispatch` and the native gate that names a path,
a program, a process or a socket, followed to the VFS.

| Surface | Before | Now |
|---|---|---|
| `open`/`openat`/`creat` of any path | label never consulted | gated by path (VFS) |
| `stat`/`lstat`/`fstatat`/`statx`/`access`/`faccessat`/`statfs`/`readlink` | same | gated (`Stat`; `access(R_OK)` is `Read`) |
| `mkdir`/`rmdir`/`unlink`/`rename*`/`truncate`/`chmod`/`chown`/`utime*` | same | gated (`Write` on the parent or the node) |
| `link`/`linkat` (#871) | `EPERM` (no filesystem held links) | gated: `Write` on the new name's parent, and `Write` on the existing name, so a link never reaches through a granted path what the old one refuses |
| `symlink` | `EPERM` (no filesystem holds symbolic links) | unchanged: there are none to follow |
| `chdir`/`fchdir`, relative paths, `dirfd`s, `..` | folded to an absolute path before the VFS (`cwd::resolve_at`) | the gate judges the folded path, so none of them leaves a grant |
| `execve` | read + `x` bit only | refused outright; the image is loaded before the label applies |
| native programs (`top`, `confctl`, ...) run by `execve` as a child | inherited creds and label | refused (`may_spawn`) |
| `fork`/`vfork`/`clone`/`spawnv` | child inherits the label | refused |
| `mmap` of a file | folded through the descriptor's `open` rights (`map_fd` never resolves a path); a regular file maps `MAP_PRIVATE`/`MAP_SHARED`, a snapshot descriptor (e.g. `/proc`) copies privately but `MAP_SHARED` is `ENODEV` | the gate judges the fd the caller already holds |
| `memfd_create` + `execve("/proc/self/fd/N")` / `execveat` | no `/proc/self/fd` open, `execveat` is `ENOSYS`; `execve` refused anyway | unchanged |
| `ptrace`, `process_vm_readv/writev`, `kcmp`, `mount`, `umount2`, `chroot`, `unshare`, `setns`, `io_uring_setup`, `userfaultfd`, `clone3`, `name_to_handle_at`/`open_by_handle_at` (a file named without a path), `openat2` | `ENOSYS` | unchanged, and **pinned by a test** (`confine_dangerous_calls_stay_unimplemented`): implementing one means gating it |
| `ioctl(TIOCSTI)` on an inherited terminal | not implemented | unchanged (nothing to inject with); standard input is `/dev/null` for a confined image anyway |
| `kill` of a same-uid process | allowed (uid rule) | refused outside the caller's process |
| `socket(AF_INET)` | no label check at all | needs the socket-interface rule |
| named `AF_UNIX` sockets | global name table, no check | path rule required, abstract names refused |
| `/dev/ptmx`, `/dev/pts/*`, `/dev/tty` | open | refused without a rule |
| native `read_file`, `list`, `write_file`, `mkdir`, `remove`, `rename`, `chmod`, `read_at` (syscalls 3, 15-20, 28, 30, 32) | VFS with the uid only | the same VFS gate |
| descriptors inherited across `fork`+`exec` | all that are not `FD_CLOEXEC` | closed above fd 2 unless a connected socket |
| Messenger handles | not inherited by a fork (per task slot), but they survive an `execve` | `execve` into a child label is refused (`EPERM`, `children::enter_on_exec`) while the slot holds any handle, so a task cannot become its confined helper in place carrying channels or buffers; an endpoint descriptor is only a pollable view |
| `read` of a terminal (`FdKind::Terminal`, a pty) | the launcher's keystrokes | end of file (`0`) for a confined task, whichever descriptor it was left on (`sys_read`), and the input stays queued for its owner |

## Residual gaps (stated, not hidden)

* **Plain `app:` labels are not confined.** Their `files` stay consent text and
  their `AF_INET` stays open. Turning the gate on for them needs every shipped
  manifest to list what the app really touches (a `LAZYOS_LABEL_TRACE=1` run of
  `core_apps.json` finds them) and the Linux apps that open sockets to declare
  `network`. The mechanism is ready: any label that carries the marker is
  confined.
* **Same uid.** A confined process is the user's uid. The label, not
  ownership, protects the user's files. A kernel bug in any syscall it can
  still make is the way out; there is no syscall filter beyond "not
  implemented", so the implemented surface (about 150 Linux and 30 native calls)
  is the attack surface. The native `int 0x80` gate is reachable from a Linux
  process; its Messenger half is label-gated, its filesystem half is VFS-gated,
  the rest (`quota`, `sysinfo`, `tasks`) reads state.
* **`tasks` (native 13) and `sysinfo` list other tasks' names and counts.**
  Information, not control.
* **Resources.** A confined task's memory is charged to the user's quota
  (it can use all of it) and the audit ring (128 entries) is shared, so a
  confined task that is hostile can flush older records with refusals.
* **Rule ids are hashes.** 64-bit FNV-1a of a string the package author chose;
  an attacker would need a collision with a path they do not control. A
  string-keyed table would remove the argument; it would also need a second
  loader.
* **Path-based, symlink-free.** The gate judges the folded absolute path
  because no filesystem stores symbolic links. When they arrive the resolver
  must apply the gate to each target it follows. A hard link is a second path
  to one file, so making one needs the label's write grant on the existing
  name: whatever the new path is granted, the old one granted at least that.
* **`/proc` is readable** (mount table, `cpuinfo`, `meminfo`): machine
  information, no per-process entries exist.
* **Fabricated `/etc` entries** are judged by their Linux path, so granting
  `read:/etc/resolv.conf` grants the fabricated file whatever backs it.
* **The label switch must happen after the image is loaded.** The child's
  program is read under the launcher's label; if the label were applied
  first, a confined label could not read its own image. The `execve` hooks
  (`may_exec` at the top, descriptor scrub after the commit) assume that
  order.

## Tests

`LAZYOS_TEST_FILTER=confine python tools/test/run.py --accel none` (the
suite is `kernel/src/tests/confine_suite`): the path matrix through the Linux
ABI and the native calls (with the mode bits permissive, so a refusal is the
label's), `..` and directory descriptors, the fabricated entries, `execve`,
sockets, process creation, signals, descriptors, reload and revocation,
wildcard rules, terminal reads (`confine_terminal_reads_are_eof`), the audit
trail, and a soak of thousands of allowed and refused calls and rule reloads
that must leave descriptors, frames, heap and task slots where they were. The
child-label side is `LAZYOS_TEST_FILTER=label` (`label_child_*` in
`kernel/src/tests/label_suite/children*`): binding and entering, coverage of
a child's file ids by its app (`label_child_file_rules_are_covered`), and no
entry while the slot holds handles (`label_child_not_entered_with_handles`). `cargo test -p filepolicy` covers the grammar and
the judgement against a reference matcher with seeded fuzzing
(`FUZZ_CASES=20000 cargo test -p filepolicy --release`).
