//! Kernel build script: rasterize a real monospace font into an anti-aliased
//! glyph atlas and emit it as Rust source + a raw coverage blob in `OUT_DIR`.

use std::env;
use std::fs;
use std::path::PathBuf;

fn main() {
    let out_dir = PathBuf::from(env::var_os("OUT_DIR").expect("OUT_DIR"));
    let manifest_dir = PathBuf::from(env::var("CARGO_MANIFEST_DIR").expect("manifest dir"));
    let font_path = manifest_dir.join("../assets/fonts/JetBrainsMono-Regular.ttf");

    println!("cargo:rerun-if-changed={}", font_path.display());
    println!("cargo:rerun-if-changed=build.rs");

    // Kernel test harness switch (issue #62): `LAZYOS_TESTS=1` compiles the
    // in-kernel suite and makes `kernel_main` run it before normal boot.
    // `rerun-if-env-changed` forces a rebuild when the mode is toggled.
    println!("cargo:rerun-if-env-changed=LAZYOS_TESTS");
    println!("cargo:rustc-check-cfg=cfg(lazyos_tests)");
    if env::var_os("LAZYOS_TESTS").as_deref() == Some(std::ffi::OsStr::new("1")) {
        println!("cargo:rustc-cfg=lazyos_tests");
    }

    // Desktop profile (issue #217): one `LAZYOS_DESKTOP=1` switch that expands
    // to the desktop recipe — a services session (`LAZYOS_SERVICES`), the
    // compositor (`LAZYOS_XUID`) and the embedded xui apps (the root build
    // script supplies the default app list when `LAZYOS_XUI_APPS` is unset).
    // It also marks the image as a user-facing desktop so `init` keeps the
    // demo/evidence-only programs (the crash test, the clipboard demo pair, the
    // `top` launch self-test) out of the boot. The user crate's `build.rs`
    // re-reads the same switch to compile `init` without them.
    println!("cargo:rerun-if-env-changed=LAZYOS_DESKTOP");
    println!("cargo:rustc-check-cfg=cfg(lazyos_desktop)");
    let desktop = env::var_os("LAZYOS_DESKTOP").as_deref() == Some(std::ffi::OsStr::new("1"));
    if desktop {
        println!("cargo:rustc-cfg=lazyos_desktop");
    }

    // Fabric observability demo switch (issue #70): `LAZYOS_MESSENGERCTL=1`
    // boots the `messengerctl` tool (`MSGCTL.ELF`) in the hello window
    // instead of HELLO.ELF.
    println!("cargo:rerun-if-env-changed=LAZYOS_MESSENGERCTL");
    println!("cargo:rustc-check-cfg=cfg(messengerctl_demo)");
    if env::var_os("LAZYOS_MESSENGERCTL").as_deref() == Some(std::ffi::OsStr::new("1")) {
        println!("cargo:rustc-cfg=messengerctl_demo");
    }

    // CLI mode switch: `LAZYOS_CLI=1` boots only the native shell (`SH.ELF`)
    // in a single mux window, dropping the `hello` demo window.
    // Ignored in services mode, where `init` owns the session.
    println!("cargo:rerun-if-env-changed=LAZYOS_CLI");
    println!("cargo:rustc-check-cfg=cfg(cli_mode)");
    if env::var_os("LAZYOS_CLI").as_deref() == Some(std::ffi::OsStr::new("1")) {
        println!("cargo:rustc-cfg=cli_mode");
    }

    // Registry daemon switch (issue #89): `LAZYOS_MESSENGERD=1` makes the
    // normal demo boot `messengerd` (`MESSENGERD.ELF`), which claims the
    // bootstrap channel and serves registry requests.
    println!("cargo:rerun-if-env-changed=LAZYOS_MESSENGERD");
    println!("cargo:rustc-check-cfg=cfg(messengerd_service)");
    if env::var_os("LAZYOS_MESSENGERD").as_deref() == Some(std::ffi::OsStr::new("1")) {
        println!("cargo:rustc-cfg=messengerd_service");
    }

    // Service supervision mode (issue #93): `LAZYOS_SERVICES=1` boots the
    // userspace `init` supervisor (`SUPER.ELF`) instead of the two-window
    // demo. `init` then starts `messengerd`, `logd`, `healthd` and the
    // crash-test service from its manifest, over the native spawn/wait
    // syscalls.
    println!("cargo:rerun-if-env-changed=LAZYOS_SERVICES");
    println!("cargo:rustc-check-cfg=cfg(services_mode)");
    if desktop || env::var_os("LAZYOS_SERVICES").as_deref() == Some(std::ffi::OsStr::new("1")) {
        println!("cargo:rustc-cfg=services_mode");
    }

    // Display protocol demo switch (issue #113): `LAZYOS_XUID=1` boots the
    // userspace compositor (`XUID.ELF`) and its demo app (`XDEMO.ELF`), which
    // claim the display device grant from the kernel mux.
    println!("cargo:rerun-if-env-changed=LAZYOS_XUID");
    println!("cargo:rustc-check-cfg=cfg(xuid_demo)");
    if desktop || env::var_os("LAZYOS_XUID").as_deref() == Some(std::ffi::OsStr::new("1")) {
        println!("cargo:rustc-cfg=xuid_demo");
    }

    // xui app switch (issue #114): with `LAZYOS_XUID=1` and a built app at
    // `LAZYOS_XUI_APP`, the kernel boots the app instead of the `xuid` demo
    // session. The app binds the display grant itself, so both cannot own the
    // screen at once; the default `xuid` demo is unchanged without the hook.
    println!("cargo:rerun-if-env-changed=LAZYOS_XUI_APP");
    println!("cargo:rustc-check-cfg=cfg(xui_app)");
    if env::var_os("LAZYOS_XUID").as_deref() == Some(std::ffi::OsStr::new("1"))
        && env::var_os("LAZYOS_XUI_APP").is_some()
    {
        println!("cargo:rustc-cfg=xui_app");
    }

    // Shell-protocol evidence client (issue #167): with `LAZYOS_XUID=1` and
    // the `LAZYOS_SHELLPROBE=1` demo hook, the kernel also boots `shellprobe`
    // (`SHELLPRB.ELF`), which exercises the S5.0 display additions. The hook
    // keeps the default compositor sessions byte-identical.
    println!("cargo:rerun-if-env-changed=LAZYOS_SHELLPROBE");
    println!("cargo:rustc-check-cfg=cfg(shellprobe_demo)");
    if env::var_os("LAZYOS_XUID").as_deref() == Some(std::ffi::OsStr::new("1"))
        && env::var_os("LAZYOS_SHELLPROBE").as_deref() == Some(std::ffi::OsStr::new("1"))
    {
        println!("cargo:rustc-cfg=shellprobe_demo");
    }

    // xui client switch (issue #168): with `LAZYOS_XUID=1`,
    // `LAZYOS_XUI_APP` and `LAZYOS_XUI_CLIENT=1`, the kernel boots `xuid`
    // *and* the app, which runs as a compositor client in a decorated window
    // (`xui-client`). Owner mode (no `LAZYOS_XUI_CLIENT`) is unchanged.
    println!("cargo:rerun-if-env-changed=LAZYOS_XUI_CLIENT");
    println!("cargo:rustc-check-cfg=cfg(xui_client)");
    if env::var_os("LAZYOS_XUID").as_deref() == Some(std::ffi::OsStr::new("1"))
        && env::var_os("LAZYOS_XUI_APP").is_some()
        && env::var_os("LAZYOS_XUI_CLIENT").as_deref() == Some(std::ffi::OsStr::new("1"))
    {
        println!("cargo:rustc-cfg=xui_client");
    }

    // Desktop session (issue #215/#216): with `LAZYOS_XUID=1`,
    // `LAZYOS_XUI_CLIENT=1` and `LAZYOS_XUI_APPS` (a list of xui app binaries
    // the root build script embeds), the kernel boots only `xuid`; the
    // supervisor (`init`, `LAZYOS_SERVICES=1`) launches the embedded apps as
    // its clients. No `xdemo`/`dragdemo` demo clients share the screen. The
    // legacy single-app switch (`LAZYOS_XUI_APP`) wins when both are set, so
    // `XAPP.ELF` is never started alongside `init`'s apps.
    println!("cargo:rerun-if-env-changed=LAZYOS_XUI_APPS");
    println!("cargo:rustc-check-cfg=cfg(xui_desktop)");
    // The desktop profile always embeds the app list (the root build script
    // defaults it), so it boots `xuid` and lets `init` open the clients.
    let desktop_apps = desktop
        || (env::var_os("LAZYOS_XUID").as_deref() == Some(std::ffi::OsStr::new("1"))
            && env::var_os("LAZYOS_XUI_CLIENT").as_deref() == Some(std::ffi::OsStr::new("1"))
            && env::var_os("LAZYOS_XUI_APPS").is_some());
    if desktop_apps && env::var_os("LAZYOS_XUI_APP").is_none() {
        println!("cargo:rustc-cfg=xui_desktop");
    }

    let font_bytes = fs::read(&font_path).expect("read JetBrainsMono-Regular.ttf");
    let atlas = font_atlas::build(&font_bytes, 20.0);

    fs::write(out_dir.join("font_atlas.bin"), &atlas.coverage).expect("write coverage blob");

    let mut src = String::new();
    src.push_str("// @generated by kernel/build.rs -- do not edit by hand.\n");
    src.push_str(&format!(
        "pub const FIRST_CHAR: u8 = {};\n",
        font_atlas::FIRST_CHAR
    ));
    src.push_str(&format!(
        "pub const LAST_CHAR: u8 = {};\n",
        font_atlas::LAST_CHAR
    ));
    src.push_str(&format!("pub const ASCENDER: i32 = {};\n", atlas.ascender));
    src.push_str(&format!(
        "pub const DESCENDER: i32 = {};\n",
        atlas.descender
    ));
    src.push_str(&format!(
        "pub const LINE_HEIGHT: i32 = {};\n",
        atlas.line_height
    ));
    src.push_str(&format!("pub const ADVANCE: u32 = {};\n", atlas.advance));
    src.push_str("pub const GLYPHS: &[Glyph] = &[\n");
    for g in &atlas.glyphs {
        src.push_str(&format!(
            "    Glyph {{ width: {}, height: {}, left: {}, top: {}, offset: {} }},\n",
            g.width, g.height, g.left, g.top, g.offset
        ));
    }
    src.push_str("];\n");
    src.push_str(
        "pub static COVERAGE: &[u8] = include_bytes!(concat!(env!(\"OUT_DIR\"), \"/font_atlas.bin\"));\n",
    );

    fs::write(out_dir.join("font_atlas.rs"), src).expect("write generated atlas source");
}
