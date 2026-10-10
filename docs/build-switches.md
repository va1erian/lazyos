# Build switches

Every `LAZYOS_*` environment variable the image build reads. They are set on
`cargo build` (or on `python tools/run_demo.py`, which sets most of them from
its flags). A switch is on when it is exactly `1` unless stated otherwise;
unset means off or the default. After changing one, a plain `cargo build`
notices (`rerun-if-env-changed`), but see the notes under *Test and debug*.

How to read the **Front end** column: the `run_demo.py` flag that sets the
switch (the GUI launcher, `python tools/lazygui/...`, wires the same flags
through `tools/lazygui/catalog.py`). `-` means the switch has no flag; set it by
hand, or through `-- ` pass-through where noted.

This page is derived from `build.rs`, `kernel/build.rs`, `build_support/*.rs`
and `tools/demo_args.py`. When you add a switch, add a row here too; the source
stays authoritative. To list every switch the code reads:

```bash
grep -rhoE 'LAZYOS_[A-Z0-9_]+' build.rs build_support kernel/build.rs kernel/src | sort -u
```

## Profile: what the image contains

| Switch | Meaning | Front end |
| --- | --- | --- |
| `LAZYOS_DESKTOP` | The desktop profile: services, `xuid`, LazyShell and the default core apps. Needs `python tools/xui/build.py` first. | `--desktop` |
| `LAZYOS_SHELL` | `0` leaves LazyShell (taskbar, menu, icons) out of a desktop image. | `--no-shell` |
| `LAZYOS_SERVICES` | Boot the userspace services (`messengerd`, `init`, ...) without the desktop. Implied by `LAZYOS_DESKTOP`. | - |
| `LAZYOS_XUID` | Embed the `xuid` compositor without the rest of the desktop. Implied by `LAZYOS_DESKTOP`. | - |
| `LAZYOS_XUI_AUTOSTART` | Comma list of core-app short names opened at login (`term`, `settings`, `writer`, ...). Unset: the session opens the Terminal only where an image says so. | - |
| `LAZYOS_XUI_APPS` | Platform path list (`;` on Windows, `:` elsewhere) of xui app ELFs to embed instead of the default set. | - |
| `LAZYOS_XUI_APP` | Path to a single xui app ELF to run as the only client (single-app images). | - |
| `LAZYOS_XUI_CLIENT` | `1`: that app runs as an `xuid` client rather than owning the framebuffer. | - |
| `LAZYOS_UI_PROBE` | Apps print `UI:RECT`/`UI:WIDGET` lines so session scripts can click by name (`click_at`). | - |
| `LAZYOS_INIT` | Path to a Linux fixture embedded as `abi-init` (the ABI bench; also stops BusyBox/rhai embedding). | - |
| `LAZYOS_CLI` | Console mode: boot only BusyBox `sh` in one window, no `hello` demo. Ignored in services mode. | - |
| `LAZYOS_MESSENGERD` | The kernel starts `messengerd` (the registry daemon). | - |
| `LAZYOS_MESSENGERCTL` | Boot `messengerctl` in the hello window instead of `hello`. | - |
| `LAZYOS_SHELLPROBE` | Embed the shell probe client (with `LAZYOS_XUID`). | `-` |
| `LAZYOS_BUSYBOX` | Path to a BusyBox ELF to embed (default: the one `tools/` builds). | - |
| `LAZYOS_RHAI` | Path to the `rhai` command to embed (default: `target/rhai/rhai.elf`, embedded when present; skipped under `LAZYOS_INIT`). `run_demo.py` rebuilds it first. | `--no-rhai` skips only that rebuild; an existing `rhai.elf` is still embedded |

## Optional apps and packages

| Switch | Meaning | Requires | Front end |
| --- | --- | --- | --- |
| `LAZYOS_LAZYRAD` | LazyRAD IDE and player as the core package `os.lazy.lazyrad`. | desktop | `--lazyrad` |
| `LAZYOS_MODPLAYER` | `modplayer.lzp` in `/system/share/samples`. | `target/pkg/modplayer.lzp` (`tools/lazyrad/package.py`); the player needs LazyRAD, which `--modplayer` turns on | `--modplayer` |
| `LAZYOS_PICTURES` | Picture Viewer core package. | desktop | `--pictures` |
| `LAZYOS_DOOM` | `doom.lzp` sample package. | `tools/doom/build.py` | `--doom` |
| `LAZYOS_EMUSIC` | `emusic.lzp` sample package. | `tools/emusic/build.py` | `--emusic` |
| `LAZYOS_LAZYWEB` | LazyWeb browser core package. | desktop, `LAZYOS_NETD` | `--lazyweb` |
| `LAZYOS_MAIL` | Mail (esMail over TLS). | desktop, TLS | `--mail` |
| `LAZYOS_TRAYDEMO` | `os.lazy.traydemo` tray sample. | desktop | `--traydemo` |
| `LAZYOS_LINUXAPPS` | Real Linux programs (dash, lua, sqlite3, jq, rg) in `/system/bin`. | `tools/linuxapps/build.py` | `--linuxapps` |
| `LAZYOS_TLS` | `curl`, `wget`, `fetch` and the Mozilla CA bundle. | - | `--tls` |
| `LAZYOS_FETCH` | Path to the `nettls` binary to embed (default: `target/nettls/fetch.elf`). | `LAZYOS_TLS` | - |
| `LAZYOS_SMB` | The `smb` client and `smbfuse`. | `LAZYOS_NETD` | `--smb` |
| `LAZYOS_ASSETS` | Directory tree of your own assets with an `assets/manifest.txt`-style manifest. | - | `--assets DIR` |
| `LAZYOS_TEST_PACKAGES` | List of `.lzp` files to ship as extra packages. | - | - |

## Drivers and networking

| Switch | Meaning | Front end |
| --- | --- | --- |
| `LAZYOS_NETD` | The network stack: `netdrv`, `netd` (DHCP, sockets), `ping`, `nslookup`, `nc`. | `--net` |
| `LAZYOS_NETD_ARGS` | Word list passed to `netd`. `demo=0` runs it without the harness's evidence clients (what `--net` sets). | - |
| `LAZYOS_NET` | The older, stack-less NIC demo (`netdrv` only). Implied by `LAZYOS_NETD`. | - |
| `LAZYOS_NET_ARGS` | Build-time `netdrv` boot arguments for `LAZYOS_NET` images. | - |
| `LAZYOS_NETFIX`, `LAZYOS_NETBULK` | Paths to harness fixtures (`netfix`, `netbulk`) embedded for `tools/net/run.py`. | - |
| `LAZYOS_SOUND` | Sound driver and `audiod`. Always on in a desktop. | `--sound` (`--sound-card` only picks the QEMU card) |
| `LAZYOS_USB` | The `usbd` HID driver and USB mass storage. | - |
| `LAZYOS_DEVD` | `0` keeps the static driver rows instead of starting `devd`. | `--no-devd` |
| `LAZYOS_IRQCHIP` | `pic` keeps the 8259 for legacy lines (default `ioapic`). | `--irqchip` |
| `LAZYOS_MSI` | `0` keeps every device on INTx. | `--no-msi` |
| `LAZYOS_DBGD` | Remote inspection daemon. | `--dbgd` (needs `LAZYOS_NETD`) |
| `LAZYOS_DBGD_KEY` | 32 to 128 hex characters; unset generates a key into `target/dbgd.key`. | - |
| `LAZYOS_DBGD_PORT` | TCP port, 1..65535. | - |
| `LAZYOS_DBGD_PEER` | The one peer address (`a.b.c.d`) allowed to connect. | - |

## Accounts and login

| Switch | Meaning | Front end |
| --- | --- | --- |
| `LAZYOS_AUTOLOGIN` | Account to log in straight away; `none` always shows the login screen. | `--autologin NAME` |
| `LAZYOS_SETUP` | First-boot setup: no account, the login screen creates the owner. | `--setup` |
| `LAZYOS_OMIT_PASSWD` | Build with no account database (no login can succeed). Test only. | - |

## Disk image and filesystem

| Switch | Meaning | Front end |
| --- | --- | --- |
| `LAZYOS_OS_SIZE` | OS volume size (`512M` default, `128M` minimum, `2G` etc.). Changing it on an existing image needs a reset. | - |
| `LAZYOS_RESET_OS` | Recreate the OS volume with a new UUID instead of updating in place. CI sets it everywhere. | `--reset-os` |
| `LAZYOS_UPDATE_DAMAGED_OS` | Update a volume anyway when the checker finds damage a crash does not leave. | - |
| `LAZYOS_JOURNAL` | Internal JBD2 journal; `1` or a block count. | `--journal` |
| `LAZYOS_BLOCK_CACHE_KB` | Block-cache size in KiB; `0` mounts uncached. | - |
| `LAZYOS_RAMDISK` | Path to a FAT image loaded as the bootloader ramdisk (registered as the `ram0` fallback block device). | - |
| `LAZYOS_USB_IMAGE` | Build the image as a USB stick (`/home` on the stick). Needs `LAZYOS_USB` and either `LAZYOS_SERVICES` or `LAZYOS_DESKTOP`. | `--usb-image` |
| `LAZYOS_USB_HOME_SIZE`, `LAZYOS_USB_ROOT_FREE` | Sizes for the stick's home volume and spare root space. | - |
| `LAZYOS_DIAG_HOLD` | Seconds (1..600) `xuid` keeps the boot-log panes on screen before the desktop opens, for PCs with no serial port (`diag.hold` in `lazyos.cfg`). | - |

## Kernel limits and display

| Switch | Meaning | Front end |
| --- | --- | --- |
| `LAZYOS_LIMIT_<KEY>` | Kernel ceilings written to `lazyos.cfg`: `HEAP_MAX`, `FD_MAX`, `STACK_SIZE`, `QUOTA_USER_MEMORY`, `QUOTA_KERNEL_MEMORY`, `SHARED_BUFFER_MAX`. Sizes take `K/M/G/T`; `FD_MAX` is a count. See [architecture/limits.md](architecture/limits.md). | `--limit key=value` |
| `LAZYOS_DISPLAY_MODE` | `<w>x<h>` the kernel switches the std VGA to after boot (e.g. `2560x1440`). | `--hidpi`, `--display-mode WxH` |
| `LAZYOS_DISPLAY_SCALE` | `auto`, `1` or `2`: the default for `sys/ui/scale`. | - |
| `LAZYOS_KBD_LAYOUT` | `us` (default) or `fr` (AZERTY). | - |

## Test and debug

| Switch | Meaning | Front end |
| --- | --- | --- |
| `LAZYOS_TESTS` | Boot the in-kernel test suite instead of the system (`tools/test/run.py` sets it). | - |
| `LAZYOS_TEST_FILTER` | Run only tests whose name contains the text. Touch `kernel/src/main.rs` after changing it; the build does not notice an environment variable alone. | - |
| `LAZYOS_DEV_FUZZ_SEED` | Seed for the dev-suite fuzz test. | - |
| `LAZYOS_BUSYBOX_TEST` | Boot BusyBox with `sh -c "echo ABI:busybox:PASS"` instead of an interactive shell (the ABI bench). | - |
| `LAZYOS_PERF` | Latency hooks and `PERF:` lines (`tools/perf/run.py`). | - |
| `LAZYOS_LABEL_TRACE` | One `LABEL:DENY` serial line per refused label-policy call. | - |
| `LAZYOS_FORCE_PANIC` | Panic after boot to prove the on-screen panic report. | - |
| `LAZYOS_TIMER` | `lapic` forces the local APIC timer where the PIT ticks. | `--timer` |
| `LAZYOS_TIMER_REF` | `hpet` or `none` narrows the reference clocks for calibration tests. | - |
| `LAZYOS_EVENT_TIMER` | `0` leaves out the one-shot deadline timer (100 Hz ticks only). | - |
| `LAZYOS_X2APIC` | Switch the APIC to x2APIC mode itself. | - |
| `LAZYOS_TLS_TEST_CA` | Extra CA appended to the bundle. Harness only, never a normal image. | - |
| `LAZYOS_TLS_TEST_HOSTS` | Host-name mappings appended to `/etc/hosts`. Harness only. | - |

## Dependencies the build enforces

The build refuses combinations that cannot work and says why:

- `LAZYOS_DBGD=1` and `LAZYOS_SMB=1` need `LAZYOS_NETD=1`.
- `LAZYOS_LAZYWEB=1` needs `LAZYOS_DESKTOP=1` and `LAZYOS_NETD=1`.
- `LAZYOS_PICTURES=1` needs `LAZYOS_DESKTOP=1`.
- `LAZYOS_MODPLAYER=1` needs `target/pkg/modplayer.lzp` (run
  `tools/lazyrad/package.py`). The build does not require `LAZYOS_LAZYRAD=1`;
  `run_demo.py --modplayer` sets both so the package has a player to run on.
- `LAZYOS_DOOM=1`, `LAZYOS_EMUSIC=1`: their `.lzp` must exist (run the
  tool's `build.py` first).
- `LAZYOS_DESKTOP=1` needs the default xui apps built (`python tools/xui/build.py`); optional ones such as `xui-docs.elf` are skipped with a warning when missing.
- A desktop with `LAZYOS_NETD=1` also needs the print spooler built.

See also: [AGENTS.md](../AGENTS.md) for how each feature is run and verified,
and [tools/lazygui/catalog.py](../tools/lazygui/catalog.py) for the GUI's
`build_env` mapping.
