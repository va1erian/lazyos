# LazyOS

LazyOS is a hobby operating system written in Rust (`no_std`, target
`x86_64-unknown-none`). It aims to be **friendly, transparent, and secure**:
no ambient authority, every privileged action is a capability or a
policy-checked message, and denials are explained rather than silent.

It boots under UEFI, renders to a framebuffer, and runs a preemptive
multitasking kernel with a writable ext2 filesystem, a VFS, and a growing set of
userspace services. It also has a Linux ABI bridge so static
`x86_64-unknown-linux-musl` binaries can run.

## Architecture

```
  apps / xui-app        sh, top, sysmon, fabricmon, ...
  ───────────────────────────────────────────────────────
  system services       init  logd  healthd  keyd  accounts
  (user/src/bin)        clipboardd  mimed  sysmond  xuid (compositor)
  ───────────────────────────────────────────────────────
  messengerd + libs/messenger   capability-based IPC / pub-sub
  ═══════════════════════════════ syscall boundary ═══════════
  kernel                scheduler · memory · VFS/ext2 · Messenger
                        core · signals · Linux ABI · drivers
```

The center of the design is **Messenger**: a kernel-mediated, capability-based
IPC and pub/sub fabric. Processes hold unforgeable handles, talk over channels
and shared buffers, and services are discovered and policy-checked through
`messengerd`. Interfaces are described in IDL (`idl/`) and compiled by
`tools/midlc`.

| Directory | Contents |
|---|---|
| `kernel/` | Scheduler, memory, IPC core, VFS/ext2, drivers, Linux ABI |
| `user/` | Userspace runtime and system services (`user/src/bin`) |
| `libs/messenger/` | Shared Messenger wire types and client library |
| `xui-app/` | XUI toolkit apps (`sysmon`, `fabricmon`, ...) |
| `idl/`, `tools/midlc/` | Interface definitions and their compiler |
| `tools/` | Demo runner, screenshot/input driver, ABI bench, test runner |
| `docs/` | Plans and per-subsystem architecture reference |

Where to read next:

- [`docs/platform-plan.md`](docs/platform-plan.md): current architecture and the staged roadmap (S0-S9).
- [`docs/architecture.md`](docs/architecture.md): terse per-subsystem reference (boot, memory, tasks, filesystem, IPC, processes, display).
- [`docs/messenger.md`](docs/messenger.md), [`docs/security-model.md`](docs/security-model.md), [`docs/linux-abi-plan.md`](docs/linux-abi-plan.md), [`docs/xui-plan.md`](docs/xui-plan.md).

## Prerequisites

- Rust (the pinned toolchain in `rust-toolchain.toml` is installed automatically by `rustup`).
- Python 3.
- QEMU (`qemu-system-x86_64`). On Windows, put `C:\Program Files\qemu` on `PATH`
  or pass `--qemu "C:\Program Files\qemu\qemu-system-x86_64.exe"`.

## Running the demos

Boot the interactive desktop demo with one command:

```bash
python tools/run_demo.py
```

It builds `target/lazyos.img` if needed and boots it in QEMU. WHPX (Windows) or
KVM (Linux) is auto-detected and is several times faster than TCG; disable it
with `--accel none`. Other useful flags: `--no-build`, `--headless`, `--release`,
and `-- --cpu max` to pass extra arguments to QEMU. In the demo, **Tab** moves
window focus and typed input goes to the focused program.

### Scripted sessions and screenshots

LazyOS renders pixels, so verify by looking. Capture a headless screenshot:

```bash
python tools/screenshot/qemu_shot.py --out shots --at 2,5,10 --image target/lazyos.img
python tools/screenshot/pngstats.py shots/*.png --min-nonblack 0.01
```

Drive the guest (typing, clicks, scrolling) from a script and capture the result:

```bash
python tools/screenshot/qemu_session.py --image target/lazyos.img \
    --out shots/demo --script tools/screenshot/examples/multitask_demo.json
```

Ready-made scripts live in `tools/screenshot/examples/` (`cli_demo`,
`services_demo`, `window_demo`, `xuid_wm`, `xui_sysmon`, `xui_fabricmon`, ...).
See [`tools/screenshot/README.md`](tools/screenshot/README.md).

## Testing

```bash
python tools/test/run.py --accel none   # in-kernel unit + stress/soak suite
python tools/abi/run.py --at 8          # Linux ABI conformance bench
```

Every kernel component must ship both **correctness tests** and **stress/soak
tests** in `kernel/src/tests.rs`; see [`AGENTS.md`](AGENTS.md) and
[`tools/test/README.md`](tools/test/README.md). Do not commit `shots/`.

## Code standards

LazyOS is small enough to be understood and audited by one person; keep it that way.

**Security first**
- No ambient authority: privileged operations go through capabilities or
  policy-checked Messenger calls (see the security model).
- Treat every syscall argument, user pointer, and Messenger parcel as hostile:
  validate lengths and ranges, use checked arithmetic, and fail closed.
- Every `unsafe` block is minimal and carries a `// SAFETY:` comment stating the
  invariant it relies on. Prefer safe abstractions at the boundary.
- Bound all per-process resources (memory, handles, fds) with quotas; never
  panic on untrusted input.

**Readable and elegant**
- Small functions with one job, descriptive names, and types that make invalid
  states unrepresentable. Match the style of the surrounding code.
- Comments explain *why* (invariants, trade-offs), not *what*.
- Prefer deleting code to adding it; no dead code or speculative abstraction.
- Keep `cargo clippy` clean.

**File size limit: 500 lines**
- New and modified source files must stay **under 500 lines**. Split by
  responsibility into modules rather than growing a file past the limit.
- Existing files over the limit are being split; see the tracking issue
  [va1erian/lazyos#194](https://github.com/va1erian/lazyos/issues/194). Do not
  make an oversized file bigger; extract a module when you touch it.

## License

GPL-3.0-or-later. See [`LICENSE`](LICENSE). Bundled third-party code keeps its own license: JetBrains Mono (`assets/fonts/OFL.txt`, SIL OFL 1.1) and the vendored `xui-canvas` (MIT). All Cargo dependencies are MIT, Apache-2.0, BSD, Zlib or Unlicense, which are GPLv3-compatible.
