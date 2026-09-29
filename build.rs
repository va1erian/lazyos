//! Host-side build script: combine the compiled kernel with the `bootloader`
//! crate into a bootable BIOS disk image, and expose the image path to the
//! runner (`src/main.rs`).

use std::path::PathBuf;

#[path = "build_support/elf_trim.rs"]
mod elf_trim;

fn main() {
    let out_dir = PathBuf::from(std::env::var_os("OUT_DIR").expect("OUT_DIR"));
    let kernel_full =
        PathBuf::from(std::env::var_os("CARGO_BIN_FILE_KERNEL_kernel").expect("kernel artifact"));
    // The bootloader reads the whole kernel file through BIOS calls, so the
    // image carries only the loadable part; the full ELF stays in `target/`.
    let kernel = out_dir.join("kernel.trimmed");
    let full = std::fs::read(&kernel_full).expect("read kernel artifact");
    let trimmed = elf_trim::trim_to_loadable(&full).unwrap_or(full);
    std::fs::write(&kernel, trimmed).expect("write trimmed kernel");
    println!("cargo:rerun-if-changed=build_support/elf_trim.rs");

    let bios_image = out_dir.join("bios.img");
    let mut builder = bootloader::DiskImageBuilder::new(kernel);
    builder.set_file_contents(
        String::from("HELLO.TXT"),
        b"Hello from LazyOS!\n\nThis file lives on the FAT16 disk image.\nYou are reading it through the ATA PIO driver and the FAT16 reader.\n".to_vec(),
    );
    builder.set_file_contents(
        String::from("NOTES.TXT"),
        b"LazyOS notes\n-----------\n- single-tasking x86_64 kernel\n- tiny-skia graphics\n- PS/2 keyboard + mouse\n- FAT16 read-only filesystem\n".to_vec(),
    );
    // The ring-3 demo programs, loaded and run by `run HELLO.ELF` / `run SH.ELF`.
    let hello =
        std::env::var_os("CARGO_BIN_FILE_USER_hello").expect("user hello artifact not found");
    builder.set_file(String::from("HELLO.ELF"), PathBuf::from(hello));
    let sh = std::env::var_os("CARGO_BIN_FILE_USER_sh").expect("user sh artifact not found");
    builder.set_file(String::from("SH.ELF"), PathBuf::from(sh));
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
    // Deliberately-crashing service used to demonstrate supervision and
    // restart-with-backoff in a boot log (issue #93).
    let flaky =
        std::env::var_os("CARGO_BIN_FILE_USER_flaky").expect("user flaky artifact not found");
    builder.set_file(String::from("FLAKY.ELF"), PathBuf::from(flaky));
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
    // The clipboard demo pair (issue #115): a lazy owner and a paster.
    // `clipboardd` spawns both at services boot (`demo=1`), so a headless
    // `LAZYOS_SERVICES=1` run records the `CLIP:COPY`/`CLIP:PASTE`/
    // `CLIP:DENIED` evidence markers.
    let clipcopy =
        std::env::var_os("CARGO_BIN_FILE_USER_clipcopy").expect("user clipcopy artifact not found");
    builder.set_file(String::from("CLIPCP.ELF"), PathBuf::from(clipcopy));
    let clippaste = std::env::var_os("CARGO_BIN_FILE_USER_clippaste")
        .expect("user clippaste artifact not found");
    builder.set_file(String::from("CLIPPS.ELF"), PathBuf::from(clippaste));

    // Accounts and console login (issue #101). `init` starts `accountsd` and
    // `logind` from its manifest; `accountsd` reads `PASSWD` when present. All
    // three are added only to the services image (`LAZYOS_SERVICES=1`): the
    // plain demo never starts them, and keeping them out of the ABI bench
    // image preserves its baseline size and boot time.
    println!("cargo:rerun-if-env-changed=LAZYOS_SERVICES");
    if std::env::var_os("LAZYOS_SERVICES").as_deref() == Some(std::ffi::OsStr::new("1")) {
        let accountsd = std::env::var_os("CARGO_BIN_FILE_USER_accountsd")
            .expect("user accountsd artifact not found");
        builder.set_file(String::from("ACCTD.ELF"), PathBuf::from(accountsd));
        let logind =
            std::env::var_os("CARGO_BIN_FILE_USER_logind").expect("user logind artifact not found");
        builder.set_file(String::from("LOGIND.ELF"), PathBuf::from(logind));

        // The MIME database and open-with registry (issue #116). `init`
        // starts it from its manifest; `MIMED.ELF` is the 8.3-safe on-disk
        // name. `MIME.TYP` is the `/etc/mime.types`-style override the
        // service reads at boot; it must be 8.3 because the kernel's FAT
        // reader only resolves short names (an ext2 boot volume can carry
        // `/etc/mime.types` instead, which `mimed` tries first).
        let mimed =
            std::env::var_os("CARGO_BIN_FILE_USER_mimed").expect("user mimed artifact not found");
        builder.set_file(String::from("MIMED.ELF"), PathBuf::from(mimed));
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
        // (`docs/security-model.md` section 3). `SH.ELF` is the native shell;
        // `root` keeps the system identity for admin operations, `alice` is
        // the unprivileged demo login a headless session uses.
        builder.set_file_contents(
            String::from("PASSWD"),
            b"root:0:0:toor:/root:/SH.ELF\nalice:1000:1000:lazy:/home/alice:/SH.ELF\n".to_vec(),
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
        let top = std::env::var_os("CARGO_BIN_FILE_USER_top").expect("user top artifact not found");
        builder.set_file(String::from("TOP.ELF"), PathBuf::from(top));
    }

    // The display protocol demo (issue #113): `LAZYOS_XUID=1` embeds the
    // userspace compositor and its demo app. Both are gated out of the default
    // demo image so its size and boot stay identical.
    println!("cargo:rerun-if-env-changed=LAZYOS_XUID");
    if std::env::var_os("LAZYOS_XUID").as_deref() == Some(std::ffi::OsStr::new("1")) {
        let xuid =
            std::env::var_os("CARGO_BIN_FILE_USER_xuid").expect("user xuid artifact not found");
        builder.set_file(String::from("XUID.ELF"), PathBuf::from(xuid));
        let xdemo =
            std::env::var_os("CARGO_BIN_FILE_USER_xdemo").expect("user xdemo artifact not found");
        builder.set_file(String::from("XDEMO.ELF"), PathBuf::from(xdemo));
        // The drag & drop demo pair (issue #145); the kernel starts its
        // launcher, and 8.3 requires the `DRAGDMO.ELF` on-disk name.
        let dragdemo = std::env::var_os("CARGO_BIN_FILE_USER_dragdemo")
            .expect("user dragdemo artifact not found");
        builder.set_file(String::from("DRAGDMO.ELF"), PathBuf::from(dragdemo));
    }

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

    embed_xui_apps(&mut builder);

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

    // BusyBox hook: embed a static `busybox` as `BUSYBOX` (run as `sh`, and
    // reachable by `execve("/busybox")` for its applets).
    println!("cargo:rerun-if-env-changed=LAZYOS_BUSYBOX");
    if let Some(busybox) = std::env::var_os("LAZYOS_BUSYBOX") {
        let busybox = PathBuf::from(busybox);
        if busybox.is_file() {
            println!(
                "cargo:warning=LAZYOS_BUSYBOX embedded: {}",
                busybox.display()
            );
            builder.set_file(String::from("BUSYBOX"), busybox);
        } else {
            println!(
                "cargo:warning=LAZYOS_BUSYBOX not found: {}",
                busybox.display()
            );
        }
    }
    builder
        .create_bios_image(&bios_image)
        .expect("failed to create BIOS disk image");

    // Also expose a stable path for tooling (CI screenshot job, scripts).
    let manifest_dir = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").expect("manifest dir"));
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

/// The 8.3 on-disk name for an xui app binary (`xui-sysmon.elf` ->
/// `XSYSMON.ELF`): the kernel's FAT reader only resolves short names, and
/// `init`'s app registry (`user/src/bin/init/apps.rs`) refers to these names.
/// Returns `(stem, disk_name)`.
fn xui_disk_name(path: &std::path::Path) -> (String, String) {
    let stem = path
        .file_stem()
        .and_then(|stem| stem.to_str())
        .unwrap_or("app")
        .to_ascii_lowercase();
    let stem = stem.strip_prefix("xui-").unwrap_or(&stem).to_string();
    let base = match stem.as_str() {
        "sysmon" => "XSYSMON".to_string(),
        "fabricmon" => "XFABMON".to_string(),
        "counter" => "XCOUNTR".to_string(),
        "term" => "XTERM".to_string(),
        "client" => "XCLIENT".to_string(),
        other => {
            let short: String = other
                .chars()
                .filter(char::is_ascii_alphanumeric)
                .take(7)
                .collect();
            format!("X{}", short.to_ascii_uppercase())
        }
    };
    (stem, format!("{base}.ELF"))
}

/// Embed the desktop's xui apps (issues #215/#216).
///
/// `LAZYOS_XUI_APPS` is a platform path list (`;` on Windows, `:` elsewhere)
/// of binaries built by `tools/xui/build.py`. Each is stored under its 8.3
/// name, and `XAPPS.LST` lists the shipped ones so `init` marks every other
/// registry row unavailable instead of failing to launch it. Rows named in
/// `LAZYOS_XUI_AUTOSTART` (comma-separated stems such as `term,sysmon`; the
/// default is every embedded app, `none` disables it) are tagged `autostart`,
/// and `init` launches them at boot as `xuid` clients.
fn embed_xui_apps(builder: &mut bootloader::DiskImageBuilder) {
    println!("cargo:rerun-if-env-changed=LAZYOS_XUI_APPS");
    println!("cargo:rerun-if-env-changed=LAZYOS_XUI_AUTOSTART");
    let Some(apps) = std::env::var_os("LAZYOS_XUI_APPS") else {
        return;
    };
    let autostart = std::env::var("LAZYOS_XUI_AUTOSTART").ok();
    let wanted = |stem: &str| match autostart.as_deref() {
        None => true,
        Some("none") => false,
        Some(list) => list.split(',').any(|item| item.trim() == stem),
    };
    let mut manifest = String::new();
    for app in std::env::split_paths(&apps) {
        // Tracked even when missing: Cargo reruns while a listed path does not
        // exist, so an app built later is picked up without changing the env.
        println!("cargo:rerun-if-changed={}", app.display());
        if !app.is_file() {
            println!(
                "cargo:warning=LAZYOS_XUI_APPS entry not found: {}",
                app.display()
            );
            continue;
        }
        let (stem, disk) = xui_disk_name(&app);
        println!(
            "cargo:warning=LAZYOS_XUI_APPS embedded: {} as {disk}",
            app.display()
        );
        let suffix = if wanted(&stem) { " autostart" } else { "" };
        manifest.push_str(&format!("{disk}{suffix}\n"));
        builder.set_file(disk, app);
    }
    builder.set_file_contents(String::from("XAPPS.LST"), manifest.into_bytes());
}
