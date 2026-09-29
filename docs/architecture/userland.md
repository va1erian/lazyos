# Userland: runtime, shared libs & services

**What it is.** Everything that runs in ring 3: the `user` crate runtime
(syscall wrappers, allocator, interpreter, Messenger clients), the shared
`libs/`, and the service programs built from `user/src/bin/`.

**Key files**

| Path | Role |
|---|---|
| `user/src/lib.rs` | Runtime modules: `sys`, `dev`, `sysinfo`, `lang`, `messenger`, `central`, `messenger_async`, `task_snapshot`, `heap` |
| `user/src/dev.rs` | Wrappers for the device syscall (23): `list`, `claim`, `map_bar`, `pio_*`, `cfg_*`, `irq_enable`/`irq_ack`, `release`, and `parse_irq` for the kernel's interrupt message (#240) |
| `user/src/sys.rs` | `int 0x80` wrappers, syscall numbers 0-14, `Cred`, display helpers |
| `user/src/sysinfo.rs`, `task_snapshot.rs` | Typed decoders for the syscall 14 system snapshot and the syscall 13 task snapshot |
| `user/src/central.rs` | Topics client that routes service publishes through `messengerd`'s central broker (#169) |
| `user/src/heap.rs` | Bump allocator over `sbrk` (see [allocators.md](allocators.md)) |
| `libs/lang/` | `lexer`, `parser`, `interp`, `value`, `repl` (shell language shared by `SH.ELF` and the desktop Terminal) |
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
  (`os.lazy.messenger.topics`, QoS, deferred `next_event`), `router` (interim
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
| `libs/generated` | `midlc` output for `idl/echo.midl` (`os_lazy_echo_v1`) | `cargo test -p messenger-generated` |
| `libs/crypto` | SHA-256, HMAC-SHA256, HKDF-SHA256, Argon2id, RNG pool, wrap/unwrap, hex | `cargo test -p lazyos-crypto` (KATs); issue #102 |

**Services** (`user/src/bin/`; image names are 8.3)

| Binary | Image | Role | Started by |
|---|---|---|---|
| `init` / `messengerd` | `SUPER` / `MSGRD.ELF` | Supervisor (manifest, spawn/wait, restart backoff, app registry + `Launch`; `XAPPS.LST` decides which registered apps the image ships, and `autostart` rows open at boot as the desktop's apps, #215/#216) / bootstrap registry proxy and topics broker | kernel / `init` |
| `logd` / `healthd` | `LOGD` / `HEALTHD.ELF` | Hash-chained event log / retained `system/health/*` aggregation | `init` |
| `keyd` / `accountsd` / `logind` | `KEYD` / `ACCTD` / `LOGIND.ELF` | Secrets and crypto (#102) / accounts (#101) / console login and credentialed spawn | `init` |
| `clipboardd` / `mimed` / `flaky` | `CLIPD` / `MIMED` / `FLAKY.ELF` | Per-session clipboard (#115) / MIME and open-with (#116) / crash-test service (#93, never started by `LAZYOS_DESKTOP=1`) | `init` |
| `clipcopy` / `clippaste` / `messengerctl` | `CLIPCP` / `CLIPPS` / `MSGCTL.ELF` | Clipboard demo pair (#115, `demo=1` only) / fabric+services views (#70/#89/#93) | `clipboardd`, kernel flag |
| `sysmond` / `top` | `SYSD` / `TOP.ELF` | System-stats service over syscall 14 with `system/stats/*` topics / one-shot text client (#144); services image only, `top` left out of `LAZYOS_DESKTOP=1` | `init` / `sysmond` (`demo=1`) or `init` `Launch` |
| `sh` / `hello` / `xuid` / `xdemo` | `SH` / `HELLO` / `XUID` / `XDEMO.ELF` | Native interpreter with DOS-style commands (`dos.rs`: `dir cd type copy del ren mkdir exec mem reboot shutdown`, #6) / demo / compositor and display demo (#113) | kernel |
| `faultprobe` | `FAULTPRB.ELF` | Deliberate ring-3 faults (`exec FAULTPRB.ELF null\|kernel\|priv\|div\|ud`, #7) | shell |
| `dragdemo` / `shellprobe` | `DRAGDMO` / `SHELLPRB.ELF` | Drag & drop evidence pair (#145) / shell-protocol evidence client (#167); `LAZYOS_XUID=1` images | kernel |
| `async_echo` / `async_service` | not on disk | `messenger_async` examples (#91) | - |

The `init` manifest (`user/src/bin/init.rs`) declares dependencies and restart
policy: `messengerd` is `Once` (bootstrap can be claimed once per boot), the
rest `Always`, and rapid crashes back off up to `MAX_RESTARTS = 5`.

**Status.** Working: all bins build; services boot under `LAZYOS_SERVICES=1`
(the manifest fills the 16-slot task table, which is why the drag & drop and
shell-probe demos only boot without it), and the `LAZYOS_DESKTOP=1` profile
(#217) runs the same services plus the compositor and its apps while keeping the
demo/evidence programs out; sync and async Messenger APIs plus
generated stubs have host tests. Open: async examples wiring, IDL coverage
beyond the echo sample, `router` removal once every service is on `central`.
