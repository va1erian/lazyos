//! The developer tools (docs/devtool-packages-plan.md) in the disk image.
//!
//! A developer tool is a core package whose manifest says `kind =
//! "developer-tool"`: it runs under the label `tool:<system_name>`, which allows
//! every call but a reviewed deny list, because the calls it makes are the ones
//! the user (or a script the user runs) names. They are shipped as a class, by
//! one switch: `LAZYOS_DEVTOOLS=1` embeds every package in [`TOOLS`] in
//! `/system/packages` (`xui_embed::embed_xui_apps`), and `pkgd` installs them
//! at boot. A default desktop image ships none of the optional ones; a tool that
//! is `always` ships in every desktop image (the Terminal, the user's shell).
//!
//! A tool runs the user's scripts, so the switch needs the desktop (a window)
//! and the `rhai` command (`tools/rhai/build.py`, embedded by `rhai_embed`):
//! it is refused without either instead of producing an image whose tool
//! cannot open a window or run anything. `python tools/run_demo.py --devtools`
//! and the launcher's "Developer tools" set both and build `rhai`. With the
//! switch unset nothing changes.

use std::ffi::OsStr;

/// One developer-tool core package: its short id (`xui-app/packages/<short>`)
/// and the program `tools/xui/build.py` builds into `target/xui/`.
pub struct Tool {
    pub short: &'static str,
    pub elf: &'static str,
    /// Shipped by every desktop image, not only by `LAZYOS_DEVTOOLS=1`. A tool
    /// the desktop cannot do without (the Terminal: every session script and
    /// harness types into it) is `always`; one that runs the user's scripts
    /// with the authority of a tool and is not the user's shell (the
    /// Playground) is opt-in.
    pub always: bool,
}

/// Every developer tool, shipped together by `LAZYOS_DEVTOOLS=1`. A new tool
/// is listed here, in `tools/xui/core_packages.py` `CORE_APPS`, in
/// `core_packages::is_core_stem` and in `tools/xui/build.py` `BINS`.
pub const TOOLS: &[Tool] = &[
    // The Terminal (docs/terminal-plan.md; docs/devtool-packages-plan.md,
    // "T-later: Terminal"): the user's shell, in every desktop image.
    Tool {
        short: "terminal",
        elf: "xui-term.elf",
        always: true,
    },
    // The Messenger Playground (docs/messenger-playground-plan.md).
    Tool {
        short: "playground",
        elf: "xui-playground.elf",
        always: false,
    },
];

/// Whether `tool` ships in this build: its `always`, or the class switch
/// (`LAZYOS_DEVTOOLS=1`, `devtools`). Both need the desktop (a window).
pub fn ships(tool: &Tool, devtools: bool, desktop: bool) -> bool {
    devtools || (tool.always && desktop)
}

/// Whether the image ships the developer tools (`LAZYOS_DEVTOOLS=1`). Panics
/// when the switch is set without the profile it needs ([`requirements`]);
/// `rhai` says whether the `rhai` command is available to embed.
pub fn enabled(desktop: bool, rhai: bool) -> bool {
    println!("cargo:rerun-if-env-changed=LAZYOS_DEVTOOLS");
    let wanted = std::env::var_os("LAZYOS_DEVTOOLS").as_deref() == Some(OsStr::new("1"));
    if let Err(reason) = requirements(wanted, desktop, rhai) {
        panic!("{reason}");
    }
    wanted
}

/// Whether a build with `devtools` can ship them: the switch needs the
/// desktop profile and the `rhai` command.
pub fn requirements(devtools: bool, desktop: bool, rhai: bool) -> Result<(), String> {
    if !devtools || (desktop && rhai) {
        return Ok(());
    }
    let missing: Vec<&str> = [
        ("LAZYOS_DESKTOP=1", desktop),
        ("the `rhai` command (python tools/rhai/build.py)", rhai),
    ]
    .into_iter()
    .filter(|(_, present)| !present)
    .map(|(name, _)| name)
    .collect();
    Err(format!(
        "LAZYOS_DEVTOOLS=1 needs {} (a developer tool is a desktop app that runs the user's \
         scripts); `python tools/run_demo.py --devtools` sets and builds both",
        missing.join(" and ")
    ))
}

/// The build failure for a wanted but unbuilt tool package, naming what to run.
pub fn not_built(tool: &Tool, elf_present: bool) -> String {
    if elf_present {
        format!(
            "the desktop ships core package {} but it is not packaged; run \
             `python tools/xui/core_packages.py`",
            tool.short
        )
    } else {
        format!(
            "the desktop ships target/xui/{} but it is not built; run `python tools/xui/build.py`",
            tool.elf
        )
    }
}
