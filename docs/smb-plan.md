# Network directories and SMB — exploration and plan

Status: **draft, nothing built.** This document is the S0 deliverable: the
protocol, transport, crypto and integration decisions needed before any code,
and the staged path to the goal. It builds on networking stages N0–N5
([networking-plan.md](networking-plan.md) §10.1,
[architecture/networking.md](architecture/networking.md)) and the TLS client
precedent ([tls-plan.md](tls-plan.md)). Nothing about SMB exists in the tree
today: a repo-wide search for `smb`, `cifs`, `samba`, `netbios` and `ntlm`
matches only issue numbers and an unrelated benchmark function.

Scope — one concrete goal and one direction:

1. **G1, file transfer.** From a running LazyOS instance, connect to a LAN Samba
   server over TCP **445**, authenticate as a named user with a password
   (NTLMv2), open a share, and transfer files **byte for byte**, both ways,
   with directory listing. The named target is the server **`chatonnas`**
   (login `chaton`). The harness proves G1 against a scripted SMB server on
   the host using `LAZYOS_SMB_USER` and `LAZYOS_SMB_PASSWORD`; a `--live` run
   uses the real server's password, entered at the prompt or supplied through
   `LAZYOS_SMB_PASSWORD`.
2. **G2, network directories.** A share appears as a browsable directory — a
   `net` namespace the shell and the desktop file manager can walk — and,
   later, a path mounted into the VFS so every program (including Linux ABI
   ones) sees it. G2 is staged after G1; the filesystem layer has no mount
   syscall and no user-space filesystem seam today (§9), so a true mount is a
   separate, larger project.

Related: [networking-plan.md](networking-plan.md),
[architecture/networking.md](architecture/networking.md),
[tls-plan.md](tls-plan.md), [filesystem-plan.md](filesystem-plan.md),
[architecture/filesystem.md](architecture/filesystem.md),
[security-model.md](security-model.md) §5 and §8,
[packages.md](packages.md), [xui-plan.md](xui-plan.md).

## 1. Summary of recommendations

| Question | Recommendation |
|---|---|
| Protocol | **SMB2, dialect 2.1 (0x0210)**, over **Direct TCP port 445**. Not SMB1/CIFS (modern Samba disables it by default), not SMB3 first (AES-CMAC/GCM, preauth integrity and optional encryption are more crypto than G1 needs) |
| Transport | Standard 4-byte NetBIOS session header on 445, then the 64-byte SMB2 header; no NetBIOS name service on the data path |
| Authentication | **NTLMv2** with an NTLMSSP type 1/2/3 exchange inside `SESSION_SETUP`; the NTLM **domain** is taken from the server's challenge target info (falling back to `-W`/`WORKGROUP`) |
| Signing | Implement **HMAC-SHA256 truncated to 16 bytes** (the SMB 2.1 rule). Sign established-session messages when the server requires it (or `--sign-required`), and optionally when asked with `--sign`; never sign the pre-authentication `SESSION_SETUP`. Key exchange (RC4) is off for the first cut |
| Confidentiality | SMB 2.1 signing is **integrity only**: **G1 assumes a trusted LAN**, and transferred file bytes are visible to a passive observer. SMB3 encryption (confidentiality) is deferred to S6 |
| Where it runs | **In the client process**, as TLS does. No SMB code in the kernel and no SMB service that sees plaintext (a `smbd`-style broker only if G2 needs a shared session) |
| Language / target | A **standalone musl `std` workspace** (the `nettls/` shape): `std::net` over the N5 `AF_INET` shim, blocking and single-threaded, because the shim gives each thread its own descriptor table and the shim lacks `select`/`recvmsg` |
| SMB crates | **Hand-write the wire layer**, host-tested and fuzzed like `libs/ftpwire`. The published Rust SMB crates (`pavao`, `smb-rs`, `smbclient`) are async and need a runtime/threads the shim cannot give |
| Crypto | RustCrypto crates in the standalone workspace: **`md-4`** (NT hash), **`md-5`** + **`hmac`** (NTLMv2, HMAC-MD5), **`sha2`** + `hmac` (signing), `getrandom` (client challenge). `libs/crypto` is not used: it has no MD4/MD5 and its `no_std` target cannot link the AES family (§5) |
| Tool | `smb`, a `smbclient`-shaped command (`ls`, `get`, `put`, `mkdir`, `rm`, `-L` to list shares), embedded at `/system/bin/smb` under `LAZYOS_SMB=1` |
| Verification | `python tools/smb/run.py`: a scripted SMB2 server on the host loopback (reachable at `10.0.2.2`), judged from **the server's own file record and the pcap**, plus negative cases; `--live` reaches real `chatonnas` |
| Name resolution | For G1 the server is named by IP or an `/etc/hosts` entry; S3 adds **NBNS (UDP 137)**, **mDNS/LLMNR** and DNS so `chatonnas` resolves on a real LAN (§7) |

## 2. Where we are

| Piece | State today | Evidence |
|---|---|---|
| TCP and UDP for native tools | built (N3): `netd` over smoltcp, parked calls, 16 KiB parcels | [architecture/networking.md](architecture/networking.md) |
| `std::net` for musl programs | built (N5): the kernel `AF_INET` shim pumped by `netd`; `nettls` fetches HTTPS this way | `kernel/src/ipc/inet/`, `nettls/src/lib.rs` |
| Name resolution for musl | `/etc/resolv.conf` written by `netd`, `/etc/hosts` served from `/system/etc`; DNS names only | `kernel/src/process/linux/etcmap.rs`; [tls-plan.md](tls-plan.md) §5.1 |
| A blocking client over the shim | proven: `nettls` (`std::net`, single-threaded, `SyncResolver`) | `nettls/src/` |
| Crypto in the OS workspace | `libs/crypto`: SHA-256, HMAC-SHA256, HKDF, Argon2id only. **No MD4/MD5/RC4/DES/AES**; AEAD crates fail codegen on `x86_64-unknown-none` | `libs/crypto/`, `docs/tls-plan.md` §2 |
| Crypto in a musl workspace | `nettls/crypto` (MIT): `aes-gcm`, `chacha20poly1305`, `sha2`, `hmac`, RustCrypto ECC/RSA. **No `md-4`/`md-5`/`rc4` yet**, but they are RustCrypto `MIT OR Apache-2.0` and add cleanly | `nettls/Cargo.lock`, `tools/nettls/licenses.py` |
| VFS and mounts | a synchronous in-kernel `Filesystem` trait, mounts from `lazyos.cfg` only; **no `mount()` syscall**, no FUSE/9p/virtiofs/netfs seam | [architecture/filesystem.md](architecture/filesystem.md) §"Mounting" |
| A user-space filesystem seam | none. The only ring-3 storage seam is the whole-disk **block provider** (syscall 33), not file-level | `kernel/src/block/provider.rs` |
| A mediated file interface | **prose only**: `os.lazy.fs.reader.v1` is named but has no `.midl` and no implementation | `docs/messenger.md` §"Files" |
| The transfer precedent | `ftp`: passive-mode TCP, host-tested `libs/ftpwire`, judged from the server's record and the pcap | `user/src/bin/ftp/`, `tools/net/run.py` |
| The named target | `chatonnas`, a Samba server; login `chaton`; the live password is supplied at run time; SMB on the usual port 445 | this brief |

## 3. The protocol: why SMB2.1 over 445

SMB reached us in three shapes; only one is worth building first.

| Shape | What | Fit | Verdict |
|---|---|---|---|
| **SMB2, dialect 2.1**, TCP 445 | The `0xFE 'S' 'M' 'B'` header, credits, compound requests, NTLMv2, HMAC-SHA256 signing | Samba's default floor is SMB2 (`server min protocol = SMB2_02`); 2.1 adds large reads/writes and durable handles but no new crypto. Everything G1 needs | **Build this** |
| SMB1 / CIFS | The old `0xFF 'S' 'M' 'B'` header, dialects up to NT LM 0.12, NetBIOS session service | Disabled by default in current Samba (`server min protocol = SMB2`); a security liability | No |
| SMB3 (3.0/3.0.2/3.1.1) | Adds AES-128-CMAC then AES-128/256-GCM signing, preauth integrity hashing, encryption, multichannel, persistent handles | Samba offers it, but it means SHA-512 preauth state, AES-CMAC/GCM and encryption policy before G1 needs any of it | Later (S5) |
| SMB over NetBIOS (139) | SMB2 framed over the NetBIOS session service on 139 | Redundant with 445, which every modern server also listens on | No |

**Why 2.1 and not 2.0.2.** Both are HMAC-SHA256; 2.1 is what Samba, Windows and
macOS negotiate today when 3.x is not offered, and it fixes several 2.0.2
behaviours (large MTU, write coalescing). Offering `[0x0210, 0x0202]` and
taking what the server returns is the safe first cut.

**Why Direct TCP.** On 445 the framing is a 4-byte NetBIOS session header (first
byte `0x00`, a 24-bit big-endian length) followed by exactly that many bytes of
SMB2 message. Compound requests (`NextCommand`) chain several SMB2 headers under
one transport frame; the first cut may send one command per frame and still
interoperate.

## 4. Where it runs

```
  smb  (and, in G2, netfsd)                    a static musl std program
    │  std::net::TcpStream
    ▼
  kernel AF_INET shim (N5)  ⇄  netd  ⇄  netdrv  ⇄  virtio-net     (unchanged)
```

| Option | For | Against | Verdict |
|---|---|---|---|
| **A. In the client process** (library in `smb`) | As TLS: the credentials and the plaintext of the files never leave the process that owns them; a crash is the client's | Each SMB program carries the code | **Chosen for G1** |
| B. A single `smbc` service every app calls | One session per share, credentials entered once, a natural home for G2's mount | One process has every session key and every file's plaintext; needs a new IDL and the account to be passed to it | G2 only, and scoped |
| C. SMB inside `netd` | Fewest hops | `netd` already parses hostile frames; adding NTLM and file semantics removes the split that justified it | Rejected |
| D. SMB in the kernel | A path that "just works" everywhere | A hostile-remote parser in ring 0; the `Filesystem` trait is synchronous and would block kernel tasks on network I/O; weeks of kernel tests | Rejected |

**Why musl `std` and not a native `no_std` tool.** A native tool has no general
file-write syscall, so `put` could only read a file and `get` only write to
stdout or checksum — not a file transfer. A musl `std` program has ordinary
`std::fs`, and the N5 shim already carries `nettls`. The constraints it leaves
are real and stated (§8): one thread per descriptor table, no
`select`/`recvmsg`, 16 KiB per tick per direction, 16 fds. SMB2 is a
request/response protocol over one socket, so these are comfortable.

## 5. The crypto the design needs

NTLMv2 and SMB 2.1 signing need only small, well-understood primitives. None is
in `libs/crypto`, and the OS `no_std` target currently cannot link the AES
family (`libs/crypto/src/wrap.rs` explains the codegen failure), so the client
brings its own, exactly as `nettls` does.

| Need | Where it is used | Crate | Licence |
|---|---|---|---|
| MD4 | `NT_hash = MD4(UTF-16LE(password))` | `md-4` | MIT OR Apache-2.0 |
| MD5 | `NTOWFv2`, `NTProofStr`, `SessionBaseKey` all use HMAC-MD5 | `md-5` | MIT OR Apache-2.0 |
| HMAC | HMAC-MD5 (NTLMv2) and HMAC-SHA256 (SMB2 signing), generic over the digest | `hmac` (`Hmac<Md4/Md5/Sha256>`) | MIT OR Apache-2.0 |
| SHA-256 | SMB 2.1 message signature (truncated to 16 bytes) | `sha2` | MIT OR Apache-2.0 |
| Randomness | 8-byte NTLMv2 client challenge | `getrandom` (already in `nettls`) | MIT OR Apache-2.0 |
| RC4 | Only if NTLM negotiation enables key exchange (client clears the flag, so unused first) | `rc4` (deferred) | MIT OR Apache-2.0 |

The NTLMv2 computation, for the record:

```
NT_hash      = MD4(UTF-16LE(password))
NTOWFv2      = HMAC-MD5(NT_hash, UTF-16LE(uppercase(user) + domain))
NTProofStr   = HMAC-MD5(NTOWFv2, server_challenge || client_blob)
SessionBaseKey = HMAC-MD5(NTOWFv2, NTProofStr)
```

The `client_blob` is a 28-byte header (version, zeroes, timestamp as FILETIME,
8-byte client challenge) followed by the server's target-info `AvPairs` echoed
verbatim and a 4-byte terminator. The **timestamp** needs a plausible wall
clock; a dead RTC (LazyOS falls back to 2026-01-01) breaks NTLMv2 the same way
it breaks TLS, and the client must say so clearly rather than report a wrong
password.

The SMB 2.1 signature is the **first 16 bytes of `HMAC-SHA256(SessionKey,
message)`**, computed with the signature field zeroed. Signing applies only to
messages belonging to an established session (a nonzero `SessionId`): they are
signed when the session requires it (the server's `SecurityMode` or
`--sign-required`), or when `--sign` asks for it, and are otherwise optional.
The `NEGOTIATE` exchange and the initial, pre-authentication `SESSION_SETUP`
are never signed; signing and verification begin once authentication has
yielded the `SessionId` and the `SessionKey`. `SessionKey = SessionBaseKey`
because key exchange is off; if a server ever demands it, SMB3 (S5) is the
answer, not RC4 in 2.1.

**Licence.** `nettls/crypto` is deliberately MIT so a GPL-2.0-only NetSurf port
can link it ([tls-plan.md](tls-plan.md) §3.2). The SMB client is a normal
LazyOS program, but the same hygiene applies: every linked crate must pass
`python tools/nettls/licenses.py` (or a sibling gate for the SMB workspace). All
of the above are `MIT OR Apache-2.0`, which passes under the MIT choice. Nothing
here touches `ring`, `aws-lc` or a plain Apache-2.0-only crate.

## 6. The client library and the tool

### 6.1 Layout

Following `nettls/`:

| Path (proposed) | Role |
|---|---|
| `smb/` | A standalone `x86_64-unknown-linux-musl` workspace, like `nettls/` and `rhai-host/`; the OS workspace never resolves it |
| `smb/proto/` | Pure protocol: transport framing, the SMB2 header and command encoders/decoders, NTLMv2, signing. `no_std`-friendly, host-tested, one fuzz entry |
| `smb/src/` | The blocking session (`Session`, `Tree`, `File`), the `std::net` transport, the `smb` command-line front end |
| `tools/smb/build.py` | Builds the workspace for musl (rust-lld, size-first profile), like `tools/nettls/build.py`; `--require` for CI |
| `build_support/smb_embed.rs` | With `LAZYOS_SMB=1`, copies `target/smb/smb.elf` to `fhs::bin::SMB` and into `.image-manifest` |
| `libs/fhs/src/bin.rs` | A new `SMB = "/system/bin/smb"` constant |
| `tools/run_demo.py` `--smb`, `tools/lazygui/catalog.py` | The `LAZYOS_SMB=1` switch from both front ends, with a `test_catalog.py` case (AGENTS.md) |

The protocol crate being separate and `no_std`-friendly is deliberate: G2's
`netfsd` (a native service) can link it without `std`, and the host tests run
under the existing test tooling.

### 6.2 Session flow against `chatonnas`

1. `TcpStream::connect(("10.0.2.2" | server IP, 445), CONNECT_MS)`.
2. **NEGOTIATE** — dialects `[0x0210, 0x0202]`, `SecurityMode` = signing enabled
   (and required only if `--sign-required`), a random `ClientGuid`, capabilities
   0. The response may carry an empty security buffer or a server `GSS` token
   (NTLMSSP type 1/2); the client does not send its own token here. A dialect
   below 2.0.2 or an encryption-required server is a clean failure.
3. **SESSION_SETUP** twice:
   - send NTLMSSP **type 1** (`NEGOTIATE_MESSAGE`) built from the server's
     flags, `NTLMSSP_NEGOTIATE_KEY_EXCH` **clear**;
   - server replies `STATUS_MORE_PROCESSING_REQUIRED` with a **type 2**
     (`CHALLENGE_MESSAGE`): 8-byte server challenge and the target-info
     `AvPairs`. Read the NTLM domain from `MsvAvNbDomainName`, unless `-W` set
     one;
   - compute `NTProofStr`/blob and send **type 3** (`AUTHENTICATE_MESSAGE`);
   - success yields `SessionId` (and, if signing, the `SessionKey` above).
4. **TREE_CONNECT** to `\\chatonnas\<share>`; response gives a `TreeId`.
5. **CREATE / READ / WRITE / CLOSE** and **QUERY_DIRECTORY** for listing;
   **QUERY_INFO** for size and attributes; **SET_INFO** for rename/delete.
6. **TREE_DISCONNECT**, **LOGOFF**.

Every server-supplied field before it is trusted is checked against the
4-byte/fixed sizes the spec fixes, the way `libs/ftpwire` treats replies; the
host tests include oversized, truncated and contradictory frames.

### 6.3 The `smb` command

```
smb [--sign] [--sign-required] [-p PORT] [-W DOMAIN] [-d DIAG] -U user[%pass] //server/share [cmd ...]
smb -L //server -U user[%pass]                 # list shares (S3)
```

Commands: `ls [path]`, `cd`, `pwd`, `get REMOTE [LOCAL|-|!]`, `put LOCAL
[REMOTE|-g N]`, `mkdir`, `rm`, `rmdir`. `get f -` writes to stdout, `get f !`
checksums, `put -g N` generates a stream, mirroring `ftp`'s conventions so the
harness sink can be the same. Serial markers: `SMB:DIALECT 0x0210`,
`SMB:LOGON user=… domain=…`, `SMB:TREE share=…`, `SMB:LIST n=…`,
`SMB:GET name bytes=N crc=…`, `SMB:PUT …`, `SMB:PASS|FAIL reason=…` — markers
say when, they are never the verdict.

### 6.4 Credentials

The password is a real credential and must not appear in `argv` (visible in
`/proc`), the serial log, a crash message or the pcap. In order:

1. **Prompted, never stored** — `smb` reads it from the Terminal using the
   secret-input mode T4 adds ([tls-plan.md](tls-plan.md) §6.4); the harness
   drives the real keyboard path with a `type_secret` step.
2. **`LAZYOS_SMB_PASSWORD`** for headless `--live`/harness runs only; the value
   is never committed and the leak check scans every artifact.
3. Later, optional **`keyd`-sealed** `~/.config/smb/<server>` (the same
   "obfuscated at rest until #187" caveat as `imapc`).

The harness reads `LAZYOS_SMB_USER`/`LAZYOS_SMB_PASSWORD` from the host
environment and never prints them; no fixed credential is committed.

## 7. Reaching `chatonnas` by name

`10.0.2.2` (QEMU slirp's host alias) is how the **harness** reaches a host
server ([networking-host-access.md](networking-host-access.md) §"10.0.2.2").
On a **real LAN**, `chatonnas` has to resolve, and Samba advertises itself three
ways, none of which the current stack speaks:

| Mechanism | Port | Notes | Plan |
|---|---|---|---|
| DNS / `/etc/hosts` | 53 | Works only if the router knows the name, or the user adds a line | **S3**: `/etc/hosts` support exists; document the entry for `--live` |
| **NBNS** (NetBIOS name service) | UDP 137 | Samba answers broadcast `NBSTAT`/`NAME QUERY`; the classic way a name like `chatonnas` resolves | **S3**: a small broadcast-then-unicast query, host-tested |
| **mDNS** (`_smb._tcp.local`) / **LLMNR** | UDP 5353 / 5355 | What Avahi/systemd-resolved advertise; the modern path | **S3** |
| WINS | UDP 137 unicast to a server | Enterprise; out of scope | No |

The data path never needs NetBIOS once the IP is known: SMB2 over **445** is
plain TCP. So name resolution is a convenience layer over `netd`'s UDP socket,
added in S3; **G1** takes an IP or a hosts entry so it does not depend on it.

A caveat the live run must respect: slirp reaches **only the host**, not the
wider LAN. To talk to a physical `chatonnas`, the guest needs bridged/TAP
networking (or real hardware), which is a launcher/`QEMU` concern (§11), not a
protocol one.

## 8. What the OS already gives, and its limits

| Need | State | Note |
|---|---|---|
| TCP with deadlines | built | `nettls`'s pattern: `SO_RCVTIMEO`/`SO_SNDTIMEO`, non-blocking + `poll` fallback |
| Single-threaded blocking I/O | required | the shim gives each thread its own fd table; SMB2 is one socket, so fine |
| 16 KiB per call / per tick | built | a `READ`/`WRITE` of 16 KiB per round trip; large files take several round trips, acceptable (the link is ~1.6 MB/s) |
| 16 fds per task | tight but enough | one control socket, plus one per tree |
| `std::fs` for `get`/`put` | built (Linux ABI) | this is why the client is musl, not native |
| Wall clock | RTC + PIT, no NTP | NTLMv2 timestamps and SMB2 `SystemTime`; a wrong clock must be reported as such |
| Entropy | kernel CSPRNG + `getrandom` | the NTLM client challenge and `ClientGuid` |
| No `select`/`recvmsg`/`sendmsg` | built limitation | blocking sequential SMB2 needs none; pipelining many requests would |

## 9. G2: network directories

G2 has two levels, and they are very different amounts of work.

**Level 1 — a `net` namespace (service + apps).** A userspace service,
`netfsd`, resolves a caller-scoped SMB credential for each requested `user`
and holds sessions per `(server, share, credential identity)`, rejecting a
mount that asks for a mismatch, and serves a new MIDL
interface, `os.lazy.netfs.v1` (`idl/netfs.midl`, generated by `midlc`,
AGENTS.md):

```idl
interface os.lazy.netfs.v1 {
    method Mount(server: String, share: String, user: String) -> (id: U32);  // parks for caller-scoped prompt/keyd authentication
    method Unmount(id: U32) -> ();
    method List(id: U32) -> (mounts: Array<MountInfo>);
    method Opendir(id: U32, path: String) -> (dir: U32);
    method Readdir(dir: U32) -> (entries: Array<Entry>);   // parks; empty = end
    method Closedir(dir: U32) -> ();
    method Stat(id: U32, path: String) -> (meta: Entry);
    method Open(id: U32, path: String, mode: U32) -> (file: U32);
    method Read(file: U32, max: U32) -> (data: Bytes);      // parks
    method Write(file: U32, data: Bytes) -> (written: U32); // parks
    method Close(file: U32) -> ();
}
```

`netfsd` authenticates on the caller's behalf, prompting through the caller's
Terminal secret mode or unwrapping a `keyd`-sealed credential, and reuses a
session only for the same `(server, share, credential identity)`.

The shell gains `net mount //chatonnas/share`, `net ls <id>:/path`, `net get`,
`net put`; the XUI File Manager gains a **Network** location. This is a real
"network directory" the user can walk and copy in and out of, and it is nearly
all userspace — the only kernel work is whatever `netfsd` needs to be a
long-running service (it is an ordinary task, so perhaps none).

**Level 2 — a real mount at a path.** This is the larger project. The VFS has
no `mount()` syscall and no user-space filesystem seam
([architecture/filesystem.md](architecture/filesystem.md)); the only precedent
is the privileged, whole-disk block provider (syscall 33). A file-level mount
needs all of:

1. a kernel `Filesystem` adapter (`kernel/src/fs/vfs/filesystem.rs`) that
   proxies `lookup`/`read`/`write`/`readdir`/… to `netfsd`;
2. a kernel-originated **synchronous** Messenger call (the same missing piece
   networking stage N5's Linux shim needed) or a new native `netfs_*` syscall;
3. a mount registration path reachable from ring 3 (a `mount`-shaped call
   carrying server/share/credentials), mounting into **both** the native and
   ABI `Vfs` tables, as `mounts.rs`/`late.rs` do;
4. a timeout/error policy: `FsError` has no `Io`/`Timeout` variant and the
   5-second flusher must never block on the network.

Because a network round trip per `stat` is far slower than the VFS assumes,
Level 2 also wants a small attribute/dentry cache with explicit invalidation.
The plan of record: **ship Level 1, design Level 2 behind the same
`os.lazy.netfs.v1`**, and treat the kernel adapter as its own reviewed project.
The mediated `os.lazy.fs.reader.v1` that `docs/messenger.md` already names is
the natural umbrella for it.

## 10. Verification

The verdict is always what the SMB server recorded and what crossed the wire,
never a serial marker alone — the principle of `tools/net/run.py` and
`tools/net/tls_run.py`.

**Harness peer.** A scripted SMB2 server on the host loopback, reachable from
the guest at `10.0.2.2:PORT` with no QEMU forward (slirp maps the gateway to
host loopback). Options, in order:

- **`impacket`'s `smbserver.py`** (Python, MIT): a configurable SMB2 server with
  `-username`/`-password` and a share directory, cross-platform on the dev
  host, and it keeps a request log. First choice for CI.
- **Real Samba in Docker / WSL**, with a pinned `smb.conf` (a `chaton` user, a
  share, `server min protocol = SMB2`, signing off then on). Used by the
  `--services`-style variant to prove interop with the same software as the
  real target.
- The developer's own **`chatonnas`** for `--live`, bridged by choice.

Use a **high host port** (e.g. 1445) so the harness needs no host privilege,
and let `smb -p` take it; the live run uses the usual **445**. (The guest's own
*remote* port draw is unrestricted; only local binds below 1024 are refused.)

| Layer | What | Run |
|---|---|---|
| Host unit | `smb/proto`: framing (length bounds, compound `NextCommand`), header encode/decode, every command, NTLMv2 vectors from `MS-NLMP` test values, signing vectors, hostile/truncated/oversized frames, a seeded fuzz entry shared with `cargo fuzz` | `cargo test --manifest-path smb/Cargo.toml` |
| End to end | `python tools/smb/run.py`: NEGOTIATE picks 0x0210; the configured user logs in; a share connects; `ls` equals the server's directory; `get` of a small text file, a several-hundred-KiB binary and a 0-byte file hash equal to the server's; `put` of generated bytes equals the file the server wrote; `mkdir`/`rm`/rename round-trip | new |
| Negative | wrong password (`STATUS_LOGON_FAILURE`), unknown share, a share needing signing when the client will not, a server demanding encryption, a truncated challenge, a signature-tampered response — each refused, and the server's log shows no file bytes sent | in `run.py` |
| Wire | the pcap's 445 flow: no plaintext password anywhere, the dialect in `NEGOTIATE`, the tree path, upload bytes only inside SMB2 `WRITE` requests and download bytes inside SMB2 `READ` responses, each signed when signing is required or requested (unsigned `WRITE`/`READ` accepted only when signing is optional and not requested) | `tools/smb/` pcap judge, with its own `test_judge.py` (the judge must fail when it should) |
| Live (opt-in) | `--live --server chatonnas --user chaton` with the password typed (or `LAZYOS_SMB_PASSWORD`); `ls`, `get`, `put` against the real server, over bridged networking | manual |

**Leak check.** The password must not appear in `serial.log`, the session
record, the pcap or any committed file — the same scan `tls_run.py` does.

## 11. Staged delivery

Each stage is mergeable and ends with evidence.

| Stage | Deliverable | Kernel change | Evidence |
|---|---|---|---|
| **S0** | This plan reviewed; dialect/transport/crypto/harness pinned | none | this document |
| **S1** | `smb/proto`: framing, SMB2 header, NEGOTIATE, SESSION_SETUP (NTLMv2), TREE_CONNECT, CREATE/READ/WRITE/CLOSE, QUERY_DIRECTORY/INFO, signing; host tests, NTLMv2 and signing vectors, fuzz entry | none | `cargo test`; a scripted server sees dialect 0x0210 and a valid type 3 |
| **S2** | `smb` CLI, `LAZYOS_SMB=1`, `smb_embed.rs`, `--smb` in `run_demo.py` and the GUI; `smb/` workspace and `build.py`; harness server + `tools/smb/run.py` | none | `SMB:GET`/`SMB:PUT` byte-exact against the host server, judged from its record and the pcap — **G1 reached** |
| **S3** | Name resolution (NBNS, mDNS/LLMNR, DNS ordering) and share listing (`-L` via `IPC$`/`srvsvc`); `--live` against `chatonnas` over a bridge | none (over `netd` UDP) | `chatonnas` resolves; `-L` lists the server's shares; live get/put |
| **S4** | `netfsd` + `os.lazy.netfs.v1` (Level 1); shell `net` commands; File Manager **Network** view | none expected | the file manager walks a share and copies in and out; a MIDL-driven ACL test |
| **S5** | Level 2 mount: mediated filesystem seam, kernel `Filesystem` adapter, cache/invalidations, `FsError` I/O/timeout variants | **yes**, with correctness + soak tests | a Linux program `cat`s a file on the mounted share; `python tools/test/run.py --accel none` |
| **S6** | SMB3: 3.1.1 negotiation, preauth integrity, AES-CMAC/GCM signing, encryption policy; large-file and pipelining performance | none | dialect 0x0311, encrypted share round-trip; throughput numbers |

ACL grants land with the actor that needs them, not at the end: `netfsd`'s
interface rules and any `keyd` use follow the same per-method, kernel-stamped
identity model as `socket.v1` ([security-model.md](security-model.md) §5).

## 12. Risks and open questions

1. **The target share name is unknown.** `-L` (S3) or the server's `smb.conf`
   supplies it; S1/S2 use a named share. If `-L` is required for G1, it moves a
   slice of S3 earlier.
2. **NTLM domain.** Sending the wrong domain is the most common
   `LOGON_FAILURE`; the client derives it from the challenge target info and
   only lets `-W` override, but a Samba configured unusually may still need a
   flag the user provides.
3. **Reaching a physical `chatonnas` from QEMU.** Slirp reaches only the host.
   A LAN live run needs TAP/bridge networking (hard on Windows/WHPX) or real
   hardware; the harness proves the protocol, `--live` proves the server.
4. **Signing policy.** Samba's default does not require signing; some hardened
   shares do. HMAC-SHA256 signing is in S1, so "required" is handled;
   "encryption required" is refused with a clear message until S6.
5. **Clock.** A dead RTC makes NTLMv2 fail as a wrong password. The client must
   detect and say "the system clock looks like 2026-01-01", as TLS does. An
   SNTP step (tls-plan T5) helps both.
6. **Crypto build on the pinned nightly.** `md-4`/`md-5`/`hmac`/`sha2` are pure
   Rust and expected to build for musl and for `x86_64-unknown-none`; if the
   OS-target build of the protocol crate fails, the crypto stays in the musl
   workspace and only the framing moves to `libs/`. Verify in S1.
7. **Async-only SMB crates.** Published Rust SMB clients assume tokio and
   threads; the shim forbids both. Hand-writing is the decision, but the effort
   is real (S1 is the bulk of the work).
8. **Level 2 mount size.** The mediated filesystem seam is a kernel project in
   its own right; it is staged last and its absence does not block G1 or
   Level 1.
9. **Password handling.** A leak through a log nobody thought of is the worst
   failure here; the harness scans every artifact, and the live run types the
   password on the real keyboard path.

## 13. Non-goals

SMB1/CIFS, SMB over NetBIOS 139, SMB3 encryption and multichannel (S6),
serving SMB (a `smbd` for LazyOS), DFS, shadow copies, oplocks/leases and
change notifications, sparse/compressed files, SMB1-style mailslots, WINS,
Active Directory (Kerberos) authentication, NTLMv1 (only NTLMv2 is accepted),
and printing over SMB. Each has a seam above; none is needed for G1.
