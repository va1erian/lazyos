# Userland: runtime, shared libs & services

**What it is.** Everything that runs in ring 3: the `user` crate runtime
(syscall wrappers, allocator, interpreter, Messenger clients), the shared
`libs/`, and the service programs built from `user/src/bin/`.

**Key files**

| Path | Role |
|---|---|
| `user/src/lib.rs` | Runtime modules: `sys`, `lang`, `messenger`, `messenger_async`, `heap` |
| `user/src/sys.rs` | `int 0x80` wrappers, syscall numbers 0-12, `Cred`, display helpers |
| `user/src/heap.rs` | Bump allocator over `sbrk` (see [allocators.md](allocators.md)) |
| `user/src/lang/` | `lexer`, `parser`, `interp`, `value` for `SH.ELF` |
| `user/src/messenger.rs` | Blocking Messenger client: endpoints, registry, topics, services |
| `user/src/messenger_async.rs` | Futures, `Executor`/`block_on`, `Selector`, `service!` |
| `user/src/bin/*` | Ring-3 programs; manifest in `user/Cargo.toml` |

**Syscall wrappers** (`sys.rs`) - register convention: `rax` = number, args in
`rdi/rsi/rdx`, result in `rax`. `rcx`/`r11` are clobbered, so every wrapper
declares `clobber_abi("sysv64")`. Numbers: 0 `exit`, 1 `write`, 2 `read_char`,
3 `read_file`, 4 `sbrk`, 5 `messenger`, 6 `spawn`, 7 `wait`, 8 `clock`,
9 `service_args`, 10 `cred_set`/`cred_get`/`spawn_as`, 12 `display_*` (11 is the
kernel-only quota mirror). See [processes.md](processes.md) and [display.md](display.md).

**Blocking Messenger client** (`messenger.rs`)

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
| `init` / `messengerd` | `SUPER` / `MSGRD.ELF` | Supervisor (manifest, spawn/wait, restart backoff, app registry + `Launch`) / bootstrap registry proxy and topics broker | kernel / `init` |
| `logd` / `healthd` | `LOGD` / `HEALTHD.ELF` | Hash-chained event log / retained `system/health/*` aggregation | `init` |
| `keyd` / `accountsd` / `logind` | `KEYD` / `ACCTD` / `LOGIND.ELF` | Secrets and crypto (#102) / accounts (#101) / console login and credentialed spawn | `init` |
| `clipboardd` / `mimed` / `flaky` | `CLIPD` / `MIMED` / `FLAKY.ELF` | Per-session clipboard (#115) / MIME and open-with (#116) / crash-test service (#93) | `init` |
| `clipcopy` / `clippaste` / `messengerctl` | `CLIPCP` / `CLIPPS` / `MSGCTL.ELF` | Clipboard demo pair (#115) / fabric+services views (#70/#89/#93) | `clipboardd`, kernel flag |
| `sh` / `hello` / `xuid` / `xdemo` | `SH` / `HELLO` / `XUID` / `XDEMO.ELF` | Native interpreter / demo / compositor and display demo (#113) | kernel |

The `init` manifest (`user/src/bin/init.rs`) declares dependencies and restart
policy: `messengerd` is `Once` (bootstrap can be claimed once per boot), the
rest `Always`, and rapid crashes back off up to `MAX_RESTARTS = 5`.

**Status.** Working: all bins build; services boot under `LAZYOS_SERVICES=1`;
sync and async Messenger APIs plus generated stubs have host tests. Open: async
examples wiring, IDL coverage beyond the echo sample.
