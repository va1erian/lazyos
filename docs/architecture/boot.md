# Boot & image build

**What it is.** Host-side build pipeline that compiles the freestanding kernel
and ring-3 programs, packs them into an MBR + FAT boot disk, and launches QEMU.

**Key files**

| Path | Role |
|---|---|
| `Cargo.toml` | Workspace; `kernel` and `user` as artifact dependencies via `bootloader` |
| `.cargo/config.toml` | `bindeps = true` (unstable artifact dependencies) |
| `rust-toolchain.toml` | Pinned nightly; targets `x86_64-unknown-none`, `x86_64-unknown-uefi` |
| `build.rs` | `DiskImageBuilder`: embeds kernel + files; writes `target/lazyos.img` |
| `kernel/build.rs` | Rasterizes `assets/fonts/JetBrainsMono-Regular.ttf`; turns env switches into cfgs |
| `kernel/src/main.rs` | `entry_point!(kernel_main)`; boot order below |
| `src/main.rs` | QEMU launcher; exits 0 only on `isa-debug-exit` 0x10 |
| `user/build.rs`, `user/link.ld` | Static non-PIE ELF at a fixed base, custom entry |
| `tools/run_demo.py` | One-command build + windowed/headless demo |

**Boot order** (`kernel/src/main.rs`)

1. `serial::init` (COM1 logging); framebuffer from `BootInfo`, else halt.
2. `console::init`, `display::init` (records geometry for the #113 grant).
3. `mem::init` - frame allocator, VMA-NX enable, kernel heap mapping.
4. `tests::run()` when built with `LAZYOS_TESTS=1` (replaces normal boot).
5. `fs::init` - mount boot volume (`/`) and ramfs (`/tmp`).
6. `arch::init` - CPU/GDT/IDT/PIC/PIT, Linux syscall MSRs, PS/2 mouse.
7. `task::register_kernel`; bootstrap channel `create()` + registry name publish.
8. Spawn branch: `BUSYBOX` file, or `INIT.ELF` (ABI bench), or the demo/services
   set selected by build flags.
9. `task::start()`, `interrupts::enable()`, `mux::run()` (kernel task).

**Image contents** (`build.rs`)

| File | Source | Condition |
|---|---|---|
| `HELLO.TXT`, `NOTES.TXT` | inline strings | always |
| `HELLO.ELF`, `SH.ELF` | `user` bins | always |
| `MSGCTL.ELF`, `MSGRD.ELF` | `messengerctl`, `messengerd` | always on disk |
| `SUPER.ELF`, `LOGD.ELF`, `HEALTHD.ELF`, `FLAKY.ELF`, `KEYD.ELF`, `CLIPD.ELF`, `CLIPCP.ELF`, `CLIPPS.ELF` | services | always on disk; `init` starts them |
| `ACCTD.ELF`, `LOGIND.ELF`, `MIMED.ELF`, `MIME.TYP`, `PASSWD`, `SYSD.ELF`, `TOP.ELF` | accounts/login/MIME/system stats | only when `LAZYOS_SERVICES=1` |
| `XUID.ELF`, `XDEMO.ELF` | compositor demo | only when `LAZYOS_XUID=1` |
| `DRAGDMO.ELF` | drag & drop demo pair | only when `LAZYOS_XUID=1` |
| `SHELLPRB.ELF` | shell-protocol evidence client | `LAZYOS_XUID=1` + `LAZYOS_SHELLPROBE=1` |
| `XAPP.ELF` | `$LAZYOS_XUI_APP` (static musl xui app from `tools/xui/build.py`) | embedded whenever set; spawned only with `LAZYOS_XUID=1` |
| `INIT.ELF` | `$LAZYOS_INIT` | ABI bench hook |
| `BUSYBOX` | `$LAZYOS_BUSYBOX` | Linux shim demo |

FAT names are 8.3 because the kernel FAT reader resolves short names only
(`kernel/src/fs/fat.rs`).

**Build switches** (`kernel/build.rs` -> `cfg`)

| Env | cfg | Effect |
|---|---|---|
| `LAZYOS_TESTS=1` | `lazyos_tests` | In-kernel test suite instead of demo |
| `LAZYOS_MESSENGERCTL=1` | `messengerctl_demo` | hello window runs `messengerctl` |
| `LAZYOS_MESSENGERD=1` | `messengerd_service` | kernel spawns `MSGRD.ELF` |
| `LAZYOS_SERVICES=1` | `services_mode` | kernel spawns `SUPER.ELF` (`init`) |
| `LAZYOS_XUID=1` | `xuid_demo` | spawns `XUID.ELF` + two `XDEMO.ELF` + `DRAGDMO.ELF` |
| `LAZYOS_SHELLPROBE=1` | `shellprobe_demo` | (with `LAZYOS_XUID=1`) spawns `SHELLPRB.ELF` |
| `LAZYOS_XUI_APP=<path>` | `xui_app` | (with `LAZYOS_XUID=1`) boots `XAPP.ELF` as the display owner *instead of* `xuid`/`xdemo` |
| `LAZYOS_XUI_CLIENT=1` | `xui_client` | (with the two above) boots `xuid` plus `XAPP.ELF` as a compositor client; no `xdemo` |

**Invariants / decisions**

- The kernel renders through the bootloader framebuffer; no scanout mapping yet.
- Effort workarounds on the live path are deliberate: user programs are
  `opt-level = "s"` and stripped because a large ELF slows ATA boot under QEMU
  (`Cargo.toml` profiles).
- Normal boots compile none of the test-only hooks.

**Status.** Working: `python tools/run_demo.py` builds and boots the two-window
demo; the switches above are exercised by the headless CI workflows
(`screenshots.yml`, `xui.yml`, `kernel-tests.yml`, `abi-compat.yml`). The
default image carries a single FAT volume on ATA; no ext2 volume is attached by
any launcher yet.
