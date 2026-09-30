# Userland: runtime, shared libs & services

**What it is.** Everything that runs in ring 3: the `user` crate runtime
(syscall wrappers, allocator, interpreter, Messenger clients), the shared
`libs/`, and the service programs built from `user/src/bin/`.

**Key files**

| Path | Role |
|---|---|
| `user/src/lib.rs` | Runtime modules: `sys`, `dev`, `files`, `sysinfo`, `messenger`, `central`, `messenger_async`, `task_snapshot`, `heap` |
| `user/src/dev.rs` | Wrappers for the device syscall (23): `list`, `claim`, `map_bar`, `pio_*`, `cfg_*`, `irq_enable`/`irq_ack`, `release`, and `parse_irq` for the kernel's interrupt message (#240) |
| `user/src/sys.rs` | `int 0x80` wrappers, syscall numbers 0-14, `Cred`, display helpers |
| `user/src/sysinfo.rs`, `task_snapshot.rs` | Typed decoders for the syscall 14 system snapshot and the syscall 13 task snapshot |
| `user/src/central.rs` | Topics client that routes service publishes through `messengerd`'s central broker (#169) |
| `user/src/heap.rs` | Bump allocator over `sbrk` (see [allocators.md](allocators.md)) |
| `user/src/messenger/` | Blocking Messenger client: endpoints, registry, topics, services |
| `user/src/messenger_async.rs` | Futures, `Executor`/`block_on`, `Selector`, `service!` |
| `user/src/bin/*` | Ring-3 programs; manifest in `user/Cargo.toml` |

**Syscall wrappers** (`sys.rs`) - register convention: `rax` = number, args in
`rdi/rsi/rdx`, result in `rax`. `rcx`/`r11` are clobbered, so every wrapper
declares `clobber_abi("sysv64")`. Numbers: 0 `exit`, 1 `write`, 2 `read_char`,
3 `read_file`, 4 `sbrk`, 5 `messenger`, 6 `spawn`, 7 `wait`, 8 `clock`,
9 `service_args`, 10 `cred_set`/`cred_get`/`spawn_as`, 12 `display_*`,
13 `tasks`, 14 `system_stats`, 15-21 filesystem and `power` (`files.rs`; 11, the quota read-back, has no wrapper yet), 23 `dev_*` (`dev.rs`; it also passes arguments in `r10` and `r8`).
See [processes.md](processes.md) and [display.md](display.md).

**Blocking Messenger client** (`messenger/`)

- `Endpoint`: `call`/`call_with`, `begin_call`/`await_reply`, `reply`, `send`,
  `recv`/`recv_into`/`recv_with`, `poll_recv`, `cancel`, `close`, `stats`.
  `Message` exposes the decoded parcel plus kernel-stamped sender, `txn`, and
  delivered handle/buffer counts; `Server` is the `serve` shape.
- `MsgArgs`/`MsgResult`/`Stats`/`FabricStats` mirror the kernel ABI byte for
  byte; sizes are pinned there.
- Modules: `registry` (direct ops plus the `Client` proxy through `messengerd`
  and the daemon's `serve_request`), `topics_client`
  (`os.lazy.messenger.topics`, QoS, deferred `next_event`; wire from the
  generated `idl/topics.midl` stubs, as `registry` uses `idl/registry.midl`), `router` (interim
  userspace topic router), `services` (init/healthd/logd shapes), `keyd`,
  `accounts`, `logind`, `display` (`os.lazy.display.v1`), `mime`, `clipboard`.
- `EXPIRED_DEADLINE = 1` implements non-blocking polls via the kernel deadline
  sweep; long loops must use the `*_with` buffer variants because the bump heap
  never frees.

**Async layer** (`messenger_async.rs`, issue #91)

- `Call`/`Recv` are leaf futures over `CALL_BEGIN`/`CALL_AWAIT`; the kernel has
  no probe op, so a leaf future parks the task rather than returning `Pending`,
  while many requests can be in flight. `Selector` multiplexes (queue calls,
  one-way recv, `step`, `cancel`); `Executor`/`block_on` are the no_std
  executor; `service!` wires a mailbox service with heartbeat and shutdown.
- `async_echo`/`async_service` are example bins, not yet in the demo image.

**Shared libraries** (`libs/`)

| Crate | Contents | Tests |
|---|---|---|
| `libs/messenger` | Parcel codec: `Header`, `Parcel`, `Encoder`/`Decoder`, `BufferDesc`, limits, `Error` | `cargo test -p libmessenger` (round-trip, limits, 1M-case decode fuzz); [README](../../libs/messenger/README.md) |
| `libs/generated` | `midlc` output for every `idl/*.midl` (`os_lazy_echo_v1`, `os_lazy_messenger_registry_v1`, `os_lazy_messenger_topics_v1`, ...); also linked by the static-musl `xui-app` | `cargo test -p messenger-generated` |
| `libs/crypto` | SHA-256, HMAC-SHA256, HKDF-SHA256, Argon2id, RNG pool, wrap/unwrap, hex | `cargo test -p lazyos-crypto` (KATs); issue #102 |

**Services** (`user/src/bin/`; image names are 8.3)

| Binary | Image | Role | Started by |
|---|---|---|---|
| `init` / `messengerd` | `SUPER` / `MSGRD.ELF` | Supervisor (manifest, spawn/wait, restart backoff, app registry + `Launch`; `XAPPS.LST` decides which registered apps the image ships, and `autostart` rows open at boot as the desktop's apps, #215/#216) / bootstrap registry proxy and topics broker | kernel / `init` |
| `logd` / `healthd` | `LOGD` / `HEALTHD.ELF` | Hash-chained event log / retained `system/health/*` aggregation | `init` |
| `keyd` / `accountsd` / `logind` | `KEYD` / `ACCTD` / `LOGIND.ELF` | Secrets and crypto (#102) / accounts (#101) / console login and credentialed spawn | `init` |
| `clipboardd` / `mimed` / `flaky` | `CLIPD` / `MIMED` / `FLAKY.ELF` | Per-session clipboard (#115) / MIME and open-with (#116) / crash-test service (#93, never started by `LAZYOS_DESKTOP=1`) | `init` |
| `clipcopy` / `clippaste` / `messengerctl` | `CLIPCP` / `CLIPPS` / `MSGCTL.ELF` | Clipboard demo pair (#115, `demo=1` only) / fabric+services views (#70/#89/#93) | `clipboardd`, kernel flag |
| `sndd` / `beep` | `SNDD` / `BEEP.ELF` | virtio-sound driver serving `os.lazy.audio.v1` (`idl/audio.midl`), supervised by `init` as `_snd`; `beep [freq_hz [ms]]` is its shell command and the sound harness's client. `LAZYOS_SOUND=1` or the desktop profile ships them; see [audio.md](audio.md) | `init` / shell (`sh` native exec) or `sndd` (`demo=1`) |
| `timed` / `timectl` | `TIMED` / `TIMECTL.ELF` | Time-of-day service `os.lazy.timed.v1` (`idl/timed.midl`, #369): UTC from syscall 24, zone from `confd` `sys/time/zone` (UTC when unset, follows change notifications), retained `time/tick` topic each minute; zone/DST tables and resolution live in `libs/timed` (`timezone`). `timectl` is its command line and `demo=1` self-test | `init` (after `messengerd` and `confd`) / `timed` (`demo=1`) |
| `inputd` | `INPUTD.ELF` | Input policy service (`docs/input-plan.md`): the only holder of the kernel `input.raw` capability. Drains the raw HID-coded key bus (syscall 25), applies the compiled-in US/FR keymap (`libs/inputmap`; `confd` key `sys/input/layout`, boot default `LAZYOS_KBD_LAYOUT`), modifier/lock state, key repeat (500 ms delay, 30 ms interval, flagged `Repeat`) and hotkeys, and serves `os.lazy.input.v1` (client sessions) and `os.lazy.input.shell.v1` (the compositor: surface registration, focus, hotkeys) from `idl/input.midl`. `trace=1` (debug images) echoes `INPUTD:KEY`/`INPUTD:TEXT` to serial (`tools/input/verify_trace.py`) | `init` (after `confd`, with only `CAP_INPUT_RAW`) |
| `sysmond` / `top` | `SYSD` / `TOP.ELF` | System-stats service over syscall 14 with `system/stats/*` topics / one-shot text client (#144); services image only, `top` left out of `LAZYOS_DESKTOP=1` | `init` / `sysmond` (`demo=1`) or `init` `Launch` |
| `hello` / `xuid` / `xdemo` | `HELLO` / `XUID` / `XDEMO.ELF` | demo / compositor and display demo (#113). The system shell is BusyBox `sh` (`BUSYBOX`, a Linux-ABI binary built by `tools/abi/busybox.py`, #254) | kernel |
| `faultprobe` | `FAULTPRB.ELF` | Deliberate ring-3 faults (#7); run by hand from `sh` (`faultprobe null`, `kernel`, `priv`, `div`, `ud`) | - |
| `dragdemo` / `shellprobe` | `DRAGDMO` / `SHELLPRB.ELF` | Drag & drop evidence pair (#145) / shell-protocol evidence client (#167); `LAZYOS_XUID=1` images | kernel |
| `async_echo` / `async_service` | not on disk | `messenger_async` examples (#91) | - |

**Running native programs from `sh`** (#315): `top`, `confctl`, `msgctl`
(`messengerctl`) and `faultprobe` are reachable by name from BusyBox `sh` (console
and desktop Terminal); the kernel's `execve` runs them as a native child of the
shell's fork child. `top` is not shipped in the `LAZYOS_DESKTOP=1` image (`not
found` there). Details and limits: [processes.md](processes.md).

**The `rhai` command** (issue #319, step R0 of the
[Rhai plan](https://github.com/va1erian/lazyos/wiki/Rhai-in-LazyOS)). An
ordinary static `x86_64-unknown-linux-musl` `std` program, not part of the OS
workspace: `rhai-host/` (thin wrapper: argv, stdio, files, clock) around
`libs/rhai-lazy/` (`no_std` + `alloc`, host-tested bindings against a mock
filesystem/environment). `python tools/rhai/build.py` builds it (rust-lld
self-contained on Windows, no C compiler; it reports "unavailable" and exits 0
when the musl target cannot be installed) to `target/rhai/rhai.elf`; the root
`build.rs` (`build_support/rhai_embed.rs`) embeds it as `RHAI.ELF` when present
(`LAZYOS_RHAI` overrides; the ABI bench's `LAZYOS_INIT` skips it).

- **Usage.** `rhai -e 'expr'` (prints the value unless `()`), `rhai script.rhai
  [args]` (a `#!` first line is ignored), `rhai - [args]` (script from stdin),
  bare `rhai` (REPL). Exit status: the script's `exit(n)`, else 0, or 1 on any
  compile/runtime/I/O error, 2 on misuse. It reads stdin and writes
  stdout/stderr, so `echo hi | rhai -e 'print(stdin_text())' | grep HI` works.
  Kernel gaps that limit its use from `sh` today (all independent of `rhai`,
  reproduced with BusyBox applets): a script cannot run through a `#!/bin/rhai`
  line (`execve` has no shebang support, `chmod` is `ENOSYS`); command
  substitution `$(...)` never returns; an external program's stdout redirected
  to a file (`rhai -e ... > f`, `ls > f`) writes nothing (`os::write` and the
  shell's own redirections work); and the interactive `sh` can die at an idle
  prompt after several command lines. Use pipelines and `rhai script.rhai`.
- **`os` module** (Rust-registered, also available as plain globals):
  `args()`, `env(k)` / `env()`, `exit([n])` (not catchable), `clock()` (seconds
  since start), `sleep(ms)`, `read(path)`, `write(path, text)`,
  `ls(path)` -> `[#{name, size, kind}]`, `stdin_text()`. Failures are catchable
  Rhai errors (`os::read: /x: No such file or directory (os error 2)`), never
  panics. Every host-backed function is registered impure/volatile so the
  optimizer never folds a call at compile time.
- **Limits, on by default, flags `--max-*`:** 10 M operations, 32 nested calls
  (flag ceiling 64; measured safe on the guest's 1 MiB main-thread stack), depth
  64 / 32 expression levels, 4 MiB strings, 100 000 array/map entries, 4 MiB
  per read (files, stdin, `ls` entries are capped *while* reading), `sleep` at
  most one hour. `--sandbox` disables `eval` and `import`. A closed stdout
  (`| head -1`) stops the script quietly. Rhai 1.26.1 does not attach a
  position to built-in arithmetic errors (`1 / 0`); syntax errors and most other
  runtime errors carry `(line, position)`.
- **REPL.** LazyOS terminals are raw (the console returns one key per `read`
  with no echo; the desktop Terminal writes into a pipe pair), so bare `rhai`
  prompts and edits its own line (echo, Backspace, Ctrl-U, Ctrl-C, Ctrl-D,
  Enter as `\n` or `\r`). Multi-line input continues while Rhai reports the text
  as incomplete; `:history`, `!!` and `!N` give in-session history, `:reset`,
  `:cancel`, `:help`, `:quit`. `rhai -q` is the plain-text form for pipes and
  scripted runs (no banner, prompts or echo). `isatty` is not usable to pick
  the mode: the shim reports every stdio fd as a terminal.
- **Resolution.** `rhai` typed at `sh` (or `/usr/local/bin/rhai`, `/bin/rhai`)
  is loaded from the image root `RHAI.ELF`: `load_executable`
  (`kernel/src/process/linux/path.rs`) tries the exact path, then the
  `<NAME>.ELF` of an applet-shaped name (at most 8 characters, mandatory
  `.ELF`, so a data file such as `PASSWD` never shadows the `passwd` applet),
  then the BusyBox alias. The `linux:` spawn path uses the same function.
- **Feature set.** `rhai =1.26.1`, `default-features = false` (no `ahash`
  runtime RNG, so no `getrandom`), `sync` off, no `no_*` language feature; no
  `libc` dependency. Tests: `cargo test` in `libs/rhai-lazy` (bindings, limits,
  REPL, failure paths) and `rhai-host` (CLI, line editor, REPL loops, bounded
  reads); the guest run is `tools/screenshot/examples/rhai_demo.json` (serial
  markers `RHAI:<name>:PASS|FAIL`, CI in `.github/workflows/rhai.yml`).
  Release ELF: see the PR description for the stripped size and the image
  delta. Out of scope here: Messenger bindings, the `Cmd` pipeline type, xui
  bindings, and Rhai as login shell.

The `init` manifest (`user/src/bin/init/state.rs`) declares dependencies and restart
policy: `messengerd` is `Once` (bootstrap can be claimed once per boot), the
rest `Always`, and rapid crashes back off up to `MAX_RESTARTS = 5`.

**Status.** Working: all bins build; services boot under `LAZYOS_SERVICES=1`
(the manifest fills the 16-slot task table, which is why the drag & drop and
shell-probe demos only boot without it), and the `LAZYOS_DESKTOP=1` profile
(#217) runs the same services plus the compositor and its apps while keeping the
demo/evidence programs out; sync and async Messenger APIs plus
generated stubs have host tests. Open: async examples wiring, IDL coverage
beyond the echo sample, `router` removal once every service is on `central`.
