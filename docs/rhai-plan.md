# Plan: Rhai as LazyOS's primary scripting, application and shell language

**Goal:** make [Rhai](https://rhai.rs) the language LazyOS users and developers
use when they don't write Rust. Concretely:

1. **Shell.** The interactive shell (`SH.ELF`, the Terminal app) *is* a Rhai REPL,
   with a light command syntax for running programs.
2. **Scripting.** `.rhai` files run like programs (`#!/bin/rhai`), can glue
   services together over Messenger, and can run as supervised services.
3. **Applications.** Small GUI apps (Calculator, Settings panes, tray widgets,
   Task Manager views) can be written in Rhai against xui.

Rust stays the language for the kernel, hot-path services (`xuid`,
`messengerd`, `keyd`) and anything that is performance- or security-critical.
BusyBox `sh` on the Linux shim stays as the POSIX-compatibility shell.

This plan replaces the "grow our own Dyon-like interpreter" direction in
[`dyon-feasibility.md`](dyon-feasibility.md) (option C) now that its
prerequisite, a user heap, exists.

## Status (2026-10-01): implementation track

This page was the wiki plan "Rhai in LazyOS"; it now lives here and is kept
current with the code. The target is **scriptable Messenger** (R3) and **Messenger
from LazyRAD scripts**, reached through these steps:

| Step | What | State |
|---|---|---|
| R0 | `rhai` command (static musl), `os` module, REPL, `RHAI.ELF` in the image (#319) | done (#326) |
| Build | `python tools/rhai/run.py` builds `rhai`, BusyBox and the image, boots it and judges the serial markers in one command; `tools/run_demo.py` rebuilds `rhai` before every image | done (#450) |
| R3a | `midlc --schema`: every IDL interface as a data table (`libs/rhai-lazy/src/msg/idl.rs`); `msg` module: `msg::connect(interface)`, method sugar (`confd.info()`), `invoke`, one-way sends, structured service errors as catchable Rhai errors; the real `int 0x80` transport (`msg::gate`) | done (#450) |
| R3b | Topics: `msg::subscribe`, `msg::publish` (declared payload types; the platform wrapper parcel), `msg::on(filter, fn)`, `msg::run([ms])`, `msg::stop()`; services written in Rhai (`msg::serve`) | done (this PR) |
| LazyRAD | `msg` inside LazyRAD form scripts: a `Platform` hook in `lazyrad-runtime` that the LazyOS player (`lazyrad-os`) fills with `rhai_lazy::msg::install` | after R3b; needs the LazyRAD P0 branch ([`lazyrad-plan.md`](lazyrad-plan.md)) |
| R2, R4-R6 | Shell layer, security integration, xui apps, developer experience | later |

Decisions taken while implementing (they refine the sections below):

- **Generated bindings are data, not code.** The plan below says `midlc --emit
  rhai` generates one Rust module per interface. Instead `midlc --schema`
  emits a single table of interfaces, methods, fields, structs, enums and
  topics, and one schema-driven codec in `rhai-lazy` turns Rhai values into
  TLV bodies and back. Same result (every service scriptable, nothing written
  by hand, CI checks the table is current), but it adds no per-interface code
  to the binary, keeps Rhai out of `libs/generated` (which the `no_std` OS
  workspace builds), and the same table can serve `messengerctl call` or the
  MCP bridge later.
- **`invoke`, not `call`.** Rhai reserves `x.call(...)` for function pointers,
  so the generic entry point is `svc.invoke("Method", args)`. The method sugar
  (`svc.list_topics()`) covers the everyday case.
- **Service names.** IDL files name interfaces, not services. `msg::connect`
  tries the name without `.vN` (`os.lazy.confd`), then the full name
  (`os.lazy.display.v1`); `msg::connect(interface, service)` takes an explicit one.
- **No ambient authority.** A script calls with its own process credentials;
  refusals come back from the service and are shown with their errno name and
  text (`os.lazy.confd.v1.Get: path is not readable (EACCES, code 13)`).
- **The gate is only installed on LazyOS**, detected with the Linux `uname`
  syscall (`LazyOS`), so a host build of `rhai` never issues `int 0x80`.

The script-facing reference is [`rhai/msg.md`](rhai/msg.md).

## Status update (2026-09-29): what changed since this plan was written

- **The homegrown shell is gone.** [#254](https://github.com/va1erian/lazyos/issues/254)
  / [#291](https://github.com/va1erian/lazyos/pull/291) deleted `libs/lang`,
  `user/src/lang`, `SH.ELF` and the DOS built-ins. **BusyBox `sh`** (static musl,
  Linux ABI) is now the console and desktop-Terminal shell, and `logind` starts
  it. So "R2 replaces `SH.ELF`" and its parity checklist no longer apply: the
  Rhai shell becomes an **additional, opt-in shell** next to `sh`, and only
  becomes a candidate for the login shell once it has parity and the pty has job
  control ([#162](https://github.com/va1erian/lazyos/issues/162)).
- **Design stance: compose with Unix, don't replace it.** The Rhai host is an
  ordinary command (`rhai -e 'expr'`, `rhai script.rhai`, bare `rhai` for the
  REPL) that reads stdin, writes stdout and sets an exit code, so it works in
  `sh` pipelines (`rhai -e 'svc.list()' | grep x`) and as `#!/bin/rhai`. Process
  plumbing (pipes, job control, redirection) stays with `sh`; scripts get the
  same power through `Cmd`/`sh "…"` (layers 2 and 3 below).
- **The host can now run on the Linux ABI.** The shim passes std, threads,
  epoll, unix sockets, `fork`/`execve` and BusyBox, and `xui-app` already reaches
  Messenger and the display from musl binaries. A `std` (musl) host therefore
  gets the file API, argv/env/cwd, pipes and signals for free, which makes
  P1–P6 below largely moot (musl's `malloc` frees). **Proposed:** start R0 with
  the musl host to unblock quickly, keep `libs/rhai-lazy` `no_std + alloc` so the
  native build stays possible as a size-optimised option, and decide native vs
  musl from the R0 size/boot numbers.
- **Native programs from a script or `sh`** need the `execve` shim tracked in
  [#315](https://github.com/va1erian/lazyos/issues/315) (spawn a native ELF as a
  child and wait); `cmd(["top"])` would go through it. Capturing native output
  through pipes needs native stdout to become a real descriptor (not part of
  #315's minimum).
- **Prerequisite status:** P1 n/a for the musl host; P2 satisfied via the ABI;
  P3 satisfied for Linux-ABI processes; P4 pipes exist (shim); P5 `#!` lookup
  must be verified in the shim's `execve`; P6 signals exist, interactive
  Ctrl-C/job control needs the pty.
- **Suggested reordering:** the highest-leverage step is R3's
  `midlc --emit rhai`, because every current and future service becomes
  scriptable for free. Proposed order: **R0 (musl host, `os` module, REPL) → R3
  (`msg`, generated bindings, `on(topic, fn)` event loop) → R1 polish → R2 shell
  → R4 → R5**. R3 can start with an untyped `msg.request(service, method, #{…})`
  before the generator lands.
- **R5 has a concrete starting point:** the `xui-rhai` crate extracted in
  [lazyrad#78](https://github.com/va1erian/lazyrad/pull/78) (Rhai ⟷ xui forms,
  with an `EngineSetup` trait so other crates register their own functions).
  System bindings plug into the same trait, so one script can both call
  Messenger services and build forms.
- **Scope guard:** keep scripting a thin, capability-checked veneer over
  Messenger. Scripts run with the caller's credentials (no ambient authority),
  engine limits are a second layer, and `eval` stays off in service contexts.

## Why Rhai (and not Dyon, Lua or the homegrown `user/src/lang`)

| Criterion | Rhai | Homegrown `lang/` | Dyon | Lua (`mlua`) |
|---|---|---|---|---|
| Pure Rust, no C toolchain | ✅ | ✅ | ✅ | ❌ C source |
| `no_std` + `alloc` supported upstream | ✅ `no_std` feature | ✅ | ❌ (see feasibility doc) | ❌ |
| Embedding API (register Rust fns/types) | ✅ mature | ❌ none yet | ⚠️ | ✅ |
| Built-in sandbox limits (ops, depth, sizes) | ✅ | ❌ | ❌ | ⚠️ manual |
| Custom syntax hooks (for shell sugar) | ✅ | n/a | ❌ | ❌ |
| Maintenance cost to us | low (upstream) | all ours | fork | C + FFI |

The homegrown interpreter (~720 lines across lexer, parser, interp and value) has
done its job as a proof that ring 3 can host a language. Growing it into a real
language (functions, closures, maps, modules, error handling, an embedding API)
repeats work Rhai already ships and tests.

## Where the Rhai host runs: native `no_std`, not the Linux shim

Rhai can be built two ways on LazyOS:

- **Native** (`x86_64-unknown-none`, `#![no_std]` + `alloc`, the `user` crate
  runtime). This gives direct access to the Messenger client, credentials,
  `spawn`/`wait`, `clock` and the display syscalls, which is everything the
  shell and service scripts need. It runs today, without waiting on further
  `std` work.
- **`std` via the Linux shim** (`x86_64-unknown-linux-musl`). This is required
  for anything that links `xui` (see [`xui-plan.md`](xui-plan.md)).

**Decision:** the Rhai *host* is native. All host bindings live in one
`no_std + alloc` crate so the same bindings also compile into the `std`/xui app
host later (see phase R5).

## Architecture

```text
 .rhai scripts / REPL lines / app bundles
                 │
       ┌─────────▼──────────┐
       │  RHAI.ELF (host)   │  one binary: REPL, script runner, service runner
       │  rhai (no_std)     │
       ├────────────────────┤
       │ libs/rhai-lazy     │  host bindings, no_std + alloc:
       │   os::   proc, fs, clock, creds, env/args
       │   msg::  Messenger calls, topics, services  (hand-written + midlc-generated)
       │   ui::   (R5) xui widget bindings, std host only
       ├────────────────────┤
       │ user runtime       │  sys.rs, messenger.rs, a heap that frees
       └────────────────────┘
```

- **One host binary.** There is no dynamic linking, so every binary that embeds
  Rhai pays for the whole engine. One `RHAI.ELF` that runs the REPL, scripts and
  services keeps the image small; `SH.ELF` becomes a small launcher for it, or an
  alias.
- **`libs/rhai-lazy`.** This crate holds the engine factory, `Engine`
  configuration (limits, `on_print`/`on_debug`, module resolver) and every
  binding module. It is host-testable with `cargo test`, like `libs/messenger`.
- **Generated service bindings.** `midlc` gets a Rhai backend: for each
  interface it emits a Rhai module that registers a function per method and a
  `CustomType` per struct/enum. Every Messenger service is then scriptable with
  no hand-written glue (`let e = msg::connect("os.lazy.echo.v1"); e.echo("hi", 2)`),
  and the script API docs are generated from the IDL, like the rest of the API.

## Prerequisites (OS side)

| # | Need | Why | Status |
|---|---|---|---|
| P1 | **A freeing user allocator** | The bump allocator never frees (`user/src/heap.rs`). Rhai allocates on every evaluation, so a long-running REPL or service would grow without limit. Replace it with a real allocator (`linked_list_allocator`, which the kernel already uses, or `talc`) over `sbrk`; keep the bump allocator as a feature flag for tiny bins. | missing |
| P2 | Native file I/O: `open`/`read`/`write`/`close`/`stat`/`readdir` | `read_file` is whole-file, read-only, 8.3 names. Scripts, modules and the shell (`ls`, `cd`, redirection) need the VFS/ext2 the kernel already has. | kernel has it, native ABI lacks it |
| P3 | `argv`/`env`/`cwd` for native processes | Scripts take arguments; the shell needs `cd`. `service_args` covers services only. | partial |
| P4 | Pipes between native processes | `ls | grep` in the shell. Messenger channels or a kernel pipe object. | exists for the Linux shim |
| P5 | `#!` interpreter lookup in `spawn` | `spawn("tool.rhai")` should exec `RHAI.ELF tool.rhai`. | missing |
| P6 | Interrupt delivery to a native task | Ctrl-C in the REPL must stop a runaway loop (wired to Rhai's `on_progress`). | signals exist for the shim |
| P7 | Long file names in the image | `.rhai` doesn't fit 8.3; scripts belong on ext2. | ext2 exists |

P1 blocks everything else. P2 and P3 block a useful shell; P4 to P7 can land during
R2 and R3.

## Phases

Each phase ends with a demo, a serial-log marker for CI (`RHAI:<name>:PASS|FAIL`,
matching the ABI/test conventions) and QMP screenshots where it is visual.

### R0 — Spike: Rhai boots in ring 3 (S)

- Add `rhai` with `default-features = false, features = ["no_std", ...]` to a
  new `rhai` bin; the hasher seed comes from build config (Rhai's
  `no_std` build needs a fixed/compile-time hash seed; verify the current
  mechanism for the pinned version).
- Swap in the freeing allocator (P1) for this bin.
- Evaluate a hard-coded script printing `RHAI:BOOT:PASS`; embed as `RHAI.ELF`.
- **Measure and record:** stripped ELF size at `opt-level = "s"`/`"z"` + LTO,
  boot-time read cost, heap high-water mark for a 10k-iteration loop.
- **Exit:** the numbers are acceptable, or a feature trim list is agreed
  (`no_custom_syntax`, `no_closure`, `no_module`, `only_i64`, `no_float`
  are all available as levers).

### R1 — `libs/rhai-lazy` and the script runner (M)

- The engine factory with default limits: `set_max_operations`,
  `set_max_call_levels`, `set_max_expr_depths`, `set_max_string_size`,
  `set_max_array_size`, `set_max_map_size`, all tied to the caller's quota class.
- `on_print`/`on_debug` → `write`; `debug` also → `logd`.
- The `os` module: `args()`, `env()`, `exit(n)`, `clock()`, `sleep(ms)`,
  `whoami()`/`creds()`, `spawn(cmd, args) -> Proc`, `Proc.wait()`,
  `read(path)`, `write(path, s)`, `ls(path) -> [#{name, size, kind}]`.
- A custom `ModuleResolver` over the VFS (`import "lib/util" as util;`), with a
  search path of `./`, `~/.lib/rhai`, `/lib/rhai`, and a compiled-AST cache.
- `RHAI.ELF script.rhai args…`; `#!` support (P5).
- **Tests:** host `cargo test -p rhai-lazy` for every binding against a mock
  `sys`; kernel suite unchanged; a boot fixture runs `tests/rhai/*.rhai`.

### R2 — The Rhai shell (M–L)

The shell is Rhai with a command layer on top. A spike against Rhai 1.26.1
(summarised below) settled how that layer is built: Rhai's own grammar expresses
every shell *semantic*, but not bare POSIX *text*. Its tokenizer drops
whitespace and treats `-`, `/`, `*`, `.`, `>` and `&` as operators, so
`ls -l /home/x.rs` becomes `ls - l / home / x . rs`. Shell syntax therefore
lives in three layers, each where it fits.

#### Layer 1 — the prompt pre-parser (interactive lines)

At the prompt, a line (or pipeline segment) that starts with a bare word that
isn't a Rhai keyword, variable or function in scope is a **command**. The shell
sees the line before Rhai does and rewrites command lines into calls on the
layer 2 API; everything else goes to Rhai unchanged.

```text
> ls /home                      # command: spawn ls, stream its output
> let n = ls("/home").len()     # expression: the os::ls binding, returns an array
> cat notes.txt | grep todo     # pipeline of commands
> for f in ls(".") { if f.size > 1000 { print(f.name) } }
> $(date).trim()                # capture a command's stdout as a string
> echo.ping()                   # a Messenger call via generated bindings
> make -j4 &                    # background job
> jobs; fg 1                    # job control built-ins
```

- Word splitting, quoting, globbing, `|`, `>`, `>>`, `<`, `2>&1`, `&&`, `||`,
  `&` and `$(…)` follow POSIX shell rules. `cat a.txt | grep x > out` becomes
  `(cmd(["cat","a.txt"]) | cmd(["grep","x"])) > "out"` followed by `.run()`.
- Escapes for the ambiguous cases: `!ls` always means a command, and `(ls)`
  always means an expression.
- The pre-parser is a pure function (`&str -> Rewritten`) with a table-driven
  host test suite, not ad-hoc code in the REPL loop.
- Because the pre-parser works on whole lines, the `$raw$` terminator issue
  (below) never arises at the prompt.

#### Layer 2 — the `Cmd` type (pipelines built in code)

Scripts build pipelines as values with Rhai's operator overloading on a custom
type, which the spike confirmed works:

```rhai
let p = (cmd(["cat", "notes.txt"]) | cmd(["grep", "todo"])) > "todo.txt";
p.run();                                    // wait, return the exit status
let job = p.spawn();                        // background; returns a Job
for line in cmd(["tail", "-f", "log"]).lines() { … }   // streamed output
let out = cmd(["date"]).capture().trim();   // like $(date)
```

- `|` joins stages; `>`, `>>` and `<` set redirections; `.err_to_out()` covers
  `2>&1`; `.env(k, v)`, `.cwd(p)` configure a stage.
- **Precedence:** Rhai's built-in precedence for existing operators is fixed,
  and `>` binds tighter than `|`, so `a | b > f` parses as `a | (b > f)`. That
  matches shell meaning (the redirect belongs to the last stage), so the
  overloads attach a redirect to the stage it was applied to, and the
  pipeline's output is the last stage's. A host test pins this behaviour.
- A pipe-into-closure operator uses a new custom operator with a precedence we
  choose (`register_custom_operator("|>", …)` was confirmed to work):
  `cmd(["ls"]) |> |line| line.ends_with(".rs")`.
- `.lines()` is a registered Rhai iterator over the process's output pipe, so
  large outputs stream instead of buffering. Process-to-process pipes are wired
  by the OS; Rhai is never in the data path between two commands.

#### Layer 3 — `sh "…"` (pasting shell text into a script)

`sh "ls -l *.rs 2>&1 | wc -l"` runs a shell-syntax string through the layer 1
pre-parser and returns the exit status (`sh_capture "…"` returns stdout). It is
an ordinary function call on a string, so it ends with `;` like any other Rhai
statement.

A **bare** form (`sh ls -l *.rs;` with no quotes) is technically possible:
Rhai's `$raw$` custom-syntax marker (`register_custom_syntax_without_look_ahead_raw`)
hands the parser the following characters one by one, whitespace included, and
the spike captured `ls -l  /home/*.rs 2>&1 | grep 'a b' > out.txt &` byte for
byte. But the raw reader consumes the `;` or newline that ends the command, and
Rhai then still requires a terminator (the spike needed `;;`), because custom
syntax only counts as self-terminated when its last matched symbol is `;`, `}`
or a block. **The bare form is deferred** until that is fixed upstream (a
proposal to let a raw-syntax parser mark itself terminated) or a clean
workaround is found.

#### Process control

None of this needs new syntax. Jobs are OS processes and Rhai is
single-threaded (no async, no generators), so control is through host functions
and handles:

- `spawn` returns a `Job` with `wait()`, `kill(sig)`, `status()` and `pid`;
  `wait_any([j1, j2])` waits on several jobs.
- `&`, `jobs`, `fg`, `bg` and `wait` are shell built-ins over the shell's job
  table. `Ctrl-Z` needs stop/continue signals for native tasks (the Linux shim
  already has them; see P6).
- **Ctrl-C:** the host sets an interrupt flag, and Rhai's `on_progress`
  callback checks it and aborts the running script with a catchable
  `Interrupted` error; a foreground child gets the signal instead.
- `$?`-style status: `last_status()`, and `run()` returns the status, so
  `if cmd(["make"]).run() == 0 { … }` works without special syntax.

#### Rest of R2

- Built-ins that must affect the shell process: `cd`, `pwd`, `exit`, `export`,
  `source`, `jobs`, `fg`, `bg`, `wait`, `help`, `history`.
- Line editing (history, cursor movement, completion of commands, paths and
  in-scope identifiers), multi-line input when the parser reports an incomplete
  block, and `~/.rhairc` at startup.
- Errors print Rhai's position info with a caret under the source line; errors
  from rewritten command lines point at the original text, not the rewrite.
- *(Superseded, see the status update.)* `user/src/lang/` and `SH.ELF` are already
  removed and BusyBox `sh` is the system shell. The Rhai shell ships as an extra
  command; making it the login shell is a later decision gated on parity and pty
  job control.
- **Tests:** pre-parser table tests (quoting, globbing, redirections, `&`,
  `$(…)`, ambiguous words and the `!`/`()` escapes); `Cmd` operator tests
  including the `a | b > f` precedence case; a guest test that pipes
  1 MB through `lines()` with a flat heap high-water mark; a Ctrl-C test that
  interrupts `loop {}` via QMP input.
- **Exit:** a QMP session script (`tools/screenshot/examples/rhai_shell.json`)
  types the sample session above and CI checks the serial markers and screenshots.

### R3 — Messenger scripting and Rhai services (M)

- `midlc --emit rhai` generates `libs/generated/src/rhai/*.rs`; the host
  registers every generated module and exposes them as `msg::<service>`.
- Hand-written `msg` core: `connect`, `call`, `subscribe(topic, |event| …)`,
  `publish`, and an event loop (`msg::run()`) built on `Selector`, so a script can
  wait on several topics and replies.
- **Services in Rhai:** the `init` manifest gains `exec = "RHAI.ELF /srv/foo.rhai"`
  entries; a script declares `fn on_call(method, args)` / `fn on_start()` and the
  host wires heartbeat and shutdown the same way `service!` does. First
  candidates are policy glue and demos, not core services: an automount notifier
  and a `healthd` alert rule.
- `messengerctl` and a new `lazyosctl` gain a `script` subcommand that runs a
  one-liner against the fabric.
- **Exit:** a Rhai echo service and a Rhai client exchange a message on boot
  under `LAZYOS_SERVICES=1`; restart-with-backoff works for a crashing script.

### R4 — Security integration (M)

- A script runs with its process's credentials; there is no ambient authority
  beyond the process. Engine limits are a second, finer layer, not the security
  boundary.
- Script bundles use the existing app manifest (`security-model.md` §6); the
  sandbox profile is enforced by the kernel on `RHAI.ELF`'s process, so
  bindings don't reimplement policy.
- Denied operations surface as a Rhai error that carries the friendly-denial
  text (`ERR_QUOTA` with usage/limit, policy reason), catchable with
  `try`/`catch`.
- `eval` is disabled by default in service and app contexts (`Engine::disable_symbol("eval")`).
- Audit: `logd` records script start/exit with a hash of the entry script.
- **Tests:** a script that exceeds each engine limit is stopped with the right
  error, and a sandboxed script denied a Messenger interface gets a
  friendly error, not a crash.

### R5 — Applications: Rhai on xui (L; depends on the xui-plan milestones)

The app model mirrors `xui_core::app::App` (Elm-style `update(msg, ui)`), with
`Msg = Dynamic`:

```rhai
// calc.rhai
fn init(ui) {
    this.n = 0;
    ui.column([
        ui.label("0", "display"),
        ui.button("+1", || #{ kind: "bump" }),
    ]);
}
fn update(msg, ui) {
    if msg.kind == "bump" { this.n += 1; ui.set_text("display", `${this.n}`); }
}
```

- **xui side (general-purpose, lives in the xui repo):** a `xui-rhai` crate
  that binds the portable widget layer to Rhai. Widget event closures are Rhai
  `FnPtr`s that return the message, which keeps xui's "events map to `Msg`
  through closures given at construction" rule. Design values stay `Dip`, and
  widgets use theme tokens only, so dark mode and the Win95 theme apply to
  script apps for free. Nothing in it is LazyOS-specific, so it also works on
  Windows/Linux/macOS xui apps.
- **LazyOS side:** an app host (`std`, Linux shim) that links `xui` +
  `xui-rhai` + `rhai-lazy` and runs `app.rhai` from a bundle. Until xui runs on
  LazyOS, an interim `ui` module over the native display protocol
  (`display_bind`/`present`/`input_poll`, as `xdemo` does) is enough for
  simple panels.
- First apps: Calculator, a Settings pane, a Task Manager view over
  `messengerctl` data.
- **Exit:** the Calculator runs in a LazyOS window in light and dark themes,
  with screenshots captured by the existing pipeline.

### R6 — Developer experience (S–M, continuous)

- `help(fn)` in the REPL from Rhai's function metadata and IDL doc comments.
- `docs/rhai/`: a language primer for LazyOS, a module reference generated
  from `rhai-lazy` + `midlc`, and a cookbook (file ops, services, apps).
- `rhai --check file.rhai` (compile only) and `rhai fmt` later.
- The MCP debug bridge (`mcp-debug-bridge.md`) gains an `eval_rhai` tool that
  runs a snippet inside the guest. This is debug builds only and gives agents a
  structured query surface instead of OCR over `messengerctl`.

## Language-level decisions

| Topic | Decision | Reason |
|---|---|---|
| Numbers | `i64` + `f64` (Rhai defaults) | Shell math and sizes need integers; the old interpreter was `f64`-only. |
| Strings | Rhai `ImmutableString` | Cheap clones between shell stages. |
| Modules | Allowed, via the VFS resolver | Shared libraries of scripts (`/lib/rhai`). |
| Custom syntax | Not used for the shell in the first release: the prompt uses a pre-parser, scripts use the `Cmd` type and `sh "…"` | Keeps scripts portable Rhai; the `$raw$` form waits on the terminator issue (R2). |
| Operators | Overload `\|`, `>`, `>>`, `<` on `Cmd`; one custom operator `\|>` | Verified in the spike; no change to Rhai's grammar. |
| `sync` feature | Off | User processes are single-threaded; `Rc` is smaller and faster. |
| Config files | Stay declarative (TOML / config registry) | Config must not be Turing-complete; Rhai is for hooks and policy glue, not for manifests. |

## Testing

- **Host:** `cargo test -p rhai-lazy` (bindings against a mock `sys`), pre-parser
  table tests, and `midlc` golden tests for the Rhai emitter (`test_midlc.py`).
- **Guest:** a `LAZYOS_RHAI_TESTS=1` image runs `tests/rhai/*.rhai` and prints
  `RHAI:<name>:PASS|FAIL`, ending with `RHAI:SUMMARY`; wired into
  `tools/test/run.py` or a sibling runner and a CI workflow.
- **Soak:** a script that loops 1M iterations allocating strings/maps, run under
  the freeing allocator, asserts a flat heap high-water mark (this catches a
  regression back to the bump allocator).
- **Visual:** QMP session scripts for the shell (R2) and apps (R5).

## Risks

| Risk | Impact | Mitigation |
|---|---|---|
| Binary size / boot read time | Slower boot, larger image | One shared host binary; feature trimming; measure in R0 before committing. |
| Tree-walking speed + soft-float on `x86_64-unknown-none` | Slow numeric scripts | Keep hot paths in Rust; expose fast Rust built-ins (sort, search, parse); cache compiled ASTs. |
| Allocator swap destabilises existing bins | Regressions across services | Opt-in per bin first (feature flag), then flip the default once the kernel suite and service boot are green. |
| Shell sugar ambiguity (`ls` the command vs. `ls` a variable) | Surprising behaviour | Scope-aware rule plus an explicit escape: `!ls` always means a command, and `(ls)` always means an expression. Documented and tested. |
| Operator precedence is fixed for built-in operators | `a \| b > f` groups as `a \| (b > f)` | Matches shell meaning; the `Cmd` overloads are written for it and a test pins it. New operators (`\|>`) get an explicit precedence. |
| `$raw$` custom syntax consumes its terminator | Bare `sh ls -l` in scripts needs `;;` | Ship `sh "…"` (a string) instead; revisit the bare form upstream. |
| Upstream `no_std` breakage | Blocked upgrades | Pin the version; a CI job builds `rhai-lazy` for `x86_64-unknown-none` on every bump. |
| FPU/SSE state across context switches | Corrupted floats under preemption if user code ever uses SSE | Native bins are soft-float today; confirm, and add an `fxsave`/`xsave` test to the kernel suite before any SSE-using host (the `std` app host). |

## Effort

- R0: days. R1: ~1–2 weeks. R2: ~2–3 weeks (pre-parser, `Cmd`, job control). R3: ~1–2 weeks. R4: ~1 week.
- R5: weeks, gated on the xui-plan milestones (`std` + canvas painter on LazyOS).
- P1 (allocator) and P2 (native file ABI) are the long poles and are useful
  beyond Rhai.

## Smallest first step

**P1 + R0 in one PR:** a freeing user allocator behind a feature flag, and a
`RHAI.ELF` that evaluates `print("RHAI:BOOT:PASS")` in ring 3, with the ELF size
and heap numbers recorded in the PR. Every later phase depends on those numbers.

## Open questions

1. ~~Should `SH.ELF` become the Rhai shell directly?~~ Resolved by #254: BusyBox
   `sh` is the system shell and the Rhai shell ships alongside it.
2. Are core services ever allowed to be Rhai, or only glue/demo services?
3. Which Rhai version to pin, and do we vendor it for reproducible offline builds?
4. For R5, bind xui widgets one by one (explicit, typed) or through a generic
   builder (`ui.widget("button", #{…})`)? Explicit is recommended for error messages and docs.
