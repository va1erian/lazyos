# Tracing plan: one evidence API instead of ad hoc prints

Status: T0, T1 and T2 landed (the inventory and lint, `libs/tracefmt`, the
kernel sink, the no_std services); T3 (std apps) is under way: T3a landed (the
`xui_app::trace` sink, `probe`, `stall`, `launch`, `lazyrad_os::marker`, the
scaffolder and the CI ratchet), the app-by-app migration (T3b) is next.
Owner: whoever picks up the tracking issue.

What T0/T1 settled while landing, where it differs from the text below:

- The mask has 128 bits (two words), not 64: the catalogue is already 100
  channels. `sysinfo` (version 6 when this landed; 7 now) carries the two words; `dbgd`'s `sysinfo`
  lists the names as `trace_channels`.
- A channel can be **opt-in**: `label` is not in the default mask, so an image
  built with `LAZYOS_TRACE=1` alone does not print every refusal; the old
  `LAZYOS_LABEL_TRACE=1` adds it (`default,label`). `trace.mask=` accepts
  `default` as well as `all` and `none`.
- `LAZYOS_TRACE` defaults on (section 4.4); `LAZYOS_TRACE=0` compiles the
  macros out, and the old switches imply it back on.
- Kernel call sites moved: `DEV:`, `FS:`, `MSG:`, `ABI:` (the kernel's own),
  `BOOT:PHASE`/`BOOT:MEDIA`, `LABEL:DENY`, `PERF:` report lines and every
  `TEST:` line (79 sites; `trace::test!`). `SCHED:GUARD:DEMOTED` stays a
  `serial_println!` (a task being demoted is something an operator wants on a
  PC with no `dbgd`; class 2) and `KLOGOP:` is the klog suite's own marker.
- `tools/trace/check_adhoc.py` also greps the arguments of the channels that
  carry a secret (`LOGIN`, `GREETER`, `KEYD`, `ELEVD`, `SMB`, `ACCOUNTS`).
  Counts at T0 (`python tools/trace/check_adhoc.py`): 846 sites in 224 files
  and 92 prefixes: kernel 27, user 408, xui-app 374, lazyrad-os 5, doom 12,
  emusic 6, quake 13, other 1.

What T2 settled while landing:

- `user/src/trace.rs` is the sink; the macros are `user::trace::{pass, fail,
  event, line}` (exported at the crate root under private names so bins can
  use them). The mask is read once, on a program's first line, through a new
  `system_stats` op (`TRACE_MASK`, 16 bytes), not the 20 KiB snapshot.
- `user/build.rs` reads `LAZYOS_TRACE` and the three old switches like the
  kernel's build script does.
- The mechanical move kept every line's text (`trace::line!(Channel::X,
  "REST", args)`); only splits that tore (`write_str("ASYNC:")` then the
  name) became one call. New channels: `Async`, `Theme`, `Wifictl`, `Wlanmd`.
- A channel budget is per channel, so `Usbd` lost its channel-wide rate (it
  would have dropped boot and key-edge evidence); `USBD:DIAG` alone is
  limited, through `trace::within` and a `tracefmt::Budget`.
- `MCP:FABRIC_STATS:` and `MCP:TASK_SNAPSHOT:` are data (JSON the bridge
  parses, longer than a trace line) and stay plain writes, exempt in
  `check_adhoc.py`.
- Verdicts: `tools/usb/run.py` and `tools/net/run.py --netd` pass. The
  shutdown, keyd, crash, accounts, fuse, smb and dbg harnesses need the
  BusyBox build (`tools/abi/busybox.py`) or other tools and were not run in
  this change.

What T3a settled while landing:

- `xui-app/src/trace.rs` is the std sink: `trace::{pass!, fail!, warn!,
  event!, line!, print!}`, `enabled`, `within`/`within_with` (a `Budget` for
  one tag), `Markers` (a `Channel` bound once, with `once` and
  `install_panic_hook`). It writes with one `write_all` to `/dev/console`
  (opened once) and falls back to stdout only when the device cannot be
  opened. `xui-app/build.rs` reads `LAZYOS_TRACE` like `user/build.rs`
  (cfg `lazyos_trace`); the macros test `xui_app::trace::ENABLED`, so a crate
  that depends on `xui-app` (the bins, `xui-webkit`, `lazyrad-os`, `emusic`,
  `doom`) needs no cfg of its own. The mask comes from the kernel's
  `TRACE_MASK` op. **Open for T3b:** the portable crates that `xui-app`
  itself depends on (`writer`, `settings`, `confd-editor`, `pdfview`; 26
  marker sites in the baseline) cannot call `xui_app::trace` (a cycle). They
  need a seam their host bin supplies (an event the bin turns into a trace),
  or `Sink` injection into a crate both can depend on; until T3b picks one,
  their baseline entries only go down.
- A line longer than `LINE_MAX` (512) is cut and ends in `...` (a panic
  message with a big `assert_eq!` value, a long `RUN:FAIL` error); the old
  marker, probe and stall code was unbounded.
- `launch::run` and `launch::finish` take a `Channel`, not a `&str`
  (`<channel>:BIND:FAIL`, `RUN:FAIL`, `RUN:CLOSED` are traces). A new app gets
  its `Channel` row from `tools/xui/new_app.py`; `About`, `Sky`, `Video` and
  `Webkit` joined the catalogue (113 channels). `WEBKIT:W5:UI:*` failures are
  now `WEBKIT:BIND|RUN:FAIL`, which `tools/webkit` already accepted.
- `probe::rect`/`widget` keep their lines byte for byte (`UI:RECT x= y= w=
  h= name=<free text>`: the sessions read `name=` to the end of the line) with
  `line!`, not `event!`, which would quote a name with a space.
  `stall::finish` keeps `UI:STALL kind= ms= at= app= thread= what=` and
  `UI:STALL:SUPPRESSED n= app=`; its per-thread `Budget` is its own.
- **`Channel::Ui` lost its channel-wide rate limit.** T1 gave `ui` the stall's
  40 lines per 10 s, but `ui` also carries `UI:RECT` (`xuid` prints one per
  window since T2, a menu opens fifty rows at once) and `UI:WIDGET`: a session
  that clicks by name would have lost rows silently. Only `label` is limited
  per channel now; a tag that can loop uses `trace::within` with its own
  `Budget`, as `USBD:DIAG` and `UI:STALL` do.
- `lazyrad_os::marker` is two constants (`PLAYER`, `IDE`) over
  `xui_app::trace::Markers`. A panic report is a class 2 diagnostic: the hook
  writes `PANIC:FAIL` and `PANIC:BT` lines whatever the switch and the mask say.
- **Enforcement started early, as a ratchet** (`check_adhoc.py --ratchet`,
  `tools/trace/adhoc_baseline.json`, a CI step): a new file, or a file with
  more ad hoc prints than its entry, fails, and a file that improved must lower
  its entry. T4's `--enforce` replaces it when the baseline reaches zero.
  AGENTS.md makes the API mandatory (section "Tracing" and the "Project
  conventions" bullet), and the `xui-app` skill teaches it.
- Counts at T3a (`python tools/trace/check_adhoc.py`): 515 sites in 124 files
  (the lint now also finds a plain `write_str("X:...")` and any
  `dev::CONSOLE` use in a std app: nine services' lines T2 missed, e.g.
  `mimed`'s selftest, are in the baseline for T3b);
  `xui-app` is 445 now, against 374 at T0 (LazyGolf, LazySky, Video, the
  Terminal rewrite and the Wi-Fi tab landed in between), `games/srb2kart/` 18.

## 1. Why

Every harness in this repository judges the OS by serial lines
(`INIT:AUTOSTART:PASS app=...`, `WRITER:SAVE:PASS`, `TEST:<name>:PASS`,
`UI:RECT x= y= ...`), and `dbgd` ([`dbgd.md`](dbgd.md)) now serves the same
lines to a person or an agent as records (`tag`, `fields`, `text`). The lines
are the product's test and inspection surface, but they are emitted by ad hoc
prints with no common owner:

| Tree | How the line is written | Sites (literal prefixes) |
|---|---|---|
| `kernel/` | `serial_println!("DEV:...")`, `boot_trace::mark`, `label_trace::denied` | 82 (33 of them `TEST:`) |
| `user/` (no_std services) | `sys::write_str(&format!("INIT:..."))` | 332 |
| `xui-app/` (std apps, shell, services) | `println!("CALC:UP:PASS")`, `probe::emit` (opens `/dev/console`), `stall.rs` (rate-limited `println!`) | 389 |
| `lazyrad-os/`, `games/doom/`, `emusic/`, `games/quake/` | `Markers::emit` (opens `/dev/console`), `println!` | 43 |
| `tools/abi/fixtures/` | `println!("ABI:...")` | out of scope: Linux binaries under test |

What this costs today:

- **No off switch.** Every image, including one meant for a real PC
  (`docs/real-pc-boot-plan.md`), carries and prints all of it: binary size,
  serial bandwidth under TCG, and lines that leak paths, ids and session
  numbers from a release image. Only four traces are switchable, each with
  its own mechanism: `lazyos_label_trace` (cfg), `lazyos_perf` (cfg),
  `lazyos_usb_trace` (cfg, passed as an argument to `usbd`), and the UI probe
  (a marker file in `/system/etc` looked up at run time).
- **Three output paths, two of them wrong sometimes.** A std app started from
  a Terminal window has a pty for stdout, so `println!` never reaches serial
  or `dbgd`'s program-output ring; `xui_app::probe` and `lazyrad_os::marker`
  learned to open `/dev/console` instead, every other app did not (a session
  that starts an app from the Terminal cannot judge its `UP` marker, and
  `dbgctl log --source programs` never shows a `UI:STALL` from such an app).
  `writeln!` sends the text and the newline as two writes, and another
  task's line lands in between (`tray.json`, fixed only in `probe.rs`).
- **Two formats by accident, and a third parser.** `PREFIX:STAGE:PASS[:detail]`
  and `PREFIX:EVENT k=v k=v` both exist; details with newlines break the
  one-line rule unless the author remembered to flatten them. `dbgwire`'s
  `logline` module defines, after the fact, what a well-formed line is
  (`TAG:SUB key=value key="a b" words`), and nothing on the emitting side
  guarantees a line parses into the tag and fields a client filters on.
- **Nothing to grep or lint.** A marker is any string that happens to start
  with capitals and a colon, so there is no catalogue of channels, no way to
  tell test evidence from a boot diagnostic, and `check_file_length.py`-style
  enforcement is impossible.

## 2. Goal

One `trace` API with three thin implementations (kernel, no_std user, std),
compiled in by `LAZYOS_TRACE=1` (cfg `lazyos_trace`) and compiled out
otherwise, with the wire format unchanged so that `qemu_session.py`, the
judges and `dbgd`'s clients keep working through the migration; and one
owner of the line grammar, shared by the emitters and `dbgwire`.

Non-goals: replacing `logd` (the product log, delivered over Messenger,
persisted in `/logs`); replacing `klog` (the boot ring the panic screen,
`dmesg` and `dbgd`'s `log.tail source=kernel` read); changing `TEST:` lines'
meaning; touching the Linux ABI fixtures (they are the programs under test,
not LazyOS code).

## 3. What counts as a trace

Three classes of line exist today and the migration must keep them apart:

1. **Test evidence** (becomes a trace, compiled out by default in a release
   image): `UP`/`QUIT` markers, `INIT:AUTOSTART:*`, `PKGD:*`, `WRITER:*`,
   `TERM:OUT`, `WEB:LOAD`, `SHELL:TRAY:*`, `SHELL:NOTIFY:*`, `UI:RECT`,
   `UI:STALL`, `LABEL:DENY`, the `PERF:` report lines, `BOOT:PHASE`,
   `USBD:DIAG`.
2. **Diagnostics an operator needs in a normal image** (stays on
   `serial_println!` or `sys::write_str` and the rings, never a trace): the
   limits table, `ext2: ... was not cleanly unmounted`, `IRQOFF:MAX`,
   `EXCEPTION:*`, the `HW:*` verdicts (`dbgd`'s `hw` method reads them from
   the boot log), panics, `init`'s restart notices, `DBGD:READY|OFF`, and
   the `DBGD:AUDIT` trail (`dbgd-plan.md`, "Security": the audit must exist
   in every image that has `dbgd`, whatever its trace switch).
3. **The kernel suite's `TEST:` protocol**: already compiled in only with
   `lazyos_tests`; it goes through the kernel trace sink (so a torn-line fix
   applies to it) but keeps its own switch, because a test image must report
   whether or not tracing is on.

A line that an operator would want on a real PC with no serial port and no
`dbgd` is class 2. When in doubt it is class 1: the trace sink can be
compiled in for a debugging build, a diagnostic cannot be added to a shipped
one. `usbd`'s `USBD:DUMP:*` snapshot is neither: it is a file
(`/transient/usbd.dump`) that `dbgd` serves, and stays as it is.

## 4. The API

### 4.1 Shared core: `libs/tracefmt` (no_std + alloc, no dependencies)

- `Channel`: a `#[repr(u8)]` enum, one variant per program or subsystem,
  named after today's prefixes (`Init`, `Pkgd`, `Shell`, `Writer`, `Ui`,
  `Label`, `Perf`, `Boot`, `Dev`, `Usbd`, `Emusic`, `Quake`, ...).
  `Channel::prefix()` gives the wire prefix, so the catalogue is the enum.
  Mask bit = discriminant (`u64`; grows to two words if we pass 64).
- **The line grammar, moved here from `dbgwire::logline`** (its `Line`,
  `parse`, `is_marker`, `MAX_FIELDS`, and the quoting rules for a value with
  spaces). `dbgwire` depends on `tracefmt` and re-exports it, so the emitter
  and the parser cannot drift: a line `tracefmt` builds parses back into the
  same tag and fields, and that round trip is a host test.
- Line builders, pure functions that return into a caller's buffer (the
  kernel has no `String` on some paths): `pass(ch, stage)`,
  `pass_with(ch, stage, detail)`, `fail(ch, stage, detail)`,
  `warn(ch, stage, detail)`, `event(ch, name, &[(key, value)])`. Every
  builder flattens control characters in `detail` so a line is always one
  line (`lazyrad_os::marker::flatten` moves here), and `event` quotes a
  value that holds a space or a quote the way `logline` expects.
- `Mask`: parse `trace.mask=init,pkgd,-shell` / `all` / `none` (the
  `limits.rs` style: tolerant, logged, never fatal), `Mask::allows(ch)`.
- `Budget`: the rate limiter `xui-app/src/stall.rs` built for `UI:STALL`
  (N lines per window per thread, then one `<TAG>:SUPPRESSED n=<count>`
  line), generalized so any channel that can fire from a wedged peer or a
  tight loop (`UI:STALL`, `LABEL:DENY`, `USBD:DIAG`) cannot flood the 64 KiB
  ring the evidence lives in (`dbgd-plan.md`, "why a program-output ring").
- `Sink` trait: `fn write_line(&self, line: &[u8])`, one call per line, the
  contract that a line is never torn. Implemented by each runtime.
- Host tests: line shapes, flattening, quoting, the parse round trip, mask
  parsing, the budget, the catalogue has no duplicate prefix, every prefix
  satisfies `is_marker`. `dbgwire`'s `logline` tests move with the code.

### 4.2 The macros

```rust
trace::pass!(Channel::Writer, "SAVE");                  // WRITER:SAVE:PASS
trace::pass!(Channel::Pdf, "OPEN", "{path}:{pages}");    // PDF:OPEN:PASS:/x.pdf:3
trace::fail!(Channel::Lrplay, "BIND", "{error}");        // LRPLAY:BIND:FAIL:code 2
trace::event!(Channel::Ui, "RECT", x = x, y = y, w = w, h = h, name = name);
                                                         // UI:RECT x=.. y=.. w=.. h=.. name=..
trace::line!(Channel::Perf, "{}", report);               // escape hatch, flattened
trace::enabled!(Channel::Ui)                             // bool: skip the work that feeds a trace
```

With `cfg(lazyos_trace)` off every macro expands to
`if false { let _ = format_args!(...); }`: the arguments stay type-checked
so compiled-out call sites cannot rot, and the optimizer removes them. No
argument is evaluated. `enabled!` is `false`, so code that only exists to
feed a trace (the shell measuring menu rows for `UI:RECT`, the stopwatch
around every Messenger call for `UI:STALL`) disappears too.

With it on, a line is built once, the channel mask is checked first
(`Relaxed` atomic load, no lock), the channel's budget is charged, and the
sink writes the line in one call.

### 4.3 The three sinks, and where `dbgd` reads them

| Runtime | Crate | Sink | Mask source |
|---|---|---|---|
| kernel | `kernel/src/trace.rs` | one `serial::_print` of the whole line under the serial lock; in a `lazyos_dbgd` kernel also `klog::push_program` (see below) | `trace.mask=` in `/boot/lazyos.cfg`, read where `limits` are |
| no_std services | `user/src/trace.rs` | one `sys::write` of the whole line to the terminal (services have no pty) | the kernel's mask, read once through `sysinfo` (one new field, no new syscall) |
| std apps | `xui_app::trace` (re-exported by `lazyrad-os`, `doom`, `emusic`, `quake`, `mail`, `web`) | `/dev/console` opened once per process (`OnceLock`), `write_all` of the whole line; stdout only when the device cannot be opened (a host build) | the same `sysinfo` field |

Both userspace sinks end in the kernel's terminal write, which calls
`serial::mirror`: the bytes go to the serial port and, in a `LAZYOS_DBGD=1`
kernel, to the program-output ring that `log.tail source=programs` serves
(`kernel/src/klog.rs`, `push_program`). So every userspace trace is already
visible to `dbgd` once it stops going to a pty; nothing new is needed on the
kernel side for that, and the std sink is `probe::emit` and `marker::emit`
merged, with the file kept open instead of reopened per line.

**Kernel traces go to the program-output ring, not the boot log.** Today
`LABEL:DENY`, `PERF:` and `BOOT:PHASE` are kernel prints and land in `klog`,
where a chatty trace evicts the boot diagnostics `dbgd`'s `hw` and
`log.tail source=kernel` exist to show. The rule becomes: the boot log holds
class 2, the program-output ring holds class 1 from every runtime, serial
shows both in order as it does now. `dbgd` gains nothing but a one-line doc
change ("`programs`: what services, apps and the kernel's traces printed").
`TEST:` lines follow the same rule.

`BOOT:PHASE` keeps its TSC and ring in `boot_trace.rs` and only its print
moves. `PERF:` keeps its `lazyos_perf` hooks; its report lines become
`trace::line!(Channel::Perf, ..)` and need both switches. `UI:STALL` keeps
its stopwatches behind `enabled!(Channel::Ui)` and its budget moves to
`tracefmt::Budget`.

### 4.4 Build switches

- `LAZYOS_TRACE=1` -> cfg `lazyos_trace` in `kernel/build.rs`, `user/build.rs`,
  and `xui-app`/`lazyrad-os`/`doom`/`emusic`/`quake` through
  `tools/xui/build.py` and the package build scripts (`RUSTFLAGS --cfg
  lazyos_trace`, as the HiDPI and probe switches reach them).
- `LAZYOS_TRACE_MASK=init,pkgd` -> `trace.mask=` in `lazyos.cfg`
  (`build_support`, next to `limit.*`). Default `all`.
- The old switches become aliases that imply `LAZYOS_TRACE=1` and add their
  channel to the mask: `LAZYOS_LABEL_TRACE=1` (channel `label`),
  `LAZYOS_USB_TRACE=1` (`usbd`), `LAZYOS_UI_PROBE=1` (`ui`; the marker file
  stays because `qemu_session.py --probe` and the shell's `enabled()` read it
  at run time, and it is what tells a session image from a normal one).
- **`LAZYOS_DBGD=1` implies `LAZYOS_TRACE=1`.** A box with `dbgd` is a
  development box someone wants to read; `UI:STALL`, `USBD:DIAG` and the
  rest of the evidence `docs/dbgd.md` and the `lazyos-dbg` skill tell an
  agent to look for must be there. `LAZYOS_TRACE=0 LAZYOS_DBGD=1` is
  allowed and documented as "inspection of a release image": the rings,
  tasks, devices and files, no evidence lines.
- `run_demo.py --no-trace`, `--trace-mask LIST`; the GUI gets *Tracing* on
  the Advanced tab through `tools/lazygui/catalog.py` (`build_env`,
  `build_plan`, tests in `test_catalog.py`).

**Default** (decision for T4, recommendation below): `LAZYOS_TRACE` defaults
to `1` until every harness sets it explicitly, then the default flips to off
and `run_demo.py` keeps `--trace` on for developers. A hobby OS still wants
its markers in an interactive boot; what must never happen is a harness that
passes only because the default happened to be on.

## 5. Stages

Each stage lands as one pull request, with `python tools/test/run.py
--accel none` and the harnesses it touches green, and leaves every session
script unchanged unless the stage says otherwise.

### T0: inventory and lint in report mode (this document)

- `tools/trace/check_adhoc.py`: finds `serial_println!("UPPER:`,
  `println!("UPPER:`, `eprintln!("UPPER:`, `write_str(&format!("UPPER:`
  outside `libs/tracefmt`, the three sink modules and `kernel/src/tests/`;
  prints a count per file; `--enforce` fails on any hit. CI runs it without
  `--enforce` until T4. Self-test `tools/trace/test_check_adhoc.py`.
- `tools/trace/catalog.py --check`: `docs/trace/channels.md` is generated
  from the `Channel` enum (prefix, channel, which tree emits it, the harness
  that judges it, the `dbgd` source it is read from) and must be current,
  like `fuzz/gen_corpus.py --check`.
- Open the tracking issue with the counts above and the class-2 list from
  §3 for review before anything moves.

### T1: `libs/tracefmt` and the kernel sink

- The crate, its host tests, the `Channel` enum with every prefix in use
  today (so later stages only move call sites). `dbgwire::logline` moves
  in; `dbgwire` re-exports it and `cargo test -p dbgwire` stays green
  (its seeded fuzz keeps running against the parser at its new home).
- `kernel/src/trace.rs`, the cfg in `kernel/build.rs`, `trace.mask=` parsing
  next to `limits`, `sysinfo` exposes the mask (`dbgd`'s `sysinfo` call
  shows it, so an agent can tell a masked channel from a silent one).
- Migrate the kernel's class-1 lines: `DEV:`, `FS:`, `MSG:`, `ABI:`,
  `BOOT:PHASE`, `LABEL:DENY` (its cfg becomes an alias), `PERF:` report
  lines. `TEST:` moves to the sink (bypassing the mask). `EXCEPTION:` and
  `HW:` stay as diagnostics. Kernel traces now reach the program-output
  ring (§4.3).
- Kernel coverage (AGENTS.md "Testing requirement"): `trace_suite` with
  correctness (line shapes from the kernel side, the mask read from
  `lazyos.cfg`, a masked channel writes nothing, a traced line is in the
  program ring and not in the boot ring under `lazyos_dbgd`) and a soak
  (two tasks racing a million lines through the sink: no torn line in a
  serial capture of a bounded window, no leaked lock; a million masked
  lines complete in a bounded time, measured with the TSC; a channel over
  budget prints exactly one `SUPPRESSED` line per window).
- Verify: `LAZYOS_TRACE=0 LAZYOS_TESTS=1` builds and the suite still
  reports; a `LAZYOS_TRACE=0` desktop boots and `grep -E '^[A-Z]+:[A-Z]'`
  on its serial log finds only class-2 lines and `TEST:` lines;
  `python tools/dbg/run.py` is green (the ring now carries kernel traces).

### T2: the no_std services

- `user/src/trace.rs` over one `sys::write`; the cfg in `user/build.rs`.
- Migrate `INIT:`, `PKGD:`, `DND:`, `USBD:` (`lazyos_usb_trace` becomes an
  alias; `usbd` reads the mask instead of an argument; `USBD:DIAG` gets a
  budget), `NETD:`, `NETDRV:`, `MIME:`, `MSGCTL:`, `SHELLPROBE:`, `XUID:`,
  `MOUNTD:`, `AUDIOD:`, `KEYD:`, `SNDD:`, `DEVD:`, `TIMED:`, `CLIP:`, `FTP:`,
  `SMB:`, `LOGIN:`, `ELEVD:` and the rest of the 332 sites. `messengerctl`'s
  `selftest::marker` becomes a `trace::pass!`/`fail!` pair. `DBGD:READY`,
  `DBGD:OFF` and `DBGD:AUDIT` stay as diagnostics (§3).
- Harnesses run as the verdict: `tools/usb/run.py`, `tools/net/run.py
  --netd`, `tools/sound/run.py --services`, `tools/shutdown/run.py`,
  `tools/crash/run.py`, `tools/accounts/run.py`, `tools/keyd/run.py`,
  `tools/fuse/ui_run.py`, `tools/smb/run.py` (it scans the serial log for
  the typed password: the trace must not change what reaches serial),
  `tools/dbg/run.py`, the `tray_resident.json` session.

### T3: the std apps

- `xui_app::trace`, replacing `probe::emit` (the `probe` module keeps
  `enabled()`, `rect()`, `widget()` as thin wrappers over `line!`, which
  keeps the free-text `name=` unquoted),
  `stall.rs`'s printing (its stopwatches stay; `finish` calls `event!`
  through `Budget`) and `lazyrad_os::marker` (its `Markers` type, `once()`
  and `install_panic_hook()` move into `xui_app::trace` so the panic hook
  that writes `PANIC` and `PANIC:BT` lines serves every app).
- Migrate the 389 `xui-app` sites, `games/doom/`, `emusic/`, `games/quake/`, the mail
  and web apps, the Terminal's `TERM:OUT`. `tools/xui/new_app.py` writes
  the new calls.
- Harnesses: `core_apps.json`, `xui_writer.json`, `xui_pdf.json`,
  `xui_archiver.json`, `xui_golf.json`, `doom.json`, `quake.json`,
  `notify.json`, `tools/emusic/run.py`, `tools/mail/run.py`,
  `tools/web/run.py`, `tools/print/run.py`,
  `tools/lazyrad/modplayer_run.py`, `lazyrad_pictures.json`, the
  `tray*.json` sessions, `login_logout.json`, `accounts_setup.json`.
- `UI:STALL` from an app started in the Terminal now reaches
  `dbgctl log --source programs`: `docs/dbgd.md` §3 drops its caveat.

### T4: launchers, CI, enforcement, docs

- `run_demo.py --no-trace` / `--trace-mask`, the GUI control, catalog tests,
  `--dbgd` implying the trace (§4.4).
- Every harness and every session's documented build line sets
  `LAZYOS_TRACE=1` explicitly (`tools/*/run.py`, `AGENTS.md`, the READMEs).
- A new CI job builds a `LAZYOS_TRACE=0 LAZYOS_DESKTOP=1 LAZYOS_NETD=1
  LAZYOS_DBGD=1` image, boots it to the greeter and runs
  `tools/trace/untraced_judge.py`: the boot reaches `init`'s class-2
  "session up" diagnostic, `dbgd` answers `log.tail` for both sources, the
  serial log and the program ring contain no channel prefix, and the boot
  log still has every `HW:` verdict and `DBGD:AUDIT` (so a compiled-out
  trace cannot silently break a boot or an inspection: the race a marker's
  write used to hide now shows here first).
- `check_adhoc.py --enforce` in CI; `AGENTS.md` gains a "Tracing" section
  and the "Project conventions" bullet: markers go through `trace`, never a
  print. `docs/dbgd.md`'s table and the `lazyos-dbg` skill point at
  `docs/trace/channels.md` for which tag lives in which source.
- Flip the default per §4.4 once the job is green for a week.

### T5 (later): a trace ring without `dbgd`

`dbgd` already gives a real PC its evidence over the network (§4.3), so the
standalone ring this plan first proposed is not needed for a box with a
NIC. What remains is a box with neither serial nor network: the
program-output ring could be compiled in by `LAZYOS_TRACE=1` alone (today
`lazyos_dbgd` allocates it) and read back by `syslog(2)` with a new facility
so BusyBox `dmesg` or a `tracectl dump` shows it on screen. Not needed for
anything above.

## 6. Decisions to confirm at T0

1. **Default on or off** (§4.4). Recommendation: on until T4's CI job exists,
   off for images built with no desktop autostart afterwards, `run_demo.py`
   and `--dbgd` on.
2. **Mask at build time only.** `trace.mask=` is read once at boot; no
   runtime toggling, no Messenger interface (tracing must work before
   `messengerd` and must not cost an IPC round trip). A `dbgd` control
   method (`trace.mask`, v2 tier) can come later if a harness needs it.
3. **One enum for all channels** rather than per-crate registration: a
   trace from a package app (`org.lazy.doom`) uses a channel the kernel
   knows, so the catalogue stays complete and a stray prefix is a compile
   error. Packages not in this repository can use `Channel::App` with their
   own second field.
4. **Keep the wire format**, and make `tracefmt` its single owner.
   `dbgwire::logline` moves rather than being duplicated, so a format
   revision later (timestamps, task ids) is one change that both the
   emitters and `dbgd`'s clients see.
5. **Kernel traces in the program ring** (§4.3). The alternative, a third
   ring for kernel traces, costs 64 KiB more and a new `log.tail` source for
   no reader that wants them apart from the services' lines.

## 7. Risks

- **Timing.** A marker's console write is a scheduling point (and in a
  `dbgd` kernel, a ring push with interrupts off). Compiling 800 of them
  out can expose a race a session never saw. T4's untraced boot job and the
  kernel soak exist for this; a hang found there is a real bug to fix, not
  a reason to leave the trace on.
- **The Terminal pty.** Apps that today print to a pty and are judged
  through `TERM:OUT` keep doing so (`TERM:OUT` is the Terminal's own trace);
  apps that print their own markers start reaching serial and the ring for
  the first time, which may double a line a session gates with
  `occurrence`. Check each `occurrence` gate in
  `tools/screenshot/examples/*.json` at T3.
- **Ring pressure.** More lines reach the 64 KiB program ring after T1 and
  T3 (kernel traces, Terminal-started apps). The budget, the mask and the
  `lazyos-dbg` skill's existing caveat ("a chatty boot can push early lines
  out") cover it; `dbgd`'s `dropped` count in a stream says when it happened.
- **Secrets.** A trace in a debugging image prints what it is given, and a
  `dbgd` image serves it over the network; review keeps passwords and
  tokens out of `detail` (the `greeter`, `keyd`, `elevd`, `smb` and
  `accounts` channels get a grep in `check_adhoc.py` for `password`,
  `secret`, `token` in trace arguments; `tools/smb/run.py` already scans
  the serial log for the typed password and stays the verdict).
- **Size of T2 and T3.** 700 mechanical edits across 90 files; split by
  program if a pull request passes 2000 lines, the lint's per-file count is
  the progress bar.
