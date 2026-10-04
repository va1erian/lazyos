//! Host-side build script: combine the compiled kernel with the `bootloader`
//! crate into the BIOS part of the disk image (MBR, stage 2, the FAT `/boot`
//! with the kernel and `lazyos.cfg`), put everything else on an ext2 OS volume
//! written by `libs/ext2fs`, and expose the image path to the runner
//! (`src/main.rs`). A rebuild updates an existing image in place
//! (`build_support/os_image.rs`).

use std::path::PathBuf;

#[path = "build_support/busybox_embed.rs"]
mod busybox_embed;
#[path = "build_support/ca_bundle.rs"]
mod ca_bundle;
#[path = "build_support/core_packages.rs"]
mod core_packages;
#[path = "build_support/docs_embed.rs"]
mod docs_embed;
#[path = "build_support/doom_embed.rs"]
mod doom_embed;
#[path = "build_support/drivers.rs"]
mod drivers;
#[path = "build_support/elf_trim.rs"]
mod elf_trim;
#[path = "build_support/hosts_embed.rs"]
mod hosts_embed;
#[path = "build_support/lazyrad_embed.rs"]
mod lazyrad_embed;
#[path = "build_support/linuxapps_embed.rs"]
mod linuxapps_embed;
#[path = "build_support/modplayer_embed.rs"]
mod modplayer_embed;
#[path = "build_support/os_disk.rs"]
mod os_disk;
#[path = "build_support/os_image.rs"]
mod os_image;
#[path = "build_support/os_layout.rs"]
mod os_layout;
#[path = "build_support/os_manifest.rs"]
mod os_manifest;
#[path = "build_support/os_recover.rs"]
mod os_recover;
#[path = "build_support/rhai_embed.rs"]
mod rhai_embed;
#[path = "build_support/samples_embed.rs"]
mod samples_embed;
#[path = "build_support/tls_embed.rs"]
mod tls_embed;
#[path = "build_support/usb_fat.rs"]
mod usb_fat;
#[path = "build_support/usb_image.rs"]
mod usb_image;
#[path = "build_support/usb_ramdisk.rs"]
mod usb_ramdisk;
#[path = "build_support/usb_stick.rs"]
mod usb_stick;
#[path = "build_support/wallpapers_embed.rs"]
mod wallpapers_embed;
#[path = "build_support/xui_embed.rs"]
mod xui_embed;

use os_image::Sink;

/// The account file (issues #101, #508), `name:uid:gid:secret:home:shell`,
/// installed as `/system/etc/passwd`: the **only** account source. `accountsd`
/// has no built-in table and fails closed without it. `build_support/passwd` is
/// the single copy: the image layout takes the `/home/<name>` directories from
/// it, and `tools/mkdisk/accounts.py` reads the same file for the home volume.
/// `admin` (uid 0) is the administrator, `user` (uid 1000) the unprivileged
/// demo login. The secret is plaintext *on purpose* for bring-up; hashes and
/// `/system/etc/shadow` are #447's (`docs/security-model.md` section 3). The
/// shell is BusyBox `sh` (the `sh` applet alias the kernel's Linux loader
/// resolves to `/system/bin/busybox`, issue #254).
const PASSWD: &[u8] = include_bytes!("build_support/passwd");

/// The account file to install: [`PASSWD`], checked with the parser
/// `accountsd` loads it with, so a file the daemon would refuse never ships.
/// `LAZYOS_OMIT_PASSWD=1` leaves it out, for the fail-closed check (an image
/// on which `accountsd` reports `failed` and no login succeeds).
fn account_file() -> Option<&'static [u8]> {
    println!("cargo:rerun-if-changed=build_support/passwd");
    println!("cargo:rerun-if-env-changed=LAZYOS_OMIT_PASSWD");
    if let Err(error) = passwd::parse(PASSWD) {
        panic!("build_support/passwd: accountsd would refuse it: {error}");
    }
    let omit = std::env::var_os("LAZYOS_OMIT_PASSWD").as_deref() == Some(std::ffi::OsStr::new("1"));
    if omit {
        println!("cargo:warning=LAZYOS_OMIT_PASSWD=1: no account file; no login will succeed");
        return None;
    }
    Some(PASSWD)
}

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
    for usb in ["usb_fat", "usb_image", "usb_ramdisk", "usb_stick"] {
        println!("cargo:rerun-if-changed=build_support/{usb}.rs");
    }

    // `LAZYOS_OS_SIZE` sizes the OS volume (a change needs `LAZYOS_RESET_OS=1`);
    // `LAZYOS_RESET_OS=1` recreates it instead of updating the existing image.
    println!("cargo:rerun-if-env-changed=LAZYOS_OS_SIZE");
    println!("cargo:rerun-if-env-changed=LAZYOS_RESET_OS");
    println!("cargo:rerun-if-env-changed=LAZYOS_UPDATE_DAMAGED_OS");
    // `LAZYOS_JOURNAL=1` (or a block count) gives the OS volume an ext2
    // journal; an update adds one to an existing image.
    println!("cargo:rerun-if-env-changed=LAZYOS_JOURNAL");
    let settings = os_image::Settings {
        update_damaged: std::env::var_os("LAZYOS_UPDATE_DAMAGED_OS").as_deref()
            == Some(std::ffi::OsStr::new("1")),
        os_size: match std::env::var("LAZYOS_OS_SIZE") {
            Ok(text) => os_disk::parse_size(&text).unwrap_or_else(|error| panic!("{error}")),
            Err(_) => os_disk::DEFAULT_OS_SIZE,
        },
        reset: std::env::var_os("LAZYOS_RESET_OS").as_deref() == Some(std::ffi::OsStr::new("1")),
    };
    let stable_image = manifest_dir.join("target").join("lazyos.img");
    if let Some(parent) = stable_image.parent() {
        std::fs::create_dir_all(parent).expect("create target dir");
    }
    // Decided first: an update keeps the OS volume's UUID, which the FAT
    // volume's `lazyos.cfg` must carry.
    let plan = os_image::plan(&stable_image, &settings).unwrap_or_else(|e| panic!("{e}"));
    if let Some(warning) = &plan.warning {
        println!("cargo:warning={warning}");
    }

    let bios_image = out_dir.join("bios.img");
    // The FAT `/boot` volume gets the kernel and `lazyos.cfg` only; every
    // other file goes to the OS file list.
    let mut builder = bootloader::DiskImageBuilder::new(kernel.clone());
    builder.set_file_contents(
        String::from(fhs::boot::LAZYOS_CFG),
        (os_image::boot_cfg(plan.uuid, &os_image::limits_cfg::from_env())
            + &os_image::display_cfg::from_env())
            .into_bytes(),
    );
    let mut files = os_image::OsFiles::default();
    // Issue #5: `LAZYOS_RAMDISK=<path>` also loads a FAT image as the
    // bootloader ramdisk, which the kernel registers as the `ram0` fallback
    // block device.
    println!("cargo:rerun-if-env-changed=LAZYOS_RAMDISK");
    if let Some(ramdisk) = std::env::var_os("LAZYOS_RAMDISK") {
        builder.set_ramdisk(PathBuf::from(ramdisk));
    }
    // The sample files (text, the Docs test document, LazyWriter's picture).
    samples_embed::embed(&mut files);
    // The ring-3 demo window. The system shell is BusyBox `sh` (issue #254),
    // embedded separately below.
    let hello =
        std::env::var_os("CARGO_BIN_FILE_USER_hello").expect("user hello artifact not found");
    files.add_file(fhs::bin::HELLO, PathBuf::from(hello));
    // Deliberate ring-3 faults (issue #7): `faultprobe null|kernel|priv|div|ud`.
    let faultprobe = std::env::var_os("CARGO_BIN_FILE_USER_faultprobe")
        .expect("user faultprobe artifact not found");
    files.add_file(fhs::bin::FAULTPROBE, PathBuf::from(faultprobe));
    // The fabric observability tool (issue #70); boot it with
    // `LAZYOS_MESSENGERCTL=1`. Every program goes to its `fhs::bin` path, which
    // its spawners use byte for byte (ext2 is case-sensitive).
    let messengerctl = std::env::var_os("CARGO_BIN_FILE_USER_messengerctl")
        .expect("user messengerctl artifact not found");
    files.add_file(fhs::bin::MESSENGERCTL, PathBuf::from(messengerctl));
    // The registry daemon (issue #89), started by `LAZYOS_MESSENGERD=1`.
    let messengerd = std::env::var_os("CARGO_BIN_FILE_USER_messengerd")
        .expect("user messengerd artifact not found");
    files.add_file(fhs::bin::MESSENGERD, PathBuf::from(messengerd));

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
    // with `LAZYOS_SERVICES=1`; it starts the rest from its manifest. The ABI
    // bench hook below places its Linux fixture at `abi-init`, not `init`.
    let init = std::env::var_os("CARGO_BIN_FILE_USER_init").expect("user init artifact not found");
    files.add_file(fhs::bin::INIT, PathBuf::from(init));
    let logd = std::env::var_os("CARGO_BIN_FILE_USER_logd").expect("user logd artifact not found");
    files.add_file(fhs::bin::LOGD, PathBuf::from(logd));
    let healthd =
        std::env::var_os("CARGO_BIN_FILE_USER_healthd").expect("user healthd artifact not found");
    files.add_file(fhs::bin::HEALTHD, PathBuf::from(healthd));
    // The secrets and crypto service (issue #102). `init` starts it from its
    // manifest when the image boots with `LAZYOS_SERVICES=1`.
    let keyd = std::env::var_os("CARGO_BIN_FILE_USER_keyd").expect("user keyd artifact not found");
    files.add_file(fhs::bin::KEYD, PathBuf::from(keyd));

    // The per-session clipboard service (issue #115). Like `keyd`, `init`
    // starts it from its manifest when the image boots with
    // `LAZYOS_SERVICES=1`.
    let clipboardd = std::env::var_os("CARGO_BIN_FILE_USER_clipboardd")
        .expect("user clipboardd artifact not found");
    files.add_file(fhs::bin::CLIPBOARDD, PathBuf::from(clipboardd));

    // The evidence-only programs. `init` never starts them in the desktop
    // profile, so the image leaves their ELFs out entirely: the deliberate
    // crash service (issue #93), whose restart-with-backoff demo is the
    // `flaky` row, and the clipboard demo pair (issue #115), which
    // `clipboardd` spawns under `demo=1`.
    if !desktop {
        let flaky =
            std::env::var_os("CARGO_BIN_FILE_USER_flaky").expect("user flaky artifact not found");
        files.add_file(fhs::bin::FLAKY, PathBuf::from(flaky));
        let clipcopy = std::env::var_os("CARGO_BIN_FILE_USER_clipcopy")
            .expect("user clipcopy artifact not found");
        files.add_file(fhs::bin::CLIPCP, PathBuf::from(clipcopy));
        let clippaste = std::env::var_os("CARGO_BIN_FILE_USER_clippaste")
            .expect("user clippaste artifact not found");
        files.add_file(fhs::bin::CLIPPASTE, PathBuf::from(clippaste));
    }

    // Accounts and console login (issue #101). `init` starts `accountsd` and
    // `logind` from its manifest; `accountsd` reads `passwd` when present. All
    // three are added only to the services image (`LAZYOS_SERVICES=1`): the
    // plain demo never starts them, and keeping them out of the ABI bench
    // image preserves its baseline size and boot time.
    if services {
        let accountsd = std::env::var_os("CARGO_BIN_FILE_USER_accountsd")
            .expect("user accountsd artifact not found");
        files.add_file(fhs::bin::ACCOUNTSD, PathBuf::from(accountsd));
        let logind =
            std::env::var_os("CARGO_BIN_FILE_USER_logind").expect("user logind artifact not found");
        files.add_file(fhs::bin::LOGIND, PathBuf::from(logind));

        // The configuration registry (issue #260). `init` starts `confd` from
        // its manifest; `confctl` is its native command line.
        // Both are gated behind `LAZYOS_SERVICES=1`, like the other services,
        // so the plain demo image is unchanged.
        let confd =
            std::env::var_os("CARGO_BIN_FILE_USER_confd").expect("user confd artifact not found");
        files.add_file(fhs::bin::CONFD, PathBuf::from(confd));
        let confctl = std::env::var_os("CARGO_BIN_FILE_USER_confctl")
            .expect("user confctl artifact not found");
        files.add_file(fhs::bin::CONFCTL, PathBuf::from(confctl));

        // The input policy service (docs/input-plan.md). `init` starts
        // `inputd` after `confd`; it is the only task that holds the kernel's
        // `input.raw` capability.
        let inputd =
            std::env::var_os("CARGO_BIN_FILE_USER_inputd").expect("user inputd artifact not found");
        files.add_file(fhs::bin::INPUTD, PathBuf::from(inputd));

        // The time-of-day service (issue #369). `init` starts `timed` after
        // `messengerd` and `confd`.
        let timed =
            std::env::var_os("CARGO_BIN_FILE_USER_timed").expect("user timed artifact not found");
        files.add_file(fhs::bin::TIMED, PathBuf::from(timed));
        let timectl = std::env::var_os("CARGO_BIN_FILE_USER_timectl")
            .expect("user timectl artifact not found");
        files.add_file(fhs::bin::TIMECTL, PathBuf::from(timectl));

        // The orderly shutdown/reboot command (docs/shutdown.md): `init`'s
        // `Shutdown` from the shell (`shutdown`, `poweroff`, `halt`, `reboot`).
        let powerctl = std::env::var_os("CARGO_BIN_FILE_USER_powerctl")
            .expect("user powerctl artifact not found");
        files.add_file(fhs::bin::POWERCTL, PathBuf::from(powerctl));

        // The MIME database and open-with registry (issue #116). `init`
        // starts it from its manifest. `mime.types` is the override file the
        // service reads at boot.
        let mimed =
            std::env::var_os("CARGO_BIN_FILE_USER_mimed").expect("user mimed artifact not found");
        files.add_file(fhs::bin::MIMED, PathBuf::from(mimed));

        // The application package manager (docs/packages.md phase 3). `init`
        // starts `pkgd` from its manifest (after `confd` and `mimed`); `pkgctl`
        // is its command line, run from the Terminal.
        let pkgd =
            std::env::var_os("CARGO_BIN_FILE_USER_pkgd").expect("user pkgd artifact not found");
        files.add_file(fhs::bin::PKGD, PathBuf::from(pkgd));
        let pkgctl =
            std::env::var_os("CARGO_BIN_FILE_USER_pkgctl").expect("user pkgctl artifact not found");
        files.add_file(fhs::bin::PKGCTL, PathBuf::from(pkgctl));
        xui_embed::embed_sample_packages(&mut files);
        files.add_bytes(
            fhs::share::MIME_TYPES,
            b"# LazyOS MIME overrides, /etc/mime.types style: <mime> <ext>...\n\
              text/x-lazy-test lzt\n\
              application/x-lazyos lazy\n"
                .to_vec(),
        );

        // The account database `accountsd` reads (see [`PASSWD`]).
        if let Some(passwd) = account_file() {
            files.add_bytes(fhs::etc::PASSWD, passwd.to_vec());
        }

        // The system monitor (issue #144). `init` starts `sysmond` from its
        // manifest; the service wraps the native system-stats syscall (14) and
        // republishes retained `system/stats/*` topics. `top` is its one-shot
        // native text client, spawned by `sysmond` (`demo=1`) so a headless
        // services boot records `SYS:TOP:PASS`.
        let sysmond = std::env::var_os("CARGO_BIN_FILE_USER_sysmond")
            .expect("user sysmond artifact not found");
        files.add_file(fhs::bin::SYSMOND, PathBuf::from(sysmond));
        // The `top` text client is the launch self-test's target, an
        // evidence-only program the desktop profile never starts, so its ELF
        // stays out of the desktop image.
        if !desktop {
            let top =
                std::env::var_os("CARGO_BIN_FILE_USER_top").expect("user top artifact not found");
            files.add_file(fhs::bin::TOP, PathBuf::from(top));
        }
    }

    // The display protocol demo (issue #113): `LAZYOS_XUID=1` embeds the
    // userspace compositor and its demo app. Both are gated out of the default
    // demo image so its size and boot stay identical.
    if xuid {
        let xuid =
            std::env::var_os("CARGO_BIN_FILE_USER_xuid").expect("user xuid artifact not found");
        files.add_file(fhs::bin::XUID, PathBuf::from(xuid));
        // `xdemo` and the drag & drop pair are demo clients; the desktop
        // profile runs its own xui apps as clients instead.
        if !desktop {
            let xdemo = std::env::var_os("CARGO_BIN_FILE_USER_xdemo")
                .expect("user xdemo artifact not found");
            files.add_file(fhs::bin::XDEMO, PathBuf::from(xdemo));
            // The drag & drop demo pair (issue #145); the kernel starts its
            // launcher.
            let dragdemo = std::env::var_os("CARGO_BIN_FILE_USER_dragdemo")
                .expect("user dragdemo artifact not found");
            files.add_file(fhs::bin::DRAGDEMO, PathBuf::from(dragdemo));
        }
    }

    // The virtio-sound and virtio-net userspace drivers (`LAZYOS_SOUND=1`,
    // `LAZYOS_NET=1`; the desktop profile always ships the sound stack).
    drivers::embed(&mut files, desktop);

    // The shell-protocol evidence client (issue #167): `LAZYOS_XUID=1` plus
    // the `LAZYOS_SHELLPROBE=1` demo hook embeds and boots it, so the default
    // compositor sessions (WM, drag & drop) keep their window layout.
    println!("cargo:rerun-if-env-changed=LAZYOS_SHELLPROBE");
    if std::env::var_os("LAZYOS_XUID").as_deref() == Some(std::ffi::OsStr::new("1"))
        && std::env::var_os("LAZYOS_SHELLPROBE").as_deref() == Some(std::ffi::OsStr::new("1"))
    {
        let probe = std::env::var_os("CARGO_BIN_FILE_USER_shellprobe")
            .expect("user shellprobe artifact not found");
        files.add_file(fhs::bin::SHELLPROBE, PathBuf::from(probe));
    }

    // The xui app (issue #114): `LAZYOS_XUI_APP=<path>` embeds a static-musl
    // binary built by `tools/xui/build.py` as `xapp`. With `LAZYOS_XUID=1`
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
            files.add_file(fhs::bin::XAPP, app);
        } else {
            println!("cargo:warning=LAZYOS_XUI_APP not found: {}", app.display());
        }
    }

    // The desktop shell (issue #157), on by default with the desktop profile.
    println!("cargo:rerun-if-changed=build_support/xui_embed.rs");
    println!("cargo:rerun-if-changed=build_support/core_packages.rs");
    let shell = xui_embed::shell_enabled(desktop, services, xuid);
    xui_embed::embed_xui_apps(&mut files, desktop, shell);
    // The desktop pictures LazyShell can draw behind the launchers.
    if shell {
        wallpapers_embed::embed(&mut files);
    }

    // Rebuild the image when the kernel test switch flips (issue #62): the
    // kernel's own build script turns `LAZYOS_TESTS=1` into `cfg(lazyos_tests)`.
    println!("cargo:rerun-if-env-changed=LAZYOS_TESTS");
    // Fabric observability demo switch (issue #70): the kernel boots the
    // `messengerctl` tool in the hello window when this is set.
    println!("cargo:rerun-if-env-changed=LAZYOS_MESSENGERCTL");
    // CLI mode switch: the kernel boots only `sh` (no `hello` window).
    println!("cargo:rerun-if-env-changed=LAZYOS_CLI");
    // Registry daemon switch (issue #89): the kernel starts `messengerd` when
    // this is set.
    println!("cargo:rerun-if-env-changed=LAZYOS_MESSENGERD");

    // ABI conformance bench hook: embed a Linux fixture as `abi-init`.
    println!("cargo:rerun-if-env-changed=LAZYOS_INIT");
    if let Some(init) = std::env::var_os("LAZYOS_INIT") {
        let init = PathBuf::from(init);
        if init.is_file() {
            println!("cargo:warning=LAZYOS_INIT embedded: {}", init.display());
            files.add_file(fhs::bin::ABI_INIT, init);
        } else {
            println!("cargo:warning=LAZYOS_INIT not found: {}", init.display());
        }
    }

    // BusyBox, the system shell (issue #254); `LAZYOS_INIT` images skip it.
    busybox_embed::embed(&mut files, &manifest_dir);
    // The `rhai` scripting command (issue #319), found from `sh` in /system/bin.
    println!("cargo:rerun-if-changed=build_support/rhai_embed.rs");
    rhai_embed::embed(&mut files, &manifest_dir);
    // The resolver's host table and the TLS trust anchors, in every image
    // (`/etc/hosts`, `/etc/ssl/certs/ca-certificates.crt` for Linux programs),
    // and with `LAZYOS_TLS=1` the HTTPS client as fetch/curl/wget.
    println!("cargo:rerun-if-changed=build_support/hosts_embed.rs");
    println!("cargo:rerun-if-changed=build_support/ca_bundle.rs");
    println!("cargo:rerun-if-changed=build_support/tls_embed.rs");
    hosts_embed::embed(&mut files);
    let roots = webpki_root_certs::TLS_SERVER_ROOT_CERTS.iter();
    ca_bundle::embed(&mut files, roots.map(|der| der.as_ref()));
    tls_embed::embed(&mut files, &manifest_dir);
    linuxapps_embed::embed(&mut files, &manifest_dir); // dash, lua, sqlite3, jq, rg
                                                       // The docs tree (`docs/**/*.md`, `README.md`) at `/docs/os/...` (Docs, Editor).
    println!("cargo:rerun-if-changed=build_support/docs_embed.rs");
    docs_embed::embed(&mut files, &manifest_dir);
    // The LazyRAD IDE is a core package (embedded with the others above when
    // `LAZYOS_LAZYRAD=1`); this adds the sample projects in `LAZYRAD_SAMPLES`.
    println!("cargo:rerun-if-changed=build_support/lazyrad_embed.rs");
    lazyrad_embed::embed(&mut files, &manifest_dir);
    // The Doom package (`LAZYOS_DOOM=1`) as a sample user package, installed
    // through pkgd.
    doom_embed::embed(&mut files, &manifest_dir);
    // The LazyRAD MOD player package (`LAZYOS_MODPLAYER=1`) in /system/share/samples.
    modplayer_embed::embed(&mut files, &manifest_dir);
    builder
        .create_bios_image(&bios_image)
        .expect("failed to create BIOS disk image");

    // Compose the stable image tooling uses (CI screenshots, scripts): the BIOS
    // part plus the OS volume, created or updated in place per `plan`.
    let bios = std::fs::read(&bios_image).expect("read the BIOS image");
    let accounts = os_layout::parse_passwd(&String::from_utf8_lossy(PASSWD));
    let dirs = os_layout::dirs(&accounts);
    // The USB stick image (`LAZYOS_USB_IMAGE=1`, docs/usb-stick.md): the same
    // files on a RAM root, booted under UEFI or BIOS, plus a home partition.
    if usb_image::enabled() {
        let usb = manifest_dir.join("target").join("lazyos-usb.img");
        usb_image::build(&kernel, &out_dir, &usb, &dirs, &files.files(), &accounts)
            .unwrap_or_else(|error| panic!("USB image: {error}"));
    }
    os_image::compose(
        &plan,
        &stable_image,
        &bios,
        &settings,
        &dirs,
        &files.files(),
    )
    .unwrap_or_else(|error| panic!("OS image: {error}"));
    println!(
        "cargo:warning=OS volume {} ({} files, {} MiB): {}",
        os_image::format_uuid(plan.uuid),
        files.len(),
        settings.os_size >> 20,
        match plan.action {
            os_image::Action::Create => "created",
            os_image::Action::Update { .. } => "updated in place",
        }
    );

    println!("cargo:rustc-env=BIOS_IMAGE={}", bios_image.display());
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=kernel/src");
    println!("cargo:rerun-if-changed=assets/fonts/JetBrainsMono-Regular.ttf");
}
