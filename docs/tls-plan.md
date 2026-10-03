# TLS and modern web services — exploration

Status: **exploratory** (2026-10-03), with three decisions taken: a Gmail test
account with an app password exists, zig is an accepted build dependency, and
the password is typed on the real keyboard path (§6.4). Nothing here is built.
Networking stages
N0–N5 are ([networking-plan.md](networking-plan.md) §10.1,
[architecture/networking.md](architecture/networking.md)), and this document
builds on them.
Scope: the shortest credible path from today's plaintext TCP to two concrete
goals:

1. **G1, the web:** fetch a page over HTTPS from a modern public server (for
   example `https://www.google.com/` or `https://en.wikipedia.org/`), with the
   certificate checked properly.
2. **G2, mail:** log in to Gmail's IMAP service (`imap.gmail.com:993`) with an
   **app password**, list the mailboxes and read the headers of the newest
   messages in `INBOX`.

This is the "TLS" item of platform stage S6 stage 3
([platform-plan.md](platform-plan.md) §4.7) and the TLS non-goal of
[networking-plan.md](networking-plan.md) §12. Serving TLS (server keys held in
`keyd`), HTTP/2 and later, OAuth, and a web browser are out of scope (§11).

Related: [networking-plan.md](networking-plan.md),
[architecture/networking.md](architecture/networking.md),
[security-model.md](security-model.md) §5 and §8,
[linux-abi-plan.md](linux-abi-plan.md), [rust-std.md](rust-std.md),
[rhai-plan.md](rhai-plan.md), [packages.md](packages.md).

## 1. Summary of recommendations

| Question | Recommendation |
|---|---|
| Where TLS runs | **In the client process**, as on every other OS. No `tlsd`: a TLS service would see the plaintext of every application's traffic and parse every server's certificates in one place. `keyd` gets involved only when LazyOS *serves* TLS (§11) |
| Which programs first | **Static musl `std` programs** on the Linux personality (like `rhai`, LazyRAD and the XUI apps). They already have `std::net` over the N5 `AF_INET` shim, so a TLS client is a library choice, not an OS port. Native `no_std` tools come later, if at all (§4.3) |
| TLS library | **rustls 0.23** (TLS 1.2 and 1.3, client, Apache-2.0/MIT/ISC). Crypto provider: **`ring`**, cross-compiled with the zig toolchain the repo already uses for Doom and the Docs app; a pure-Rust RustCrypto provider is the fallback if the C build is a problem (§3) |
| Trust anchors | One PEM bundle at **`/system/etc/ssl/certs/ca-certificates.crt`**, generated at build time from a pinned Mozilla root list, and visible to Linux programs at `/etc/ssl/certs/ca-certificates.crt` (§5.2) |
| HTTP | **`ureq` 3** (blocking HTTP/1.1, rustls inside, redirects, chunked, gzip). No async runtime (§4.2) |
| IMAP | A small blocking client over **`imap-codec`** (a well-fuzzed IMAP4rev1 parser and encoder). Implicit TLS on port 993 only; no `STARTTLS` (§6) |
| Tools | `fetch` (a curl-like `GET`) and `imapc` (`login`, `list`, `select`, `headers`, `logout`), static musl, embedded in `/system/bin` (§7) |
| OS work this needs | Name resolution for musl (`/etc/resolv.conf`), the CA bundle at `/etc/ssl`, socket receive/send timeouts in the shim, a trustworthy wall clock (SNTP), and storage for the app password. No TLS code in the kernel (§5) |
| Verification | `tools/net/run.py --tls`: host TLS servers (HTTPS and IMAPS) under a test CA, judged from what the servers recorded and the capture, including negative cases a correct client must refuse; an opt-in `--live` run against the real services (§8) |

## 2. Where we are

| Piece | State | Evidence |
|---|---|---|
| TCP/UDP, DNS for native tools | built (N3): `netd` over smoltcp, `Resolve` on `stack.v1` | [architecture/networking.md](architecture/networking.md) |
| `std::net` for musl programs | built (N5): kernel `InetSock` objects pumped by `netd` | `kernel/src/ipc/inet/`, `netfix` fixture |
| Reaching the internet | QEMU user networking (slirp) NATs the guest through the host; DNS at 10.0.2.3 forwards to the host's resolver | `tools/net/run.py`; `run_demo.py --net` |
| `/etc` for Linux programs | a **synthetic empty directory**: no `resolv.conf`, `hosts` or `ssl/` | `kernel/src/process/linux/path.rs` (`synthetic_dir`) |
| musl's resolver | cannot work: it reads `/etc/resolv.conf` and finds none | "Not done" in [architecture/networking.md](architecture/networking.md) |
| Socket options | `setsockopt` accepts the usual options **and ignores them**, so `SO_RCVTIMEO`/`SO_SNDTIMEO` do nothing | same |
| Sockets across threads | a thread gets its own descriptor table: a socket opened before `thread::spawn` is not visible in the new thread | same |
| Throughput and latency | about 10 ms per control step (`connect`), at most 16 KiB per socket per direction per tick (about 1.6 MB/s) | same |
| Entropy | kernel ChaCha20 pool behind Linux `getrandom` (seeded from `RDRAND` and timing) and native syscall 26 | `kernel/src/entropy.rs` |
| Wall clock | RTC read once at boot plus PIT uptime; `clock_settime` with `CAP_SYS_TIME`; **no NTP**. A garbage RTC falls back to 2026-01-01 | `kernel/src/wallclock.rs` |
| FPU/SIMD state | `FXSAVE`/`FXRSTOR` per task (x87 + SSE). **No `XSAVE`**, so AVX state is not preserved and `OSXSAVE` is off; AES-NI and `PCLMULQDQ` work (they use XMM registers) | `kernel/src/task/fpu.rs` |
| Crypto in tree | `libs/crypto` (SHA-256, HMAC, HKDF, Argon2id; `keyd`'s primitives). No AEAD, no public-key | `libs/crypto/src/lib.rs` |
| `keyd` | `Verify`, `Sign` (HMAC), `Wrap`/`Unwrap`, `Random`, `Generate`, `List`; no asymmetric keys, no per-user sealing yet | `idl/keyd.midl`, [security-model.md](security-model.md) §8 |
| musl cross builds | `tools/abi/build.py`, `tools/rhai/build.py`, `tools/xui/build.py` (rust-lld, no C); `tools/doom/build.py` and the Docs app compile C/C++ with zig | `tools/xui/zig.py` |

What G1 and G2 need from a TLS client on the wire, today:

| Need | Why |
|---|---|
| TLS 1.2 and 1.3 | 1.3 is what Google and most CDNs negotiate; 1.2 is still the floor many servers run |
| SNI | Mandatory in practice: shared front ends (Google, Cloudflare, Fastly) pick the certificate by name and refuse or misroute without it |
| ECDHE over X25519 and P-256 | The groups modern servers offer first |
| AES-128/256-GCM and ChaCha20-Poly1305 | The only suites a modern server enables |
| ECDSA P-256/P-384 and RSA (PKCS#1 v1.5, PSS) signatures | Leaf and chain signatures in public PKI; Google serves both ECDSA and RSA chains |
| X.509 path building and name checking against a root store | Without it TLS protects against nobody |
| ALPN `http/1.1` | Lets an HTTP/2-preferring server fall back cleanly |
| A wall clock within the certificates' validity | Leaf certificates now live 90 days or less |

## 3. TLS libraries

### 3.1 Survey

| Candidate | What it is | Fit | Verdict |
|---|---|---|---|
| **[rustls](https://github.com/rustls/rustls) 0.23** | Memory-safe TLS 1.2/1.3 in Rust, with `webpki` for path validation; pluggable crypto providers; `std` or `no_std` + `alloc` | Builds unchanged for `x86_64-unknown-linux-musl`; everything in §2's table is supported; the default in `ureq`, `reqwest` and most of the Rust ecosystem; audited | **Use this** |
| rustls + **`ring`** provider | `ring` 0.17: BoringSSL-derived primitives, Rust with C and pre-generated assembly | Fast (AES-NI, `PCLMULQDQ`), the most deployed rustls configuration. Needs a C compiler for the musl target: zig, which Doom and `xui-docs` already depend on | **Default provider** |
| rustls + `aws-lc-rs` | rustls's own default provider | Needs CMake and a large C build; adds FIPS and post-quantum key exchange we do not need yet | No |
| rustls + `rustls-rustcrypto` | Pure-Rust provider over the RustCrypto crates | No C at all, builds with rust-lld like `rhai`; younger and marked experimental, slower without hand-written assembly | **Fallback** if the zig build fights us |
| rustls + `graviola` | Pure Rust plus Rust-embedded assembly, fast | Its x86_64 code assumes AVX2-class CPUs; LazyOS does not save AVX state (§2), so it is unusable until `XSAVE` lands | Not now |
| [embedded-tls](https://crates.io/crates/embedded-tls) | `no_std`, no allocator, TLS 1.3 client only | Certificate verification is partial; no TLS 1.2. Good for a microcontroller talking to one known server, not for the public web | No (for G1/G2) |
| BusyBox `wget` / `ssl_client` | BusyBox's own TLS (`tls.c`) | Does not verify certificates at all; TLS 1.2 with a handful of suites | No; never treat it as secure |
| mbedTLS, BearSSL, OpenSSL through zig | Mature C libraries | A C parser of hostile input in every client, against the code standards in `README.md`; OpenSSL is also large | No |
| Write our own | | A TLS stack is a multi-year security liability | No |

`webpki` (rustls's verifier) is the piece that matters most for security: it is
used by everything above and is the reason not to hand-roll anything here.

### 3.2 Why `ring` and not a pure-Rust provider first

- It is the provider with the most deployment behind it, and the one `ureq`
  ships by default, so we take the tested path rather than the novel one.
- Its assembly uses AES-NI, `PCLMULQDQ` and SSSE3, all of which LazyOS tasks
  can use (XMM state is saved). Under TCG (CI without KVM) the handshake is the
  slow part, and a software-only provider makes it noticeably slower.
- The C toolchain is not new: `tools/xui/zig.py` already finds or installs zig
  0.16, and `tools/doom/build.py` shows a static musl Rust + C link with it.

The cost: the TLS tools need zig to build, so without zig they are skipped with
a warning, exactly as `xui-docs` is. *Decided (2026-10-03):* accepted; zig is
already a build dependency for litehtml and Doom. `rustls-rustcrypto` stays the
documented escape hatch, and the tools do not depend on which provider is used.

One build detail to settle when pinning: `ring` probes CPU features through
`cpuid`, and must not choose an AVX path. It checks `OSXSAVE` before AVX, so a
kernel that leaves `CR4.OSXSAVE` clear is safe, but the ABI bench should carry a
fixture that proves it (§8).

## 4. Where TLS runs

### 4.1 In-process, not a service

```
  fetch     imapc     rhai http::get     a LazyRAD form           (musl std programs)
    │         │            │                  │
    └── ureq / imap client ┴──── rustls + webpki + ring ──────────  in the caller's process
                       │ std::net::TcpStream
                       ▼
             kernel AF_INET shim (N5) ⇄ netd ⇄ netdrv        (unchanged)
```

| Option | For | Against | Verdict |
|---|---|---|---|
| **A. Library in each client** | The model of every mainstream OS; plaintext never leaves the process that owns it; a compromised client exposes only its own connections | Every client carries its own copy (a few hundred KiB) | **Chosen** |
| B. A `tlsd` service clients hand sockets to | One copy of the code; could hold client certificates | One process sees every application's plaintext and parses every server's certificates: the worst place to have a bug. Needs sockets passed between tasks, which neither Messenger replies nor the shim support | Rejected for clients |
| C. TLS in `netd` | | `netd` already parses hostile frames; adding certificates and every plaintext to it removes the split that justified it | Rejected |
| D. Kernel TLS (kTLS) | Fast record layer | A record decryptor in ring 0, for a throughput we cannot use (1.6 MB/s link pump) | Rejected |

`keyd` stays the home of *long-term secrets*. A TLS client has none (its
session keys are ephemeral), so `keyd` is not on the client path. It is for
serving TLS (§11) and for sealing the app password (§6.3).

### 4.2 Blocking, single-threaded clients

The N5 shim gives each thread its own descriptor table (§2), so a socket must be
used by the thread that opened it. That rules out async runtimes that move I/O
across worker threads (tokio's blocking DNS pool, `reqwest`), and favours
blocking, single-threaded clients: `ureq` and a hand-driven IMAP loop. This is
also the simplest thing to reason about. When the shim shares descriptor tables
across `CLONE_FILES` threads, the constraint goes away; nothing here depends on
it staying.

### 4.3 Native `no_std` programs

Native tools (`nc`, `ftp`) use Messenger sockets and have no `std`. rustls does
build `no_std` + `alloc` (with a custom time provider), so a native TLS client
is possible, but there is no reason to build one first: every program a user
would point at a web service (Terminal tools, Rhai scripts, LazyRAD forms, XUI
apps) is already a musl `std` program. Revisit if a native service needs TLS.

## 5. What the OS needs

Each item names the smallest change and where it lives.

### 5.1 Name resolution for musl (blocker for both goals)

`TcpStream::connect("imap.gmail.com:993")` calls musl's `getaddrinfo`, which
reads `/etc/resolv.conf` and queries the listed servers over UDP itself. Today
there is no such file. Options:

| Option | How | Verdict |
|---|---|---|
| **R1. `netd` writes the file** | When DHCP gives resolvers, `netd` writes `/transient/net/resolv.conf` (path from `libs/fhs`); the Linux personality serves `/etc/resolv.conf` from it, as `/etc` is already synthesised | **Chosen**: musl's own resolver over the N5 UDP path, which the `netfix` fixture already exercises (its `sendto`/`recvfrom`/`poll` are what musl uses) |
| R2. Static file in the image | `nameserver 10.0.2.3` | Right only under QEMU slirp; wrong on a real LAN. Acceptable as the first commit, replaced by R1 |
| R3. An NSS-like hook into `stack.v1` `Resolve` | | musl has no NSS; would need a patched libc |

Also serve `/etc/hosts` (`127.0.0.1 localhost`). `netd` must not write the
file with whatever a DHCP server sent: it already validates resolver addresses
(N2), and should write only those.

AAAA answers: slirp forwards the host's DNS, so `imap.gmail.com` resolves to
IPv6 addresses as well. musl sorts them by trying a UDP `connect` per family;
`socket(AF_INET6)` fails with `EAFNOSUPPORT`, so IPv4 sorts first, and `std`
tries the remaining addresses in order anyway. Worth a fixture, not a change.

### 5.2 Trust anchors

- **Source:** the Mozilla root program (CCADB), through the pinned
  `webpki-root-certs` crate (or a checked-in `cacert.pem` with its SHA-256 in
  the build). A build step writes it to `/system/etc/ssl/certs/ca-certificates.crt`
  on the OS volume; the image manifest replaces it on update.
- **Path for Linux programs:** `/etc/ssl/certs/ca-certificates.crt`, served
  from the file above by the same `/etc` mapping as §5.1. `rustls-native-certs`
  (and `SSL_CERT_FILE`) find it there; nothing is compiled into each tool.
- **Size and cost:** about 150 roots, roughly 200 KiB of PEM, parsed per
  process start. Acceptable; a pre-parsed DER bundle is an optimisation.
- **Updates:** roots change a few times a year. A rebuild refreshes them; a
  package-delivered update (`pkgd`) is a later concern.
- **Local roots:** `/system/etc/ssl/certs/local/*.pem` for a user-added CA,
  root-writable only. Not needed for G1/G2.
- **Revocation:** none, as in browsers' default for most certificates
  (short-lived leaves, CRLite and similar are browser-side). Stated, not solved.

G2's chain: Gmail's certificates chain to Google Trust Services roots (GTS Root
R1/R4, also cross-signed by a GlobalSign root), all in the Mozilla list.

### 5.3 A trustworthy wall clock

Certificate validity is checked against `SystemTime::now()`. Under QEMU the RTC
starts at host time, so G1/G2 work from day one in the harness. Elsewhere:

- A machine with a wrong RTC fails every handshake ("certificate not valid
  yet/expired"). The fallback base (2026-01-01) is *before* most current leaves
  were issued, so a dead RTC breaks TLS outright.
- **Recommendation:** an SNTP client in the `timed` service (it already owns
  time policy), off by default, steps the clock once at boot through
  `clock_settime` (it would need `CAP_SYS_TIME`, which `timed` does not hold
  today). Plain SNTP is
  unauthenticated; it is a correctness aid, not a security control. A sanity
  floor (never earlier than the image's build date) catches a dead RTC without
  any network.
- Clients report a clock error clearly ("system clock is 2026-01-01, the
  certificate is valid from 2026-09-12"), because otherwise the failure looks
  like an attack.

### 5.4 Socket timeouts

`ureq` and any IMAP client rely on `set_read_timeout`/`set_write_timeout`
(`SO_RCVTIMEO`/`SO_SNDTIMEO`). The shim accepts and ignores them, so a silent
server hangs a client forever. Implement both on `Fd::Inet` (the underlying
socket pair already has deadline-aware waits for `poll`). This is kernel code:
correctness tests (blocking read times out with `EAGAIN`, a timeout of zero
means none, hostile `timeval`s) and a soak in `kernel/src/tests/linux_suite/`,
and `python tools/test/run.py --accel none` must pass. Until it lands, clients
can use non-blocking sockets and `poll`, which work today.

### 5.5 Things that are already good enough

- **Entropy:** `getrandom` from the kernel CSPRNG is what rustls needs for
  nonces and key shares. Note it in the plan: if `RDRAND` is missing on a real
  machine, early-boot seeding is weaker; TLS clients start long after boot.
- **Throughput:** 1.6 MB/s is plenty for pages and mail headers. A 10 MiB
  download takes several seconds; acceptable.
- **Latency:** a TLS 1.3 handshake is one round trip plus `connect`; the
  tick-driven pump adds tens of milliseconds, invisible next to the WAN.
- **Memory:** a static rustls + ring + ureq binary is a few MiB; record
  buffers are 16–32 KiB per connection.

## 6. Gmail IMAP with an app password (G2)

### 6.1 What Google requires

- **Endpoint:** `imap.gmail.com`, port **993, implicit TLS** (TLS from the
  first byte). Port 143 with `STARTTLS` exists but is not worth supporting.
- **Authentication:** an **app password** is a 16-letter credential Google
  generates for one application; it requires 2-Step Verification on the
  account. It is used with `LOGIN user app-password` or `AUTHENTICATE PLAIN`
  over TLS. Google displays it in four groups separated by spaces; the client
  should strip the spaces before sending.
- **Availability caveat:** Google has been retiring password-based access for
  third-party apps. Personal accounts with 2-Step Verification can still create
  app passwords; Google Workspace accounts may have them disabled by their
  administrator, in which case only OAuth 2.0 (`AUTHENTICATE XOAUTH2`) works.
  OAuth needs a browser-based consent flow and a registered client id, which is
  out of scope here (§11); the IMAP client should be written so `XOAUTH2` is a
  second `Authenticate` variant later, not a rewrite.
- **Limits:** Gmail caps simultaneous IMAP connections per account (about 15)
  and bandwidth per day; one connection per command run is fine.

### 6.2 The client

IMAP is a line protocol with tagged responses, untagged data, and **literals**
(`{123}\r\n` followed by exactly 123 bytes), which is where hand-written parsers
break. Use a parser that has been fuzzed:

| Choice | Notes | Verdict |
|---|---|---|
| **`imap-codec`** (+ `imap-types`) | Complete IMAP4rev1 grammar (and many extensions) as a parser *and* encoder, fuzzed, no I/O of its own; we drive it from a blocking loop over the rustls stream | **Use** |
| `imap` 3.x (alpha) over `imap-proto` | A whole blocking client; long in alpha | Reasonable alternative |
| Hand-written, like `libs/ftpwire` | Small for `LOGIN`/`SELECT`, but literals, quoting and `FETCH` responses make it a parser of the same size as a library's | No |

The encoder matters as much as the parser: a password or mailbox name holding
`"`, `\` or CRLF must become a quoted string or literal, never splice a second
command (the same injection rule `ftpwire::command` enforces).

Commands for G2, in order: greeting, `CAPABILITY`, `LOGIN` (or `AUTHENTICATE
PLAIN` when `LOGINDISABLED` is absent and `AUTH=PLAIN` is advertised), `LIST ""
"*"`, `EXAMINE INBOX` (read-only, so nothing is marked read), `FETCH n-m
(UID FLAGS INTERNALDATE BODY.PEEK[HEADER.FIELDS (FROM SUBJECT DATE)])`,
`LOGOUT`. Server text that reaches the console is reduced to printable
characters (RFC 2047 encoded words decoded first), as `ftp` does.

### 6.3 Where the app password lives

The app password is a real credential: anyone holding it can read the mailbox.
In order of preference:

1. **Prompted, never stored** (first cut): `imapc` reads it from the Terminal,
   typed on the keyboard (§6.4). It must never appear in argv (visible in
   `/proc`), in an environment variable, in the serial log, on screen, or in a
   crash message.
2. **Sealed by `keyd`** (second cut): `imapc login --save` stores
   `keyd.Wrap(key, password)` in `~/.config/imapc/<account>` (0600), and
   `Unwrap` returns it on the next run. This uses `keyd` as it exists today; it
   becomes strong when `keyd`'s keys are scoped to their owner (issue #187) and
   sealed per user at login ([security-model.md](security-model.md) §8). Until
   #187, any caller can unwrap with a known key id: document it as
   "obfuscated at rest", not "protected".
3. **A secrets service** (`os.lazy.secrets.v1`: `Store(service, account,
   secret)`, `Lookup`, `Delete`, served by `keyd`, scoped by the caller's
   label) when a second application needs the same thing. Out of scope until
   then; listed so (2) does not grow into an ad-hoc one.

### 6.4 Typing the password: the keyboard path

*Decided (2026-10-03):* the password reaches `imapc` the way a user would give
it, typed into the desktop Terminal, and the live test drives that same path
with QMP key events (§8). The tree is not ready for this yet:

- **The Terminal logs every submitted line.** `xui-term` prints
  `TERM:CMD:<line>` to serial for each line sent to the child
  (`xui-app/src/bin/term.rs`), so a typed password lands in `serial.log` today.
- **There is no tty.** The child's standard input is a pipe; `TCGETS` returns a
  zeroed termios and `TCSETS` is refused (`kernel/src/process/linux/misc.rs`).
  Password-prompt crates (`rpassword` and the like) turn `ECHO` off with
  `tcsetattr` and fail here. Echo comes from BusyBox's line editor, so while a
  program reads its stdin, typed characters are not echoed by anyone; that is
  accidentally right for a password and wrong for every other prompt.

Smallest change that makes the real path safe, before T4:

1. **A secret-input mode in the Terminal.** The child asks for it with a
   private escape sequence on its output (`ESC ] 7770 ; secret ST`, ended by
   `ESC ] 7770 ; normal ST` or by the next submitted line). While it is on,
   the Terminal echoes nothing and **prints no `TERM:CMD`** for that line
   (a `TERM:SECRET:<length>` marker instead, so a session can still wait for it).
   A host test of the grid/marker code proves the line never reaches the log.
2. **`imapc` prompts through it**: writes the prompt, turns secret mode on,
   reads one line, turns it off, wipes its buffer (`zeroize`) once the login
   command is encoded.
3. **Later, a real pty layer** (termios with `ECHO`/`ICANON`, a controlling
   terminal) replaces the escape sequence, and `imapc` switches to `tcsetattr`.
   That is the right end state and a larger kernel project; it is not needed
   for G2.

The same prompt works on the text console only once the console has an
equivalent mode; G2 targets the desktop Terminal.

## 7. The tools

Both are static musl programs in one new standalone workspace (say
`nettls/`, built by `tools/nettls/build.py` like `rhai-host/` and
`tools/rhai/build.py`), embedded as `/system/bin/fetch` and `/system/bin/imapc`
(names from `libs/fhs`), with a `LAZYOS_TLS=1` image switch and `--tls` in
`run_demo.py` and the GUI launcher (AGENTS.md "launchable from both front
ends"). Neither is a desktop app, so no core package is needed.

| Tool | Does | Notes |
|---|---|---|
| `fetch [-I] [-o FILE] [-L] [--max-redirs N] [--timeout S] [-v] URL` | `GET` over HTTP or HTTPS; prints the body (or headers with `-I`); `-v` prints the negotiated TLS version, cipher suite, ALPN and the chain's subjects | `ureq` 3 with rustls; sends `Accept-Encoding: gzip`, decodes it (`flate2` with the pure-Rust backend); refuses `https`→`http` redirects; a body cap (say 64 MiB) |
| `imapc [--host H] [--port P] [--user U] CMD` | `login` (check credentials), `list`, `headers [MAILBOX] [N]`, `logout`; prints one line per message | §6; `--insecure` does not exist |
| `TLS:*` serial markers | `TLS:HANDSHAKE version=… suite=… sni=…`, `TLS:FAIL reason=…`, `IMAP:LOGIN ok`, `IMAP:HEADERS n=…` | so the harness knows when to judge; never the evidence on their own |

Later consumers, each a thin layer over the same two crates:

- **Rhai:** an `http` module in `libs/rhai-lazy` (`http::get(url)` returning
  status, headers and body; `http::post`) behind a permission, so scripts and
  LazyRAD forms can call web APIs.
- **XUI apps:** a mail viewer, a feed reader. The Docs app could open `https://`
  Markdown.

## 8. Verification

As with networking: serial markers say *when*, the evidence is what the peers
saw and what crossed the wire.

**Harness peers** (`tools/net/hostpeers.py`, Python `ssl`):

- A **test CA** and leaf certificates generated by the harness at run time
  (with the `cryptography` package, or a small Rust host tool using `rcgen`),
  installed in the image as an extra root through a test-only build switch
  (`LAZYOS_TLS_TEST_CA=path`), never in a normal image.
- An **HTTPS server** that records the SNI, ALPN, TLS version and cipher suite
  of every handshake and every request line and header; serves a fixed page, a
  chunked page, a gzip page, a 301 chain and a large body.
- An **IMAPS server**: a scripted IMAP4rev1 responder (greeting, `CAPABILITY`,
  `LOGIN`, `LIST`, `EXAMINE`, `FETCH` with literals, `LOGOUT`) that records the
  commands it received and the credentials offered.

| Layer | What | Run |
|---|---|---|
| Host unit | The tools' own logic against in-memory transports: URL and redirect rules, body caps, IMAP command encoding (quoting, literals, CRLF injection refused), response handling, the password never in error text | `cargo test --manifest-path nettls/Cargo.toml` |
| ABI fixture | `tlsfix`: one rustls handshake against the harness server, plus a CPU-feature report (`ring` must not choose an AVX path) | `python tools/abi/run.py` |
| End to end | `python tools/net/run.py --tls`: `fetch` of every page (body hashes equal what the server served), the server saw SNI = the name and ALPN `http/1.1`; `imapc login`, `list`, `headers` (output equals the server's mailbox) | new |
| Negative | The client **must refuse**: an expired leaf, a not-yet-valid leaf, a wrong host name, a self-signed leaf, an unknown CA, a TLS 1.0-only server, a server offering only CBC/RC4 suites, a truncated record, an `https`→`http` redirect. For each, the server's record shows no application data from the client, and `imapc` never sent `LOGIN` | in `--tls` |
| Wire | From the pcap: every flow to the TLS ports starts with a ClientHello carrying the expected SNI; **the password's bytes appear nowhere in the capture**; nothing on 993 is plaintext after the handshake | `tools/net/tls_pcap.py` + `test_tls_pcap.py` (the judge fails when it should) |
| Kernel | `SO_RCVTIMEO`/`SO_SNDTIMEO` and the `/etc` mappings: correctness + soak | `python tools/test/run.py --accel none` |
| Live (opt-in) | `python tools/net/run.py --tls --live`: `fetch https://www.google.com/` and `https://en.wikipedia.org/` (status 200, handshake markers), and, when `LAZYOS_IMAP_USER`/`LAZYOS_IMAP_PASSWORD` are set on the host, `imapc headers INBOX 5` against Gmail. Never in CI; never logged | manual |

**The live IMAP run types the password through the keyboard** (*decided*, §6.4),
so it exercises what a user does: QMP key events into the desktop Terminal.
`qemu_session.py` gains one step kind, `{"type_secret": "LAZYOS_IMAP_PASSWORD"}`,
which reads the named host environment variable and types it with
`QmpClient.type_text`. The value is never in the session script, never printed
by the session's progress lines or its JSON record (the step is recorded as
`type_secret <name> (<length> chars)`), and the step fails if the variable is
unset. The run ends with a leak check: the password's bytes must not appear in
`serial.log`, the session record or the packet capture, and a screenshot after
typing must show nothing after the prompt (the check compares the prompt line's
pixels before and after typing). The account name is
typed with the ordinary `type` step from `LAZYOS_IMAP_USER`.

## 9. Staged delivery

| Stage | Deliverable | Kernel change | Evidence |
|---|---|---|---|
| **T0** | This plan reviewed; library choices pinned; `tlsfix` built with zig (ring) and run on the bench | none | `ABI:tlsfix:PASS` against a harness server, no AVX path |
| **T1** | `/etc/resolv.conf` and `/etc/hosts` (R2 static first, then R1 from `netd`); CA bundle at `/system/etc/ssl` and `/etc/ssl` | the `/etc` mappings, with tests | `netfix` resolves a name with `std`; `tlsfix` validates against the system bundle |
| **T2** | `SO_RCVTIMEO`/`SO_SNDTIMEO` in the shim | **yes**, correctness + soak | kernel suite; a client against a silent server times out |
| **T3** | `fetch`, `LAZYOS_TLS=1`, `--tls` in `run_demo.py` and the GUI, harness HTTPS peer and negative cases | none | `run.py --tls` page hashes and refusals; **G1 reached** with `--live` |
| **T4** | Terminal secret-input mode (§6.4), `type_secret` session step, `imapc` (password typed at a prompt), harness IMAPS peer | none | `run.py --tls` IMAP session judged with the password typed over QMP and absent from every log; **G2 reached** with `--live` against Gmail |
| **T5** | SNTP in `timed` with a build-date floor; `keyd`-wrapped saved password; `http` module for Rhai | none (capability grant for `timed`) | clock stepped from a skewed RTC in the harness; Rhai script fetches a page |

## 10. Risks and open questions

1. **Gmail app passwords may disappear** for some accounts (Workspace policy,
   future Google changes). Then G2 needs OAuth 2.0 and a consent flow, which
   needs either a browser or the device-code flow on another machine.
   *Resolved for now:* a Gmail test account with an app password is available
   for the live run.
2. **The zig dependency** for a core tool. *Resolved:* accepted (zig already
   builds litehtml and Doom); `rustls-rustcrypto` remains the fallback.
3. **Clock trust.** SNTP is spoofable; a network attacker who can move the
   clock back can make an expired, compromised certificate acceptable again.
   Roughtime or NTS would fix it; not needed for the goals.
4. **Threads and sockets** (§4.2). A library that quietly resolves names or
   reads on a helper thread will hang or fail with `EBADF`. Pin to blocking,
   single-threaded clients until the shim shares descriptor tables.
5. **musl resolver details.** musl sends A and AAAA queries in parallel from one
   socket and uses `poll` with a timeout; the N5 UDP path has carried datagrams
   but not this exact pattern. T1's fixture must resolve a real name through
   slirp, not only `localhost`.
6. **Network permission.** Any Linux program can open `AF_INET` sockets today
   (the socket rules allow everyone, [architecture/networking.md](architecture/networking.md)).
   TLS adds no new authority, but tools that hold credentials make it more
   pressing to land N6's per-profile rules.
7. **Getting secrets into the guest for live tests.** *Resolved:* typed on the
   real keyboard path over QMP (§6.4, §8). The remaining risk is a leak through
   a log nobody thought of; the leak check scans every artifact the run
   writes, and the password must never be in a committed file or a CI log.
8. **Certificate bundle freshness.** An image built today and booted in two
   years may lack a new root that a server switched to. Accept; document how to
   rebuild.

## 11. Non-goals

Serving TLS (and with it asymmetric keys in `keyd`, TLS server sessions through
`keyd` as [security-model.md](security-model.md) §8 envisages), mutual TLS for
remote Messenger ([messenger.md](messenger.md)), HTTP/2 and HTTP/3/QUIC,
OAuth 2.0 and `XOAUTH2`, SMTP sending, `STARTTLS`, certificate revocation,
post-quantum key exchange, a web browser (`xui-docs` renders local Markdown with
litehtml; fetching and rendering arbitrary pages is a different project), and
BusyBox's TLS. Each has a seam above; none is needed for G1 or G2.
