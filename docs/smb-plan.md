# Network directories and SMB — exploration and plan

Status: **F0 reviewed, F1 (the FUSE mechanism) built**: syscall 35,
`kernel/src/fs/fuse/`, `libs/fused`, `memfuse`, `fuse_suite` and
`tools/fuse/run.py` (§3.1 records what the review changed). **F2 (the SMB
client) built**: `libs/smbwire`, the `smb` command (`LAZYOS_SMB=1`,
`run_demo.py --smb`), `tools/smb/run.py` and a real-Samba interop check
(§4.6 records what F2 decided). F3 onwards is not started. It is built
inside-out from a **FUSE mechanism** — a user-space filesystem framework whose
kernel side is as thin and generic as we can make it — so that every
filesystem behaviour, including SMB, lives in userspace. SMB is then just one
user-space filesystem, the way `sshfs` and `smbnetfs` are on Linux. The plan
builds on networking stages N0–N5
([networking-plan.md](networking-plan.md) §10.1,
[architecture/networking.md](architecture/networking.md)) and on the existing
user-space **block provider** ([architecture/block-devices.md](architecture/block-devices.md)),
whose kernel-mediated, parked, ring-3 provider shape we reuse.

Scope — one concrete goal and one direction:

1. **G1, file transfer.** From a running LazyOS instance, mount a LAN Samba
   share over TCP **445** through the FUSE mechanism, authenticate as a named
   user with a password (NTLMv2), and transfer files **byte for byte**, both
   ways, with directory listing. The named target is the server
   **`chatonnas`** (login `chaton`). The harness proves G1 against a scripted
   SMB server on the host using `LAZYOS_SMB_USER` and `LAZYOS_SMB_PASSWORD`; a
   `--live` run uses the real server's password, entered at the prompt or
   supplied through `LAZYOS_SMB_PASSWORD`. A direct `smb` command is kept as a
   kernel-free way to exercise and judge the protocol.
2. **G2, network directories.** The mounted share is a **real directory at a
   path** that every program sees — native and Linux ABI alike — because the
   kernel VFS routes ordinary `open`/`read`/`readdir` to the user-space
   daemon.

**Guiding rule.** The kernel gains **one generic file-provider backend and one
syscall**, modeled on the block provider that already exists, and **no SMB,
NTLM, network-session or file-semantics code**. Credentials, session state,
caching, retries and the whole SMB2 state machine are a user-space daemon. Where
a kernel change can be avoided at all, it is (§3.5).

Related: [networking-plan.md](networking-plan.md),
[architecture/networking.md](architecture/networking.md),
[architecture/filesystem.md](architecture/filesystem.md),
[architecture/block-devices.md](architecture/block-devices.md),
[tls-plan.md](tls-plan.md), [filesystem-plan.md](filesystem-plan.md),
[security-model.md](security-model.md) §5, [packages.md](packages.md),
[xui-plan.md](xui-plan.md).

## 1. Summary of recommendations

| Question | Recommendation |
|---|---|
| Architecture | **FUSE first**: a generic kernel bridge plus a user-space daemon protocol. Any later network filesystem (9p, NFS, an sshfs-style one) reuses it; SMB is the first daemon |
| Kernel delta | **One generic `Filesystem` backend** (`kernel/src/fs/fuse/`) and **one provider syscall** (35), a sibling of the block provider (syscall 33). No SMB, NTLM, network or filesystem-specific code |
| Where SMB runs | A **user-space daemon** (`smbfuse`); the `smb` command links the same library in-process. Never the kernel |
| Language | **Native `no_std`** in the `user/` workspace, over the existing Messenger socket client (`netsock`), as `ftp` and `nc` are. The mount removes the need for local file I/O in the client, so no musl program is needed |
| Filesystem data path | A **64 KiB bounce buffer** first (the block provider's model: one copy each way, tiny kernel); a shared fenced buffer is a later optimisation |
| Blocking | A VFS op parks the calling task while the daemon serves it; `flush`/`writeback` must never block (the flusher rule) |
| Mount | The daemon registers a provider under a **name**; the bridge mounts it at `/mnt/<name>` in **both** the native and ABI `Vfs` tables, always `nosuid`. Registering needs `CAP_FS_PROVIDER` |
| Protocol | **SMB2, dialect 2.1 (0x0210)**, over **Direct TCP 445**. Not SMB1 (modern Samba disables it), not SMB3 first |
| Authentication | **NTLMv2** inside `SESSION_SETUP`; the NTLM **domain** comes from the server's challenge target info |
| Signing | **HMAC-SHA256 truncated to 16 bytes** on established sessions; `--sign`/`--sign-required` |
| Confidentiality | SMB 2.1 signing is **integrity only**: **G1 assumes a trusted LAN**; SMB3 encryption is deferred |
| Crypto | RustCrypto `md-4`, `md-5`, `hmac`, `sha2` (all `no_std`) plus native syscall 26 for randomness |
| Tools | `smb` (direct transfer and protocol evidence) and `net mount`/`net ls` (through the mounted directory) |
| Verification | A toy file daemon proves the **mechanism**; SMB is judged from the server's own file record and the pcap, plus negatives; `--live` against `chatonnas` |

## 2. Where we are

| Piece | State today | Evidence |
|---|---|---|
| User-space **block** provider | built: a ring-3 driver registers a whole disk; the kernel posts requests through a 64 KiB bounce buffer, parks the caller in 10-tick slices with a 10 s deadline, and treats two timeouts or provider death as a dead disk | `kernel/src/block/provider.rs`; [architecture/block-devices.md](architecture/block-devices.md) |
| A user-space **file**-level seam | none: no FUSE/9p/virtiofs, and no `mount()` syscall | [architecture/filesystem.md](architecture/filesystem.md) §"Mounting" |
| The kernel `Filesystem` trait | synchronous, `Send + Sync`, object-safe: `lookup/stat/read/write/truncate/setattr/create/mkdir/unlink/rmdir/rename/readdir/flush/writeback/statfs` | `kernel/src/fs/vfs/filesystem.rs` |
| Mount tables and flags | `FS` (native) and `ABI_FS`, both `Vfs`; mounts from `lazyos.cfg` at boot, plus the late USB `/home`; `readdir` appends mount points | `kernel/src/fs/mod.rs`, `kernel/src/fs/mounts.rs`, `kernel/src/fs/vfs.rs` |
| Errors | `FsError` has no `Io`/`Timeout`/unreachable variant; device failure folds into `Invalid` | `kernel/src/fs/vfs/meta.rs` |
| TCP for native tools | built (N3): `netd` over smoltcp, parked calls, 16 KiB parcels | `user/src/messenger/netsock.rs`, `user/src/messenger/netstd.rs` |
| A blocking native client precedent | built: `ftp` (passive TCP, host-tested `libs/ftpwire`, judged from the server record and the pcap) | `user/src/bin/ftp/`, `tools/net/run.py` |
| Crypto in the OS workspace | `libs/crypto`: SHA-256, HMAC-SHA256, HKDF, Argon2id. **No MD4/MD5/AES**; the AES crates fail codegen on `x86_64-unknown-none` | `libs/crypto/`, `docs/tls-plan.md` §2 |
| Native randomness | syscall 26, `random(buf, len)`, open to every task | `docs/networking-plan.md` §13 (N2) |
| The named target | `chatonnas`, a Samba server; login `chaton`; the live password is supplied at run time; SMB on the usual port 445 | this brief |

## 3. The FUSE mechanism (the foundation)

The plan starts here, not at SMB: a generic way for a user-space process to
serve a directory tree, so no filesystem has to be written in the kernel.

```
  cat, ls, cp, a GUI file manager      native + Linux ABI callers
        │  open/read/write/readdir/getdents/stat …     (ordinary VFS)
        ▼
  kernel Vfs (FS and ABI_FS)  ──►  kernel/src/fs/fuse.rs   generic proxy
        │  request (op, path, handle, ≤64 KiB bounce buffer)   ┌──────────┐
        │  reply   (result, data)                               │ parked   │
        ▼                                                       │ caller   │
  fuse provider syscall  ◄──── NEXT / REPLY loop ────  smbfuse (user space)
                                                          │  smbwire + netsock
                                                          ▼
                                              netd ⇄ netdrv ⇄ virtio-net  (unchanged)
```

### 3.1 The kernel side, kept minimal

Everything below is generic: it knows nothing about SMB or the network.

| Piece | Change | Why it is small |
|---|---|---|
| `kernel/src/fs/fuse/` | A `Filesystem` impl (`backend.rs`) that translates each trait call into a provider request and returns the reply; the slot, channel and mount edge beside it | The trait already exists; this is a translator, not a filesystem |
| Provider syscall **35** (34 is the monotonic clock) | `REGISTER` (mounts), `NEXT`, `REPLY`, `UNREGISTER` (unmounts) | Copies the block provider's parking, deadline, provider-death and one-outstanding-request design |
| Mount registration | Attach a provider at `/mnt/<name>` in **both** `FS` and `ABI_FS`; `Vfs::unmount` (new) takes it out | `Vfs::mount`, plus the unmount the VFS lacked |
| `FsError` | Add `Io` (`EIO`); a timeout is an I/O error too | One variant and the two errno tables |
| Kernel tests | Correctness + soak for the new syscall and backend (hostile paths, bad handles, provider death mid-op, fd/lifecycle, thousands of ops) | AGENTS.md requires it for any kernel component |

**Data plane.** The first cut uses a per-provider **bounce buffer** of 64 KiB
of data plus one path (`fused::wire::MAX_PAYLOAD`), the same data bound as the
block provider (`MAX_REQUEST_BYTES`). Each request carries at most 64 KiB: a
larger `read` or `write` is split into sequential chunks, and the backend
advances the file offset and the buffer position by the bytes each chunk
completed. It stops on a short chunk and returns the accumulated count; a chunk
that fails returns its error when nothing was transferred yet, and otherwise
the count so far (a short write, as POSIX allows), since earlier chunks may
already have reached the server. The kernel copies each chunk into or out of the request: one
copy each way, no pinning, no mapping, and the daemon never sees caller memory.
A shared, fenced buffer (as the NIC rings and audio streams use) removes the
copies later and is a pure optimisation — it changes no interface.

**Requests and replies.** The records are `libs/fused::wire`, linked by both
sides. A request is ten words `(tag, op, ino, generation, offset, len,
path_len, mode, uid, gid)` plus the bounce payload (the path, then a write's
data, a rename target or a `SETATTR` record); a reply is thirteen words
`(tag, status, count, data_len, attributes)` plus its payload. `readdir`
returns encoded entries from an index on, as many as fit, and the kernel asks
again until a batch is empty (at most 65536 entries); `lookup` returns the
attribute block; `read`/`write` use the payload. Paths are relative to the
mount root and length-bounded; an op word with `FLAG_NODE` names the
`(ino, generation)` a lookup returned instead of a path, which is how open
files are read and written. Each side treats every field as hostile, the same
rule `libs/ftpwire` applies to server replies: the kernel refuses a node type
other than file or directory, an id wider than 32 bits, a count above what it
asked for, reply data longer than the request's room, and a malformed
directory payload, each as `EIO`.

**Blocking and the flusher.** A VFS operation is synchronous: the backend posts
the request and **parks the caller** (as the block provider parks a `read`),
with a real deadline; when the daemon replies, the task wakes and the operation
returns. The 5-second `fs::flusher` must **never block**
(`kernel/src/fs/flusher.rs`), so periodic `Filesystem::writeback` on a FUSE mount
is a non-blocking no-op or a best-effort post. `Filesystem::flush`, by contrast,
**is** the durability point: an explicit `flush` (and the `fsync` that reaches
it) parks its caller until the daemon has completed the SMB `FLUSH` and
acknowledged the data, so a success can never precede durability. The daemon
owns durability; the flusher never waits.

**Authority.** Registering a provider and mounting are privileged edges:
`CAP_FS_PROVIDER` (bit 12; root holds it) allows it, and only under `/mnt`, so
a daemon can never shadow a system directory. Unlike the block provider there
is no uid allow-list yet: F3 adds one (or a label rule) when `smbfuse` gets its
service account ([security-model.md](security-model.md) §5).

**Caching and coherence.** The kernel backend is stateless; the daemon caches
SMB metadata and handles. The VFS's own dentry/inode cache sits above and
**keeps entries until a mutation through the VFS invalidates them** (the
review's correction: the draft said it needs no change). That is right for
`memfuse`, whose tree only changes through the mount, and wrong for a share
another client writes. F3 must add an expiry to the VFS cache for FUSE mounts
(a per-mount TTL, or an attribute-timeout field in the reply) before `smbfuse`
ships; until then a remote change is invisible to `stat` and `ls` of a path
already looked up. The two mount tables also cache separately, so a change
made through one (a native `chmod`) is not seen by the other's cache (a Linux
`stat`) until that path is invalidated there: the same expiry fixes both, and
the kernel suite shows the gap.

**The mount table lock** (found in review). The VFS holds its mount table (a
`YieldMutex`) across every filesystem call, so a path operation (lookup,
create, readdir) waiting on a daemon stalls *every* VFS caller, and a daemon
that touches the VFS on its request path, directly or through a service it
waits on (`logd` writing a journal, say), deadlocks until the deadline. Two
mitigations are built: open files are read and written **by node**
(`Filesystem::open_node`), and the ABI layer opens a FUSE file in place rather
than copying it whole into a heap snapshot at `open` (`abi_persistent`, which
only knew ext2), so the bulk of the traffic does not hold the table; and the
rule that a daemon never uses the filesystem while serving is written into
`fs/fuse/mod.rs`. F3 must check `smbfuse`'s path (`netd`, `logd`) against it;
releasing the table across the call is the real fix if it bites.

**Lifecycle.** `UNREGISTER` unmounts at once. A daemon that dies cannot be
unmounted from its teardown (which may run where the table cannot be waited
for): its mount fails every call with `EIO` until the periodic flusher (which
never waits) or the next `REGISTER` of the same name removes it. A slot is
reused only when no requester still holds it, and each registration has a new
epoch, so an open file of a dead daemon can never reach its successor.
`/mnt` is a directory of the image (`build_support/os_layout.rs`); a system
without it refuses `REGISTER` with `ENOENT`.

### 3.2 The user-space side

| Path (proposed) | Role |
|---|---|
| `libs/fused/` | The protocol (`wire`, `payload`), shared with the kernel; a `no_std` daemon library (`daemon::serve_one` over a `Provider`, a `FuseFs` trait a daemon implements) and an in-memory tree (`memfs`). Host-tested against a scripted provider and seeded garbage; the kernel suite serves the same code through the real kernel path |
| `user/src/sys/fuse.rs` | The syscall wrapper: `Mount`, a `Provider` over syscall 35 |
| `libs/smbwire/` | The SMB2 + NTLMv2 protocol (no I/O); host-tested and fuzzed (§4.2) |
| `user/src/bin/memfuse.rs` | A toy in-memory filesystem daemon that proves the mechanism with **no network at all** (stage F1); on every image |
| `user/src/bin/ftpfuse.rs` | A proof of concept of a **network** filesystem daemon: an FTP server at `/mnt/<name>`, over `netsock` and `libs/ftpwire`, sharing the `ftp` client's session code (`LAZYOS_NETD=1` images) |
| `user/src/bin/smbfuse.rs` | The SMB daemon: implements `FuseFs` over `smbwire` and `netsock` (§4.4) |
| `user/src/bin/smb.rs` | The direct command for tests and diagnostics (§4.5) |

**`ftpfuse`, the network PoC.** It has the shape `smbfuse` will have (a
daemon owning a remote session, a metadata cache, reconnects) and shows what
the mechanism leaves to a daemon when the protocol is not a filesystem. FTP
has no random-access write, so a write at the end of a file is an `APPE` (a
sequential `cp` or `>>` costs one upload per 64 KiB request) and any other
write fetches the file whole, patches it and `STOR`s it back (files up to
32 MiB); reads fetch a file whole on first use. Listings (`MLSD`, or Unix
`LIST`) are believed for 3 s. The desktop mounts it through `mountd` (§3.4). Known limits, acceptable for a PoC and not for
SMB: a connection lost mid-`APPE` is retried once and could duplicate the
chunk, and the VFS cache above it has the expiry gap of §3.1.

### 3.3 What we deliberately do **not** put in the kernel

- No SMB2, no NTLM, no signing, no session key.
- No TCP or TLS; the daemon uses the socket service like any other client.
- No credentials, no password storage, no `keyd` use.
- No file semantics beyond pass-through; the daemon owns create/rename/delete.
- No retries, reconnect, caching or case-insensitivity rules.
- No per-filesystem code: adding 9p or an sshfs-style filesystem later is a new
  user-space daemon, not a kernel patch.

### 3.4 Mounting from the desktop: `mountd` and Network Drives

A daemon needs `CAP_FS_PROVIDER` and an installed app holds no capability, so
the desktop cannot start `ftpfuse` itself. **`mountd`** (`user/src/bin/mountd.rs`,
`LAZYOS_NETD=1` images) is the one place that may: a supervised service running
as `_mountd` (uid 910) with `CAP_FS_PROVIDER` and nothing else, serving
`os.lazy.mount.v1` (`idl/mount.midl`: `Mount`, `Unmount`, `List`). Each
`Mount` starts one `ftpfuse`, which inherits that credential; a package reaches
the service only through its manifest's `os.lazy.mount.v1` permission.

| Rule | How |
|---|---|
| Hostile requests | `libs/mounttable` (host-tested) checks every field before anything starts: the name is one directory (`a-z0-9-_`, 32 bytes), the host cannot read as an option or carry a port, no control characters; at most 8 mounts |
| Ownership | the files are reported as the **caller's** kernel-stamped uid and gid (`ftpfuse owner=`), not `_mountd`'s; only that uid or root may unmount |
| Passwords | travel only in the daemon's `argv`; the table, `List` and the serial log never hold one |
| State without a pipe | native programs have no pipe, so the mount point appearing makes a mount `mounted`, the daemon's exit code (`ftpfuse`'s `Failure`: network, resolve, login, mount, serve) makes it `failed` with a reason, and 45 s without either kills it |
| Unmount | `SIGTERM` to the daemon; the kernel removes the dead mount at the next flusher pass or `REGISTER` of the name |
| Shutdown | `mountd` serves `os.lazy.lifecycle.v1` and stops its daemons; a crash of `mountd` leaves them serving, unlisted |

**Network Drives** (`os.lazy.netdrives`, `xui-app/src/bin/netdrives.rs`) is the
front end, a core package in every `--net` desktop: a connect form (server,
port, user, password, folder name) checked with `mounttable`'s rules, the mount
list refreshed every second with each state and failure reason, **Open in
Files** (`init.Launch` of `os.lazy.files` at `/mnt/<name>`) and **Unmount**.
Scripts reach the same service as `sys::mount` (`rhai`). `tools/fuse/ui_run.py`
drives the app against the host FTP server under `LAZYOS_LABEL_TRACE=1`.

## 4. SMB as a user-space filesystem

### 4.1 Protocol: why SMB2.1 over 445

| Shape | What | Verdict |
|---|---|---|
| **SMB2, dialect 2.1**, TCP 445 | The `0xFE 'S' 'M' 'B'` header, credits, compounds, NTLMv2, HMAC-SHA256 signing | **Build this.** Samba's default floor is SMB2 (`server min protocol = SMB2_02`); 2.1 adds large reads/writes but no new crypto |
| SMB1 / CIFS | The old `0xFF 'S' 'M' 'B'` header and NetBIOS session service | No: disabled by default in current Samba; a security liability |
| SMB3 (3.0/3.0.2/3.1.1) | AES-CMAC then AES-GCM signing, preauth integrity, encryption | Later (F6): more crypto than G1 needs |
| SMB over NetBIOS 139 | SMB2 framed over the NetBIOS session service | No: 445 is what every modern server listens on |

On 445 the framing is a 4-byte NetBIOS session header (first byte `0x00`, a
24-bit big-endian length) then the 64-byte SMB2 header. Compound requests
(`NextCommand`) may be used later; the first cut sends one command per frame.

### 4.2 The client library

`libs/smbwire` (native `no_std` + `alloc`, host-tested and fuzzed like
`libs/ftpwire`): transport framing, the SMB2 header and command
encoders/decoders, NTLMv2, and signing. Published Rust SMB crates (`pavao`,
`smb-rs`, `smbclient`) are async and need a runtime and threads the task model
does not give, so the wire layer is hand-written.

The transport is a trait with two implementations: `netsock` (`TcpStream`) for
the native daemon and CLI, and an in-memory script for host unit tests. The
daemon and the CLI share this library, so the harness exercises exactly the
bytes a mount would send.

### 4.3 Session flow against `chatonnas`

1. `TcpStream::connect((server IP, 445), CONNECT_MS)`.
2. **NEGOTIATE** — dialects `[0x0210, 0x0202]`, `SecurityMode` = signing enabled
   (and required only if `--sign-required`), a random `ClientGuid`, capabilities
   0. The response may carry an empty security buffer or a server `GSS`/SPNEGO
   token; the client does not send its own token here. A dialect below 2.0.2 or
   an encryption-required server is a clean failure.
3. **SESSION_SETUP** twice:
   - send NTLMSSP **type 1** (`NEGOTIATE_MESSAGE`), `NTLMSSP_NEGOTIATE_KEY_EXCH`
     **clear**. Its flags come from the server's NTLM flags when the `NEGOTIATE`
     security buffer supplied them, and otherwise from the client's own
     supported capabilities — the type 1 message always has defined flags;
   - the server replies `STATUS_MORE_PROCESSING_REQUIRED` with a **type 2**
     (`CHALLENGE_MESSAGE`): the 8-byte server challenge and the target-info
     `AvPairs`. Read the NTLM domain from `MsvAvNbDomainName`, unless `-W` set
     one;
   - compute `NTProofStr`/blob and send **type 3** (`AUTHENTICATE_MESSAGE`);
   - success yields `SessionId` (and, if signing, the `SessionKey`).
4. **TREE_CONNECT** to `\\chatonnas\<share>`; the response gives a `TreeId`.
5. **CREATE / READ / WRITE / CLOSE**, **QUERY_DIRECTORY**, **QUERY_INFO**,
   **SET_INFO**.
6. **TREE_DISCONNECT**, **LOGOFF**.

`libs/smbwire` carries a minimal SPNEGO codec so both server-initiated and
client-initiated exchanges work: a `GSS`/SPNEGO wrapper received in `NEGOTIATE`
or `SESSION_SETUP` is unwrapped to its NTLMSSP token, and the client wraps its
type 1/3 replies in a SPNEGO `NegTokenInit`/`NegTokenResp` when the server's
token was wrapped, or sends them raw when it was not. Both modes are tested, so
a compliant Samba that expects SPNEGO is not rejected.

### 4.4 The daemon: FUSE operations onto SMB2

| FUSE op | SMB2 |
|---|---|
| `lookup`, `stat` | `CREATE` (open, no read/write) or `QUERY_INFO`, then `CLOSE` |
| `readdir` | `QUERY_DIRECTORY` (FileIdBothDirectoryInformation) |
| `read` / `write` | `READ` / `WRITE` at an offset |
| `create` / `mkdir` | `CREATE` (file or directory disposition) |
| `unlink` / `rmdir` | `SET_INFO` (disposition delete), then `CLOSE` |
| `rename` | `SET_INFO` (FileRenameInformation) |
| `truncate` / `setattr` | `SET_INFO` (end-of-file and basic info) |
| `flush` | `FLUSH`, parking the explicit caller until acknowledged |
| `statfs` | `QUERY_INFO` (FileFsFullSizeInformation) |

The daemon holds the SMB session, its handles and its metadata cache. A
caller-scoped credential resolves the requested `user`, and a session is reused
only for the same **`(server, share, credential identity)`**; a mount asking
for a mismatch is rejected.

### 4.5 The `smb` command

```
smb [-q] [-v] [--sign | --sign-required | --no-sign] [-p PORT] [-W DOMAIN]
    -U user //server/share [cmd ; cmd ...]
smb -L //server -U user                        # list shares (F4)
```

(As built: `-v` echoes each command in place of the planned `-d DIAG`;
`--no-sign` refuses a server that requires signing; `mv FROM TO` and `df`
join the commands. From a shell, quote the command list: `;` is the
shell's separator too.)

The password is never an argument (a `%pass` form does not exist): it comes from
the prompt or, for headless runs, `LAZYOS_SMB_PASSWORD` (§6). Commands:
`ls [path]`, `cd`, `pwd`, `get REMOTE [-|!]`, `put [LOCAL|-g N] [REMOTE]`,
`mkdir`, `rm`, `rmdir`. `get f -` writes to stdout, `get f !` checksums, `put
-g N` generates a stream, mirroring `ftp`'s conventions. Native programs have no
general file-**write** syscall, so the direct command does not create local
files: `get` only streams to stdout or checksums, and `put` only uploads a local
file it can read (`sys::read_file`) or a generated stream. Real transfer in both
directions is the **mount** (`cp` through the directory); the command exists to
run the protocol without FUSE and to feed the harness sink.
Serial markers: `SMB:DIALECT 0x0210`, `SMB:LOGON user=… domain=…`,
`SMB:TREE share=…`, `SMB:LIST n=…`, `SMB:GET name bytes=N crc=…`, `SMB:PUT …`,
`SMB:PASS|FAIL reason=…` — markers say when, they are never the verdict.

### 4.6 What F2 built and decided

| Piece | As built |
|---|---|
| `libs/smbwire` | `frame` (Direct TCP, 256 KiB bound checked before buffering), `header`, `msg` (every command of §4.4, each response checked against its `StructureSize` and every offset against the message), `ntlm`, `spnego`, `crypto`, `name` (paths to the backslash form; `..`, streams, wildcards and control characters refused; listed names that are not one plain component skipped), and `client`: a synchronous session over a `Transport` trait that owns message ids, credits, interim `STATUS_PENDING` responses, unsolicited oplock breaks and signing. No clock and no random source: the caller passes the time, the `ClientGuid` and the client challenge |
| NTLMv2 timestamp | the server's `MsvAvTimestamp` when the challenge carries one (Samba and Windows do), with 24 zero bytes for the LM response (`MS-NLMP` 3.1.5.1.2); the client's clock only otherwise, with LMv2. So a dead RTC breaks only servers that send no time, and `smb` says so on a `LOGON_FAILURE` when the clock reads before 2026-01-02 |
| No MIC | the AUTHENTICATE carries no MIC: a MIC makes Samba demand SPNEGO's `mechListMIC` too. Both are optional, and Samba 4.19 accepts the exchange without them; adding them is a self-contained later change |
| Refusals | a guest or anonymous session (`map to guest` would make a wrong password look like a logon), `ENCRYPT_DATA` on the session or the share, a non-disk share, a server requiring signing under `--no-sign`, a dialect other than 2.0.2/2.1, a missing or wrong signature when signing is on, and a forged one on any response that claims to be signed. Samba with `server smb encrypt = required` answers a 2.1 logon with `ACCESS_DENIED`, which `smb` explains as possibly needing SMB3 |
| Sizes | no `LARGE_MTU`: reads, writes and listings are at most 64 KiB (or the server's smaller maximum), one request outstanding |
| `smb` | `user/src/bin/smb.rs` (+ `smb/link.rs`, `smb/cmds.rs`): the password from `LAZYOS_SMB_PASSWORD` or a prompt that does not echo; a native program, so it is on the kernel's `execve` list of native programs (`process/linux/native.rs`) |
| Harness peer | **not impacket**: antivirus flags it on Windows hosts, so `tools/smb/smbserver.py` is a standard-library SMB 2.1 server written for the harness (NTLMv2 verified with its own MD4, signatures checked both ways, a request record, and switches for each negative). Real Samba is the interop check: `tools/smb/samba_interop.py` runs Samba 4.19 (Alpine, Docker) against the library's host client `smbcat` |
| Secrets in sessions | `qemu_session.py` gained `type_secret`, which types a host environment variable, so the password is in neither the script nor the summary |

## 5. The crypto the design needs

NTLMv2 and SMB 2.1 signing need only small, well-understood primitives. None is
in `libs/crypto`, and the AES family currently fails codegen on
`x86_64-unknown-none` (`libs/crypto/src/wrap.rs`), so the protocol crate brings
its own.

| Need | Where | Crate | Licence |
|---|---|---|---|
| MD4 | `NT_hash = MD4(UTF-16LE(password))` | `md-4` | MIT OR Apache-2.0 |
| MD5 | `NTOWFv2`, `NTProofStr`, `SessionBaseKey` (HMAC-MD5) | `md-5` | MIT OR Apache-2.0 |
| HMAC | HMAC-MD5 (NTLMv2) and HMAC-SHA256 (signing) | `hmac` (`Hmac<Md4/Md5/Sha256>`) | MIT OR Apache-2.0 |
| SHA-256 | the SMB 2.1 message signature (truncated to 16 bytes) | `sha2` | MIT OR Apache-2.0 |
| Randomness | the 8-byte NTLMv2 client challenge | native syscall 26 | in tree |
| RC4 | only if NTLM key exchange is ever enabled (the client clears the flag, so unused first) | `rc4` (deferred) | MIT OR Apache-2.0 |

```
NT_hash      = MD4(UTF-16LE(password))
NTOWFv2      = HMAC-MD5(NT_hash, UTF-16LE(uppercase(user) + domain))
NTProofStr   = HMAC-MD5(NTOWFv2, server_challenge || client_blob)
SessionBaseKey = HMAC-MD5(NTOWFv2, NTProofStr)
```

The `client_blob` is a 28-byte header (version, zeroes, FILETIME timestamp,
8-byte client challenge) followed by the server's target-info `AvPairs` echoed
verbatim and a 4-byte terminator. The **timestamp** needs a plausible wall
clock; a dead RTC (the 2026-01-01 fallback) breaks NTLMv2 the same way it
breaks TLS, and the client must say so rather than report a wrong password.

The SMB 2.1 signature is the **first 16 bytes of `HMAC-SHA256(SessionKey,
message)`**, with the signature field zeroed. Signing applies only to messages
belonging to an established session (a nonzero `SessionId`): signed when the
session requires it (the server's `SecurityMode` or `--sign-required`), or when
`--sign` asks, and otherwise optional. `NEGOTIATE` and the initial,
pre-authentication `SESSION_SETUP` are never signed; signing and verification
begin once authentication has yielded the `SessionId` and `SessionKey`.
`SessionKey = SessionBaseKey` because key exchange is off; if a server ever
demands it, SMB3 (F6) is the answer, not RC4 in 2.1.

**Licence.** Every crate linked into the OS or a tool must have a
GPLv2-compatible licence; `md-4`/`md-5`/`hmac`/`sha2` are `MIT OR Apache-2.0`,
which passes under the MIT choice. Reuse the `tools/nettls/licenses.py` gate (or
a sibling) so a later link into a GPL-2.0-only NetSurf port stays clean.

## 6. Credentials

The password is a real credential and must not appear in `argv` (visible in
`/proc`), the serial log, a crash message, the pcap or a committed file. In
order:

1. **Prompted, never stored** — `smbfuse`/`smb` read it from the Terminal using
   the secret-input mode T4 adds ([tls-plan.md](tls-plan.md) §6.4); the harness
   drives the real keyboard path with a `type_secret` step.
2. **`LAZYOS_SMB_PASSWORD`** for headless `--live`/harness runs only; the value
   is never committed and the leak check scans every artifact.
3. Later, optional **`keyd`-sealed** `~/.config/smb/<server>` (the same
   "obfuscated at rest until #187" caveat as `imapc`).

`smbfuse` authenticates on the caller's behalf and reuses a session only for the
same `(server, share, credential identity)`. The harness reads
`LAZYOS_SMB_USER`/`LAZYOS_SMB_PASSWORD` from the host environment and never
prints them; no fixed credential is committed.

## 7. Reaching `chatonnas` by name

`10.0.2.2` (QEMU slirp's host alias) is how the **harness** reaches a host
server ([networking-host-access.md](networking-host-access.md) §"10.0.2.2").
On a **real LAN**, `chatonnas` must resolve, and Samba advertises itself three
ways the stack does not speak yet:

| Mechanism | Port | Plan |
|---|---|---|
| DNS / `/etc/hosts` | 53 | `/etc/hosts` support exists; document the entry for `--live` |
| **NBNS** (NetBIOS name service) | UDP 137 | **F4**: a small broadcast-then-unicast query, host-tested |
| **mDNS** / **LLMNR** | UDP 5353 / 5355 | **F4** |
| WINS | UDP 137 | No |

The data path never needs NetBIOS once the IP is known: SMB2 over **445** is
plain TCP. **G1** therefore takes an IP or a hosts entry so it does not depend
on name resolution. A physical `chatonnas` also needs bridged/TAP networking
from QEMU (or real hardware); slirp reaches only the host, which is fine for the
harness (`--live` is the opt-in bridge run).

## 8. What the OS already gives, and its limits

| Need | State | Note |
|---|---|---|
| TCP with deadlines | built | `netsock` parked calls with explicit ms timeouts; the `ftp` pattern |
| Provider registration + parking | built for blocks | the FUSE provider copies `kernel/src/block/provider.rs`'s design |
| `readdir`/`stat`/`open` for ABI programs | built | the FUSE mount lands in the ABI table too, so `cat`/`cp` work |
| File writes from the client | unnecessary | unlike the earlier draft, the mount does local I/O; the daemon never writes the local filesystem |
| Wall clock | RTC + PIT, no NTP | NTLMv2 timestamp and SMB2 `SystemTime`; a wrong clock is reported as such |
| Entropy | syscall 26 | client challenge and `ClientGuid` |
| 16 KiB per socket call | built | one SMB2 `READ`/`WRITE` per round trip; a several-MiB file is many round trips at ~1.6 MB/s |

## 9. Verification

The verdict is what the SMB server recorded and what crossed the wire, never a
serial marker alone — the principle of `tools/net/run.py` and
`tools/net/tls_run.py`.

**Mechanism first.** Before any SMB, `memfuse` is mounted and judged on its own:
`ls`/`cat`/`cp`/`echo >` through the mount, a byte-exact copy round trip, and
the kernel provider exercised under load. This proves the FUSE bridge with no
network variable.

**Harness peer.** A scripted SMB2 server on the host loopback, reachable from
the guest at `10.0.2.2:PORT` with no QEMU forward (slirp maps the gateway to
host loopback): `tools/smb/smbserver.py`, a standard-library server written
for the harness (planned as impacket's `smbserver.py`, which antivirus
quarantines on Windows hosts; §4.6), then **real Samba in
Docker** with a pinned `smb.conf` for interop, and the developer's own
**`chatonnas`** for `--live`. Use a **high host port** (e.g. 1445) so the
harness needs no host privilege; the live run uses the usual **445**.

| Layer | What | Run |
|---|---|---|
| Host unit | `libs/smbwire`: framing (length bounds, compound `NextCommand`), header encode/decode, every command, NTLMv2 vectors from `MS-NLMP`, signing vectors, hostile/truncated/oversized frames, a seeded fuzz entry; `libs/fused` against a scripted provider (built) | `cargo test -p smbwire -p fused` |
| Kernel | `fuse_suite`: the real path with `memfs` served by `serve_one`, both tables, nodes across renames, in-place open files, a 3000-entry directory, read-only mounts, unmount/remount epochs; hostile daemons (error statuses, silence, death mid-request, stale and oversized replies, faulting data, lying attributes and listings); the syscall gate and hostile buffers; a soak (1500 write/read rounds, 200 mount generations) | `LAZYOS_TEST_FILTER=fuse python tools/test/run.py --accel none` |
| Mechanism e2e | `tools/fuse/run.py`: start `memfuse`, `cp` a file in and out and `cmp` it, rename, append, remove, `statfs`, kill the daemon and remount | built |
| Network daemon PoC | `tools/fuse/ftp_run.py` (`--list` for servers without `MLSD`): `ftpfuse` against a host FTP server (`tools/fuse/ftpserver.py`, checked by `test_ftpserver.py` against `ftplib`); list, read, `md5sum`, `cp` in, `>>`, an in-place `dd` patch, `mkdir`/`mv`/`rm`/`rmdir`, judged from the server's directory | built |
| Desktop mount (§3.4) | `cargo test -p mounttable`; `tools/fuse/ui_run.py`: Network Drives fills its form by widget name, a wrong password fails with the login reason, the right one mounts `/mnt/site` owned by the requester, the Terminal reads and writes through it, Files opens it, Unmount removes it; no `LABEL:DENY` | built |
| SMB host | `cargo test -p smbwire`: the `MS-NLMP` NTLMv2 vectors, a signature computed by the Python reference, framing, headers, paths, challenges, SPNEGO both ways, and whole sessions against an in-memory server that verifies the NTLMv2 proof and every client signature (2.0.2 and 2.1, SPNEGO and raw, signing off, required and asked for, and every refusal), plus seeded fuzz of every decoder and of replayed, damaged server transcripts; the cargo-fuzz target `smbwire` | built |
| Harness server | `tools/smb/test_smbserver.py`: the server's NTLM against the vectors, its DER, and sessions with the host client `smbcat` in every behaviour the guest harness uses | built |
| Real Samba | `tools/smb/samba_interop.py`: Samba 4.19 in Docker; a round trip of every operation with signing off, asked for and required; mandatory signing; `--no-sign` refused; required encryption refused | built |
| SMB e2e | `python tools/smb/run.py`: four transfers (plain, signing required, `--sign`, raw NTLM without a server time): NEGOTIATE picks 0x0210, `ls`, `get` to stdout and checksummed, a 300 KB generated `put`, a `put` of a file the shell wrote, `mkdir`, `mv`, `rm`, `rmdir`, `cd`, `df`; the main server's directory afterwards equals exactly what was sent | built |
| Negative | wrong password (`STATUS_LOGON_FAILURE`), unknown share, `--no-sign` against a server requiring signing, a guest logon, a server demanding encryption, a truncated challenge, a signature-tampered response, an SMB3-only server — each refused for its reason, and the refusing servers' records show no file operation | built, in `run.py` |
| Wire | the capture: no password (ASCII or UTF-16) on any flow, the client offering 0x0210 and the server's choice, the tree path, every message after the logon signed on the signing ports (and no signed request where signing was neither required nor asked for), uploads rebuilt from `WRITE` data alone and absent from the rest of the client's stream, downloads rebuilt from `READ` responses alone | built: `tools/smb/smb_pcap.py` + `test_judge.py` (the judge must fail when it should) |
| Live (opt-in) | `--live --server chatonnas --user chaton` with the password typed (or `LAZYOS_SMB_PASSWORD`); `net mount`, `ls`, `cp` against the real server over bridged networking | manual |

**Leak check.** The password must not appear in `serial.log`, the session
record, the pcap or any committed file — the same scan `tls_run.py` does.
`run.py` makes a random password per run (or takes `LAZYOS_SMB_USER` and
`LAZYOS_SMB_PASSWORD`), types it at `smb`'s prompt with `type_secret`, and
scans every artifact for it raw and in UTF-16LE.

## 10. Staged delivery

Each stage is mergeable and ends with evidence. The mechanism comes first; SMB
is a daemon on top of it.

| Stage | Deliverable | Kernel change | Evidence |
|---|---|---|---|
| **F0** | This plan reviewed; FUSE provider ABI, dialect, crypto and harness pinned | none | this document |
| **F1** (built) | The **FUSE mechanism**: syscall 35, `kernel/src/fs/fuse/`, mount registration and `Vfs::unmount`, `FsError::Io`, `libs/fused`, the `memfuse` toy daemon; correctness + soak tests | **yes**, generic, with full tests | `memfuse` is mounted; `cp`/`ls`/`cat` round-trip byte-exact (`tools/fuse/run.py`); `python tools/test/run.py --accel none` |
| **F2** (built) | `libs/smbwire` (SMB2.1 + NTLMv2 + signing) and the `smb` command; `LAZYOS_SMB=1`, `smb` in the image, `--smb` in `run_demo.py` and the GUI; harness server + `tools/smb/run.py` | none | `SMB:GET`/`SMB:PUT` byte-exact against the host server, judged from its record and the pcap — **G1 reached directly** |
| **F3** | `smbfuse`: the SMB daemon over `libs/fused`, `net mount`/`net ls`, the XUI File Manager **Network** view; a VFS cache expiry for FUSE mounts (§3.1) and the daemon's uid/label rule | the cache expiry (generic) | the share is a directory; `cp` in and out round-trips; the file manager walks it — **G2 reached** |
| **F4** | Name resolution (NBNS, mDNS/LLMNR, DNS ordering) and share listing (`-L` via `IPC$`/`srvsvc`); `--live` against `chatonnas` over a bridge | none | `chatonnas` resolves; `-L` lists the server's shares; live mount and `cp` |
| **F5** | Hardening: shared fenced data plane (remove the bounce copy), attribute timeouts, reconnect, quotas, case-insensitivity rules, long soaks | none | throughput numbers; a soak of reconnects and large trees |
| **F6** | SMB3: 3.1.1 negotiation, preauth integrity, AES-CMAC/GCM signing, encryption policy | none | dialect 0x0311, encrypted share round-trip |

ACL grants land with the actor that needs them: the provider/mount capability
in F1, and `keyd` use in F3, follow the same per-method, kernel-stamped identity
model as `socket.v1` ([security-model.md](security-model.md) §5).

## 11. Risks and open questions

1. **The FUSE provider is still kernel code.** It is generic and small, modeled
   on the block provider, but it is a new syscall and a new backend and must
   pass the correctness and soak suite before SMB leans on it. This is the one
   unavoidable kernel cost; everything else is user space.
2. **The target share name is unknown.** `-L` (F4) or the server's `smb.conf`
   supplies it; F2/F3 use a named share. If `-L` is needed for G1, a slice of
   F4 moves earlier.
3. **NTLM domain.** The wrong domain is the most common `LOGON_FAILURE`; the
   client derives it from the challenge target info and only lets `-W` override.
4. **Reaching a physical `chatonnas` from QEMU.** Slirp reaches only the host;
   a LAN run needs TAP/bridge networking (hard on Windows/WHPX) or real
   hardware. The harness proves the protocol, `--live` proves the server.
5. **Signing policy.** Samba's default does not require signing; some hardened
   shares do. HMAC-SHA256 signing is in F2, so "required" is handled;
   "encryption required" is refused with a clear message until F6.
6. **Clock.** A dead RTC makes NTLMv2 fail as a wrong password; the client must
   detect and say so. An SNTP step (tls-plan T5) helps both.
7. **Native crypto build.** `md-4`/`md-5`/`hmac`/`sha2` are pure Rust and are
   expected to build for `x86_64-unknown-none`; verify in F2. If one fails, the
   crypto moves behind a small in-tree implementation rather than a musl
   workspace, since the mount removes the old reason for a musl client.
8. **Async-only SMB crates.** Published Rust SMB clients assume tokio and
   threads; hand-writing is the decision, and F2 is the bulk of the work.
9. **Remote vs. cached coherence.** A network tree changes under the VFS cache;
   F5's attribute timeouts and invalidation are what keep `ls` honest.

## 12. Non-goals

Writing a filesystem in the kernel (the point of the mechanism is not to),
SMB1/CIFS, SMB over NetBIOS 139, SMB3 encryption and multichannel (F6), serving
SMB (a `smbd` for LazyOS), DFS, shadow copies, oplocks/leases and change
notifications, sparse/compressed files, mailslots, WINS, Active Directory
(Kerberos), NTLMv1 (only NTLMv2 is accepted), and printing over SMB. Each has a
seam above; none is needed for G1.
