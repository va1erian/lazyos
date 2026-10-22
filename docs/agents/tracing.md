# Tracing in full

Companion to [`AGENTS.md`](../../AGENTS.md); covers: The `trace` API, channels, masks and the ad hoc print lint.

## Tracing (`LAZYOS_TRACE`)

**Mandatory: every evidence line goes through the `trace` API of its runtime,
never a print.** An evidence line is anything a harness, a session script or
`dbgctl` reads off the serial port or the program ring: `UP`/`QUIT`/`PASS`/
`FAIL` markers, `UI:RECT`, `TEST:`, `DEV:`, `LABEL:DENY`, ... Do not write
`println!("UP:PASS")`, `eprintln!`, `serial_println!("X:..")` or
`sys::write_str(&format!("X:..."))` for one. A print is also torn by another
task's line and is invisible to `LAZYOS_TRACE=0`, the mask and the budgets.

| You are writing | Use | Sink |
|---|---|---|
| a desktop (xui) app, `lazyrad-os`, `doom`, `emusic`, `quake`, `srb2kart` | `xui_app::trace` | `/dev/console`, one write per line |
| a service or command in `user/` | `user::trace` | the terminal, one write per line |
| the kernel | `kernel/src/trace.rs` | the serial lock |

```rust
use xui_app::trace::{self, Channel};

trace::pass!(Channel::Writer, "SAVE");                 // WRITER:SAVE:PASS
trace::pass!(Channel::Pdf, "OPEN", "{path}:{pages}");  // PDF:OPEN:PASS:/x.pdf:3
trace::fail!(Channel::Lrplay, "BIND", "{error}");      // LRPLAY:BIND:FAIL:code 2
trace::event!(Channel::Golf, "FPS", fps = fps, w = w); // GOLF:FPS fps=.. w=..
trace::line!(Channel::Ui, "RECT x={x} name={name}");   // a line with a shape of its own
if trace::enabled(Channel::Ui) { /* work that only feeds a trace */ }
```

- **Name the channel.** A line's prefix is a `tracefmt::Channel`
  (`libs/tracefmt/src/channel.rs`, one row per prefix); a stray prefix is a
  compile error. A new app gets its row from `tools/xui/new_app.py`; add one by
  hand otherwise and run `python tools/trace/catalog.py`
  ([`docs/trace/channels.md`](../trace/channels.md)).
- **An xui app starts with `launch::run(Channel::X, ...)`**, which prints
  `X:BIND:FAIL` and `X:RUN:FAIL`; `UP` and `QUIT` are `trace::pass!(Channel::X,
  "UP")`. `xui_app::probe` (`UI:RECT`/`UI:WIDGET`), `xui_app::stall`
  (`UI:STALL`) and `lazyrad_os::marker::Markers` are thin layers over it:
  use them, do not reimplement them. A `Markers` panic hook (`install_panic_hook`)
  reports a crash on the console.
- **A line with free text keeps it last and unquoted** (`name=<text>`):
  session scripts parse to the end of the line. Use `trace::line!` for that
  shape, `event!` for `key=value` fields.
- **A diagnostic an operator needs on a PC with no serial port** (`HW:`,
  `EXCEPTION:`, `IRQOFF:`, `DBGD:AUDIT`, a panic, a command's result) is not
  evidence: it stays on `serial_println!`/`sys::write_str`, or
  `trace::print!` for a command's output. When in doubt it is a trace.
- **A tag that can fire in a loop** (a stall, a refusal) gets its own
  `Budget` (`trace::within`); a whole channel is never rate limited if it also
  carries evidence that must not be dropped (`UI:RECT`).
- **Never put a password, token or secret in a trace's arguments**; the lint
  greps the `LOGIN`/`GREETER`/`KEYD`/`ELEVD`/`SMB`/`ACCOUNTS` channels.

`LAZYOS_TRACE=0` compiles the macros out (the arguments stay type-checked, none
is evaluated), `LAZYOS_TRACE_MASK=init,pkgd,-shell` (`trace.mask=` in
`lazyos.cfg`) chooses channels, and `LAZYOS_LABEL_TRACE`, `LAZYOS_USB_TRACE`
and `LAZYOS_UI_PROBE` add theirs. Plan and status:
[`docs/tracing-plan.md`](../tracing-plan.md).

CI checks the common forms. `python tools/trace/check_adhoc.py --ratchet`
fails when a file has more ad hoc prints than `tools/trace/adhoc_baseline.json`
records (a new file's baseline is 0, so a new app or module cannot start with a
print) and when a file has fewer and its entry was not lowered. It finds a
`println!`/`eprintln!`/`serial_println!`/`write_str` whose literal starts with
`PREFIX:`, and any use of `fhs::dev::CONSOLE` in a std app (a private copy of
the old console helper). It cannot see a dynamic prefix
(`println!("{marker}:UP")`) or a `writeln!` to a device: review catches those,
so do not write them. Migrating a file means moving its prints to `trace` and
running `python tools/trace/check_adhoc.py --update-baseline`; never raise an
entry.

```bash
cargo test -p tracefmt -p dbgwire
LAZYOS_TEST_FILTER=trace python tools/test/run.py --accel none
python tools/trace/test_check_adhoc.py
python tools/trace/check_adhoc.py --ratchet
cargo test --manifest-path xui-app/Cargo.toml -p xui-app --lib trace   # Linux/WSL
```
