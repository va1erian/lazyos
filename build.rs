//! Host-side build script: combine the compiled kernel with the `bootloader`
//! crate into a bootable BIOS disk image, and expose the image path to the
//! runner (`src/main.rs`).

use std::path::PathBuf;

#[path = "build_support/docs_embed.rs"]
mod docs_embed;
#[path = "build_support/drivers.rs"]
mod drivers;
#[path = "build_support/elf_trim.rs"]
mod elf_trim;
#[path = "build_support/lazyrad_embed.rs"]
mod lazyrad_embed;
#[path = "build_support/rhai_embed.rs"]
mod rhai_embed;
#[path = "build_support/xui_embed.rs"]
mod xui_embed;

fn main() {
    let out_dir = PathBuf::from(std::env::var_os("OUT_DIR").expect("OUT_DIR"));
    let manifest_dir = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").expect("manifest dir"));
    let kernel_full =
        PathBuf::from(std::env::var_os("CARGO_BIN_FILE_KERNEL_kernel").expect("kernel artifact"));
    // The bootloader reads the whole kernel file through BIOS calls, so the
    // image carries only the loadable part; the full ELF stays in `target/`.
    let kernel = out_dir.join("kernel.trimmed");
    let full = std::fs::read(&kernel_full).expect("read kernel artifact");
    let trimmed = elf_trim::trim_to_loadable(&full).unwrap_or(full);
    std::fs::write(&kernel, trimmed).expect("write trimmed kernel");
    println!("cargo:rerun-if-changed=build_support/elf_trim.rs");
    println!("cargo:rerun-if-changed=build_support/drivers.rs");

    let bios_image = out_dir.join("bios.img");
    let mut builder = bootloader::DiskImageBuilder::new(kernel);
    // Issue #5: `LAZYOS_RAMDISK=<path>` also loads a FAT image as the
    // bootloader ramdisk, which the kernel registers as the `ram0` fallback
    // block device.
    println!("cargo:rerun-if-env-changed=LAZYOS_RAMDISK");
    if let Some(ramdisk) = std::env::var_os("LAZYOS_RAMDISK") {
        builder.set_ramdisk(PathBuf::from(ramdisk));
    }
    builder.set_file_contents(
        String::from("HELLO.TXT"),
        b"Hello from LazyOS!\n\nThis file lives on the FAT16 disk image.\nYou are reading it through the ATA PIO driver and the FAT16 reader.\n".to_vec(),
    );
    builder.set_file_contents(
        String::from("NOTES.TXT"),
        b"LazyOS notes\n-----------\n- single-tasking x86_64 kernel\n- tiny-skia graphics\n- PS/2 keyboard + mouse\n- FAT16 read-only filesystem\n".to_vec(),
    );
    // The Docs app's test document (`xui-app/docs/testdata/`): opened by the
    // Docs screenshot session through the Open dialog, and by hand as
    // `/TESTDOC.MD` in the Docs app or the Editor.
    println!("cargo:rerun-if-changed=xui-app/docs/testdata/testdoc.md");
    builder.set_file_contents(
        String::from("TESTDOC.MD"),
        include_bytes!("xui-app/docs/testdata/testdoc.md").to_vec(),
    );
    // The ring-3 demo program, loaded and run by `run HELLO.ELF`. The system
    // shell is BusyBox `sh` (issue #254), embedded separately below.
    let hello =
        std::env::var_os("CARGO_BIN_FILE_USER_hello").expect("user hello artifact not found");
    builder.set_file(String::from("HELLO.ELF"), PathBuf::from(hello));
    // Deliberate ring-3 faults (issue #7): `exec FAULTPRB.ELF null|kernel|priv|div|ud`.
    let faultprobe = std::env::var_os("CARGO_BIN_FILE_USER_faultprobe")
        .expect("user faultprobe artifact not found");
    builder.set_file(String::from("FAULTPRB.ELF"), PathBuf::from(faultprobe));
    // The fabric observability tool (issue #70); boot it with
    // `LAZYOS_MESSENGERCTL=1`. The on-disk name is 8.3 because the kernel's
    // FAT reader only resolves short names (`MESSENGERCTL.ELF` would be stored
    // as a long-name alias the kernel cannot see).
    let messengerctl = std::env::var_os("CARGO_BIN_FILE_USER_messengerctl")
        .expect("user messengerctl artifact not found");
    builder.set_file(String::from("MSGCTL.ELF"), PathBuf::from(messengerctl));
    // The registry daemon (issue #89), started by `LAZYOS_MESSENGERD=1`. The
    // on-disk name is 8.3-safe: the base is at most eight characters, because
    // the kernel's FAT reader only resolves short names (`MESSENGERD.ELF` is
    // ten and would only exist as a long-name alias the kernel cannot see).
    let messengerd = std::env::var_os("CARGO_BIN_FILE_USER_messengerd")
        .expect("user messengerd artifact not found");
    builder.set_file(String::from("MSGRD.ELF"), PathBuf::from(messengerd));

    // Desktop profile (issue #217): one `LAZYOS_DESKTOP=1` switch that expands
    // to the desktop recipe — a services session (`LAZYOS_SERVICES`), the
    // compositor (`LAZYOS_XUID`) and the embedded xui apps (the default set
    // below, unless `LAZYOS_XUI_APPS` overrides it). It also keeps the
    // demo/evidence-only ELFs (the crash test, the clipboard demo pair, the
    // `top` text client, the `xdemo`/`dragdemo` demo clients) out of the image;
    // the kernel and the `user` crate re-read the same switch.
    println!("cargo:rerun-if-env-changed=LAZYOS_DESKTOP");
    println!("cargo:rerun-if-env-changed=LAZYOS_SERVICES");
    println!("cargo:rerun-if-env-changed=LAZYOS_XUID");
    let desktop = std::env::var_os("LAZYOS_DESKTOP").as_deref() == Some(std::ffi::OsStr::new("1"));
    let services = desktop
        || std::env::var_os("LAZYOS_SERVICES").as_deref() == Some(std::ffi::OsStr::new("1"));
    let xuid =
        desktop || std::env::var_os("LAZYOS_XUID").as_deref() == Some(std::ffi::OsStr::new("1"));

    // System services (issue #93). `init` is the supervisor the kernel boots
    // with `LAZYOS_SERVICES=1`; it starts the rest from its manifest. The
    // on-disk name is `SUPER.ELF`, not `INIT.ELF`: the ABI bench hook below
    // reserves `INIT.ELF` for a Linux fixture, and 8.3 is required by the
    // kernel's short-name FAT reader.
    let init = std::env::var_os("CARGO_BIN_FILE_USER_init").expect("user init artifact not found");
    builder.set_file(String::from("SUPER.ELF"), PathBuf::from(init));
    let logd = std::env::var_os("CARGO_BIN_FILE_USER_logd").expect("user logd artifact not found");
    builder.set_file(String::from("LOGD.ELF"), PathBuf::from(logd));
    let healthd =
        std::env::var_os("CARGO_BIN_FILE_USER_healthd").expect("user healthd artifact not found");
    builder.set_file(String::from("HEALTHD.ELF"), PathBuf::from(healthd));
    // The secrets and crypto service (issue #102). `init` starts it from its
    // manifest when the image boots with `LAZYOS_SERVICES=1`; the 8.3 name
    // `KEYD.ELF` is what the kernel's short-name FAT reader resolves.
    let keyd = std::env::var_os("CARGO_BIN_FILE_USER_keyd").expect("user keyd artifact not found");
    builder.set_file(String::from("KEYD.ELF"), PathBuf::from(keyd));

    // The per-session clipboard service (issue #115). Like `keyd`, `init`
    // starts it from its manifest when the image boots with
    // `LAZYOS_SERVICES=1`; the 8.3 name is what the kernel's FAT reader sees.
    let clipboardd = std::env::var_os("CARGO_BIN_FILE_USER_clipboardd")
        .expect("user clipboardd artifact not found");
    builder.set_file(String::from("CLIPD.ELF"), PathBuf::from(clipboardd));

    // The evidence-only programs. `init` never starts them in the desktop
    // profile, so the image leaves their ELFs out entirely: the deliberate
    // crash service (issue #93), whose restart-with-backoff demo is the
    // `FLAKY.ELF` row, and the clipboard demo pair (issue #115), which
    // `clipboardd` spawns under `demo=1`.
    if !desktop {
        let flaky =
            std::env::var_os("CARGO_BIN_FILE_USER_flaky").expect("user flaky artifact not found");
        builder.set_file(String::from("FLAKY.ELF"), PathBuf::from(flaky));
        let clipcopy = std::env::var_os("CARGO_BIN_FILE_USER_clipcopy")
            .expect("user clipcopy artifact not found");
        builder.set_file(String::from("CLIPCP.ELF"), PathBuf::from(clipcopy));
        let clippaste = std::env::var_os("CARGO_BIN_FILE_USER_clippaste")
            .expect("user clippaste artifact not found");
        builder.set_file(String::from("CLIPPS.ELF"), PathBuf::from(clippaste));
    }

    // Accounts and console login (issue #101). `init` starts `accountsd` and
    // `logind` from its manifest; `accountsd` reads `PASSWD` when present. All
    // three are added only to the services image (`LAZYOS_SERVICES=1`): the
    // plain demo never starts them, and keeping them out of the ABI bench
    // image preserves its baseline size and boot time.
    if services {
        let accountsd = std::env::var_os("CARGO_BIN_FILE_USER_accountsd")
            .expect("user accountsd artifact not found");
        builder.set_file(String::from("ACCTD.ELF"), PathBuf::from(accountsd));
        let logind =
            std::env::var_os("CARGO_BIN_FILE_USER_logind").expect("user logind artifact not found");
        builder.set_file(String::from("LOGIND.ELF"), PathBuf::from(logind));

        // The configuration registry (issue #260). `init` starts `confd`
        // (`CONFD.ELF`) from its manifest; `confctl` is its native command line.
        // Both are gated behind `LAZYOS_SERVICES=1`, like the other services,
        // so the plain demo image is unchanged.
        let confd =
            std::env::var_os("CARGO_BIN_FILE_USER_confd").expect("user confd artifact not found");
        builder.set_file(String::from("CONFD.ELF"), PathBuf::from(confd));
        let confctl = std::env::var_os("CARGO_BIN_FILE_USER_confctl")
            .expect("user confctl artifact not found");
        builder.set_file(String::from("CONFCTL.ELF"), PathBuf::from(confctl));

        // The input policy service (docs/input-plan.md). `init` starts
        // `inputd` (`INPUTD.ELF`) after `confd`; it is the only task that
        // holds the kernel's `input.raw` capability.
        let inputd =
            std::env::var_os("CARGO_BIN_FILE_USER_inputd").expect("user inputd artifact not found");
        builder.set_file(String::from("INPUTD.ELF"), PathBuf::from(inputd));

        // The time-of-day service (issue #369). `init` starts `timed`
        // (`TIMED.ELF`) after `messengerd` and `confd`.
        let timed =
            std::env::var_os("CARGO_BIN_FILE_USER_timed").expect("user timed artifact not found");
        builder.set_file(String::from("TIMED.ELF"), PathBuf::from(timed));
        let timectl = std::env::var_os("CARGO_BIN_FILE_USER_timectl")
            .expect("user timectl artifact not found");
        builder.set_file(String::from("TIMECTL.ELF"), PathBuf::from(timectl));

        // The MIME database and open-with registry (issue #116). `init`
        // starts it from its manifest; `MIMED.ELF` is the 8.3-safe on-disk
        // name. `MIME.TYP` is the `/etc/mime.types`-style override the
        // service reads at boot; it must be 8.3 because the kernel's FAT
        // reader only resolves short names (an ext2 boot volume can carry
        // `/etc/mime.types` instead, which `mimed` tries first).
        let mimed =
            std::env::var_os("CARGO_BIN_FILE_USER_mimed").expect("user mimed artifact not found");
        builder.set_file(String::from("MIMED.ELF"), PathBuf::from(mimed));

        // The application package manager (docs/packages.md phase 3). `init`
        // starts `PKGD.ELF` from its manifest (after `confd` and `mimed`);
        // `PKGCTL.ELF` is its command line, run from the Terminal. Both names
        // are 8.3-safe for the kernel's short-name FAT reader.
        let pkgd =
            std::env::var_os("CARGO_BIN_FILE_USER_pkgd").expect("user pkgd artifact not found");
        builder.set_file(String::from("PKGD.ELF"), PathBuf::from(pkgd));
        let pkgctl =
            std::env::var_os("CARGO_BIN_FILE_USER_pkgctl").expect("user pkgctl artifact not found");
        builder.set_file(String::from("PKGCTL.ELF"), PathBuf::from(pkgctl));
        embed_sample_packages(&mut builder);
        builder.set_file_contents(
            String::from("MIME.TYP"),
            b"# LazyOS MIME overrides, /etc/mime.types style: <mime> <ext>...\n\
              # The boot image uses an 8.3 name because the FAT reader cannot\n\
              # resolve long names; an ext2 boot volume uses /etc/mime.types.\n\
              text/x-lazy-test lzt\n\
              application/x-lazyos lazy\n"
                .to_vec(),
        );

        // The passwd-style account database (issue #101), `name:uid:gid:
        // secret:home:shell`. This branch has no writable store, so accountsd
        // reads this read-only fallback; the secret is plaintext *on purpose*
        // for bring-up and is replaced by keyd + Argon2id
        // (`docs/security-model.md` section 3). The shell is BusyBox `sh` (the
        // `sh` applet alias the kernel's Linux loader resolves to `BUSYBOX`,
        // issue #254); `logind` prefixes it with `linux:` when it spawns the
        // login shell. `root` keeps the system identity for admin operations,
        // `alice` is the unprivileged demo login a headless session uses.
        builder.set_file_contents(
            String::from("PASSWD"),
            b"root:0:0:toor:/root:sh\nalice:1000:1000:lazy:/home/alice:sh\n".to_vec(),
        );

        // The system monitor (issue #144). `init` starts `sysmond`
        // (`SYSD.ELF`) from its manifest; the service wraps the native
        // system-stats syscall (14) and republishes retained `system/stats/*`
        // topics. `TOP.ELF` is its one-shot native text client, spawned by
        // `sysmond` (`demo=1`) so a headless services boot records `SYS:TOP:PASS`.
        // Both names are 8.3-safe for the kernel's short-name FAT reader.
        let sysmond = std::env::var_os("CARGO_BIN_FILE_USER_sysmond")
            .expect("user sysmond artifact not found");
        builder.set_file(String::from("SYSD.ELF"), PathBuf::from(sysmond));
        // The `top` text client is the launch self-test's target, an
        // evidence-only program the desktop profile never starts, so its ELF
        // stays out of the desktop image.
        if !desktop {
            let top =
                std::env::var_os("CARGO_BIN_FILE_USER_top").expect("user top artifact not found");
            builder.set_file(String::from("TOP.ELF"), PathBuf::from(top));
        }
    }

    // The display protocol demo (issue #113): `LAZYOS_XUID=1` embeds the
    // userspace compositor and its demo app. Both are gated out of the default
    // demo image so its size and boot stay identical.
    if xuid {
        let xuid =
            std::env::var_os("CARGO_BIN_FILE_USER_xuid").expect("user xuid artifact not found");
        builder.set_file(String::from("XUID.ELF"), PathBuf::from(xuid));
        // `xdemo` and the drag & drop pair are demo clients; the desktop
        // profile runs its own xui apps as clients instead.
        if !desktop {
            let xdemo = std::env::var_os("CARGO_BIN_FILE_USER_xdemo")
                .expect("user xdemo artifact not found");
            builder.set_file(String::from("XDEMO.ELF"), PathBuf::from(xdemo));
            // The drag & drop demo pair (issue #145); the kernel starts its
            // launcher, and 8.3 requires the `DRAGDMO.ELF` on-disk name.
            let dragdemo = std::env::var_os("CARGO_BIN_FILE_USER_dragdemo")
                .expect("user dragdemo artifact not found");
            builder.set_file(String::from("DRAGDMO.ELF"), PathBuf::from(dragdemo));
        }
    }

    // The virtio-sound and virtio-net userspace drivers (`LAZYOS_SOUND=1`,
    // `LAZYOS_NET=1`; the desktop profile always ships the sound stack).
    drivers::embed(&mut builder, desktop);

    // The shell-protocol evidence client (issue #167): `LAZYOS_XUID=1` plus
    // the `LAZYOS_SHELLPROBE=1` demo hook embeds and boots it, so the default
    // compositor sessions (WM, drag & drop) keep their window layout.
    println!("cargo:rerun-if-env-changed=LAZYOS_SHELLPROBE");
    if std::env::var_os("LAZYOS_XUID").as_deref() == Some(std::ffi::OsStr::new("1"))
        && std::env::var_os("LAZYOS_SHELLPROBE").as_deref() == Some(std::ffi::OsStr::new("1"))
    {
        let probe = std::env::var_os("CARGO_BIN_FILE_USER_shellprobe")
            .expect("user shellprobe artifact not found");
        builder.set_file(String::from("SHELLPRB.ELF"), PathBuf::from(probe));
    }

    // The xui app (issue #114): `LAZYOS_XUI_APP=<path>` embeds a static-musl
    // binary built by `tools/xui/build.py` as `XAPP.ELF`. With `LAZYOS_XUID=1`
    // the kernel boots it instead of the `xuid` + `xdemo` session, because the
    // app binds the display grant itself (it is the session's compositor).
    // Without `LAZYOS_XUID=1` the file is only embedded, never spawned.
    // Issue #168 adds `LAZYOS_XUI_CLIENT=1`: the app then runs *as a client*
    // of `xuid`, which the kernel spawns alongside it.
    println!("cargo:rerun-if-env-changed=LAZYOS_XUI_CLIENT");
    println!("cargo:rerun-if-env-changed=LAZYOS_XUI_APP");
    if let Some(app) = std::env::var_os("LAZYOS_XUI_APP") {
        let app = PathBuf::from(app);
        if app.is_file() {
            println!("cargo:warning=LAZYOS_XUI_APP embedded: {}", app.display());
            println!("cargo:rerun-if-changed={}", app.display());
            builder.set_file(String::from("XAPP.ELF"), app);
        } else {
            println!("cargo:warning=LAZYOS_XUI_APP not found: {}", app.display());
        }
    }

    // The desktop shell (issue #157), on by default with the desktop profile.
    println!("cargo:rerun-if-changed=build_support/xui_embed.rs");
    let shell = xui_embed::shell_enabled(desktop, services, xuid);
    xui_embed::embed_xui_apps(&mut builder, desktop, shell);

    // Rebuild the image when the kernel test switch flips (issue #62): the
    // kernel's own build script turns `LAZYOS_TESTS=1` into `cfg(lazyos_tests)`.
    println!("cargo:rerun-if-env-changed=LAZYOS_TESTS");
    // Fabric observability demo switch (issue #70): the kernel boots the
    // `messengerctl` tool (`MSGCTL.ELF`) in the hello window when this is set.
    println!("cargo:rerun-if-env-changed=LAZYOS_MESSENGERCTL");
    // CLI mode switch: the kernel boots only `sh` (no `hello` window).
    println!("cargo:rerun-if-env-changed=LAZYOS_CLI");
    // Registry daemon switch (issue #89): the kernel starts `messengerd`
    // (`MESSENGERD.ELF`) when this is set.
    println!("cargo:rerun-if-env-changed=LAZYOS_MESSENGERD");

    // ABI conformance bench hook: embed a Linux fixture as `INIT.ELF`.
    println!("cargo:rerun-if-env-changed=LAZYOS_INIT");
    if let Some(init) = std::env::var_os("LAZYOS_INIT") {
        let init = PathBuf::from(init);
        if init.is_file() {
            println!("cargo:warning=LAZYOS_INIT embedded: {}", init.display());
            builder.set_file(String::from("INIT.ELF"), init);
        } else {
            println!("cargo:warning=LAZYOS_INIT not found: {}", init.display());
        }
    }

    // BusyBox is the system shell (issue #254). It is a fetched/built artifact
    // (see `tools/abi/busybox.py`), so embed it automatically whenever it is
    // available — `LAZYOS_BUSYBOX` overrides the search. The ABI bench embeds a
    // fixture as `INIT.ELF` instead (`LAZYOS_INIT`); skipping BusyBox then
    // keeps those images small and lets the fixture own the boot. Without a
    // BusyBox the image still boots, just without a console shell,
    // and this warns so the reason is visible in the build log.
    println!("cargo:rerun-if-env-changed=LAZYOS_BUSYBOX");
    println!("cargo:rerun-if-env-changed=LAZYOS_BUSYBOX_TEST");
    let explicit = std::env::var_os("LAZYOS_BUSYBOX")
        .map(PathBuf::from)
        .filter(|path| path.is_file());
    let busybox = explicit.or_else(|| {
        if std::env::var_os("LAZYOS_INIT").is_some() {
            None
        } else {
            // Watch every candidate, not just the one picked: a BusyBox built
            // or dropped in later (or one that outranks the cached pick) must
            // trigger a rebuild of the image.
            for path in busybox_candidates(&manifest_dir) {
                println!("cargo:rerun-if-changed={}", path.display());
            }
            find_busybox(&manifest_dir)
        }
    });
    match busybox {
        Some(path) => {
            println!("cargo:warning=LAZYOS_BUSYBOX embedded: {}", path.display());
            println!("cargo:rerun-if-changed={}", path.display());
            builder.set_file(String::from("BUSYBOX"), path);
        }
        None => println!(
            "cargo:warning=LAZYOS_BUSYBOX unavailable; the image will have no console shell \
             (run tools/abi/busybox.py for build/supply instructions)"
        ),
    }
    // The `rhai` scripting command (issue #319), resolved from `sh` as RHAI.ELF.
    println!("cargo:rerun-if-changed=build_support/rhai_embed.rs");
    rhai_embed::embed(&mut builder, &manifest_dir);
    // The documentation tree (`docs/**/*.md` plus `README.md`) at `/docs/...`,
    // read by the Docs app and the Editor.
    println!("cargo:rerun-if-changed=build_support/docs_embed.rs");
    docs_embed::embed(&mut builder, &manifest_dir);
    // The `lazyrad` runtime (`LAZYOS_LAZYRAD=1`), embedded as LRPLAY.ELF and
    // LAZYRAD.ELF, plus the sample projects in `LAZYRAD_SAMPLES`.
    println!("cargo:rerun-if-changed=build_support/lazyrad_embed.rs");
    lazyrad_embed::embed(&mut builder, &manifest_dir);
    builder
        .create_bios_image(&bios_image)
        .expect("failed to create BIOS disk image");

    // Also expose a stable path for tooling (CI screenshot job, scripts).
    let stable_image = manifest_dir.join("target").join("lazyos.img");
    if let Some(parent) = stable_image.parent() {
        std::fs::create_dir_all(parent).expect("create target dir");
    }
    std::fs::copy(&bios_image, &stable_image).expect("copy disk image to target/lazyos.img");

    println!("cargo:rustc-env=BIOS_IMAGE={}", bios_image.display());
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=kernel/src");
    println!("cargo:rerun-if-changed=assets/fonts/JetBrainsMono-Regular.ttf");
}

/// Locate a BusyBox built by `tools/abi/busybox.py`, whether it was dropped by
/// hand (`tools/abi/busybox`) or built into the ABI cache
/// (`target/abi/busybox/busybox`). Returns `None` when the host could not build
/// one, which makes the image boot without a console shell.
fn find_busybox(manifest_dir: &std::path::Path) -> Option<PathBuf> {
    busybox_candidates(manifest_dir)
        .into_iter()
        .find(|path| path.is_file())
}

/// Where a BusyBox may appear, highest priority first.
fn busybox_candidates(manifest_dir: &std::path::Path) -> Vec<PathBuf> {
    [
        "tools/abi/busybox",
        "target/abi/busybox/busybox",
        "target/abi/fixtures/busybox.elf",
    ]
    .iter()
    .map(|relative| manifest_dir.join(relative))
    .collect()
}

/// Embed the sample `.lzp` packages as 8.3 names in the FAT root, when
/// `tools/pkg/build_samples.py` produced them (`tools/xui/build.py` runs it
/// after building the xui apps): `PKGDEMO.LZP` is the Counter demo as an
/// installable package, installed with `pkgctl install /PKGDEMO.LZP`. A missing
/// sample only means a smaller image, so it warns instead of failing.
fn embed_sample_packages(builder: &mut bootloader::DiskImageBuilder) {
    let dir = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").expect("manifest dir"))
        .join("target")
        .join("pkg");
    for disk_name in ["PKGDEMO.LZP"] {
        let path = dir.join(disk_name);
        // Tracked even when missing, so building it later is picked up.
        println!("cargo:rerun-if-changed={}", path.display());
        if path.is_file() {
            println!(
                "cargo:warning=sample package embedded: {} as {disk_name}",
                path.display()
            );
            builder.set_file(String::from(disk_name), path);
        } else {
            println!(
                "cargo:warning=sample package {disk_name} not built \
                 (`python tools/xui/build.py` or `python tools/pkg/build_samples.py`)"
            );
        }
    }
}
