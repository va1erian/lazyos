# Boot & image build

**What it is.** Host-side build pipeline that compiles the freestanding kernel
and ring-3 programs, packs them into an MBR disk (a FAT `/boot` plus an ext2 OS volume), and launches QEMU.

**Key files**

| Path | Role |
|---|---|
| `Cargo.toml` | Workspace; `kernel` and `user` as artifact dependencies via `bootloader` |
| `.cargo/config.toml` | `bindeps = true` (unstable artifact dependencies) |
| `rust-toolchain.toml` | Pinned nightly; targets `x86_64-unknown-none`, `x86_64-unknown-uefi` |
| `build.rs`, `build_support/os_*.rs` | `DiskImageBuilder` for the BIOS part (kernel + `lazyos.cfg`); every other file goes to the ext2 OS volume, created or updated in place; writes `target/lazyos.img` ([filesystem.md](filesystem.md), the OS image) |
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
8. Spawn branch: `/system/bin/busybox` file, or `/system/bin/abi-init` (ABI bench), or the demo/services
   set selected by build flags.
9. `task::start()`, `interrupts::enable()`, `mux::run()` (kernel task).

**Image contents** (`build.rs`; every file below is on the ext2 OS volume at the root, except the kernel and `lazyos.cfg` on the FAT `/boot`)

| File | Source | Condition |
|---|---|---|
| `/system/share/samples/hello.txt`, `/system/share/samples/notes.txt` | inline strings | always |
| `/system/bin/hello` | `user` bin | always |
| `/system/bin/busybox` | `tools/abi/busybox.py` (`/system/bin/busybox`), the system shell | when built (else boot logs no shell; issue #254) |
| `/system/bin/rhai` | `tools/rhai/build.py` (`target/rhai/rhai.elf`), the `rhai` command; `sh` resolves `rhai` to it (issue #319) | when built (`$LAZYOS_RHAI` overrides); skipped by the ABI bench (`LAZYOS_INIT`) |
| `/system/bin/messengerctl`, `/system/bin/messengerd` | `messengerctl`, `messengerd` | always on disk |
| `/system/bin/init`, `/system/bin/logd`, `/system/bin/healthd`, `/system/bin/keyd`, `/system/bin/clipboardd` | services | always on disk; `init` starts them |
| `/system/bin/flaky`, `/system/bin/clipcp`, `/system/bin/clippaste` | crash/clipboard evidence programs | embedded, but `init` only starts them outside the desktop profile |
| `/system/bin/inputd`, `/system/bin/accountsd`, `/system/bin/logind`, `/system/bin/mimed`, `/system/share/mime.types`, `/system/etc/passwd`, `/system/bin/sysmond` | accounts/login/MIME/system stats | only when `LAZYOS_SERVICES=1` (or `LAZYOS_DESKTOP=1`) |
| `/system/bin/top` | system-stats text client | `LAZYOS_SERVICES=1` only; the desktop profile leaves it out |
| `/system/bin/xuid`, `/system/bin/xdemo` | compositor demo | only when `LAZYOS_XUID=1` (or `LAZYOS_DESKTOP=1`); `/system/bin/xdemo` is dropped in the desktop profile |
| `/system/bin/dragdemo` | drag & drop demo pair | `LAZYOS_XUID=1`; dropped in the desktop profile |
| `/system/bin/shellprobe` | shell-protocol evidence client | `LAZYOS_XUID=1` + `LAZYOS_SHELLPROBE=1` |
| `/system/bin/xapp` | `$LAZYOS_XUI_APP` (static musl xui app from `tools/xui/build.py`) | embedded whenever set; spawned only with `LAZYOS_XUID=1` |
| `/system/packages/os.lazy.<short>.lzp`, `/system/packages/index` | the core packages `tools/xui/build.py` writes to `target/pkg/core/` from the apps it built (`$LAZYOS_XUI_APPS`, a path list with `;` on Windows and `:` elsewhere, or the desktop default set when unset) | every desktop app except the four below is a core package (issue #509): `pkgd` installs each into `/apps` on the first boot and after an image update, and `init` launches it from there under its `app:<system_name>` label; `LAZYOS_XUI_AUTOSTART` (default `terminal`) picks the variant whose manifest sets `autostart`; the rest open from the Start menu or open-with |
| `/system/bin/terminal`, `/system/bin/devices`, `/system/bin/installer`, `/system/bin/lazyshell` | the same xui build | the four xui programs that stay unlabelled built-ins in `init`'s registry: the Terminal (a label would sandbox every command typed in it), Devices (it reads `os.kernel.dev`, which no permission names), the Installer (`pkgd`'s trusted UI) and LazyShell; the Terminal autostarts by default |
| `/system/bin/abi-init` | `$LAZYOS_INIT` | ABI bench hook |
| `/system/bin/busybox` | `$LAZYOS_BUSYBOX` | Linux shim demo |
| `/system/bin/rhai` | `$LAZYOS_RHAI` | the `rhai` command (auto-embedded from `target/rhai/rhai.elf`) |

Since F3 every program lives at its real lowercase name in `/system/bin`, data
in `/system/etc` and `/system/share`, the docs in `/docs/os`; no regular file
sits at the root. ext2 is case-sensitive, so code opens them with the exact
spelling from `libs/fhs` (`fhs::bin`, `fhs::etc`, `fhs::share`).

**Build switches** (`kernel/build.rs` -> `cfg`)

| Env | cfg | Effect |
|---|---|---|
| `LAZYOS_TESTS=1` | `lazyos_tests` | In-kernel test suite instead of demo |
| `LAZYOS_MESSENGERCTL=1` | `messengerctl_demo` | hello window runs `messengerctl` |
| `LAZYOS_CLI=1` | `cli_mode` | (without `LAZYOS_SERVICES=1`) spawns only BusyBox `sh`: one terminal window, no `hello` window |
| `LAZYOS_MESSENGERD=1` | `messengerd_service` | kernel spawns `/system/bin/messengerd` |
| `LAZYOS_SERVICES=1` | `services_mode` | kernel spawns `/system/bin/init` (`init`) |
| `LAZYOS_XUID=1` | `xuid_demo` | spawns `/system/bin/xuid` + two `/system/bin/xdemo` + `/system/bin/dragdemo` |
| `LAZYOS_SHELLPROBE=1` | `shellprobe_demo` | (with `LAZYOS_XUID=1`) spawns `/system/bin/shellprobe` |
| `LAZYOS_XUI_APP=<path>` | `xui_app` | (with `LAZYOS_XUID=1`) boots `/system/bin/xapp` as the display owner *instead of* `xuid`/`xdemo` |
| `LAZYOS_XUI_CLIENT=1` | `xui_client` | (with the two above) boots `xuid` plus `/system/bin/xapp` as a compositor client; no `xdemo` |
| `LAZYOS_XUI_APPS=<paths>` | `xui_desktop` | (with `LAZYOS_XUID=1` + `LAZYOS_XUI_CLIENT=1`) the desktop session: the kernel boots only `xuid`; `init` (`LAZYOS_SERVICES=1`) opens the embedded apps as clients, so several run side by side. `LAZYOS_XUI_AUTOSTART=term,sysmon` picks which (default `term`, `none` disables) |
| `LAZYOS_DESKTOP=1` | `services_mode`, `xuid_demo`, `xui_desktop`, `lazyos_desktop` | The desktop profile (issue #217): one switch for the whole recipe. It implies `LAZYOS_SERVICES` + `LAZYOS_XUID`; the root build script embeds the default xui app set (`target/xui/xui-{term,sysmon,fabricmon,counter}.elf`, overridable with `LAZYOS_XUI_APPS`; a missing default app fails the build), and `init` starts only the real session — no `flaky`, clipboard demo pair or `top` launch self-test (`lazyos_desktop` drops their ELFs and manifest rows too) |
| `LAZYOS_KBD_LAYOUT=fr` | (none; `option_env!` in `kernel/src/input/layout.rs` and `user/src/bin/inputd/config.rs`) | Keyboard layout: French AZERTY with AltGr layer instead of the default US QWERTY. Dead keys are not modelled (`^`, `¨` are literal). `inputd` uses it as the boot default; `confd` key `sys/input/layout` overrides it live |

**Invariants / decisions**

- The kernel renders through the bootloader framebuffer; no scanout mapping yet.
- Effort workarounds on the live path are deliberate: user programs are
  `opt-level = "s"` and stripped because a large ELF slows ATA boot under QEMU
  (`Cargo.toml` profiles).
- Normal boots compile none of the test-only hooks.

**Status.** Working: `python tools/run_demo.py` builds and boots the two-window
demo; the switches above are exercised by the headless CI workflows
(`screenshots.yml`, `xui.yml`, `kernel-tests.yml`, `abi-compat.yml`). The
default image carries the FAT `/boot` and the ext2 OS volume on virtio-blk; the
launchers also attach `target/home.img` as the `/home` volume.
