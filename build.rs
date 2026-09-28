//! Host-side build script: combine the compiled kernel with the `bootloader`
//! crate into a bootable BIOS disk image, and expose the image path to the
//! runner (`src/main.rs`).

use std::path::PathBuf;

fn main() {
    let out_dir = PathBuf::from(std::env::var_os("OUT_DIR").expect("OUT_DIR"));
    let kernel =
        PathBuf::from(std::env::var_os("CARGO_BIN_FILE_KERNEL_kernel").expect("kernel artifact"));

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
    }

    // Rebuild the image when the kernel test switch flips (issue #62): the
    // kernel's own build script turns `LAZYOS_TESTS=1` into `cfg(laZYOS_TESTS)`.
    println!("cargo:rerun-if-env-changed=LAZYOS_TESTS");
    // Fabric observability demo switch (issue #70): the kernel boots the
    // `messengerctl` tool (`MSGCTL.ELF`) in the hello window when this is set.
    println!("cargo:rerun-if-env-changed=LAZYOS_MESSENGERCTL");
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
